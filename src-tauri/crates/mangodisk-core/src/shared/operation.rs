use std::sync::{
    atomic::{AtomicBool, AtomicU64, AtomicU8, Ordering},
    Arc, Mutex, OnceLock,
};
use std::{
    fs::{self, File, OpenOptions},
    io::ErrorKind,
    time::Instant,
};

use crate::shared::application_paths;
use fs2::FileExt;

use super::{CoreError, CoreResult};

static COORDINATOR: OnceLock<OperationCoordinator> = OnceLock::new();
#[cfg(test)]
static TEST_OPERATION_LOCK: OnceLock<Mutex<()>> = OnceLock::new();
const OPERATION_RUNNING: u8 = 0;
const OPERATION_COMPLETED: u8 = 1;
pub(crate) const OPERATION_CANCELLED_ERROR: &str = "operation cancelled";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CoordinatedOperationKind {
    CleanupScan,
    Analysis,
    LargeFiles,
    DuplicateFiles,
    ApplicationScan,
    Applications,
    ApplicationLeftoverCleanup,
    ApplicationClose,
    Cleanup,
    PermanentDelete,
    StartupScan,
    StartupChange,
    SystemSettingsScan,
    SystemSettingsChange,
}

impl CoordinatedOperationKind {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::CleanupScan => "cleanup_scan",
            Self::Analysis => "analysis",
            Self::LargeFiles => "large_files",
            Self::DuplicateFiles => "duplicate_files",
            Self::ApplicationScan => "application_scan",
            Self::Applications => "applications",
            Self::ApplicationLeftoverCleanup => "application_leftover_cleanup",
            Self::ApplicationClose => "application_close",
            Self::Cleanup => "cleanup",
            Self::PermanentDelete => "permanent_delete",
            Self::StartupScan => "startup_scan",
            Self::StartupChange => "startup_change",
            Self::SystemSettingsScan => "system_settings_scan",
            Self::SystemSettingsChange => "system_settings_change",
        }
    }
}

/// Adapter-owned handle for cancelling one class of active Core operation.
///
/// The token contains no UI or terminal state. Desktop and CLI adapters can
/// therefore request cancellation through the same Core contract.
#[derive(Clone, Copy, Debug)]
pub struct OperationCancellationToken {
    kind: CoordinatedOperationKind,
}

impl OperationCancellationToken {
    pub const fn cleanup_scan() -> Self {
        Self {
            kind: CoordinatedOperationKind::CleanupScan,
        }
    }

    pub const fn analysis() -> Self {
        Self {
            kind: CoordinatedOperationKind::Analysis,
        }
    }

    pub const fn large_files() -> Self {
        Self {
            kind: CoordinatedOperationKind::LargeFiles,
        }
    }

    pub const fn duplicate_files() -> Self {
        Self {
            kind: CoordinatedOperationKind::DuplicateFiles,
        }
    }

    pub const fn cleanup() -> Self {
        Self {
            kind: CoordinatedOperationKind::Cleanup,
        }
    }

    pub const fn applications() -> Self {
        Self {
            kind: CoordinatedOperationKind::Applications,
        }
    }

    pub const fn application_leftover_cleanup() -> Self {
        Self {
            kind: CoordinatedOperationKind::ApplicationLeftoverCleanup,
        }
    }

    pub const fn application_scan() -> Self {
        Self {
            kind: CoordinatedOperationKind::ApplicationScan,
        }
    }

    pub const fn startup_scan() -> Self {
        Self {
            kind: CoordinatedOperationKind::StartupScan,
        }
    }

    pub const fn startup_change() -> Self {
        Self {
            kind: CoordinatedOperationKind::StartupChange,
        }
    }

    pub const fn system_settings_scan() -> Self {
        Self {
            kind: CoordinatedOperationKind::SystemSettingsScan,
        }
    }

    pub fn cancel(self) {
        OperationGuard::cancel(self.kind);
    }
}

struct ActiveOperation {
    id: u64,
    kind: CoordinatedOperationKind,
    cancelled: Arc<AtomicBool>,
}

struct OperationCoordinator {
    next_id: AtomicU64,
    active: Mutex<Option<ActiveOperation>>,
}

struct ProcessOperationLock {
    file: File,
}

impl ProcessOperationLock {
    fn acquire(kind: CoordinatedOperationKind) -> CoreResult<Self> {
        let directory = application_paths()?.runtime_directory();
        fs::create_dir_all(directory).map_err(|error| {
            CoreError::operation_failed(format!(
                "failed to create the operation lock directory: {error}"
            ))
        })?;
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(directory.join("operation.lock"))
            .map_err(|error| {
                CoreError::operation_failed(format!("failed to open the operation lock: {error}"))
            })?;
        match file.try_lock_exclusive() {
            Ok(()) => Ok(Self { file }),
            Err(error) if is_lock_contention(&error) => Err(CoreError::operation_busy(format!(
                "another MangoDisk operation is already running; requested={}",
                kind.as_str()
            ))),
            Err(error) => Err(CoreError::operation_failed(format!(
                "failed to acquire the operation lock: {error}"
            ))),
        }
    }
}

fn is_lock_contention(error: &std::io::Error) -> bool {
    if error.kind() == ErrorKind::WouldBlock {
        return true;
    }

    // LockFileEx reports sharing and lock violations as native Windows errors
    // instead of ErrorKind::WouldBlock on some toolchain versions. Mapping only
    // those two codes preserves stable busy semantics without hiding genuine
    // permission or filesystem failures.
    #[cfg(windows)]
    {
        matches!(error.raw_os_error(), Some(32 | 33))
    }

    #[cfg(not(windows))]
    {
        false
    }
}

impl Drop for ProcessOperationLock {
    fn drop(&mut self) {
        if let Err(error) = FileExt::unlock(&self.file) {
            log::warn!("operation_process_lock_release_failed error={error}");
        }
    }
}

impl OperationCoordinator {
    fn global() -> &'static Self {
        COORDINATOR.get_or_init(|| Self {
            next_id: AtomicU64::new(1),
            active: Mutex::new(None),
        })
    }
}

/// Coordinates disk-intensive and mutating operations across threads and
/// processes. This prevents concurrent adapters from invalidating cache or
/// cleanup state while another operation is still using it.
pub(crate) struct OperationGuard {
    id: u64,
    kind: CoordinatedOperationKind,
    cancelled: Arc<AtomicBool>,
    started: Instant,
    outcome: AtomicU8,
    _process_lock: ProcessOperationLock,
}

impl OperationGuard {
    pub(crate) fn start(kind: CoordinatedOperationKind) -> CoreResult<Self> {
        let coordinator = OperationCoordinator::global();
        let mut active = coordinator.active.lock().map_err(|_| {
            CoreError::operation_failed("the operation coordinator is temporarily unavailable")
        })?;
        if let Some(operation) = active.as_ref() {
            return Err(CoreError::operation_busy(format!(
                "another MangoDisk operation is already running: {} ({})",
                operation.kind.as_str(),
                operation.id
            )));
        }

        let process_lock = ProcessOperationLock::acquire(kind)?;
        let id = coordinator.next_id.fetch_add(1, Ordering::Relaxed);
        let cancelled = Arc::new(AtomicBool::new(false));
        *active = Some(ActiveOperation {
            id,
            kind,
            cancelled: Arc::clone(&cancelled),
        });
        log::info!(
            "operation_started operation_id={} operation_kind={}",
            id,
            kind.as_str()
        );
        Ok(Self {
            id,
            kind,
            cancelled,
            started: Instant::now(),
            outcome: AtomicU8::new(OPERATION_RUNNING),
            _process_lock: process_lock,
        })
    }

    pub(crate) fn id(&self) -> u64 {
        self.id
    }

    pub(crate) fn cancelled(&self) -> &AtomicBool {
        self.cancelled.as_ref()
    }

    /// Background validation receives only the flag so a worker cannot extend
    /// guard ownership and keep the process lock alive accidentally.
    pub(crate) fn cancellation_flag(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.cancelled)
    }

    pub(crate) fn ensure_not_cancelled(&self) -> CoreResult<()> {
        if self.cancelled.load(Ordering::Relaxed) {
            Err(CoreError::operation_cancelled())
        } else {
            Ok(())
        }
    }

    pub(crate) fn complete(&self) {
        self.outcome.store(OPERATION_COMPLETED, Ordering::Relaxed);
    }

    pub(crate) fn cancel(kind: CoordinatedOperationKind) {
        let Ok(active) = OperationCoordinator::global().active.lock() else {
            log::warn!(
                "operation_cancel_failed operation_kind={} reason=coordinator_poisoned",
                kind.as_str()
            );
            return;
        };
        let Some(operation) = active.as_ref().filter(|operation| operation.kind == kind) else {
            return;
        };
        operation.cancelled.store(true, Ordering::Relaxed);
        log::info!(
            "operation_cancel_requested operation_id={} operation_kind={}",
            operation.id,
            operation.kind.as_str()
        );
    }
}

impl Drop for OperationGuard {
    fn drop(&mut self) {
        let Ok(mut active) = OperationCoordinator::global().active.lock() else {
            log::warn!(
                "operation_release_failed operation_id={} operation_kind={} reason=coordinator_poisoned",
                self.id,
                self.kind.as_str()
            );
            return;
        };
        if active
            .as_ref()
            .is_some_and(|operation| operation.id == self.id)
        {
            *active = None;
        }
        let cancelled = self.cancelled.load(Ordering::Relaxed);
        let status = if cancelled {
            "cancelled"
        } else {
            match self.outcome.load(Ordering::Relaxed) {
                OPERATION_COMPLETED => "completed",
                _ => "failed",
            }
        };
        if status == "failed" {
            log::warn!(
                "operation_finished operation_id={} operation_kind={} status={} cancelled={} elapsed_ms={}",
                self.id,
                self.kind.as_str(),
                status,
                cancelled,
                self.started.elapsed().as_millis()
            );
        } else {
            log::info!(
                "operation_finished operation_id={} operation_kind={} status={} cancelled={} elapsed_ms={}",
                self.id,
                self.kind.as_str(),
                status,
                cancelled,
                self.started.elapsed().as_millis()
            );
        }
    }
}

#[cfg(test)]
pub(crate) fn test_operation_lock() -> std::sync::MutexGuard<'static, ()> {
    TEST_OPERATION_LOCK
        .get_or_init(|| Mutex::new(()))
        .lock()
        // The test-only mutex carries no business state. Recovering a poisoned
        // guard prevents one assertion failure from hiding later independent
        // disk test results.
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::CoreErrorCode;

    #[test]
    fn cancellation_token_stops_the_matching_operation() {
        let _test_guard = test_operation_lock();
        let operation = OperationGuard::start(CoordinatedOperationKind::CleanupScan)
            .expect("the isolated operation should start");

        OperationCancellationToken::cleanup_scan().cancel();

        assert_eq!(
            operation
                .ensure_not_cancelled()
                .expect_err("the operation should be cancelled")
                .code(),
            CoreErrorCode::OperationCancelled
        );
    }

    #[test]
    fn application_scan_cancellation_does_not_cancel_application_execution() {
        let _test_guard = test_operation_lock();
        let operation = OperationGuard::start(CoordinatedOperationKind::Applications)
            .expect("the isolated application operation should start");

        OperationCancellationToken::application_scan().cancel();

        operation
            .ensure_not_cancelled()
            .expect("scan cancellation must not affect an application mutation");
    }

    #[test]
    fn process_lock_is_released_when_the_guard_drops() {
        let _test_guard = test_operation_lock();
        let first = ProcessOperationLock::acquire(CoordinatedOperationKind::Analysis)
            .expect("the first process lock should succeed");
        let error = ProcessOperationLock::acquire(CoordinatedOperationKind::Analysis)
            .err()
            .expect("a second process lock should be rejected");
        assert_eq!(error.code(), CoreErrorCode::OperationBusy);

        drop(first);
        let second = ProcessOperationLock::acquire(CoordinatedOperationKind::Analysis)
            .expect("the process lock should be reusable after release");
        drop(second);
    }
}

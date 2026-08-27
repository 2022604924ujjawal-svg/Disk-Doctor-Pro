use std::sync::Arc;

/// Cloneable cancellation boundary for platform operations that retain a
/// callback while native code is running.
///
/// A borrowed closure is insufficient for Windows disk-cleanup COM handlers:
/// cancellation must remain callable from `GetSpaceUsed` and `Purge` progress
/// callbacks until the native call returns.
#[derive(Clone)]
pub struct PlatformCancellation {
    probe: Arc<dyn Fn() -> bool + Send + Sync>,
}

impl PlatformCancellation {
    pub fn new(probe: impl Fn() -> bool + Send + Sync + 'static) -> Self {
        Self {
            probe: Arc::new(probe),
        }
    }

    pub fn is_cancelled(&self) -> bool {
        (self.probe)()
    }
}

/// Stable Windows-owned cleanup categories exposed to Core.
///
/// The variants intentionally describe operating-system capabilities instead
/// of registry handler names. Windows is free to change or localize those
/// implementation details without leaking them across the platform boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum WindowsDiskCleanupKind {
    RecycleBin,
    SystemLogs,
    InternetCache,
    DeliveryOptimization,
    DefenderCache,
    UpdateCleanup,
}

impl WindowsDiskCleanupKind {
    pub const fn stable_id(self) -> &'static str {
        match self {
            Self::RecycleBin => "recycle_bin",
            Self::SystemLogs => "system_logs",
            Self::InternetCache => "internet_cache",
            Self::DeliveryOptimization => "delivery_optimization",
            Self::DefenderCache => "defender_cache",
            Self::UpdateCleanup => "update_cleanup",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WindowsDiskCleanupAvailability {
    Ready,
    NotApplicable,
    Limited,
}

/// Read-only result returned by Windows' registered disk-cleanup handlers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WindowsDiskCleanupEstimate {
    pub kind: WindowsDiskCleanupKind,
    pub availability: WindowsDiskCleanupAvailability,
    pub bytes: u64,
    pub item_count: u64,
    pub elapsed_ms: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WindowsDiskCleanupExecutionStatus {
    Completed,
    Partial,
    /// Native cleanup ran, but the read-only reconciliation could not confirm
    /// the final released space.
    ///
    /// Keep this distinct from `Failed`: irreversible effects may already have
    /// occurred, so callers must not claim that no files were removed.
    VerificationFailed,
    Failed,
    Cancelled,
}

/// Verified execution result from the native Windows cleanup boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WindowsDiskCleanupExecution {
    pub kind: WindowsDiskCleanupKind,
    pub status: WindowsDiskCleanupExecutionStatus,
    pub bytes_expected: u64,
    pub released_bytes: u64,
    pub affected_item_count: u64,
    pub failed_item_count: u64,
}

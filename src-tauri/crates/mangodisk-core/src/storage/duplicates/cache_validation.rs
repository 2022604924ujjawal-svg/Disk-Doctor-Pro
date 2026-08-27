use std::{
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    thread::{self, JoinHandle},
};

use mangodisk_platform::{
    current_platform, FilesystemChangeMonitor, FilesystemChangeStatus, Platform,
};

use crate::shared::operation::{OperationGuard, OPERATION_CANCELLED_ERROR};

use super::hash_cache::DuplicateHashCacheRoot;

pub(super) struct DuplicateCacheValidation {
    pub(super) roots: Vec<DuplicateHashCacheRoot>,
    monitors: Vec<FilesystemChangeMonitor>,
}

pub(super) struct PendingDuplicateCacheValidation {
    operation_id: u64,
    stop: Arc<AtomicBool>,
    handle: Option<JoinHandle<Result<Option<DuplicateCacheValidation>, String>>>,
}

#[derive(Clone, Copy)]
pub(super) enum CacheValidationPhase {
    ExistingSnapshotStart,
    ExistingSnapshotEnd,
    FreshSnapshotStart,
    FreshSnapshotEnd,
}

impl CacheValidationPhase {
    const fn as_str(self) -> &'static str {
        match self {
            Self::ExistingSnapshotStart => "existing_start",
            Self::ExistingSnapshotEnd => "existing_end",
            Self::FreshSnapshotStart => "fresh_start",
            Self::FreshSnapshotEnd => "fresh_end",
        }
    }
}

impl PendingDuplicateCacheValidation {
    /// Filesystem history catch-up can be slow and is independent from enumeration. Starting it
    /// in one bounded worker overlaps read-only validation with the scan without authorizing any
    /// cached digest until the caller joins and verifies the result.
    pub(super) fn start(
        roots: Vec<DuplicateHashCacheRoot>,
        operation: &OperationGuard,
        phase: CacheValidationPhase,
    ) -> Option<Self> {
        let operation_id = operation.id();
        let operation_cancelled = operation.cancellation_flag();
        let stop = Arc::new(AtomicBool::new(false));
        let worker_stop = Arc::clone(&stop);
        let handle = thread::Builder::new()
            .name("mangodisk-duplicate-cache-validation".to_string())
            .spawn(move || {
                start_duplicate_cache_validation_with_cancel(
                    &roots,
                    operation_id,
                    &|| {
                        operation_cancelled.load(Ordering::Relaxed)
                            || worker_stop.load(Ordering::Relaxed)
                    },
                    phase,
                )
            });
        match handle {
            Ok(handle) => Some(Self {
                operation_id,
                stop,
                handle: Some(handle),
            }),
            Err(error) => {
                log_cache_error(operation_id, "spawn_validation", &error.to_string());
                None
            }
        }
    }

    pub(super) fn finish(
        mut self,
        operation: &OperationGuard,
    ) -> Result<Option<DuplicateCacheValidation>, String> {
        let Some(handle) = self.handle.take() else {
            return Ok(None);
        };
        let joined = handle.join();
        operation
            .ensure_not_cancelled()
            .map_err(|error| error.to_string())?;
        match joined {
            Ok(Ok(validation)) => Ok(validation),
            Ok(Err(error)) => {
                log_cache_error(self.operation_id, "join_validation", &error);
                Ok(None)
            }
            Err(_) => {
                log_cache_error(
                    self.operation_id,
                    "join_validation",
                    "duplicate cache validation worker panicked",
                );
                Ok(None)
            }
        }
    }
}

impl Drop for PendingDuplicateCacheValidation {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        let Some(handle) = self.handle.take() else {
            return;
        };
        if handle.join().is_err() {
            log_cache_error(
                self.operation_id,
                "drop_validation",
                "duplicate cache validation worker panicked",
            );
        }
    }
}

pub(super) fn capture_duplicate_cache_roots(
    roots: &[PathBuf],
    operation: &OperationGuard,
) -> Option<Vec<DuplicateHashCacheRoot>> {
    let mut cache_roots = Vec::with_capacity(roots.len());
    for root in roots {
        if operation.ensure_not_cancelled().is_err() {
            return None;
        }
        match current_platform().capture_filesystem_change_token(root) {
            Ok(Some(change_token)) => cache_roots.push(DuplicateHashCacheRoot {
                path: root.clone(),
                change_token,
            }),
            Ok(None) => {
                log::info!(
                    "duplicate_hash_cache_token_unavailable operation_id={} reason=unsupported",
                    operation.id()
                );
                return None;
            }
            Err(error) => {
                log_cache_error(operation.id(), "capture_token", error.diagnostic());
                return None;
            }
        }
    }
    Some(cache_roots)
}

pub(super) fn start_duplicate_cache_validation(
    roots: &[DuplicateHashCacheRoot],
    operation: &OperationGuard,
    phase: CacheValidationPhase,
) -> Result<Option<DuplicateCacheValidation>, String> {
    start_duplicate_cache_validation_with_cancel(
        roots,
        operation.id(),
        &|| operation.cancelled().load(Ordering::Relaxed),
        phase,
    )
}

fn start_duplicate_cache_validation_with_cancel(
    roots: &[DuplicateHashCacheRoot],
    operation_id: u64,
    is_cancelled: &(dyn Fn() -> bool + Sync),
    phase: CacheValidationPhase,
) -> Result<Option<DuplicateCacheValidation>, String> {
    let mut monitors = Vec::with_capacity(roots.len());
    for root in roots {
        if is_cancelled() {
            return Err(OPERATION_CANCELLED_ERROR.to_string());
        }
        let started = current_platform().start_filesystem_change_monitor(
            &root.path,
            &root.change_token,
            is_cancelled,
        );
        if is_cancelled() {
            return Err(OPERATION_CANCELLED_ERROR.to_string());
        }
        let monitor = match started {
            Ok(Some(monitor)) => monitor,
            Ok(None) => {
                log_cache_validation(operation_id, phase, "unsupported", roots.len());
                return Ok(None);
            }
            Err(error) => {
                log_cache_error(operation_id, phase.as_str(), error.diagnostic());
                return Ok(None);
            }
        };
        if monitor.status() != FilesystemChangeStatus::Clean {
            log_cache_validation(
                operation_id,
                phase,
                change_status_name(monitor.status()),
                roots.len(),
            );
            return Ok(None);
        }
        monitors.push(monitor);
    }
    log_cache_validation(operation_id, phase, "clean", roots.len());
    Ok(Some(DuplicateCacheValidation {
        roots: roots.to_vec(),
        monitors,
    }))
}

pub(super) fn finish_duplicate_cache_validation(
    validation: &DuplicateCacheValidation,
    operation: &OperationGuard,
    phase: CacheValidationPhase,
) -> Result<bool, String> {
    for (root, monitor) in validation.roots.iter().zip(&validation.monitors) {
        operation
            .ensure_not_cancelled()
            .map_err(|error| error.to_string())?;
        let status = if monitor.continuously_tracks_changes() {
            monitor.status()
        } else {
            let repeated = current_platform().start_filesystem_change_monitor(
                &root.path,
                &root.change_token,
                &|| operation.cancelled().load(Ordering::Relaxed),
            );
            operation
                .ensure_not_cancelled()
                .map_err(|error| error.to_string())?;
            match repeated {
                Ok(Some(repeated)) => repeated.status(),
                Ok(None) => {
                    log_cache_validation(
                        operation.id(),
                        phase,
                        "unsupported",
                        validation.roots.len(),
                    );
                    return Ok(false);
                }
                Err(error) => {
                    log_cache_error(operation.id(), phase.as_str(), error.diagnostic());
                    return Ok(false);
                }
            }
        };
        if status != FilesystemChangeStatus::Clean {
            log_cache_validation(
                operation.id(),
                phase,
                change_status_name(status),
                validation.roots.len(),
            );
            return Ok(false);
        }
    }
    log_cache_validation(operation.id(), phase, "clean", validation.roots.len());
    Ok(true)
}

fn log_cache_validation(
    operation_id: u64,
    phase: CacheValidationPhase,
    status: &str,
    root_count: usize,
) {
    log::info!(
        "duplicate_hash_cache_validation operation_id={} phase={} status={} root_count={}",
        operation_id,
        phase.as_str(),
        status,
        root_count
    );
}

pub(super) fn log_cache_error(operation_id: u64, stage: &str, error: &str) {
    log::warn!(
        "duplicate_hash_cache_error operation_id={} stage={} error_digest={}",
        operation_id,
        stage,
        blake3::hash(error.as_bytes()).to_hex()
    );
}

const fn change_status_name(status: FilesystemChangeStatus) -> &'static str {
    match status {
        FilesystemChangeStatus::Pending => "pending",
        FilesystemChangeStatus::Clean => "clean",
        FilesystemChangeStatus::Changed => "changed",
        FilesystemChangeStatus::HistoryUnavailable => "history_unavailable",
    }
}

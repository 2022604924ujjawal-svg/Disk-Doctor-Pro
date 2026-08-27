use std::{path::Path, time::Instant};

use crate::{
    filesystem::{
        metadata::{diagnostic_path, now_ms},
        permanent_delete::delete_file_candidate_permanently,
        PermanentDeleteBatchResult, PermanentDeleteFailure,
    },
    history::{file_cleanup_record, FileCleanupHistoryCategory, HistoryService},
    shared::{
        operation::{CoordinatedOperationKind, OperationGuard},
        CoreResult, TraversalProgress,
    },
    storage::index::cache,
    storage::large_files::LargeFilesResult,
    storage::traversal::{LargeFileScanDiagnostics, StorageTraversal},
    ProgressSink,
};

use super::session::{
    publish_result_session, resolve_delete_candidates, resolve_open_target,
    synchronize_removed_paths,
};

pub struct LargeFileService;

impl LargeFileService {
    pub fn find_with_progress(
        path: Option<String>,
        minimum_bytes: u64,
        refresh: bool,
        callback: impl ProgressSink,
    ) -> CoreResult<LargeFilesResult> {
        let result = StorageTraversal::find_large_files_with_progress(
            path,
            minimum_bytes,
            refresh,
            move |progress| callback.report(progress),
        )?;
        Ok(publish_result_session(result)?)
    }

    pub(crate) fn find_with_diagnostics(
        path: Option<String>,
        minimum_bytes: u64,
        refresh: bool,
        callback: impl Fn(TraversalProgress) + Send + Sync + 'static,
    ) -> CoreResult<(LargeFilesResult, LargeFileScanDiagnostics)> {
        StorageTraversal::find_large_files_with_diagnostics(path, minimum_bytes, refresh, callback)
    }

    pub fn cancel() {
        StorageTraversal::cancel_large_files();
    }

    pub fn resolve_open_target(scan_id: u64, selected_path: String) -> CoreResult<String> {
        Ok(resolve_open_target(scan_id, &selected_path)?)
    }

    pub fn delete_files_permanently(
        scan_id: u64,
        selected_paths: Vec<String>,
    ) -> CoreResult<PermanentDeleteBatchResult> {
        let candidates = resolve_delete_candidates(scan_id, selected_paths)?;
        let operation = OperationGuard::start(CoordinatedOperationKind::PermanentDelete)?;
        let started = Instant::now();
        let started_at_ms = now_ms();
        let requested_count = candidates.len();
        let expected_bytes = candidates
            .iter()
            .map(|candidate| candidate.expected_bytes)
            .sum();
        let selected_paths = candidates
            .iter()
            .map(|candidate| candidate.path.clone())
            .collect::<Vec<_>>();
        let path_sample = candidates
            .iter()
            .take(3)
            .map(|candidate| diagnostic_path(Path::new(&candidate.path)))
            .collect::<Vec<_>>();
        let mut result = PermanentDeleteBatchResult {
            removed_paths: Vec::new(),
            failed: Vec::new(),
            released_bytes: 0,
        };
        for candidate in candidates {
            match delete_file_candidate_permanently(&candidate) {
                Ok((target, bytes)) => {
                    result.released_bytes = result.released_bytes.saturating_add(bytes);
                    result.removed_paths.push(candidate.path);
                    cache::remove_entry(&target, bytes, 1, false);
                }
                Err(error) => result.failed.push(PermanentDeleteFailure {
                    path: candidate.path,
                    message: error.to_string(),
                }),
            }
        }
        synchronize_removed_paths(scan_id, &result.removed_paths, result.released_bytes)?;
        let history_record = file_cleanup_record(
            format!("large-file-cleanup-{}-{}", operation.id(), now_ms()),
            FileCleanupHistoryCategory::LargeFiles,
            started_at_ms,
            now_ms(),
            selected_paths,
            expected_bytes,
            &result,
        );
        if let Err(error) = HistoryService::append(history_record) {
            log::warn!(
                "large_file_history_save_failed operation_id={} error_digest={}",
                operation.id(),
                blake3::hash(error.diagnostic().as_bytes()).to_hex()
            );
        }
        log::info!(
            "permanent_delete_batch_finished operation_id={} scan_id={} requested_count={} path_sample={:?} removed_count={} failed_count={} released_bytes={} elapsed_ms={}",
            operation.id(),
            scan_id,
            requested_count,
            path_sample,
            result.removed_paths.len(),
            result.failed.len(),
            result.released_bytes,
            started.elapsed().as_millis()
        );
        operation.complete();
        Ok(result)
    }
}

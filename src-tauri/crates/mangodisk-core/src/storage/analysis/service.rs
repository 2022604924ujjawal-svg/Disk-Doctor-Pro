use std::time::Instant;

use crate::{
    filesystem::{
        metadata::diagnostic_path, permanent_delete::delete_analysis_candidate_permanently,
    },
    shared::{
        operation::{CoordinatedOperationKind, OperationGuard},
        CoreResult, TraversalProgress,
    },
    storage::traversal::{AnalysisScanDiagnostics, StorageTraversal},
    storage::{
        analysis::{AnalysisDeleteResult, AnalysisResult},
        index::cache,
    },
    ProgressSink,
};

use super::session::{publish_result_session, resolve_entry_candidate, synchronize_removed_path};

pub struct AnalysisService;

impl AnalysisService {
    pub fn analyze_with_progress(
        path: Option<String>,
        refresh: bool,
        callback: impl ProgressSink,
    ) -> CoreResult<AnalysisResult> {
        let result =
            StorageTraversal::analyze_path_with_progress(path, refresh, move |progress| {
                callback.report(progress);
            })?;
        Ok(publish_result_session(result)?)
    }

    pub(crate) fn analyze_with_diagnostics(
        path: Option<String>,
        refresh: bool,
        callback: impl Fn(TraversalProgress) + Send + Sync + 'static,
    ) -> CoreResult<(AnalysisResult, AnalysisScanDiagnostics)> {
        StorageTraversal::analyze_path_with_diagnostics(path, refresh, callback)
    }

    pub fn cancel() {
        StorageTraversal::cancel_analysis();
    }

    /// Resolves an external-open request against the authoritative scan snapshot.
    ///
    /// The platform adapter owns launching the system handler. Core only proves
    /// that the requested path was published to the current UI by a real scan.
    pub fn resolve_open_target(scan_id: u64, selected_path: String) -> CoreResult<String> {
        Ok(resolve_entry_candidate(scan_id, &selected_path)?.path)
    }

    pub fn delete_entry_permanently(
        scan_id: u64,
        selected_path: String,
    ) -> CoreResult<AnalysisDeleteResult> {
        let candidate = resolve_entry_candidate(scan_id, &selected_path)?;
        let operation = OperationGuard::start(CoordinatedOperationKind::PermanentDelete)?;
        let started = Instant::now();
        let is_directory = candidate.is_directory;
        let outcome = match delete_analysis_candidate_permanently(candidate) {
            Ok(outcome) => outcome,
            Err(error) => {
                if error.is_partial() {
                    // A partially changed directory no longer matches any derived index snapshot.
                    // Clearing the rebuildable cache prevents stale sizes from surviving the
                    // irreversible boundary.
                    if let Err(cache_error) = cache::clear_all() {
                        log::error!(
                            "analysis_partial_delete_cache_clear_failed operation_id={} scan_id={} error_digest={}",
                            operation.id(),
                            scan_id,
                            blake3::hash(cache_error.to_string().as_bytes()).to_hex()
                        );
                    }
                }
                log::warn!(
                    "analysis_permanent_delete_failed operation_id={} scan_id={} partial={} released_bytes={} error_digest={}",
                    operation.id(),
                    scan_id,
                    error.is_partial(),
                    error.released_bytes(),
                    blake3::hash(error.to_string().as_bytes()).to_hex()
                );
                let mut core_error = crate::shared::CoreError::operation_failed(error.to_string());
                if let Some(reason) = error.reason() {
                    core_error = core_error.with_reason(reason);
                }
                return Err(core_error);
            }
        };
        cache::remove_entry(
            &outcome.target,
            outcome.result.released_bytes,
            outcome.result.removed_file_count,
            is_directory,
        );
        synchronize_removed_path(scan_id, &outcome.target, outcome.result.released_bytes)?;
        log::info!(
            "analysis_permanent_delete_finished operation_id={} scan_id={} path={} entry_kind={} released_bytes={} file_count={} elapsed_ms={}",
            operation.id(),
            scan_id,
            diagnostic_path(&outcome.target),
            if is_directory { "directory" } else { "file" },
            outcome.result.released_bytes,
            outcome.result.removed_file_count,
            started.elapsed().as_millis()
        );
        operation.complete();
        Ok(outcome.result)
    }
}

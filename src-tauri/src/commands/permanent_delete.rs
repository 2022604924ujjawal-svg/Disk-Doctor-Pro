use std::path::Path;

use mangodisk_core::{
    diagnostic_path, AnalysisDeleteResult, AnalysisService, LargeFileService,
    PermanentDeleteBatchResult,
};

use super::error::{run_blocking, CommandResult};

#[tauri::command]
pub async fn delete_files_permanently(
    scan_id: u64,
    selected_paths: Vec<String>,
) -> CommandResult<PermanentDeleteBatchResult> {
    run_blocking("delete_files_permanently", move || {
        LargeFileService::delete_files_permanently(scan_id, selected_paths)
    })
    .await
}

#[tauri::command]
pub async fn delete_analysis_entry_permanently(
    scan_id: u64,
    selected_path: String,
) -> CommandResult<AnalysisDeleteResult> {
    let requested_path_log = diagnostic_path(Path::new(&selected_path));
    run_blocking("delete_analysis_entry_permanently", move || {
        let result = AnalysisService::delete_entry_permanently(scan_id, selected_path);
        if let Err(error) = &result {
            log::warn!("analysis_permanent_delete_failed path={requested_path_log} error={error}");
        }
        result
    })
    .await
}

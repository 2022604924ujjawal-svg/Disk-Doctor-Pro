use crate::services::folder_selection::FolderSelectionService;

use super::error::{run_blocking_value, CommandResult};

#[tauri::command]
pub async fn filter_directory_paths(paths: Vec<String>) -> CommandResult<Vec<String>> {
    run_blocking_value("filter_directory_paths", move || {
        let requested_count = paths.len();
        let outcome = FolderSelectionService::filter_directories(paths);
        log::info!(
            "dropped_paths_validated requested_count={} directory_count={} skipped_unsafe_count={}",
            requested_count,
            outcome.paths.len(),
            outcome.skipped_unsafe_count
        );
        outcome.paths
    })
    .await
}

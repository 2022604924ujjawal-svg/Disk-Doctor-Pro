use mangodisk_core::{LargeFileService, LargeFilesResult};

use crate::events;

use super::error::{run_blocking, CommandResult};

#[tauri::command]
pub async fn find_large_files(
    app: tauri::AppHandle,
    path: Option<String>,
    minimum_bytes: u64,
    refresh: bool,
) -> CommandResult<LargeFilesResult> {
    run_blocking("find_large_files", move || {
        LargeFileService::find_with_progress(path, minimum_bytes, refresh, move |progress| {
            events::emit(&app, events::LARGE_FILES_PROGRESS, progress);
        })
    })
    .await
}

#[tauri::command]
pub fn cancel_large_files() {
    LargeFileService::cancel();
}

use mangodisk_core::{
    DuplicateFileService, DuplicateFilesResult, DuplicateGroupPage, PermanentDeleteBatchResult,
    PermanentDeleteCandidate,
};

use crate::events;

use super::error::{run_blocking, CommandResult};

#[tauri::command]
pub async fn find_duplicate_files(
    app: tauri::AppHandle,
    roots: Vec<String>,
    minimum_bytes: u64,
) -> CommandResult<DuplicateFilesResult> {
    run_blocking("find_duplicate_files", move || {
        let progress_app = app.clone();
        DuplicateFileService::find_paged_with_progress(
            roots,
            minimum_bytes,
            move |progress| {
                events::emit(&progress_app, events::DUPLICATE_FILES_PROGRESS, progress);
            },
            move |batch| events::emit(&app, events::DUPLICATE_FILE_GROUPS, batch),
        )
    })
    .await
}

#[tauri::command]
pub fn cancel_duplicate_files() {
    DuplicateFileService::cancel();
}

#[tauri::command]
pub async fn get_duplicate_file_groups(
    scan_id: u64,
    offset: u64,
    limit: u64,
) -> CommandResult<DuplicateGroupPage> {
    run_blocking("get_duplicate_file_groups", move || {
        DuplicateFileService::page(scan_id, offset, limit)
    })
    .await
}

#[tauri::command]
pub async fn delete_duplicate_files_permanently(
    scan_id: u64,
    candidates: Vec<PermanentDeleteCandidate>,
) -> CommandResult<PermanentDeleteBatchResult> {
    run_blocking("delete_duplicate_files_permanently", move || {
        DuplicateFileService::delete_files_permanently(scan_id, candidates)
    })
    .await
}

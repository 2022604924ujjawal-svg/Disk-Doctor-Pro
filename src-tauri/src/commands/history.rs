use mangodisk_core::{HistoryService, OperationRecord};

use super::error::{run_blocking, CommandResult};

#[tauri::command]
pub async fn list_history() -> CommandResult<Vec<OperationRecord>> {
    run_blocking("list_history", HistoryService::list).await
}

#[tauri::command]
pub async fn clear_history() -> CommandResult<()> {
    run_blocking("clear_history", HistoryService::clear).await
}

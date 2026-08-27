use mangodisk_core::{AnalysisResult, AnalysisService};

use crate::events;

use super::error::{run_blocking, CommandResult};

#[tauri::command]
pub async fn analyze_path(
    app: tauri::AppHandle,
    path: Option<String>,
    refresh: bool,
) -> CommandResult<AnalysisResult> {
    run_blocking("analyze_path", move || {
        AnalysisService::analyze_with_progress(path, refresh, move |progress| {
            events::emit(&app, events::ANALYSIS_PROGRESS, progress);
        })
    })
    .await
}

#[tauri::command]
pub fn cancel_analysis() {
    AnalysisService::cancel();
}

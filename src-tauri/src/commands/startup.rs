use mangodisk_core::{
    StartupCatalog, StartupChangePlan, StartupChangeResult, StartupChangeSelection, StartupService,
};

use super::error::{run_blocking, CommandResult};
#[tauri::command]
pub async fn scan_startup_catalog() -> CommandResult<StartupCatalog> {
    run_blocking("scan_startup_catalog", StartupService::scan).await
}

#[tauri::command]
pub fn cancel_startup_catalog_scan() {
    StartupService::cancel_scan();
}

#[tauri::command]
pub fn cancel_startup_change() {
    StartupService::cancel_change();
}

#[tauri::command]
pub async fn prepare_startup_change(
    selection: StartupChangeSelection,
) -> CommandResult<StartupChangePlan> {
    run_blocking("prepare_startup_change", move || {
        StartupService::prepare_change(selection)
    })
    .await
}

#[tauri::command]
pub async fn execute_startup_change(
    plan_id: String,
    authorization_prompt: String,
) -> CommandResult<StartupChangeResult> {
    run_blocking("execute_startup_change", move || {
        StartupService::execute_change(plan_id, Some(&authorization_prompt))
    })
    .await
}

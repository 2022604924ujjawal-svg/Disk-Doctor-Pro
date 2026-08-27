use mangodisk_core::{
    ApplicationCloseBatchResult, CleanupApplicationCloseRequest, CleanupRequest, CleanupResult,
    CleanupScanResult, CleanupScanService, CleanupService,
};
use serde::Deserialize;

use crate::events;

use super::error::{run_blocking, CommandResult};

#[derive(Debug, Default, Deserialize)]
#[serde(
    tag = "mode",
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum CleanupScanScope {
    #[default]
    Standard,
    SelectedVolumes {
        volume_mount_points: Vec<String>,
    },
}

#[tauri::command]
pub async fn scan_cleanup_candidates(
    app: tauri::AppHandle,
    scan_scope: Option<CleanupScanScope>,
) -> CommandResult<CleanupScanResult> {
    run_blocking("scan_cleanup_candidates", move || {
        let progress = move |value| events::emit(&app, events::CLEANUP_SCAN_PROGRESS, value);
        match scan_scope.unwrap_or_default() {
            CleanupScanScope::Standard => CleanupScanService::scan_with_progress(progress),
            CleanupScanScope::SelectedVolumes {
                volume_mount_points,
            } => CleanupScanService::scan_with_selected_volumes(volume_mount_points, progress),
        }
    })
    .await
}

#[tauri::command]
pub fn cancel_cleanup_scan() {
    CleanupScanService::cancel();
}

#[tauri::command]
pub fn cancel_cleanup_execution() {
    CleanupService::cancel();
}

#[tauri::command]
pub async fn close_cleanup_applications(
    request: CleanupApplicationCloseRequest,
) -> CommandResult<ApplicationCloseBatchResult> {
    run_blocking("close_cleanup_applications", move || {
        CleanupService::close_applications(request)
    })
    .await
}

#[tauri::command]
pub async fn execute_cleanup(
    app: tauri::AppHandle,
    mut request: CleanupRequest,
    scan_scope: Option<CleanupScanScope>,
    deep_cleanup_operation_id: String,
) -> CommandResult<CleanupResult> {
    // Arbitrary WebView paths remain untrusted. Execution replaces them with
    // mount points matched against the live platform inventory so the selected
    // volume scope can be reproduced without widening it to arbitrary folders.
    request.project_roots.clear();
    run_blocking("execute_cleanup", move || {
        let progress = move |value| {
            events::emit(&app, events::CLEANUP_EXECUTION_PROGRESS, value);
        };
        match scan_scope.unwrap_or_default() {
            CleanupScanScope::Standard => CleanupService::execute_deep_cleanup_step_with_progress(
                request,
                deep_cleanup_operation_id,
                progress,
            ),
            CleanupScanScope::SelectedVolumes {
                volume_mount_points,
            } => {
                request.project_roots = volume_mount_points;
                CleanupService::execute_deep_cleanup_step_with_selected_volumes_and_progress(
                    request,
                    deep_cleanup_operation_id,
                    progress,
                )
            }
        }
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::CleanupScanScope;

    #[test]
    fn selected_volume_scope_accepts_the_frontend_payload_shape() {
        let scope = serde_json::from_value::<CleanupScanScope>(serde_json::json!({
            "mode": "selectedVolumes",
            "volumeMountPoints": ["C:\\"]
        }))
        .expect("deserialize the frontend scan scope");

        assert!(matches!(scope, CleanupScanScope::SelectedVolumes { .. }));
    }
}

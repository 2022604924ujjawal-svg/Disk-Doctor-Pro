use mangodisk_core::{
    SystemSettingsCatalog, SystemSettingsChangePlan, SystemSettingsChangeResult,
    SystemSettingsChangeSelection, SystemSettingsService,
};
use serde::Deserialize;

use super::error::{run_blocking, CommandResult};

#[tauri::command]
pub async fn scan_system_settings() -> CommandResult<SystemSettingsCatalog> {
    run_blocking("scan_system_settings", SystemSettingsService::scan).await
}

#[tauri::command]
pub fn cancel_system_settings_scan() {
    SystemSettingsService::cancel_scan();
}

#[tauri::command]
pub async fn prepare_system_settings_change(
    selection: SystemSettingsChangeSelection,
) -> CommandResult<SystemSettingsChangePlan> {
    run_blocking("prepare_system_settings_change", move || {
        SystemSettingsService::prepare_change(selection)
    })
    .await
}

#[tauri::command]
pub async fn execute_system_settings_change(
    plan_id: String,
) -> CommandResult<SystemSettingsChangeResult> {
    run_blocking("execute_system_settings_change", move || {
        SystemSettingsService::execute_change(plan_id)
    })
    .await
}

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum MacOsPrivacyDestination {
    ApplicationData,
    FilesAndFolders,
    FullDiskAccess,
}

impl MacOsPrivacyDestination {
    fn settings_uri(self) -> &'static str {
        match self {
            Self::ApplicationData => {
                "x-apple.systempreferences:com.apple.settings.PrivacySecurity.extension?Privacy_AppData"
            }
            Self::FilesAndFolders => {
                "x-apple.systempreferences:com.apple.settings.PrivacySecurity.extension?Privacy_FilesAndFolders"
            }
            Self::FullDiskAccess => {
                "x-apple.systempreferences:com.apple.settings.PrivacySecurity.extension?Privacy_AllFiles"
            }
        }
    }

    fn diagnostic_name(self) -> &'static str {
        match self {
            Self::ApplicationData => "application_data",
            Self::FilesAndFolders => "files_and_folders",
            Self::FullDiskAccess => "full_disk_access",
        }
    }
}

/// Opens only a known macOS privacy destination. Keeping the URI mapping in
/// the Tauri adapter avoids granting the webview access to arbitrary custom
/// schemes and keeps operating-system navigation out of the Core domains.
#[tauri::command]
pub async fn open_privacy_settings(destination: MacOsPrivacyDestination) -> CommandResult<()> {
    run_blocking("open_macos_privacy_settings", move || {
        log::info!(
            "macos_privacy_settings_open_requested destination={}",
            destination.diagnostic_name()
        );
        open_settings_uri(destination.settings_uri())
    })
    .await
}

/// Opens the fixed Login Items pane without exposing an arbitrary URL opener
/// to the webview. System-managed background items can only be changed there.
#[tauri::command]
pub async fn open_macos_login_items_settings() -> CommandResult<()> {
    run_blocking("open_macos_login_items_settings", || {
        log::info!("macos_login_items_settings_open_requested");
        open_settings_uri("x-apple.systempreferences:com.apple.LoginItems-Settings.extension")
    })
    .await
}

#[cfg(target_os = "macos")]
fn open_settings_uri(uri: &str) -> Result<(), String> {
    tauri_plugin_opener::open_url(uri, None::<&str>)
        .map_err(|error| format!("failed to open macOS System Settings: {error}"))
}

#[cfg(not(target_os = "macos"))]
fn open_settings_uri(_uri: &str) -> Result<(), String> {
    Err("macOS privacy settings are unavailable on this platform".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_destination_maps_to_a_fixed_privacy_uri() {
        for destination in [
            MacOsPrivacyDestination::ApplicationData,
            MacOsPrivacyDestination::FilesAndFolders,
            MacOsPrivacyDestination::FullDiskAccess,
        ] {
            let uri = destination.settings_uri();
            assert!(uri.starts_with(
                "x-apple.systempreferences:com.apple.settings.PrivacySecurity.extension?Privacy_"
            ));
            assert!(!uri.contains([' ', '\n', '\r']));
        }
    }

    #[test]
    fn login_items_destination_is_a_fixed_settings_uri() {
        let uri = "x-apple.systempreferences:com.apple.LoginItems-Settings.extension";
        assert!(uri.starts_with("x-apple.systempreferences:"));
        assert!(!uri.contains([' ', '\n', '\r']));
    }
}

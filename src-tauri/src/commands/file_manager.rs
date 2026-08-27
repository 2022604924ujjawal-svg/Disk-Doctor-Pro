use std::{fmt::Display, path::Path};

use mangodisk_core::{diagnostic_path, AnalysisService, DuplicateFileService, LargeFileService};
use tauri::{AppHandle, Manager};
use tauri_plugin_opener::OpenerExt;

use super::error::{into_command_result, run_blocking, CommandResult};

#[tauri::command]
pub async fn reveal_in_file_manager(path: String) -> CommandResult<()> {
    run_blocking("reveal_in_file_manager", move || {
        let path_log = diagnostic_path(Path::new(&path));
        log::info!("file_manager_reveal_requested path={path_log}");
        match tauri_plugin_opener::reveal_item_in_dir(Path::new(&path)) {
            Ok(()) => {
                log::info!("file_manager_reveal_finished path={path_log}");
                Ok(())
            }
            Err(error) => {
                let diagnostic = opener_error_diagnostic(&error);
                log::warn!("file_manager_reveal_failed path={path_log} {diagnostic}");
                Err(diagnostic)
            }
        }
    })
    .await
}

#[tauri::command]
pub async fn open_analysis_entry(scan_id: u64, selected_path: String) -> CommandResult<()> {
    let target = into_command_result(
        "open_analysis_entry",
        AnalysisService::resolve_open_target(scan_id, selected_path),
    )?;
    open_scanned_entry("open_analysis_entry", "analysis", scan_id, target).await
}

#[tauri::command]
pub async fn open_large_file_entry(scan_id: u64, selected_path: String) -> CommandResult<()> {
    let target = into_command_result(
        "open_large_file_entry",
        LargeFileService::resolve_open_target(scan_id, selected_path),
    )?;
    open_scanned_entry("open_large_file_entry", "large_files", scan_id, target).await
}

#[tauri::command]
pub async fn open_duplicate_file_entry(scan_id: u64, selected_path: String) -> CommandResult<()> {
    let target = into_command_result(
        "open_duplicate_file_entry",
        DuplicateFileService::resolve_open_target(scan_id, selected_path),
    )?;
    open_scanned_entry(
        "open_duplicate_file_entry",
        "duplicate_files",
        scan_id,
        target,
    )
    .await
}

async fn open_scanned_entry(
    operation: &'static str,
    domain: &'static str,
    scan_id: u64,
    target: String,
) -> CommandResult<()> {
    run_blocking(operation, move || {
        let path_log = diagnostic_path(Path::new(&target));
        log::info!(
            "storage_entry_open_requested domain={domain} scan_id={scan_id} path={path_log}"
        );
        match open_path_with_default_handler(Path::new(&target)) {
            Ok(()) => {
                log::info!(
                    "storage_entry_open_dispatched domain={domain} scan_id={scan_id} path={path_log}"
                );
                Ok(())
            }
            Err(error) => {
                let diagnostic = open_error_diagnostic(&error);
                log::warn!(
                    "storage_entry_open_failed domain={domain} scan_id={scan_id} path={path_log} {diagnostic}"
                );
                Err(diagnostic)
            }
        }
    })
    .await
}

fn open_path_with_default_handler(path: &Path) -> Result<(), tauri_plugin_opener::Error> {
    tauri_plugin_opener::open_path(path, None::<&str>)
}

/// Opens MangoDisk's application-owned log directory without exposing its
/// platform-specific path to the webview. Directory resolution stays in the
/// Tauri adapter because it depends on the installed application identity.
#[tauri::command]
pub fn open_application_log_directory(app: AppHandle) -> CommandResult<()> {
    // Resolve and encode the application-owned path entirely in the backend.
    // The supported Windows and macOS directory providers return Unicode
    // application paths, while the webview never sees the resolved value.
    let directory = app
        .path()
        .app_log_dir()
        .map_err(|error| log_directory_error_diagnostic(&error))
        .and_then(|path| {
            path.into_os_string().into_string().map_err(|path| {
                log_directory_error_diagnostic(&format!("non-unicode path: {path:?}"))
            })
        });
    let result: Result<(), String> = (|| {
        let directory = directory?;
        log::info!("application_log_directory_open_requested");
        app.opener()
            .open_path(directory, None::<String>)
            .map_err(|error| log_directory_error_diagnostic(&error))?;
        log::info!("application_log_directory_open_finished");
        Ok(())
    })();
    into_command_result("open_application_log_directory", result)
}

fn opener_error_diagnostic(error: &tauri_plugin_opener::Error) -> String {
    let digest = blake3::hash(error.to_string().as_bytes()).to_hex();
    format!("opener_reveal_failed error_digest={}", &digest[..12])
}

fn open_error_diagnostic(error: &tauri_plugin_opener::Error) -> String {
    let digest = blake3::hash(error.to_string().as_bytes()).to_hex();
    format!("opener_open_failed error_digest={}", &digest[..12])
}

fn log_directory_error_diagnostic(error: &dyn Display) -> String {
    let digest = blake3::hash(error.to_string().as_bytes()).to_hex();
    format!(
        "application_log_directory_open_failed error_digest={}",
        &digest[..12]
    )
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    #[test]
    fn opener_diagnostic_does_not_expose_private_paths() {
        let private_path = PathBuf::from(r"C:\Users\Developer\Private\project\target");
        let error = tauri_plugin_opener::Error::NoParent(private_path.clone());

        let diagnostic = opener_error_diagnostic(&error);

        assert!(diagnostic.starts_with("opener_reveal_failed error_digest="));
        assert!(!diagnostic.contains(private_path.to_string_lossy().as_ref()));
        assert!(!diagnostic.contains("Developer"));
        assert!(!diagnostic.contains("Private"));
    }

    #[test]
    fn open_diagnostic_does_not_expose_private_paths() {
        let private_path = PathBuf::from(r"C:\Users\Developer\Private\project\target");
        let error = tauri_plugin_opener::Error::NoParent(private_path.clone());

        let diagnostic = open_error_diagnostic(&error);

        assert!(diagnostic.starts_with("opener_open_failed error_digest="));
        assert!(!diagnostic.contains(private_path.to_string_lossy().as_ref()));
        assert!(!diagnostic.contains("Developer"));
        assert!(!diagnostic.contains("Private"));
    }

    #[test]
    fn default_open_rejects_a_missing_target_before_dispatch() {
        let missing_path = std::env::temp_dir().join(format!(
            "mangodisk-missing-open-target-{}",
            std::process::id()
        ));
        assert!(!missing_path.exists(), "the test target must remain absent");

        assert!(open_path_with_default_handler(&missing_path).is_err());
    }

    #[test]
    fn log_directory_diagnostic_does_not_expose_private_paths() {
        let private_path = r"C:\Users\Developer\AppData\Local\MangoDisk\logs";

        let diagnostic = log_directory_error_diagnostic(&format!("cannot open {private_path}"));

        assert!(diagnostic.starts_with("application_log_directory_open_failed error_digest="));
        assert!(!diagnostic.contains(private_path));
        assert!(!diagnostic.contains("Developer"));
    }
}

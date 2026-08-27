use std::time::Instant;

use base64::{engine::general_purpose::STANDARD, Engine as _};
use mangodisk_platform::{
    NativeFileIconItemKind, NativeFileIconMode, NativeFileIconRequest, NativeFileIconService,
};
use serde::{Deserialize, Serialize};
use tauri::Manager;

use super::error::{run_blocking_value, CommandResult};

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileIconRequest {
    path: String,
    kind: FileIconItemKind,
    mode: FileIconMode,
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
enum FileIconItemKind {
    File,
    Directory,
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
enum FileIconMode {
    Automatic,
    Generic,
    Path,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FileIconAssignment {
    path: String,
    kind: FileIconItemKind,
    mode: FileIconMode,
    icon_key: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FileIconAsset {
    icon_key: String,
    data_url: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FileIconBatch {
    assignments: Vec<FileIconAssignment>,
    assets: Vec<FileIconAsset>,
}

/// Returns a compact batch: each path references an icon identity, while each
/// PNG is transferred only once even when many visible rows share a file type.
#[tauri::command]
pub async fn get_file_icons(
    app: tauri::AppHandle,
    requests: Vec<FileIconRequest>,
) -> CommandResult<FileIconBatch> {
    let requested_count = requests.len().min(NativeFileIconService::MAX_REQUESTS);
    let requests = requests
        .into_iter()
        .map(|request| NativeFileIconRequest {
            path: request.path,
            kind: match request.kind {
                FileIconItemKind::File => NativeFileIconItemKind::File,
                FileIconItemKind::Directory => NativeFileIconItemKind::Directory,
            },
            mode: match request.mode {
                FileIconMode::Automatic => NativeFileIconMode::Automatic,
                FileIconMode::Generic => NativeFileIconMode::Generic,
                FileIconMode::Path => NativeFileIconMode::Path,
            },
        })
        .collect();
    let cache_root = app
        .path()
        .app_cache_dir()
        .map(|path| path.join("cache").join("file-icons"))
        .map_err(|error| {
            log::warn!("file_icon_cache_path_unavailable error={error}");
            error
        })
        .ok();
    let started_at = Instant::now();
    let result = run_blocking_value("get_file_icons", move || {
        NativeFileIconService::load(requests, cache_root)
    })
    .await?;

    log::info!(
        "file_icons_loaded requested={} identities={} assignments={} assets={} cache_hits={} system_lookups={} elapsed_ms={}",
        requested_count,
        result.unique_identities,
        result.assignments.len(),
        result.assets.len(),
        result.cache_hits,
        result.system_lookups,
        started_at.elapsed().as_millis()
    );

    Ok(FileIconBatch {
        assignments: result
            .assignments
            .into_iter()
            .map(|assignment| FileIconAssignment {
                path: assignment.path,
                kind: match assignment.kind {
                    NativeFileIconItemKind::File => FileIconItemKind::File,
                    NativeFileIconItemKind::Directory => FileIconItemKind::Directory,
                },
                mode: match assignment.mode {
                    NativeFileIconMode::Automatic => FileIconMode::Automatic,
                    NativeFileIconMode::Generic => FileIconMode::Generic,
                    NativeFileIconMode::Path => FileIconMode::Path,
                },
                icon_key: assignment.icon_key,
            })
            .collect(),
        assets: result
            .assets
            .into_iter()
            .map(|asset| FileIconAsset {
                icon_key: asset.icon_key,
                data_url: format!("data:image/png;base64,{}", STANDARD.encode(asset.png)),
            })
            .collect(),
    })
}

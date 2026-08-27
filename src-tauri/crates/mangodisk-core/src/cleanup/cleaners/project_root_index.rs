use std::{
    fs::{self, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
    sync::{Mutex, OnceLock},
};

use mangodisk_platform::{current_platform, Platform};
use serde::{Deserialize, Serialize};

use crate::{filesystem::metadata::display_path, shared::application_paths};

const INDEX_SCHEMA_VERSION: u32 = 1;
const INDEX_FILE_NAME: &str = "project-roots.json";
const INDEX_TEMPORARY_FILE_NAME: &str = "project-roots.json.tmp";
const MAX_INDEX_BYTES: u64 = 4 * 1024 * 1024;
const MAX_INDEXED_ROOTS: usize = 10_000;

static INDEX_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ProjectRootIndexDocument {
    schema_version: u32,
    roots: Vec<String>,
}

pub(super) fn load() -> Result<Vec<PathBuf>, String> {
    let _guard = index_lock()
        .lock()
        .map_err(|_| "project root index is temporarily unavailable".to_string())?;
    let path = index_path()?;
    let metadata = match fs::symlink_metadata(&path) {
        Ok(metadata) if metadata.is_file() && !current_platform().is_link_like(&metadata) => {
            metadata
        }
        Ok(_) => return Err("project root index is not a regular file".to_string()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(format!("failed to inspect the project root index: {error}")),
    };
    if metadata.len() > MAX_INDEX_BYTES {
        return Err("project root index exceeds the supported size".to_string());
    }
    let content = fs::read(&path)
        .map_err(|error| format!("failed to read the project root index: {error}"))?;
    let document = serde_json::from_slice::<ProjectRootIndexDocument>(&content)
        .map_err(|error| format!("project root index has an invalid format: {error}"))?;
    if document.schema_version != INDEX_SCHEMA_VERSION {
        return Ok(Vec::new());
    }
    Ok(document
        .roots
        .into_iter()
        .take(MAX_INDEXED_ROOTS)
        .map(PathBuf::from)
        .collect())
}

pub(super) fn save(roots: &[PathBuf]) -> Result<(), String> {
    let _guard = index_lock()
        .lock()
        .map_err(|_| "project root index is temporarily unavailable".to_string())?;
    let mut roots = roots.to_vec();
    roots.sort_by_key(|root| current_platform().path_identity_key(root));
    roots.dedup_by(|left, right| current_platform().paths_equal(left, right));
    roots.truncate(MAX_INDEXED_ROOTS);
    let roots = roots
        .iter()
        .map(|root| display_path(root))
        .collect::<Vec<_>>();
    let content = serde_json::to_vec_pretty(&ProjectRootIndexDocument {
        schema_version: INDEX_SCHEMA_VERSION,
        roots,
    })
    .map_err(|error| format!("failed to serialize the project root index: {error}"))?;
    if content.len() as u64 > MAX_INDEX_BYTES {
        return Err("project root index exceeds the supported size".to_string());
    }
    let path = index_path()?;
    let temporary = path.with_file_name(INDEX_TEMPORARY_FILE_NAME);
    match fs::symlink_metadata(&temporary) {
        Ok(metadata) if metadata.is_file() && !current_platform().is_link_like(&metadata) => {
            fs::remove_file(&temporary)
                .map_err(|error| format!("failed to reset the project root index: {error}"))?;
        }
        Ok(_) => return Err("project root index temporary path is not a regular file".to_string()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => {
            return Err(format!(
                "failed to inspect the project root index temporary path: {error}"
            ));
        }
    }
    let mut file = OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&temporary)
        .map_err(|error| format!("failed to create the project root index: {error}"))?;
    file.write_all(&content)
        .and_then(|_| file.sync_all())
        .map_err(|error| format!("failed to write the project root index: {error}"))?;
    replace_file(&temporary, &path)
        .map_err(|error| format!("failed to save the project root index: {error}"))
}

fn index_lock() -> &'static Mutex<()> {
    INDEX_LOCK.get_or_init(|| Mutex::new(()))
}

fn index_path() -> Result<PathBuf, String> {
    let directory = application_paths()
        .map_err(|error| error.to_string())?
        .data_directory();
    fs::create_dir_all(directory)
        .map_err(|error| format!("failed to create the application data directory: {error}"))?;
    current_platform()
        .validate_path_no_links(directory)
        .map_err(|error| format!("application data directory validation failed: {error}"))?;
    Ok(directory.join(INDEX_FILE_NAME))
}

#[cfg(not(windows))]
fn replace_file(source: &Path, target: &Path) -> std::io::Result<()> {
    fs::rename(source, target)
}

#[cfg(windows)]
fn replace_file(source: &Path, target: &Path) -> std::io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::{
        MoveFileExW, MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH,
    };

    let source = source
        .as_os_str()
        .encode_wide()
        .chain(Some(0))
        .collect::<Vec<_>>();
    let target = target
        .as_os_str()
        .encode_wide()
        .chain(Some(0))
        .collect::<Vec<_>>();
    let succeeded = unsafe {
        MoveFileExW(
            source.as_ptr(),
            target.as_ptr(),
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
        )
    };
    if succeeded == 0 {
        Err(std::io::Error::last_os_error())
    } else {
        Ok(())
    }
}

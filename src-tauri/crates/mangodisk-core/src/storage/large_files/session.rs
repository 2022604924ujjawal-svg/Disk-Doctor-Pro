use std::{
    collections::{HashMap, HashSet, VecDeque},
    sync::{
        atomic::{AtomicU64, Ordering},
        Mutex, MutexGuard, OnceLock,
    },
};

use super::LargeFilesResult;
use crate::filesystem::PermanentDeleteCandidate;

static NEXT_LARGE_FILE_SCAN_ID: AtomicU64 = AtomicU64::new(1);
static LARGE_FILE_RESULT_SESSIONS: OnceLock<Mutex<VecDeque<LargeFilesResult>>> = OnceLock::new();
const LARGE_FILE_RESULT_SESSION_LIMIT: usize = 8;

fn sessions() -> &'static Mutex<VecDeque<LargeFilesResult>> {
    LARGE_FILE_RESULT_SESSIONS.get_or_init(|| Mutex::new(VecDeque::new()))
}

fn lock_sessions() -> Result<MutexGuard<'static, VecDeque<LargeFilesResult>>, String> {
    sessions()
        .lock()
        .map_err(|_| "the large-file result session is unavailable".to_string())
}

pub(super) fn publish_result_session(
    mut result: LargeFilesResult,
) -> Result<LargeFilesResult, String> {
    result.scan_id = NEXT_LARGE_FILE_SCAN_ID.fetch_add(1, Ordering::Relaxed);
    let mut sessions = lock_sessions()?;
    sessions.push_front(result.clone());
    sessions.truncate(LARGE_FILE_RESULT_SESSION_LIMIT);
    Ok(result)
}

/// Restores immutable file snapshots from the authoritative scan result.
///
/// Paths identify visible rows, but every size and timestamp used for safety validation comes from
/// the Core-owned session rather than the WebView request.
pub(super) fn resolve_delete_candidates(
    scan_id: u64,
    selected_paths: Vec<String>,
) -> Result<Vec<PermanentDeleteCandidate>, String> {
    if selected_paths.is_empty() {
        return Err("at least one large file must be selected".to_string());
    }
    let mut unique_paths = HashSet::with_capacity(selected_paths.len());
    if selected_paths
        .iter()
        .any(|path| !unique_paths.insert(path.as_str()))
    {
        return Err("a large file was selected more than once".to_string());
    }

    let sessions = lock_sessions()?;
    let result = sessions
        .iter()
        .find(|result| result.scan_id == scan_id)
        .ok_or_else(|| "the large-file result session expired; scan again".to_string())?;
    let entries = result
        .entries
        .iter()
        .map(|entry| (entry.path.as_str(), entry))
        .collect::<HashMap<_, _>>();
    selected_paths
        .into_iter()
        .map(|path| {
            let entry = entries
                .get(path.as_str())
                .ok_or_else(|| "a selected file is not part of the current scan".to_string())?;
            Ok(PermanentDeleteCandidate {
                path: entry.path.clone(),
                expected_bytes: entry.bytes,
                expected_modified_at_ms: entry.modified_at_ms,
            })
        })
        .collect()
}

pub(super) fn resolve_open_target(scan_id: u64, selected_path: &str) -> Result<String, String> {
    let sessions = lock_sessions()?;
    let result = sessions
        .iter()
        .find(|result| result.scan_id == scan_id)
        .ok_or_else(|| "the large-file result session expired; scan again".to_string())?;
    result
        .entries
        .iter()
        .find(|entry| entry.path == selected_path)
        .map(|entry| entry.path.clone())
        .ok_or_else(|| "the selected file is not part of the current large-file scan".to_string())
}

pub(super) fn synchronize_removed_paths(
    scan_id: u64,
    removed_paths: &[String],
    released_bytes: u64,
) -> Result<(), String> {
    let removed_paths = removed_paths.iter().collect::<HashSet<_>>();
    let mut sessions = lock_sessions()?;
    let result = sessions
        .iter_mut()
        .find(|result| result.scan_id == scan_id)
        .ok_or_else(|| "the large-file result session expired; scan again".to_string())?;
    result
        .entries
        .retain(|entry| !removed_paths.contains(&entry.path));
    result.total_bytes = result.total_bytes.saturating_sub(released_bytes);
    result.total_count = result
        .total_count
        .saturating_sub(removed_paths.len() as u64);
    result.returned_count = u64::try_from(result.entries.len()).unwrap_or(u64::MAX);
    result.truncated = result.returned_count < result.total_count;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::large_files::LargeFileEntry;

    fn result() -> LargeFilesResult {
        LargeFilesResult {
            scan_id: 0,
            root: "/fixture".to_string(),
            scanned_at_ms: 1,
            minimum_bytes: 1,
            total_bytes: 12,
            total_count: 1,
            returned_count: 1,
            truncated: false,
            skipped_count: 0,
            cache_reused: false,
            entries: vec![LargeFileEntry {
                name: "sample.bin".to_string(),
                path: "/fixture/sample.bin".to_string(),
                parent_path: "/fixture".to_string(),
                bytes: 12,
                modified_at_ms: Some(7),
            }],
        }
    }

    #[test]
    fn large_file_candidates_are_derived_from_the_active_result() {
        let result = publish_result_session(result()).expect("publish the large-file fixture");
        let candidate =
            resolve_delete_candidates(result.scan_id, vec!["/fixture/sample.bin".to_string()])
                .expect("resolve the published file");
        assert_eq!(candidate[0].expected_bytes, 12);
        assert!(
            resolve_delete_candidates(result.scan_id, vec!["/fixture/not-scanned.bin".to_string()])
                .is_err(),
            "a fabricated path must not cross the permanent-delete boundary"
        );
    }

    #[test]
    fn large_file_open_target_is_bound_to_the_scan_result() {
        let result = publish_result_session(result()).expect("publish the large-file fixture");

        let target = resolve_open_target(result.scan_id, "/fixture/sample.bin")
            .expect("resolve the published file");
        assert_eq!(target, "/fixture/sample.bin");
        assert!(resolve_open_target(result.scan_id, "/fixture/not-scanned.bin").is_err());
    }
}

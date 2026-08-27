use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{Arc, Mutex, OnceLock},
    time::{Instant, SystemTime},
};

use mangodisk_platform::FilesystemChangeToken;

/// Retain only one recently verified scan and bound its entry count. Skipping this acceleration
/// cache changes only a later scan's duration; it never changes duplicate-file correctness.
const MAX_HASH_CACHE_ENTRIES: usize = 500_000;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct DuplicateHashCacheRoot {
    pub(super) path: PathBuf,
    pub(super) change_token: FilesystemChangeToken,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct DuplicateHashCacheFile {
    pub(super) root_ordinal: usize,
    pub(super) path: PathBuf,
    pub(super) bytes: u64,
    pub(super) modified_at: Option<SystemTime>,
    pub(super) identity: [u8; 16],
    pub(super) sample_hash: [u8; 32],
    pub(super) full_hash: Option<[u8; 32]>,
}

#[derive(Clone, Debug)]
pub(super) struct DuplicateHashCacheSnapshot {
    pub(super) roots: Vec<DuplicateHashCacheRoot>,
    pub(super) files: Arc<HashMap<PathBuf, DuplicateHashCacheFile>>,
}

#[derive(Debug, Clone, Copy, Default)]
pub(super) struct DuplicateHashCacheWriteDiagnostics {
    pub(super) entry_count: u64,
    pub(super) elapsed_ms: u64,
}

#[derive(Clone)]
struct CacheEntry {
    root_paths: Vec<PathBuf>,
    minimum_bytes: u64,
    sample_plan: String,
    snapshot: DuplicateHashCacheSnapshot,
}

fn cache() -> &'static Mutex<Option<CacheEntry>> {
    static CACHE: OnceLock<Mutex<Option<CacheEntry>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(None))
}

pub(super) fn find_snapshot(
    roots: &[PathBuf],
    minimum_bytes: u64,
    sample_plan: &str,
) -> Result<(Option<DuplicateHashCacheSnapshot>, u64), String> {
    let started = Instant::now();
    let guard = cache()
        .lock()
        .map_err(|_| "duplicate hash cache lock is unavailable".to_string())?;
    let snapshot = guard
        .as_ref()
        .filter(|entry| {
            entry.root_paths == roots
                && entry.minimum_bytes == minimum_bytes
                && entry.sample_plan == sample_plan
        })
        .map(|entry| entry.snapshot.clone());
    Ok((snapshot, elapsed_ms(started)))
}

pub(super) fn store_snapshot(
    roots: &[DuplicateHashCacheRoot],
    minimum_bytes: u64,
    sample_plan: &str,
    files: Vec<DuplicateHashCacheFile>,
    is_cancelled: impl Fn() -> bool,
) -> Result<DuplicateHashCacheWriteDiagnostics, String> {
    let started = Instant::now();
    if is_cancelled() {
        return Err(crate::shared::operation::OPERATION_CANCELLED_ERROR.to_string());
    }
    if files.len() > MAX_HASH_CACHE_ENTRIES {
        clear()?;
        log::info!(
            "duplicate_hash_cache_skipped entry_count={} limit={}",
            files.len(),
            MAX_HASH_CACHE_ENTRIES
        );
        return Ok(DuplicateHashCacheWriteDiagnostics {
            entry_count: 0,
            elapsed_ms: elapsed_ms(started),
        });
    }

    let files = files
        .into_iter()
        .map(|file| (file.path.clone(), file))
        .collect::<HashMap<_, _>>();
    let entry_count = u64::try_from(files.len()).unwrap_or(u64::MAX);
    let entry = CacheEntry {
        root_paths: roots.iter().map(|root| root.path.clone()).collect(),
        minimum_bytes,
        sample_plan: sample_plan.to_string(),
        snapshot: DuplicateHashCacheSnapshot {
            roots: roots.to_vec(),
            files: Arc::new(files),
        },
    };
    if is_cancelled() {
        return Err(crate::shared::operation::OPERATION_CANCELLED_ERROR.to_string());
    }
    *cache()
        .lock()
        .map_err(|_| "duplicate hash cache lock is unavailable".to_string())? = Some(entry);
    Ok(DuplicateHashCacheWriteDiagnostics {
        entry_count,
        elapsed_ms: elapsed_ms(started),
    })
}

/// Any successful deletion below a cached root invalidates the complete snapshot. Rebuilding on
/// demand is safer than attempting to mutate digest groups after filesystem state changed.
pub(super) fn invalidate_containing(target: &Path) {
    let Ok(mut guard) = cache().lock() else {
        log::warn!("duplicate_hash_cache_invalidation_failed reason=poisoned_lock");
        return;
    };
    if guard.as_ref().is_some_and(|entry| {
        entry
            .root_paths
            .iter()
            .any(|root| target.starts_with(root) || root.starts_with(target))
    }) {
        *guard = None;
    }
}

pub(super) fn clear() -> Result<(), String> {
    *cache()
        .lock()
        .map_err(|_| "duplicate hash cache lock is unavailable".to_string())? = None;
    Ok(())
}

fn elapsed_ms(started: Instant) -> u64 {
    u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn token() -> FilesystemChangeToken {
        FilesystemChangeToken {
            volume_id: [0; 16],
            history_id: 0,
            cursor: 1,
        }
    }

    #[test]
    fn cache_requires_an_exact_root_and_option_match() {
        clear().expect("clear cache before test");
        let root = PathBuf::from("/test/root");
        store_snapshot(
            &[DuplicateHashCacheRoot {
                path: root.clone(),
                change_token: token(),
            }],
            200,
            "sample-plan",
            Vec::new(),
            || false,
        )
        .expect("store memory cache");
        assert!(
            find_snapshot(std::slice::from_ref(&root), 200, "sample-plan")
                .expect("read matching cache")
                .0
                .is_some()
        );
        assert!(
            find_snapshot(std::slice::from_ref(&root), 201, "sample-plan")
                .expect("read mismatched cache")
                .0
                .is_none()
        );
        clear().expect("clear cache after test");
    }

    #[test]
    fn deletion_below_a_root_invalidates_the_snapshot() {
        clear().expect("clear cache before test");
        let root = PathBuf::from("/test/root");
        store_snapshot(
            &[DuplicateHashCacheRoot {
                path: root.clone(),
                change_token: token(),
            }],
            200,
            "sample-plan",
            Vec::new(),
            || false,
        )
        .expect("store memory cache");
        invalidate_containing(&root.join("file.bin"));
        assert!(find_snapshot(&[root], 200, "sample-plan")
            .expect("read invalidated cache")
            .0
            .is_none());
    }
}

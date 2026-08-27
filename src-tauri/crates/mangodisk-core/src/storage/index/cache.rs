use std::{
    collections::{HashMap, VecDeque},
    fs,
    path::{Path, PathBuf},
    sync::{Mutex, OnceLock},
};

use mangodisk_platform::{
    current_platform, FilesystemChangeMonitor, FilesystemChangeStatus, FilesystemChangeToken,
    Platform, ScanPurpose,
};

use crate::{
    filesystem::metadata::{display_fingerprint, display_path, is_link_like, modified_ms},
    shared::operation::OPERATION_CANCELLED_ERROR,
    storage::{
        analysis::{AnalysisResult, DirectoryEntryInfo},
        large_files::{LargeFileEntry, LargeFilesResult},
    },
};

const ANALYSIS_CACHE_ROOT_LIMIT: usize = 2;
const LARGE_FILE_RESULT_LIMIT: usize = 2_000;
const ANALYSIS_CACHE_UNAVAILABLE_ERROR: &str = "the analysis cache is unavailable";

/// Large-file scans retain every file above the smallest selectable threshold. Higher thresholds
/// can then be applied directly to the current in-memory scan without touching the filesystem.
pub(crate) const LARGE_FILE_INDEX_FLOOR_BYTES: u64 = 50 * 1024 * 1024;

static ANALYSIS_CACHE: OnceLock<Mutex<AnalysisCache>> = OnceLock::new();

#[derive(Clone, Copy, Default)]
pub(crate) struct DirectoryAggregate {
    pub(crate) bytes: u64,
    pub(crate) file_count: u64,
    pub(crate) skipped_count: u64,
    pub(crate) scanned_at_ms: u64,
    pub(crate) fingerprint: Option<[u8; 32]>,
}

#[derive(Clone, Copy)]
pub(crate) struct IndexedFile {
    pub(crate) bytes: u64,
    pub(crate) modified_at_ms: Option<u64>,
}

#[derive(Default)]
struct AnalysisCache {
    directories: HashMap<PathBuf, DirectoryAggregate>,
    files: HashMap<PathBuf, IndexedFile>,
    scan_roots: HashMap<PathBuf, ScanPurpose>,
    /// Orders cached roots from least recently used to most recently used.
    ///
    /// Directory and file maps are intentionally shared across roots to keep lookup inexpensive.
    /// The separate order only owns eviction policy and must be updated whenever a completed root
    /// is stored or reused.
    root_recency: VecDeque<PathBuf>,
    change_tokens: HashMap<PathBuf, Option<FilesystemChangeToken>>,
    change_monitors: HashMap<PathBuf, CachedChangeMonitor>,
}

struct CachedChangeMonitor {
    token: FilesystemChangeToken,
    monitor: FilesystemChangeMonitor,
}

enum ChangeValidation {
    Valid(Option<FilesystemChangeMonitor>),
    Stale,
}

pub(crate) enum CacheReuseDecision {
    Reusable,
    Miss,
}

impl ChangeValidation {
    fn into_valid_monitor(self) -> Option<Option<FilesystemChangeMonitor>> {
        match self {
            Self::Valid(monitor) => Some(monitor),
            Self::Stale => None,
        }
    }
}

/// Reuse is intentionally limited to the current process. A completed scan has one authoritative
/// result in memory, avoiding duplicate storage and write backpressure on the traversal path.
pub(crate) fn reuse_decision(
    root: &Path,
    requested: ScanPurpose,
    is_cancelled: &(dyn Fn() -> bool + Sync),
) -> Result<CacheReuseDecision, String> {
    if is_cancelled() {
        return Err(OPERATION_CANCELLED_ERROR.to_string());
    }

    let candidate = {
        let cache = cache()
            .lock()
            .map_err(|_| ANALYSIS_CACHE_UNAVAILABLE_ERROR.to_string())?;
        cache
            .directories
            .contains_key(root)
            .then(|| {
                cache
                    .scan_roots
                    .iter()
                    .filter(|(scan_root, _)| root.starts_with(scan_root))
                    .max_by_key(|(scan_root, _)| scan_root.components().count())
                    .map(|(scan_root, purpose)| {
                        let token = cache.change_tokens.get(scan_root).copied().flatten();
                        let monitor = token.and_then(|token| {
                            cache
                                .change_monitors
                                .get(scan_root)
                                .filter(|cached| cached.token == token)
                                .map(|cached| cached.monitor.clone())
                        });
                        (
                            scan_root.clone(),
                            *purpose,
                            token,
                            monitor,
                            scan_root != root,
                        )
                    })
            })
            .flatten()
    };

    let Some((scan_root, cached_purpose, token, monitor, is_descendant_page)) = candidate else {
        return Ok(CacheReuseDecision::Miss);
    };
    let purpose_compatible = matches!(
        (cached_purpose, requested),
        (ScanPurpose::Analysis, _) | (ScanPurpose::LargeFiles, ScanPurpose::LargeFiles)
    );
    if !purpose_compatible {
        evict_memory_root(&scan_root)?;
        return Ok(CacheReuseDecision::Miss);
    }

    // Descendant navigation and a large-file view derived from an analysis are reads from the
    // immutable result of the active scan. Revalidating the pre-scan change token here makes a
    // busy home directory immediately stale even though the user only changed views. An explicit
    // refresh still bypasses this function, and destructive operations independently preflight
    // every selected path before execution.
    let derives_from_active_analysis = cached_purpose == ScanPurpose::Analysis
        && (is_descendant_page || requested == ScanPurpose::LargeFiles);
    if derives_from_active_analysis {
        mark_root_recent(&scan_root)?;
        return Ok(CacheReuseDecision::Reusable);
    }

    if let Some(new_monitor) =
        validate_change_token(&scan_root, token, monitor, is_cancelled)?.into_valid_monitor()
    {
        if let (Some(token), Some(new_monitor)) = (token, new_monitor) {
            install_change_monitor(&scan_root, token, new_monitor)?;
        }
        mark_root_recent(&scan_root)?;
        return Ok(CacheReuseDecision::Reusable);
    }

    if is_cancelled() {
        return Err(OPERATION_CANCELLED_ERROR.to_string());
    }
    evict_memory_root(&scan_root)?;
    Ok(CacheReuseDecision::Miss)
}

pub(crate) fn large_files_result(
    root: &Path,
    minimum_bytes: u64,
    cache_reused: bool,
) -> Result<LargeFilesResult, String> {
    let cache = cache()
        .lock()
        .map_err(|_| ANALYSIS_CACHE_UNAVAILABLE_ERROR.to_string())?;
    let root_aggregate = cache
        .directories
        .get(root)
        .copied()
        .ok_or_else(|| "the large-file scan result is no longer available".to_string())?;
    let mut entries = cache
        .files
        .iter()
        .filter(|(path, file)| path.starts_with(root) && file.bytes >= minimum_bytes)
        .filter(|(path, _)| {
            current_platform()
                .should_skip(path, root, ScanPurpose::LargeFiles)
                .is_none()
        })
        .map(|(path, file)| large_file_entry(path, root, *file))
        .collect::<Vec<_>>();
    entries.sort_by(|left, right| {
        right
            .bytes
            .cmp(&left.bytes)
            .then_with(|| left.path.cmp(&right.path))
    });
    let total_count = entries.len() as u64;
    let total_bytes = entries.iter().map(|entry| entry.bytes).sum();
    entries.truncate(LARGE_FILE_RESULT_LIMIT);
    let returned_count = entries.len() as u64;

    Ok(LargeFilesResult {
        scan_id: 0,
        root: display_path(root),
        scanned_at_ms: root_aggregate.scanned_at_ms,
        minimum_bytes,
        total_bytes,
        total_count,
        returned_count,
        truncated: returned_count < total_count,
        skipped_count: root_aggregate.skipped_count,
        cache_reused,
        entries,
    })
}

fn large_file_entry(path: &Path, root: &Path, file: IndexedFile) -> LargeFileEntry {
    LargeFileEntry {
        name: path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default(),
        path: display_path(path),
        parent_path: display_path(path.parent().unwrap_or(root)),
        bytes: file.bytes,
        modified_at_ms: file.modified_at_ms,
    }
}

pub(crate) fn analysis_result(root: &Path) -> Result<Option<AnalysisResult>, String> {
    let root_aggregate = {
        let cache = cache()
            .lock()
            .map_err(|_| ANALYSIS_CACHE_UNAVAILABLE_ERROR.to_string())?;
        cache.directories.get(root).copied()
    };
    let Some(root_aggregate) = root_aggregate else {
        return Ok(None);
    };

    let children = fs::read_dir(root)
        .map_err(|error| format!("failed to read the analysis root: {error}"))?
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let path = entry.path();
            let metadata = fs::symlink_metadata(&path).ok()?;
            (!is_link_like(&metadata)).then_some((entry, path, metadata))
        })
        .collect::<Vec<_>>();
    let cache = cache()
        .lock()
        .map_err(|_| ANALYSIS_CACHE_UNAVAILABLE_ERROR.to_string())?;
    let mut entries = build_analysis_entries(children, |path| cache.directories.get(path).copied());
    entries.sort_by(|left, right| {
        right
            .bytes
            .cmp(&left.bytes)
            .then_with(|| left.path.cmp(&right.path))
    });
    entries.truncate(80);

    Ok(Some(AnalysisResult {
        scan_id: 0,
        root: display_path(root),
        scanned_at_ms: root_aggregate.scanned_at_ms,
        total_bytes: root_aggregate.bytes,
        skipped_count: root_aggregate.skipped_count,
        entries,
    }))
}

fn build_analysis_entries(
    children: Vec<(fs::DirEntry, PathBuf, fs::Metadata)>,
    mut directory_aggregate: impl FnMut(&Path) -> Option<DirectoryAggregate>,
) -> Vec<DirectoryEntryInfo> {
    children
        .into_iter()
        .map(|(entry, path, metadata)| {
            let aggregate = if metadata.is_dir() {
                directory_aggregate(&path).unwrap_or_default()
            } else {
                DirectoryAggregate {
                    bytes: metadata.len(),
                    file_count: u64::from(metadata.is_file()),
                    ..DirectoryAggregate::default()
                }
            };
            DirectoryEntryInfo {
                name: entry.file_name().to_string_lossy().into_owned(),
                path: display_path(&path),
                bytes: aggregate.bytes,
                file_count: aggregate.file_count,
                is_directory: metadata.is_dir(),
                modified_at_ms: modified_ms(&metadata),
                content_fingerprint: metadata
                    .is_dir()
                    .then(|| aggregate.fingerprint.map(display_fingerprint))
                    .flatten(),
            }
        })
        .collect()
}

pub(crate) fn store_memory_only(
    root: &Path,
    root_aggregate: DirectoryAggregate,
    scanned_directories: HashMap<PathBuf, DirectoryAggregate>,
    scanned_files: HashMap<PathBuf, IndexedFile>,
    purpose: ScanPurpose,
    refresh: bool,
    change_token: Option<FilesystemChangeToken>,
) -> Result<(), String> {
    let removed_monitors = {
        let mut cache = cache()
            .lock()
            .map_err(|_| ANALYSIS_CACHE_UNAVAILABLE_ERROR.to_string())?;
        let mut removed_monitors = Vec::new();
        while !cache.scan_roots.contains_key(root)
            && cache.scan_roots.len() >= ANALYSIS_CACHE_ROOT_LIMIT
        {
            let least_recent_root = cache
                .root_recency
                .front()
                .cloned()
                .ok_or_else(|| "the analysis cache root recency is inconsistent".to_string())?;
            let roots_before = cache.scan_roots.len();
            removed_monitors.extend(evict_cached_root(&mut cache, &least_recent_root));
            log::info!(
                "analysis_cache_root_evicted roots_before={} roots_after={} root_limit={}",
                roots_before,
                cache.scan_roots.len(),
                ANALYSIS_CACHE_ROOT_LIMIT
            );
        }
        if refresh {
            if let Some(previous) = cache.directories.get(root).copied() {
                for (path, aggregate) in &mut cache.directories {
                    if path != root && root.starts_with(path) {
                        aggregate.bytes = aggregate
                            .bytes
                            .saturating_sub(previous.bytes)
                            .saturating_add(root_aggregate.bytes);
                        aggregate.file_count = aggregate
                            .file_count
                            .saturating_sub(previous.file_count)
                            .saturating_add(root_aggregate.file_count);
                        aggregate.skipped_count = aggregate
                            .skipped_count
                            .saturating_sub(previous.skipped_count)
                            .saturating_add(root_aggregate.skipped_count);
                        aggregate.scanned_at_ms = root_aggregate.scanned_at_ms;
                    }
                }
            }
            cache.directories.retain(|path, _| !path.starts_with(root));
            cache.files.retain(|path, _| !path.starts_with(root));
            cache.scan_roots.retain(|path, _| !path.starts_with(root));
            cache.root_recency.retain(|path| !path.starts_with(root));
            cache
                .change_tokens
                .retain(|path, _| !path.starts_with(root));
            removed_monitors.extend(take_monitors(&mut cache, |path| path.starts_with(root)));
        } else if cache
            .change_monitors
            .get(root)
            .is_some_and(|cached| Some(cached.token) != change_token)
        {
            if let Some(cached) = cache.change_monitors.remove(root) {
                removed_monitors.push(cached.monitor);
            }
        }
        cache.directories.extend(scanned_directories);
        cache.files.extend(scanned_files);
        cache.scan_roots.insert(root.to_path_buf(), purpose);
        touch_root(&mut cache, root);
        cache.change_tokens.insert(root.to_path_buf(), change_token);
        removed_monitors
    };
    drop(removed_monitors);
    Ok(())
}

pub(crate) fn remove_entry(target: &Path, bytes: u64, file_count: u64, is_directory: bool) {
    let removed_monitors = {
        let Ok(mut cache) = cache().lock() else {
            log::warn!("analysis_cache_update_failed reason=poisoned_lock");
            return;
        };
        let removed_monitors = if is_directory {
            cache.files.retain(|path, _| !path.starts_with(target));
            cache
                .directories
                .retain(|path, _| !path.starts_with(target));
            cache.scan_roots.retain(|path, _| !path.starts_with(target));
            cache.root_recency.retain(|path| !path.starts_with(target));
            cache
                .change_tokens
                .retain(|path, _| !path.starts_with(target));
            take_monitors(&mut cache, |path| path.starts_with(target))
        } else {
            cache.files.remove(target);
            Vec::new()
        };
        for (directory, aggregate) in &mut cache.directories {
            if target.starts_with(directory) {
                aggregate.bytes = aggregate.bytes.saturating_sub(bytes);
                aggregate.file_count = aggregate.file_count.saturating_sub(file_count);
                aggregate.fingerprint = None;
            }
        }
        removed_monitors
    };
    drop(removed_monitors);
}

pub(crate) fn clear_all() -> Result<(), String> {
    let previous = {
        let mut cache = cache()
            .lock()
            .map_err(|_| ANALYSIS_CACHE_UNAVAILABLE_ERROR.to_string())?;
        std::mem::take(&mut *cache)
    };
    drop(previous);
    Ok(())
}

#[cfg(test)]
pub(crate) fn memory_entry_counts() -> Result<(usize, usize, usize), String> {
    let cache = cache()
        .lock()
        .map_err(|_| ANALYSIS_CACHE_UNAVAILABLE_ERROR.to_string())?;
    Ok((
        cache.scan_roots.len(),
        cache.directories.len(),
        cache.files.len(),
    ))
}

fn cache() -> &'static Mutex<AnalysisCache> {
    ANALYSIS_CACHE.get_or_init(|| Mutex::new(AnalysisCache::default()))
}

fn validate_change_token(
    root: &Path,
    token: Option<FilesystemChangeToken>,
    monitor: Option<FilesystemChangeMonitor>,
    is_cancelled: &(dyn Fn() -> bool + Sync),
) -> Result<ChangeValidation, String> {
    if is_cancelled() {
        return Err(OPERATION_CANCELLED_ERROR.to_string());
    }
    let Some(token) = token else {
        return Ok(ChangeValidation::Stale);
    };
    if let Some(monitor) = monitor {
        return Ok(match monitor.status() {
            FilesystemChangeStatus::Clean => ChangeValidation::Valid(None),
            FilesystemChangeStatus::Pending
            | FilesystemChangeStatus::Changed
            | FilesystemChangeStatus::HistoryUnavailable => ChangeValidation::Stale,
        });
    }

    let started = current_platform().start_filesystem_change_monitor(root, &token, is_cancelled);
    if is_cancelled() {
        return Err(OPERATION_CANCELLED_ERROR.to_string());
    }
    match started {
        Ok(Some(monitor)) if monitor.status() == FilesystemChangeStatus::Clean => {
            let reusable_monitor = cfg!(target_os = "macos").then_some(monitor);
            Ok(ChangeValidation::Valid(reusable_monitor))
        }
        Ok(Some(_) | None) => Ok(ChangeValidation::Stale),
        Err(error) => {
            log::warn!(
                "analysis_cache_change_validation_failed diagnostic={}",
                error.diagnostic()
            );
            Ok(ChangeValidation::Stale)
        }
    }
}

fn install_change_monitor(
    root: &Path,
    token: FilesystemChangeToken,
    monitor: FilesystemChangeMonitor,
) -> Result<(), String> {
    let removed_monitors = {
        let mut cache = cache()
            .lock()
            .map_err(|_| ANALYSIS_CACHE_UNAVAILABLE_ERROR.to_string())?;
        let mut removed_monitors = Vec::new();
        if let Some(previous) = cache
            .change_monitors
            .insert(root.to_path_buf(), CachedChangeMonitor { token, monitor })
        {
            removed_monitors.push(previous.monitor);
        }
        removed_monitors
    };
    drop(removed_monitors);
    Ok(())
}

fn take_monitors(
    cache: &mut AnalysisCache,
    mut matches: impl FnMut(&Path) -> bool,
) -> Vec<FilesystemChangeMonitor> {
    let roots = cache
        .change_monitors
        .keys()
        .filter(|root| matches(root))
        .cloned()
        .collect::<Vec<_>>();
    roots
        .into_iter()
        .filter_map(|root| cache.change_monitors.remove(&root))
        .map(|cached| cached.monitor)
        .collect()
}

fn evict_memory_root(root: &Path) -> Result<(), String> {
    let removed_monitors = {
        let mut cache = cache()
            .lock()
            .map_err(|_| ANALYSIS_CACHE_UNAVAILABLE_ERROR.to_string())?;
        evict_cached_root(&mut cache, root)
    };
    drop(removed_monitors);
    Ok(())
}

fn mark_root_recent(root: &Path) -> Result<(), String> {
    let mut cache = cache()
        .lock()
        .map_err(|_| ANALYSIS_CACHE_UNAVAILABLE_ERROR.to_string())?;
    if cache.scan_roots.contains_key(root) {
        touch_root(&mut cache, root);
    }
    Ok(())
}

fn touch_root(cache: &mut AnalysisCache, root: &Path) {
    cache.root_recency.retain(|cached| cached != root);
    cache.root_recency.push_back(root.to_path_buf());
}

/// Removes one independently cached root and any nested refresh roots that share its records.
/// Nested roots cannot outlive their owner because the flattened directory and file maps contain
/// overlapping keys. Distinct roots remain available and preserve their relative recency.
fn evict_cached_root(cache: &mut AnalysisCache, root: &Path) -> Vec<FilesystemChangeMonitor> {
    cache.directories.retain(|path, _| !path.starts_with(root));
    cache.files.retain(|path, _| !path.starts_with(root));
    cache.scan_roots.retain(|path, _| !path.starts_with(root));
    cache.root_recency.retain(|path| !path.starts_with(root));
    cache
        .change_tokens
        .retain(|path, _| !path.starts_with(root));
    take_monitors(cache, |path| path.starts_with(root))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store_test_analysis_root(root: &Path, scanned_at_ms: u64) {
        let aggregate = DirectoryAggregate {
            scanned_at_ms,
            ..DirectoryAggregate::default()
        };
        store_memory_only(
            root,
            aggregate,
            HashMap::from([(root.to_path_buf(), aggregate)]),
            HashMap::new(),
            ScanPurpose::Analysis,
            true,
            None,
        )
        .expect("test analysis result should store");
    }

    #[test]
    fn missing_change_token_is_stale() {
        let validation =
            validate_change_token(Path::new("/missing-change-token"), None, None, &|| false)
                .expect("validation should succeed");
        assert!(matches!(validation, ChangeValidation::Stale));
    }

    #[test]
    fn large_file_results_are_derived_from_memory() {
        let _operation_lock = crate::shared::operation::test_operation_lock();
        clear_all().expect("cache should clear");
        let root = PathBuf::from("/memory-large-files");
        let file = root.join("large.bin");
        let aggregate = DirectoryAggregate {
            bytes: LARGE_FILE_INDEX_FLOOR_BYTES,
            file_count: 1,
            scanned_at_ms: 7,
            ..DirectoryAggregate::default()
        };
        store_memory_only(
            &root,
            aggregate,
            HashMap::from([(root.clone(), aggregate)]),
            HashMap::from([(
                file,
                IndexedFile {
                    bytes: LARGE_FILE_INDEX_FLOOR_BYTES,
                    modified_at_ms: Some(5),
                },
            )]),
            ScanPurpose::LargeFiles,
            true,
            None,
        )
        .expect("memory result should store");

        let result = large_files_result(&root, LARGE_FILE_INDEX_FLOOR_BYTES, false)
            .expect("memory result should load");
        assert_eq!(result.total_count, 1);
        assert_eq!(result.total_bytes, LARGE_FILE_INDEX_FLOOR_BYTES);
        clear_all().expect("cache should clear");
    }

    #[test]
    fn large_file_view_reuses_active_analysis_without_change_history() {
        let _operation_lock = crate::shared::operation::test_operation_lock();
        clear_all().expect("cache should clear");
        let root = PathBuf::from("/memory-analysis-for-large-files");
        let aggregate = DirectoryAggregate {
            bytes: LARGE_FILE_INDEX_FLOOR_BYTES,
            file_count: 1,
            scanned_at_ms: 8,
            ..DirectoryAggregate::default()
        };
        store_memory_only(
            &root,
            aggregate,
            HashMap::from([(root.clone(), aggregate)]),
            HashMap::new(),
            ScanPurpose::Analysis,
            true,
            None,
        )
        .expect("analysis result should store");

        let decision = reuse_decision(&root, ScanPurpose::LargeFiles, &|| false)
            .expect("cache reuse should succeed");
        assert!(matches!(decision, CacheReuseDecision::Reusable));
        clear_all().expect("cache should clear");
    }

    #[test]
    fn clearing_memory_removes_the_only_scan_result() {
        let _operation_lock = crate::shared::operation::test_operation_lock();
        clear_all().expect("cache should clear");
        let root = PathBuf::from("/memory-only-result");
        let aggregate = DirectoryAggregate {
            scanned_at_ms: 9,
            ..DirectoryAggregate::default()
        };
        store_memory_only(
            &root,
            aggregate,
            HashMap::from([(root.clone(), aggregate)]),
            HashMap::new(),
            ScanPurpose::Analysis,
            true,
            None,
        )
        .expect("memory result should store");
        clear_all().expect("cache should clear");
        assert!(analysis_result(&root)
            .expect("cache lookup should succeed")
            .is_none());
    }

    #[test]
    fn storing_a_third_root_evicts_only_the_least_recent_root() {
        let _operation_lock = crate::shared::operation::test_operation_lock();
        clear_all().expect("cache should clear");
        let root_a = PathBuf::from("/memory-lru-a");
        let root_b = PathBuf::from("/memory-lru-b");
        let root_c = PathBuf::from("/memory-lru-c");

        store_test_analysis_root(&root_a, 1);
        store_test_analysis_root(&root_b, 2);
        store_test_analysis_root(&root_c, 3);

        let cache = cache().lock().expect("cache should be readable");
        assert!(!cache.scan_roots.contains_key(&root_a));
        assert!(!cache.directories.contains_key(&root_a));
        assert!(cache.scan_roots.contains_key(&root_b));
        assert!(cache.directories.contains_key(&root_b));
        assert!(cache.scan_roots.contains_key(&root_c));
        assert!(cache.directories.contains_key(&root_c));
        assert_eq!(
            cache.root_recency,
            VecDeque::from([root_b.clone(), root_c.clone()])
        );
        drop(cache);
        clear_all().expect("cache should clear");
    }

    #[test]
    fn reusing_a_root_updates_its_eviction_recency() {
        let _operation_lock = crate::shared::operation::test_operation_lock();
        clear_all().expect("cache should clear");
        let root_a = PathBuf::from("/memory-lru-reused-a");
        let root_b = PathBuf::from("/memory-lru-reused-b");
        let root_c = PathBuf::from("/memory-lru-reused-c");

        store_test_analysis_root(&root_a, 1);
        store_test_analysis_root(&root_b, 2);
        let decision = reuse_decision(&root_a, ScanPurpose::LargeFiles, &|| false)
            .expect("analysis result should be reusable for large files");
        assert!(matches!(decision, CacheReuseDecision::Reusable));
        store_test_analysis_root(&root_c, 3);

        let cache = cache().lock().expect("cache should be readable");
        assert!(cache.scan_roots.contains_key(&root_a));
        assert!(!cache.scan_roots.contains_key(&root_b));
        assert!(cache.scan_roots.contains_key(&root_c));
        assert_eq!(
            cache.root_recency,
            VecDeque::from([root_a.clone(), root_c.clone()])
        );
        drop(cache);
        clear_all().expect("cache should clear");
    }
}

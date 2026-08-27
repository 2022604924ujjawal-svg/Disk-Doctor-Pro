use std::{
    ffi::{c_void, OsString},
    fs, io,
    os::windows::{
        ffi::{OsStrExt, OsStringExt},
        fs::MetadataExt,
    },
    path::{Path, PathBuf},
};

use windows_sys::Win32::{
    Foundation::{
        GetLastError, ERROR_FILE_NOT_FOUND, ERROR_INVALID_FUNCTION, ERROR_INVALID_PARAMETER,
        ERROR_NOT_SUPPORTED, ERROR_NO_MORE_FILES, HANDLE, INVALID_HANDLE_VALUE,
    },
    Storage::FileSystem::{
        FindClose, FindExInfoBasic, FindExSearchNameMatch, FindFirstFileExW, FindNextFileW,
        FILE_ATTRIBUTE_DIRECTORY, FILE_ATTRIBUTE_REPARSE_POINT, FIND_FIRST_EX_LARGE_FETCH,
        WIN32_FIND_DATAW,
    },
};

use crate::{
    DirectPhysicalDirectoryEnumeration, DirectoryAggregateProgress, DirectoryTreeAggregate,
    DirectoryTreeAggregateError, DirectoryTreeSourceAggregate,
};

use super::is_remote_placeholder_attributes;

const WINDOWS_EPOCH_OFFSET_100NS: u64 = 116_444_736_000_000_000;

struct FindHandle(HANDLE);

impl Drop for FindHandle {
    fn drop(&mut self) {
        unsafe {
            FindClose(self.0);
        }
    }
}

struct PendingDirectory {
    path: PathBuf,
    source_index: Option<usize>,
}

struct AggregateCollection<'a> {
    root_source: DirectoryTreeSourceAggregate,
    child_sources: Vec<DirectoryTreeSourceAggregate>,
    pending: Vec<PendingDirectory>,
    skipped_count: u64,
    remote_placeholder_count: u64,
    progress: DirectoryAggregateProgress<'a>,
    large_fetch_enabled: bool,
    count_link_metadata: bool,
    is_cancelled: &'a (dyn Fn() -> bool + Sync),
}

impl AggregateCollection<'_> {
    fn observe_non_file(&mut self, directory: &Path) {
        self.progress.observe(directory, 1, 0, 0);
    }

    fn observe_file(
        &mut self,
        directory: &Path,
        source_index: Option<usize>,
        data: &WIN32_FIND_DATAW,
    ) {
        let source = source_index
            .and_then(|index| self.child_sources.get_mut(index))
            .unwrap_or(&mut self.root_source);
        let bytes = (u64::from(data.nFileSizeHigh) << 32) | u64::from(data.nFileSizeLow);
        source.bytes = source.bytes.saturating_add(bytes);
        source.file_count = source.file_count.saturating_add(1);
        source.modified_at_ms = latest_timestamp(source.modified_at_ms, modified_at_ms(data));
        self.progress.observe(directory, 1, 1, bytes);
    }
}

/// Measures one directory tree with the same Win32 enumeration used by the
/// large-file scanner. `WIN32_FIND_DATAW` already carries type, reparse-point,
/// remote-storage, size, and timestamp fields, so the hot path avoids a metadata
/// syscall and a matcher dispatch for every file in complete-root cleanup rules.
pub(super) fn measure(
    root: &Path,
    count_link_metadata: bool,
    is_cancelled: &(dyn Fn() -> bool + Sync),
    report_progress: &(dyn Fn(&Path, u64, u64) + Sync),
) -> Result<DirectoryTreeAggregate, DirectoryTreeAggregateError> {
    if is_cancelled() {
        return Err(DirectoryTreeAggregateError::Cancelled);
    }
    let metadata = fs::symlink_metadata(root)
        .map_err(|error| platform_error("validate directory aggregate root", error))?;
    let root_attributes = metadata.file_attributes();
    if is_remote_placeholder_attributes(root_attributes) {
        // Reject the root before Win32 enumeration can recall its directory listing. The event
        // intentionally records only a stable reason and aggregate mode, never the private path.
        log::info!(
            "windows_directory_aggregate_blocked reason=remote_placeholder entry_kind=root_directory mode={}",
            if count_link_metadata {
                "project_artifact"
            } else {
                "cleanup"
            }
        );
        return Err(DirectoryTreeAggregateError::Platform(
            "directory aggregate root is a remote placeholder".to_string(),
        ));
    }
    if !metadata.is_dir() || root_attributes & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
        return Err(DirectoryTreeAggregateError::Platform(
            "directory aggregate root is not a physical directory".to_string(),
        ));
    }

    let mut collection = AggregateCollection {
        root_source: DirectoryTreeSourceAggregate {
            path: root.to_path_buf(),
            bytes: 0,
            file_count: 0,
            modified_at_ms: None,
        },
        child_sources: Vec::new(),
        pending: vec![PendingDirectory {
            path: root.to_path_buf(),
            source_index: None,
        }],
        skipped_count: 0,
        remote_placeholder_count: 0,
        progress: DirectoryAggregateProgress::new(report_progress),
        large_fetch_enabled: true,
        count_link_metadata,
        is_cancelled,
    };

    while let Some(directory) = collection.pending.pop() {
        if is_cancelled() {
            return Err(DirectoryTreeAggregateError::Cancelled);
        }
        if let Err(error) = enumerate_directory(root, &directory, &mut collection) {
            if is_cancelled() {
                return Err(DirectoryTreeAggregateError::Cancelled);
            }
            if directory.path == root {
                return Err(platform_error("enumerate directory aggregate root", error));
            }
            collection.skipped_count = collection.skipped_count.saturating_add(1);
            log::debug!(
                "directory_aggregate_directory_skipped platform=windows error_kind={:?} os_error={:?}",
                error.kind(),
                error.raw_os_error()
            );
        }
    }
    collection.progress.finish(root);

    let mut sources = Vec::with_capacity(collection.child_sources.len().saturating_add(1));
    if collection.root_source.file_count > 0 {
        sources.push(collection.root_source);
    }
    sources.extend(
        collection
            .child_sources
            .into_iter()
            .filter(|source| source.file_count > 0),
    );
    let bytes = sources
        .iter()
        .fold(0_u64, |total, source| total.saturating_add(source.bytes));
    let file_count = sources.iter().fold(0_u64, |total, source| {
        total.saturating_add(source.file_count)
    });
    if collection.remote_placeholder_count > 0 {
        log::info!(
            "windows_directory_aggregate_remote_placeholders_skipped count={} strategy=win32_directory_enumeration",
            collection.remote_placeholder_count
        );
    }
    Ok(DirectoryTreeAggregate {
        bytes,
        file_count,
        skipped_count: collection.skipped_count,
        sources,
        strategy: if collection.large_fetch_enabled {
            "win32-find-large-fetch-resident-files-v2"
        } else {
            "win32-find-resident-files-v2"
        },
    })
}

/// Enumerates one shallow directory with Win32's cached find data. The previous portable caller
/// invoked `symlink_metadata` for every AppData child solely to distinguish a directory from a
/// reparse point; on virtualized Windows disks that dominated the uninstall catalog latency.
pub(super) fn direct_physical_directories(
    root: &Path,
    maximum_entries: usize,
    is_cancelled: &(dyn Fn() -> bool + Sync),
) -> Result<DirectPhysicalDirectoryEnumeration, DirectoryTreeAggregateError> {
    if is_cancelled() {
        return Err(DirectoryTreeAggregateError::Cancelled);
    }
    let mut pattern = root.to_path_buf();
    pattern.push("*");
    let wide_pattern = pattern
        .as_os_str()
        .encode_wide()
        .chain(Some(0))
        .collect::<Vec<_>>();
    let mut data = WIN32_FIND_DATAW::default();
    let mut large_fetch_enabled = true;
    let mut raw_handle = find_first(&wide_pattern, &mut data, true);
    if raw_handle == INVALID_HANDLE_VALUE {
        let error = unsafe { GetLastError() };
        if large_fetch_is_unsupported(error) {
            large_fetch_enabled = false;
            raw_handle = find_first(&wide_pattern, &mut data, false);
        }
    }
    if raw_handle == INVALID_HANDLE_VALUE {
        let error = unsafe { GetLastError() };
        if error == ERROR_FILE_NOT_FOUND {
            return Ok(DirectPhysicalDirectoryEnumeration {
                directories: Vec::new(),
                observed_count: 0,
                strategy: "win32-direct-physical-directory-enumeration-v2",
            });
        }
        return Err(platform_error(
            "enumerate direct physical directories",
            io::Error::from_raw_os_error(error as i32),
        ));
    }
    let handle = FindHandle(raw_handle);
    let mut directories = Vec::new();
    let mut observed_count = 0_usize;
    loop {
        if is_cancelled() {
            return Err(DirectoryTreeAggregateError::Cancelled);
        }
        let name_length = data
            .cFileName
            .iter()
            .position(|value| *value == 0)
            .unwrap_or(data.cFileName.len());
        let name = OsString::from_wide(&data.cFileName[..name_length]);
        if name != "." && name != ".." {
            if observed_count >= maximum_entries {
                break;
            }
            observed_count += 1;
            if data.dwFileAttributes & FILE_ATTRIBUTE_DIRECTORY != 0
                && data.dwFileAttributes & FILE_ATTRIBUTE_REPARSE_POINT == 0
                && !is_remote_placeholder_attributes(data.dwFileAttributes)
            {
                directories.push(root.join(name));
            }
        }
        if unsafe { FindNextFileW(handle.0, &mut data) } == 0 {
            let error = unsafe { GetLastError() };
            if error == ERROR_NO_MORE_FILES {
                break;
            }
            return Err(platform_error(
                "continue direct physical directory enumeration",
                io::Error::from_raw_os_error(error as i32),
            ));
        }
    }
    Ok(DirectPhysicalDirectoryEnumeration {
        directories,
        observed_count,
        strategy: if large_fetch_enabled {
            "win32-direct-physical-directory-large-fetch-v2"
        } else {
            "win32-direct-physical-directory-enumeration-v2"
        },
    })
}

fn enumerate_directory(
    root: &Path,
    directory: &PendingDirectory,
    collection: &mut AggregateCollection<'_>,
) -> io::Result<()> {
    let mut pattern = directory.path.clone();
    pattern.push("*");
    let wide_pattern = pattern
        .as_os_str()
        .encode_wide()
        .chain(Some(0))
        .collect::<Vec<_>>();
    let mut data = WIN32_FIND_DATAW::default();
    let mut raw_handle = find_first(&wide_pattern, &mut data, collection.large_fetch_enabled);
    if raw_handle == INVALID_HANDLE_VALUE && collection.large_fetch_enabled {
        let error = unsafe { GetLastError() };
        if large_fetch_is_unsupported(error) {
            collection.large_fetch_enabled = false;
            raw_handle = find_first(&wide_pattern, &mut data, false);
        }
    }
    if raw_handle == INVALID_HANDLE_VALUE {
        let error = unsafe { GetLastError() };
        // An existing empty directory has no first wildcard match. Treat it as
        // a complete zero-entry enumeration rather than a limited result.
        if error == ERROR_FILE_NOT_FOUND {
            return Ok(());
        }
        return Err(io::Error::from_raw_os_error(error as i32));
    }
    let handle = FindHandle(raw_handle);

    loop {
        if (collection.is_cancelled)() {
            return Err(io::Error::new(
                io::ErrorKind::Interrupted,
                "directory aggregate cancelled",
            ));
        }
        collect_entry(root, directory, &data, collection);
        if unsafe { FindNextFileW(handle.0, &mut data) } == 0 {
            let error = unsafe { GetLastError() };
            if error == ERROR_NO_MORE_FILES {
                return Ok(());
            }
            return Err(io::Error::from_raw_os_error(error as i32));
        }
    }
}

fn find_first(wide_pattern: &[u16], data: &mut WIN32_FIND_DATAW, large_fetch: bool) -> HANDLE {
    unsafe {
        FindFirstFileExW(
            wide_pattern.as_ptr(),
            FindExInfoBasic,
            data as *mut WIN32_FIND_DATAW as *mut c_void,
            FindExSearchNameMatch,
            std::ptr::null(),
            if large_fetch {
                FIND_FIRST_EX_LARGE_FETCH
            } else {
                0
            },
        )
    }
}

fn large_fetch_is_unsupported(error: u32) -> bool {
    matches!(
        error,
        ERROR_INVALID_FUNCTION | ERROR_INVALID_PARAMETER | ERROR_NOT_SUPPORTED
    )
}

fn collect_entry(
    root: &Path,
    directory: &PendingDirectory,
    data: &WIN32_FIND_DATAW,
    collection: &mut AggregateCollection<'_>,
) {
    let name_length = data
        .cFileName
        .iter()
        .position(|value| *value == 0)
        .unwrap_or(data.cFileName.len());
    let name = OsString::from_wide(&data.cFileName[..name_length]);
    if name == "." || name == ".." {
        return;
    }

    if is_remote_placeholder_attributes(data.dwFileAttributes) {
        // Remote-only entries never contribute local reclaimable bytes. This check precedes the
        // project-artifact reparse branch because counting the link metadata would otherwise make
        // a cloud placeholder eligible for cleanup preview.
        collection.observe_non_file(&directory.path);
        collection.skipped_count = collection.skipped_count.saturating_add(1);
        collection.remote_placeholder_count = collection.remote_placeholder_count.saturating_add(1);
        return;
    }

    if data.dwFileAttributes & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
        if collection.count_link_metadata {
            // A generated project tree counts the reparse point itself, but never descends into
            // its target. Ordinary cleanup aggregates retain the stricter skipped-entry behavior.
            collection.observe_file(&directory.path, directory.source_index, data);
            return;
        }
        collection.observe_non_file(&directory.path);
        collection.skipped_count = collection.skipped_count.saturating_add(1);
        return;
    }

    let path = directory.path.join(name);
    if data.dwFileAttributes & FILE_ATTRIBUTE_DIRECTORY != 0 {
        collection.observe_non_file(&directory.path);
        let source_index = if directory.path == root {
            collection.child_sources.push(DirectoryTreeSourceAggregate {
                path: path.clone(),
                bytes: 0,
                file_count: 0,
                modified_at_ms: None,
            });
            Some(collection.child_sources.len() - 1)
        } else {
            directory.source_index
        };
        collection
            .pending
            .push(PendingDirectory { path, source_index });
        return;
    }
    collection.observe_file(&directory.path, directory.source_index, data);
}

fn modified_at_ms(data: &WIN32_FIND_DATAW) -> Option<u64> {
    let ticks = (u64::from(data.ftLastWriteTime.dwHighDateTime) << 32)
        | u64::from(data.ftLastWriteTime.dwLowDateTime);
    ticks
        .checked_sub(WINDOWS_EPOCH_OFFSET_100NS)
        .map(|unix_ticks| unix_ticks / 10_000)
}

fn latest_timestamp(left: Option<u64>, right: Option<u64>) -> Option<u64> {
    match (left, right) {
        (Some(left), Some(right)) => Some(left.max(right)),
        (Some(value), None) | (None, Some(value)) => Some(value),
        (None, None) => None,
    }
}

fn platform_error(operation: &'static str, error: io::Error) -> DirectoryTreeAggregateError {
    DirectoryTreeAggregateError::Platform(format!("{operation}: {:?}", error.kind()))
}

#[cfg(test)]
mod tests {
    use std::{
        collections::BTreeMap,
        time::{SystemTime, UNIX_EPOCH},
    };

    use super::*;
    use crate::reference_directory_tree_aggregate;

    struct DirectoryCleanup(PathBuf);

    impl Drop for DirectoryCleanup {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn fixture_root(name: &str) -> (PathBuf, DirectoryCleanup) {
        let root = std::env::temp_dir().join(format!(
            "mangodisk-directory-aggregate-{name}-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("system clock must be after the Unix epoch")
                .as_nanos()
        ));
        fs::create_dir_all(&root).expect("fixture root must be created");
        (root.clone(), DirectoryCleanup(root))
    }

    fn source_summary(aggregate: &DirectoryTreeAggregate) -> BTreeMap<PathBuf, (u64, u64)> {
        aggregate
            .sources
            .iter()
            .map(|source| (source.path.clone(), (source.bytes, source.file_count)))
            .collect()
    }

    #[test]
    fn native_aggregate_preserves_direct_child_sources() {
        let (root, _cleanup) = fixture_root("sources");
        let nested = root.join("nested");
        fs::create_dir_all(nested.join("deep")).expect("nested fixture must be created");
        fs::create_dir(root.join("empty")).expect("empty fixture directory must be created");
        fs::write(root.join("direct.bin"), [0_u8; 4]).expect("direct fixture must be written");
        fs::write(nested.join("child.bin"), [0_u8; 5]).expect("child fixture must be written");
        fs::write(nested.join("deep/grandchild.bin"), [0_u8; 6])
            .expect("grandchild fixture must be written");

        let aggregate =
            measure(&root, false, &|| false, &|_, _, _| {}).expect("native aggregate must succeed");

        assert_eq!(aggregate.bytes, 15);
        assert_eq!(aggregate.file_count, 3);
        assert_eq!(aggregate.skipped_count, 0);
        assert!(aggregate.strategy.starts_with("win32-find"));
        assert_eq!(
            source_summary(&aggregate),
            BTreeMap::from([(root.clone(), (4, 1)), (nested, (11, 2))])
        );
    }

    #[test]
    fn native_aggregate_matches_the_reference_walker() {
        let (root, _cleanup) = fixture_root("equivalence");
        fs::create_dir_all(root.join("alpha/deep")).expect("nested fixture must be created");
        fs::create_dir(root.join("empty")).expect("empty fixture directory must be created");
        fs::write(root.join("direct.bin"), [0_u8; 3]).expect("direct fixture must be written");
        fs::write(root.join("alpha/child.bin"), [0_u8; 7]).expect("child fixture must be written");
        fs::write(root.join("alpha/deep/grandchild.bin"), [0_u8; 11])
            .expect("grandchild fixture must be written");

        let native =
            measure(&root, false, &|| false, &|_, _, _| {}).expect("native aggregate must succeed");
        let reference = reference_directory_tree_aggregate(&root);

        assert_eq!(
            (native.bytes, native.file_count, native.skipped_count),
            (
                reference.bytes,
                reference.file_count,
                reference.skipped_count
            )
        );
        assert_eq!(source_summary(&native), source_summary(&reference));
    }

    #[test]
    fn native_aggregate_skips_reparse_points_when_available() {
        let (root, _cleanup) = fixture_root("reparse");
        let target = root.join("target.bin");
        let linked = root.join("linked.bin");
        fs::write(&target, [0_u8; 7]).expect("target fixture must be written");
        if std::os::windows::fs::symlink_file(&target, &linked).is_err() {
            return;
        }

        let aggregate =
            measure(&root, false, &|| false, &|_, _, _| {}).expect("native aggregate must succeed");

        assert_eq!((aggregate.bytes, aggregate.file_count), (7, 1));
        assert_eq!(aggregate.skipped_count, 1);
    }

    #[test]
    fn project_artifact_aggregate_counts_reparse_metadata_without_following_targets() {
        let (root, _cleanup) = fixture_root("project-reparse");
        let target = root.join("target.bin");
        let linked = root.join("linked.bin");
        fs::write(&target, [0_u8; 7]).expect("target fixture must be written");
        if std::os::windows::fs::symlink_file(&target, &linked).is_err() {
            return;
        }
        let link_bytes = fs::symlink_metadata(&linked)
            .expect("link metadata must be readable")
            .file_size();

        let aggregate =
            measure(&root, true, &|| false, &|_, _, _| {}).expect("native aggregate must succeed");

        assert_eq!(aggregate.bytes, 7 + link_bytes);
        assert_eq!(aggregate.file_count, 2);
        assert_eq!(aggregate.skipped_count, 0);
    }

    #[test]
    fn native_aggregate_honors_cancellation_before_enumeration() {
        let (root, _cleanup) = fixture_root("cancel");

        assert!(matches!(
            measure(&root, false, &|| true, &|_, _, _| {}),
            Err(DirectoryTreeAggregateError::Cancelled)
        ));
    }

    #[test]
    fn win32_aggregate_skips_remote_only_entries() {
        let root = PathBuf::from(r"C:\fixture");
        let directory = PendingDirectory {
            path: root.clone(),
            source_index: None,
        };
        let mut data = WIN32_FIND_DATAW {
            dwFileAttributes: 0x0000_1000,
            nFileSizeLow: 1024,
            ..Default::default()
        };
        data.cFileName[0] = b'x' as u16;
        let mut collection = AggregateCollection {
            root_source: DirectoryTreeSourceAggregate {
                path: root.clone(),
                bytes: 0,
                file_count: 0,
                modified_at_ms: None,
            },
            child_sources: Vec::new(),
            pending: Vec::new(),
            skipped_count: 0,
            remote_placeholder_count: 0,
            progress: DirectoryAggregateProgress::new(&|_, _, _| {}),
            large_fetch_enabled: true,
            count_link_metadata: true,
            is_cancelled: &|| false,
        };

        collect_entry(&root, &directory, &data, &mut collection);

        assert_eq!(collection.skipped_count, 1);
        assert_eq!(collection.remote_placeholder_count, 1);
        assert_eq!(collection.root_source.bytes, 0);
        assert_eq!(collection.root_source.file_count, 0);
    }

    #[test]
    fn direct_directory_enumeration_rejects_files_and_reparse_points() {
        let (root, _cleanup) = fixture_root("direct-physical");
        let physical = root.join("physical");
        fs::create_dir(&physical).expect("physical directory must be created");
        fs::write(root.join("ordinary.bin"), [0_u8; 1]).expect("ordinary file must be written");
        let linked = root.join("linked");
        let link_created = std::os::windows::fs::symlink_dir(&physical, &linked).is_ok();

        let result = direct_physical_directories(&root, 16, &|| false)
            .expect("direct enumeration must succeed");

        assert_eq!(result.directories, vec![physical]);
        assert_eq!(result.observed_count, if link_created { 3 } else { 2 });
    }

    #[test]
    fn direct_directory_enumeration_applies_the_total_entry_limit() {
        let (root, _cleanup) = fixture_root("direct-limit");
        fs::write(root.join("a-file.bin"), [0_u8; 1]).expect("ordinary file must be written");
        fs::create_dir(root.join("b-directory")).expect("physical directory must be created");

        let result = direct_physical_directories(&root, 1, &|| false)
            .expect("direct enumeration must succeed");

        assert_eq!(result.observed_count, 1);
        assert!(result.directories.len() <= 1);
    }

    #[test]
    fn root_error_does_not_expose_the_private_path() {
        let missing = std::env::temp_dir().join(format!(
            "mangodisk-private-directory-aggregate-{}-Δ",
            std::process::id()
        ));
        assert!(!missing.exists(), "missing root fixture must not exist");

        let error = measure(&missing, false, &|| false, &|_, _, _| {})
            .expect_err("a missing root must reject native aggregation");
        let detail = match error {
            DirectoryTreeAggregateError::Platform(detail) => detail,
            DirectoryTreeAggregateError::Cancelled => {
                panic!("missing root must not report cancellation")
            }
        };
        assert!(!detail.contains(&missing.to_string_lossy().into_owned()));
    }
}

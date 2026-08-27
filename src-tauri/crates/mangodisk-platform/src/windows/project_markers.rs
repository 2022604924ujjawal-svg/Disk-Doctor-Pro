use std::{
    collections::HashSet,
    ffi::{c_void, OsString},
    io,
    os::windows::ffi::{OsStrExt, OsStringExt},
    path::{Path, PathBuf},
};

use windows_sys::Win32::{
    Foundation::{
        GetLastError, ERROR_INVALID_FUNCTION, ERROR_INVALID_PARAMETER, ERROR_NOT_SUPPORTED,
        ERROR_NO_MORE_FILES, HANDLE, INVALID_HANDLE_VALUE,
    },
    Storage::FileSystem::{
        FindClose, FindExInfoBasic, FindExSearchNameMatch, FindFirstFileExW, FindNextFileW,
        FILE_ATTRIBUTE_DIRECTORY, FILE_ATTRIBUTE_REPARSE_POINT, FIND_FIRST_EX_LARGE_FETCH,
        WIN32_FIND_DATAW,
    },
};

use crate::{
    ProjectMarkerCandidateProgress, ProjectMarkerCandidateScanError, ProjectMarkerCandidateSummary,
};

struct FindHandle(HANDLE);

impl Drop for FindHandle {
    fn drop(&mut self) {
        unsafe {
            FindClose(self.0);
        }
    }
}

enum EnumerateError {
    Cancelled,
    Io(io::Error),
    Consumer(String),
}

struct ProjectMarkerScanner<'a> {
    pending: Vec<(PathBuf, usize, bool)>,
    file_names: HashSet<String>,
    file_suffixes: Vec<String>,
    pruned_directory_names: HashSet<String>,
    maximum_depth: usize,
    is_cancelled: &'a (dyn Fn() -> bool + Sync),
    report_progress: &'a (dyn Fn(ProjectMarkerCandidateProgress) + Sync),
    consumer: &'a mut dyn FnMut(PathBuf) -> Result<(), String>,
    candidate_count: u64,
    file_count: u64,
    directory_count: u64,
    large_fetch_enabled: bool,
}

/// Keeps project-discovery policy at the native boundary as the declarative rule contract grows.
pub(super) struct ProjectMarkerScanRequest<'a> {
    pub(super) root: &'a Path,
    pub(super) file_names: &'a [String],
    pub(super) file_suffixes: &'a [String],
    pub(super) pruned_directory_names: &'a [String],
    pub(super) maximum_depth: usize,
    pub(super) is_cancelled: &'a (dyn Fn() -> bool + Sync),
    pub(super) report_progress: &'a (dyn Fn(ProjectMarkerCandidateProgress) + Sync),
}

/// Finds project marker files with the native Win32 directory enumerator.
///
/// The traversal retrieves names and reparse attributes in the enumeration call, prunes generated
/// artifact trees before descent, and never follows a reparse point. Core revalidates every marker
/// and the complete project rule before any cleanup candidate can be shown or selected.
pub(super) fn scan(
    request: ProjectMarkerScanRequest<'_>,
    consumer: &mut dyn FnMut(PathBuf) -> Result<(), String>,
) -> Result<ProjectMarkerCandidateSummary, ProjectMarkerCandidateScanError> {
    let mut scanner = ProjectMarkerScanner {
        pending: vec![(request.root.to_path_buf(), 0, true)],
        file_names: request
            .file_names
            .iter()
            .map(|name| name.to_ascii_lowercase())
            .collect(),
        file_suffixes: request
            .file_suffixes
            .iter()
            .map(|suffix| suffix.to_ascii_lowercase())
            .collect(),
        pruned_directory_names: request
            .pruned_directory_names
            .iter()
            .map(|name| name.to_ascii_lowercase())
            .collect(),
        maximum_depth: request.maximum_depth,
        is_cancelled: request.is_cancelled,
        report_progress: request.report_progress,
        consumer,
        candidate_count: 0,
        file_count: 0,
        directory_count: 0,
        large_fetch_enabled: true,
    };
    scanner.run(request.root)?;
    Ok(ProjectMarkerCandidateSummary {
        candidate_count: scanner.candidate_count,
        file_count: scanner.file_count,
        directory_count: scanner.directory_count,
        strategy: "win32-find-large-fetch-project-markers-v1",
    })
}

impl ProjectMarkerScanner<'_> {
    fn run(&mut self, root: &Path) -> Result<(), ProjectMarkerCandidateScanError> {
        while let Some((directory, depth, is_root)) = self.pending.pop() {
            if (self.is_cancelled)() {
                return Err(ProjectMarkerCandidateScanError::Cancelled);
            }
            match self.enumerate_directory(&directory, depth) {
                Ok(()) => {}
                Err(EnumerateError::Cancelled) => {
                    return Err(ProjectMarkerCandidateScanError::Cancelled);
                }
                Err(EnumerateError::Consumer(error)) => {
                    return Err(ProjectMarkerCandidateScanError::Consumer(error));
                }
                Err(EnumerateError::Io(error)) if is_root || directory == root => {
                    return Err(ProjectMarkerCandidateScanError::Platform(format!(
                        "unable to enumerate project scan root: {error}"
                    )));
                }
                Err(EnumerateError::Io(error)) => {
                    log::debug!(
                        "project_marker_directory_skipped platform=windows error_kind={:?} os_error={:?}",
                        error.kind(),
                        error.raw_os_error()
                    );
                }
            }
        }
        Ok(())
    }

    fn enumerate_directory(
        &mut self,
        directory: &Path,
        depth: usize,
    ) -> Result<(), EnumerateError> {
        let initial_file_count = self.file_count;
        let initial_directory_count = self.directory_count;
        let mut pattern = directory
            .join("*")
            .as_os_str()
            .encode_wide()
            .collect::<Vec<_>>();
        pattern.push(0);
        let mut data = unsafe { std::mem::zeroed::<WIN32_FIND_DATAW>() };
        let mut handle = find_first(&pattern, &mut data, self.large_fetch_enabled);
        if handle == INVALID_HANDLE_VALUE && self.large_fetch_enabled {
            let error = unsafe { GetLastError() };
            if large_fetch_is_unsupported(error) {
                self.large_fetch_enabled = false;
                handle = find_first(&pattern, &mut data, false);
            }
        }
        if handle == INVALID_HANDLE_VALUE {
            return Err(EnumerateError::Io(io::Error::last_os_error()));
        }
        let handle = FindHandle(handle);
        loop {
            if (self.is_cancelled)() {
                // Cancellation must remain distinct from a successful end-of-directory result.
                // Otherwise a root containing no child directories could publish an incomplete
                // marker set when cancellation arrives during its final enumeration page.
                return Err(EnumerateError::Cancelled);
            }
            self.collect_entry(directory, depth, &data)?;
            if (self.is_cancelled)() {
                // The current entry can be the final wildcard result, so check again before
                // `FindNextFileW` is allowed to translate `ERROR_NO_MORE_FILES` into success.
                return Err(EnumerateError::Cancelled);
            }
            if unsafe { FindNextFileW(handle.0, &mut data) } == 0 {
                let error = unsafe { GetLastError() };
                if error == ERROR_NO_MORE_FILES {
                    (self.report_progress)(ProjectMarkerCandidateProgress {
                        current_directory: directory.to_path_buf(),
                        file_count: self.file_count.saturating_sub(initial_file_count),
                        directory_count: self
                            .directory_count
                            .saturating_sub(initial_directory_count),
                    });
                    return Ok(());
                }
                return Err(EnumerateError::Io(io::Error::from_raw_os_error(
                    error as i32,
                )));
            }
        }
    }

    fn collect_entry(
        &mut self,
        directory: &Path,
        depth: usize,
        data: &WIN32_FIND_DATAW,
    ) -> Result<(), EnumerateError> {
        let name_length = data
            .cFileName
            .iter()
            .position(|value| *value == 0)
            .unwrap_or(data.cFileName.len());
        let name = OsString::from_wide(&data.cFileName[..name_length]);
        if name == "." || name == ".." || data.dwFileAttributes & FILE_ATTRIBUTE_REPARSE_POINT != 0
        {
            return Ok(());
        }
        let normalized_name = name.to_string_lossy().to_ascii_lowercase();
        let path = directory.join(name);
        if data.dwFileAttributes & FILE_ATTRIBUTE_DIRECTORY != 0 {
            self.directory_count = self.directory_count.saturating_add(1);
            if depth < self.maximum_depth
                && !normalized_name.starts_with('.')
                && !self.pruned_directory_names.contains(&normalized_name)
            {
                self.pending.push((path, depth + 1, false));
            }
        } else if self.file_names.contains(&normalized_name)
            || self
                .file_suffixes
                .iter()
                .any(|suffix| normalized_name.ends_with(suffix))
        {
            self.file_count = self.file_count.saturating_add(1);
            (self.consumer)(path).map_err(EnumerateError::Consumer)?;
            self.candidate_count = self.candidate_count.saturating_add(1);
        } else {
            self.file_count = self.file_count.saturating_add(1);
        }
        Ok(())
    }
}

fn large_fetch_is_unsupported(error: u32) -> bool {
    matches!(
        error,
        ERROR_INVALID_FUNCTION | ERROR_INVALID_PARAMETER | ERROR_NOT_SUPPORTED
    )
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

#[cfg(test)]
mod tests {
    use std::{
        fs,
        path::PathBuf,
        sync::atomic::{AtomicBool, AtomicU64, Ordering},
    };

    use super::{scan, ProjectMarkerScanRequest};

    #[test]
    fn scan_is_case_insensitive_and_prunes_generated_trees() {
        let fixture = TestDirectory::new();
        let visible = fixture.path.join("Workspace/App");
        let pruned = fixture.path.join("Workspace/App/node_modules/dependency");
        fs::create_dir_all(&visible).unwrap();
        fs::create_dir_all(&pruned).unwrap();
        fs::write(visible.join("PACKAGE.JSON"), b"{}").unwrap();
        fs::write(pruned.join("package.json"), b"{}").unwrap();

        let mut candidates = Vec::<PathBuf>::new();
        let file_names = ["package.json".to_string()];
        let pruned_directory_names = ["node_modules".to_string()];
        let summary = scan(
            ProjectMarkerScanRequest {
                root: &fixture.path,
                file_names: &file_names,
                file_suffixes: &[],
                pruned_directory_names: &pruned_directory_names,
                maximum_depth: 64,
                is_cancelled: &|| false,
                report_progress: &|_| {},
            },
            &mut |path| {
                candidates.push(path);
                Ok(())
            },
        )
        .unwrap();

        assert_eq!(summary.candidate_count, 1);
        assert_eq!(candidates, vec![visible.join("PACKAGE.JSON")]);
    }

    #[test]
    fn cancellation_during_the_final_directory_is_not_reported_as_success() {
        let fixture = TestDirectory::new();
        fs::write(fixture.path.join("package.json"), b"{}").unwrap();
        let cancelled = AtomicBool::new(false);

        let file_names = ["package.json".to_string()];
        let result = scan(
            ProjectMarkerScanRequest {
                root: &fixture.path,
                file_names: &file_names,
                file_suffixes: &[],
                pruned_directory_names: &[],
                maximum_depth: 64,
                is_cancelled: &|| cancelled.load(Ordering::Relaxed),
                report_progress: &|_| {},
            },
            &mut |_| {
                cancelled.store(true, Ordering::Relaxed);
                Ok(())
            },
        );

        assert!(matches!(
            result,
            Err(crate::ProjectMarkerCandidateScanError::Cancelled)
        ));
    }

    static TEST_DIRECTORY_SEQUENCE: AtomicU64 = AtomicU64::new(0);

    struct TestDirectory {
        path: PathBuf,
    }

    impl TestDirectory {
        fn new() -> Self {
            let sequence = TEST_DIRECTORY_SEQUENCE.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "mangodisk-project-markers-{}-{sequence}",
                std::process::id()
            ));
            let _ = fs::remove_dir_all(&path);
            fs::create_dir_all(&path).unwrap();
            Self { path }
        }
    }

    impl Drop for TestDirectory {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.path);
        }
    }
}

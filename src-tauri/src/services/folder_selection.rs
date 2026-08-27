use std::{collections::HashSet, fs, path::Path};

use mangodisk_platform::{current_platform, Platform};

pub struct FolderSelectionOutcome {
    pub paths: Vec<String>,
    pub skipped_unsafe_count: u64,
}

pub struct FolderSelectionService;

impl FolderSelectionService {
    /// Canonicalizes user-selected or dropped directories without following
    /// links. Keeping this platform integration outside the command leaves the
    /// IPC adapter responsible only for arguments, logging, and serialization.
    pub fn filter_directories(paths: Vec<String>) -> FolderSelectionOutcome {
        let mut seen = HashSet::new();
        let mut skipped_unsafe_count = 0_u64;
        let paths = paths
            .into_iter()
            .filter_map(|path| {
                let trimmed = path.trim();
                if trimmed.is_empty() {
                    return None;
                }
                let requested = Path::new(trimmed);
                let canonical = match current_platform().canonicalize_no_links(requested) {
                    Ok(path) => path,
                    Err(_) => {
                        skipped_unsafe_count += 1;
                        return None;
                    }
                };
                let metadata = fs::symlink_metadata(&canonical).ok()?;
                if !metadata.is_dir() {
                    return None;
                }
                seen.insert(canonical.clone())
                    .then(|| current_platform().display_path(&canonical))
            })
            .collect();
        FolderSelectionOutcome {
            paths,
            skipped_unsafe_count,
        }
    }
}

#[cfg(all(test, windows))]
mod windows_tests {
    use std::{
        path::PathBuf,
        time::{SystemTime, UNIX_EPOCH},
    };

    use super::*;

    struct DirectorySandbox(PathBuf);

    impl Drop for DirectorySandbox {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn directory_filter_deduplicates_windows_path_casing() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "MangoDisk-Directory-Identity-{}-{unique}",
            std::process::id()
        ));
        let _sandbox_cleanup = DirectorySandbox(root.clone());
        fs::create_dir_all(&root).expect("the directory identity fixture should be created");
        let original = root.to_string_lossy().into_owned();
        let different_case = original.to_uppercase();

        let outcome = FolderSelectionService::filter_directories(vec![original, different_case]);

        assert_eq!(outcome.skipped_unsafe_count, 0);
        assert_eq!(outcome.paths.len(), 1);
        assert!(!outcome.paths[0].starts_with(r"\\?\"));
    }
}

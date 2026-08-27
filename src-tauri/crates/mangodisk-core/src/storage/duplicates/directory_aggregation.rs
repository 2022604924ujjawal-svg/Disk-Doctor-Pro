use std::{
    collections::{HashMap, HashSet},
    fs::{self, File},
    io::Read,
    path::{Path, PathBuf},
};

use mangodisk_platform::{current_platform, Platform};

use super::{DuplicateFileEntry, DuplicateGroup, DuplicateGroupKind};
use crate::{
    filesystem::metadata::{modified_ms, native_path_string},
    shared::operation::OperationGuard,
};

const MAX_DIRECTORY_AGGREGATION_DEPTH: usize = 20;
const HASH_BUFFER_BYTES: usize = 1024 * 1024;

#[derive(Debug, Default)]
pub(super) struct DirectoryAggregationDiagnostics {
    pub(super) candidate_directory_count: u64,
    pub(super) aggregated_directory_group_count: u64,
    pub(super) aggregated_file_entry_count: u64,
}

#[derive(Debug)]
pub(super) struct DirectoryAggregationResult {
    pub(super) groups: Vec<DuplicateGroup>,
    pub(super) diagnostics: DirectoryAggregationDiagnostics,
}

#[derive(Debug, Clone)]
struct KnownFile {
    bytes: u64,
    hash: String,
}

#[derive(Debug, Clone, Copy, Default, Hash, PartialEq, Eq)]
struct DirectorySeed {
    bytes: u64,
    file_count: u64,
    hash_sum_low: u64,
    hash_sum_high: u64,
    hash_xor_low: u64,
    hash_xor_high: u64,
}

impl DirectorySeed {
    fn add(&mut self, file: &KnownFile) {
        let digest = blake3::hash(file.hash.as_bytes());
        let bytes = digest.as_bytes();
        let low = u64::from_le_bytes(bytes[0..8].try_into().expect("fixed hash slice"));
        let high = u64::from_le_bytes(bytes[8..16].try_into().expect("fixed hash slice"));
        self.bytes = self.bytes.saturating_add(file.bytes);
        self.file_count = self.file_count.saturating_add(1);
        self.hash_sum_low = self.hash_sum_low.wrapping_add(low);
        self.hash_sum_high = self.hash_sum_high.wrapping_add(high);
        self.hash_xor_low ^= low;
        self.hash_xor_high ^= high;
    }
}

#[derive(Debug, Clone, Hash, PartialEq, Eq)]
struct DirectoryFingerprint {
    digest: [u8; 32],
    bytes: u64,
    file_count: u64,
}

/// Replaces overlapping file groups with exact directory groups when every regular file in each
/// directory is already covered by an exact file hash. The cheap seed phase narrows candidates
/// without reading file contents again; the recursive fingerprint then proves identical structure
/// and content before any file-level group is suppressed.
pub(super) fn aggregate_exact_directories(
    roots: &[PathBuf],
    file_groups: Vec<DuplicateGroup>,
    operation: &OperationGuard,
) -> Result<DirectoryAggregationResult, String> {
    let known_files = known_files(&file_groups);
    let seeds = directory_seeds(roots, &known_files);
    let candidate_directory_count = u64::try_from(seeds.len()).unwrap_or(u64::MAX);
    let mut seed_groups = HashMap::<DirectorySeed, Vec<PathBuf>>::new();
    for (path, seed) in seeds {
        if seed.file_count > 0 {
            seed_groups.entry(seed).or_default().push(path);
        }
    }

    let mut fingerprint_groups = HashMap::<DirectoryFingerprint, Vec<PathBuf>>::new();
    for directories in seed_groups.into_values().filter(|items| items.len() > 1) {
        operation
            .ensure_not_cancelled()
            .map_err(|error| error.to_string())?;
        for directory in directories {
            if let Some(fingerprint) =
                fingerprint_known_directory(&directory, &known_files, operation, 0)?
            {
                fingerprint_groups
                    .entry(fingerprint)
                    .or_default()
                    .push(directory);
            }
        }
    }

    let mut candidates = fingerprint_groups
        .into_iter()
        .filter(|(_, paths)| paths.len() > 1)
        .collect::<Vec<_>>();
    candidates.sort_by(
        |(left_fingerprint, left_paths), (right_fingerprint, right_paths)| {
            right_fingerprint
                .bytes
                .cmp(&left_fingerprint.bytes)
                .then_with(|| {
                    right_fingerprint
                        .file_count
                        .cmp(&left_fingerprint.file_count)
                })
                // A parent containing only one matching child can have the same byte and file
                // counts as that child. Prefer the shallower candidate so the result always keeps
                // the largest exact directory instead of depending on lexical path ordering.
                .then_with(|| {
                    minimum_directory_depth(left_paths).cmp(&minimum_directory_depth(right_paths))
                })
                .then_with(|| left_paths[0].cmp(&right_paths[0]))
        },
    );

    // Larger parent groups own their descendants. Emitting both parent and child groups would
    // double-count reclaimable bytes and recreate the noisy nested result that aggregation avoids.
    let mut claimed_directories = Vec::<PathBuf>::new();
    let mut directory_groups = Vec::<DuplicateGroup>::new();
    let mut aggregated_file_entry_count = 0_u64;
    for (fingerprint, mut paths) in candidates {
        paths.sort();
        paths.dedup();
        paths.retain(|path| {
            !claimed_directories
                .iter()
                .any(|claimed| path.starts_with(claimed) || claimed.starts_with(path))
        });
        if paths.len() < 2 {
            continue;
        }
        aggregated_file_entry_count = aggregated_file_entry_count.saturating_add(
            fingerprint
                .file_count
                .saturating_mul(u64::try_from(paths.len()).unwrap_or(u64::MAX)),
        );
        claimed_directories.extend(paths.iter().cloned());
        directory_groups.push(build_directory_group(fingerprint, paths));
    }

    let claimed = claimed_directories.into_iter().collect::<HashSet<_>>();
    let mut groups = file_groups
        .into_iter()
        .filter_map(|mut group| {
            group.entries.retain(|entry| {
                let path = Path::new(&entry.path);
                !claimed.iter().any(|directory| path.starts_with(directory))
            });
            if group.entries.len() < 2 {
                return None;
            }
            group.reclaimable_bytes = group
                .bytes_per_file
                .saturating_mul(group.entries.len().saturating_sub(1) as u64);
            Some(group)
        })
        .collect::<Vec<_>>();
    groups.extend(directory_groups);
    groups.sort_by(|left, right| {
        right
            .reclaimable_bytes
            .cmp(&left.reclaimable_bytes)
            .then_with(|| left.hash.cmp(&right.hash))
    });

    Ok(DirectoryAggregationResult {
        diagnostics: DirectoryAggregationDiagnostics {
            candidate_directory_count,
            aggregated_directory_group_count: groups
                .iter()
                .filter(|group| group.kind == DuplicateGroupKind::Directory)
                .count() as u64,
            aggregated_file_entry_count,
        },
        groups,
    })
}

/// Recomputes a directory fingerprint from live file contents before permanent deletion.
pub(super) fn verify_live_directory(
    path: &Path,
    expected_hash: &str,
    expected_bytes: u64,
    expected_file_count: u64,
    operation: &OperationGuard,
) -> Result<(), String> {
    let fingerprint = fingerprint_live_directory(path, operation, 0)?
        .ok_or_else(|| "the directory contains an unsupported or inaccessible item".to_string())?;
    let actual_hash = directory_hash(&fingerprint.digest);
    if actual_hash != expected_hash
        || fingerprint.bytes != expected_bytes
        || fingerprint.file_count != expected_file_count
    {
        return Err("the directory contents changed after scanning".to_string());
    }
    Ok(())
}

fn known_files(groups: &[DuplicateGroup]) -> HashMap<PathBuf, KnownFile> {
    let mut files = HashMap::new();
    for group in groups {
        for entry in &group.entries {
            files.insert(
                PathBuf::from(&entry.path),
                KnownFile {
                    bytes: entry.bytes,
                    hash: group.hash.clone(),
                },
            );
        }
    }
    files
}

fn directory_seeds(
    roots: &[PathBuf],
    files: &HashMap<PathBuf, KnownFile>,
) -> HashMap<PathBuf, DirectorySeed> {
    let mut seeds = HashMap::<PathBuf, DirectorySeed>::new();
    for (path, file) in files {
        let Some(root) = roots
            .iter()
            .filter(|root| path.starts_with(root))
            .max_by_key(|root| root.components().count())
        else {
            continue;
        };
        let mut ancestor = path.parent();
        let mut depth = 0_usize;
        while let Some(directory) = ancestor {
            if directory == root || depth >= MAX_DIRECTORY_AGGREGATION_DEPTH {
                break;
            }
            seeds.entry(directory.to_path_buf()).or_default().add(file);
            ancestor = directory.parent();
            depth = depth.saturating_add(1);
        }
    }
    seeds
}

fn fingerprint_known_directory(
    path: &Path,
    known_files: &HashMap<PathBuf, KnownFile>,
    operation: &OperationGuard,
    depth: usize,
) -> Result<Option<DirectoryFingerprint>, String> {
    if depth > MAX_DIRECTORY_AGGREGATION_DEPTH {
        return Ok(None);
    }
    operation
        .ensure_not_cancelled()
        .map_err(|error| error.to_string())?;
    let entries = match fs::read_dir(path) {
        Ok(entries) => entries,
        Err(_) => return Ok(None),
    };
    let mut tokens = Vec::<Vec<u8>>::new();
    let mut bytes = 0_u64;
    let mut file_count = 0_u64;
    for entry in entries {
        let Ok(entry) = entry else {
            return Ok(None);
        };
        let child = entry.path();
        let Ok(metadata) = fs::symlink_metadata(&child) else {
            return Ok(None);
        };
        if current_platform().is_link_like(&metadata) {
            return Ok(None);
        }
        if metadata.is_file() {
            let Some(file) = known_files
                .get(&child)
                .filter(|file| file.bytes == metadata.len())
            else {
                return Ok(None);
            };
            bytes = bytes.saturating_add(file.bytes);
            file_count = file_count.saturating_add(1);
            tokens.push(file_token(file.bytes, file.hash.as_bytes()));
        } else if metadata.is_dir() {
            let Some(child_fingerprint) =
                fingerprint_known_directory(&child, known_files, operation, depth + 1)?
            else {
                return Ok(None);
            };
            bytes = bytes.saturating_add(child_fingerprint.bytes);
            file_count = file_count.saturating_add(child_fingerprint.file_count);
            tokens.push(directory_token(&child_fingerprint));
        } else {
            return Ok(None);
        }
    }
    if file_count == 0 {
        return Ok(None);
    }
    Ok(Some(fingerprint_tokens(tokens, bytes, file_count)))
}

fn fingerprint_live_directory(
    path: &Path,
    operation: &OperationGuard,
    depth: usize,
) -> Result<Option<DirectoryFingerprint>, String> {
    if depth > MAX_DIRECTORY_AGGREGATION_DEPTH {
        return Ok(None);
    }
    operation
        .ensure_not_cancelled()
        .map_err(|error| error.to_string())?;
    let entries = fs::read_dir(path).map_err(|error| error.to_string())?;
    let mut tokens = Vec::<Vec<u8>>::new();
    let mut bytes = 0_u64;
    let mut file_count = 0_u64;
    for entry in entries {
        let child = entry.map_err(|error| error.to_string())?.path();
        let metadata = fs::symlink_metadata(&child).map_err(|error| error.to_string())?;
        if current_platform().is_link_like(&metadata) {
            return Ok(None);
        }
        if metadata.is_file() {
            let hash = hash_file(&child, operation)?;
            bytes = bytes.saturating_add(metadata.len());
            file_count = file_count.saturating_add(1);
            tokens.push(file_token(metadata.len(), hash.as_bytes()));
        } else if metadata.is_dir() {
            let Some(child_fingerprint) = fingerprint_live_directory(&child, operation, depth + 1)?
            else {
                return Ok(None);
            };
            bytes = bytes.saturating_add(child_fingerprint.bytes);
            file_count = file_count.saturating_add(child_fingerprint.file_count);
            tokens.push(directory_token(&child_fingerprint));
        } else {
            return Ok(None);
        }
    }
    if file_count == 0 {
        return Ok(None);
    }
    Ok(Some(fingerprint_tokens(tokens, bytes, file_count)))
}

fn hash_file(path: &Path, operation: &OperationGuard) -> Result<String, String> {
    let mut file = File::open(path).map_err(|error| error.to_string())?;
    let mut hasher = blake3::Hasher::new();
    let mut buffer = vec![0_u8; HASH_BUFFER_BYTES];
    loop {
        operation
            .ensure_not_cancelled()
            .map_err(|error| error.to_string())?;
        let read = file.read(&mut buffer).map_err(|error| error.to_string())?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(hasher.finalize().to_hex().to_string())
}

fn fingerprint_tokens(
    mut tokens: Vec<Vec<u8>>,
    bytes: u64,
    file_count: u64,
) -> DirectoryFingerprint {
    tokens.sort();
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"mangodisk-duplicate-directory-v1");
    for token in tokens {
        hasher.update(&(token.len() as u64).to_le_bytes());
        hasher.update(&token);
    }
    DirectoryFingerprint {
        digest: *hasher.finalize().as_bytes(),
        bytes,
        file_count,
    }
}

fn file_token(bytes: u64, hash: &[u8]) -> Vec<u8> {
    let mut token = Vec::with_capacity(1 + 8 + hash.len());
    token.push(b'f');
    token.extend_from_slice(&bytes.to_le_bytes());
    token.extend_from_slice(hash);
    token
}

fn directory_token(fingerprint: &DirectoryFingerprint) -> Vec<u8> {
    let mut token = Vec::with_capacity(1 + 8 + 8 + fingerprint.digest.len());
    token.push(b'd');
    token.extend_from_slice(&fingerprint.bytes.to_le_bytes());
    token.extend_from_slice(&fingerprint.file_count.to_le_bytes());
    token.extend_from_slice(&fingerprint.digest);
    token
}

fn build_directory_group(fingerprint: DirectoryFingerprint, paths: Vec<PathBuf>) -> DuplicateGroup {
    let hash = directory_hash(&fingerprint.digest);
    let reclaimable_bytes = fingerprint
        .bytes
        .saturating_mul(paths.len().saturating_sub(1) as u64);
    DuplicateGroup {
        id: hash.chars().take(26).collect(),
        hash,
        kind: DuplicateGroupKind::Directory,
        bytes_per_file: fingerprint.bytes,
        file_count_per_entry: fingerprint.file_count,
        reclaimable_bytes,
        entries: paths
            .into_iter()
            .map(|path| {
                let metadata = fs::symlink_metadata(&path).ok();
                DuplicateFileEntry {
                    name: path
                        .file_name()
                        .map(|name| name.to_string_lossy().into_owned())
                        .unwrap_or_default(),
                    parent_path: native_path_string(path.parent().unwrap_or(&path)),
                    path: native_path_string(&path),
                    bytes: fingerprint.bytes,
                    modified_at_ms: metadata.as_ref().and_then(modified_ms),
                }
            })
            .collect(),
    }
}

fn directory_hash(digest: &[u8; 32]) -> String {
    format!("directory:{}", blake3::Hash::from_bytes(*digest).to_hex())
}

fn minimum_directory_depth(paths: &[PathBuf]) -> usize {
    paths
        .iter()
        .map(|path| path.components().count())
        .min()
        .unwrap_or(usize::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shared::operation::{test_operation_lock, CoordinatedOperationKind};

    fn test_root(name: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!(
            "mangodisk-directory-aggregation-{name}-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).expect("create directory aggregation fixture");
        root
    }

    fn file_group(hash: &str, bytes: u64, paths: &[PathBuf]) -> DuplicateGroup {
        DuplicateGroup {
            id: hash.to_string(),
            hash: hash.to_string(),
            kind: DuplicateGroupKind::File,
            bytes_per_file: bytes,
            file_count_per_entry: 1,
            reclaimable_bytes: bytes.saturating_mul(paths.len().saturating_sub(1) as u64),
            entries: paths
                .iter()
                .map(|path| DuplicateFileEntry {
                    name: path.file_name().unwrap().to_string_lossy().into_owned(),
                    path: native_path_string(path),
                    parent_path: native_path_string(path.parent().unwrap()),
                    bytes,
                    modified_at_ms: None,
                })
                .collect(),
        }
    }

    #[test]
    fn exact_directories_replace_overlapping_file_groups() {
        let _operation_lock = test_operation_lock();
        let root = test_root("exact");
        let left = root.join("left");
        let right = root.join("right");
        fs::create_dir_all(left.join("nested")).expect("create left");
        fs::create_dir_all(right.join("nested")).expect("create right");
        fs::write(left.join("a.bin"), b"one").expect("write left file");
        fs::write(right.join("renamed.bin"), b"one").expect("write right file");
        fs::write(left.join("nested/b.bin"), b"two").expect("write left nested file");
        fs::write(right.join("nested/c.bin"), b"two").expect("write right nested file");
        let groups = vec![
            file_group(
                "hash-one",
                3,
                &[left.join("a.bin"), right.join("renamed.bin")],
            ),
            file_group(
                "hash-two",
                3,
                &[left.join("nested/b.bin"), right.join("nested/c.bin")],
            ),
        ];
        let operation = OperationGuard::start(CoordinatedOperationKind::DuplicateFiles)
            .expect("start operation");
        let result = aggregate_exact_directories(std::slice::from_ref(&root), groups, &operation)
            .expect("aggregate directories");
        operation.complete();

        assert_eq!(result.groups.len(), 1);
        assert_eq!(result.groups[0].kind, DuplicateGroupKind::Directory);
        assert_eq!(result.groups[0].entries.len(), 2);
        assert_eq!(result.groups[0].file_count_per_entry, 2);
        assert_eq!(result.groups[0].bytes_per_file, 6);
        fs::remove_dir_all(root).expect("remove directory aggregation fixture");
    }

    #[test]
    fn directories_with_unhashed_files_remain_file_groups() {
        let _operation_lock = test_operation_lock();
        let root = test_root("unknown-file");
        let left = root.join("left");
        let right = root.join("right");
        fs::create_dir_all(&left).expect("create left");
        fs::create_dir_all(&right).expect("create right");
        fs::write(left.join("same.bin"), b"same").expect("write left duplicate");
        fs::write(right.join("same.bin"), b"same").expect("write right duplicate");
        fs::write(left.join("unique.bin"), b"left").expect("write unique file");
        let groups = vec![file_group(
            "hash-same",
            4,
            &[left.join("same.bin"), right.join("same.bin")],
        )];
        let operation = OperationGuard::start(CoordinatedOperationKind::DuplicateFiles)
            .expect("start operation");
        let result = aggregate_exact_directories(std::slice::from_ref(&root), groups, &operation)
            .expect("aggregate directories");
        operation.complete();

        assert_eq!(result.groups.len(), 1);
        assert_eq!(result.groups[0].kind, DuplicateGroupKind::File);
        fs::remove_dir_all(root).expect("remove directory aggregation fixture");
    }

    #[test]
    fn wrapper_directories_win_equal_size_ties() {
        let _operation_lock = test_operation_lock();
        let root = test_root("wrapper-parent");
        let left = root.join("left");
        let right = root.join("right");
        fs::create_dir_all(left.join("nested")).expect("create left wrapper");
        fs::create_dir_all(right.join("renamed-nested")).expect("create right wrapper");
        fs::write(left.join("nested/payload.bin"), b"same").expect("write left payload");
        fs::write(right.join("renamed-nested/copy.bin"), b"same").expect("write right payload");
        let groups = vec![file_group(
            "hash-same",
            4,
            &[
                left.join("nested/payload.bin"),
                right.join("renamed-nested/copy.bin"),
            ],
        )];
        let operation = OperationGuard::start(CoordinatedOperationKind::DuplicateFiles)
            .expect("start operation");
        let result = aggregate_exact_directories(std::slice::from_ref(&root), groups, &operation)
            .expect("aggregate wrapper directories");
        operation.complete();

        assert_eq!(result.groups.len(), 1);
        assert_eq!(result.groups[0].kind, DuplicateGroupKind::Directory);
        let paths = result.groups[0]
            .entries
            .iter()
            .map(|entry| PathBuf::from(&entry.path))
            .collect::<HashSet<_>>();
        assert_eq!(paths, HashSet::from([left, right]));
        fs::remove_dir_all(root).expect("remove directory aggregation fixture");
    }

    #[test]
    fn live_directory_verification_rejects_content_changes() {
        let _operation_lock = test_operation_lock();
        let root = test_root("live-verification");
        let directory = root.join("candidate");
        fs::create_dir_all(directory.join("nested")).expect("create candidate directory");
        fs::write(directory.join("a.bin"), b"one").expect("write candidate file");
        fs::write(directory.join("nested/b.bin"), b"two").expect("write nested candidate file");
        let operation = OperationGuard::start(CoordinatedOperationKind::PermanentDelete)
            .expect("start delete verification operation");
        let fingerprint = fingerprint_live_directory(&directory, &operation, 0)
            .expect("fingerprint candidate directory")
            .expect("candidate directory is supported");
        let expected_hash = directory_hash(&fingerprint.digest);

        verify_live_directory(
            &directory,
            &expected_hash,
            fingerprint.bytes,
            fingerprint.file_count,
            &operation,
        )
        .expect("unchanged directory should verify");
        fs::write(directory.join("a.bin"), b"changed").expect("mutate candidate file");
        let error = verify_live_directory(
            &directory,
            &expected_hash,
            fingerprint.bytes,
            fingerprint.file_count,
            &operation,
        )
        .expect_err("changed directory must fail closed");

        assert!(error.contains("changed after scanning"));
        operation.complete();
        fs::remove_dir_all(root).expect("remove directory aggregation fixture");
    }
}

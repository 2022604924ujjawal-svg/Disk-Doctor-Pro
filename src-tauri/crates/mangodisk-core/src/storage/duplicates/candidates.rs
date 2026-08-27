use std::{
    collections::{HashMap, HashSet},
    fs::{self, File},
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        mpsc::sync_channel,
    },
    thread,
    time::{SystemTime, UNIX_EPOCH},
};

use mangodisk_platform::{
    current_platform, Platform, PlatformCancellation, PlatformErrorCode, ScanPurpose,
};

use crate::shared::{
    operation::{OperationGuard, OPERATION_CANCELLED_ERROR},
    TraversalStage,
};

const PROTECTED_FILE_EXTENSIONS: [&str; 3] = ["bin", "dll", "jar"];

const IDENTITY_HINT_FAILURE_SAMPLE_LIMIT: usize = 3;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct FileIdentity {
    pub(super) volume: u64,
    pub(super) index: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum FileIdentitySource {
    Metadata,
    DirectoryHint,
    FileHandle,
}

#[derive(Clone)]
pub(super) struct FileCandidate {
    pub(super) root_ordinal: usize,
    pub(super) path: PathBuf,
    pub(super) bytes: u64,
    pub(super) modified_at: Option<SystemTime>,
    pub(super) modified_at_ms: Option<u64>,
    pub(super) identity: Option<FileIdentity>,
    pub(super) identity_source: Option<FileIdentitySource>,
}

#[derive(Debug)]
pub(super) struct IdentityHintFailureSample {
    pub(super) code: PlatformErrorCode,
    pub(super) diagnostic_digest: String,
}

struct DirectoryIdentityHintLoad {
    hint_count: usize,
    fallback_directory_count: usize,
    failure_samples: Vec<IdentityHintFailureSample>,
}

#[derive(Default)]
struct IdentityLoadMetrics {
    worker_count: usize,
    peak_in_flight: usize,
}

impl IdentityLoadMetrics {
    fn include(&mut self, other: Self) {
        self.worker_count = self.worker_count.max(other.worker_count);
        self.peak_in_flight = self.peak_in_flight.max(other.peak_in_flight);
    }
}

pub(super) struct PhysicalIdentityFilter {
    pub(super) candidates: Vec<FileCandidate>,
    pub(super) alias_count: usize,
    pub(super) unavailable_count: usize,
    pub(super) worker_count: usize,
    pub(super) peak_in_flight: usize,
    pub(super) hint_count: usize,
    pub(super) verified_hint_count: usize,
    pub(super) hint_fallback_directory_count: usize,
    pub(super) hint_failure_samples: Vec<IdentityHintFailureSample>,
}

/// Defines which files and subtrees are meaningful during one duplicate discovery scan.
///
/// The broad-scope decision is intentionally computed once per root. Resolving platform user
/// directories for every candidate previously added repeated filesystem work to the hottest
/// classification path and made native and generic enumeration harder to keep semantically
/// identical.
#[derive(Clone, Copy)]
pub(super) struct DuplicateCandidatePolicy {
    broad_discovery: bool,
}

impl DuplicateCandidatePolicy {
    pub(super) fn for_scan_root(scan_root: &Path) -> Self {
        Self {
            broad_discovery: is_broad_user_scope(scan_root),
        }
    }

    pub(super) fn should_prune_directory(path: &Path) -> bool {
        let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
            return false;
        };
        // Hidden implementation trees are noisy during broad discovery and often contain VCS or
        // tool metadata rather than independent user copies. Visible build and dependency folders
        // deliberately remain eligible: their names are not reliable safety boundaries, and both
        // developers and ordinary applications can store user-managed files inside them.
        name.starts_with('.')
    }

    pub(super) fn should_exclude_file(self, path: &Path) -> bool {
        if path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.eq_ignore_ascii_case(".DS_Store"))
        {
            return true;
        }
        self.broad_discovery
            && path
                .extension()
                .and_then(|extension| extension.to_str())
                .is_some_and(|extension| {
                    PROTECTED_FILE_EXTENSIONS
                        .iter()
                        .any(|candidate| extension.eq_ignore_ascii_case(candidate))
                })
    }
}

pub(super) struct CandidateEnumeration<'a> {
    root_ordinal: usize,
    minimum_bytes: u64,
    visit: &'a dyn Fn(TraversalStage, &Path, u64),
    size_groups: &'a mut HashMap<u64, Vec<FileCandidate>>,
    skipped_count: &'a mut u64,
    scanned_file_count: &'a mut u64,
    operation: &'a OperationGuard,
    policy: DuplicateCandidatePolicy,
}

pub(super) struct CandidateEnumerationRequest<'a> {
    pub(super) root_ordinal: usize,
    pub(super) minimum_bytes: u64,
    pub(super) visit: &'a dyn Fn(TraversalStage, &Path, u64),
    pub(super) size_groups: &'a mut HashMap<u64, Vec<FileCandidate>>,
    pub(super) skipped_count: &'a mut u64,
    pub(super) scanned_file_count: &'a mut u64,
    pub(super) operation: &'a OperationGuard,
    pub(super) policy: DuplicateCandidatePolicy,
}

impl<'a> CandidateEnumeration<'a> {
    pub(super) fn new(request: CandidateEnumerationRequest<'a>) -> Self {
        Self {
            root_ordinal: request.root_ordinal,
            minimum_bytes: request.minimum_bytes,
            visit: request.visit,
            size_groups: request.size_groups,
            skipped_count: request.skipped_count,
            scanned_file_count: request.scanned_file_count,
            operation: request.operation,
            policy: request.policy,
        }
    }

    pub(super) fn scan(&mut self, path: &Path, scan_root: &Path) -> Result<(), String> {
        self.operation
            .ensure_not_cancelled()
            .map_err(|error| error.to_string())?;
        let entries = match fs::read_dir(path) {
            Ok(entries) => entries,
            Err(_) => {
                *self.skipped_count += 1;
                return Ok(());
            }
        };
        for entry in entries {
            self.operation
                .ensure_not_cancelled()
                .map_err(|error| error.to_string())?;
            let Ok(entry) = entry else {
                *self.skipped_count += 1;
                continue;
            };
            let child = entry.path();
            if current_platform()
                .should_skip(&child, scan_root, ScanPurpose::DuplicateFiles)
                .is_some()
            {
                *self.skipped_count += 1;
                continue;
            }
            let Ok(metadata) = fs::symlink_metadata(&child) else {
                *self.skipped_count += 1;
                continue;
            };
            if current_platform().is_link_like(&metadata) {
                *self.skipped_count += 1;
                continue;
            }
            if metadata.is_dir() {
                if DuplicateCandidatePolicy::should_prune_directory(&child) {
                    // Broad duplicate discovery should expose independent user copies, not hidden
                    // implementation trees. An explicitly selected hidden root remains inspectable
                    // because only descendants are evaluated here.
                    *self.skipped_count += 1;
                    continue;
                }
                self.scan(&child, scan_root)?;
                continue;
            }
            if !metadata.is_file() {
                continue;
            }
            *self.scanned_file_count += 1;
            (self.visit)(TraversalStage::Analyzing, &child, metadata.len());
            if metadata.len() < self.minimum_bytes {
                continue;
            }
            if self.policy.should_exclude_file(&child) {
                *self.skipped_count += 1;
                continue;
            }
            let identity = initial_file_identity(&metadata);
            self.size_groups
                .entry(metadata.len())
                .or_default()
                .push(FileCandidate {
                    root_ordinal: self.root_ordinal,
                    path: child,
                    bytes: metadata.len(),
                    modified_at: metadata.modified().ok(),
                    modified_at_ms: modified_ms(&metadata),
                    identity,
                    identity_source: identity.map(|_| FileIdentitySource::Metadata),
                });
        }
        Ok(())
    }
}

/// Returns whether one root represents broad discovery rather than a narrowly selected folder.
///
/// Binary payload extensions are poor cleanup candidates during home- or volume-wide discovery,
/// but the exact duplicate engine must still honor a user who explicitly selects a smaller folder
/// containing those file types. Keeping this decision beside candidate classification prevents
/// platform traversal and presentation code from inventing different meanings for the same scan.
fn is_broad_user_scope(scan_root: &Path) -> bool {
    scan_root == current_platform().system_volume_path()
        || current_platform()
            .user_directories()
            .is_ok_and(|directories| directories.home_directory().starts_with(scan_root))
}

/// Returns the generated or hidden subtree containing a native candidate.
///
/// Generic traversal stops at these directories before reading their children. Native filesystem
/// enumeration may discover candidates without walking the same call stack, so this component
/// check preserves identical product semantics instead of leaking build artifacts into results.
pub(super) fn pruned_directory_ancestor(scan_root: &Path, path: &Path) -> Option<PathBuf> {
    let relative = path.strip_prefix(scan_root).ok()?;
    let mut ancestor = scan_root.to_path_buf();
    for component in relative.parent()?.components() {
        ancestor.push(component.as_os_str());
        if component.as_os_str().to_str().is_none() {
            continue;
        }
        if DuplicateCandidatePolicy::should_prune_directory(&ancestor) {
            return Some(ancestor);
        }
    }
    None
}

/// Revalidates one untrusted native candidate before it enters size grouping.
///
/// Platform-native enumeration only accelerates discovery. Core still owns link rejection,
/// purpose-specific protection, live size and modification time, and stable file identity.
pub(super) struct NativeCandidateRequest<'a> {
    pub(super) root_ordinal: usize,
    pub(super) path: PathBuf,
    pub(super) scan_root: &'a Path,
    pub(super) minimum_bytes: u64,
    pub(super) size_groups: &'a mut HashMap<u64, Vec<FileCandidate>>,
    pub(super) skipped_count: &'a mut u64,
    pub(super) operation: &'a OperationGuard,
    pub(super) policy: DuplicateCandidatePolicy,
}

pub(super) fn collect_native_candidate(request: NativeCandidateRequest<'_>) -> Result<(), String> {
    request
        .operation
        .ensure_not_cancelled()
        .map_err(|error| error.to_string())?;
    if current_platform()
        .should_skip(
            &request.path,
            request.scan_root,
            ScanPurpose::DuplicateFiles,
        )
        .is_some()
    {
        *request.skipped_count = request.skipped_count.saturating_add(1);
        return Ok(());
    }
    let Ok(metadata) = fs::symlink_metadata(&request.path) else {
        *request.skipped_count = request.skipped_count.saturating_add(1);
        return Ok(());
    };
    if !metadata.is_file() || current_platform().is_link_like(&metadata) {
        *request.skipped_count = request.skipped_count.saturating_add(1);
        return Ok(());
    }
    let bytes = metadata.len();
    let excluded = request.policy.should_exclude_file(&request.path);
    if bytes < request.minimum_bytes || excluded {
        if bytes >= request.minimum_bytes && excluded {
            *request.skipped_count = request.skipped_count.saturating_add(1);
        }
        return Ok(());
    }
    let identity = initial_file_identity(&metadata);
    request
        .size_groups
        .entry(bytes)
        .or_default()
        .push(FileCandidate {
            root_ordinal: request.root_ordinal,
            path: request.path,
            bytes,
            modified_at: metadata.modified().ok(),
            modified_at_ms: modified_ms(&metadata),
            identity,
            identity_source: identity.map(|_| FileIdentitySource::Metadata),
        });
    Ok(())
}

pub(super) fn remove_physical_aliases(
    mut candidates: Vec<FileCandidate>,
    configured_worker_count: usize,
    cancellation: &PlatformCancellation,
    observe: impl Fn(&Path) -> Result<(), String> + Sync,
) -> Result<PhysicalIdentityFilter, String> {
    let hint_load = load_directory_identity_hints(&mut candidates, cancellation)?;
    let fallback_indices = candidates
        .iter()
        .enumerate()
        .filter_map(|(index, candidate)| candidate.identity.is_none().then_some(index))
        .collect::<Vec<_>>();
    let mut identity_load = load_file_identities(
        &mut candidates,
        &fallback_indices,
        configured_worker_count,
        &observe,
    )?;
    let hinted_collision_indices = hinted_identity_collision_indices(&candidates);
    identity_load.include(load_file_identities(
        &mut candidates,
        &hinted_collision_indices,
        configured_worker_count,
        &observe,
    )?);

    let mut observed_indices = vec![false; candidates.len()];
    for &index in fallback_indices
        .iter()
        .chain(hinted_collision_indices.iter())
    {
        observed_indices[index] = true;
    }

    let mut identities = HashSet::<(u64, u64)>::new();
    let mut alias_count = 0_usize;
    let mut unavailable_count = 0_usize;
    let mut unique_candidates = Vec::with_capacity(candidates.len());
    for (candidate_index, candidate) in candidates.into_iter().enumerate() {
        if !observed_indices[candidate_index] {
            // Metadata or a unique native hint supplied this identity without a fallback file open.
            // Preserve the original per-candidate cancellation and progress contract.
            observe(&candidate.path)?;
        }
        let Some(identity) = candidate.identity else {
            // Exact duplicate deletion requires stable physical identity. An unavailable identity
            // fails closed because a same-size path replacement cannot be proven safe.
            unavailable_count = unavailable_count.saturating_add(1);
            continue;
        };
        if identities.insert((identity.volume, identity.index)) {
            unique_candidates.push(candidate);
        } else {
            alias_count = alias_count.saturating_add(1);
        }
    }
    Ok(PhysicalIdentityFilter {
        candidates: unique_candidates,
        alias_count,
        unavailable_count,
        worker_count: identity_load.worker_count,
        peak_in_flight: identity_load.peak_in_flight,
        hint_count: hint_load.hint_count,
        verified_hint_count: hinted_collision_indices.len(),
        hint_fallback_directory_count: hint_load.fallback_directory_count,
        hint_failure_samples: hint_load.failure_samples,
    })
}

fn load_directory_identity_hints(
    candidates: &mut [FileCandidate],
    cancellation: &PlatformCancellation,
) -> Result<DirectoryIdentityHintLoad, String> {
    let mut candidates_by_parent = HashMap::<PathBuf, Vec<usize>>::new();
    for (index, candidate) in candidates.iter().enumerate() {
        if candidate.identity.is_some() {
            continue;
        }
        let Some(parent) = candidate.path.parent() else {
            continue;
        };
        candidates_by_parent
            .entry(parent.to_path_buf())
            .or_default()
            .push(index);
    }

    let mut hint_count = 0_usize;
    let mut fallback_directory_count = 0_usize;
    let mut failure_samples = Vec::<IdentityHintFailureSample>::new();
    for (parent, indices) in candidates_by_parent {
        if cancellation.is_cancelled() {
            return Err(OPERATION_CANCELLED_ERROR.to_string());
        }
        let hints = match current_platform().directory_entry_identities(&parent, cancellation) {
            Ok(Some(hints)) => hints,
            Ok(None) => continue,
            Err(error)
                if cancellation.is_cancelled()
                    || error.code() == PlatformErrorCode::UserCancelled =>
            {
                return Err(OPERATION_CANCELLED_ERROR.to_string());
            }
            Err(error) => {
                fallback_directory_count = fallback_directory_count.saturating_add(1);
                let diagnostic_digest = blake3::hash(error.as_bytes()).to_hex().to_string();
                if failure_samples.len() < IDENTITY_HINT_FAILURE_SAMPLE_LIMIT
                    && !failure_samples.iter().any(|sample| {
                        sample.code == error.code() && sample.diagnostic_digest == diagnostic_digest
                    })
                {
                    failure_samples.push(IdentityHintFailureSample {
                        code: error.code(),
                        diagnostic_digest,
                    });
                }
                continue;
            }
        };
        for index in indices {
            let Some(hint) = candidates[index]
                .path
                .file_name()
                .and_then(|name| hints.get(name))
            else {
                continue;
            };
            candidates[index].identity = Some(FileIdentity {
                volume: hint.volume,
                index: hint.index,
            });
            candidates[index].identity_source = Some(FileIdentitySource::DirectoryHint);
            hint_count = hint_count.saturating_add(1);
        }
    }
    Ok(DirectoryIdentityHintLoad {
        hint_count,
        fallback_directory_count,
        failure_samples,
    })
}

fn hinted_identity_collision_indices(candidates: &[FileCandidate]) -> Vec<usize> {
    let identity_counts = candidates.iter().fold(
        HashMap::<(u64, u64), usize>::new(),
        |mut counts, candidate| {
            if let Some(identity) = candidate.identity {
                *counts.entry((identity.volume, identity.index)).or_default() += 1;
            }
            counts
        },
    );
    candidates
        .iter()
        .enumerate()
        .filter_map(|(index, candidate)| {
            let identity = candidate.identity?;
            (candidate.identity_source == Some(FileIdentitySource::DirectoryHint)
                && identity_counts
                    .get(&(identity.volume, identity.index))
                    .is_some_and(|count| *count > 1))
            .then_some(index)
        })
        .collect()
}

fn load_file_identities(
    candidates: &mut [FileCandidate],
    candidate_indices: &[usize],
    configured_worker_count: usize,
    observe: &(impl Fn(&Path) -> Result<(), String> + Sync),
) -> Result<IdentityLoadMetrics, String> {
    if candidate_indices.is_empty() {
        return Ok(IdentityLoadMetrics::default());
    }

    let worker_count = configured_worker_count.max(1).min(candidate_indices.len());
    let next_task = AtomicUsize::new(0);
    let active_tasks = AtomicUsize::new(0);
    let peak_in_flight = AtomicUsize::new(0);
    let stopped = AtomicBool::new(false);
    let mut identities = vec![None; candidates.len()];
    let mut first_error = None;

    thread::scope(|scope| -> Result<(), String> {
        let (sender, receiver) = sync_channel(worker_count.saturating_mul(2));
        let workers = (0..worker_count)
            .map(|_| {
                let sender = sender.clone();
                let next_task = &next_task;
                let active_tasks = &active_tasks;
                let peak_in_flight = &peak_in_flight;
                let stopped = &stopped;
                let candidates = &*candidates;
                scope.spawn(move || loop {
                    if stopped.load(Ordering::Acquire) {
                        break;
                    }
                    let task_position = next_task.fetch_add(1, Ordering::Relaxed);
                    let Some(&candidate_index) = candidate_indices.get(task_position) else {
                        break;
                    };
                    let candidate = &candidates[candidate_index];
                    let active = active_tasks.fetch_add(1, Ordering::AcqRel) + 1;
                    peak_in_flight.fetch_max(active, Ordering::AcqRel);
                    let result = observe(&candidate.path)
                        .map(|()| load_file_identity(&candidate.path, candidate.bytes));
                    active_tasks.fetch_sub(1, Ordering::AcqRel);
                    if result.is_err() {
                        stopped.store(true, Ordering::Release);
                    }
                    if sender.send((candidate_index, result)).is_err() {
                        break;
                    }
                })
            })
            .collect::<Vec<_>>();
        drop(sender);

        while let Ok((candidate_index, result)) = receiver.recv() {
            match result {
                Ok(identity) => identities[candidate_index] = identity,
                Err(error) if first_error.is_none() => first_error = Some(error),
                Err(_) => {}
            }
        }
        for worker in workers {
            worker
                .join()
                .map_err(|_| "a duplicate-file identity worker exited unexpectedly".to_string())?;
        }
        Ok(())
    })?;

    if let Some(error) = first_error {
        return Err(error);
    }
    for &candidate_index in candidate_indices {
        candidates[candidate_index].identity = identities[candidate_index];
        candidates[candidate_index].identity_source =
            identities[candidate_index].map(|_| FileIdentitySource::FileHandle);
    }
    Ok(IdentityLoadMetrics {
        worker_count,
        peak_in_flight: peak_in_flight.load(Ordering::Acquire),
    })
}

pub(super) fn normalize_roots(roots: Vec<String>) -> Result<Vec<PathBuf>, String> {
    let mut canonical = roots
        .into_iter()
        .filter(|value| !value.trim().is_empty())
        .map(PathBuf::from)
        .map(|path| {
            let canonical = current_platform()
                .canonicalize_no_links(&path)
                .map_err(|error| format!("the duplicate-file scan root is unsafe: {error}"))?;
            let metadata = fs::symlink_metadata(&canonical).map_err(|error| {
                format!("failed to access the duplicate-file scan root: {error}")
            })?;
            if !metadata.is_dir() {
                return Err(
                    "the duplicate-file scan root must be a directory or volume".to_string()
                );
            }
            Ok(canonical)
        })
        .collect::<Result<Vec<_>, _>>()?;
    if canonical.iter().any(|path| !path.is_dir()) {
        return Err("the duplicate-file scan root must be a directory or volume".to_string());
    }
    // Root ordinals enter cache keys and file facts. Native path ordering provides a stable
    // secondary key without lossy string conversion when equal-depth roots are reordered.
    canonical.sort_by(|left, right| {
        left.components()
            .count()
            .cmp(&right.components().count())
            .then_with(|| left.cmp(right))
    });
    canonical.dedup();
    let mut normalized = Vec::<PathBuf>::new();
    for path in canonical {
        if !normalized.iter().any(|root| path.starts_with(root)) {
            normalized.push(path);
        }
    }
    Ok(normalized)
}

pub(super) fn validate_open_file(
    candidate: &FileCandidate,
    file: &File,
    verify_identity: bool,
) -> Result<(), String> {
    let metadata = file.metadata().map_err(|error| error.to_string())?;
    if !metadata.is_file()
        || metadata.len() != candidate.bytes
        || metadata.modified().ok() != candidate.modified_at
        || (verify_identity
            && candidate
                .identity
                .is_some_and(|expected| file_identity(file, &metadata) != Some(expected)))
    {
        return Err("the file changed during duplicate-content verification".to_string());
    }
    Ok(())
}

pub(super) fn validate_current_path(candidate: &FileCandidate) -> Result<(), String> {
    let metadata = fs::symlink_metadata(&candidate.path).map_err(|error| error.to_string())?;
    if !metadata.is_file()
        || current_platform().is_link_like(&metadata)
        || metadata.len() != candidate.bytes
        || metadata.modified().ok() != candidate.modified_at
    {
        return Err("the file path changed during duplicate-content verification".to_string());
    }
    if candidate
        .identity
        .is_some_and(|expected| path_file_identity(&candidate.path, &metadata) != Some(expected))
    {
        return Err(
            "the file path now refers to a different object during duplicate-content verification"
                .to_string(),
        );
    }
    Ok(())
}

#[cfg(unix)]
fn initial_file_identity(metadata: &fs::Metadata) -> Option<FileIdentity> {
    use std::os::unix::fs::MetadataExt;
    Some(FileIdentity {
        volume: metadata.dev(),
        index: metadata.ino(),
    })
}

#[cfg(windows)]
fn initial_file_identity(_metadata: &fs::Metadata) -> Option<FileIdentity> {
    None
}

#[cfg(unix)]
fn file_identity(_file: &File, metadata: &fs::Metadata) -> Option<FileIdentity> {
    initial_file_identity(metadata)
}

#[cfg(windows)]
fn file_identity(file: &File, _metadata: &fs::Metadata) -> Option<FileIdentity> {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Storage::FileSystem::{
        GetFileInformationByHandle, BY_HANDLE_FILE_INFORMATION,
    };

    let mut information = BY_HANDLE_FILE_INFORMATION::default();
    // Stable NTFS identity needs the read-only Win32 volume serial and file index because stable
    // Rust metadata APIs do not expose both values. `File` owns and closes the borrowed handle.
    let succeeded =
        unsafe { GetFileInformationByHandle(file.as_raw_handle(), &mut information) } != 0;
    succeeded.then_some(FileIdentity {
        volume: u64::from(information.dwVolumeSerialNumber),
        index: (u64::from(information.nFileIndexHigh) << 32) | u64::from(information.nFileIndexLow),
    })
}

fn path_file_identity(path: &Path, metadata: &fs::Metadata) -> Option<FileIdentity> {
    initial_file_identity(metadata).or_else(|| {
        let file = File::open(path).ok()?;
        let opened_metadata = file.metadata().ok()?;
        file_identity(&file, &opened_metadata)
    })
}

pub(super) fn load_file_identity(path: &Path, _expected_bytes: u64) -> Option<FileIdentity> {
    let file = File::open(path).ok()?;
    let metadata = file.metadata().ok()?;
    file_identity(&file, &metadata)
}

pub(super) fn modified_ms(metadata: &fs::Metadata) -> Option<u64> {
    metadata
        .modified()
        .ok()
        .and_then(|value| value.duration_since(UNIX_EPOCH).ok())
        .map(|duration| duration.as_millis() as u64)
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use mangodisk_platform::{current_platform, Platform};

    use super::{pruned_directory_ancestor, DuplicateCandidatePolicy};

    #[test]
    fn native_candidates_preserve_hidden_subtree_exclusions() {
        let root = Path::new("/workspace");
        assert_eq!(
            pruned_directory_ancestor(
                root,
                Path::new("/workspace/project/.cache/package/archive.bin")
            ),
            Some(PathBuf::from("/workspace/project/.cache"))
        );
        assert_eq!(
            pruned_directory_ancestor(
                root,
                Path::new("/workspace/project/node_modules/package/archive.bin")
            ),
            None
        );
    }

    #[test]
    fn an_explicit_pruned_root_is_not_silently_excluded() {
        let root = Path::new("/workspace/node_modules");
        assert_eq!(
            pruned_directory_ancestor(
                root,
                Path::new("/workspace/node_modules/package/archive.bin")
            ),
            None
        );
    }

    #[test]
    fn discovery_prunes_hidden_but_not_visible_technical_directories() {
        assert!(DuplicateCandidatePolicy::should_prune_directory(Path::new(
            "/workspace/.cache"
        )));
        assert!(!DuplicateCandidatePolicy::should_prune_directory(
            Path::new("/workspace/node_modules")
        ));
        assert!(!DuplicateCandidatePolicy::should_prune_directory(
            Path::new("/workspace/project/target")
        ));
        assert!(!DuplicateCandidatePolicy::should_prune_directory(
            Path::new("/workspace/project/build")
        ));
        assert!(!DuplicateCandidatePolicy::should_prune_directory(
            Path::new("/workspace/Documents")
        ));
    }

    #[test]
    fn protected_payload_extensions_apply_only_to_broad_discovery() {
        let volume_root = current_platform().system_volume_path();
        let broad_payload = volume_root.join("payload.bin");
        assert!(DuplicateCandidatePolicy::for_scan_root(&volume_root)
            .should_exclude_file(&broad_payload));

        let narrow_root = std::env::temp_dir().join("mangodisk-explicit-duplicate-scope");
        let narrow_policy = DuplicateCandidatePolicy::for_scan_root(&narrow_root);
        assert!(!narrow_policy.should_exclude_file(&narrow_root.join("payload.bin")));
        assert!(narrow_policy.should_exclude_file(&narrow_root.join(".DS_Store")));
    }

    #[test]
    fn user_container_is_treated_as_broad_discovery() {
        let directories = current_platform()
            .user_directories()
            .expect("platform user directories should be available");
        let home = directories.home_directory();
        let user_container = home
            .parent()
            .expect("the user home should have a parent directory");

        assert!(DuplicateCandidatePolicy::for_scan_root(user_container)
            .should_exclude_file(&user_container.join("payload.bin")));
    }
}

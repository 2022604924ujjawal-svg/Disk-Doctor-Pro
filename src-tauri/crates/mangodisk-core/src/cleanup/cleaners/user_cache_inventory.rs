use std::{
    fs,
    os::unix::fs::MetadataExt,
    path::{Path, PathBuf},
    sync::{Mutex, OnceLock},
    time::Instant,
};

use mangodisk_platform::{current_platform, Platform, ScanPurpose};

use crate::{
    applications::catalog::{ApplicationInventory, ProcessSnapshot},
    cleanup::rules::root_validation::validate_automatic_cleanup_root,
    cleanup::source_selection::SourceScope,
    cleanup::{
        CleanupActionKind, CleanupActionReason, CleanupActionResult, CleanupActionStatus,
        CleanupCategory, CleanupGroup, CleanupSourceDetail, RiskLevel, ScanItemStatus,
        ScanRuleResult,
    },
    filesystem::{
        metadata::{
            diagnostic_path, display_fingerprint, display_path, modified_ms,
            snapshot_metadata_tree, snapshot_metadata_tree_with_observer,
        },
        permanent_delete::{delete_path_permanently, prepare_path_for_permanent_delete},
    },
    shared::operation::OperationGuard,
};

pub(super) const CLEANER_ID: &str = "special.additional-user-caches";
pub(super) const CLEANER_REVISION: &str =
    "additional-user-caches-v4-file-provider-state-protection";

const OWN_CACHE_DIRECTORIES: &[&str] = &[crate::APPLICATION_IDENTIFIER];

static LAST_PREVIEW: OnceLock<Mutex<Option<Vec<CacheCandidate>>>> = OnceLock::new();

#[derive(Debug, Clone, PartialEq, Eq)]
struct CacheCandidate {
    path: PathBuf,
    boundary_root: PathBuf,
    ownership_key: String,
    bytes: u64,
    file_count: u64,
    modified_at_ms: Option<u64>,
    fingerprint: String,
}

#[derive(Default)]
struct Discovery {
    candidates: Vec<CacheCandidate>,
    skipped_count: u64,
    active_count: u64,
    protected_count: u64,
}

struct SandboxDiscoveryContext<'a> {
    inventory: &'a ApplicationInventory,
    processes: &'a ProcessSnapshot,
    declared_roots: &'a [PathBuf],
    is_cancelled: &'a (dyn Fn() -> bool + Sync),
    report_path: &'a (dyn Fn(&Path) + Sync),
    report_files: &'a (dyn Fn(&Path, u64, u64) + Sync),
}

pub(super) fn preview(
    inventory: &ApplicationInventory,
    declared_roots: &[PathBuf],
    is_cancelled: &(dyn Fn() -> bool + Sync),
    report_path: &(dyn Fn(&Path) + Sync),
    report_files: &(dyn Fn(&Path, u64, u64) + Sync),
) -> ScanRuleResult {
    let started = Instant::now();
    if replace_last_preview(None).is_err() {
        log::warn!("additional_user_cache_preview_failed reason=snapshotState");
        return limited_rule_with_elapsed(started.elapsed().as_millis() as u64);
    }
    let processes = ProcessSnapshot::capture();
    let result = match processes {
        Ok(processes) => discover(
            inventory,
            &processes,
            declared_roots,
            is_cancelled,
            report_path,
            report_files,
        ),
        Err(error) => {
            log::warn!(
                "additional_user_cache_preview_failed reason=processSnapshot error_digest={}",
                blake3::hash(error.as_bytes()).to_hex()
            );
            return limited_rule_with_elapsed(started.elapsed().as_millis() as u64);
        }
    };
    let discovery = match result {
        Ok(discovery) => discovery,
        Err(error) => {
            log::warn!(
                "additional_user_cache_preview_failed reason=discovery error_digest={}",
                blake3::hash(error.as_bytes()).to_hex()
            );
            return limited_rule_with_elapsed(started.elapsed().as_millis() as u64);
        }
    };
    if replace_last_preview(Some(discovery.candidates.clone())).is_err() {
        log::warn!("additional_user_cache_preview_failed reason=snapshotState");
        return limited_rule_with_elapsed(started.elapsed().as_millis() as u64);
    }

    let bytes = discovery
        .candidates
        .iter()
        .map(|candidate| candidate.bytes)
        .sum();
    let file_count = discovery
        .candidates
        .iter()
        .map(|candidate| candidate.file_count)
        .sum();
    let source_count = discovery.candidates.len() as u64;
    let mut sources = discovery
        .candidates
        .iter()
        .map(|candidate| CleanupSourceDetail {
            path: display_path(&candidate.path),
            bytes: candidate.bytes,
            file_count: candidate.file_count,
            modified_at_ms: candidate.modified_at_ms,
            block_reason: None,
        })
        .collect::<Vec<_>>();
    sources.sort_by(|left, right| {
        right
            .bytes
            .cmp(&left.bytes)
            .then_with(|| left.path.cmp(&right.path))
    });
    log::info!(
        "additional_user_cache_preview_finished candidate_count={} active_excluded_count={} protected_excluded_count={} skipped_count={} bytes={} elapsed_ms={}",
        source_count,
        discovery.active_count,
        discovery.protected_count,
        discovery.skipped_count,
        bytes,
        started.elapsed().as_millis()
    );
    ScanRuleResult {
        rule_id: CLEANER_ID.to_string(),
        category: CleanupCategory::Application,
        group: CleanupGroup::UserCache,
        risk: RiskLevel::Recoverable,
        default_selected: false,
        recommended_selected: false,
        bytes,
        file_count,
        available: true,
        selectable: !discovery.candidates.is_empty(),
        status: if discovery.candidates.is_empty() {
            ScanItemStatus::Clean
        } else {
            ScanItemStatus::Found
        },
        running_processes: Vec::new(),
        requires_app_close: false,
        sources,
        source_count,
        sources_truncated: false,
        scan_elapsed_ms: started.elapsed().as_millis() as u64,
    }
}

fn replace_last_preview(candidates: Option<Vec<CacheCandidate>>) -> Result<(), String> {
    let mut snapshot = LAST_PREVIEW
        .get_or_init(|| Mutex::new(None))
        .lock()
        .map_err(|_| "the additional user cache snapshot lock is poisoned".to_string())?;
    *snapshot = candidates;
    Ok(())
}

pub(super) fn limited_rule() -> ScanRuleResult {
    limited_rule_with_elapsed(0)
}

fn limited_rule_with_elapsed(elapsed_ms: u64) -> ScanRuleResult {
    ScanRuleResult {
        rule_id: CLEANER_ID.to_string(),
        category: CleanupCategory::Application,
        group: CleanupGroup::UserCache,
        risk: RiskLevel::Recoverable,
        default_selected: false,
        recommended_selected: false,
        bytes: 0,
        file_count: 0,
        available: true,
        selectable: false,
        status: ScanItemStatus::Limited,
        running_processes: Vec::new(),
        requires_app_close: false,
        sources: Vec::new(),
        source_count: 0,
        sources_truncated: false,
        scan_elapsed_ms: elapsed_ms,
    }
}

pub(super) fn execute(
    inventory: &ApplicationInventory,
    declared_roots: &[PathBuf],
    source_scope: Option<&SourceScope>,
    dry_run: bool,
    operation: &OperationGuard,
) -> CleanupActionResult {
    let expected = LAST_PREVIEW
        .get_or_init(|| Mutex::new(None))
        .lock()
        .ok()
        .and_then(|snapshot| snapshot.clone());
    let Some(mut expected) = expected else {
        return failed_action(0, CleanupActionReason::PreflightFailed);
    };
    if let Some(scope) = source_scope {
        if scope
            .validate_known_paths(expected.iter().map(|candidate| candidate.path.as_path()))
            .is_err()
        {
            return failed_action(0, CleanupActionReason::PreflightFailed);
        }
        expected.retain(|candidate| scope.selects(&candidate.path));
    }
    let expected_bytes = expected.iter().map(|candidate| candidate.bytes).sum();
    let processes = match ProcessSnapshot::capture() {
        Ok(processes) => processes,
        Err(error) => {
            log::warn!(
                "additional_user_cache_preflight_failed reason=processSnapshot error_digest={}",
                blake3::hash(error.as_bytes()).to_hex()
            );
            return failed_action(expected_bytes, CleanupActionReason::PreflightFailed);
        }
    };

    let mut released_bytes = 0_u64;
    let mut affected_items = 0_u64;
    let mut failed_items = 0_u64;
    let mut validated_candidates = 0_u64;
    for candidate in &expected {
        if operation.ensure_not_cancelled().is_err() {
            failed_items = failed_items.saturating_add(1);
            break;
        }
        let Ok(prepared) = prepare_path_for_permanent_delete(&candidate.path) else {
            failed_items = failed_items.saturating_add(1);
            continue;
        };
        if revalidate_candidate(candidate, inventory, &processes, declared_roots).is_err() {
            failed_items = failed_items.saturating_add(1);
            continue;
        }
        validated_candidates = validated_candidates.saturating_add(1);
        if dry_run {
            continue;
        }
        match delete_path_permanently(prepared, candidate.bytes, candidate.file_count) {
            Ok(()) => {
                released_bytes = released_bytes.saturating_add(candidate.bytes);
                affected_items = affected_items.saturating_add(candidate.file_count);
            }
            Err(error) => {
                released_bytes = released_bytes.saturating_add(error.released_bytes());
                affected_items = affected_items.saturating_add(error.affected_item_count());
                log::warn!(
                    "additional_user_cache_permanent_delete_failed path={} partial={} released_bytes={} affected_item_count={} error_digest={}",
                    diagnostic_path(&candidate.path),
                    error.is_partial(),
                    error.released_bytes(),
                    error.affected_item_count(),
                    blake3::hash(error.to_string().as_bytes()).to_hex()
                );
                failed_items = failed_items.saturating_add(1);
            }
        }
    }
    if !dry_run && replace_last_preview(None).is_err() {
        log::warn!("additional_user_cache_snapshot_clear_failed");
    }
    let cancelled = operation.ensure_not_cancelled().is_err();
    CleanupActionResult {
        rule_id: CLEANER_ID.to_string(),
        action_kind: CleanupActionKind::Delete,
        status: if cancelled {
            CleanupActionStatus::Blocked
        } else if failed_items == 0 {
            if dry_run {
                CleanupActionStatus::Previewed
            } else {
                CleanupActionStatus::Completed
            }
        } else if validated_candidates > 0 {
            CleanupActionStatus::Partial
        } else {
            CleanupActionStatus::Failed
        },
        reason_code: if cancelled {
            Some(CleanupActionReason::Cancelled)
        } else {
            (failed_items > 0).then_some(CleanupActionReason::ItemsSkipped)
        },
        bytes_expected: expected_bytes,
        released_bytes,
        affected_item_count: affected_items,
        failed_item_count: failed_items,
        running_processes: Vec::new(),
    }
}

fn discover(
    inventory: &ApplicationInventory,
    processes: &ProcessSnapshot,
    declared_roots: &[PathBuf],
    is_cancelled: &(dyn Fn() -> bool + Sync),
    report_path: &(dyn Fn(&Path) + Sync),
    report_files: &(dyn Fn(&Path, u64, u64) + Sync),
) -> Result<Discovery, String> {
    let user_directories = current_platform()
        .user_directories()
        .map_err(|error| error.to_string())?;
    let home = user_directories.home_directory();
    let cache_root = user_directories.cache_directory();
    let mut discovery = discover_in_root(
        cache_root,
        home,
        declared_roots,
        is_cancelled,
        report_path,
        report_files,
        &|name| cache_is_active(name, inventory, processes),
    )?;
    let sandbox_context = SandboxDiscoveryContext {
        inventory,
        processes,
        declared_roots,
        is_cancelled,
        report_path,
        report_files,
    };
    discover_sandbox_caches(home, &sandbox_context, &mut discovery)?;
    discovery.candidates.sort_by(|left, right| {
        right
            .bytes
            .cmp(&left.bytes)
            .then_with(|| left.path.cmp(&right.path))
    });
    Ok(discovery)
}

fn discover_sandbox_caches(
    home: &Path,
    context: &SandboxDiscoveryContext<'_>,
    discovery: &mut Discovery,
) -> Result<(), String> {
    for (container_directory, cache_suffix) in [
        ("Containers", Path::new("Data/Library/Caches")),
        ("Group Containers", Path::new("Library/Caches")),
    ] {
        let container_root = home.join("Library").join(container_directory);
        if !container_root.exists() {
            continue;
        }
        current_platform()
            .validate_path_no_links(&container_root)
            .map_err(|error| error.to_string())?;
        let mut containers = fs::read_dir(&container_root)
            .map_err(|error| format!("failed to enumerate sandbox containers: {error}"))?
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .collect::<Vec<_>>();
        containers.sort();
        for container in containers {
            if (context.is_cancelled)() {
                return Err("sandbox user cache discovery was cancelled".to_string());
            }
            let Some(ownership_key) = container
                .file_name()
                .and_then(|value| value.to_str())
                .map(str::to_owned)
            else {
                discovery.skipped_count = discovery.skipped_count.saturating_add(1);
                continue;
            };
            if OWN_CACHE_DIRECTORIES
                .iter()
                .any(|owned| ownership_key.eq_ignore_ascii_case(owned))
                || cache_is_active(&ownership_key, context.inventory, context.processes)
            {
                discovery.active_count = discovery.active_count.saturating_add(1);
                continue;
            }
            let inspection = CacheInspection {
                boundary_root: &container,
                declared_roots: context.declared_roots,
                report_path: context.report_path,
                report_files: context.report_files,
                home,
            };
            inspect_cache_candidate(
                container.join(cache_suffix),
                ownership_key,
                &inspection,
                discovery,
            );
        }
    }
    Ok(())
}

fn discover_in_root(
    cache_root: &Path,
    home: &Path,
    declared_roots: &[PathBuf],
    is_cancelled: &(dyn Fn() -> bool + Sync),
    report_path: &(dyn Fn(&Path) + Sync),
    report_files: &(dyn Fn(&Path, u64, u64) + Sync),
    is_active: &dyn Fn(&str) -> bool,
) -> Result<Discovery, String> {
    if !cache_root.exists() {
        return Ok(Discovery::default());
    }
    current_platform()
        .validate_path_no_links(cache_root)
        .map_err(|error| error.to_string())?;
    report_path(cache_root);
    let mut paths = fs::read_dir(cache_root)
        .map_err(|error| format!("failed to enumerate the user cache directory: {error}"))?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .collect::<Vec<_>>();
    paths.sort();

    let mut discovery = Discovery::default();
    let inspection = CacheInspection {
        boundary_root: cache_root,
        declared_roots,
        report_path,
        report_files,
        home,
    };
    for path in paths {
        if is_cancelled() {
            return Err("additional user cache discovery was cancelled".to_string());
        }
        let Some(name) = path
            .file_name()
            .and_then(|value| value.to_str())
            .map(str::to_owned)
        else {
            discovery.skipped_count = discovery.skipped_count.saturating_add(1);
            continue;
        };
        if OWN_CACHE_DIRECTORIES
            .iter()
            .any(|owned| name.eq_ignore_ascii_case(owned))
            || overlaps_declared_root(&path, declared_roots)
        {
            continue;
        }
        let Ok(metadata) = fs::symlink_metadata(&path) else {
            discovery.skipped_count = discovery.skipped_count.saturating_add(1);
            continue;
        };
        if !metadata.is_dir()
            || current_platform().is_link_like(&metadata)
            || metadata.uid() != unsafe { libc::geteuid() }
        {
            discovery.skipped_count = discovery.skipped_count.saturating_add(1);
            continue;
        }
        if is_active(&name) {
            discovery.active_count = discovery.active_count.saturating_add(1);
            continue;
        }
        inspect_cache_candidate(path, name, &inspection, &mut discovery);
    }
    discovery.candidates.sort_by(|left, right| {
        right
            .bytes
            .cmp(&left.bytes)
            .then_with(|| left.path.cmp(&right.path))
    });
    Ok(discovery)
}

struct CacheInspection<'a> {
    boundary_root: &'a Path,
    declared_roots: &'a [PathBuf],
    report_path: &'a (dyn Fn(&Path) + Sync),
    report_files: &'a (dyn Fn(&Path, u64, u64) + Sync),
    home: &'a Path,
}

fn inspect_cache_candidate(
    path: PathBuf,
    ownership_key: String,
    inspection: &CacheInspection<'_>,
    discovery: &mut Discovery,
) {
    if !path.exists() || overlaps_declared_root(&path, inspection.declared_roots) {
        return;
    }
    if current_platform().validate_path_no_links(&path).is_err() {
        discovery.skipped_count = discovery.skipped_count.saturating_add(1);
        return;
    }
    if validate_automatic_cleanup_root(&path, inspection.home).is_err() {
        discovery.protected_count = discovery.protected_count.saturating_add(1);
        return;
    }
    let Ok(metadata) = fs::symlink_metadata(&path) else {
        discovery.skipped_count = discovery.skipped_count.saturating_add(1);
        return;
    };
    if !metadata.is_dir()
        || current_platform().is_link_like(&metadata)
        || metadata.uid() != unsafe { libc::geteuid() }
    {
        discovery.skipped_count = discovery.skipped_count.saturating_add(1);
        return;
    }
    (inspection.report_path)(&path);
    let snapshot = snapshot_metadata_tree_with_observer(
        &path,
        inspection.boundary_root,
        ScanPurpose::Cleanup,
        inspection.report_files,
    );
    let Some(fingerprint) = snapshot.fingerprint.map(display_fingerprint) else {
        discovery.skipped_count = discovery.skipped_count.saturating_add(1);
        return;
    };
    if snapshot.skipped_count > 0 || snapshot.bytes == 0 {
        discovery.skipped_count = discovery.skipped_count.saturating_add(1);
        return;
    }
    discovery.candidates.push(CacheCandidate {
        path,
        boundary_root: inspection.boundary_root.to_path_buf(),
        ownership_key,
        bytes: snapshot.bytes,
        file_count: snapshot.file_count,
        modified_at_ms: modified_ms(&metadata),
        fingerprint,
    });
}

fn overlaps_declared_root(candidate: &Path, declared_roots: &[PathBuf]) -> bool {
    declared_roots
        .iter()
        .any(|root| root.starts_with(candidate) || candidate.starts_with(root))
}

fn cache_is_active(
    cache_name: &str,
    inventory: &ApplicationInventory,
    processes: &ProcessSnapshot,
) -> bool {
    let mut names = vec![cache_name.to_string()];
    for application in inventory.installed_applications() {
        if application.name.eq_ignore_ascii_case(cache_name)
            || application
                .identifiers
                .iter()
                .any(|identifier| identifier.eq_ignore_ascii_case(cache_name))
        {
            names.push(application.name.clone());
            names.extend(application.identifiers.clone());
            names.extend(application.executable_paths.iter().filter_map(|path| {
                path.file_stem()
                    .map(|value| value.to_string_lossy().into_owned())
            }));
        }
    }
    names.sort_by_key(|value| value.to_ascii_lowercase());
    names.dedup_by(|left, right| left.eq_ignore_ascii_case(right));
    processes.contains_any(&names)
}

fn revalidate_candidate(
    candidate: &CacheCandidate,
    inventory: &ApplicationInventory,
    processes: &ProcessSnapshot,
    declared_roots: &[PathBuf],
) -> Result<(), String> {
    let user_directories = current_platform()
        .user_directories()
        .map_err(|error| error.to_string())?;
    if !candidate_within_reviewed_boundary(
        candidate,
        user_directories.home_directory(),
        user_directories.cache_directory(),
    ) || overlaps_declared_root(&candidate.path, declared_roots)
    {
        return Err("the cache candidate is outside its reviewed ownership boundary".to_string());
    }
    validate_automatic_cleanup_root(&candidate.path, user_directories.home_directory())?;
    if OWN_CACHE_DIRECTORIES
        .iter()
        .any(|owned| candidate.ownership_key.eq_ignore_ascii_case(owned))
        || cache_is_active(&candidate.ownership_key, inventory, processes)
    {
        return Err("the cache candidate is active or owned by MangoDisk".to_string());
    }
    current_platform()
        .validate_path_no_links(&candidate.path)
        .map_err(|error| error.to_string())?;
    current_platform()
        .validate_cleanup_root(&candidate.path)
        .map_err(|error| error.to_string())?;
    let metadata = fs::symlink_metadata(&candidate.path)
        .map_err(|error| format!("failed to revalidate a user cache: {error}"))?;
    if !metadata.is_dir()
        || current_platform().is_link_like(&metadata)
        || metadata.uid() != unsafe { libc::geteuid() }
        || modified_ms(&metadata) != candidate.modified_at_ms
    {
        return Err("the cache candidate identity changed after scanning".to_string());
    }
    let snapshot = snapshot_metadata_tree(
        &candidate.path,
        &candidate.boundary_root,
        ScanPurpose::Cleanup,
    );
    if snapshot.skipped_count > 0
        || snapshot.bytes != candidate.bytes
        || snapshot.file_count != candidate.file_count
        || snapshot.fingerprint.map(display_fingerprint).as_deref()
            != Some(candidate.fingerprint.as_str())
    {
        return Err("the cache candidate content changed after scanning".to_string());
    }
    Ok(())
}

fn candidate_within_reviewed_boundary(
    candidate: &CacheCandidate,
    home: &Path,
    user_cache_root: &Path,
) -> bool {
    if candidate.boundary_root == user_cache_root {
        return candidate.path.parent() == Some(user_cache_root);
    }

    for (container_directory, cache_suffix) in [
        ("Containers", Path::new("Data/Library/Caches")),
        ("Group Containers", Path::new("Library/Caches")),
    ] {
        let container_root = home.join("Library").join(container_directory);
        if candidate.boundary_root.parent() == Some(container_root.as_path())
            && candidate.path == candidate.boundary_root.join(cache_suffix)
        {
            return true;
        }
    }
    false
}

fn failed_action(expected_bytes: u64, reason: CleanupActionReason) -> CleanupActionResult {
    CleanupActionResult {
        rule_id: CLEANER_ID.to_string(),
        action_kind: CleanupActionKind::Delete,
        status: CleanupActionStatus::Failed,
        reason_code: Some(reason),
        bytes_expected: expected_bytes,
        released_bytes: 0,
        affected_item_count: 0,
        failed_item_count: 1,
        running_processes: Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn fixture_path(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "mangodisk-user-cache-{name}-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        ))
    }

    #[test]
    fn declared_descendants_exclude_their_direct_cache_directory() {
        let cache = PathBuf::from("/Users/example/Library/Caches/Google");
        let roots = vec![cache.join("Chrome")];

        assert!(overlaps_declared_root(&cache, &roots));
        assert!(!overlaps_declared_root(
            Path::new("/Users/example/Library/Caches/Unowned"),
            &roots
        ));
    }

    #[test]
    fn mangodisk_operational_caches_are_never_candidates() {
        assert!(OWN_CACHE_DIRECTORIES
            .iter()
            .any(|name| name.eq_ignore_ascii_case("app.mangodisk.desktop")));
    }

    #[test]
    fn starting_a_new_preview_removes_stale_candidates() {
        let _operation_lock = crate::shared::operation::test_operation_lock();
        let stale = CacheCandidate {
            path: PathBuf::from("/stale/cache"),
            boundary_root: PathBuf::from("/stale"),
            ownership_key: "cache".to_string(),
            bytes: 1,
            file_count: 1,
            modified_at_ms: Some(1),
            fingerprint: "stale".to_string(),
        };
        replace_last_preview(Some(vec![stale]))
            .expect("the stale preview fixture should be stored");
        replace_last_preview(None).expect("a new preview should clear stale state");

        assert!(LAST_PREVIEW
            .get_or_init(|| Mutex::new(None))
            .lock()
            .expect("the preview snapshot should be readable")
            .is_none());
    }

    #[test]
    fn reviewed_boundaries_accept_only_user_and_sandbox_cache_locations() {
        let home = PathBuf::from("/Users/example");
        let direct = CacheCandidate {
            path: home.join("Library/Caches/com.example.app"),
            boundary_root: home.join("Library/Caches"),
            ownership_key: "com.example.app".to_string(),
            bytes: 1,
            file_count: 1,
            modified_at_ms: Some(1),
            fingerprint: "direct".to_string(),
        };
        let sandbox = CacheCandidate {
            path: home.join("Library/Containers/com.example.app/Data/Library/Caches"),
            boundary_root: home.join("Library/Containers/com.example.app"),
            ownership_key: "com.example.app".to_string(),
            bytes: 1,
            file_count: 1,
            modified_at_ms: Some(1),
            fingerprint: "sandbox".to_string(),
        };
        let unrelated = CacheCandidate {
            path: home.join("Library/Containers/com.example.app/Data/Documents"),
            boundary_root: home.join("Library/Containers/com.example.app"),
            ownership_key: "com.example.app".to_string(),
            bytes: 1,
            file_count: 1,
            modified_at_ms: Some(1),
            fingerprint: "documents".to_string(),
        };

        let cache_root = home.join("Library/Caches");
        assert!(candidate_within_reviewed_boundary(
            &direct,
            &home,
            &cache_root
        ));
        assert!(candidate_within_reviewed_boundary(
            &sandbox,
            &home,
            &cache_root
        ));
        assert!(!candidate_within_reviewed_boundary(
            &unrelated,
            &home,
            &cache_root
        ));
    }

    #[test]
    fn discovery_returns_only_complete_unowned_inactive_cache_directories() {
        let root = fixture_path("discovery");
        let available = root.join("Available");
        let covered = root.join("Covered");
        let active = root.join("Active");
        fs::create_dir_all(&available).expect("the available cache should be created");
        fs::create_dir_all(covered.join("Nested")).expect("the covered cache should be created");
        fs::create_dir_all(&active).expect("the active cache should be created");
        fs::create_dir_all(root.join(crate::APPLICATION_IDENTIFIER))
            .expect("the owned cache should be created");
        fs::write(available.join("cache.bin"), b"available")
            .expect("the available cache payload should be written");
        fs::write(covered.join("Nested/cache.bin"), b"covered")
            .expect("the covered cache payload should be written");
        fs::write(active.join("cache.bin"), b"active")
            .expect("the active cache payload should be written");
        fs::write(
            root.join(crate::APPLICATION_IDENTIFIER).join("index.db"),
            b"owned",
        )
        .expect("the owned cache payload should be written");

        let discovery = discover_in_root(
            &root,
            root.parent().expect("the fixture should have a parent"),
            &[covered.join("Nested")],
            &|| false,
            &|_| {},
            &|_, _, _| {},
            &|name| name == "Active",
        )
        .expect("the cache fixture should be discovered");

        assert_eq!(discovery.candidates.len(), 1);
        assert_eq!(discovery.candidates[0].path, available);
        assert_eq!(discovery.active_count, 1);
        fs::remove_dir_all(root).expect("the cache fixture should be removed");
    }

    #[test]
    fn discovery_excludes_file_provider_state_from_cache_inventory() {
        let home = fixture_path("file-provider-protection");
        let cache_root = home.join("Library/Caches");
        let ordinary = cache_root.join("com.example.ordinary");
        let cloudkit = cache_root.join("CloudKit");
        fs::create_dir_all(&ordinary).expect("the ordinary cache should be created");
        fs::create_dir_all(&cloudkit).expect("the CloudKit state should be created");
        fs::write(ordinary.join("cache.bin"), b"ordinary")
            .expect("the ordinary cache payload should be written");
        fs::write(cloudkit.join("state.db"), b"sync-state")
            .expect("the CloudKit state payload should be written");

        let discovery = discover_in_root(
            &cache_root,
            &home,
            &[],
            &|| false,
            &|_| {},
            &|_, _, _| {},
            &|_| false,
        )
        .expect("the cache fixture should be discovered");

        assert_eq!(discovery.candidates.len(), 1);
        assert_eq!(discovery.candidates[0].path, ordinary);
        assert_eq!(discovery.protected_count, 1);
        fs::remove_dir_all(home).expect("the cache fixture should be removed");
    }

    #[test]
    #[ignore = "reads the current user's real cache directory without modifying it"]
    fn real_preview_and_dry_run_preserve_all_reviewed_cache_directories() {
        let _operation_lock = crate::shared::operation::test_operation_lock();
        let context = crate::applications::catalog::ScanContext::capture();
        let declared_roots = crate::cleanup::rules::registry()
            .expect("the rule catalog should compile")
            .into_iter()
            .flat_map(|rule| rule.roots)
            .collect::<Vec<_>>();
        let rule = preview(
            &context.inventory,
            &declared_roots,
            &|| false,
            &|_| {},
            &|_, _, _| {},
        );
        let reviewed = LAST_PREVIEW
            .get_or_init(|| Mutex::new(None))
            .lock()
            .expect("the preview snapshot should be readable")
            .clone()
            .expect("the preview should retain reviewed candidates");
        let operation =
            OperationGuard::start(crate::shared::operation::CoordinatedOperationKind::Cleanup)
                .expect("the dry run should start");
        let action = execute(&context.inventory, &declared_roots, None, true, &operation);
        operation.complete();

        assert_eq!(action.released_bytes, 0);
        assert_ne!(action.status, CleanupActionStatus::Completed);
        assert!(reviewed.iter().all(|candidate| candidate.path.exists()));
        println!(
            "Additional user cache dry run: status={:?}, candidates={}, bytes={}, failed={}",
            action.status,
            reviewed.len(),
            rule.bytes,
            action.failed_item_count
        );
    }
}

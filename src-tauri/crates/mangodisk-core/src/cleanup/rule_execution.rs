use std::{
    cell::RefCell,
    collections::HashSet,
    fs,
    path::{Path, PathBuf},
    time::Instant,
};

use mangodisk_platform::{current_platform, Platform};

use crate::{
    applications::catalog::ProcessSnapshot,
    cleanup::{
        measurement::{measure_path_filtered, MeasureResult},
        rules::{
            matches_rule, root_validation::validate_automatic_cleanup_root, CompiledRule,
            MatcherSpec, ScanPlan,
        },
        source_selection::{cleanup_source_path, SourceScope},
        CleanupActionKind, CleanupActionReason, CleanupActionResult, CleanupActionStatus,
    },
    filesystem::{
        metadata::{diagnostic_path, is_link_like, modified_ms},
        permanent_delete::{
            delete_directory_contents_permanently_with_cancellation,
            delete_directory_tree_permanently_with_cancellation,
            delete_empty_directory_permanently, delete_path_permanently,
            prepare_path_for_permanent_delete, PermanentDeleteError,
        },
    },
    shared::operation::OperationGuard,
};

pub(super) struct RuleExecutionContext<'a> {
    pub(super) ownership_plan: &'a ScanPlan,
    pub(super) process_snapshot: &'a ProcessSnapshot,
    pub(super) source_scope: Option<&'a SourceScope>,
    pub(super) operation: &'a OperationGuard,
    pub(super) dry_run: bool,
}

pub(super) fn execute_rule(
    rule: &CompiledRule,
    rule_index: usize,
    before: Option<MeasureResult>,
    context: &RuleExecutionContext<'_>,
    report_item: &mut dyn FnMut(&Path, &DeleteStats),
) -> CleanupActionResult {
    let measured_bytes = before.as_ref().map_or(0, |measurement| measurement.bytes);
    let running = context
        .process_snapshot
        .matching_processes(&rule.required_stopped_processes);
    if !running.is_empty() {
        let process_list = running.join(",");
        log::warn!(
            "cleanup_rule_blocked rule_id={} running_processes={}",
            rule.id,
            process_list
        );
        return CleanupActionResult {
            rule_id: rule.id.to_string(),
            action_kind: CleanupActionKind::Delete,
            status: CleanupActionStatus::Blocked,
            reason_code: Some(CleanupActionReason::RunningProcesses),
            bytes_expected: measured_bytes,
            released_bytes: 0,
            affected_item_count: 0,
            failed_item_count: 1,
            running_processes: running,
        };
    }
    if context.dry_run {
        // Preview never enters the destructive traversal, so its estimate must
        // come from the read-only measurement completed by the service.
        debug_assert!(before.is_some(), "dry-run cleanup requires a measurement");
        return CleanupActionResult {
            rule_id: rule.id.to_string(),
            action_kind: CleanupActionKind::Delete,
            status: CleanupActionStatus::Previewed,
            reason_code: None,
            bytes_expected: measured_bytes,
            released_bytes: 0,
            affected_item_count: 0,
            failed_item_count: 0,
            running_processes: Vec::new(),
        };
    }

    let mut stats = DeleteStats::default();
    for root in &rule.roots {
        if context.operation.ensure_not_cancelled().is_err() {
            stats.failed_item_count = stats.failed_item_count.saturating_add(1);
            break;
        }
        if !root.exists() {
            continue;
        }
        match validate_rule_root(root, &rule.matcher) {
            Ok(canonical_root) => {
                let handled = rule.deletes_whole_root()
                    && try_delete_whole_root(
                        rule,
                        rule_index,
                        root,
                        context,
                        &mut stats,
                        report_item,
                    );
                if !handled {
                    let bulk_complete_directories = matches!(rule.matcher, MatcherSpec::All)
                        && context.source_scope.is_none()
                        && context
                            .ownership_plan
                            .rule_exclusively_owns_root(rule_index, root);
                    let owns_path = |path: &Path, metadata: &fs::Metadata| {
                        context
                            .ownership_plan
                            .rule_owns_path(rule_index, path, metadata)
                            && context
                                .source_scope
                                .is_none_or(|scope| scope.selects(&cleanup_source_path(root, path)))
                    };
                    let is_cancelled = || context.operation.ensure_not_cancelled().is_err();
                    delete_root_contents_with_progress(
                        root,
                        &canonical_root,
                        &rule.matcher,
                        DeleteRootContentsPolicy {
                            owns_path: &owns_path,
                            is_cancelled: &is_cancelled,
                            bulk_complete_directories,
                        },
                        &mut stats,
                        report_item,
                    );
                }
            }
            Err(error) => {
                let error_digest = blake3::hash(error.as_bytes()).to_hex().to_string();
                log::warn!(
                    "cleanup_root_validation_failed rule_id={} path={} error_digest={}",
                    rule.id,
                    diagnostic_path(root),
                    error_digest
                );
                stats.failed_item_count += 1;
            }
        }
    }
    CleanupActionResult {
        rule_id: rule.id.to_string(),
        action_kind: CleanupActionKind::Delete,
        status: if stats.failed_item_count == 0 {
            CleanupActionStatus::Completed
        } else {
            CleanupActionStatus::Partial
        },
        // Cancellation belongs to this action only when it actually stopped
        // remaining work. A request arriving after the rule completed is
        // observed by the outer loop before the next rule, preventing an
        // contradictory Completed + Cancelled action result.
        reason_code: (stats.failed_item_count > 0).then(|| {
            if context.operation.ensure_not_cancelled().is_err() {
                CleanupActionReason::Cancelled
            } else {
                CleanupActionReason::ItemsSkipped
            }
        }),
        // Whole-rule cleanup discovers and deletes each candidate in one pass.
        // Scoped cleanup retains its preflight estimate because the selected
        // source paths must be proven live before the first mutation.
        bytes_expected: before.map_or(stats.matched_bytes, |measurement| measurement.bytes),
        released_bytes: stats.deleted_bytes,
        affected_item_count: stats.affected_item_count,
        failed_item_count: stats.failed_item_count,
        running_processes: Vec::new(),
    }
}

/// Tries the optimized complete-root deletion and reports whether this call
/// fully handled the root. `false` is a safe, pre-mutation downgrade request;
/// any failure after staging starts is recorded here and never retried through
/// a second deletion strategy.
fn try_delete_whole_root(
    rule: &CompiledRule,
    rule_index: usize,
    root: &Path,
    context: &RuleExecutionContext<'_>,
    stats: &mut DeleteStats,
    report_item: &mut dyn FnMut(&Path, &DeleteStats),
) -> bool {
    let fallback_reason = if context.source_scope.is_some() {
        Some("source_scope")
    } else if !context
        .ownership_plan
        .rule_exclusively_owns_root(rule_index, root)
    {
        Some("nested_ownership")
    } else {
        None
    };
    if let Some(reason) = fallback_reason {
        log::info!(
            "cleanup_whole_root_fallback rule_id={} reason={}",
            rule.id,
            reason
        );
        return false;
    }

    // Capture the physical root identity once. The permanent deletion boundary
    // checks the same identity after its atomic rename, so a concurrent path
    // replacement cannot redirect the recursive removal.
    let prepared = match prepare_path_for_permanent_delete(root) {
        Ok(prepared) if prepared.metadata().is_dir() => prepared,
        Ok(_) => {
            stats.failed_item_count = stats.failed_item_count.saturating_add(1);
            return true;
        }
        Err(error) => {
            log::warn!(
                "cleanup_whole_root_prepare_failed rule_id={} error_digest={}",
                rule.id,
                blake3::hash(error.to_string().as_bytes()).to_hex()
            );
            stats.failed_item_count = stats.failed_item_count.saturating_add(1);
            return true;
        }
    };
    let started = Instant::now();
    let is_cancelled = || context.operation.ensure_not_cancelled().is_err();
    if context.operation.ensure_not_cancelled().is_err() {
        stats.failed_item_count = stats.failed_item_count.saturating_add(1);
        return true;
    }

    match delete_directory_tree_permanently_with_cancellation(prepared, 0, 0, &is_cancelled) {
        Ok(outcome) => {
            stats.matched_bytes = stats.matched_bytes.saturating_add(outcome.released_bytes());
            stats.deleted_bytes = stats.deleted_bytes.saturating_add(outcome.released_bytes());
            stats.affected_item_count = stats
                .affected_item_count
                .saturating_add(outcome.affected_item_count());
            report_item(root, stats);
            log::info!(
                "cleanup_whole_root_completed rule_id={} affected_item_count={} released_bytes={} elapsed_ms={}",
                rule.id,
                outcome.affected_item_count(),
                outcome.released_bytes(),
                started.elapsed().as_millis()
            );
        }
        Err(error) => {
            record_bulk_delete_error(root, &error, &is_cancelled, stats);
            report_item(root, stats);
            log::warn!(
                "cleanup_whole_root_delete_failed rule_id={} released_bytes={} affected_item_count={} error_digest={}",
                rule.id,
                error.released_bytes(),
                error.affected_item_count(),
                blake3::hash(error.to_string().as_bytes()).to_hex()
            );
        }
    }
    true
}

pub(super) fn cancelled_action(
    rule_id: &str,
    action_kind: CleanupActionKind,
    bytes_expected: u64,
) -> CleanupActionResult {
    CleanupActionResult {
        rule_id: rule_id.to_string(),
        action_kind,
        status: CleanupActionStatus::Blocked,
        reason_code: Some(CleanupActionReason::Cancelled),
        bytes_expected,
        released_bytes: 0,
        affected_item_count: 0,
        failed_item_count: 1,
        running_processes: Vec::new(),
    }
}

pub(super) fn measure_owned_rule(
    plan: &ScanPlan,
    rule_index: usize,
    source_scope: Option<&SourceScope>,
) -> Result<MeasureResult, String> {
    let rule = &plan.rules[rule_index];
    let known_sources = RefCell::new(HashSet::<PathBuf>::new());
    let total = rule
        .roots
        .iter()
        .fold(MeasureResult::default(), |mut total, root| {
            let result = measure_path_filtered(root, Some(&rule.matcher), &|path, metadata| {
                if !plan.rule_owns_path(rule_index, path, metadata) {
                    return false;
                }
                let source = cleanup_source_path(root, path);
                known_sources.borrow_mut().insert(source.clone());
                source_scope.is_none_or(|scope| scope.selects(&source))
            });
            total.bytes = total.bytes.saturating_add(result.bytes);
            total.file_count = total.file_count.saturating_add(result.file_count);
            total.skipped_count = total.skipped_count.saturating_add(result.skipped_count);
            total
        });
    if let Some(scope) = source_scope {
        scope.validate_known_paths(known_sources.borrow().iter().map(PathBuf::as_path))?;
    }
    Ok(total)
}

/// Revalidates a declared cleanup root against the live filesystem.
///
/// Symbolic links and Windows reparse points cannot become roots because their
/// targets could cross into user data or another volume.
pub(super) fn validate_rule_root(root: &Path, matcher: &MatcherSpec) -> Result<PathBuf, String> {
    current_platform()
        .validate_path_no_links(root)
        .map_err(|error| error.to_string())?;
    let metadata = fs::symlink_metadata(root).map_err(|error| error.to_string())?;
    if !metadata.is_dir() {
        return Err("the cleanup rule root must be a directory".to_string());
    }
    let canonical = current_platform()
        .canonicalize_no_links(root)
        .map_err(|error| error.to_string())?;
    if let Err(error) = current_platform().validate_cleanup_root(&canonical) {
        if !is_narrow_stale_download_root(&canonical, matcher)? {
            return Err(error.to_string());
        }
    }
    let home = current_platform()
        .user_directories()
        .map_err(|error| error.to_string())?
        .home_directory()
        .to_path_buf();
    validate_automatic_cleanup_root(&canonical, &home)?;
    Ok(canonical)
}

fn is_narrow_stale_download_root(
    canonical_root: &Path,
    matcher: &MatcherSpec,
) -> Result<bool, String> {
    // Downloads is personal content and cannot be a general cleanup root. Stale
    // partial downloads are the only narrow exception: the root must exactly
    // match the current user's Downloads directory and the matcher must require
    // both a seven-day age gate and the complete temporary-extension allowlist.
    let downloads = current_platform()
        .user_directories()
        .map_err(|error| error.to_string())?
        .home_directory()
        .join("Downloads");
    let Ok(canonical_downloads) = fs::canonicalize(downloads) else {
        return Ok(false);
    };
    if canonical_root != canonical_downloads {
        return Ok(false);
    }
    let MatcherSpec::AllOf(items) = matcher else {
        return Ok(false);
    };
    let has_age_gate = items
        .iter()
        .any(|item| matches!(item, MatcherSpec::OlderThanDays(days) if *days >= 7));
    let allowed_extensions = ["crdownload", "download", "partial", "part"];
    let has_strict_extension_gate = items.iter().any(|item| {
        let MatcherSpec::ExtensionIn(values) = item else {
            return false;
        };
        !values.is_empty()
            && values.iter().all(|value| {
                allowed_extensions
                    .iter()
                    .any(|allowed| value.trim_start_matches('.').eq_ignore_ascii_case(allowed))
            })
    });
    Ok(has_age_gate && has_strict_extension_gate)
}

#[cfg(test)]
pub(super) fn delete_root_contents(
    root: &Path,
    canonical_root: &Path,
    matcher: &MatcherSpec,
    owns_path: &dyn Fn(&Path, &fs::Metadata) -> bool,
    is_cancelled: &(dyn Fn() -> bool + Sync),
    stats: &mut DeleteStats,
) {
    delete_root_contents_with_progress(
        root,
        canonical_root,
        matcher,
        DeleteRootContentsPolicy {
            owns_path,
            is_cancelled,
            bulk_complete_directories: false,
        },
        stats,
        &mut |_, _| {},
    );
}

pub(super) struct DeleteRootContentsPolicy<'a> {
    pub(super) owns_path: &'a dyn Fn(&Path, &fs::Metadata) -> bool,
    pub(super) is_cancelled: &'a (dyn Fn() -> bool + Sync),
    pub(super) bulk_complete_directories: bool,
}

pub(super) fn delete_root_contents_with_progress(
    root: &Path,
    canonical_root: &Path,
    matcher: &MatcherSpec,
    policy: DeleteRootContentsPolicy<'_>,
    stats: &mut DeleteStats,
    report_item: &mut dyn FnMut(&Path, &DeleteStats),
) {
    if (policy.is_cancelled)() {
        stats.failed_item_count += 1;
        return;
    }
    if validate_cleanup_directory(root, canonical_root).is_err() {
        stats.failed_item_count += 1;
        return;
    }
    let Ok(entries) = fs::read_dir(root) else {
        stats.failed_item_count += 1;
        return;
    };
    let mut traversal = DeleteTraversalContext {
        canonical_root,
        matcher,
        owns_path: policy.owns_path,
        is_cancelled: policy.is_cancelled,
        bulk_complete_directories: policy.bulk_complete_directories,
        report_item,
    };
    for entry in entries {
        if (policy.is_cancelled)() {
            stats.failed_item_count += 1;
            break;
        }
        let Ok(entry) = entry else {
            stats.failed_item_count += 1;
            continue;
        };
        // Revalidate the root before each child. If another process replaces it
        // with a symlink or junction during cleanup, traversal stops immediately.
        if !revalidate_cleanup_directory(root, canonical_root) {
            stats.failed_item_count += 1;
            break;
        }
        delete_entry(&entry.path(), canonical_root, stats, &mut traversal);
    }
}

struct DeleteTraversalContext<'a> {
    canonical_root: &'a Path,
    matcher: &'a MatcherSpec,
    owns_path: &'a dyn Fn(&Path, &fs::Metadata) -> bool,
    is_cancelled: &'a (dyn Fn() -> bool + Sync),
    bulk_complete_directories: bool,
    report_item: &'a mut dyn FnMut(&Path, &DeleteStats),
}

fn delete_entry(
    path: &Path,
    canonical_parent: &Path,
    stats: &mut DeleteStats,
    traversal: &mut DeleteTraversalContext<'_>,
) -> bool {
    if (traversal.is_cancelled)() {
        stats.failed_item_count += 1;
        return false;
    }
    if !path
        .parent()
        .is_some_and(|parent| revalidate_cleanup_directory(parent, canonical_parent))
    {
        stats.failed_item_count += 1;
        return false;
    }
    let Ok(initial_metadata) = fs::symlink_metadata(path) else {
        stats.failed_item_count += 1;
        return false;
    };
    if is_link_like(&initial_metadata) {
        // Links inside durable subtrees are expected in mixed-purpose roots.
        // They become failures only when the matcher attempted to select the
        // link itself; otherwise no deletion authority exists for that entry.
        if matches_rule(
            traversal.canonical_root,
            path,
            &initial_metadata,
            Some(traversal.matcher),
        ) && (traversal.owns_path)(path, &initial_metadata)
        {
            stats.failed_item_count += 1;
        }
        return false;
    }
    let Ok(prepared) = prepare_path_for_permanent_delete(path) else {
        stats.failed_item_count += 1;
        return false;
    };
    let metadata = prepared.metadata();
    if is_link_like(metadata) {
        stats.failed_item_count += 1;
        return false;
    }
    if metadata.is_file() {
        if !matches_rule(
            traversal.canonical_root,
            path,
            metadata,
            Some(traversal.matcher),
        ) || !(traversal.owns_path)(path, metadata)
        {
            return false;
        }
        // Record the live candidate before the final identity check and delete
        // attempt. This preserves truthful expected-byte reporting for partial
        // failures without requiring a separate measurement traversal.
        stats.matched_bytes = stats.matched_bytes.saturating_add(metadata.len());
        if !path
            .parent()
            .is_some_and(|parent| revalidate_cleanup_directory(parent, canonical_parent))
        {
            stats.failed_item_count += 1;
            return false;
        }
        let Ok(verified) = fs::symlink_metadata(path) else {
            stats.failed_item_count += 1;
            return false;
        };
        if is_link_like(&verified)
            || !verified.is_file()
            || verified.len() != metadata.len()
            || modified_ms(&verified) != modified_ms(metadata)
        {
            stats.failed_item_count += 1;
            return false;
        }
        let released_bytes = metadata.len();
        let removed = match delete_path_permanently(prepared, released_bytes, 1) {
            Ok(()) => {
                // Metadata was captured immediately before deletion, so released
                // bytes can be accumulated without a second full directory walk.
                stats.deleted_bytes = stats.deleted_bytes.saturating_add(released_bytes);
                stats.affected_item_count += 1;
                true
            }
            Err(error) => {
                stats.deleted_bytes = stats.deleted_bytes.saturating_add(error.released_bytes());
                stats.affected_item_count = stats
                    .affected_item_count
                    .saturating_add(error.affected_item_count());
                stats.failed_item_count += 1;
                false
            }
        };
        (traversal.report_item)(path, stats);
        return removed;
    }

    let prepared = if traversal.bulk_complete_directories
        && canonical_parent == traversal.canonical_root
        && (traversal.owns_path)(path, metadata)
    {
        match delete_directory_contents_permanently_with_cancellation(
            prepared,
            traversal.is_cancelled,
        ) {
            Ok(outcome) => {
                stats.matched_bytes = stats.matched_bytes.saturating_add(outcome.released_bytes());
                stats.deleted_bytes = stats.deleted_bytes.saturating_add(outcome.released_bytes());
                stats.affected_item_count = stats
                    .affected_item_count
                    .saturating_add(outcome.affected_item_count());
                if outcome.affected_item_count() > 0 || outcome.released_bytes() > 0 {
                    (traversal.report_item)(path, stats);
                }
                return !path.exists();
            }
            Err(error) if !error.is_partial() && !(traversal.is_cancelled)() => {
                log::info!(
                    "cleanup_complete_directory_fallback path={} error_digest={}",
                    diagnostic_path(path),
                    blake3::hash(error.to_string().as_bytes()).to_hex()
                );
                let Ok(prepared) = prepare_path_for_permanent_delete(path) else {
                    stats.failed_item_count = stats.failed_item_count.saturating_add(1);
                    return false;
                };
                prepared
            }
            Err(error) => {
                record_bulk_delete_error(path, &error, traversal.is_cancelled, stats);
                (traversal.report_item)(path, stats);
                return false;
            }
        }
    } else {
        prepared
    };

    let canonical_directory = match validate_cleanup_directory(path, traversal.canonical_root) {
        Ok(path) => path,
        Err(error) => {
            log::debug!(
                "cleanup_directory_validation_failed path={} error={}",
                diagnostic_path(path),
                error
            );
            stats.failed_item_count += 1;
            return false;
        }
    };
    if canonical_directory.parent() != Some(canonical_parent) {
        stats.failed_item_count += 1;
        return false;
    }
    let Ok(entries) = fs::read_dir(path) else {
        stats.failed_item_count += 1;
        return false;
    };
    if !revalidate_cleanup_directory(path, &canonical_directory) {
        stats.failed_item_count += 1;
        return false;
    }
    let mut all_removed = true;
    let mut had_entry = false;
    for entry in entries {
        if (traversal.is_cancelled)() {
            stats.failed_item_count += 1;
            return false;
        }
        had_entry = true;
        let Ok(entry) = entry else {
            stats.failed_item_count += 1;
            all_removed = false;
            continue;
        };
        if !revalidate_cleanup_directory(path, &canonical_directory) {
            stats.failed_item_count += 1;
            return false;
        }
        if !delete_entry(&entry.path(), &canonical_directory, stats, traversal) {
            all_removed = false;
        }
    }
    // A matcher authorizes only matching files. Removing a directory that was
    // already empty would expand scope through vacuous success. Prune a
    // directory only when it contained entries and this operation removed all
    // of them.
    if had_entry
        && all_removed
        && (!revalidate_cleanup_directory(path, &canonical_directory)
            || delete_empty_directory_permanently(prepared).is_err())
    {
        stats.failed_item_count = stats.failed_item_count.saturating_add(1);
        all_removed = false;
    }
    had_entry && all_removed
}

fn record_bulk_delete_error(
    restored_path: &Path,
    error: &PermanentDeleteError,
    is_cancelled: &(dyn Fn() -> bool + Sync),
    stats: &mut DeleteStats,
) {
    // Cancellation must remain responsive; only failure paths perform the
    // additional traversal needed to account for a restored remainder.
    let remaining = if error.remaining_was_restored() && !is_cancelled() {
        measure_path_filtered(restored_path, Some(&MatcherSpec::All), &|_, _| true)
    } else {
        MeasureResult::default()
    };
    if remaining.skipped_count > 0 {
        log::warn!(
            "cleanup_partial_remainder_measurement_incomplete path={} skipped_count={}",
            diagnostic_path(restored_path),
            remaining.skipped_count
        );
    }
    stats.matched_bytes = stats
        .matched_bytes
        .saturating_add(error.released_bytes())
        .saturating_add(remaining.bytes);
    stats.deleted_bytes = stats.deleted_bytes.saturating_add(error.released_bytes());
    stats.affected_item_count = stats
        .affected_item_count
        .saturating_add(error.affected_item_count());
    stats.failed_item_count = stats.failed_item_count.saturating_add(1);
}

fn validate_cleanup_directory(path: &Path, canonical_root: &Path) -> Result<PathBuf, String> {
    current_platform()
        .validate_path_no_links(path)
        .map_err(|error| error.to_string())?;
    let metadata = fs::symlink_metadata(path).map_err(|error| error.to_string())?;
    if !metadata.is_dir() || is_link_like(&metadata) {
        return Err("the cleanup path is no longer a regular directory".to_string());
    }
    let canonical = current_platform()
        .canonicalize_no_links(path)
        .map_err(|error| error.to_string())?;
    if !current_platform().path_is_same_or_child(&canonical, canonical_root) {
        return Err("the cleanup path escaped the rule root".to_string());
    }
    Ok(canonical)
}

fn revalidate_cleanup_directory(path: &Path, expected_canonical: &Path) -> bool {
    if current_platform().validate_path_no_links(path).is_err() {
        return false;
    }
    let Ok(metadata) = fs::symlink_metadata(path) else {
        return false;
    };
    metadata.is_dir()
        && !is_link_like(&metadata)
        && fs::canonicalize(path)
            .is_ok_and(|canonical| current_platform().paths_equal(&canonical, expected_canonical))
}

#[derive(Default)]
pub(super) struct DeleteStats {
    pub(super) matched_bytes: u64,
    pub(super) deleted_bytes: u64,
    pub(super) affected_item_count: u64,
    pub(super) failed_item_count: u64,
}

#[cfg(test)]
mod bulk_cleanup_tests {
    use std::{
        path::PathBuf,
        sync::atomic::{AtomicU64, Ordering},
    };

    use super::*;
    use crate::filesystem::permanent_delete::physical_path_identity_snapshot;

    struct TestDirectory(PathBuf);

    impl TestDirectory {
        fn new(name: &str) -> Self {
            let unique = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos();
            let path = std::env::temp_dir()
                .join(format!("mangodisk-{name}-{}-{unique}", std::process::id()));
            fs::create_dir_all(&path).expect("create the cleanup test directory");
            Self(path)
        }
    }

    impl Drop for TestDirectory {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn bulk_contents_cleanup_preserves_preexisting_empty_directories() {
        let sandbox = TestDirectory::new("bulk-empty-directory-test");
        let target = sandbox.0.join("cache");
        let generated = target.join("generated");
        let empty = target.join("empty/nested");
        fs::create_dir_all(&generated).expect("create the generated cache directory");
        fs::create_dir_all(&empty).expect("create the empty cache directory");
        let file_count = 1_024_u64;
        for index in 0..file_count {
            fs::write(generated.join(format!("{index:04}.cache")), b"cache")
                .expect("write a generated cache file");
        }
        let target_identity =
            physical_path_identity_snapshot(&target).expect("capture the retained cache identity");
        let empty_identity =
            physical_path_identity_snapshot(&empty).expect("capture the empty directory identity");
        let prepared = prepare_path_for_permanent_delete(&target)
            .expect("capture the cache directory identity");

        let outcome = delete_directory_contents_permanently_with_cancellation(prepared, &|| false)
            .expect("delete generated files while retaining empty directories");

        assert_eq!(outcome.released_bytes(), file_count * 5);
        assert_eq!(outcome.affected_item_count(), file_count);
        assert!(!generated.exists());
        assert_eq!(
            physical_path_identity_snapshot(&target).expect("read the retained cache identity"),
            target_identity
        );
        assert_eq!(
            physical_path_identity_snapshot(&empty).expect("read the retained empty identity"),
            empty_identity
        );
    }

    #[test]
    fn restored_partial_cleanup_counts_deleted_and_remaining_bytes() {
        let sandbox = TestDirectory::new("bulk-partial-accounting-test");
        let target = sandbox.0.join("cache");
        fs::create_dir_all(&target).expect("create the partial cleanup directory");
        let file_count = 1_024_u64;
        for index in 0..file_count {
            fs::write(target.join(format!("{index:02}.cache")), b"cache")
                .expect("write the partial cleanup fixture");
        }
        let prepared = prepare_path_for_permanent_delete(&target)
            .expect("capture the partial cleanup directory");
        let checks = AtomicU64::new(0);
        let error = delete_directory_contents_permanently_with_cancellation(prepared, &|| {
            checks.fetch_add(1, Ordering::Relaxed) >= 256
        })
        .expect_err("cancellation must stop the staged contents cleanup");
        assert!(error.is_partial());
        assert!(error.remaining_was_restored());

        let mut stats = DeleteStats::default();
        record_bulk_delete_error(&target, &error, &|| false, &mut stats);

        assert_eq!(stats.matched_bytes, file_count * 5);
        assert!(stats.deleted_bytes > 0);
        assert!(stats.deleted_bytes < stats.matched_bytes);
        assert_eq!(stats.failed_item_count, 1);
    }

    #[cfg(unix)]
    #[test]
    fn cleanup_fifo_returns_within_the_cancellation_deadline() {
        use std::{
            ffi::CString,
            os::unix::{ffi::OsStrExt, fs::FileTypeExt},
            sync::{
                atomic::{AtomicBool, Ordering},
                mpsc, Arc,
            },
            thread,
            time::Duration,
        };

        let sandbox = TestDirectory::new("fifo-cancellation-test");
        let fifo = sandbox.0.join("stale.pipe");
        let native_path = CString::new(fifo.as_os_str().as_bytes())
            .expect("the FIFO fixture path should not contain a null byte");
        // SAFETY: `native_path` is a valid, null-terminated pathname and the
        // fixture is confined to the test sandbox.
        let result = unsafe { libc::mkfifo(native_path.as_ptr(), 0o600) };
        assert_eq!(
            result,
            0,
            "the FIFO fixture should be created: {}",
            std::io::Error::last_os_error()
        );

        let root = sandbox.0.clone();
        let canonical_root = fs::canonicalize(&root).expect("canonicalize the cleanup fixture");
        let cancelled = Arc::new(AtomicBool::new(false));
        let worker_cancellation = Arc::clone(&cancelled);
        let (sender, receiver) = mpsc::channel();
        thread::spawn(move || {
            let mut stats = DeleteStats::default();
            delete_root_contents(
                &root,
                &canonical_root,
                &MatcherSpec::All,
                &|_, _| true,
                &|| worker_cancellation.load(Ordering::Acquire),
                &mut stats,
            );
            let _ = sender.send((
                stats.deleted_bytes,
                stats.affected_item_count,
                stats.failed_item_count,
            ));
        });
        thread::sleep(Duration::from_millis(50));
        cancelled.store(true, Ordering::Release);

        let (deleted_bytes, affected_item_count, failed_item_count) = receiver
            .recv_timeout(Duration::from_secs(1))
            .expect("cleanup must return after cancellation instead of blocking on a FIFO");
        assert_eq!(deleted_bytes, 0);
        assert_eq!(affected_item_count, 0);
        assert!(failed_item_count > 0);
        let metadata = fs::symlink_metadata(&fifo).expect("the FIFO fixture should remain");
        assert!(metadata.file_type().is_fifo());
    }

    #[cfg(unix)]
    #[test]
    fn narrow_cleanup_ignores_unmatched_links_and_rejects_matched_links() {
        use std::os::unix::fs::symlink;

        let sandbox = TestDirectory::new("unmatched-link-test");
        let root = sandbox.0.join("versions");
        let active_version = root.join("current");
        fs::create_dir_all(&active_version).expect("create the retained version directory");
        fs::write(active_version.join("runtime.bin"), b"runtime")
            .expect("write the retained runtime fixture");
        symlink("runtime.bin", active_version.join("runtime-link"))
            .expect("create the retained runtime link");
        symlink("current/runtime.bin", root.join("pending.zip"))
            .expect("create the matching archive link");
        let archive = root.join("update.zip");
        fs::write(&archive, b"archive").expect("write the matching archive fixture");

        let canonical_root = fs::canonicalize(&root).expect("canonicalize the cleanup fixture");
        let mut stats = DeleteStats::default();
        delete_root_contents(
            &root,
            &canonical_root,
            &MatcherSpec::NameGlob(vec!["*.zip".to_string()]),
            &|_, _| true,
            &|| false,
            &mut stats,
        );

        assert!(!archive.exists());
        assert!(active_version.join("runtime.bin").exists());
        assert!(active_version.join("runtime-link").exists());
        assert!(root.join("pending.zip").exists());
        assert_eq!(stats.deleted_bytes, 7);
        assert_eq!(stats.affected_item_count, 1);
        assert_eq!(stats.failed_item_count, 1);
    }
}

use std::{
    fs,
    path::{Path, PathBuf},
    time::{Duration, Instant, SystemTime},
};

use mangodisk_platform::{current_platform, Platform};

use crate::{
    cleanup::{
        source_selection::SourceScope, CleanupActionKind, CleanupActionReason, CleanupActionResult,
        CleanupActionStatus, CleanupCategory, CleanupGroup, CleanupSourceDetail, RiskLevel,
        ScanItemStatus, ScanRuleResult,
    },
    filesystem::{
        metadata::{display_path, is_link_like, modified_ms},
        permanent_delete::{delete_path_permanently, prepare_path_for_permanent_delete},
    },
    shared::operation::OperationGuard,
};

pub(super) const CLEANER_ID: &str = "special.codex-archived-sessions";
pub(super) const CLEANER_REVISION: &str = "codex-archived-sessions-v1-30-day-direct-jsonl";

const RETENTION_AGE: Duration = Duration::from_secs(30 * 24 * 60 * 60);
const MAX_PREVIEW_SOURCES: usize = 256;

#[derive(Debug, Clone, PartialEq, Eq)]
struct ArchivedSessionCandidate {
    path: PathBuf,
    bytes: u64,
    modified_at_ms: Option<u64>,
}

pub(super) fn preview(
    is_cancelled: &(dyn Fn() -> bool + Sync),
    report_path: &(dyn Fn(&Path) + Sync),
) -> ScanRuleResult {
    let started = Instant::now();
    let Some(root) = archive_root() else {
        return unavailable_rule(
            ScanItemStatus::Limited,
            started.elapsed().as_millis() as u64,
        );
    };
    if !root.exists() {
        return unavailable_rule(
            ScanItemStatus::NotApplicable,
            started.elapsed().as_millis() as u64,
        );
    }
    match discover_candidates(&root, SystemTime::now(), is_cancelled, report_path) {
        Ok(candidates) => candidate_rule(candidates, started.elapsed().as_millis() as u64),
        Err(DiscoverError::Cancelled) => unavailable_rule(
            ScanItemStatus::Limited,
            started.elapsed().as_millis() as u64,
        ),
        Err(DiscoverError::Incomplete) => {
            log::warn!("codex_archived_sessions_preview_limited reason=incompleteDiscovery");
            unavailable_rule(
                ScanItemStatus::Limited,
                started.elapsed().as_millis() as u64,
            )
        }
    }
}

pub(super) fn limited_rule() -> ScanRuleResult {
    unavailable_rule(ScanItemStatus::Limited, 0)
}

pub(super) fn execute(
    scope: Option<&SourceScope>,
    dry_run: bool,
    operation: &OperationGuard,
) -> CleanupActionResult {
    let Some(root) = archive_root() else {
        return failed_action(0, CleanupActionReason::PreflightFailed);
    };
    let cancelled = || {
        operation
            .cancelled()
            .load(std::sync::atomic::Ordering::Relaxed)
    };
    let candidates = match discover_candidates(&root, SystemTime::now(), &cancelled, &|_| {}) {
        Ok(candidates) => candidates,
        Err(DiscoverError::Cancelled) => {
            return failed_action(0, CleanupActionReason::Cancelled);
        }
        Err(DiscoverError::Incomplete) => {
            return failed_action(0, CleanupActionReason::PreflightFailed);
        }
    };
    if let Some(scope) = scope {
        if scope
            .validate_known_paths(candidates.iter().map(|candidate| candidate.path.as_path()))
            .is_err()
        {
            return failed_action(0, CleanupActionReason::PreflightFailed);
        }
    }
    let selected = candidates
        .into_iter()
        .filter(|candidate| scope.is_none_or(|scope| scope.selects(&candidate.path)))
        .collect::<Vec<_>>();
    let expected_bytes = selected.iter().map(|candidate| candidate.bytes).sum();
    if dry_run {
        return completed_action(
            CleanupActionStatus::Previewed,
            false,
            expected_bytes,
            0,
            selected.len() as u64,
            0,
        );
    }

    let mut released_bytes = 0_u64;
    let mut affected_item_count = 0_u64;
    let mut failed_item_count = 0_u64;
    let mut was_cancelled = false;
    for candidate in selected {
        if operation.ensure_not_cancelled().is_err() {
            was_cancelled = true;
            break;
        }
        let prepared = match prepare_path_for_permanent_delete(&candidate.path) {
            Ok(prepared) if prepared.metadata().is_file() => prepared,
            _ => {
                failed_item_count += 1;
                continue;
            }
        };
        if prepared.metadata().len() != candidate.bytes
            || modified_ms(prepared.metadata()) != candidate.modified_at_ms
        {
            failed_item_count += 1;
            continue;
        }
        match delete_path_permanently(prepared, candidate.bytes, 1) {
            Ok(()) => {
                released_bytes = released_bytes.saturating_add(candidate.bytes);
                affected_item_count += 1;
            }
            Err(error) => {
                released_bytes = released_bytes.saturating_add(error.released_bytes());
                affected_item_count =
                    affected_item_count.saturating_add(error.affected_item_count());
                failed_item_count += 1;
            }
        }
    }
    let status = if was_cancelled && affected_item_count == 0 {
        CleanupActionStatus::Blocked
    } else if was_cancelled {
        CleanupActionStatus::Partial
    } else if failed_item_count == 0 {
        CleanupActionStatus::Completed
    } else if affected_item_count > 0 {
        CleanupActionStatus::Partial
    } else {
        CleanupActionStatus::Failed
    };
    log::info!(
        "codex_archived_sessions_clean_finished expected_bytes={expected_bytes} released_bytes={released_bytes} affected_items={affected_item_count} failed_items={failed_item_count} cancelled={was_cancelled}"
    );
    completed_action(
        status,
        was_cancelled,
        expected_bytes,
        released_bytes,
        affected_item_count,
        failed_item_count,
    )
}

fn archive_root() -> Option<PathBuf> {
    current_platform()
        .user_directories()
        .ok()
        .map(|directories| {
            directories
                .home_directory()
                .join(".codex/archived_sessions")
        })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DiscoverError {
    Cancelled,
    Incomplete,
}

fn discover_candidates(
    root: &Path,
    now: SystemTime,
    is_cancelled: &(dyn Fn() -> bool + Sync),
    report_path: &(dyn Fn(&Path) + Sync),
) -> Result<Vec<ArchivedSessionCandidate>, DiscoverError> {
    // The archive root is derived from the user's home directory, but it can
    // still be replaced by a link outside MangoDisk. Refuse that redirection
    // so a cleanup preview and its later execution keep the same boundary.
    let root_metadata = fs::symlink_metadata(root).map_err(|_| DiscoverError::Incomplete)?;
    if is_link_like(&root_metadata) || !root_metadata.is_dir() {
        return Err(DiscoverError::Incomplete);
    }
    let cutoff = now
        .checked_sub(RETENTION_AGE)
        .ok_or(DiscoverError::Incomplete)?;
    let entries = fs::read_dir(root).map_err(|_| DiscoverError::Incomplete)?;
    let mut candidates = Vec::new();
    let mut incomplete = false;
    for entry in entries {
        if is_cancelled() {
            return Err(DiscoverError::Cancelled);
        }
        let Ok(entry) = entry else {
            incomplete = true;
            continue;
        };
        let path = entry.path();
        report_path(&path);
        if !path
            .extension()
            .is_some_and(|extension| extension.eq_ignore_ascii_case("jsonl"))
        {
            continue;
        }
        let Ok(metadata) = fs::symlink_metadata(&path) else {
            incomplete = true;
            continue;
        };
        if is_link_like(&metadata) || !metadata.is_file() {
            continue;
        }
        let Ok(modified) = metadata.modified() else {
            incomplete = true;
            continue;
        };
        if modified > cutoff {
            continue;
        }
        candidates.push(ArchivedSessionCandidate {
            path,
            bytes: metadata.len(),
            modified_at_ms: modified_ms(&metadata),
        });
    }
    if incomplete {
        return Err(DiscoverError::Incomplete);
    }
    candidates.sort_by(|left, right| {
        right
            .bytes
            .cmp(&left.bytes)
            .then_with(|| left.path.cmp(&right.path))
    });
    Ok(candidates)
}

fn candidate_rule(candidates: Vec<ArchivedSessionCandidate>, elapsed_ms: u64) -> ScanRuleResult {
    let bytes = candidates.iter().map(|candidate| candidate.bytes).sum();
    let source_count = candidates.len() as u64;
    let sources = candidates
        .iter()
        .take(MAX_PREVIEW_SOURCES)
        .map(|candidate| CleanupSourceDetail {
            path: display_path(&candidate.path),
            bytes: candidate.bytes,
            file_count: 1,
            modified_at_ms: candidate.modified_at_ms,
            block_reason: None,
        })
        .collect::<Vec<_>>();
    ScanRuleResult {
        rule_id: CLEANER_ID.to_string(),
        category: CleanupCategory::Development,
        group: CleanupGroup::Development,
        risk: RiskLevel::Recoverable,
        default_selected: false,
        recommended_selected: false,
        bytes,
        file_count: source_count,
        available: true,
        selectable: !candidates.is_empty(),
        status: if candidates.is_empty() {
            ScanItemStatus::Clean
        } else {
            ScanItemStatus::Found
        },
        running_processes: Vec::new(),
        requires_app_close: false,
        sources,
        source_count,
        sources_truncated: source_count > MAX_PREVIEW_SOURCES as u64,
        scan_elapsed_ms: elapsed_ms,
    }
}

fn unavailable_rule(status: ScanItemStatus, elapsed_ms: u64) -> ScanRuleResult {
    ScanRuleResult {
        rule_id: CLEANER_ID.to_string(),
        category: CleanupCategory::Development,
        group: CleanupGroup::Development,
        risk: RiskLevel::Recoverable,
        default_selected: false,
        recommended_selected: false,
        bytes: 0,
        file_count: 0,
        available: status != ScanItemStatus::NotApplicable,
        selectable: false,
        status,
        running_processes: Vec::new(),
        requires_app_close: false,
        sources: Vec::new(),
        source_count: 0,
        sources_truncated: false,
        scan_elapsed_ms: elapsed_ms,
    }
}

fn failed_action(bytes_expected: u64, reason: CleanupActionReason) -> CleanupActionResult {
    CleanupActionResult {
        rule_id: CLEANER_ID.to_string(),
        action_kind: CleanupActionKind::Delete,
        status: if reason == CleanupActionReason::Cancelled {
            CleanupActionStatus::Blocked
        } else {
            CleanupActionStatus::Failed
        },
        reason_code: Some(reason),
        bytes_expected,
        released_bytes: 0,
        affected_item_count: 0,
        failed_item_count: u64::from(reason != CleanupActionReason::Cancelled),
        running_processes: Vec::new(),
    }
}

fn completed_action(
    status: CleanupActionStatus,
    was_cancelled: bool,
    bytes_expected: u64,
    released_bytes: u64,
    affected_item_count: u64,
    failed_item_count: u64,
) -> CleanupActionResult {
    CleanupActionResult {
        rule_id: CLEANER_ID.to_string(),
        action_kind: CleanupActionKind::Delete,
        status,
        reason_code: if was_cancelled {
            Some(CleanupActionReason::Cancelled)
        } else {
            (failed_item_count > 0).then_some(CleanupActionReason::ItemsSkipped)
        },
        bytes_expected,
        released_bytes,
        affected_item_count,
        failed_item_count,
        running_processes: Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn test_directory(label: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "mangodisk-codex-archive-{label}-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&path).unwrap();
        path
    }

    #[test]
    fn discovery_keeps_only_old_direct_jsonl_files() {
        let temporary = test_directory("direct");
        let old = temporary.join("old.jsonl");
        let ignored = temporary.join("note.txt");
        fs::File::create(&old)
            .unwrap()
            .write_all(b"history")
            .unwrap();
        fs::File::create(&ignored)
            .unwrap()
            .write_all(b"note")
            .unwrap();
        let now = SystemTime::now() + RETENTION_AGE + Duration::from_secs(1);

        let candidates = discover_candidates(&temporary, now, &|| false, &|_| {}).unwrap();

        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].path, old);
        assert_eq!(candidates[0].bytes, 7);
        fs::remove_dir_all(temporary).unwrap();
    }

    #[test]
    fn discovery_does_not_enter_nested_directories() {
        let temporary = test_directory("nested");
        let nested = temporary.join("nested");
        fs::create_dir(&nested).unwrap();
        fs::write(nested.join("old.jsonl"), b"history").unwrap();
        let now = SystemTime::now() + RETENTION_AGE + Duration::from_secs(1);

        let candidates = discover_candidates(&temporary, now, &|| false, &|_| {}).unwrap();

        assert!(candidates.is_empty());
        fs::remove_dir_all(temporary).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn discovery_rejects_a_linked_archive_root() {
        use std::os::unix::fs::symlink;

        let temporary = test_directory("linked-root");
        let target = temporary.join("target");
        let linked = temporary.join("archive");
        fs::create_dir(&target).unwrap();
        fs::write(target.join("old.jsonl"), b"history").unwrap();
        symlink(&target, &linked).unwrap();
        let now = SystemTime::now() + RETENTION_AGE + Duration::from_secs(1);

        assert_eq!(
            discover_candidates(&linked, now, &|| false, &|_| {}),
            Err(DiscoverError::Incomplete)
        );
        fs::remove_dir_all(temporary).unwrap();
    }

    #[test]
    fn cancelled_action_does_not_report_a_failed_archive() {
        let result = failed_action(42, CleanupActionReason::Cancelled);

        assert_eq!(result.status, CleanupActionStatus::Blocked);
        assert_eq!(result.reason_code, Some(CleanupActionReason::Cancelled));
        assert_eq!(result.failed_item_count, 0);
    }

    #[test]
    fn partial_cancellation_preserves_deleted_archives() {
        let result = completed_action(CleanupActionStatus::Partial, true, 100, 60, 1, 0);

        assert_eq!(result.status, CleanupActionStatus::Partial);
        assert_eq!(result.reason_code, Some(CleanupActionReason::Cancelled));
        assert_eq!(result.released_bytes, 60);
        assert_eq!(result.affected_item_count, 1);
        assert_eq!(result.failed_item_count, 0);
    }
}

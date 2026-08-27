use std::sync::atomic::Ordering;

use crate::{
    cleanup::{
        CleanupActionKind, CleanupActionReason, CleanupActionResult, CleanupActionStatus,
        CleanupCategory, CleanupGroup, RiskLevel, ScanItemStatus, ScanRuleResult,
    },
    shared::operation::OperationGuard,
};
use mangodisk_platform::{
    execute_windows_disk_cleanup, fresh_windows_disk_cleanup_estimates,
    windows_disk_cleanup_estimates, PlatformCancellation, WindowsDiskCleanupAvailability,
    WindowsDiskCleanupExecutionStatus, WindowsDiskCleanupKind,
};

pub(super) const CLEANER_REVISION: &str =
    "windows-native-disk-cleanup-v3-recycle-bin-and-bounded-system-logs";
pub(super) const RECYCLE_BIN_ID: &str = "special.windows-recycle-bin";

const CLEANERS: [(&str, WindowsDiskCleanupKind); 6] = [
    (RECYCLE_BIN_ID, WindowsDiskCleanupKind::RecycleBin),
    (
        "special.windows-system-logs",
        WindowsDiskCleanupKind::SystemLogs,
    ),
    (
        "special.windows-internet-cache",
        WindowsDiskCleanupKind::InternetCache,
    ),
    (
        "special.windows-delivery-optimization",
        WindowsDiskCleanupKind::DeliveryOptimization,
    ),
    (
        "special.windows-defender-cache",
        WindowsDiskCleanupKind::DefenderCache,
    ),
    (
        "special.windows-update-cleanup",
        WindowsDiskCleanupKind::UpdateCleanup,
    ),
];

pub(super) fn preview_all(cancellation: &PlatformCancellation) -> Vec<ScanRuleResult> {
    let kinds = CLEANERS.iter().map(|(_, kind)| *kind).collect::<Vec<_>>();
    windows_disk_cleanup_estimates(&kinds, cancellation)
        .into_iter()
        .filter_map(|estimate| {
            let rule_id = id_for_kind(estimate.kind)?;
            let (available, selectable, status) = match estimate.availability {
                WindowsDiskCleanupAvailability::Ready
                    if estimate.bytes == 0 && estimate.item_count == 0 =>
                {
                    (true, false, ScanItemStatus::Clean)
                }
                WindowsDiskCleanupAvailability::Ready => (true, true, ScanItemStatus::Found),
                WindowsDiskCleanupAvailability::NotApplicable => {
                    (false, false, ScanItemStatus::NotApplicable)
                }
                WindowsDiskCleanupAvailability::Limited => (true, false, ScanItemStatus::Limited),
            };
            Some(ScanRuleResult {
                rule_id: rule_id.to_string(),
                category: CleanupCategory::System,
                group: CleanupGroup::System,
                risk: RiskLevel::Recoverable,
                default_selected: false,
                recommended_selected: is_recommended(estimate.kind),
                bytes: estimate.bytes,
                file_count: estimate.item_count,
                available,
                selectable,
                status,
                running_processes: Vec::new(),
                requires_app_close: false,
                sources: Vec::new(),
                source_count: 0,
                sources_truncated: false,
                scan_elapsed_ms: estimate.elapsed_ms,
            })
        })
        .collect()
}

pub(super) fn preview_limited_all() -> Vec<ScanRuleResult> {
    CLEANERS
        .iter()
        .map(|(id, kind)| ScanRuleResult {
            rule_id: (*id).to_string(),
            category: CleanupCategory::System,
            group: CleanupGroup::System,
            risk: RiskLevel::Recoverable,
            default_selected: false,
            recommended_selected: is_recommended(*kind),
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
            scan_elapsed_ms: 0,
        })
        .collect()
}

pub(super) fn contains(id: &str) -> bool {
    CLEANERS.iter().any(|(candidate, _)| *candidate == id)
}

pub(super) fn count() -> usize {
    CLEANERS.len()
}

pub(super) fn catalog_digest() -> String {
    let mut hasher = blake3::Hasher::new();
    for (id, kind) in CLEANERS {
        hasher.update(id.as_bytes());
        hasher.update(kind.stable_id().as_bytes());
        hasher.update(CLEANER_REVISION.as_bytes());
    }
    hasher.finalize().to_hex().to_string()
}

pub(super) fn execute(id: &str, dry_run: bool, operation: &OperationGuard) -> CleanupActionResult {
    let Some(kind) = kind_for_id(id) else {
        return failed_action(id, CleanupActionReason::CleanerUnavailable, 0);
    };
    if operation.ensure_not_cancelled().is_err() {
        return cancelled_action(id, 0);
    }
    let cancellation_flag = operation.cancellation_flag();
    let cancellation = PlatformCancellation::new(move || cancellation_flag.load(Ordering::Relaxed));
    if dry_run {
        let estimate = fresh_windows_disk_cleanup_estimates(&[kind], &cancellation)
            .into_iter()
            .next();
        return match estimate {
            Some(estimate) if estimate.availability == WindowsDiskCleanupAvailability::Ready => {
                CleanupActionResult {
                    rule_id: id.to_string(),
                    action_kind: action_kind(id),
                    status: CleanupActionStatus::Previewed,
                    reason_code: None,
                    bytes_expected: estimate.bytes,
                    released_bytes: 0,
                    affected_item_count: 0,
                    failed_item_count: 0,
                    running_processes: Vec::new(),
                }
            }
            _ => failed_action(id, CleanupActionReason::PreflightFailed, 0),
        };
    }

    let result = execute_windows_disk_cleanup(kind, &cancellation);
    let (status, reason_code) = action_status_and_reason(result.status);
    CleanupActionResult {
        rule_id: id.to_string(),
        action_kind: action_kind(id),
        status,
        reason_code,
        bytes_expected: result.bytes_expected,
        released_bytes: result.released_bytes,
        affected_item_count: result.affected_item_count,
        failed_item_count: result.failed_item_count,
        running_processes: Vec::new(),
    }
}

/// Converts platform execution facts into the stable cleanup protocol so
/// native cleaners never infer user-facing failure semantics independently.
fn action_status_and_reason(
    status: WindowsDiskCleanupExecutionStatus,
) -> (CleanupActionStatus, Option<CleanupActionReason>) {
    match status {
        WindowsDiskCleanupExecutionStatus::Completed => (CleanupActionStatus::Completed, None),
        WindowsDiskCleanupExecutionStatus::Partial => (
            CleanupActionStatus::Partial,
            Some(CleanupActionReason::ItemsSkipped),
        ),
        WindowsDiskCleanupExecutionStatus::VerificationFailed => (
            CleanupActionStatus::Partial,
            Some(CleanupActionReason::VerificationFailed),
        ),
        WindowsDiskCleanupExecutionStatus::Failed => (
            CleanupActionStatus::Failed,
            Some(CleanupActionReason::ExecutionFailed),
        ),
        WindowsDiskCleanupExecutionStatus::Cancelled => (
            CleanupActionStatus::Blocked,
            Some(CleanupActionReason::Cancelled),
        ),
    }
}

pub(super) fn action_kind(id: &str) -> CleanupActionKind {
    if id == RECYCLE_BIN_ID {
        CleanupActionKind::Delete
    } else {
        CleanupActionKind::Command
    }
}

fn id_for_kind(kind: WindowsDiskCleanupKind) -> Option<&'static str> {
    CLEANERS
        .iter()
        .find_map(|(id, candidate)| (*candidate == kind).then_some(*id))
}

fn kind_for_id(id: &str) -> Option<WindowsDiskCleanupKind> {
    CLEANERS
        .iter()
        .find_map(|(candidate, kind)| (*candidate == id).then_some(*kind))
}

fn is_recommended(kind: WindowsDiskCleanupKind) -> bool {
    // Emptying the Recycle Bin permanently removes user-deleted content, and
    // Windows Update cleanup can remove rollback material. Both therefore
    // remain deliberate user choices even though their native boundaries are
    // precise. Other kinds expose only OS-approved disposable data.
    !matches!(
        kind,
        WindowsDiskCleanupKind::RecycleBin | WindowsDiskCleanupKind::UpdateCleanup
    )
}

fn failed_action(
    id: &str,
    reason_code: CleanupActionReason,
    bytes_expected: u64,
) -> CleanupActionResult {
    CleanupActionResult {
        rule_id: id.to_string(),
        action_kind: action_kind(id),
        status: CleanupActionStatus::Failed,
        reason_code: Some(reason_code),
        bytes_expected,
        released_bytes: 0,
        affected_item_count: 0,
        failed_item_count: 1,
        running_processes: Vec::new(),
    }
}

fn cancelled_action(id: &str, bytes_expected: u64) -> CleanupActionResult {
    CleanupActionResult {
        rule_id: id.to_string(),
        action_kind: action_kind(id),
        status: CleanupActionStatus::Blocked,
        reason_code: Some(CleanupActionReason::Cancelled),
        bytes_expected,
        released_bytes: 0,
        affected_item_count: 0,
        failed_item_count: 1,
        running_processes: Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_native_kind_has_one_stable_rule_id() {
        let mut ids = CLEANERS.iter().map(|(id, _)| *id).collect::<Vec<_>>();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), CLEANERS.len());
        for (_, kind) in CLEANERS {
            assert_eq!(kind_for_id(id_for_kind(kind).unwrap()), Some(kind));
        }
    }

    #[test]
    fn update_cleanup_stays_out_of_smart_recommendations() {
        assert!(!is_recommended(WindowsDiskCleanupKind::UpdateCleanup));
        assert!(!is_recommended(WindowsDiskCleanupKind::RecycleBin));
        assert!(is_recommended(WindowsDiskCleanupKind::SystemLogs));
        assert!(is_recommended(WindowsDiskCleanupKind::DeliveryOptimization));
    }

    #[test]
    fn recycle_bin_uses_delete_action_semantics() {
        assert_eq!(action_kind(RECYCLE_BIN_ID), CleanupActionKind::Delete);
        assert_eq!(
            action_kind("special.windows-system-logs"),
            CleanupActionKind::Command
        );
    }

    #[test]
    fn native_verification_failure_reports_possible_side_effects() {
        assert_eq!(
            action_status_and_reason(WindowsDiskCleanupExecutionStatus::VerificationFailed),
            (
                CleanupActionStatus::Partial,
                Some(CleanupActionReason::VerificationFailed)
            )
        );
    }
}

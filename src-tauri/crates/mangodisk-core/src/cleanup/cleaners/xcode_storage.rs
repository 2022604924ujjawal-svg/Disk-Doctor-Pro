use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    sync::{atomic::Ordering, Mutex, OnceLock},
    time::{Duration, Instant},
};

use mangodisk_platform::{
    current_platform, run_controlled_command, ControlledCommandLimits, ControlledEnvironmentPolicy,
    ControlledExecutable, Platform,
};
use serde::Deserialize;

use crate::{
    applications::catalog::{ApplicationInventory, ProcessSnapshot},
    cleanup::{
        source_selection::SourceScope, CleanupActionKind, CleanupActionReason, CleanupActionResult,
        CleanupActionStatus, CleanupCategory, CleanupGroup, CleanupSourceDetail, RiskLevel,
        ScanItemStatus, ScanRuleResult,
    },
    filesystem::{
        metadata::{diagnostic_path, display_path, is_link_like, modified_ms},
        permanent_delete::{delete_path_permanently, prepare_path_for_permanent_delete},
    },
    shared::operation::OperationGuard,
};

pub(super) const DEVICE_SUPPORT_ID: &str = "special.xcode-device-support";
pub(super) const ARCHIVES_ID: &str = "special.xcode-archives";
pub(super) const SIMULATOR_RUNTIME_ID: &str = "special.xcode-simulator-runtime";
pub(super) const CLEANER_REVISION: &str = "xcode-storage-v4-system-runtime-inventory";
const REQUIRED_STOPPED_PROCESSES: &[&str] = &["Xcode", "xcodebuild", "xctest"];
const RUNTIME_REQUIRED_STOPPED_PROCESSES: &[&str] = &["Xcode", "Simulator", "xcodebuild", "xctest"];
const XCRUN_ALIASES: &[&str] = &["xcrun"];
const RUNTIME_LIST_ARGS: &[&str] = &["simctl", "runtime", "list", "--json"];
const RUNTIME_LIST_COMMAND_ID: &str = "xcode.simctl-runtime-list";
const RUNTIME_DELETE_COMMAND_ID: &str = "xcode.simctl-runtime-delete";
const RUNTIME_COMMAND_OUTPUT_LIMIT: usize = 4 * 1024 * 1024;
const RUNTIME_LIST_TIMEOUT: Duration = Duration::from_secs(15);
const RUNTIME_DELETE_TIMEOUT: Duration = Duration::from_secs(10 * 60);
const RUNTIME_ROOT: &str = "/Library/Developer/CoreSimulator/Volumes";

static LAST_DEVICE_SUPPORT_PREVIEW: OnceLock<Mutex<Option<Vec<CandidateIdentity>>>> =
    OnceLock::new();
static LAST_RUNTIME_PREVIEW: OnceLock<Mutex<Option<Vec<RuntimeIdentity>>>> = OnceLock::new();

#[derive(Debug, Clone, PartialEq, Eq)]
struct CandidateIdentity {
    path: PathBuf,
    bytes: u64,
    file_count: u64,
    modified_at_ms: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct RuntimeIdentity {
    identifier: String,
    path: PathBuf,
    version: String,
    build: String,
    bytes: u64,
}

#[derive(Debug)]
struct RuntimeInventory {
    candidates: Vec<RuntimeIdentity>,
    identifiers: std::collections::HashSet<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RuntimeRecord {
    identifier: String,
    runtime_bundle_path: String,
    version: String,
    build: String,
    deletable: bool,
    signature_state: String,
    state: String,
    size_bytes: u64,
}

#[derive(Debug, Clone, Copy, Default)]
struct TreeMeasurement {
    bytes: u64,
    file_count: u64,
    modified_at_ms: Option<u64>,
}

#[derive(Debug)]
enum DiscoveryError {
    Cancelled,
    Incomplete,
}

pub(super) fn preview_all(
    inventory: &ApplicationInventory,
    is_cancelled: &(dyn Fn() -> bool + Sync),
    report_path: &(dyn Fn(&Path) + Sync),
) -> Vec<ScanRuleResult> {
    /*
     * A scan result is the authorization snapshot for a later cleanup. Clear
     * previous snapshots before any fallible discovery so a limited or
     * cancelled rescan can never leave older candidates actionable.
     */
    replace_device_support_preview(None);
    replace_runtime_preview(None);

    let started = Instant::now();
    let device_support = discover_device_support(is_cancelled, report_path);
    let device_elapsed_ms = started.elapsed().as_millis() as u64;
    let archive_started = Instant::now();
    let archives = discover_archives(is_cancelled, report_path);
    let archive_elapsed_ms = archive_started.elapsed().as_millis() as u64;
    let runtime_started = Instant::now();
    let runtimes = inventory
        .executable(XCRUN_ALIASES)
        .ok_or(DiscoveryError::Incomplete)
        .and_then(|executable| discover_runtimes(&executable, is_cancelled));
    let runtime_elapsed_ms = runtime_started.elapsed().as_millis() as u64;

    let running = ProcessSnapshot::capture().map(|snapshot| {
        snapshot.matching_processes(
            &REQUIRED_STOPPED_PROCESSES
                .iter()
                .map(|name| (*name).to_string())
                .collect::<Vec<_>>(),
        )
    });

    vec![
        device_support_rule(device_support, running, device_elapsed_ms),
        runtime_rule(runtimes, runtime_elapsed_ms),
        archives_rule(archives, archive_elapsed_ms),
    ]
}

pub(super) fn preview_limited_all() -> Vec<ScanRuleResult> {
    replace_device_support_preview(None);
    replace_runtime_preview(None);
    vec![
        unavailable_rule(DEVICE_SUPPORT_ID, ScanItemStatus::Limited),
        unavailable_rule(SIMULATOR_RUNTIME_ID, ScanItemStatus::Limited),
        unavailable_rule(ARCHIVES_ID, ScanItemStatus::Limited),
    ]
}

pub(super) fn contains(id: &str) -> bool {
    matches!(id, DEVICE_SUPPORT_ID | SIMULATOR_RUNTIME_ID | ARCHIVES_ID)
}

pub(super) fn execute(
    id: &str,
    inventory: &ApplicationInventory,
    source_scope: Option<&SourceScope>,
    dry_run: bool,
    operation: &OperationGuard,
) -> CleanupActionResult {
    if id == ARCHIVES_ID {
        return failed_action(id, 0, CleanupActionReason::PreflightFailed);
    }
    if id == SIMULATOR_RUNTIME_ID {
        return execute_runtimes(inventory, source_scope, dry_run, operation);
    }
    if id != DEVICE_SUPPORT_ID {
        return failed_action(id, 0, CleanupActionReason::CleanerUnavailable);
    }

    let expected = LAST_DEVICE_SUPPORT_PREVIEW
        .get_or_init(|| Mutex::new(None))
        .lock()
        .ok()
        .and_then(|snapshot| snapshot.clone());
    let Some(expected_all) = expected else {
        log::warn!("xcode_device_support_preflight_failed reason=missingPreview");
        return failed_action(id, 0, CleanupActionReason::PreflightFailed);
    };
    if let Some(scope) = source_scope {
        if scope
            .validate_known_paths(
                expected_all
                    .iter()
                    .map(|candidate| candidate.path.as_path()),
            )
            .is_err()
        {
            return failed_action(id, 0, CleanupActionReason::PreflightFailed);
        }
    }
    let expected = expected_all
        .iter()
        .filter(|candidate| source_scope.is_none_or(|scope| scope.selects(&candidate.path)))
        .cloned()
        .collect::<Vec<_>>();
    let expected_bytes = expected.iter().map(|candidate| candidate.bytes).sum();

    let running = match ProcessSnapshot::capture() {
        Ok(snapshot) => snapshot.matching_processes(
            &REQUIRED_STOPPED_PROCESSES
                .iter()
                .map(|name| (*name).to_string())
                .collect::<Vec<_>>(),
        ),
        Err(error) => {
            log::warn!(
                "xcode_device_support_preflight_failed reason=processSnapshot error_digest={}",
                blake3::hash(error.as_bytes()).to_hex()
            );
            return failed_action(id, expected_bytes, CleanupActionReason::PreflightFailed);
        }
    };
    if !running.is_empty() {
        return CleanupActionResult {
            rule_id: id.to_string(),
            action_kind: CleanupActionKind::Delete,
            status: CleanupActionStatus::Blocked,
            reason_code: Some(CleanupActionReason::RunningProcesses),
            bytes_expected: expected_bytes,
            released_bytes: 0,
            affected_item_count: 0,
            failed_item_count: 1,
            running_processes: running,
        };
    }

    let actual = match discover_device_support(
        &|| {
            operation
                .cancelled()
                .load(std::sync::atomic::Ordering::Relaxed)
        },
        &|_| {},
    ) {
        Ok(actual) => actual,
        Err(error) => {
            log_discovery_failure("execute_preflight", &error);
            return failed_action(id, expected_bytes, CleanupActionReason::PreflightFailed);
        }
    };
    if actual != expected_all {
        log::warn!(
            "xcode_device_support_preflight_failed reason=candidateSnapshotChanged expected_count={} actual_count={}",
            expected_all.len(),
            actual.len()
        );
        return failed_action(id, expected_bytes, CleanupActionReason::PreflightFailed);
    }
    if dry_run {
        return completed_action(id, CleanupActionStatus::Previewed, expected_bytes, 0, 0);
    }

    let platform = current_platform();
    let mut deleted_bytes = 0_u64;
    let mut deleted_items = 0_u64;
    let mut failed_items = 0_u64;
    for candidate in &expected {
        if operation.ensure_not_cancelled().is_err() {
            failed_items = failed_items.saturating_add(1);
            break;
        }
        let Ok(prepared) = prepare_path_for_permanent_delete(&candidate.path) else {
            failed_items = failed_items.saturating_add(1);
            continue;
        };
        if platform.validate_path_no_links(&candidate.path).is_err() {
            failed_items = failed_items.saturating_add(1);
            continue;
        }
        match delete_path_permanently(prepared, candidate.bytes, candidate.file_count) {
            Ok(()) => {
                deleted_bytes = deleted_bytes.saturating_add(candidate.bytes);
                deleted_items = deleted_items.saturating_add(candidate.file_count);
            }
            Err(error) => {
                deleted_bytes = deleted_bytes.saturating_add(error.released_bytes());
                deleted_items = deleted_items.saturating_add(error.affected_item_count());
                log::warn!(
                    "xcode_device_support_permanent_delete_failed path={} partial={} released_bytes={} affected_item_count={} error_digest={}",
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
    replace_device_support_preview(None);
    log::info!(
        "xcode_device_support_cleanup_finished candidate_count={} deleted_bytes={} deleted_items={} failed_items={}",
        expected.len(),
        deleted_bytes,
        deleted_items,
        failed_items
    );
    CleanupActionResult {
        rule_id: id.to_string(),
        action_kind: CleanupActionKind::Delete,
        status: if failed_items == 0 {
            CleanupActionStatus::Completed
        } else if deleted_items > 0 {
            CleanupActionStatus::Partial
        } else {
            CleanupActionStatus::Failed
        },
        reason_code: if operation.ensure_not_cancelled().is_err() {
            Some(CleanupActionReason::Cancelled)
        } else {
            (failed_items > 0).then_some(CleanupActionReason::ItemsSkipped)
        },
        bytes_expected: expected_bytes,
        released_bytes: deleted_bytes,
        affected_item_count: deleted_items,
        failed_item_count: failed_items,
        running_processes: Vec::new(),
    }
}

fn execute_runtimes(
    inventory: &ApplicationInventory,
    source_scope: Option<&SourceScope>,
    dry_run: bool,
    operation: &OperationGuard,
) -> CleanupActionResult {
    let Some(executable) = inventory.executable(XCRUN_ALIASES) else {
        return failed_action(
            SIMULATOR_RUNTIME_ID,
            0,
            CleanupActionReason::CleanerUnavailable,
        );
    };
    let expected_all = LAST_RUNTIME_PREVIEW
        .get_or_init(|| Mutex::new(None))
        .lock()
        .ok()
        .and_then(|snapshot| snapshot.clone());
    let Some(expected_all) = expected_all else {
        log::warn!("xcode_runtime_preflight_failed reason=missingPreview");
        return failed_action(
            SIMULATOR_RUNTIME_ID,
            0,
            CleanupActionReason::PreflightFailed,
        );
    };
    if let Some(scope) = source_scope {
        if scope
            .validate_known_paths(expected_all.iter().map(|runtime| runtime.path.as_path()))
            .is_err()
        {
            return failed_action(
                SIMULATOR_RUNTIME_ID,
                0,
                CleanupActionReason::PreflightFailed,
            );
        }
    }
    let expected = expected_all
        .iter()
        .filter(|runtime| source_scope.is_none_or(|scope| scope.selects(&runtime.path)))
        .cloned()
        .collect::<Vec<_>>();
    let expected_bytes = expected.iter().map(|runtime| runtime.bytes).sum();

    let running = match ProcessSnapshot::capture() {
        Ok(snapshot) => snapshot.matching_processes(
            &RUNTIME_REQUIRED_STOPPED_PROCESSES
                .iter()
                .map(|name| (*name).to_string())
                .collect::<Vec<_>>(),
        ),
        Err(error) => {
            log::warn!(
                "xcode_runtime_preflight_failed reason=processSnapshot error_digest={}",
                blake3::hash(error.as_bytes()).to_hex()
            );
            return failed_action(
                SIMULATOR_RUNTIME_ID,
                expected_bytes,
                CleanupActionReason::PreflightFailed,
            );
        }
    };
    if !running.is_empty() {
        return CleanupActionResult {
            rule_id: SIMULATOR_RUNTIME_ID.to_string(),
            action_kind: CleanupActionKind::Command,
            status: CleanupActionStatus::Blocked,
            reason_code: Some(CleanupActionReason::RunningProcesses),
            bytes_expected: expected_bytes,
            released_bytes: 0,
            affected_item_count: 0,
            failed_item_count: 1,
            running_processes: running,
        };
    }

    let actual = match discover_runtimes(&executable, &|| {
        operation.cancelled().load(Ordering::Relaxed)
    }) {
        Ok(actual) => actual,
        Err(error) => {
            log_discovery_failure("execute_runtime_preflight", &error);
            return failed_action(
                SIMULATOR_RUNTIME_ID,
                expected_bytes,
                CleanupActionReason::PreflightFailed,
            );
        }
    };
    if actual != expected_all {
        log::warn!(
            "xcode_runtime_preflight_failed reason=candidateSnapshotChanged expected_count={} actual_count={}",
            expected_all.len(),
            actual.len()
        );
        return failed_action(
            SIMULATOR_RUNTIME_ID,
            expected_bytes,
            CleanupActionReason::PreflightFailed,
        );
    }

    for runtime in &expected {
        if run_runtime_delete(&executable, runtime, true, operation).is_err() {
            return failed_action(
                SIMULATOR_RUNTIME_ID,
                expected_bytes,
                CleanupActionReason::PreflightFailed,
            );
        }
    }
    if dry_run {
        return completed_action(
            SIMULATOR_RUNTIME_ID,
            CleanupActionStatus::Previewed,
            expected_bytes,
            0,
            0,
        );
    }

    let mut command_failed_count = 0_u64;
    for runtime in &expected {
        if operation.ensure_not_cancelled().is_err() {
            command_failed_count = command_failed_count.saturating_add(1);
            break;
        }
        if run_runtime_delete(&executable, runtime, false, operation).is_err() {
            command_failed_count = command_failed_count.saturating_add(1);
        }
    }
    /*
     * Once a delete command has started, cancellation must not suppress the
     * final read-only reconciliation. The command remains bounded by
     * RUNTIME_LIST_TIMEOUT, while the original cancellation state is
     * preserved in the returned action.
     */
    let after = match discover_runtime_inventory(&executable, &|| false) {
        Ok(after) => after,
        Err(error) => {
            log_discovery_failure("execute_runtime_verify", &error);
            replace_runtime_preview(None);
            return CleanupActionResult {
                rule_id: SIMULATOR_RUNTIME_ID.to_string(),
                action_kind: CleanupActionKind::Command,
                status: CleanupActionStatus::Partial,
                reason_code: Some(if operation.cancelled().load(Ordering::Relaxed) {
                    CleanupActionReason::Cancelled
                } else {
                    CleanupActionReason::VerificationFailed
                }),
                bytes_expected: expected_bytes,
                released_bytes: 0,
                affected_item_count: 0,
                failed_item_count: expected.len() as u64,
                running_processes: Vec::new(),
            };
        }
    };
    let (released_bytes, deleted_count, failed_item_count) =
        summarize_runtime_deletions(&expected, &after.identifiers);
    replace_runtime_preview(None);
    log::info!(
        "xcode_runtime_cleanup_finished candidate_count={} released_bytes={} deleted_count={} command_failed_count={} failed_count={}",
        expected.len(),
        released_bytes,
        deleted_count,
        command_failed_count,
        failed_item_count
    );
    CleanupActionResult {
        rule_id: SIMULATOR_RUNTIME_ID.to_string(),
        action_kind: CleanupActionKind::Command,
        status: if failed_item_count == 0 {
            CleanupActionStatus::Completed
        } else if deleted_count == 0 {
            CleanupActionStatus::Failed
        } else {
            CleanupActionStatus::Partial
        },
        reason_code: if operation.ensure_not_cancelled().is_err() {
            Some(CleanupActionReason::Cancelled)
        } else {
            (failed_item_count > 0).then_some(CleanupActionReason::ItemsSkipped)
        },
        bytes_expected: expected_bytes,
        released_bytes,
        affected_item_count: deleted_count,
        failed_item_count,
        running_processes: Vec::new(),
    }
}

fn summarize_runtime_deletions(
    expected: &[RuntimeIdentity],
    remaining_identifiers: &std::collections::HashSet<String>,
) -> (u64, u64, u64) {
    let (released_bytes, deleted_count) = expected.iter().fold(
        (0_u64, 0_u64),
        |(released_bytes, deleted_count), runtime| {
            if remaining_identifiers.contains(&runtime.identifier) {
                (released_bytes, deleted_count)
            } else {
                (
                    released_bytes.saturating_add(runtime.bytes),
                    deleted_count.saturating_add(1),
                )
            }
        },
    );
    (
        released_bytes,
        deleted_count,
        (expected.len() as u64).saturating_sub(deleted_count),
    )
}

fn run_runtime_delete(
    executable: &ControlledExecutable,
    runtime: &RuntimeIdentity,
    dry_run: bool,
    operation: &OperationGuard,
) -> Result<(), ()> {
    let mut args = vec!["simctl", "runtime", "delete", runtime.identifier.as_str()];
    if dry_run {
        args.push("--dry-run");
    }
    let output = run_controlled_command(
        RUNTIME_DELETE_COMMAND_ID,
        executable,
        &args,
        ControlledEnvironmentPolicy::Isolated,
        ControlledCommandLimits {
            timeout: RUNTIME_DELETE_TIMEOUT,
            stdout_bytes: RUNTIME_COMMAND_OUTPUT_LIMIT,
            stderr_bytes: RUNTIME_COMMAND_OUTPUT_LIMIT,
        },
        &|| operation.cancelled().load(Ordering::Relaxed),
    )
    .map_err(|error| {
        log::warn!(
            "xcode_runtime_command_failed stage={} identifier={} reason={}",
            if dry_run { "dryRun" } else { "delete" },
            runtime.identifier,
            error.as_str()
        );
    })?;
    if !output.status.success() {
        log::warn!(
            "xcode_runtime_command_failed stage={} identifier={} reason=nonZeroExit stderr_bytes={} elapsed_ms={}",
            if dry_run { "dryRun" } else { "delete" },
            runtime.identifier,
            output.stderr_bytes,
            output.elapsed_ms
        );
        return Err(());
    }
    Ok(())
}

fn device_support_rule(
    candidates: Result<Vec<CandidateIdentity>, DiscoveryError>,
    running: Result<Vec<String>, String>,
    elapsed_ms: u64,
) -> ScanRuleResult {
    match (candidates, running) {
        (Ok(candidates), Ok(running)) => {
            if !replace_device_support_preview(Some(candidates.clone())) {
                return unavailable_rule_with_elapsed(
                    DEVICE_SUPPORT_ID,
                    ScanItemStatus::Limited,
                    elapsed_ms,
                );
            }
            let bytes = candidates.iter().map(|candidate| candidate.bytes).sum();
            let file_count = candidates
                .iter()
                .map(|candidate| candidate.file_count)
                .sum();
            let (sources, source_count) = summarize_sources(&candidates);
            ScanRuleResult {
                rule_id: DEVICE_SUPPORT_ID.to_string(),
                category: CleanupCategory::Xcode,
                group: CleanupGroup::Xcode,
                risk: RiskLevel::Recoverable,
                default_selected: false,
                recommended_selected: false,
                bytes,
                file_count,
                available: true,
                selectable: !candidates.is_empty(),
                status: if candidates.is_empty() {
                    ScanItemStatus::Clean
                } else if running.is_empty() {
                    ScanItemStatus::Found
                } else {
                    ScanItemStatus::RequiresClose
                },
                running_processes: running,
                requires_app_close: true,
                sources,
                source_count,
                sources_truncated: false,
                scan_elapsed_ms: elapsed_ms,
            }
        }
        (Err(error), _) => {
            replace_device_support_preview(None);
            log_discovery_failure("preview_device_support", &error);
            unavailable_rule_with_elapsed(DEVICE_SUPPORT_ID, ScanItemStatus::Limited, elapsed_ms)
        }
        (Ok(_), Err(error)) => {
            replace_device_support_preview(None);
            log::warn!(
                "xcode_device_support_preview_failed reason=processSnapshot error_digest={}",
                blake3::hash(error.as_bytes()).to_hex()
            );
            unavailable_rule_with_elapsed(DEVICE_SUPPORT_ID, ScanItemStatus::Limited, elapsed_ms)
        }
    }
}

fn runtime_rule(
    runtimes: Result<Vec<RuntimeIdentity>, DiscoveryError>,
    elapsed_ms: u64,
) -> ScanRuleResult {
    match runtimes {
        Ok(runtimes) => {
            let running = ProcessSnapshot::capture().map(|snapshot| {
                snapshot.matching_processes(
                    &RUNTIME_REQUIRED_STOPPED_PROCESSES
                        .iter()
                        .map(|name| (*name).to_string())
                        .collect::<Vec<_>>(),
                )
            });
            let Ok(running) = running else {
                replace_runtime_preview(None);
                return unavailable_rule_with_elapsed(
                    SIMULATOR_RUNTIME_ID,
                    ScanItemStatus::Limited,
                    elapsed_ms,
                );
            };
            if !replace_runtime_preview(Some(runtimes.clone())) {
                return unavailable_rule_with_elapsed(
                    SIMULATOR_RUNTIME_ID,
                    ScanItemStatus::Limited,
                    elapsed_ms,
                );
            }
            let bytes = runtimes.iter().map(|runtime| runtime.bytes).sum();
            let sources = runtimes
                .iter()
                .map(|runtime| CleanupSourceDetail {
                    path: display_path(&runtime.path),
                    bytes: runtime.bytes,
                    file_count: 1,
                    modified_at_ms: None,
                    block_reason: None,
                })
                .collect::<Vec<_>>();
            ScanRuleResult {
                rule_id: SIMULATOR_RUNTIME_ID.to_string(),
                category: CleanupCategory::Xcode,
                group: CleanupGroup::Xcode,
                risk: RiskLevel::Recoverable,
                default_selected: false,
                recommended_selected: false,
                bytes,
                file_count: runtimes.len() as u64,
                available: true,
                selectable: !runtimes.is_empty(),
                status: if runtimes.is_empty() {
                    ScanItemStatus::Clean
                } else if running.is_empty() {
                    ScanItemStatus::Found
                } else {
                    ScanItemStatus::RequiresClose
                },
                running_processes: running,
                requires_app_close: true,
                sources,
                source_count: runtimes.len() as u64,
                sources_truncated: false,
                scan_elapsed_ms: elapsed_ms,
            }
        }
        Err(error) => {
            replace_runtime_preview(None);
            log_discovery_failure("preview_simulator_runtime", &error);
            unavailable_rule_with_elapsed(SIMULATOR_RUNTIME_ID, ScanItemStatus::Limited, elapsed_ms)
        }
    }
}

fn replace_device_support_preview(candidates: Option<Vec<CandidateIdentity>>) -> bool {
    match LAST_DEVICE_SUPPORT_PREVIEW
        .get_or_init(|| Mutex::new(None))
        .lock()
    {
        Ok(mut snapshot) => {
            *snapshot = candidates;
            true
        }
        Err(_) => {
            log::error!("xcode_device_support_preview_update_failed reason=lockPoisoned");
            false
        }
    }
}

fn replace_runtime_preview(runtimes: Option<Vec<RuntimeIdentity>>) -> bool {
    match LAST_RUNTIME_PREVIEW.get_or_init(|| Mutex::new(None)).lock() {
        Ok(mut snapshot) => {
            *snapshot = runtimes;
            true
        }
        Err(_) => {
            log::error!("xcode_runtime_preview_update_failed reason=lockPoisoned");
            false
        }
    }
}

fn archives_rule(
    archives: Result<Vec<CandidateIdentity>, DiscoveryError>,
    elapsed_ms: u64,
) -> ScanRuleResult {
    match archives {
        Ok(archives) => {
            let bytes = archives.iter().map(|candidate| candidate.bytes).sum();
            let file_count = archives.iter().map(|candidate| candidate.file_count).sum();
            let (sources, source_count) = summarize_sources(&archives);
            ScanRuleResult {
                rule_id: ARCHIVES_ID.to_string(),
                category: CleanupCategory::Xcode,
                group: CleanupGroup::Xcode,
                risk: RiskLevel::Recoverable,
                default_selected: false,
                recommended_selected: false,
                bytes,
                file_count,
                available: true,
                selectable: false,
                status: if archives.is_empty() {
                    ScanItemStatus::Clean
                } else {
                    ScanItemStatus::ReviewOnly
                },
                running_processes: Vec::new(),
                requires_app_close: false,
                sources,
                source_count,
                sources_truncated: false,
                scan_elapsed_ms: elapsed_ms,
            }
        }
        Err(error) => {
            log_discovery_failure("preview_archives", &error);
            unavailable_rule_with_elapsed(ARCHIVES_ID, ScanItemStatus::Limited, elapsed_ms)
        }
    }
}

fn discover_runtimes(
    executable: &ControlledExecutable,
    is_cancelled: &(dyn Fn() -> bool + Sync),
) -> Result<Vec<RuntimeIdentity>, DiscoveryError> {
    discover_runtime_inventory(executable, is_cancelled).map(|inventory| inventory.candidates)
}

fn discover_runtime_inventory(
    executable: &ControlledExecutable,
    is_cancelled: &(dyn Fn() -> bool + Sync),
) -> Result<RuntimeInventory, DiscoveryError> {
    let output = run_controlled_command(
        RUNTIME_LIST_COMMAND_ID,
        executable,
        RUNTIME_LIST_ARGS,
        ControlledEnvironmentPolicy::Isolated,
        ControlledCommandLimits {
            timeout: RUNTIME_LIST_TIMEOUT,
            stdout_bytes: RUNTIME_COMMAND_OUTPUT_LIMIT,
            stderr_bytes: RUNTIME_COMMAND_OUTPUT_LIMIT,
        },
        is_cancelled,
    )
    .map_err(|error| {
        log::warn!(
            "xcode_runtime_command_failed stage=list reason={}",
            error.as_str()
        );
        if error.as_str() == "cancelled" {
            DiscoveryError::Cancelled
        } else {
            DiscoveryError::Incomplete
        }
    })?;
    if !output.status.success() {
        log::warn!(
            "xcode_runtime_command_failed stage=list reason=nonZeroExit stderr_bytes={} elapsed_ms={}",
            output.stderr_bytes,
            output.elapsed_ms
        );
        return Err(DiscoveryError::Incomplete);
    }
    parse_runtime_inventory(&output.stdout)
}

fn parse_runtime_inventory(bytes: &[u8]) -> Result<RuntimeInventory, DiscoveryError> {
    let records = serde_json::from_slice::<BTreeMap<String, RuntimeRecord>>(bytes)
        .map_err(|_| DiscoveryError::Incomplete)?;
    let mut runtimes = Vec::new();
    let mut identifiers = std::collections::HashSet::with_capacity(records.len());
    for (key, record) in records {
        if key != record.identifier || !valid_runtime_identifier(&record.identifier) {
            return Err(DiscoveryError::Incomplete);
        }
        identifiers.insert(record.identifier.clone());
        let path = PathBuf::from(&record.runtime_bundle_path);
        if !record.deletable
            || record.state != "Ready"
            || record.signature_state != "Verified"
            || record.size_bytes == 0
            || !is_managed_runtime_path(&path)
        {
            continue;
        }
        runtimes.push(RuntimeIdentity {
            identifier: record.identifier,
            path,
            version: record.version,
            build: record.build,
            bytes: record.size_bytes,
        });
    }
    runtimes.sort_by(|left, right| left.path.cmp(&right.path));
    Ok(RuntimeInventory {
        candidates: runtimes,
        identifiers,
    })
}

fn is_managed_runtime_path(path: &Path) -> bool {
    path.is_absolute()
        && path.starts_with(RUNTIME_ROOT)
        && path.components().all(|component| {
            !matches!(
                component,
                std::path::Component::CurDir | std::path::Component::ParentDir
            )
        })
}

fn valid_runtime_identifier(value: &str) -> bool {
    value.len() == 36
        && value.chars().enumerate().all(|(index, character)| {
            if matches!(index, 8 | 13 | 18 | 23) {
                character == '-'
            } else {
                character.is_ascii_hexdigit()
            }
        })
}

fn discover_device_support(
    is_cancelled: &(dyn Fn() -> bool + Sync),
    report_path: &(dyn Fn(&Path) + Sync),
) -> Result<Vec<CandidateIdentity>, DiscoveryError> {
    let library = user_library()?;
    discover_device_support_in_library(&library, is_cancelled, report_path)
}

fn discover_device_support_in_library(
    library: &Path,
    is_cancelled: &(dyn Fn() -> bool + Sync),
    report_path: &(dyn Fn(&Path) + Sync),
) -> Result<Vec<CandidateIdentity>, DiscoveryError> {
    let roots = [
        library.join("Developer/Xcode/iOS DeviceSupport"),
        library.join("Developer/Xcode/watchOS DeviceSupport"),
        library.join("Developer/Xcode/tvOS DeviceSupport"),
        library.join("Developer/Xcode/visionOS DeviceSupport"),
    ];
    let mut candidates = Vec::new();
    for root in roots {
        if !root.exists() {
            continue;
        }
        report_path(&root);
        /*
         * Device Support is recoverable but not an automatic cleanup target.
         * Showing the complete direct-child inventory gives users an honest
         * view of Xcode storage and lets them choose individual system
         * versions. Execution still revalidates the complete snapshot and
         * permanently deletes only reviewed directories.
         */
        candidates.extend(measured_children(&root, is_cancelled)?);
    }
    candidates.sort_by(|left, right| left.path.cmp(&right.path));
    Ok(candidates)
}

fn discover_archives(
    is_cancelled: &(dyn Fn() -> bool + Sync),
    report_path: &(dyn Fn(&Path) + Sync),
) -> Result<Vec<CandidateIdentity>, DiscoveryError> {
    let root = user_library()?.join("Developer/Xcode/Archives");
    if !root.exists() {
        return Ok(Vec::new());
    }
    report_path(&root);
    measured_children(&root, is_cancelled)
}

fn measured_children(
    root: &Path,
    is_cancelled: &(dyn Fn() -> bool + Sync),
) -> Result<Vec<CandidateIdentity>, DiscoveryError> {
    current_platform()
        .validate_path_no_links(root)
        .map_err(|_| DiscoveryError::Incomplete)?;
    let entries = fs::read_dir(root).map_err(|_| DiscoveryError::Incomplete)?;
    let mut paths = Vec::new();
    for entry in entries {
        if is_cancelled() {
            return Err(DiscoveryError::Cancelled);
        }
        let entry = entry.map_err(|_| DiscoveryError::Incomplete)?;
        let metadata =
            fs::symlink_metadata(entry.path()).map_err(|_| DiscoveryError::Incomplete)?;
        if is_link_like(&metadata) || !metadata.is_dir() {
            continue;
        }
        paths.push(entry.path());
    }
    paths.sort();
    paths
        .into_iter()
        .map(|path| {
            let measured = measure_tree(&path, is_cancelled)?;
            Ok(CandidateIdentity {
                path,
                bytes: measured.bytes,
                file_count: measured.file_count,
                modified_at_ms: measured.modified_at_ms,
            })
        })
        .collect()
}

fn measure_tree(
    path: &Path,
    is_cancelled: &(dyn Fn() -> bool + Sync),
) -> Result<TreeMeasurement, DiscoveryError> {
    if is_cancelled() {
        return Err(DiscoveryError::Cancelled);
    }
    let metadata = fs::symlink_metadata(path).map_err(|_| DiscoveryError::Incomplete)?;
    if is_link_like(&metadata) {
        return Err(DiscoveryError::Incomplete);
    }
    let mut result = TreeMeasurement {
        bytes: if metadata.is_file() {
            metadata.len()
        } else {
            0
        },
        file_count: u64::from(metadata.is_file()),
        modified_at_ms: modified_ms(&metadata),
    };
    if !metadata.is_dir() {
        return Ok(result);
    }
    for entry in fs::read_dir(path).map_err(|_| DiscoveryError::Incomplete)? {
        let entry = entry.map_err(|_| DiscoveryError::Incomplete)?;
        let child = measure_tree(&entry.path(), is_cancelled)?;
        result.bytes = result.bytes.saturating_add(child.bytes);
        result.file_count = result.file_count.saturating_add(child.file_count);
        result.modified_at_ms = result.modified_at_ms.max(child.modified_at_ms);
    }
    Ok(result)
}

fn summarize_sources(candidates: &[CandidateIdentity]) -> (Vec<CleanupSourceDetail>, u64) {
    let source_count = candidates.len() as u64;
    let mut sources = candidates.to_vec();
    sources.sort_by(|left, right| {
        right
            .bytes
            .cmp(&left.bytes)
            .then_with(|| left.path.cmp(&right.path))
    });
    let sources = sources
        .into_iter()
        .map(|candidate| CleanupSourceDetail {
            path: display_path(&candidate.path),
            bytes: candidate.bytes,
            file_count: candidate.file_count,
            modified_at_ms: candidate.modified_at_ms,
            block_reason: None,
        })
        .collect::<Vec<_>>();
    (sources, source_count)
}

fn user_library() -> Result<PathBuf, DiscoveryError> {
    current_platform()
        .user_directories()
        .map(|directories| directories.home_directory().join("Library"))
        .map_err(|_| DiscoveryError::Incomplete)
}

fn unavailable_rule(id: &str, status: ScanItemStatus) -> ScanRuleResult {
    unavailable_rule_with_elapsed(id, status, 0)
}

fn unavailable_rule_with_elapsed(
    id: &str,
    status: ScanItemStatus,
    elapsed_ms: u64,
) -> ScanRuleResult {
    ScanRuleResult {
        rule_id: id.to_string(),
        category: CleanupCategory::Xcode,
        group: CleanupGroup::Xcode,
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

fn completed_action(
    id: &str,
    status: CleanupActionStatus,
    expected_bytes: u64,
    released_bytes: u64,
    affected_item_count: u64,
) -> CleanupActionResult {
    CleanupActionResult {
        rule_id: id.to_string(),
        action_kind: action_kind(id),
        status,
        reason_code: None,
        bytes_expected: expected_bytes,
        released_bytes,
        affected_item_count,
        failed_item_count: 0,
        running_processes: Vec::new(),
    }
}

fn failed_action(
    id: &str,
    expected_bytes: u64,
    reason: CleanupActionReason,
) -> CleanupActionResult {
    CleanupActionResult {
        rule_id: id.to_string(),
        action_kind: action_kind(id),
        status: CleanupActionStatus::Failed,
        reason_code: Some(reason),
        bytes_expected: expected_bytes,
        released_bytes: 0,
        affected_item_count: 0,
        failed_item_count: 1,
        running_processes: Vec::new(),
    }
}

fn action_kind(id: &str) -> CleanupActionKind {
    if id == SIMULATOR_RUNTIME_ID {
        CleanupActionKind::Command
    } else {
        CleanupActionKind::Delete
    }
}

fn log_discovery_failure(stage: &str, error: &DiscoveryError) {
    log::warn!("xcode_storage_discovery_failed stage={stage} reason={error:?}");
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        env,
        sync::atomic::{AtomicU64, Ordering},
        time::{Duration, UNIX_EPOCH},
    };

    static NEXT_FIXTURE_ID: AtomicU64 = AtomicU64::new(1);

    fn fixture_root() -> PathBuf {
        let root = env::temp_dir().join(format!(
            "mangodisk-xcode-storage-{}-{}",
            std::process::id(),
            NEXT_FIXTURE_ID.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&root).expect("fixture root should be created");
        root
    }

    fn runtime_identity(identifier: &str, suffix: &str, bytes: u64) -> RuntimeIdentity {
        RuntimeIdentity {
            identifier: identifier.to_string(),
            path: PathBuf::from(RUNTIME_ROOT).join(suffix),
            version: "18.0".to_string(),
            build: "22A000".to_string(),
            bytes,
        }
    }

    #[test]
    fn tree_measurement_rejects_links() {
        let root = fixture_root();
        fs::write(root.join("data"), b"cache").expect("fixture file should be written");
        #[cfg(unix)]
        std::os::unix::fs::symlink(root.join("data"), root.join("link"))
            .expect("fixture link should be created");
        let measured = measure_tree(&root, &|| false);
        assert!(matches!(measured, Err(DiscoveryError::Incomplete)));
        fs::remove_dir_all(root).expect("fixture should be removed");
    }

    #[test]
    fn device_support_discovery_reports_every_reviewable_version() {
        let root = fixture_root();
        let library = root.join("Library");
        let support = library.join("Developer/Xcode/iOS DeviceSupport");
        fs::create_dir_all(support.join("iPhone14,2 18.7.7 (22H340)"))
            .expect("older device support should be created");
        fs::write(
            support.join("iPhone14,2 18.7.7 (22H340)").join("symbols"),
            b"old",
        )
        .expect("older fixture should be written");
        std::thread::sleep(Duration::from_millis(20));
        fs::create_dir_all(support.join("iPhone14,2 18.7.10 (22H400)"))
            .expect("newer device support should be created");
        fs::write(
            support.join("iPhone14,2 18.7.10 (22H400)").join("symbols"),
            b"new",
        )
        .expect("newer fixture should be written");
        fs::create_dir_all(support.join("iPhone15,4 18.7.8 (22H352)"))
            .expect("single device family should be created");

        let candidates = discover_device_support_in_library(&library, &|| false, &|_| {})
            .expect("fixture discovery should succeed");

        assert_eq!(candidates.len(), 3);
        assert!(candidates.iter().any(|candidate| {
            candidate.path.file_name().and_then(|name| name.to_str())
                == Some("iPhone14,2 18.7.10 (22H400)")
        }));
        fs::remove_dir_all(root).expect("fixture should be removed");
    }

    #[test]
    fn source_summary_returns_every_source_largest_first() {
        let modified = UNIX_EPOCH + Duration::from_secs(10);
        let modified_at_ms = modified
            .duration_since(UNIX_EPOCH)
            .ok()
            .map(|value| value.as_millis() as u64);
        let candidates = (0..514)
            .map(|index| CandidateIdentity {
                path: PathBuf::from(format!("/tmp/candidate-{index}")),
                bytes: index as u64,
                file_count: 1,
                modified_at_ms,
            })
            .collect::<Vec<_>>();
        let (sources, count) = summarize_sources(&candidates);
        assert_eq!(sources.len(), candidates.len());
        assert_eq!(count, candidates.len() as u64);
        assert!(sources
            .windows(2)
            .all(|pair| pair[0].bytes >= pair[1].bytes));
    }

    #[test]
    fn runtime_inventory_keeps_only_verified_deletable_managed_images() {
        let fixture = br#"{
          "96AD28C1-7789-4F52-8376-486FBAEDA226": {
            "identifier": "96AD28C1-7789-4F52-8376-486FBAEDA226",
            "runtimeBundlePath": "/Library/Developer/CoreSimulator/Volumes/iOS_22D8075/Library/Developer/CoreSimulator/Profiles/Runtimes/iOS 18.3.simruntime",
            "version": "18.3.1",
            "build": "22D8075",
            "deletable": true,
            "signatureState": "Verified",
            "state": "Ready",
            "sizeBytes": 8708125252
          },
          "EB50B481-5DF9-4DCF-9160-49D2F07B914E": {
            "identifier": "EB50B481-5DF9-4DCF-9160-49D2F07B914E",
            "runtimeBundlePath": "/tmp/unmanaged.simruntime",
            "version": "16.0",
            "build": "20A360",
            "deletable": true,
            "signatureState": "Verified",
            "state": "Ready",
            "sizeBytes": 6280680540
          }
        }"#;

        let inventory = parse_runtime_inventory(fixture).expect("the runtime fixture should parse");
        assert_eq!(inventory.identifiers.len(), 2);
        let runtimes = inventory.candidates;
        assert_eq!(runtimes.len(), 1);
        assert_eq!(
            runtimes[0].identifier,
            "96AD28C1-7789-4F52-8376-486FBAEDA226"
        );
        assert_eq!(runtimes[0].bytes, 8_708_125_252);
    }

    #[test]
    fn limited_preview_invalidates_previous_xcode_candidates() {
        let _operation_lock = crate::shared::operation::test_operation_lock();
        let device_candidate = CandidateIdentity {
            path: PathBuf::from("/fixture/iOS DeviceSupport/18.0"),
            bytes: 10,
            file_count: 1,
            modified_at_ms: Some(1),
        };
        let runtime = runtime_identity("96AD28C1-7789-4F52-8376-486FBAEDA226", "iOS_22A000", 20);
        assert!(replace_device_support_preview(Some(vec![device_candidate])));
        assert!(replace_runtime_preview(Some(vec![runtime])));

        let device_rule = device_support_rule(Err(DiscoveryError::Incomplete), Ok(Vec::new()), 0);
        let runtime_rule = runtime_rule(Err(DiscoveryError::Incomplete), 0);

        assert_eq!(device_rule.status, ScanItemStatus::Limited);
        assert_eq!(runtime_rule.status, ScanItemStatus::Limited);
        assert!(LAST_DEVICE_SUPPORT_PREVIEW
            .get_or_init(|| Mutex::new(None))
            .lock()
            .expect("device support preview lock should remain available")
            .is_none());
        assert!(LAST_RUNTIME_PREVIEW
            .get_or_init(|| Mutex::new(None))
            .lock()
            .expect("runtime preview lock should remain available")
            .is_none());
    }

    #[test]
    fn runtime_reconciliation_reports_partial_deletion_after_cancellation() {
        let first = runtime_identity("96AD28C1-7789-4F52-8376-486FBAEDA226", "iOS_22A000", 20);
        let second = runtime_identity("EB50B481-5DF9-4DCF-9160-49D2F07B914E", "iOS_22B000", 30);
        let remaining = std::collections::HashSet::from([second.identifier.clone()]);

        assert_eq!(
            summarize_runtime_deletions(&[first, second], &remaining),
            (20, 1, 1)
        );
    }

    /// This ignored test reads the current user's Xcode Device Support and
    /// exercises MangoDisk's dry-run path. It never moves any directory.
    #[test]
    #[ignore = "reads locally installed Xcode Device Support"]
    fn real_device_support_preview_and_dry_run_are_read_only() {
        let _operation_lock = crate::shared::operation::test_operation_lock();
        let before =
            discover_device_support(&|| false, &|_| {}).expect("real discovery should complete");
        let context = crate::applications::catalog::ScanContext::capture();
        let rules = preview_all(&context.inventory, &|| false, &|_| {});
        let rule = rules
            .iter()
            .find(|rule| rule.rule_id == DEVICE_SUPPORT_ID)
            .expect("the Xcode cleaner registry must include Device Support");
        if rule.status == ScanItemStatus::RequiresClose {
            println!("Xcode is running; the dry-run preflight was intentionally skipped");
            return;
        }
        assert_eq!(rule.status, ScanItemStatus::Found);

        let operation =
            OperationGuard::start(crate::shared::operation::CoordinatedOperationKind::Cleanup)
                .expect("the dry run should start");
        let action = execute(
            DEVICE_SUPPORT_ID,
            &context.inventory,
            None,
            true,
            &operation,
        );
        operation.complete();
        let after = discover_device_support(&|| false, &|_| {})
            .expect("post-dry-run discovery should work");

        assert_eq!(action.status, CleanupActionStatus::Previewed);
        assert_eq!(action.bytes_expected, rule.bytes);
        assert_eq!(action.released_bytes, 0);
        assert_eq!(before, after);
    }

    /// This ignored test asks simctl for its managed runtime inventory and
    /// exercises only MangoDisk's dry-run path. It never removes a runtime.
    #[test]
    #[ignore = "reads locally installed Simulator runtimes through simctl"]
    fn real_runtime_preview_and_dry_run_are_read_only() {
        let _operation_lock = crate::shared::operation::test_operation_lock();
        let context = crate::applications::catalog::ScanContext::capture();
        let executable = context
            .inventory
            .executable(XCRUN_ALIASES)
            .expect("xcrun should be captured from the macOS inventory");
        let before =
            discover_runtimes(&executable, &|| false).expect("real runtime discovery should work");
        let rule = runtime_rule(Ok(before.clone()), 0);
        if rule.status == ScanItemStatus::RequiresClose {
            println!("Xcode or Simulator is running; runtime dry-run was intentionally skipped");
            return;
        }
        assert_eq!(rule.status, ScanItemStatus::Found);

        let operation =
            OperationGuard::start(crate::shared::operation::CoordinatedOperationKind::Cleanup)
                .expect("the dry run should start");
        let action = execute(
            SIMULATOR_RUNTIME_ID,
            &context.inventory,
            None,
            true,
            &operation,
        );
        operation.complete();
        let after =
            discover_runtimes(&executable, &|| false).expect("post-dry-run discovery should work");

        assert_eq!(action.status, CleanupActionStatus::Previewed);
        assert_eq!(action.bytes_expected, rule.bytes);
        assert_eq!(action.released_bytes, 0);
        assert_eq!(before, after);
    }
}

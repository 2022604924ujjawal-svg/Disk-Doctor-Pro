use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::sync::atomic::Ordering;
use std::sync::{Mutex, OnceLock};
use std::time::Instant;

use mangodisk_platform::{
    current_platform, PlatformCancellation, PlatformError, PlatformErrorCode, PlatformResult,
    PlatformStartupArtifact, PlatformStartupChangeRequest, PlatformStartupChangeResult,
    PlatformStartupConfiguredState, PlatformStartupControlCapability, PlatformStartupDesiredState,
    PlatformStartupDiagnosticCode, PlatformStartupRuntimeState, PlatformStartupScope,
    PlatformStartupSourceResult, StartupPlatform,
};

use crate::{
    filesystem::metadata::now_ms,
    history::{
        HistoryService, OperationCategory, OperationDetails, OperationOutcome, OperationRecord,
        StartupHistoryItem, StartupHistoryItemStatus, StartupHistoryState,
        StartupManagementOperationDetails, OPERATION_RECORD_SCHEMA_VERSION,
    },
    shared::{
        application_paths,
        operation::{CoordinatedOperationKind, OperationCancellationToken, OperationGuard},
        CoreError, CoreResult,
    },
};

use super::{
    aggregation, StartupCatalog, StartupChangeFailureReason, StartupChangeItemResult,
    StartupChangeOutcomeStatus, StartupChangePlan, StartupChangePlanItem, StartupChangeResult,
    StartupChangeSelection, StartupChangeSkipReason, StartupChangeSkippedItem,
    StartupChangeWarning, StartupConfiguredState, StartupCoverageStatus, StartupDesiredState,
    STARTUP_CATALOG_SCHEMA_VERSION, STARTUP_CHANGE_PLAN_SCHEMA_VERSION,
};

const CHANGE_PLAN_TTL_MS: u64 = 5 * 60 * 1_000;
const MAX_CHANGE_ITEMS: usize = 256;

static CATALOG_SESSION: OnceLock<Mutex<Option<StartupCatalogSession>>> = OnceLock::new();
static CHANGE_PLAN: OnceLock<Mutex<Option<PendingChangePlan>>> = OnceLock::new();

#[derive(Clone)]
struct NativeStartupRecord {
    source_id: String,
    artifact: PlatformStartupArtifact,
}

#[derive(Clone)]
struct StartupCatalogSession {
    catalog: StartupCatalog,
    native_records: BTreeMap<String, NativeStartupRecord>,
}

#[derive(Clone)]
struct PendingChangeTarget {
    item_id: String,
    request: PlatformStartupChangeRequest,
}

#[derive(Clone)]
struct PendingChangePlan {
    public_plan: StartupChangePlan,
    targets: Vec<PendingChangeTarget>,
}

pub struct StartupService;

impl StartupService {
    pub fn cancel_scan() {
        OperationCancellationToken::startup_scan().cancel();
    }

    pub fn cancel_change() {
        OperationCancellationToken::startup_change().cancel();
    }

    pub fn scan() -> CoreResult<StartupCatalog> {
        remove_legacy_recovery_data();
        let operation = OperationGuard::start(CoordinatedOperationKind::StartupScan)?;
        let started = Instant::now();
        let cancellation = platform_cancellation(&operation);
        let platform_results = current_platform().scan_startup_sources(&cancellation)?;
        operation.ensure_not_cancelled()?;
        let session =
            catalog_session_from_results(platform_results, started.elapsed().as_millis() as u64);
        log_catalog(&operation, &session.catalog);
        replace_catalog_session(session.clone())?;
        operation.complete();
        Ok(session.catalog)
    }

    pub fn prepare_change(selection: StartupChangeSelection) -> CoreResult<StartupChangePlan> {
        validate_selection(&selection)?;
        let operation = OperationGuard::start(CoordinatedOperationKind::StartupChange)?;
        let session = current_catalog_session()?;
        if session.catalog.scan_id != selection.scan_id {
            return Err(CoreError::invalid_input(
                "startup catalog session has expired",
            ));
        }

        let cancellation = platform_cancellation(&operation);
        let current_results = current_platform().scan_startup_sources(&cancellation)?;
        operation.ensure_not_cancelled()?;
        let current_records = native_records(&current_results);
        let public_artifacts = session
            .catalog
            .artifacts
            .iter()
            .map(|artifact| (artifact.item_id.as_str(), artifact))
            .collect::<BTreeMap<_, _>>();
        let mut items = Vec::new();
        let mut skipped_items = Vec::new();
        for item_id in deduplicated_item_ids(&selection.item_ids) {
            let Some(display_artifact) = public_artifacts.get(item_id.as_str()) else {
                skipped_items.push(skipped_item(
                    item_id,
                    "Unknown startup item",
                    StartupChangeSkipReason::ItemMissing,
                ));
                continue;
            };
            let Some(original) = session.native_records.get(&item_id) else {
                skipped_items.push(skipped_item(
                    item_id,
                    &display_artifact.display_name,
                    StartupChangeSkipReason::CatalogExpired,
                ));
                continue;
            };
            let Some(current) = current_records.get(&item_id) else {
                skipped_items.push(skipped_item(
                    item_id,
                    &display_artifact.display_name,
                    StartupChangeSkipReason::ItemMissing,
                ));
                continue;
            };
            if current.artifact != original.artifact {
                skipped_items.push(skipped_item(
                    item_id,
                    &display_artifact.display_name,
                    StartupChangeSkipReason::ItemChanged,
                ));
                continue;
            }
            if let Some(reason) = preflight_skip_reason(&current.artifact, selection.desired_state)
            {
                skipped_items.push(skipped_item(
                    item_id,
                    &display_artifact.display_name,
                    reason,
                ));
                continue;
            }
            items.push(StartupChangePlanItem {
                item_id: item_id.clone(),
                display_name: display_artifact.display_name.clone(),
                source_kind: display_artifact.source_kind,
                scope: display_artifact.scope,
                previous_state: display_artifact.configured_state,
                desired_state: selection.desired_state,
                warnings: change_warnings(&current.artifact),
                requires_elevation: current.artifact.control_capability
                    == PlatformStartupControlCapability::ElevationRequired,
            });
        }

        let created_at_ms = now_ms();
        let expires_at_ms = created_at_ms.saturating_add(CHANGE_PLAN_TTL_MS);
        let plan_id = change_plan_id(
            &session.catalog.catalog_revision,
            selection.desired_state,
            &items,
            expires_at_ms,
        );
        let public_plan = StartupChangePlan {
            schema_version: STARTUP_CHANGE_PLAN_SCHEMA_VERSION,
            plan_id,
            scan_id: session.catalog.scan_id,
            catalog_revision: session.catalog.catalog_revision,
            created_at_ms,
            expires_at_ms,
            desired_state: selection.desired_state,
            requires_confirmation: !items.is_empty(),
            items,
            skipped_items,
        };
        let targets = public_plan
            .items
            .iter()
            .filter_map(|item| {
                current_records
                    .get(&item.item_id)
                    .map(|record| PendingChangeTarget {
                        item_id: item.item_id.clone(),
                        request: PlatformStartupChangeRequest {
                            provider_item_id: record.artifact.provider_item_id.clone(),
                            source_id: record.source_id.clone(),
                            expected_artifact: record.artifact.clone(),
                            desired_state: selection.desired_state.into(),
                        },
                    })
            })
            .collect();
        replace_change_plan(PendingChangePlan {
            public_plan: public_plan.clone(),
            targets,
        })?;
        log::info!(
            "startup_change_prepared operation_id={} desired_state={:?} target_count={} skipped_count={} expires_in_ms={}",
            operation.id(),
            public_plan.desired_state,
            public_plan.items.len(),
            public_plan.skipped_items.len(),
            CHANGE_PLAN_TTL_MS
        );
        operation.complete();
        Ok(public_plan)
    }

    /// Executes a prepared change and forwards an ephemeral native authorization prompt.
    ///
    /// The localized prompt is UI context only. It is never persisted or logged, and
    /// platforms that do not display a MangoDisk-managed authorization dialog ignore it.
    pub fn execute_change(
        plan_id: String,
        authorization_prompt: Option<&str>,
    ) -> CoreResult<StartupChangeResult> {
        validate_plan_id(&plan_id)?;
        let operation = OperationGuard::start(CoordinatedOperationKind::StartupChange)?;
        let started_at_ms = now_ms();
        let pending = take_change_plan(&plan_id)?;
        if now_ms() > pending.public_plan.expires_at_ms {
            return Err(CoreError::invalid_input("startup change plan has expired"));
        }

        let platform = current_platform();
        let mut results = Vec::with_capacity(pending.targets.len());
        let requests = pending
            .targets
            .iter()
            .map(|target| target.request.clone())
            .collect::<Vec<_>>();
        let elevated_count = requests
            .iter()
            .filter(|request| startup_change_execution_path(request) == "elevated_helper")
            .count();
        let source_count = requests
            .iter()
            .map(|request| request.source_id.as_str())
            .collect::<BTreeSet<_>>()
            .len();
        log::info!(
            "startup_change_execution_started operation_id={} desired_state={:?} target_count={} source_count={} direct_count={} elevated_count={}",
            operation.id(),
            pending.public_plan.desired_state,
            requests.len(),
            source_count,
            requests.len().saturating_sub(elevated_count),
            elevated_count
        );
        if operation.cancelled().load(Ordering::Relaxed) {
            for target in &pending.targets {
                log_startup_change_item_failure(
                    target,
                    operation.id(),
                    "before_native_change",
                    PlatformErrorCode::UserCancelled,
                    None,
                    "not_attempted",
                );
                results.push(StartupChangeItemResult {
                    item_id: target.item_id.clone(),
                    status: StartupChangeOutcomeStatus::Failed,
                    configured_state: target.request.expected_artifact.configured_state.into(),
                    verified: false,
                    failure_reason: Some(StartupChangeFailureReason::UserCancelled),
                });
            }
        } else {
            match platform.change_startup_items(&requests, authorization_prompt) {
                Ok(platform_results) if platform_results.len() == pending.targets.len() => {
                    for (target, platform_result) in pending.targets.iter().zip(platform_results) {
                        results.push(change_item_result(target, platform_result, operation.id()));
                    }
                }
                Ok(_) => {
                    log::warn!(
                        "startup_change_batch_failed operation_id={} reason={:?}",
                        operation.id(),
                        PlatformErrorCode::InvalidData
                    );
                    for target in &pending.targets {
                        results.push(change_item_result(
                            target,
                            Err(PlatformError::new(
                                PlatformErrorCode::InvalidData,
                                "startup platform returned an invalid batch result count",
                            )),
                            operation.id(),
                        ));
                    }
                }
                Err(error) => {
                    let error_code = error.code();
                    log::warn!(
                        "startup_change_batch_failed operation_id={} target_count={} source_count={} direct_count={} elevated_count={} reason={:?} mutation_state={:?} diagnostic_digest={}",
                        operation.id(),
                        requests.len(),
                        source_count,
                        requests.len().saturating_sub(elevated_count),
                        elevated_count,
                        error_code,
                        error.mutation_state(),
                        platform_error_digest(&error)
                    );
                    for target in &pending.targets {
                        results.push(change_item_result(
                            target,
                            Err(error.clone()),
                            operation.id(),
                        ));
                    }
                }
            }
        }

        let changed_count = results
            .iter()
            .filter(|item| item.status == StartupChangeOutcomeStatus::Changed)
            .count() as u64;
        let failed_count = results
            .iter()
            .filter(|item| item.status == StartupChangeOutcomeStatus::Failed)
            .count() as u64;
        append_change_history(&pending.public_plan, &results, started_at_ms, now_ms());

        // The native mutation is already final at this point. A readback failure must not erase
        // its verified result or invite the UI to retry a consumed plan.
        let catalog = refresh_catalog_after_change(&platform, operation.id());
        log::info!(
            "startup_change_completed operation_id={} desired_state={:?} changed_count={} failed_count={} verified_count={} catalog_refreshed={}",
            operation.id(),
            pending.public_plan.desired_state,
            changed_count,
            failed_count,
            results.iter().filter(|item| item.verified).count(),
            catalog.is_some()
        );
        let result = StartupChangeResult {
            plan_id,
            changed_count,
            failed_count,
            items: results,
            catalog,
        };
        operation.complete();
        Ok(result)
    }
}

fn remove_legacy_recovery_data() {
    let result = application_paths().and_then(|paths| {
        let path = paths.data_directory().join("startup").join("recovery.json");
        match fs::remove_file(path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(CoreError::persistence(format!(
                "failed to remove obsolete startup recovery data: {error}"
            ))),
        }
    });
    if let Err(error) = result {
        log::warn!(
            "startup_legacy_recovery_cleanup_failed error_digest={}",
            blake3::hash(error.diagnostic().as_bytes()).to_hex()
        );
    }
}

fn change_item_result(
    target: &PendingChangeTarget,
    result: PlatformResult<PlatformStartupChangeResult>,
    operation_id: u64,
) -> StartupChangeItemResult {
    match result {
        Ok(outcome) if outcome.verified => {
            let status = if outcome.previous_state == outcome.configured_state {
                StartupChangeOutcomeStatus::Unchanged
            } else {
                StartupChangeOutcomeStatus::Changed
            };
            log::info!(
                "startup_change_item_completed operation_id={} item_id={} source_id={} source_kind={:?} scope={:?} control_capability={:?} execution_path={} desired_state={:?} previous_state={:?} configured_state={:?} outcome={:?} verified=true",
                operation_id,
                target.item_id,
                target.request.source_id,
                target.request.expected_artifact.source_kind,
                target.request.expected_artifact.scope,
                target.request.expected_artifact.control_capability,
                startup_change_execution_path(&target.request),
                target.request.desired_state,
                outcome.previous_state,
                outcome.configured_state,
                status
            );
            StartupChangeItemResult {
                item_id: target.item_id.clone(),
                status,
                configured_state: outcome.configured_state.into(),
                verified: true,
                failure_reason: None,
            }
        }
        Ok(outcome) => {
            log_startup_change_item_failure(
                target,
                operation_id,
                "verification",
                PlatformErrorCode::OperationFailed,
                None,
                "may_have_changed",
            );
            StartupChangeItemResult {
                item_id: target.item_id.clone(),
                status: StartupChangeOutcomeStatus::Failed,
                configured_state: outcome.configured_state.into(),
                verified: false,
                failure_reason: Some(StartupChangeFailureReason::VerificationFailed),
            }
        }
        Err(error) => {
            log_startup_change_item_failure(
                target,
                operation_id,
                "native_change",
                error.code(),
                Some(platform_error_digest(&error)),
                match error.mutation_state() {
                    mangodisk_platform::PlatformMutationState::NotAttempted => "not_attempted",
                    mangodisk_platform::PlatformMutationState::MayHaveChanged => "may_have_changed",
                },
            );
            StartupChangeItemResult {
                item_id: target.item_id.clone(),
                status: StartupChangeOutcomeStatus::Failed,
                configured_state: target.request.expected_artifact.configured_state.into(),
                verified: false,
                failure_reason: Some(platform_failure_reason(error.code())),
            }
        }
    }
}

fn log_startup_change_item_failure(
    target: &PendingChangeTarget,
    operation_id: u64,
    failure_stage: &str,
    reason: PlatformErrorCode,
    diagnostic_digest: Option<String>,
    mutation_state: &str,
) {
    log::warn!(
        "startup_change_item_failed operation_id={} item_id={} source_id={} source_kind={:?} scope={:?} control_capability={:?} execution_path={} desired_state={:?} expected_state={:?} failure_stage={} reason={:?} mutation_state={} diagnostic_digest={}",
        operation_id,
        target.item_id,
        target.request.source_id,
        target.request.expected_artifact.source_kind,
        target.request.expected_artifact.scope,
        target.request.expected_artifact.control_capability,
        startup_change_execution_path(&target.request),
        target.request.desired_state,
        target.request.expected_artifact.configured_state,
        failure_stage,
        reason,
        mutation_state,
        diagnostic_digest.as_deref().unwrap_or("none")
    );
}

fn startup_change_execution_path(request: &PlatformStartupChangeRequest) -> &'static str {
    if request.expected_artifact.control_capability
        == PlatformStartupControlCapability::ElevationRequired
        || (request.desired_state == PlatformStartupDesiredState::Removed
            && matches!(
                request.expected_artifact.scope,
                PlatformStartupScope::AllUsers | PlatformStartupScope::Machine
            ))
    {
        "elevated_helper"
    } else {
        "direct"
    }
}

fn platform_error_digest(error: &PlatformError) -> String {
    blake3::hash(error.as_bytes()).to_hex().to_string()
}

fn append_change_history(
    plan: &StartupChangePlan,
    results: &[StartupChangeItemResult],
    started_at_ms: u64,
    finished_at_ms: u64,
) {
    let result_by_id = results
        .iter()
        .map(|result| (result.item_id.as_str(), result))
        .collect::<BTreeMap<_, _>>();
    let items = plan
        .items
        .iter()
        .filter_map(|item| {
            let result = result_by_id.get(item.item_id.as_str())?;
            Some(StartupHistoryItem {
                item_id: item.item_id.clone(),
                display_name: item.display_name.clone(),
                previous_state: history_state_from_configured(item.previous_state),
                desired_state: history_state_from_desired(item.desired_state),
                status: history_status(result.status),
                failure_reason: result.failure_reason.map(failure_reason_code),
            })
        })
        .collect::<Vec<_>>();
    append_history_record(
        format!("startup-change-{}", &plan.plan_id[13..]),
        plan.plan_id.clone(),
        items,
        started_at_ms,
        finished_at_ms,
    );
}

fn append_history_record(
    operation_id: String,
    plan_id: String,
    items: Vec<StartupHistoryItem>,
    started_at_ms: u64,
    finished_at_ms: u64,
) {
    if items.is_empty() {
        return;
    }
    let affected_item_count = items
        .iter()
        .filter(|item| item.status == StartupHistoryItemStatus::Changed)
        .count() as u64;
    let failed_item_count = items
        .iter()
        .filter(|item| item.status == StartupHistoryItemStatus::Failed)
        .count() as u64;
    let record = OperationRecord {
        schema_version: OPERATION_RECORD_SCHEMA_VERSION,
        operation_id,
        category: OperationCategory::StartupManagement,
        started_at_ms,
        finished_at_ms,
        outcome: if failed_item_count == 0 {
            OperationOutcome::Completed
        } else {
            OperationOutcome::CompletedWithWarnings
        },
        dry_run: false,
        selected_item_count: items.len() as u64,
        affected_item_count,
        expected_bytes: 0,
        released_bytes: None,
        released_bytes_is_estimate: false,
        failed_item_count,
        details: OperationDetails::StartupManagement(StartupManagementOperationDetails {
            plan_id: Some(plan_id),
            items,
        }),
    };
    let history_operation_id = record.operation_id.clone();
    if let Err(error) = HistoryService::append(record) {
        log::warn!(
            "startup_history_save_failed history_operation_id={} error_digest={}",
            history_operation_id,
            blake3::hash(error.diagnostic().as_bytes()).to_hex()
        );
    }
}

fn refresh_catalog_after_change(
    platform: &impl StartupPlatform,
    operation_id: u64,
) -> Option<StartupCatalog> {
    // Cancellation applies to the mutation request, not to the readback needed to reconcile UI
    // state after the operating system may already have committed a change.
    let cancellation = PlatformCancellation::new(|| false);
    let refreshed_results = match platform.scan_startup_sources(&cancellation) {
        Ok(results) => results,
        Err(error) => {
            log::warn!(
                "startup_change_catalog_refresh_failed operation_id={} reason={:?} error_digest={}",
                operation_id,
                error.code(),
                blake3::hash(error.as_bytes()).to_hex()
            );
            return None;
        }
    };
    let session = catalog_session_from_results(refreshed_results, 0);
    if let Err(error) = replace_catalog_session(session.clone()) {
        log::warn!(
            "startup_change_catalog_refresh_failed operation_id={} reason={:?} error_digest={}",
            operation_id,
            error.code(),
            blake3::hash(error.diagnostic().as_bytes()).to_hex()
        );
        return None;
    }
    Some(session.catalog)
}

fn history_state_from_configured(state: StartupConfiguredState) -> StartupHistoryState {
    match state {
        StartupConfiguredState::Enabled => StartupHistoryState::Enabled,
        StartupConfiguredState::Disabled => StartupHistoryState::Disabled,
        StartupConfiguredState::Unknown | StartupConfiguredState::NotApplicable => {
            StartupHistoryState::Unknown
        }
    }
}

fn history_state_from_desired(state: StartupDesiredState) -> StartupHistoryState {
    match state {
        StartupDesiredState::Enabled => StartupHistoryState::Enabled,
        StartupDesiredState::Disabled => StartupHistoryState::Disabled,
        StartupDesiredState::Removed => StartupHistoryState::Removed,
    }
}

fn history_status(status: StartupChangeOutcomeStatus) -> StartupHistoryItemStatus {
    match status {
        StartupChangeOutcomeStatus::Changed => StartupHistoryItemStatus::Changed,
        StartupChangeOutcomeStatus::Unchanged => StartupHistoryItemStatus::Unchanged,
        StartupChangeOutcomeStatus::Failed => StartupHistoryItemStatus::Failed,
    }
}

fn failure_reason_code(reason: StartupChangeFailureReason) -> String {
    match reason {
        StartupChangeFailureReason::ItemChanged => "itemChanged",
        StartupChangeFailureReason::PermissionDenied => "permissionDenied",
        StartupChangeFailureReason::UserCancelled => "userCancelled",
        StartupChangeFailureReason::Unsupported => "unsupported",
        StartupChangeFailureReason::VerificationFailed => "verificationFailed",
        StartupChangeFailureReason::PlatformFailure => "platformFailure",
    }
    .to_owned()
}

fn catalog_session_from_results(
    platform_results: Vec<PlatformStartupSourceResult>,
    elapsed_ms: u64,
) -> StartupCatalogSession {
    let native_records = native_records(&platform_results);
    let aggregated = aggregation::aggregate(platform_results);
    let scanned_at_ms = now_ms();
    let scan_id = format!("startup-{}-{}", scanned_at_ms, &aggregated.revision[..16]);
    let catalog = StartupCatalog {
        schema_version: STARTUP_CATALOG_SCHEMA_VERSION,
        scan_id,
        catalog_revision: aggregated.revision,
        scanned_at_ms,
        complete: aggregated.complete,
        artifacts: aggregated.artifacts,
        groups: aggregated.groups,
        coverage: aggregated.coverage,
        summary: aggregated.summary,
        elapsed_ms,
    };
    StartupCatalogSession {
        catalog,
        native_records,
    }
}

fn native_records(
    platform_results: &[PlatformStartupSourceResult],
) -> BTreeMap<String, NativeStartupRecord> {
    let mut records = BTreeMap::new();
    for source in platform_results {
        for artifact in &source.items {
            let item_id =
                aggregation::public_item_id(&source.source_id, &artifact.provider_item_id);
            records.insert(
                item_id,
                NativeStartupRecord {
                    source_id: source.source_id.clone(),
                    artifact: artifact.clone(),
                },
            );
        }
    }
    records
}

fn preflight_skip_reason(
    artifact: &PlatformStartupArtifact,
    desired_state: StartupDesiredState,
) -> Option<StartupChangeSkipReason> {
    if desired_state == StartupDesiredState::Removed {
        return (!super::policy::is_removable_orphan(artifact))
            .then_some(StartupChangeSkipReason::UnsupportedCapability);
    }
    match artifact.control_capability {
        PlatformStartupControlCapability::ElevationRequired
        | PlatformStartupControlCapability::Toggleable => {}
        _ => return Some(StartupChangeSkipReason::UnsupportedCapability),
    }
    if desired_state == StartupDesiredState::Enabled
        && artifact.diagnostics.iter().any(|diagnostic| {
            matches!(
                diagnostic,
                PlatformStartupDiagnosticCode::MissingTarget
                    | PlatformStartupDiagnosticCode::InvalidData
            )
        })
    {
        return Some(StartupChangeSkipReason::TargetUnavailable);
    }
    let desired = match desired_state {
        StartupDesiredState::Enabled => PlatformStartupConfiguredState::Enabled,
        StartupDesiredState::Disabled => PlatformStartupConfiguredState::Disabled,
        StartupDesiredState::Removed => {
            unreachable!("removed startup items return before state comparison")
        }
    };
    match artifact.configured_state {
        PlatformStartupConfiguredState::Unknown | PlatformStartupConfiguredState::NotApplicable => {
            Some(StartupChangeSkipReason::StateUnknown)
        }
        state if state == desired => Some(StartupChangeSkipReason::AlreadyInDesiredState),
        _ => None,
    }
}

fn change_warnings(artifact: &PlatformStartupArtifact) -> Vec<StartupChangeWarning> {
    let mut warnings = Vec::new();
    if artifact.source_kind == mangodisk_platform::PlatformStartupSourceKind::ScheduledTask
        && artifact.triggers.iter().any(|trigger| {
            !matches!(
                trigger,
                mangodisk_platform::PlatformStartupTrigger::Boot
                    | mangodisk_platform::PlatformStartupTrigger::UserLogon
            )
        })
    {
        warnings.push(StartupChangeWarning::AffectsOtherTriggers);
    }
    if matches!(
        artifact.runtime_state,
        PlatformStartupRuntimeState::Running | PlatformStartupRuntimeState::Loaded
    ) {
        warnings.push(StartupChangeWarning::ItemCurrentlyRunning);
    }
    warnings
}

fn skipped_item(
    item_id: String,
    display_name: &str,
    reason: StartupChangeSkipReason,
) -> StartupChangeSkippedItem {
    StartupChangeSkippedItem {
        item_id,
        display_name: display_name.to_owned(),
        reason,
    }
}

fn change_plan_id(
    catalog_revision: &str,
    desired_state: StartupDesiredState,
    items: &[StartupChangePlanItem],
    expires_at_ms: u64,
) -> String {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"mangodisk-startup-change-plan-v1");
    hasher.update(catalog_revision.as_bytes());
    hasher.update(format!("{desired_state:?}").as_bytes());
    hasher.update(&expires_at_ms.to_le_bytes());
    for item in items {
        hasher.update(item.item_id.as_bytes());
    }
    format!("startup-plan-{}", &hasher.finalize().to_hex()[..24])
}

fn platform_failure_reason(code: PlatformErrorCode) -> StartupChangeFailureReason {
    match code {
        PlatformErrorCode::AccessDenied => StartupChangeFailureReason::PermissionDenied,
        PlatformErrorCode::UserCancelled => StartupChangeFailureReason::UserCancelled,
        PlatformErrorCode::ItemChanged => StartupChangeFailureReason::ItemChanged,
        PlatformErrorCode::Unsupported => StartupChangeFailureReason::Unsupported,
        _ => StartupChangeFailureReason::PlatformFailure,
    }
}

fn platform_cancellation(operation: &OperationGuard) -> PlatformCancellation {
    let cancellation_flag = operation.cancellation_flag();
    PlatformCancellation::new(move || cancellation_flag.load(Ordering::Relaxed))
}

fn log_catalog(operation: &OperationGuard, catalog: &StartupCatalog) {
    for source in &catalog.coverage {
        log::info!(
            "startup_source_scanned operation_id={} source_id={} status={:?} reason={:?} item_count={} elapsed_ms={}",
            operation.id(),
            source.source_id,
            source.status,
            source.reason,
            source.item_count,
            source.elapsed_ms
        );
    }
    let incomplete_source_count = catalog
        .coverage
        .iter()
        .filter(|source| source.status != StartupCoverageStatus::Complete)
        .count();
    log::info!(
        "startup_catalog_ready operation_id={} item_count={} group_count={} incomplete_source_count={} complete={} elapsed_ms={}",
        operation.id(),
        catalog.summary.item_count,
        catalog.summary.group_count,
        incomplete_source_count,
        catalog.complete,
        catalog.elapsed_ms
    );
}

fn validate_selection(selection: &StartupChangeSelection) -> CoreResult<()> {
    if selection.scan_id.is_empty() || selection.scan_id.len() > 128 {
        return Err(CoreError::invalid_input(
            "startup scan identifier is invalid",
        ));
    }
    if selection.item_ids.is_empty() || selection.item_ids.len() > MAX_CHANGE_ITEMS {
        return Err(CoreError::invalid_input(
            "startup change selection size is invalid",
        ));
    }
    if selection
        .item_ids
        .iter()
        .any(|item_id| item_id.len() != 64 || !item_id.bytes().all(|byte| byte.is_ascii_hexdigit()))
    {
        return Err(CoreError::invalid_input(
            "startup item identifier is invalid",
        ));
    }
    Ok(())
}

fn validate_plan_id(plan_id: &str) -> CoreResult<()> {
    let digest = plan_id.strip_prefix("startup-plan-").unwrap_or_default();
    if digest.len() != 24 || !digest.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(CoreError::invalid_input(
            "startup change plan identifier is invalid",
        ));
    }
    Ok(())
}

fn deduplicated_item_ids(item_ids: &[String]) -> Vec<String> {
    let mut seen = BTreeSet::new();
    item_ids
        .iter()
        .filter(|item_id| seen.insert((*item_id).clone()))
        .cloned()
        .collect()
}

fn catalog_session() -> &'static Mutex<Option<StartupCatalogSession>> {
    CATALOG_SESSION.get_or_init(|| Mutex::new(None))
}

fn change_plan() -> &'static Mutex<Option<PendingChangePlan>> {
    CHANGE_PLAN.get_or_init(|| Mutex::new(None))
}

fn current_catalog_session() -> CoreResult<StartupCatalogSession> {
    catalog_session()
        .lock()
        .map_err(|_| CoreError::operation_failed("startup catalog session lock is poisoned"))?
        .clone()
        .ok_or_else(|| CoreError::invalid_input("startup catalog session is unavailable"))
}

fn replace_catalog_session(session: StartupCatalogSession) -> CoreResult<()> {
    *catalog_session()
        .lock()
        .map_err(|_| CoreError::operation_failed("startup catalog session lock is poisoned"))? =
        Some(session);
    Ok(())
}

fn replace_change_plan(plan: PendingChangePlan) -> CoreResult<()> {
    *change_plan()
        .lock()
        .map_err(|_| CoreError::operation_failed("startup change plan lock is poisoned"))? =
        Some(plan);
    Ok(())
}

fn take_change_plan(plan_id: &str) -> CoreResult<PendingChangePlan> {
    let mut guard = change_plan()
        .lock()
        .map_err(|_| CoreError::operation_failed("startup change plan lock is poisoned"))?;
    let matches = guard
        .as_ref()
        .is_some_and(|plan| plan.public_plan.plan_id == plan_id);
    if !matches {
        return Err(CoreError::invalid_input(
            "startup change plan is unavailable or has been replaced",
        ));
    }
    guard
        .take()
        .ok_or_else(|| CoreError::invalid_input("startup change plan is unavailable"))
}

impl From<StartupDesiredState> for PlatformStartupDesiredState {
    fn from(value: StartupDesiredState) -> Self {
        match value {
            StartupDesiredState::Enabled => Self::Enabled,
            StartupDesiredState::Disabled => Self::Disabled,
            StartupDesiredState::Removed => Self::Removed,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct FailingScanPlatform;

    impl StartupPlatform for FailingScanPlatform {
        fn scan_startup_sources(
            &self,
            _cancellation: &PlatformCancellation,
        ) -> PlatformResult<Vec<PlatformStartupSourceResult>> {
            Err(PlatformError::new(
                PlatformErrorCode::OperationFailed,
                "test readback failure",
            ))
        }

        fn change_startup_item(
            &self,
            _request: &PlatformStartupChangeRequest,
            _authorization_prompt: Option<&str>,
        ) -> PlatformResult<PlatformStartupChangeResult> {
            unreachable!("the readback regression test does not perform a mutation")
        }
    }

    #[test]
    fn selection_rejects_untrusted_identifiers() {
        let selection = StartupChangeSelection {
            scan_id: "startup-test".to_owned(),
            item_ids: vec!["../../registry".to_owned()],
            desired_state: StartupDesiredState::Disabled,
        };

        assert!(validate_selection(&selection).is_err());
    }

    #[test]
    fn current_user_toggle_preflight_preserves_running_warning() {
        let artifact = test_artifact(
            PlatformStartupControlCapability::Toggleable,
            PlatformStartupConfiguredState::Enabled,
        );

        assert_eq!(
            preflight_skip_reason(&artifact, StartupDesiredState::Disabled),
            None
        );
        assert_eq!(
            change_warnings(&artifact),
            vec![StartupChangeWarning::ItemCurrentlyRunning]
        );
    }

    #[test]
    fn preflight_skips_unknown_but_accepts_privileged_items() {
        let unknown = test_artifact(
            PlatformStartupControlCapability::Toggleable,
            PlatformStartupConfiguredState::Unknown,
        );
        let privileged = test_artifact(
            PlatformStartupControlCapability::ElevationRequired,
            PlatformStartupConfiguredState::Enabled,
        );

        assert_eq!(
            preflight_skip_reason(&unknown, StartupDesiredState::Disabled),
            Some(StartupChangeSkipReason::StateUnknown)
        );
        assert_eq!(
            preflight_skip_reason(&privileged, StartupDesiredState::Disabled),
            None
        );
    }

    #[test]
    fn preflight_does_not_enable_an_item_with_an_unavailable_target() {
        let mut artifact = test_artifact(
            PlatformStartupControlCapability::Toggleable,
            PlatformStartupConfiguredState::Disabled,
        );
        artifact
            .diagnostics
            .push(PlatformStartupDiagnosticCode::MissingTarget);

        assert_eq!(
            preflight_skip_reason(&artifact, StartupDesiredState::Enabled),
            Some(StartupChangeSkipReason::TargetUnavailable)
        );

        artifact.configured_state = PlatformStartupConfiguredState::Enabled;
        assert_eq!(
            preflight_skip_reason(&artifact, StartupDesiredState::Disabled),
            None
        );
    }

    #[test]
    fn preflight_only_removes_allowlisted_orphaned_configurations() {
        let mut launch_agent = test_artifact(
            PlatformStartupControlCapability::Toggleable,
            PlatformStartupConfiguredState::Disabled,
        );
        launch_agent.configuration_path = Some(std::path::PathBuf::from(
            "/Users/fixture/Library/LaunchAgents/com.example.fixture.plist",
        ));

        assert_eq!(
            preflight_skip_reason(&launch_agent, StartupDesiredState::Removed),
            Some(StartupChangeSkipReason::UnsupportedCapability)
        );

        launch_agent
            .diagnostics
            .push(PlatformStartupDiagnosticCode::MissingTarget);

        assert_eq!(
            preflight_skip_reason(&launch_agent, StartupDesiredState::Removed),
            None
        );

        launch_agent.source_kind = mangodisk_platform::PlatformStartupSourceKind::Service;
        assert_eq!(
            preflight_skip_reason(&launch_agent, StartupDesiredState::Removed),
            Some(StartupChangeSkipReason::UnsupportedCapability)
        );
    }

    #[test]
    fn post_change_readback_failure_is_reported_without_failing_the_change() {
        assert!(refresh_catalog_after_change(&FailingScanPlatform, 42).is_none());
    }

    #[test]
    fn execution_path_reports_direct_and_elevated_routes() {
        let direct = PlatformStartupChangeRequest {
            provider_item_id: "test-item".to_owned(),
            source_id: "windows.scheduled_tasks".to_owned(),
            expected_artifact: test_artifact(
                PlatformStartupControlCapability::Toggleable,
                PlatformStartupConfiguredState::Enabled,
            ),
            desired_state: PlatformStartupDesiredState::Disabled,
        };
        assert_eq!(startup_change_execution_path(&direct), "direct");

        let mut privileged = direct.clone();
        privileged.expected_artifact.control_capability =
            PlatformStartupControlCapability::ElevationRequired;
        assert_eq!(
            startup_change_execution_path(&privileged),
            "elevated_helper"
        );

        let mut all_users_removal = direct;
        all_users_removal.expected_artifact.scope = PlatformStartupScope::AllUsers;
        all_users_removal.desired_state = PlatformStartupDesiredState::Removed;
        assert_eq!(
            startup_change_execution_path(&all_users_removal),
            "elevated_helper"
        );
    }

    fn test_artifact(
        capability: PlatformStartupControlCapability,
        configured_state: PlatformStartupConfiguredState,
    ) -> PlatformStartupArtifact {
        use mangodisk_platform::{
            PlatformStartupDiagnosticCode, PlatformStartupIdentityConfidence, PlatformStartupOwner,
            PlatformStartupScope, PlatformStartupSourceKind, PlatformStartupSummarySource,
            PlatformStartupTarget, PlatformStartupTargetKind, PlatformStartupTrigger,
            PlatformStartupTrustState,
        };

        PlatformStartupArtifact {
            provider_item_id: "test-item".to_owned(),
            source_kind: PlatformStartupSourceKind::LaunchAgent,
            scope: PlatformStartupScope::CurrentUser,
            triggers: vec![PlatformStartupTrigger::UserLogon],
            display_name: "Fixture".to_owned(),
            configuration_path: None,
            target: PlatformStartupTarget {
                kind: PlatformStartupTargetKind::Executable,
                identity_key: "fixture".to_owned(),
                path: None,
                executable_name: None,
                arguments: Vec::new(),
            },
            owner: PlatformStartupOwner {
                identity_key: None,
                name: None,
                publisher: None,
                summary: None,
                summary_source: PlatformStartupSummarySource::Unavailable,
                version: None,
                icon_path: None,
                confidence: PlatformStartupIdentityConfidence::Unresolved,
            },
            configured_state,
            runtime_state: PlatformStartupRuntimeState::Running,
            control_capability: capability,
            trust: PlatformStartupTrustState::Unknown,
            modified_at_ms: None,
            diagnostics: Vec::<PlatformStartupDiagnosticCode>::new(),
        }
    }
}

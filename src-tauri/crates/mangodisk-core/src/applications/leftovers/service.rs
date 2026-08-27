use std::{
    collections::{HashMap, HashSet},
    time::Instant,
};

use mangodisk_platform::{current_platform, Platform};

use crate::{
    applications::catalog::ScanContext,
    filesystem::metadata::now_ms,
    history::{
        summarize_deep_cleanup, ApplicationLeftoverOperationDetails, DeepCleanupOperationDetails,
        HistoryService,
    },
    shared::{
        operation::{CoordinatedOperationKind, OperationCancellationToken, OperationGuard},
        CoreError, CoreResult,
    },
};

#[cfg(target_os = "macos")]
use crate::filesystem::permanent_delete::{
    delete_path_permanently, prepare_path_for_permanent_delete,
};

#[cfg(target_os = "macos")]
use crate::applications::catalog::ProcessSnapshot;

use super::models::{
    ApplicationLeftoverActionReason, ApplicationLeftoverActionResult,
    ApplicationLeftoverActionStatus, ApplicationLeftoverPlan, ApplicationLeftoverPlanItem,
    ApplicationLeftoverResult, ApplicationLeftoverScanResult, ApplicationLeftoverSource,
    APPLICATION_LEFTOVER_PLAN_SCHEMA_VERSION, APPLICATION_LEFTOVER_SCAN_SCHEMA_VERSION,
};

#[cfg(target_os = "macos")]
use super::macos;

struct InternalScan {
    result: ApplicationLeftoverScanResult,
    inventory_revision: Option<String>,
}

pub struct ApplicationLeftoverService;

impl ApplicationLeftoverService {
    /// Requests cooperative cancellation of an active leftover cleanup.
    /// Candidate deletion is atomic at the candidate boundary, so the current
    /// candidate may finish while all later candidates are recorded as
    /// cancelled without being touched.
    pub fn cancel() {
        OperationCancellationToken::application_leftover_cleanup().cancel();
    }

    pub fn scan() -> CoreResult<ApplicationLeftoverScanResult> {
        let operation = OperationGuard::start(CoordinatedOperationKind::Applications)?;
        let scan = scan_without_guard()?;
        operation.complete();
        Ok(scan.result)
    }

    pub fn create_plan(
        scan: &ApplicationLeftoverScanResult,
        candidate_ids: &[String],
    ) -> Result<ApplicationLeftoverPlan, String> {
        if scan.schema_version != APPLICATION_LEFTOVER_SCAN_SCHEMA_VERSION {
            return Err(format!(
                "unsupported application leftover scan schema version: {}",
                scan.schema_version
            ));
        }
        if !scan.supported || !scan.inventory_complete {
            return Err("application leftover inventory is incomplete".to_string());
        }
        if candidate_ids.is_empty() {
            return Err("application leftover plan contains no candidates".to_string());
        }
        let selected = candidate_ids.iter().collect::<HashSet<_>>();
        if selected.len() != candidate_ids.len() {
            return Err("application leftover plan contains duplicate candidates".to_string());
        }
        let candidates = scan
            .candidates
            .iter()
            .map(|candidate| (&candidate.candidate_id, candidate))
            .collect::<HashMap<_, _>>();
        let mut items = Vec::with_capacity(candidate_ids.len());
        for candidate_id in candidate_ids {
            let candidate = candidates.get(candidate_id).ok_or_else(|| {
                format!("application leftover candidate is unavailable: {candidate_id}")
            })?;
            items.push(ApplicationLeftoverPlanItem {
                candidate_id: candidate.candidate_id.clone(),
                expected_bytes: candidate.bytes,
                expected_file_count: candidate.file_count,
                expected_snapshot_fingerprint: candidate.snapshot_fingerprint.clone(),
            });
        }
        items.sort_by(|left, right| left.candidate_id.cmp(&right.candidate_id));
        let expected_bytes = items.iter().fold(0_u64, |total, item| {
            total.saturating_add(item.expected_bytes)
        });
        let created_at_ms = now_ms();
        let plan_hash = plan_hash(created_at_ms, &items, expected_bytes);
        Ok(ApplicationLeftoverPlan {
            schema_version: APPLICATION_LEFTOVER_PLAN_SCHEMA_VERSION,
            plan_id: format!("application-leftover-plan-{}", &plan_hash[..16]),
            plan_hash,
            created_at_ms,
            items,
            expected_bytes,
        })
    }

    pub fn validate_plan(plan: &ApplicationLeftoverPlan) -> Result<(), String> {
        if plan.schema_version != APPLICATION_LEFTOVER_PLAN_SCHEMA_VERSION {
            return Err(format!(
                "unsupported application leftover plan schema version: {}",
                plan.schema_version
            ));
        }
        if plan.items.is_empty() {
            return Err("application leftover plan contains no candidates".to_string());
        }
        let unique = plan
            .items
            .iter()
            .map(|item| &item.candidate_id)
            .collect::<HashSet<_>>();
        if unique.len() != plan.items.len() {
            return Err("application leftover plan contains duplicate candidates".to_string());
        }
        let expected_bytes = plan.items.iter().fold(0_u64, |total, item| {
            total.saturating_add(item.expected_bytes)
        });
        let expected_hash = plan_hash(plan.created_at_ms, &plan.items, expected_bytes);
        if expected_bytes != plan.expected_bytes
            || expected_hash != plan.plan_hash
            || plan.plan_id != format!("application-leftover-plan-{}", &expected_hash[..16])
        {
            return Err("application leftover plan integrity validation failed".to_string());
        }
        Ok(())
    }

    pub fn execute(
        plan: ApplicationLeftoverPlan,
        dry_run: bool,
        deep_cleanup_operation_id: String,
    ) -> CoreResult<ApplicationLeftoverResult> {
        Self::validate_plan(&plan)?;
        if deep_cleanup_operation_id.trim().is_empty() {
            return Err(CoreError::invalid_input(
                "deep cleanup operation id must not be empty",
            ));
        }
        // Leftover deletion has its own cancellation identity. The global
        // coordinator still keeps all disk operations mutually exclusive, but
        // the dedicated kind prevents this command from cancelling an
        // unrelated application uninstall or inventory operation.
        let operation =
            OperationGuard::start(CoordinatedOperationKind::ApplicationLeftoverCleanup)?;
        let started_at_ms = now_ms();
        let scan = scan_without_guard()?;
        if !scan.result.supported || !scan.result.inventory_complete {
            return Err(CoreError::operation_failed(
                "application leftover inventory is incomplete",
            ));
        }
        let candidates = scan
            .result
            .candidates
            .into_iter()
            .map(|candidate| (candidate.candidate_id.clone(), candidate))
            .collect::<HashMap<_, _>>();
        let mut actions = Vec::with_capacity(plan.items.len());
        let mut released_bytes = 0_u64;
        let mut affected_item_count = 0_u64;
        let mut failed_item_count = 0_u64;
        let mut cancelled_item_count = 0_u64;

        // Exact companion paths use the verified Container metadata as their
        // ownership anchor. Process companions first and the Container last so
        // every item can still repeat that ownership check immediately before
        // it is moved.
        let mut execution_items = plan.items.iter().collect::<Vec<_>>();
        execution_items.sort_by_key(|item| {
            candidates.get(&item.candidate_id).is_some_and(|candidate| {
                candidate.source == ApplicationLeftoverSource::SandboxContainer
            })
        });
        for item in execution_items {
            let Some(candidate) = candidates.get(&item.candidate_id) else {
                failed_item_count += 1;
                actions.push(missing_action(item));
                continue;
            };
            if operation.ensure_not_cancelled().is_err() {
                cancelled_item_count += 1;
                actions.push(ApplicationLeftoverActionResult {
                    candidate_id: candidate.candidate_id.clone(),
                    application_identifier: candidate.application_identifier.clone(),
                    application_name: candidate.application_name.clone(),
                    status: ApplicationLeftoverActionStatus::Cancelled,
                    reason: None,
                    expected_bytes: candidate.bytes,
                    released_bytes: 0,
                });
                continue;
            }
            if candidate.bytes != item.expected_bytes
                || candidate.file_count != item.expected_file_count
                || candidate.snapshot_fingerprint != item.expected_snapshot_fingerprint
            {
                failed_item_count += 1;
                actions.push(failed_action(
                    candidate,
                    ApplicationLeftoverActionReason::CandidateChanged,
                ));
                continue;
            }
            if dry_run {
                actions.push(ApplicationLeftoverActionResult {
                    candidate_id: candidate.candidate_id.clone(),
                    application_identifier: candidate.application_identifier.clone(),
                    application_name: candidate.application_name.clone(),
                    status: ApplicationLeftoverActionStatus::Previewed,
                    reason: None,
                    expected_bytes: candidate.bytes,
                    released_bytes: 0,
                });
                continue;
            }
            match execute_candidate(candidate, scan.inventory_revision.as_deref()) {
                Ok(()) => {
                    released_bytes = released_bytes.saturating_add(candidate.bytes);
                    affected_item_count += 1;
                    actions.push(ApplicationLeftoverActionResult {
                        candidate_id: candidate.candidate_id.clone(),
                        application_identifier: candidate.application_identifier.clone(),
                        application_name: candidate.application_name.clone(),
                        status: ApplicationLeftoverActionStatus::Completed,
                        reason: None,
                        expected_bytes: candidate.bytes,
                        released_bytes: candidate.bytes,
                    });
                }
                Err((reason, partially_released_bytes)) => {
                    released_bytes = released_bytes.saturating_add(partially_released_bytes);
                    failed_item_count += 1;
                    let mut action = failed_action(candidate, reason);
                    action.released_bytes = partially_released_bytes;
                    actions.push(action);
                }
            }
        }

        let finished_at_ms = now_ms();
        let record = summarize_deep_cleanup(
            deep_cleanup_operation_id,
            started_at_ms,
            finished_at_ms,
            dry_run,
            DeepCleanupOperationDetails {
                cleanup: None,
                application_leftovers: Some(ApplicationLeftoverOperationDetails {
                    candidate_ids: plan
                        .items
                        .iter()
                        .map(|item| item.candidate_id.clone())
                        .collect(),
                    expected_bytes: plan.expected_bytes,
                    actions: actions.clone(),
                }),
            },
        );
        let history_saved = match HistoryService::upsert_deep_cleanup(record) {
            Ok(()) => true,
            Err(error) => {
                log::warn!(
                    "application_leftover_history_save_failed operation_id={} error_digest={}",
                    operation.id(),
                    blake3::hash(error.diagnostic().as_bytes()).to_hex()
                );
                false
            }
        };
        log::info!(
            "application_leftover_execution_finished operation_id={} candidate_count={} affected_count={} failed_count={} cancelled_count={} expected_bytes={} released_bytes={} dry_run={}",
            operation.id(),
            plan.items.len(),
            affected_item_count,
            failed_item_count,
            cancelled_item_count,
            plan.expected_bytes,
            released_bytes,
            dry_run
        );
        operation.complete();
        Ok(ApplicationLeftoverResult {
            plan_id: plan.plan_id,
            expected_bytes: plan.expected_bytes,
            released_bytes,
            affected_item_count,
            failed_item_count,
            dry_run,
            actions,
            history_saved,
        })
    }

    pub fn execute_reviewed(
        reviewed_items: Vec<ApplicationLeftoverPlanItem>,
        dry_run: bool,
        deep_cleanup_operation_id: String,
    ) -> CoreResult<ApplicationLeftoverResult> {
        let plan = create_plan_from_reviewed_items(reviewed_items)?;
        Self::execute(plan, dry_run, deep_cleanup_operation_id)
    }
}

fn scan_without_guard() -> Result<InternalScan, String> {
    let started = Instant::now();
    let revision_before = current_platform().system_inventory_revision().ok();
    let context = ScanContext::capture();
    #[cfg(target_os = "macos")]
    let (processes, processes_complete) = match ProcessSnapshot::capture() {
        Ok(processes) => (processes, true),
        Err(error) => {
            log::warn!(
                "application_leftover_process_snapshot_failed error_digest={}",
                blake3::hash(error.as_bytes()).to_hex()
            );
            (ProcessSnapshot::default(), false)
        }
    };
    #[cfg(not(target_os = "macos"))]
    let processes_complete = true;
    let revision_after = current_platform().system_inventory_revision().ok();
    let stable_inventory = revision_before.is_some() && revision_before == revision_after;
    let inventory_complete = context.inventory.application_inventory_complete()
        && stable_inventory
        && processes_complete;

    #[cfg(target_os = "macos")]
    let (supported, mut candidates, skipped_count, access_limited) = if inventory_complete {
        let (candidates, skipped_count, access_limited) =
            apply_macos_access_policy(macos::scan_candidates(&context.inventory, &processes)?);
        (true, candidates, skipped_count, access_limited)
    } else {
        (true, Vec::new(), 0, false)
    };

    #[cfg(not(target_os = "macos"))]
    let (supported, mut candidates, skipped_count, access_limited): (
        bool,
        Vec<super::models::ApplicationLeftoverCandidate>,
        u64,
        bool,
    ) = (false, Vec::new(), 0, false);

    /*
     * Leftovers join the interactive smart recommendation only after the
     * complete application and filesystem inventories agree that every
     * candidate has no installed owner. A limited scan keeps candidates
     * unselected instead of turning incomplete evidence into cleanup intent.
     */
    apply_default_selection(
        &mut candidates,
        supported && inventory_complete && !access_limited,
    );

    let total_bytes = candidates.iter().fold(0_u64, |total, candidate| {
        total.saturating_add(candidate.bytes)
    });
    let total_file_count = candidates.iter().fold(0_u64, |total, candidate| {
        total.saturating_add(candidate.file_count)
    });
    let result = ApplicationLeftoverScanResult {
        schema_version: APPLICATION_LEFTOVER_SCAN_SCHEMA_VERSION,
        scanned_at_ms: now_ms(),
        supported,
        inventory_complete: inventory_complete && !access_limited,
        access_limited,
        candidates,
        total_bytes,
        total_file_count,
        skipped_count,
        elapsed_ms: started.elapsed().as_millis() as u64,
    };
    log::info!(
        "application_leftover_scan_finished supported={} inventory_complete={} access_limited={} candidate_count={} total_bytes={} skipped_count={} elapsed_ms={}",
        result.supported,
        result.inventory_complete,
        result.access_limited,
        result.candidates.len(),
        result.total_bytes,
        result.skipped_count,
        result.elapsed_ms
    );
    Ok(InternalScan {
        result,
        inventory_revision: revision_after,
    })
}

fn apply_default_selection(
    candidates: &mut [super::models::ApplicationLeftoverCandidate],
    recommendation_evidence_complete: bool,
) {
    for candidate in candidates {
        candidate.default_selected = recommendation_evidence_complete
            && matches!(
                candidate.confidence,
                super::models::ApplicationLeftoverConfidence::High
            );
    }
}

#[cfg(target_os = "macos")]
fn apply_macos_access_policy(
    scan: macos::CandidateScan,
) -> (Vec<super::models::ApplicationLeftoverCandidate>, u64, bool) {
    if scan.access_denied_count == 0 {
        return (scan.candidates, scan.skipped_count, false);
    }
    // A partial container inventory cannot prove that an apparent orphan has
    // no installed owner. Discard every candidate instead of presenting a
    // misleading subset until the user grants Full Disk Access.
    log::warn!(
        "application_leftover_access_limited access_denied_count={}",
        scan.access_denied_count
    );
    (Vec::new(), scan.skipped_count, true)
}

#[cfg(target_os = "macos")]
fn execute_candidate(
    candidate: &super::models::ApplicationLeftoverCandidate,
    expected_inventory_revision: Option<&str>,
) -> Result<(), (ApplicationLeftoverActionReason, u64)> {
    let current_revision = current_platform()
        .system_inventory_revision()
        .map_err(|_| (ApplicationLeftoverActionReason::OwnerReappeared, 0))?;
    if Some(current_revision.as_str()) != expected_inventory_revision {
        return Err((ApplicationLeftoverActionReason::OwnerReappeared, 0));
    }
    let processes = ProcessSnapshot::capture()
        .map_err(|_| (ApplicationLeftoverActionReason::ApplicationRunning, 0))?;
    if !processes
        .matching_processes(&macos::candidate_process_names(candidate))
        .is_empty()
    {
        return Err((ApplicationLeftoverActionReason::ApplicationRunning, 0));
    }
    let prepared = prepare_path_for_permanent_delete(std::path::Path::new(&candidate.path))
        .map_err(|_| (ApplicationLeftoverActionReason::CandidateChanged, 0))?;
    macos::revalidate_candidate(candidate)
        .map_err(|_| (ApplicationLeftoverActionReason::CandidateChanged, 0))?;
    delete_path_permanently(prepared, candidate.bytes, candidate.file_count).map_err(|error| {
        log::warn!(
            "application_leftover_permanent_delete_failed candidate_id={} path={} partial={} released_bytes={} error_digest={}",
            candidate.candidate_id,
            crate::filesystem::metadata::diagnostic_path(std::path::Path::new(&candidate.path)),
            error.is_partial(),
            error.released_bytes(),
            blake3::hash(error.to_string().as_bytes()).to_hex()
        );
        (
            ApplicationLeftoverActionReason::PermanentDeleteFailed,
            error.released_bytes(),
        )
    })
}

#[cfg(not(target_os = "macos"))]
fn execute_candidate(
    _candidate: &super::models::ApplicationLeftoverCandidate,
    _expected_inventory_revision: Option<&str>,
) -> Result<(), (ApplicationLeftoverActionReason, u64)> {
    Err((ApplicationLeftoverActionReason::OwnerReappeared, 0))
}

fn missing_action(item: &ApplicationLeftoverPlanItem) -> ApplicationLeftoverActionResult {
    ApplicationLeftoverActionResult {
        candidate_id: item.candidate_id.clone(),
        application_identifier: String::new(),
        application_name: String::new(),
        status: ApplicationLeftoverActionStatus::Failed,
        reason: Some(ApplicationLeftoverActionReason::CandidateChanged),
        expected_bytes: item.expected_bytes,
        released_bytes: 0,
    }
}

fn failed_action(
    candidate: &super::models::ApplicationLeftoverCandidate,
    reason: ApplicationLeftoverActionReason,
) -> ApplicationLeftoverActionResult {
    ApplicationLeftoverActionResult {
        candidate_id: candidate.candidate_id.clone(),
        application_identifier: candidate.application_identifier.clone(),
        application_name: candidate.application_name.clone(),
        status: ApplicationLeftoverActionStatus::Failed,
        reason: Some(reason),
        expected_bytes: candidate.bytes,
        released_bytes: 0,
    }
}

fn plan_hash(
    created_at_ms: u64,
    items: &[ApplicationLeftoverPlanItem],
    expected_bytes: u64,
) -> String {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"mangodisk-application-leftover-plan-v1");
    hasher.update(&created_at_ms.to_le_bytes());
    hasher.update(&expected_bytes.to_le_bytes());
    for item in items {
        hasher.update(item.candidate_id.as_bytes());
        hasher.update(&item.expected_bytes.to_le_bytes());
        hasher.update(&item.expected_file_count.to_le_bytes());
        hasher.update(item.expected_snapshot_fingerprint.as_bytes());
    }
    hasher.finalize().to_hex().to_string()
}

fn create_plan_from_reviewed_items(
    mut items: Vec<ApplicationLeftoverPlanItem>,
) -> Result<ApplicationLeftoverPlan, String> {
    if items.is_empty() {
        return Err("application leftover plan contains no candidates".to_string());
    }
    let unique = items
        .iter()
        .map(|item| &item.candidate_id)
        .collect::<HashSet<_>>();
    if unique.len() != items.len() {
        return Err("application leftover plan contains duplicate candidates".to_string());
    }
    if items.iter().any(|item| {
        item.candidate_id.is_empty()
            || item.expected_snapshot_fingerprint.is_empty()
            || item.expected_file_count == 0
    }) {
        return Err("application leftover plan contains an invalid reviewed candidate".to_string());
    }
    items.sort_by(|left, right| left.candidate_id.cmp(&right.candidate_id));
    let expected_bytes = items.iter().fold(0_u64, |total, item| {
        total.saturating_add(item.expected_bytes)
    });
    let created_at_ms = now_ms();
    let plan_hash = plan_hash(created_at_ms, &items, expected_bytes);
    Ok(ApplicationLeftoverPlan {
        schema_version: APPLICATION_LEFTOVER_PLAN_SCHEMA_VERSION,
        plan_id: format!("application-leftover-plan-{}", &plan_hash[..16]),
        plan_hash,
        created_at_ms,
        items,
        expected_bytes,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::applications::leftovers::models::{
        ApplicationLeftoverCandidate, ApplicationLeftoverConfidence, ApplicationLeftoverEvidence,
        ApplicationLeftoverSource,
    };

    fn scan_fixture() -> ApplicationLeftoverScanResult {
        ApplicationLeftoverScanResult {
            schema_version: APPLICATION_LEFTOVER_SCAN_SCHEMA_VERSION,
            scanned_at_ms: 1,
            supported: true,
            inventory_complete: true,
            access_limited: false,
            candidates: vec![ApplicationLeftoverCandidate {
                candidate_id: "leftover-fixture".to_string(),
                application_identifier: "com.example.removed".to_string(),
                application_name: "Removed".to_string(),
                source: ApplicationLeftoverSource::SandboxContainer,
                path: "/fixture/Containers/com.example.removed".to_string(),
                bytes: 42,
                file_count: 2,
                modified_at_ms: Some(1),
                confidence: ApplicationLeftoverConfidence::High,
                default_selected: true,
                evidence: vec![ApplicationLeftoverEvidence::ContainerMetadataVerified],
                snapshot_fingerprint: "fingerprint".to_string(),
            }],
            total_bytes: 42,
            total_file_count: 2,
            skipped_count: 0,
            elapsed_ms: 1,
        }
    }

    #[test]
    fn plan_is_versioned_and_integrity_checked() {
        let plan = ApplicationLeftoverService::create_plan(
            &scan_fixture(),
            &["leftover-fixture".to_string()],
        )
        .expect("the plan should be created");

        ApplicationLeftoverService::validate_plan(&plan)
            .expect("the untouched plan should remain valid");
        let mut modified = plan;
        modified.expected_bytes += 1;
        assert!(ApplicationLeftoverService::validate_plan(&modified).is_err());
    }

    #[test]
    fn scan_protocol_reports_access_state_in_schema_two() {
        let value = serde_json::to_value(scan_fixture())
            .expect("the application leftover scan should serialize");

        assert_eq!(value["schemaVersion"], 2);
        assert_eq!(value["accessLimited"], false);
        assert_eq!(value["inventoryComplete"], true);
    }

    #[test]
    fn plan_rejects_unknown_and_duplicate_candidates() {
        let scan = scan_fixture();
        assert!(ApplicationLeftoverService::create_plan(&scan, &["unknown".to_string()]).is_err());
        assert!(ApplicationLeftoverService::create_plan(
            &scan,
            &[
                "leftover-fixture".to_string(),
                "leftover-fixture".to_string(),
            ],
        )
        .is_err());
    }

    #[test]
    fn leftover_cancellation_does_not_cancel_unrelated_application_operations() {
        let _operation_lock = crate::shared::operation::test_operation_lock();
        {
            let leftover =
                OperationGuard::start(CoordinatedOperationKind::ApplicationLeftoverCleanup)
                    .expect("the isolated leftover cleanup must start");

            ApplicationLeftoverService::cancel();

            assert!(
                leftover.ensure_not_cancelled().is_err(),
                "the cancellation request must reach leftover cleanup"
            );
        }
        {
            let unrelated = OperationGuard::start(CoordinatedOperationKind::Applications)
                .expect("the unrelated application operation must start");

            ApplicationLeftoverService::cancel();

            assert!(
                unrelated.ensure_not_cancelled().is_ok(),
                "leftover cancellation must not affect application uninstall or inventory"
            );
            unrelated.complete();
        }
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn denied_container_access_discards_partial_candidates() {
        let scan = scan_fixture();
        assert!(!scan.candidates.is_empty());
        let (candidates, skipped_count, access_limited) =
            apply_macos_access_policy(macos::CandidateScan {
                candidates: scan.candidates,
                skipped_count: 7,
                access_denied_count: 1,
            });

        assert!(candidates.is_empty());
        assert_eq!(skipped_count, 7);
        assert!(access_limited);
    }

    #[test]
    fn default_selection_requires_complete_recommendation_evidence() {
        let mut candidates = scan_fixture().candidates;

        apply_default_selection(&mut candidates, false);
        assert!(candidates
            .iter()
            .all(|candidate| !candidate.default_selected));

        apply_default_selection(&mut candidates, true);
        assert!(candidates
            .iter()
            .all(|candidate| candidate.default_selected));
    }

    #[test]
    fn reviewed_items_preserve_the_original_candidate_snapshot() {
        let candidate = &scan_fixture().candidates[0];
        let plan = create_plan_from_reviewed_items(vec![ApplicationLeftoverPlanItem {
            candidate_id: candidate.candidate_id.clone(),
            expected_bytes: candidate.bytes,
            expected_file_count: candidate.file_count,
            expected_snapshot_fingerprint: candidate.snapshot_fingerprint.clone(),
        }])
        .expect("a reviewed candidate should create an execution plan");

        assert_eq!(plan.items[0].candidate_id, candidate.candidate_id);
        assert_eq!(
            plan.items[0].expected_snapshot_fingerprint,
            candidate.snapshot_fingerprint
        );
        ApplicationLeftoverService::validate_plan(&plan)
            .expect("the reviewed plan should pass integrity validation");
    }
}

use std::{
    collections::HashSet,
    path::Path,
    time::{Duration, Instant},
};

use sha2::{Digest, Sha256};

use crate::{
    cleanup::{
        CleanupActionKind, CleanupActionResult, CleanupApplicationCloseRequest,
        CleanupExecutionProgress, CleanupExecutionRuleResult, CleanupExecutionStage,
        CleanupRequest, CleanupResult,
    },
    history::{summarize_deep_cleanup, CleanupOperationDetails, DeepCleanupOperationDetails},
};

use crate::{
    applications::{
        catalog::{ProcessSnapshot, ScanContext},
        process_control::{close_resolved_applications, ResolvedApplicationCloseTarget},
    },
    cleanup::applicability::{evaluate_rule, rule_requires_process, Applicability},
    cleanup::cleaners,
    cleanup::rule_execution::{
        cancelled_action, execute_rule, measure_owned_rule, DeleteStats, RuleExecutionContext,
    },
    cleanup::rules::{compile_scan_plan, registry},
    cleanup::source_selection::SourceSelectionPolicy,
    filesystem::metadata::{display_path, now_ms},
    history::HistoryService,
    shared::{
        operation::{CoordinatedOperationKind, OperationCancellationToken, OperationGuard},
        CoreError, CoreResult,
    },
};

#[cfg(test)]
use std::fs;

#[cfg(test)]
use crate::cleanup::{
    rule_execution::{
        delete_root_contents, delete_root_contents_with_progress, validate_rule_root,
        DeleteRootContentsPolicy,
    },
    rules::{CompiledRule, MatcherSpec},
};

#[cfg(test)]
use crate::filesystem::permanent_delete::{
    delete_directory_contents_permanently_with_cancellation_serial,
    physical_path_identity_snapshot, prepare_path_for_permanent_delete,
};

pub struct CleanupService;

const ITEM_PROGRESS_INTERVAL: Duration = Duration::from_millis(80);

struct ExecutionProgressReporter<F> {
    handler: F,
    started: Instant,
    planned_rule_ids: Vec<String>,
    total_rule_count: u64,
    validated_rule_count: u64,
    completed_rule_count: u64,
    checked_item_count: u64,
    checked_bytes: u64,
    affected_item_count: u64,
    released_bytes: u64,
    current_item_path: Option<String>,
    current_rule_affected_item_count: u64,
    current_rule_released_bytes: u64,
    completed_rule_results: Vec<CleanupExecutionRuleResult>,
    last_item_emit: Option<Instant>,
}

impl<F> ExecutionProgressReporter<F>
where
    F: FnMut(CleanupExecutionProgress),
{
    fn new(planned_rule_ids: Vec<String>, handler: F) -> Self {
        let total_rule_count = planned_rule_ids.len();
        Self {
            handler,
            started: Instant::now(),
            planned_rule_ids,
            total_rule_count: total_rule_count as u64,
            validated_rule_count: 0,
            completed_rule_count: 0,
            checked_item_count: 0,
            checked_bytes: 0,
            affected_item_count: 0,
            released_bytes: 0,
            current_item_path: None,
            current_rule_affected_item_count: 0,
            current_rule_released_bytes: 0,
            completed_rule_results: Vec::with_capacity(total_rule_count),
            last_item_emit: None,
        }
    }

    fn emit(&mut self, stage: CleanupExecutionStage, current_rule_id: Option<&str>) {
        (self.handler)(CleanupExecutionProgress {
            stage,
            planned_rule_ids: self.planned_rule_ids.clone(),
            current_rule_id: current_rule_id.map(str::to_owned),
            current_item_path: self.current_item_path.clone(),
            current_rule_affected_item_count: self.current_rule_affected_item_count,
            current_rule_released_bytes: self.current_rule_released_bytes,
            completed_rule_results: self.completed_rule_results.clone(),
            validated_rule_count: self.validated_rule_count,
            completed_rule_count: self.completed_rule_count,
            total_rule_count: self.total_rule_count,
            checked_item_count: self.checked_item_count,
            checked_bytes: self.checked_bytes,
            affected_item_count: self
                .affected_item_count
                .saturating_add(self.current_rule_affected_item_count),
            released_bytes: self
                .released_bytes
                .saturating_add(self.current_rule_released_bytes),
            elapsed_ms: self.started.elapsed().as_millis() as u64,
        });
    }

    fn record_validation(&mut self, item_count: u64, bytes: u64) {
        self.validated_rule_count = self
            .validated_rule_count
            .saturating_add(1)
            .min(self.total_rule_count);
        self.checked_item_count = self.checked_item_count.saturating_add(item_count);
        self.checked_bytes = self.checked_bytes.saturating_add(bytes);
    }

    fn finish_validation(&mut self) {
        // When a measurement stage exists, rules without a generic filesystem
        // measurement are already ready to execute or own their specialized
        // validation. Complete the stage without presenting those rules as
        // missing file checks.
        self.validated_rule_count = self.total_rule_count;
    }

    fn begin_rule(&mut self) {
        self.current_item_path = None;
        self.current_rule_affected_item_count = 0;
        self.current_rule_released_bytes = 0;
        self.last_item_emit = None;
    }

    fn record_item(&mut self, rule_id: &str, path: &Path, stats: &DeleteStats) {
        self.current_rule_affected_item_count = stats.affected_item_count;
        self.current_rule_released_bytes = stats.deleted_bytes;
        let now = Instant::now();
        if self
            .last_item_emit
            .is_some_and(|last_emit| now.duration_since(last_emit) < ITEM_PROGRESS_INTERVAL)
        {
            return;
        }
        // Path conversion allocates on both Windows and macOS. Do it only for
        // snapshots that will actually cross the adapter boundary; deletion
        // may otherwise pay this cost tens of thousands of times per rule.
        self.current_item_path = Some(display_path(path));
        self.last_item_emit = Some(now);
        self.emit(CleanupExecutionStage::Cleaning, Some(rule_id));
    }

    fn record_action(&mut self, action: &CleanupActionResult) {
        self.current_item_path = None;
        self.current_rule_affected_item_count = 0;
        self.current_rule_released_bytes = 0;
        self.last_item_emit = None;
        self.completed_rule_count = self
            .completed_rule_count
            .saturating_add(1)
            .min(self.total_rule_count);
        self.affected_item_count = self
            .affected_item_count
            .saturating_add(action.affected_item_count);
        self.released_bytes = self.released_bytes.saturating_add(action.released_bytes);
        self.completed_rule_results
            .push(CleanupExecutionRuleResult {
                rule_id: action.rule_id.clone(),
                status: action.status,
                affected_item_count: action.affected_item_count,
                released_bytes: action.released_bytes,
            });
    }
}

impl CleanupService {
    /// Closes applications referenced by trusted declarative cleanup rules.
    ///
    /// The WebView supplies only stable rule IDs. Process aliases remain in
    /// the validated rule catalog so an adapter cannot turn this workflow into
    /// an arbitrary process termination primitive.
    pub fn close_applications(
        request: CleanupApplicationCloseRequest,
    ) -> CoreResult<crate::ApplicationCloseBatchResult> {
        let operation = OperationGuard::start(CoordinatedOperationKind::ApplicationClose)?;
        let selected = request.rule_ids.iter().cloned().collect::<HashSet<_>>();
        if selected.is_empty() || selected.len() != request.rule_ids.len() {
            return Err(CoreError::invalid_input(
                "the cleanup application close selection is invalid",
            ));
        }
        let rules = registry()?;
        let mut targets = Vec::with_capacity(request.rule_ids.len());
        for rule_id in &request.rule_ids {
            let rule = rules
                .iter()
                .find(|rule| rule.id == rule_id.as_str())
                .ok_or_else(|| {
                    CoreError::invalid_input(
                        "the cleanup application close request contains an unknown rule",
                    )
                })?;
            if rule.required_stopped_processes.is_empty() {
                return Err(CoreError::invalid_input(
                    "the cleanup rule does not define a close requirement",
                ));
            }
            targets.push(ResolvedApplicationCloseTarget {
                target_id: rule.id.to_string(),
                executable_names: rule.required_stopped_processes.clone(),
                executable_paths: Vec::new(),
            });
        }
        let result = close_resolved_applications(targets, request.mode)?;
        operation.complete();
        Ok(result)
    }

    /// Requests cooperative cancellation of the active cleanup execution.
    ///
    /// Files that were already removed remain reflected in the result and
    /// history. Long-running platform commands may finish their current native
    /// step before observing the token, but no later cleanup rule is started.
    pub fn cancel() {
        OperationCancellationToken::cleanup().cancel();
    }

    pub fn execute(request: CleanupRequest) -> CoreResult<CleanupResult> {
        Self::execute_with_progress(request, |_| {})
    }

    pub fn execute_with_progress<F>(
        request: CleanupRequest,
        progress: F,
    ) -> CoreResult<CleanupResult>
    where
        F: FnMut(CleanupExecutionProgress),
    {
        let operation_id = format!("deep-cleanup-{}", now_ms());
        Self::execute_deep_cleanup_step_with_progress(request, operation_id, progress)
    }

    pub fn execute_deep_cleanup_step(
        request: CleanupRequest,
        deep_cleanup_operation_id: String,
    ) -> CoreResult<CleanupResult> {
        Self::execute_deep_cleanup_step_with_progress(request, deep_cleanup_operation_id, |_| {})
    }

    pub fn execute_deep_cleanup_step_with_progress<F>(
        request: CleanupRequest,
        deep_cleanup_operation_id: String,
        progress: F,
    ) -> CoreResult<CleanupResult>
    where
        F: FnMut(CleanupExecutionProgress),
    {
        Self::execute_deep_cleanup_step_with_scope(
            request,
            deep_cleanup_operation_id,
            false,
            progress,
        )
    }

    pub fn execute_deep_cleanup_step_with_selected_volumes_and_progress<F>(
        mut request: CleanupRequest,
        deep_cleanup_operation_id: String,
        progress: F,
    ) -> CoreResult<CleanupResult>
    where
        F: FnMut(CleanupExecutionProgress),
    {
        request.project_roots = super::volume_scope::resolve_selected_volume_roots(
            &request.project_roots,
            super::volume_scope::SelectedVolumeScopeOperation::Cleanup,
        )?;
        Self::execute_deep_cleanup_step_with_scope(
            request,
            deep_cleanup_operation_id,
            true,
            progress,
        )
    }

    fn execute_deep_cleanup_step_with_scope<F>(
        request: CleanupRequest,
        deep_cleanup_operation_id: String,
        selected_volume_scope: bool,
        progress: F,
    ) -> CoreResult<CleanupResult>
    where
        F: FnMut(CleanupExecutionProgress),
    {
        let operation = OperationGuard::start(CoordinatedOperationKind::Cleanup)?;
        if request.rule_ids.is_empty() {
            return Err(CoreError::invalid_input(
                "at least one cleanup rule must be selected",
            ));
        }
        if deep_cleanup_operation_id.trim().is_empty() {
            return Err(CoreError::invalid_input(
                "deep cleanup operation id must not be empty",
            ));
        }
        let started_at_ms = now_ms();
        let started = Instant::now();
        let selected = request.rule_ids.iter().cloned().collect::<HashSet<_>>();
        if selected.len() != request.rule_ids.len() {
            return Err(CoreError::invalid_input(
                "the cleanup plan contains duplicate rules",
            ));
        }
        let source_selection_policy =
            SourceSelectionPolicy::from_request(&selected, &request.source_selections)?;
        let rules = registry()?;
        if selected
            .iter()
            .any(|id| !rules.iter().any(|rule| rule.id == id.as_str()) && !cleaners::contains(id))
        {
            return Err(CoreError::invalid_input(
                "the cleanup plan contains an unknown rule",
            ));
        }
        let cleaner_rule_ids = request
            .rule_ids
            .iter()
            .filter(|id| cleaners::contains(id))
            .cloned()
            .collect::<Vec<_>>();
        // The execution pipeline validates and runs declarative filesystem
        // rules first, then specialized cleaners. Publish that deterministic
        // queue so adapters never mistake selection order for execution order.
        let mut planned_rule_ids = rules
            .iter()
            .filter(|rule| selected.contains(rule.id.as_str()))
            .map(|rule| rule.id.clone())
            .collect::<Vec<_>>();
        planned_rule_ids.extend(cleaners::execution_rule_ids(&cleaner_rule_ids));
        if planned_rule_ids.len() != request.rule_ids.len() {
            return Err(CoreError::operation_failed(
                "cleanup execution planning did not preserve every selected rule",
            ));
        }
        let mut progress = ExecutionProgressReporter::new(planned_rule_ids, progress);
        let validation_started = Instant::now();
        let applicability_context = ScanContext::capture();
        let applicability_process_snapshot = if rules.iter().any(rule_requires_process) {
            match ProcessSnapshot::capture() {
                Ok(snapshot) => Some(snapshot),
                Err(error) => {
                    log::warn!(
                        "cleanup_applicability_process_snapshot_failed error_digest={}",
                        blake3::hash(error.as_bytes()).to_hex()
                    );
                    None
                }
            }
        } else {
            None
        };
        let availability = rules
            .iter()
            .map(|rule| {
                evaluate_rule(
                    &applicability_context.inventory,
                    rule,
                    applicability_process_snapshot.as_ref(),
                ) != Applicability::NotApplicable
            })
            .collect::<Vec<_>>();
        // Recompile ownership for every applicable rule, not only selected
        // rules. An unselected child rule still protects its files from a
        // selected parent rule.
        let ownership_plan = compile_scan_plan(rules, &availability, &[])?;
        let selected_rule_indices = ownership_plan
            .rules
            .iter()
            .enumerate()
            .filter(|(_, rule)| selected.contains(rule.id.as_str()))
            .map(|(rule_index, _)| rule_index)
            .collect::<Vec<_>>();
        // A dry run has no deletion traversal from which to derive an estimate.
        // Source-scoped execution must also prove every untrusted UI path is a
        // live source before mutation. Ordinary whole-rule cleanup can instead
        // validate and account for each candidate during its deletion pass.
        let measured_rule_count = selected_rule_indices
            .iter()
            .filter(|rule_index| {
                requires_preflight_measurement(
                    request.dry_run,
                    source_selection_policy
                        .scope(&ownership_plan.rules[**rule_index].id)
                        .is_some(),
                )
            })
            .count();
        progress.emit(preparation_stage(measured_rule_count), None);
        let mut measured_rules = Vec::with_capacity(selected_rule_indices.len());
        for rule_index in &selected_rule_indices {
            operation.ensure_not_cancelled()?;
            let rule = &ownership_plan.rules[*rule_index];
            let requires_measurement = requires_preflight_measurement(
                request.dry_run,
                source_selection_policy.scope(&rule.id).is_some(),
            );
            if !requires_measurement {
                measured_rules.push(None);
                continue;
            }
            progress.emit(CleanupExecutionStage::Validating, Some(&rule.id));
            let measured = measure_owned_rule(
                &ownership_plan,
                *rule_index,
                source_selection_policy.scope(&rule.id),
            )?;
            progress.record_validation(measured.file_count, measured.bytes);
            progress.emit(CleanupExecutionStage::Validating, Some(&rule.id));
            measured_rules.push(Some(measured));
        }
        // No filesystem mutation has happened yet. Even executions that skip
        // measurement may still be cancelled cleanly before the first rule.
        operation.ensure_not_cancelled()?;
        // Capture one process snapshot immediately before deletion. Rules
        // share the system query instead of trusting process state from the
        // earlier result screen or a potentially long scoped measurement.
        let process_snapshot = if selected_rule_indices
            .iter()
            .any(|rule_index| ownership_plan.rules[*rule_index].requires_app_close())
        {
            ProcessSnapshot::capture()
                .map_err(|error| format!("failed to verify running applications: {error}"))?
        } else {
            ProcessSnapshot::default()
        };
        let validation_elapsed_ms = validation_started.elapsed().as_millis() as u64;
        log::info!(
            "cleanup_started operation_id={} ownership_plan_id={} rule_count={} filesystem_rule_count={} cleaner_rule_count={} measured_rule_count={} validation_elapsed_ms={} rule_ids={:?} dry_run={}",
            operation.id(),
            ownership_plan.plan_id,
            request.rule_ids.len(),
            selected_rule_indices.len(),
            cleaner_rule_ids.len(),
            measured_rule_count,
            validation_elapsed_ms,
            request.rule_ids,
            request.dry_run
        );

        let plan_id = format!("plan-{started_at_ms}");
        let plan_hash = plan_hash(
            &plan_id,
            &request.rule_ids,
            &request.project_roots,
            &request.source_selections,
            request.dry_run,
        );
        let mut actions = Vec::new();
        if measured_rule_count > 0 {
            progress.finish_validation();
        }
        for (rule_index, measured) in selected_rule_indices.into_iter().zip(measured_rules) {
            let rule = &ownership_plan.rules[rule_index];
            progress.begin_rule();
            progress.emit(CleanupExecutionStage::Cleaning, Some(&rule.id));
            let action = if operation.ensure_not_cancelled().is_err() {
                cancelled_action(
                    &rule.id,
                    CleanupActionKind::Delete,
                    measured.as_ref().map_or(0, |measurement| measurement.bytes),
                )
            } else {
                let mut report_item = |path: &Path, stats: &DeleteStats| {
                    progress.record_item(&rule.id, path, stats);
                };
                execute_rule(
                    rule,
                    rule_index,
                    measured,
                    &RuleExecutionContext {
                        ownership_plan: &ownership_plan,
                        process_snapshot: &process_snapshot,
                        source_scope: source_selection_policy.scope(&rule.id),
                        operation: &operation,
                        dry_run: request.dry_run,
                    },
                    &mut report_item,
                )
            };
            progress.record_action(&action);
            progress.emit(CleanupExecutionStage::Cleaning, Some(&rule.id));
            actions.push(action);
        }
        let active_rule_roots = ownership_plan.active_rule_roots();
        actions.extend(cleaners::execute_selected_with_progress(
            cleaners::CleanerExecutionRequest {
                rule_ids: &cleaner_rule_ids,
                inventory: &applicability_context.inventory,
                declared_roots: &active_rule_roots,
                project_roots: &request.project_roots,
                selected_volume_scope,
                source_selections: &source_selection_policy,
                dry_run: request.dry_run,
                operation: &operation,
            },
            |rule_id, action| {
                if let Some(action) = action {
                    progress.record_action(action);
                } else {
                    progress.begin_rule();
                }
                progress.emit(CleanupExecutionStage::Cleaning, Some(rule_id));
            },
        ));
        let expected_bytes = actions.iter().map(|action| action.bytes_expected).sum();
        let released_bytes = actions.iter().map(|action| action.released_bytes).sum();
        let affected_item_count = actions
            .iter()
            .map(|action| action.affected_item_count)
            .sum();
        let failed_item_count = actions.iter().map(|action| action.failed_item_count).sum();
        progress.emit(CleanupExecutionStage::Finalizing, None);
        let record = summarize_deep_cleanup(
            deep_cleanup_operation_id,
            started_at_ms,
            now_ms(),
            request.dry_run,
            DeepCleanupOperationDetails {
                cleanup: Some(CleanupOperationDetails {
                    selected_rule_ids: request.rule_ids,
                    expected_bytes,
                    actions: actions.clone(),
                }),
                application_leftovers: None,
            },
        );
        // History is an auxiliary audit capability after deletion has occurred.
        // A persistence failure must not report an irreversible successful
        // operation as failed; structured state and logs preserve the failure.
        let history_saved = match HistoryService::upsert_deep_cleanup(record.clone()) {
            Ok(()) => true,
            Err(error) => {
                log::warn!(
                    "cleanup_history_save_failed operation_id={} run_id={} error_digest={}",
                    operation.id(),
                    record.operation_id,
                    blake3::hash(error.diagnostic().as_bytes()).to_hex()
                );
                false
            }
        };
        log::info!(
            "cleanup_finished operation_id={} status={} expected_bytes={} released_bytes={} affected_item_count={} failed_item_count={} history_saved={} elapsed_ms={}",
            operation.id(),
            record.outcome.as_str(),
            expected_bytes,
            released_bytes,
            affected_item_count,
            failed_item_count,
            history_saved,
            started.elapsed().as_millis()
        );
        operation.complete();
        Ok(CleanupResult {
            plan_id,
            plan_hash,
            expected_bytes,
            released_bytes,
            affected_item_count,
            failed_item_count,
            dry_run: request.dry_run,
            actions,
            record,
            history_saved,
        })
    }
}

/// Limits the expensive read-only traversal to cases that cannot safely derive
/// their result from the destructive pass. A preview has no destructive pass,
/// while a source-scoped request must authenticate untrusted UI paths before
/// any selected file is removed.
fn requires_preflight_measurement(dry_run: bool, has_source_scope: bool) -> bool {
    dry_run || has_source_scope
}

fn preparation_stage(measured_rule_count: usize) -> CleanupExecutionStage {
    if measured_rule_count == 0 {
        CleanupExecutionStage::Cleaning
    } else {
        CleanupExecutionStage::Validating
    }
}

fn plan_hash(
    plan_id: &str,
    rule_ids: &[String],
    project_roots: &[String],
    source_selections: &[crate::cleanup::CleanupSourceSelection],
    dry_run: bool,
) -> String {
    let mut normalized = rule_ids.to_vec();
    normalized.sort();
    let mut normalized_roots = project_roots.to_vec();
    normalized_roots.sort();
    let mut hasher = Sha256::new();
    update_hash_field(&mut hasher, plan_id.as_bytes());
    update_hash_field(&mut hasher, if dry_run { b"dry-run" } else { b"apply" });
    for id in normalized {
        update_hash_field(&mut hasher, id.as_bytes());
    }
    for root in normalized_roots {
        update_hash_field(&mut hasher, root.as_bytes());
    }
    let mut normalized_sources = source_selections.to_vec();
    normalized_sources.sort_by(|left, right| left.rule_id.cmp(&right.rule_id));
    for selection in normalized_sources {
        update_hash_field(&mut hasher, selection.rule_id.as_bytes());
        update_hash_field(
            &mut hasher,
            match selection.mode {
                crate::cleanup::CleanupSourceSelectionMode::Include => b"include",
                crate::cleanup::CleanupSourceSelectionMode::Exclude => b"exclude",
            },
        );
        let mut paths = selection.paths;
        paths.sort();
        for path in paths {
            update_hash_field(&mut hasher, path.as_bytes());
        }
    }
    hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn update_hash_field(hasher: &mut Sha256, value: &[u8]) {
    hasher.update((value.len() as u64).to_le_bytes());
    hasher.update(value);
}

#[cfg(test)]
mod cleanup_matcher_tests {
    use std::path::PathBuf;

    use super::*;

    #[test]
    fn preflight_measurement_is_limited_to_preview_and_source_scoped_requests() {
        assert!(!requires_preflight_measurement(false, false));
        assert!(requires_preflight_measurement(true, false));
        assert!(requires_preflight_measurement(false, true));
        assert!(requires_preflight_measurement(true, true));
        assert_eq!(preparation_stage(0), CleanupExecutionStage::Cleaning);
        assert_eq!(preparation_stage(1), CleanupExecutionStage::Validating);
    }

    #[test]
    fn cleanup_service_cancels_the_active_cleanup_operation() {
        let _operation_lock = crate::shared::operation::test_operation_lock();
        let operation = OperationGuard::start(CoordinatedOperationKind::Cleanup)
            .expect("the isolated cleanup operation must start");

        CleanupService::cancel();

        assert!(
            operation.ensure_not_cancelled().is_err(),
            "the public cleanup cancellation contract must reach the active operation"
        );
    }

    #[test]
    fn execution_progress_preserves_stage_order_and_final_totals() {
        let mut snapshots = Vec::new();
        let action = CleanupActionResult {
            rule_id: "system.fixture".to_string(),
            action_kind: CleanupActionKind::Delete,
            status: crate::cleanup::CleanupActionStatus::Completed,
            reason_code: None,
            bytes_expected: 120,
            released_bytes: 96,
            affected_item_count: 2,
            failed_item_count: 0,
            running_processes: Vec::new(),
        };
        {
            let mut reporter =
                ExecutionProgressReporter::new(vec!["system.fixture".to_string()], |progress| {
                    snapshots.push(progress)
                });
            reporter.emit(CleanupExecutionStage::Validating, None);
            reporter.record_validation(3, 120);
            reporter.emit(CleanupExecutionStage::Validating, Some("system.fixture"));
            reporter.finish_validation();
            reporter.emit(CleanupExecutionStage::Cleaning, Some(&action.rule_id));
            reporter.record_action(&action);
            reporter.emit(CleanupExecutionStage::Cleaning, Some(&action.rule_id));
            reporter.emit(CleanupExecutionStage::Finalizing, None);
        }

        let stages = snapshots
            .iter()
            .map(|progress| progress.stage)
            .collect::<Vec<_>>();
        assert_eq!(
            stages,
            vec![
                CleanupExecutionStage::Validating,
                CleanupExecutionStage::Validating,
                CleanupExecutionStage::Cleaning,
                CleanupExecutionStage::Cleaning,
                CleanupExecutionStage::Finalizing,
            ]
        );
        assert!(snapshots
            .iter()
            .all(|progress| progress.completed_rule_count <= progress.total_rule_count));
        assert!(snapshots
            .iter()
            .all(|progress| progress.planned_rule_ids.as_slice() == ["system.fixture"]));
        let final_snapshot = snapshots.last().expect("final progress must be emitted");
        let serialized = serde_json::to_value(final_snapshot)
            .expect("cleanup execution progress must serialize for desktop events");
        assert_eq!(serialized["plannedRuleIds"][0], "system.fixture");
        assert_eq!(
            final_snapshot.affected_item_count,
            action.affected_item_count
        );
        assert_eq!(final_snapshot.released_bytes, action.released_bytes);
    }

    #[test]
    fn execution_progress_reports_live_items_and_completed_rule_results() {
        let mut snapshots = Vec::new();
        let action = CleanupActionResult {
            rule_id: "system.fixture".to_string(),
            action_kind: CleanupActionKind::Delete,
            status: crate::cleanup::CleanupActionStatus::Completed,
            reason_code: None,
            bytes_expected: 120,
            released_bytes: 64,
            affected_item_count: 1,
            failed_item_count: 0,
            running_processes: Vec::new(),
        };
        {
            let mut reporter =
                ExecutionProgressReporter::new(vec![action.rule_id.clone()], |progress| {
                    snapshots.push(progress)
                });
            reporter.begin_rule();
            reporter.record_item(
                &action.rule_id,
                Path::new("fixture/cache.tmp"),
                &DeleteStats {
                    matched_bytes: 64,
                    deleted_bytes: 64,
                    affected_item_count: 1,
                    failed_item_count: 0,
                },
            );
            reporter.record_action(&action);
            reporter.emit(CleanupExecutionStage::Cleaning, Some(&action.rule_id));
        }

        let live_snapshot = snapshots
            .first()
            .expect("the first deleted item must produce live progress");
        assert_eq!(
            live_snapshot.current_item_path.as_deref(),
            Some("fixture/cache.tmp")
        );
        assert_eq!(live_snapshot.current_rule_affected_item_count, 1);
        assert_eq!(live_snapshot.current_rule_released_bytes, 64);
        assert_eq!(live_snapshot.affected_item_count, 1);
        assert_eq!(live_snapshot.released_bytes, 64);

        let completed_snapshot = snapshots
            .last()
            .expect("the completed rule must produce a summary");
        assert_eq!(completed_snapshot.current_item_path, None);
        assert_eq!(completed_snapshot.completed_rule_count, 1);
        assert_eq!(completed_snapshot.completed_rule_results.len(), 1);
        assert_eq!(
            completed_snapshot.completed_rule_results[0].rule_id,
            action.rule_id
        );
        assert_eq!(
            completed_snapshot.completed_rule_results[0].status,
            action.status
        );
        assert_eq!(
            completed_snapshot.completed_rule_results[0].affected_item_count,
            action.affected_item_count
        );
        assert_eq!(
            completed_snapshot.completed_rule_results[0].released_bytes,
            action.released_bytes
        );
    }

    #[test]
    fn whole_rule_cleanup_derives_expected_bytes_during_single_pass() {
        let _operation_lock = crate::shared::operation::test_operation_lock();
        let sandbox = std::env::temp_dir().join(format!(
            "mangodisk-cleanup-single-pass-test-{}-{}",
            std::process::id(),
            now_ms()
        ));
        let _sandbox_cleanup = DirectoryCleanup(sandbox.clone());
        let cleanup_root = sandbox.join("cache");
        let cache_file = cleanup_root.join("generated.tmp");
        let cache_bytes = b"single-pass cleanup fixture";
        fs::create_dir_all(&cleanup_root).expect("the isolated cleanup root must be created");
        fs::write(&cache_file, cache_bytes).expect("the cleanup fixture must be written");
        let plan = compile_scan_plan(
            vec![CompiledRule::fixture(
                "system.single-pass-fixture",
                cleanup_root,
                crate::cleanup::CleanupCategory::System,
                MatcherSpec::ExtensionIn(vec!["tmp".to_string()]),
            )],
            &[true],
            &[],
        )
        .expect("the isolated rule must compile");
        let process_snapshot = ProcessSnapshot::default();
        let operation = OperationGuard::start(CoordinatedOperationKind::Cleanup)
            .expect("the isolated cleanup operation must start");

        let action = execute_rule(
            &plan.rules[0],
            0,
            None,
            &RuleExecutionContext {
                ownership_plan: &plan,
                process_snapshot: &process_snapshot,
                source_scope: None,
                operation: &operation,
                dry_run: false,
            },
            &mut |_, _| {},
        );

        operation.complete();
        assert_eq!(
            action.status,
            crate::cleanup::CleanupActionStatus::Completed
        );
        assert_eq!(action.bytes_expected, cache_bytes.len() as u64);
        assert_eq!(action.released_bytes, cache_bytes.len() as u64);
        assert_eq!(action.affected_item_count, 1);
        assert!(!cache_file.exists());
    }

    #[test]
    fn complete_root_cleanup_reduces_per_file_deletion_transactions() {
        let _operation_lock = crate::shared::operation::test_operation_lock();
        let sandbox = std::env::temp_dir().join(format!(
            "mangodisk-cleanup-whole-root-test-{}-{}",
            std::process::id(),
            now_ms()
        ));
        let _sandbox_cleanup = DirectoryCleanup(sandbox.clone());
        let cleanup_root = sandbox.join("cache");
        let nested = cleanup_root.join("many-small-files");
        let generic_root = sandbox.join("generic-cache");
        let generic_nested = generic_root.join("many-small-files");
        let generic_empty = generic_root.join("empty-scaffold/nested");
        fs::create_dir_all(&nested).expect("the isolated cleanup root must be created");
        fs::create_dir_all(&generic_nested).expect("the generic comparison root must be created");
        fs::create_dir_all(&generic_empty).expect("create the empty comparison directory");
        let generic_root_identity = physical_path_identity_snapshot(&generic_root)
            .expect("capture the comparison root identity");
        let generic_root_permissions = fs::metadata(&generic_root)
            .expect("read the comparison root metadata")
            .permissions();
        let generic_empty_identity = physical_path_identity_snapshot(&generic_empty)
            .expect("capture the empty comparison directory identity");
        let file_count = 128_u64;
        for index in 0..file_count {
            fs::write(nested.join(format!("{index}.cache")), b"cache")
                .expect("the small cache fixture must be written");
            fs::write(generic_nested.join(format!("{index}.cache")), b"cache")
                .expect("the comparison cache fixture must be written");
        }
        let whole_root_plan = compile_scan_plan(
            vec![CompiledRule::whole_root_fixture(
                "development.whole-root-fixture",
                cleanup_root.clone(),
                crate::cleanup::CleanupCategory::Development,
            )],
            &[true],
            &[],
        )
        .expect("the isolated whole-root rule must compile");
        let generic_plan = compile_scan_plan(
            vec![CompiledRule::fixture(
                "development.generic-fixture",
                generic_root.clone(),
                crate::cleanup::CleanupCategory::Development,
                MatcherSpec::All,
            )],
            &[true],
            &[],
        )
        .expect("the generic comparison rule must compile");
        let process_snapshot = ProcessSnapshot::default();
        let operation = OperationGuard::start(CoordinatedOperationKind::Cleanup)
            .expect("the isolated cleanup operation must start");
        let mut reported_paths = Vec::new();
        let mut generic_report_count = 0_u64;

        let generic_action = execute_rule(
            &generic_plan.rules[0],
            0,
            None,
            &RuleExecutionContext {
                ownership_plan: &generic_plan,
                process_snapshot: &process_snapshot,
                source_scope: None,
                operation: &operation,
                dry_run: false,
            },
            &mut |_, _| generic_report_count = generic_report_count.saturating_add(1),
        );

        let action = execute_rule(
            &whole_root_plan.rules[0],
            0,
            None,
            &RuleExecutionContext {
                ownership_plan: &whole_root_plan,
                process_snapshot: &process_snapshot,
                source_scope: None,
                operation: &operation,
                dry_run: false,
            },
            &mut |path, _| reported_paths.push(path.to_path_buf()),
        );

        operation.complete();
        assert_eq!(
            generic_action.status,
            crate::cleanup::CleanupActionStatus::Completed
        );
        assert_eq!(generic_report_count, 1);
        assert!(
            generic_root.exists(),
            "content cleanup must retain its root"
        );
        assert_eq!(
            physical_path_identity_snapshot(&generic_root)
                .expect("read the retained comparison root identity"),
            generic_root_identity,
            "content cleanup must retain the physical root directory"
        );
        assert_eq!(
            fs::metadata(&generic_root)
                .expect("read the retained root metadata")
                .permissions(),
            generic_root_permissions,
            "content cleanup must retain the root permissions"
        );
        assert_eq!(
            fs::read_dir(&generic_root)
                .expect("read the retained cache root")
                .count(),
            1,
            "content cleanup must retain only the preexisting empty scaffold"
        );
        assert_eq!(
            physical_path_identity_snapshot(&generic_empty)
                .expect("read the retained empty directory identity"),
            generic_empty_identity
        );
        assert_eq!(
            action.status,
            crate::cleanup::CleanupActionStatus::Completed
        );
        assert_eq!(action.bytes_expected, file_count * 5);
        assert_eq!(action.released_bytes, file_count * 5);
        assert_eq!(action.affected_item_count, file_count);
        assert_eq!(reported_paths, vec![cleanup_root.clone()]);
        assert!(
            !cleanup_root.exists(),
            "the complete cache root must be removed"
        );
    }

    #[test]
    fn source_scoped_cleanup_keeps_unselected_complete_root_content() {
        let _operation_lock = crate::shared::operation::test_operation_lock();
        let sandbox = std::env::temp_dir().join(format!(
            "mangodisk-cleanup-scoped-root-test-{}-{}",
            std::process::id(),
            now_ms()
        ));
        let _sandbox_cleanup = DirectoryCleanup(sandbox.clone());
        let cleanup_root = sandbox.join("cache");
        let selected_source = cleanup_root.join("selected");
        let retained_source = cleanup_root.join("retained");
        let selected_file = selected_source.join("selected.cache");
        let retained_file = retained_source.join("retained.cache");
        fs::create_dir_all(&selected_source).expect("create the selected cache source");
        fs::create_dir_all(&retained_source).expect("create the retained cache source");
        fs::write(&selected_file, b"selected").expect("write the selected cache fixture");
        fs::write(&retained_file, b"retained").expect("write the retained cache fixture");
        let rule_id = "development.scoped-complete-root";
        let plan = compile_scan_plan(
            vec![CompiledRule::fixture(
                rule_id,
                cleanup_root.clone(),
                crate::cleanup::CleanupCategory::Development,
                MatcherSpec::All,
            )],
            &[true],
            &[],
        )
        .expect("compile the source-scoped cleanup plan");
        let policy = SourceSelectionPolicy::from_request(
            &HashSet::from([rule_id.to_string()]),
            &[crate::cleanup::CleanupSourceSelection {
                rule_id: rule_id.to_string(),
                mode: crate::cleanup::CleanupSourceSelectionMode::Include,
                paths: vec![selected_source.to_string_lossy().into_owned()],
            }],
        )
        .expect("compile the source selection");
        let process_snapshot = ProcessSnapshot::default();
        let operation = OperationGuard::start(CoordinatedOperationKind::Cleanup)
            .expect("start the isolated cleanup operation");

        let action = execute_rule(
            &plan.rules[0],
            0,
            None,
            &RuleExecutionContext {
                ownership_plan: &plan,
                process_snapshot: &process_snapshot,
                source_scope: policy.scope(rule_id),
                operation: &operation,
                dry_run: false,
            },
            &mut |_, _| {},
        );

        operation.complete();
        assert_eq!(
            action.status,
            crate::cleanup::CleanupActionStatus::Completed
        );
        assert!(!selected_file.exists());
        assert!(retained_file.exists());
        assert!(cleanup_root.exists());
    }

    /// Compares the previous per-entry traversal with both bulk strategies.
    ///
    /// The benchmark is ignored by default to keep normal tests independent of
    /// disk variance. The file and bucket environment variables shape the
    /// workload, while `MANGODISK_CLEANUP_BENCHMARK_PARALLEL_FIRST=1` reverses
    /// the bulk-strategy order to expose cache bias. Output contains only
    /// counts and timings, never private paths.
    #[test]
    #[ignore = "filesystem performance benchmark"]
    fn benchmark_complete_root_cleanup_strategies() {
        let _operation_lock = crate::shared::operation::test_operation_lock();
        let file_count = std::env::var("MANGODISK_CLEANUP_BENCHMARK_FILE_COUNT")
            .ok()
            .and_then(|value| value.parse::<u64>().ok())
            .filter(|count| *count > 0)
            .unwrap_or(5_000);
        let bucket_count = std::env::var("MANGODISK_CLEANUP_BENCHMARK_BUCKET_COUNT")
            .ok()
            .and_then(|value| value.parse::<u64>().ok())
            .filter(|count| *count > 0)
            .unwrap_or(128)
            .min(file_count);
        let available_parallelism = std::thread::available_parallelism().map_or(1, usize::from);
        let parallel_first =
            std::env::var("MANGODISK_CLEANUP_BENCHMARK_PARALLEL_FIRST").as_deref() == Ok("1");
        let sandbox = std::env::temp_dir().join(format!(
            "mangodisk-cleanup-whole-root-benchmark-{}-{}",
            std::process::id(),
            now_ms()
        ));
        let _sandbox_cleanup = DirectoryCleanup(sandbox.clone());
        let per_entry_root = sandbox.join("per-entry-cache");
        let serial_contents_root = sandbox.join("serial-contents-cache");
        let parallel_contents_root = sandbox.join("parallel-contents-cache");
        let whole_root = sandbox.join("whole-root-cache");
        fs::create_dir_all(&per_entry_root).expect("create the per-entry benchmark root");
        fs::create_dir_all(&serial_contents_root)
            .expect("create the serial contents benchmark root");
        fs::create_dir_all(&parallel_contents_root)
            .expect("create the parallel contents benchmark root");
        fs::create_dir_all(&whole_root).expect("create the whole-root benchmark root");
        let payload = [b'x'; 64];
        for index in 0..file_count {
            let bucket = format!("{:03}", index % bucket_count);
            let per_entry_bucket = per_entry_root.join(&bucket);
            let serial_contents_bucket = serial_contents_root.join(&bucket);
            let parallel_contents_bucket = parallel_contents_root.join(&bucket);
            let whole_bucket = whole_root.join(&bucket);
            fs::create_dir_all(&per_entry_bucket).expect("create the per-entry benchmark bucket");
            fs::create_dir_all(&serial_contents_bucket)
                .expect("create the serial contents benchmark bucket");
            fs::create_dir_all(&parallel_contents_bucket)
                .expect("create the parallel contents benchmark bucket");
            fs::create_dir_all(&whole_bucket).expect("create the whole-root benchmark bucket");
            let name = format!("{index:08}.cache");
            fs::write(per_entry_bucket.join(&name), payload)
                .expect("write the per-entry benchmark file");
            fs::write(serial_contents_bucket.join(&name), payload)
                .expect("write the serial contents benchmark file");
            fs::write(parallel_contents_bucket.join(&name), payload)
                .expect("write the parallel contents benchmark file");
            fs::write(whole_bucket.join(name), payload)
                .expect("write the whole-root benchmark file");
        }
        for bucket in 0..bucket_count {
            let bucket = format!("{bucket:03}");
            fs::create_dir_all(per_entry_root.join(&bucket).join("empty-scaffold"))
                .expect("create the per-entry empty scaffold");
            fs::create_dir_all(serial_contents_root.join(&bucket).join("empty-scaffold"))
                .expect("create the serial contents empty scaffold");
            fs::create_dir_all(parallel_contents_root.join(&bucket).join("empty-scaffold"))
                .expect("create the parallel contents empty scaffold");
            fs::create_dir_all(whole_root.join(&bucket).join("empty-scaffold"))
                .expect("create the whole-root empty scaffold");
        }
        let parallel_contents_plan = compile_scan_plan(
            vec![CompiledRule::fixture(
                "development.parallel-contents-benchmark",
                parallel_contents_root,
                crate::cleanup::CleanupCategory::Development,
                MatcherSpec::All,
            )],
            &[true],
            &[],
        )
        .expect("compile the parallel contents benchmark plan");
        let whole_root_plan = compile_scan_plan(
            vec![CompiledRule::whole_root_fixture(
                "development.whole-root-benchmark",
                whole_root,
                crate::cleanup::CleanupCategory::Development,
            )],
            &[true],
            &[],
        )
        .expect("compile the whole-root benchmark plan");
        let process_snapshot = ProcessSnapshot::default();
        let operation = OperationGuard::start(CoordinatedOperationKind::Cleanup)
            .expect("start the benchmark cleanup operation");

        let per_entry_canonical = validate_rule_root(&per_entry_root, &MatcherSpec::All)
            .expect("validate the per-entry benchmark root");
        let mut per_entry_stats = DeleteStats::default();
        let per_entry_started = Instant::now();
        delete_root_contents_with_progress(
            &per_entry_root,
            &per_entry_canonical,
            &MatcherSpec::All,
            DeleteRootContentsPolicy {
                owns_path: &|_, _| true,
                is_cancelled: &|| false,
                bulk_complete_directories: false,
            },
            &mut per_entry_stats,
            &mut |_, _| {},
        );
        let per_entry_ms = per_entry_started.elapsed().as_secs_f64() * 1_000.0;

        let run_serial_contents = || {
            let started = Instant::now();
            let mut file_count = 0_u64;
            for entry in fs::read_dir(&serial_contents_root)
                .expect("read the serial contents benchmark root")
            {
                let path = entry
                    .expect("read a serial contents benchmark entry")
                    .path();
                let prepared = prepare_path_for_permanent_delete(&path)
                    .expect("prepare a serial contents benchmark directory");
                let outcome = delete_directory_contents_permanently_with_cancellation_serial(
                    prepared,
                    &|| false,
                )
                .expect("delete a serial contents benchmark directory");
                file_count = file_count.saturating_add(outcome.affected_item_count());
            }
            (file_count, started.elapsed().as_secs_f64() * 1_000.0)
        };
        let run_parallel_contents = || {
            let started = Instant::now();
            let action = execute_rule(
                &parallel_contents_plan.rules[0],
                0,
                None,
                &RuleExecutionContext {
                    ownership_plan: &parallel_contents_plan,
                    process_snapshot: &process_snapshot,
                    source_scope: None,
                    operation: &operation,
                    dry_run: false,
                },
                &mut |_, _| {},
            );
            (action, started.elapsed().as_secs_f64() * 1_000.0)
        };
        let (
            (serial_contents_file_count, serial_contents_ms),
            (parallel_contents_action, parallel_contents_ms),
        ) = if parallel_first {
            let parallel = run_parallel_contents();
            let serial = run_serial_contents();
            (serial, parallel)
        } else {
            (run_serial_contents(), run_parallel_contents())
        };

        let whole_started = Instant::now();
        let whole_action = execute_rule(
            &whole_root_plan.rules[0],
            0,
            None,
            &RuleExecutionContext {
                ownership_plan: &whole_root_plan,
                process_snapshot: &process_snapshot,
                source_scope: None,
                operation: &operation,
                dry_run: false,
            },
            &mut |_, _| {},
        );
        let whole_ms = whole_started.elapsed().as_secs_f64() * 1_000.0;
        operation.complete();

        assert_eq!(
            parallel_contents_action.status,
            crate::cleanup::CleanupActionStatus::Completed
        );
        assert_eq!(
            whole_action.status,
            crate::cleanup::CleanupActionStatus::Completed
        );
        assert_eq!(per_entry_stats.affected_item_count, file_count);
        assert_eq!(serial_contents_file_count, file_count);
        assert_eq!(parallel_contents_action.affected_item_count, file_count);
        assert_eq!(whole_action.affected_item_count, file_count);
        println!(
            "cleanup_complete_root_benchmark file_count={file_count} bucket_count={bucket_count} available_parallelism={available_parallelism} parallel_first={parallel_first} per_entry_ms={per_entry_ms:.2} serial_contents_ms={serial_contents_ms:.2} parallel_contents_ms={parallel_contents_ms:.2} whole_root_ms={whole_ms:.2} serial_speedup={:.2} parallel_speedup={:.2} incremental_speedup={:.2}",
            per_entry_ms / serial_contents_ms.max(0.01),
            per_entry_ms / parallel_contents_ms.max(0.01),
            serial_contents_ms / parallel_contents_ms.max(0.01)
        );
    }

    #[test]
    fn complete_root_cleanup_falls_back_for_nested_rule_ownership() {
        let _operation_lock = crate::shared::operation::test_operation_lock();
        let sandbox = std::env::temp_dir().join(format!(
            "mangodisk-cleanup-whole-root-fallback-test-{}-{}",
            std::process::id(),
            now_ms()
        ));
        let _sandbox_cleanup = DirectoryCleanup(sandbox.clone());
        let cleanup_root = sandbox.join("cache");
        let child_root = cleanup_root.join("owned-by-child");
        let parent_file = cleanup_root.join("parent.cache");
        let child_file = child_root.join("child.cache");
        fs::create_dir_all(&child_root).expect("the nested rule root must be created");
        fs::write(&parent_file, b"parent cache").expect("the parent fixture must be written");
        fs::write(&child_file, b"child cache").expect("the child fixture must be written");
        let plan = compile_scan_plan(
            vec![
                CompiledRule::fixture(
                    "development.parent-fixture",
                    cleanup_root,
                    crate::cleanup::CleanupCategory::Development,
                    MatcherSpec::All,
                ),
                CompiledRule::fixture(
                    "development.child-fixture",
                    child_root,
                    crate::cleanup::CleanupCategory::Development,
                    MatcherSpec::All,
                ),
            ],
            &[true, true],
            &[],
        )
        .expect("nested ownership must compile");
        let process_snapshot = ProcessSnapshot::default();
        let operation = OperationGuard::start(CoordinatedOperationKind::Cleanup)
            .expect("the isolated cleanup operation must start");

        let action = execute_rule(
            &plan.rules[0],
            0,
            None,
            &RuleExecutionContext {
                ownership_plan: &plan,
                process_snapshot: &process_snapshot,
                source_scope: None,
                operation: &operation,
                dry_run: false,
            },
            &mut |_, _| {},
        );

        operation.complete();
        assert_eq!(
            action.status,
            crate::cleanup::CleanupActionStatus::Completed
        );
        assert!(
            !parent_file.exists(),
            "the parent-owned cache must be removed"
        );
        assert!(
            child_file.exists(),
            "fallback traversal must preserve a nested rule boundary"
        );
    }

    struct DirectoryCleanup(PathBuf);

    impl Drop for DirectoryCleanup {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn filtered_cleanup_preserves_unmatched_empty_directories() {
        let _operation_lock = crate::shared::operation::test_operation_lock();
        let sandbox = std::env::temp_dir().join(format!(
            "mangodisk-cleanup-matcher-test-{}-{}",
            std::process::id(),
            now_ms()
        ));
        let _sandbox_cleanup = DirectoryCleanup(sandbox.clone());
        let cleanup_root = sandbox.join("cache");
        let unmatched_empty = cleanup_root.join("user-created-empty");
        let matched_directory = cleanup_root.join("generated");
        let matched_file = matched_directory.join("cache.tmp");
        fs::create_dir_all(&unmatched_empty).expect("the unmatched directory must be created");
        fs::create_dir_all(&matched_directory).expect("the matched directory must be created");
        fs::write(&matched_file, b"temporary cache").expect("the matched file must be written");
        let canonical_root = validate_rule_root(&cleanup_root, &MatcherSpec::All)
            .expect("the isolated root must be safe");
        let mut stats = DeleteStats {
            matched_bytes: 0,
            deleted_bytes: 0,
            affected_item_count: 0,
            failed_item_count: 0,
        };
        let mut item_progress = Vec::new();

        delete_root_contents_with_progress(
            &cleanup_root,
            &canonical_root,
            &MatcherSpec::ExtensionIn(vec!["tmp".to_string()]),
            DeleteRootContentsPolicy {
                owns_path: &|_, _| true,
                is_cancelled: &|| false,
                bulk_complete_directories: false,
            },
            &mut stats,
            &mut |path, stats| {
                item_progress.push((
                    path.to_path_buf(),
                    stats.affected_item_count,
                    stats.deleted_bytes,
                ));
            },
        );

        assert!(
            !matched_file.exists(),
            "the matched cache file must be deleted"
        );
        assert!(
            !matched_directory.exists(),
            "a directory emptied by this operation may be pruned"
        );
        assert!(
            unmatched_empty.exists(),
            "a pre-existing empty directory is outside the matcher scope"
        );
        assert_eq!(stats.affected_item_count, 1);
        assert_eq!(stats.matched_bytes, b"temporary cache".len() as u64);
        assert_eq!(stats.failed_item_count, 0);
        assert_eq!(item_progress.len(), 1);
        assert_eq!(item_progress[0].0, matched_file);
        assert_eq!(item_progress[0].1, 1);
        assert_eq!(item_progress[0].2, b"temporary cache".len() as u64);
    }

    #[test]
    fn cancelled_cleanup_stops_before_removing_the_next_entry() {
        let _operation_lock = crate::shared::operation::test_operation_lock();
        let sandbox = std::env::temp_dir().join(format!(
            "mangodisk-cleanup-cancel-test-{}-{}",
            std::process::id(),
            now_ms()
        ));
        let _sandbox_cleanup = DirectoryCleanup(sandbox.clone());
        let cleanup_root = sandbox.join("cache");
        let cache_file = cleanup_root.join("cache.tmp");
        fs::create_dir_all(&cleanup_root).expect("the isolated cleanup root must be created");
        fs::write(&cache_file, b"temporary cache").expect("the cache fixture must be written");
        let canonical_root = validate_rule_root(&cleanup_root, &MatcherSpec::All)
            .expect("the isolated root must be safe");
        let mut stats = DeleteStats {
            matched_bytes: 0,
            deleted_bytes: 0,
            affected_item_count: 0,
            failed_item_count: 0,
        };

        delete_root_contents(
            &cleanup_root,
            &canonical_root,
            &MatcherSpec::All,
            &|_, _| true,
            &|| true,
            &mut stats,
        );

        assert!(
            cache_file.exists(),
            "a cancellation observed before traversal must preserve the file"
        );
        assert_eq!(stats.affected_item_count, 0);
        assert_eq!(stats.failed_item_count, 1);
    }

    #[test]
    fn overlapping_cleanup_respects_unselected_child_rule_ownership() {
        let _operation_lock = crate::shared::operation::test_operation_lock();
        let sandbox = std::env::temp_dir().join(format!(
            "mangodisk-cleanup-ownership-test-{}-{}",
            std::process::id(),
            now_ms()
        ));
        let _sandbox_cleanup = DirectoryCleanup(sandbox.clone());
        let parent_root = sandbox.join("cache");
        let child_root = parent_root.join("specialized");
        let parent_file = parent_root.join("general.bin");
        let child_file = child_root.join("owned.tmp");
        fs::create_dir_all(&child_root).expect("the overlapping roots must be created");
        fs::write(&parent_file, b"general cache").expect("the parent-owned file must be written");
        fs::write(&child_file, b"specialized cache").expect("the child-owned file must be written");

        let plan = compile_scan_plan(
            vec![
                CompiledRule::fixture(
                    "system.parent",
                    parent_root.clone(),
                    crate::cleanup::CleanupCategory::System,
                    MatcherSpec::All,
                ),
                CompiledRule::fixture(
                    "application.child",
                    child_root.clone(),
                    crate::cleanup::CleanupCategory::Application,
                    MatcherSpec::ExtensionIn(vec!["tmp".to_string()]),
                ),
            ],
            &[true, true],
            &[],
        )
        .expect("overlapping cleanup rules must produce stable ownership");

        let canonical_parent = validate_rule_root(&parent_root, &MatcherSpec::All)
            .expect("the parent root must be safe");
        let mut parent_stats = DeleteStats {
            matched_bytes: 0,
            deleted_bytes: 0,
            affected_item_count: 0,
            failed_item_count: 0,
        };
        delete_root_contents(
            &parent_root,
            &canonical_parent,
            &MatcherSpec::All,
            &|path, metadata| plan.rule_owns_path(0, path, metadata),
            &|| false,
            &mut parent_stats,
        );

        assert!(
            !parent_file.exists(),
            "the parent-owned file must be deleted"
        );
        assert!(
            child_file.exists(),
            "the unselected child rule must retain ownership of its file"
        );
        assert_eq!(parent_stats.affected_item_count, 1);
        assert_eq!(parent_stats.failed_item_count, 0);

        let canonical_child = validate_rule_root(
            &child_root,
            &MatcherSpec::ExtensionIn(vec!["tmp".to_string()]),
        )
        .expect("the child rule root must be safe");
        let mut child_stats = DeleteStats {
            matched_bytes: 0,
            deleted_bytes: 0,
            affected_item_count: 0,
            failed_item_count: 0,
        };
        delete_root_contents(
            &child_root,
            &canonical_child,
            &MatcherSpec::ExtensionIn(vec!["tmp".to_string()]),
            &|path, metadata| plan.rule_owns_path(1, path, metadata),
            &|| false,
            &mut child_stats,
        );

        assert!(
            !child_file.exists(),
            "the selected child rule may delete its file"
        );
        assert_eq!(child_stats.affected_item_count, 1);
        assert_eq!(child_stats.failed_item_count, 0);
    }
}

#[cfg(all(test, target_os = "macos"))]
mod macos_cleanup_tests {
    use std::{
        ffi::OsString,
        os::unix::fs::symlink,
        path::PathBuf,
        time::{Duration, SystemTime},
    };

    use super::*;
    use crate::cleanup::CleanupRequest;

    struct DirectoryCleanup(PathBuf);

    struct FileCleanup(Vec<PathBuf>);

    struct EnvironmentRestore(Vec<(&'static str, Option<OsString>)>);

    impl Drop for DirectoryCleanup {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    impl Drop for FileCleanup {
        fn drop(&mut self) {
            for path in &self.0 {
                let _ = fs::remove_file(path);
            }
        }
    }

    impl Drop for EnvironmentRestore {
        fn drop(&mut self) {
            for (name, value) in self.0.drain(..) {
                match value {
                    Some(value) => std::env::set_var(name, value),
                    None => std::env::remove_var(name),
                }
            }
        }
    }

    #[test]
    fn cleanup_deletes_regular_files_without_following_external_links() {
        let _operation_lock = crate::shared::operation::test_operation_lock();
        let sandbox = std::env::temp_dir().join(format!(
            "mangodisk-cleanup-boundary-test-{}-{}",
            std::process::id(),
            now_ms()
        ));
        let _sandbox_cleanup = DirectoryCleanup(sandbox.clone());
        let cleanup_root = sandbox.join("cache");
        let external_root = sandbox.join("external");
        let regular_file = cleanup_root.join("regular.tmp");
        let protected_file = external_root.join("protected.txt");
        let external_link = cleanup_root.join("external-link");
        fs::create_dir_all(&cleanup_root).expect("the isolated cleanup root must be created");
        fs::create_dir_all(&external_root).expect("the external fixture must be created");
        fs::write(&regular_file, b"temporary cache")
            .expect("the regular cache file must be written");
        fs::write(&protected_file, b"must remain").expect("the protected file must be written");
        symlink(&external_root, &external_link).expect("the external symlink must be created");
        let canonical_root = validate_rule_root(&cleanup_root, &MatcherSpec::All)
            .expect("the isolated root must be safe");
        let mut stats = DeleteStats {
            matched_bytes: 0,
            deleted_bytes: 0,
            affected_item_count: 0,
            failed_item_count: 0,
        };

        delete_root_contents(
            &cleanup_root,
            &canonical_root,
            &MatcherSpec::All,
            &|_, _| true,
            &|| false,
            &mut stats,
        );

        assert!(
            !regular_file.exists(),
            "the regular cache file must be deleted"
        );
        assert!(
            protected_file.exists(),
            "cleanup must not follow links outside the rule root"
        );
        assert!(
            external_link.symlink_metadata().is_ok(),
            "a rejected link must remain unchanged"
        );
        assert_eq!(stats.affected_item_count, 1);
        assert_eq!(stats.failed_item_count, 1);
    }

    #[test]
    #[ignore = "modifies HOME and executes isolated cleanup; run this test alone"]
    fn communication_cache_rule_preserves_message_container_data() {
        let _operation_lock = crate::shared::operation::test_operation_lock();
        let sandbox = std::env::current_dir()
            .expect("the test process must have a working directory")
            .join("target")
            .join(format!(
                "mangodisk-communication-cache-cleanup-test-{}-{}",
                std::process::id(),
                now_ms()
            ));
        let _sandbox_cleanup = DirectoryCleanup(sandbox.clone());
        let home = sandbox.join("home");
        let cache_file = home
            .join("Library/Caches/net.whatsapp.WhatsApp")
            .join("generated-cache.bin");
        let message_database = home
            .join("Library/Containers/net.whatsapp.WhatsApp/Data/Documents")
            .join("messages.db");
        for fixture in [&cache_file, &message_database] {
            fs::create_dir_all(fixture.parent().expect("the fixture must have a parent"))
                .expect("the isolated application directory must be created");
            fs::write(fixture, b"MangoDisk communication cache fixture")
                .expect("the isolated fixture must be written");
        }

        let _restore = EnvironmentRestore(vec![("HOME", std::env::var_os("HOME"))]);
        std::env::set_var("HOME", &home);

        let preview = CleanupService::execute(CleanupRequest {
            rule_ids: vec!["app.whatsapp-cache".to_string()],
            source_selections: Vec::new(),
            dry_run: true,
            project_roots: Vec::new(),
        })
        .expect("isolated communication cache preview must succeed");
        assert_eq!(preview.failed_item_count, 0);
        assert!(
            cache_file.exists(),
            "dry-run must preserve the cache fixture"
        );
        assert!(
            message_database.exists(),
            "dry-run must preserve message container data"
        );

        let result = CleanupService::execute(CleanupRequest {
            rule_ids: vec!["app.whatsapp-cache".to_string()],
            source_selections: Vec::new(),
            dry_run: false,
            project_roots: Vec::new(),
        })
        .expect("isolated communication cache cleanup must succeed");
        assert_eq!(result.failed_item_count, 0, "{:?}", result.actions);
        assert_eq!(result.affected_item_count, 1);
        assert!(
            !cache_file.exists(),
            "the rebuildable bundle cache must be deleted"
        );
        assert!(
            message_database.exists(),
            "message container data must remain outside the cleanup boundary"
        );
    }

    #[test]
    #[ignore = "modifies HOME and executes isolated cleanup; run this test alone"]
    fn developer_cache_rules_preserve_tools_configuration_and_project_data() {
        let _operation_lock = crate::shared::operation::test_operation_lock();
        let sandbox = std::env::current_dir()
            .expect("the test process must have a working directory")
            .join("target")
            .join(format!(
                "mangodisk-developer-cache-cleanup-test-{}-{}",
                std::process::id(),
                now_ms()
            ));
        let _sandbox_cleanup = DirectoryCleanup(sandbox.clone());
        let home = sandbox.join("home");
        let cache_files = [
            home.join("Library/Caches/deno/deps/cache.bin"),
            home.join(".bun/install/cache/package/index.js"),
            home.join("Library/Caches/composer/files/package.zip"),
            home.join(".composer/cache/repo/metadata.json"),
            home.join("Library/Caches/mise/node/remote_versions.msgpack.z"),
            home.join("Library/Caches/ccache/a/result"),
            home.join("Library/Caches/Mozilla.sccache/0/compile-result"),
            home.join(".gem/ruby/4.0.0/cache/example-1.0.0.gem"),
            home.join(".hex/cache/registry.ets"),
            home.join("Library/Caches/copilot/marketplace/index.json"),
            home.join(".m2/repository/org/example/demo/1.0/demo-1.0.jar"),
            home.join(".nuget/packages/example/1.0/example.1.0.nupkg"),
            home.join(".gradle/wrapper/dists/gradle-bin/hash/gradle/bin/gradle"),
            home.join(".gradle/.tmp/download.part"),
        ];
        let protected_files = [
            home.join(".deno/bin/deno"),
            home.join(".bun/bin/bun"),
            home.join(".composer/auth.json"),
            home.join(".local/share/mise/installs/node/22/bin/node"),
            home.join("Library/Caches/mise/http-tarballs/tool/extracted/bin/http-backend-tool"),
            home.join("project/vendor/package/source.php"),
            home.join("Library/Preferences/ccache/ccache.conf"),
            home.join("Library/Application Support/Mozilla.sccache/config"),
            home.join(".gem/ruby/4.0.0/gems/example-1.0.0/lib/example.rb"),
            home.join(".hex/hex.config"),
            home.join(".copilot/settings.json"),
            home.join(".m2/settings.xml"),
            home.join(".nuget/NuGet/NuGet.Config"),
            home.join("project/pom.xml"),
            home.join(".gradle/gradle.properties"),
            home.join("project/gradle/wrapper/gradle-wrapper.properties"),
        ];
        for fixture in cache_files.iter().chain(&protected_files) {
            fs::create_dir_all(fixture.parent().expect("the fixture must have a parent"))
                .expect("the isolated developer tool directory must be created");
            fs::write(fixture, b"MangoDisk developer cache fixture")
                .expect("the isolated developer tool fixture must be written");
        }

        let _restore = EnvironmentRestore(vec![("HOME", std::env::var_os("HOME"))]);
        std::env::set_var("HOME", &home);
        let rule_ids = [
            "dev.deno-cache",
            "dev.bun-cache",
            "dev.composer-cache",
            "dev.mise-cache",
            "dev.ccache-cache",
            "dev.sccache-cache",
            "dev.rubygems-cache",
            "dev.hex-cache",
            "dev.copilot-cli-cache",
            "dev.maven-cache",
            "dev.nuget-cache",
            "dev.gradle-cache",
        ]
        .map(str::to_string)
        .to_vec();

        let preview = CleanupService::execute(CleanupRequest {
            rule_ids: rule_ids.clone(),
            source_selections: Vec::new(),
            dry_run: true,
            project_roots: Vec::new(),
        })
        .expect("isolated developer cache preview must succeed");
        assert_eq!(preview.failed_item_count, 0, "{:?}", preview.actions);
        assert!(cache_files.iter().all(|fixture| fixture.exists()));
        assert!(protected_files.iter().all(|fixture| fixture.exists()));

        let result = CleanupService::execute(CleanupRequest {
            rule_ids,
            source_selections: Vec::new(),
            dry_run: false,
            project_roots: Vec::new(),
        })
        .expect("isolated developer cache cleanup must succeed");
        assert_eq!(result.failed_item_count, 0, "{:?}", result.actions);
        assert_eq!(result.affected_item_count, 14);
        assert!(cache_files.iter().all(|fixture| !fixture.exists()));
        assert!(protected_files.iter().all(|fixture| fixture.exists()));
    }

    /// Verifies the macOS Chrome rule against an isolated profile. Browser-level
    /// shader caches are deliberately placed beside account and browsing state
    /// so future root changes cannot silently widen the cleanup boundary.
    #[test]
    #[ignore = "modifies HOME and requires Google Chrome to be stopped"]
    fn chrome_cache_rule_preserves_isolated_profile_state() {
        let _operation_lock = crate::shared::operation::test_operation_lock();
        let sandbox = std::env::current_dir()
            .expect("the test process must have a working directory")
            .join("target")
            .join(format!(
                "mangodisk-chrome-cache-cleanup-test-{}-{}",
                std::process::id(),
                now_ms()
            ));
        let _sandbox_cleanup = DirectoryCleanup(sandbox.clone());
        let home = sandbox.join("home");
        let chrome_root = home.join("Library/Application Support/Google/Chrome");
        let cache_files = [
            home.join("Library/Caches/Google/Chrome/http-cache/data.bin"),
            chrome_root.join("ShaderCache/data.bin"),
            chrome_root.join("GrShaderCache/data.bin"),
            chrome_root.join("GraphiteDawnCache/data.bin"),
            chrome_root.join("GPUPersistentCache/data.bin"),
            chrome_root.join("Default/Cache/data.bin"),
            chrome_root.join("Default/Code Cache/data.bin"),
            chrome_root.join("Default/GPUCache/data.bin"),
        ];
        let protected_files = [
            chrome_root.join("Local State"),
            chrome_root.join("Default/Bookmarks"),
            chrome_root.join("Default/Cookies"),
            chrome_root.join("Default/History"),
            chrome_root.join("Default/Login Data"),
            chrome_root.join("Default/Preferences"),
            chrome_root.join("Default/Extensions/example/manifest.json"),
            chrome_root.join("Default/Service Worker/Database/000001.log"),
        ];
        for fixture in cache_files.iter().chain(&protected_files) {
            fs::create_dir_all(fixture.parent().expect("the fixture must have a parent"))
                .expect("the isolated Chrome directory must be created");
            fs::write(fixture, b"MangoDisk Chrome cache fixture")
                .expect("the isolated Chrome fixture must be written");
        }

        let _restore = EnvironmentRestore(vec![("HOME", std::env::var_os("HOME"))]);
        std::env::set_var("HOME", &home);
        let request = |dry_run| CleanupRequest {
            rule_ids: vec!["browser.chrome-cache".to_string()],
            source_selections: Vec::new(),
            dry_run,
            project_roots: Vec::new(),
        };

        let preview = CleanupService::execute(request(true))
            .expect("the isolated Chrome cache preview must succeed");
        assert_eq!(preview.failed_item_count, 0, "{:?}", preview.actions);
        assert!(cache_files.iter().all(|fixture| fixture.exists()));
        assert!(protected_files.iter().all(|fixture| fixture.exists()));

        let result = CleanupService::execute(request(false))
            .expect("the isolated Chrome cache cleanup must succeed");
        assert_eq!(result.failed_item_count, 0, "{:?}", result.actions);
        assert_eq!(result.affected_item_count, cache_files.len() as u64);
        assert!(cache_files.iter().all(|fixture| !fixture.exists()));
        assert!(protected_files.iter().all(|fixture| fixture.exists()));
    }

    /// Confirms that a live Chrome process blocks the complete rule before any
    /// browser-level or profile cache is traversed.
    #[test]
    #[ignore = "requires the real Google Chrome application to be running"]
    fn real_chrome_cache_blocks_while_running() {
        assert_eq!(
            std::env::var("MANGODISK_TEST_REAL_MACOS_CHROME_CACHE_BLOCK").as_deref(),
            Ok("1"),
            "set MANGODISK_TEST_REAL_MACOS_CHROME_CACHE_BLOCK=1 for the real process-gate diagnostic"
        );
        let running = ProcessSnapshot::capture()
            .expect("the macOS process inventory must be available")
            .matching_processes(&["Google Chrome".to_string()]);
        assert!(!running.is_empty(), "Google Chrome must be running");

        let result = CleanupService::execute(CleanupRequest {
            rule_ids: vec!["browser.chrome-cache".to_string()],
            source_selections: Vec::new(),
            dry_run: false,
            project_roots: Vec::new(),
        })
        .expect("the blocked Chrome cleanup must return a structured result");
        assert_eq!(result.actions.len(), 1);
        let action = &result.actions[0];
        assert_eq!(action.status, crate::cleanup::CleanupActionStatus::Blocked);
        assert_eq!(
            action.reason_code,
            Some(crate::cleanup::CleanupActionReason::RunningProcesses)
        );
        assert_eq!(action.released_bytes, 0);
        assert_eq!(action.affected_item_count, 0);
        assert!(!action.running_processes.is_empty());
        println!(
            "real_macos_chrome_cache_block running_process_count={}",
            action.running_processes.len()
        );
    }

    /// Runs the production Chrome rule against the initialized local profile.
    /// The test records representative durable-state metadata before mutation
    /// and adds a marker to every discovered cache root so dry-run and deletion
    /// coverage remain observable when Chrome has already pruned a cache itself.
    #[test]
    #[ignore = "permanently clears real Google Chrome caches"]
    fn real_chrome_cache_preserves_profile_state() {
        fn tree_metadata_signature(root: &Path) -> (u64, u64, u128) {
            fn visit(root: &Path, path: &Path, signature: &mut (u64, u64, u128)) {
                let metadata = fs::symlink_metadata(path)
                    .expect("the preserved Chrome metadata must remain readable");
                signature.0 = signature.0.saturating_add(1);
                signature.1 = signature.1.saturating_add(metadata.len());
                signature.2 = signature.2.saturating_add(
                    metadata
                        .modified()
                        .ok()
                        .and_then(|modified| modified.duration_since(SystemTime::UNIX_EPOCH).ok())
                        .map(|duration| duration.as_nanos())
                        .unwrap_or_default(),
                );
                if !metadata.is_dir() || metadata.file_type().is_symlink() {
                    return;
                }
                for entry in fs::read_dir(path)
                    .expect("the preserved Chrome metadata directory must be readable")
                {
                    let child = entry
                        .expect("the preserved Chrome metadata entry must be readable")
                        .path();
                    assert!(child.starts_with(root));
                    visit(root, &child, signature);
                }
            }

            let mut signature = (0u64, 0u64, 0u128);
            visit(root, root, &mut signature);
            signature
        }

        assert_eq!(
            std::env::var("MANGODISK_TEST_REAL_MACOS_CHROME_CACHE").as_deref(),
            Ok("1"),
            "set MANGODISK_TEST_REAL_MACOS_CHROME_CACHE=1 to authorize this real cache diagnostic"
        );
        let running = ProcessSnapshot::capture()
            .expect("the macOS process inventory must be available")
            .matching_processes(&["Google Chrome".to_string()]);
        assert!(
            running.is_empty(),
            "Google Chrome must be completely stopped"
        );

        let home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .expect("HOME must be available");
        let chrome_root = home.join("Library/Application Support/Google/Chrome");
        let profile = chrome_root.join("Default");
        assert!(
            profile.is_dir(),
            "Google Chrome must have an initialized profile"
        );

        let preserved_candidates = [
            chrome_root.join("Local State"),
            profile.join("Bookmarks"),
            profile.join("Cookies"),
            profile.join("History"),
            profile.join("Login Data"),
            profile.join("Network"),
            profile.join("Preferences"),
            profile.join("Extensions"),
            profile.join("Local Storage"),
            profile.join("Service Worker"),
            profile.join("Sessions"),
            profile.join("WebStorage"),
        ];
        let preserved_paths = preserved_candidates
            .into_iter()
            .filter(|path| path.exists())
            .collect::<Vec<_>>();
        assert!(
            preserved_paths.len() >= 9,
            "the initialized Chrome profile must expose representative durable state"
        );
        let preserved_before = preserved_paths
            .iter()
            .map(|path| tree_metadata_signature(path))
            .collect::<Vec<_>>();

        let mut target_roots = [
            home.join("Library/Caches/Google/Chrome"),
            chrome_root.join("ShaderCache"),
            chrome_root.join("GrShaderCache"),
            chrome_root.join("GraphiteDawnCache"),
            chrome_root.join("GPUPersistentCache"),
        ]
        .into_iter()
        .filter(|path| path.is_dir())
        .collect::<Vec<_>>();
        for profile_name in fs::read_dir(&chrome_root)
            .expect("the Chrome profile root must be readable")
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .filter(|path| {
                path.file_name().is_some_and(|name| {
                    name == "Default"
                        || name == "Guest Profile"
                        || name == "System Profile"
                        || name.to_string_lossy().starts_with("Profile ")
                })
            })
        {
            for suffix in [
                "Cache",
                "Code Cache",
                "GPUCache",
                "DawnCache",
                "GrShaderCache",
            ] {
                let candidate = profile_name.join(suffix);
                if candidate.is_dir() {
                    target_roots.push(candidate);
                }
            }
        }
        target_roots.sort();
        target_roots.dedup();
        assert!(
            target_roots
                .iter()
                .any(|path| path == &chrome_root.join("GraphiteDawnCache")),
            "the real profile must expose the newly covered Graphite cache"
        );

        let markers = target_roots
            .iter()
            .map(|root| root.join("mangodisk-rule-validation.bin"))
            .collect::<Vec<_>>();
        let _marker_cleanup = FileCleanup(markers.clone());
        for marker in &markers {
            fs::write(marker, b"payload").expect("the Chrome cache marker must be created");
        }
        let request = |dry_run| CleanupRequest {
            rule_ids: vec!["browser.chrome-cache".to_string()],
            source_selections: Vec::new(),
            dry_run,
            project_roots: Vec::new(),
        };

        let preview = CleanupService::execute(request(true))
            .expect("the real Chrome cache preview must succeed");
        assert_eq!(preview.failed_item_count, 0, "{:?}", preview.actions);
        assert!(preview.expected_bytes >= markers.len() as u64 * 7);
        assert!(markers.iter().all(|marker| marker.exists()));

        let result = CleanupService::execute(request(false))
            .expect("the real Chrome cache cleanup must succeed");
        assert_eq!(result.failed_item_count, 0, "{:?}", result.actions);
        assert!(result.released_bytes >= markers.len() as u64 * 7);
        assert!(markers.iter().all(|marker| !marker.exists()));
        let preserved_after = preserved_paths
            .iter()
            .map(|path| tree_metadata_signature(path))
            .collect::<Vec<_>>();
        assert_eq!(preserved_after, preserved_before);
        println!(
            "real_macos_chrome_cache_cleanup expected_bytes={} released_bytes={} affected_item_count={} target_root_count={} preserved_root_count={}",
            preview.expected_bytes,
            result.released_bytes,
            result.affected_item_count,
            target_roots.len(),
            preserved_paths.len()
        );
    }

    /// Runs the complete production Gradle rule so the new Wrapper and temp
    /// roots are validated together with the existing cache family. Sibling
    /// Gradle metadata remains hashed to detect accidental boundary expansion.
    #[test]
    #[ignore = "permanently clears real Gradle caches and downloaded Wrapper distributions"]
    fn real_gradle_cache_preserves_non_cache_state() {
        assert_eq!(
            std::env::var("MANGODISK_TEST_REAL_MACOS_GRADLE_CACHE").as_deref(),
            Ok("1"),
            "set MANGODISK_TEST_REAL_MACOS_GRADLE_CACHE=1 to authorize this real cache diagnostic"
        );
        let running = ProcessSnapshot::capture()
            .expect("the macOS process inventory must be available")
            .matching_processes(&["gradle".to_string(), "java".to_string()]);
        assert!(
            running.is_empty(),
            "Gradle and Java must be completely stopped"
        );

        let home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .expect("HOME must be available");
        let gradle_home = home.join(".gradle");
        let required_new_roots = [gradle_home.join("wrapper/dists"), gradle_home.join(".tmp")];
        assert!(
            required_new_roots.iter().all(|path| path.is_dir()),
            "both newly covered Gradle roots must exist"
        );
        let target_roots = [
            gradle_home.join("caches"),
            gradle_home.join("daemon"),
            gradle_home.join("workers"),
            gradle_home.join("notifications"),
            required_new_roots[0].clone(),
            required_new_roots[1].clone(),
        ]
        .into_iter()
        .filter(|path| path.is_dir())
        .collect::<Vec<_>>();
        let preserved_paths = [
            gradle_home.join("android"),
            gradle_home.join("kotlin-profile"),
            gradle_home.join("native"),
            gradle_home.join("gradle.properties"),
            gradle_home.join("init.gradle"),
            gradle_home.join("init.d"),
            gradle_home.join("jdks"),
        ]
        .into_iter()
        .filter(|path| path.exists())
        .collect::<Vec<_>>();
        assert!(
            !preserved_paths.is_empty(),
            "the Gradle home must expose non-cache state for boundary verification"
        );
        let preserved_before = preserved_paths
            .iter()
            .map(|path| digest_macos_tree_without_following_links(path))
            .collect::<Vec<_>>();

        let markers = target_roots
            .iter()
            .map(|root| root.join("mangodisk-rule-validation.bin"))
            .collect::<Vec<_>>();
        let _marker_cleanup = FileCleanup(markers.clone());
        for marker in &markers {
            fs::write(marker, b"payload").expect("the Gradle cache marker must be created");
        }
        let request = |dry_run| CleanupRequest {
            rule_ids: vec!["dev.gradle-cache".to_string()],
            source_selections: Vec::new(),
            dry_run,
            project_roots: Vec::new(),
        };

        let preview = CleanupService::execute(request(true))
            .expect("the real Gradle cache preview must succeed");
        assert_eq!(preview.failed_item_count, 0, "{:?}", preview.actions);
        assert!(preview.expected_bytes >= markers.len() as u64 * 7);
        assert!(markers.iter().all(|marker| marker.exists()));

        let result = CleanupService::execute(request(false))
            .expect("the real Gradle cache cleanup must succeed");
        assert_eq!(result.failed_item_count, 0, "{:?}", result.actions);
        assert!(result.released_bytes >= markers.len() as u64 * 7);
        assert!(markers.iter().all(|marker| !marker.exists()));
        let preserved_after = preserved_paths
            .iter()
            .map(|path| digest_macos_tree_without_following_links(path))
            .collect::<Vec<_>>();
        assert_eq!(preserved_after, preserved_before);
        println!(
            "real_macos_gradle_cache_cleanup expected_bytes={} released_bytes={} affected_item_count={} target_root_count={} preserved_root_count={}",
            preview.expected_bytes,
            result.released_bytes,
            result.affected_item_count,
            target_roots.len(),
            preserved_paths.len()
        );
    }

    /// Exercises the reference-derived macOS rules against isolated layouts
    /// that mirror the verified applications. Poetry's virtual environment,
    /// PyInstaller siblings, Ollama models, VS Code settings, and Docker state
    /// deliberately sit beside the selected cache data to guard each boundary.
    #[test]
    #[ignore = "modifies HOME and requires VS Code and Docker Desktop to be stopped"]
    fn reference_cache_rules_preserve_durable_developer_and_application_state() {
        let _operation_lock = crate::shared::operation::test_operation_lock();
        let sandbox = std::env::current_dir()
            .expect("the test process must have a working directory")
            .join("target")
            .join(format!(
                "mangodisk-reference-cache-cleanup-test-{}-{}",
                std::process::id(),
                now_ms()
            ));
        let _sandbox_cleanup = DirectoryCleanup(sandbox.clone());
        let home = sandbox.join("home");
        let cache_files = [
            home.join("Library/Caches/pypoetry/artifacts/aa/package.whl"),
            home.join("Library/Caches/pypoetry/cache/repositories/PyPI/index.json"),
            home.join("Library/Application Support/pyinstaller/bincache00py31364bit/arm64/adhoc/no-entitlements/index.dat"),
            home.join("Library/Application Support/Code/CachedExtensionVSIXs/extension.vsix"),
            home.join("Library/Caches/ollama/updates/hash/Ollama-darwin.zip"),
            home.join("Library/Containers/com.docker.docker/Data/log/vm/init.log.1"),
        ];
        let recent_ollama_update =
            home.join("Library/Caches/ollama/updates/recent/Ollama-darwin.zip");
        let recent_docker_log =
            home.join("Library/Containers/com.docker.docker/Data/log/vm/init.log");
        let protected_files = [
            home.join("Library/Caches/pypoetry/virtualenvs/project-py3.13/pyvenv.cfg"),
            home.join("Library/Application Support/pyinstaller/state/keep.json"),
            home.join(".ollama/models/blobs/sha256-model"),
            home.join("Library/Application Support/Code/User/settings.json"),
            home.join("Library/Group Containers/group.com.docker/settings-store.json"),
            home.join("Library/Containers/com.docker.docker/Data/vms/0/data/Docker.raw"),
        ];
        for fixture in cache_files
            .iter()
            .chain([&recent_ollama_update, &recent_docker_log])
            .chain(&protected_files)
        {
            fs::create_dir_all(fixture.parent().expect("the fixture must have a parent"))
                .expect("the isolated reference cache directory must be created");
            fs::write(fixture, b"MangoDisk reference cache fixture")
                .expect("the isolated reference cache fixture must be written");
        }
        let stale_time = SystemTime::now()
            .checked_sub(Duration::from_secs(8 * 86_400))
            .expect("the test time must move back by eight days");
        for fixture in [&cache_files[4], &cache_files[5]] {
            fs::File::options()
                .write(true)
                .open(fixture)
                .expect("the stale reference fixture must open")
                .set_times(fs::FileTimes::new().set_modified(stale_time))
                .expect("the stale reference fixture timestamp must be set");
        }

        let _restore = EnvironmentRestore(vec![("HOME", std::env::var_os("HOME"))]);
        std::env::set_var("HOME", &home);
        let rule_ids = [
            "dev.python-cache",
            "dev.pyinstaller-cache",
            "dev.vscode-cache",
            "app.ollama-update-cache",
            "container.docker-desktop-diagnostic-cache",
        ]
        .map(str::to_string)
        .to_vec();

        let preview = CleanupService::execute(CleanupRequest {
            rule_ids: rule_ids.clone(),
            source_selections: Vec::new(),
            dry_run: true,
            project_roots: Vec::new(),
        })
        .expect("the isolated reference cache preview must succeed");
        assert_eq!(preview.failed_item_count, 0, "{:?}", preview.actions);
        assert!(cache_files.iter().all(|fixture| fixture.exists()));
        assert!(protected_files.iter().all(|fixture| fixture.exists()));

        let result = CleanupService::execute(CleanupRequest {
            rule_ids,
            source_selections: Vec::new(),
            dry_run: false,
            project_roots: Vec::new(),
        })
        .expect("the isolated reference cache cleanup must succeed");
        assert_eq!(result.failed_item_count, 0, "{:?}", result.actions);
        assert_eq!(result.affected_item_count, 6, "{:?}", result.actions);
        assert!(cache_files.iter().all(|fixture| !fixture.exists()));
        assert!(recent_ollama_update.exists());
        assert!(recent_docker_log.exists());
        assert!(protected_files.iter().all(|fixture| fixture.exists()));
    }

    /// Permanently clears the verified real cache families after their owners
    /// stop. Durable state is recorded from disjoint Poetry, Ollama, VS Code,
    /// and Docker locations; the large Ollama model store and Docker VM disk
    /// use metadata signatures so validation never reads multi-gigabyte data.
    #[test]
    #[ignore = "permanently clears real Poetry, PyInstaller, Ollama, VS Code, and Docker caches"]
    fn real_reference_cache_rules_preserve_environments_models_and_vm_state() {
        fn tree_metadata_signature(root: &Path) -> (u64, u64, u128) {
            fn visit(path: &Path, signature: &mut (u64, u64, u128)) {
                let metadata = fs::symlink_metadata(path)
                    .expect("the preserved metadata entry must remain readable");
                signature.0 = signature.0.saturating_add(1);
                signature.1 = signature.1.saturating_add(metadata.len());
                signature.2 = signature.2.saturating_add(
                    metadata
                        .modified()
                        .ok()
                        .and_then(|modified| modified.duration_since(SystemTime::UNIX_EPOCH).ok())
                        .map(|duration| duration.as_nanos())
                        .unwrap_or_default(),
                );
                if !metadata.is_dir() || metadata.file_type().is_symlink() {
                    return;
                }
                for entry in
                    fs::read_dir(path).expect("the preserved metadata directory must be readable")
                {
                    visit(
                        &entry
                            .expect("the preserved metadata entry must be readable")
                            .path(),
                        signature,
                    );
                }
            }

            let mut signature = (0u64, 0u64, 0u128);
            visit(root, &mut signature);
            signature
        }

        assert_eq!(
            std::env::var("MANGODISK_TEST_REAL_MACOS_REFERENCE_CACHE").as_deref(),
            Ok("1"),
            "set MANGODISK_TEST_REAL_MACOS_REFERENCE_CACHE=1 to authorize this real cache diagnostic"
        );
        let process_snapshot =
            ProcessSnapshot::capture().expect("the macOS process inventory must be available");
        for process_name in [
            "Visual Studio Code",
            "Code",
            "Docker",
            "Docker Desktop",
            "com.docker.backend",
            "com.docker.virtualization",
        ] {
            assert!(
                process_snapshot
                    .matching_processes(&[process_name.to_string()])
                    .is_empty(),
                "every reference cache owner must be stopped before cleanup"
            );
        }

        let home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .expect("HOME must be available");
        let poetry_virtualenvs = home.join("Library/Caches/pypoetry/virtualenvs");
        let ollama_models = home.join(".ollama/models");
        let vscode_user = home.join("Library/Application Support/Code/User");
        let docker_group = home.join("Library/Group Containers/group.com.docker");
        let docker_disk =
            home.join("Library/Containers/com.docker.docker/Data/vms/0/data/Docker.raw");
        for path in [
            &poetry_virtualenvs,
            &ollama_models,
            &vscode_user,
            &docker_group,
            &docker_disk,
        ] {
            assert!(path.exists(), "every durable-state fixture must exist");
        }
        let poetry_before = digest_macos_tree_without_following_links(&poetry_virtualenvs);
        let ollama_before = tree_metadata_signature(&ollama_models);
        let vscode_before = digest_macos_tree_without_following_links(&vscode_user);
        let docker_group_before = digest_macos_tree_without_following_links(&docker_group);
        let docker_disk_before = tree_metadata_signature(&docker_disk);

        let rule_ids = [
            "dev.python-cache",
            "dev.pyinstaller-cache",
            "dev.vscode-cache",
            "app.ollama-update-cache",
            "container.docker-desktop-diagnostic-cache",
        ]
        .map(str::to_string)
        .to_vec();
        let preview = CleanupService::execute(CleanupRequest {
            rule_ids: rule_ids.clone(),
            source_selections: Vec::new(),
            dry_run: true,
            project_roots: Vec::new(),
        })
        .expect("the real reference cache preview must succeed");
        assert_eq!(preview.failed_item_count, 0, "{:?}", preview.actions);
        assert!(
            preview.expected_bytes > 350 * 1024 * 1024,
            "the real reference cache baseline must provide material benefit"
        );
        assert_eq!(
            digest_macos_tree_without_following_links(&poetry_virtualenvs),
            poetry_before
        );
        assert_eq!(tree_metadata_signature(&ollama_models), ollama_before);
        assert_eq!(
            digest_macos_tree_without_following_links(&vscode_user),
            vscode_before
        );
        assert_eq!(
            digest_macos_tree_without_following_links(&docker_group),
            docker_group_before
        );
        assert_eq!(tree_metadata_signature(&docker_disk), docker_disk_before);

        let result = CleanupService::execute(CleanupRequest {
            rule_ids,
            source_selections: Vec::new(),
            dry_run: false,
            project_roots: Vec::new(),
        })
        .expect("the real reference cache cleanup must succeed");
        assert_eq!(result.failed_item_count, 0, "{:?}", result.actions);
        assert!(result.released_bytes > 350 * 1024 * 1024);
        assert!(result.affected_item_count > 100);
        assert_eq!(
            digest_macos_tree_without_following_links(&poetry_virtualenvs),
            poetry_before
        );
        assert_eq!(tree_metadata_signature(&ollama_models), ollama_before);
        assert_eq!(
            digest_macos_tree_without_following_links(&vscode_user),
            vscode_before
        );
        assert_eq!(
            digest_macos_tree_without_following_links(&docker_group),
            docker_group_before
        );
        assert_eq!(tree_metadata_signature(&docker_disk), docker_disk_before);
        println!(
            "real_macos_reference_cache_cleanup expected_bytes={} released_bytes={} affected_item_count={} preserved_root_count=5",
            preview.expected_bytes, result.released_bytes, result.affected_item_count
        );
    }

    #[test]
    #[ignore = "modifies HOME and executes isolated cleanup; run this test alone"]
    fn ai_cache_rules_clean_only_rebuildable_data_and_preserve_models() {
        let _operation_lock = crate::shared::operation::test_operation_lock();
        // HOME cannot be nested under the system temporary directory because
        // the real system.user-temp rule would correctly own its parent. Keep
        // the isolated home under target to avoid user data and preserve the
        // same non-overlapping relationship as a real home directory.
        let sandbox = std::env::current_dir()
            .expect("the test process must have a working directory")
            .join("target")
            .join(format!(
                "mangodisk-ai-cache-cleanup-test-{}-{}",
                std::process::id(),
                now_ms()
            ));
        let _sandbox_cleanup = DirectoryCleanup(sandbox.clone());
        let home = sandbox.join("home");
        let downloads = home.join("Downloads");
        let huggingface_hub = home.join(".cache/huggingface/hub/models--fixture/blobs");
        let xet_environment = home.join(".cache/huggingface/xet/environment");
        let xet_chunk_cache = xet_environment.join("chunk_cache");
        let xet_shard_cache = xet_environment.join("shard_cache");
        let xet_staging = xet_environment.join("staging");
        let project = home.join("project");
        for directory in [
            &downloads,
            &huggingface_hub,
            &xet_chunk_cache,
            &xet_shard_cache,
            &xet_staging,
            &project,
        ] {
            fs::create_dir_all(directory).expect("the isolated rule directory must be created");
        }

        let stale_partial = downloads.join("old-model.crdownload");
        let recent_partial = downloads.join("active-model.crdownload");
        let completed_download = downloads.join("archive.zip");
        let downloaded_model = huggingface_hub.join("downloaded-model.bin");
        let xet_chunk = xet_chunk_cache.join("chunk.bin");
        let xet_shard = xet_shard_cache.join("shard.mdb");
        let resumable_upload = xet_staging.join("upload.mdb");
        let project_model = project.join("model.bin");
        for fixture in [
            &stale_partial,
            &recent_partial,
            &completed_download,
            &downloaded_model,
            &xet_chunk,
            &xet_shard,
            &resumable_upload,
            &project_model,
        ] {
            fs::write(fixture, b"MangoDisk AI cache fixture")
                .expect("the isolated cleanup fixture must be written");
        }
        let stale_time = SystemTime::now()
            .checked_sub(Duration::from_secs(8 * 86_400))
            .expect("the fixture timestamp must support an eight-day offset");
        fs::File::options()
            .write(true)
            .open(&stale_partial)
            .expect("the stale download fixture must open")
            .set_times(fs::FileTimes::new().set_modified(stale_time))
            .expect("the stale download timestamp must be updated");

        let _restore = EnvironmentRestore(vec![("HOME", std::env::var_os("HOME"))]);
        std::env::set_var("HOME", &home);

        assert!(
            validate_rule_root(&downloads, &MatcherSpec::All).is_err(),
            "Downloads must not be authorized as a broad cleanup root"
        );
        assert!(
            validate_rule_root(
                &downloads,
                &MatcherSpec::AllOf(vec![
                    MatcherSpec::OlderThanDays(6),
                    MatcherSpec::ExtensionIn(vec!["crdownload".to_string()]),
                ]),
            )
            .is_err(),
            "a recent partial download must not be authorized"
        );
        assert!(
            validate_rule_root(
                &downloads,
                &MatcherSpec::AllOf(vec![
                    MatcherSpec::OlderThanDays(7),
                    MatcherSpec::ExtensionIn(vec!["zip".to_string()]),
                ]),
            )
            .is_err(),
            "a regular downloaded file must not be authorized"
        );

        let retired_rule = CleanupService::execute(CleanupRequest {
            rule_ids: vec![
                "system.stale-partial-downloads".to_string(),
                "ai.model-cache".to_string(),
            ],
            source_selections: Vec::new(),
            dry_run: false,
            project_roots: Vec::new(),
        });
        assert!(
            retired_rule.is_err(),
            "a retired unsafe rule ID must be rejected before deletion"
        );
        assert!(
            stale_partial.exists(),
            "an unknown rule must prevent all deletion in the request"
        );
        assert!(
            CleanupService::execute(CleanupRequest {
                rule_ids: vec!["ai.gemini-temp-files".to_string()],
                source_selections: Vec::new(),
                dry_run: false,
                project_roots: Vec::new(),
            })
            .is_err(),
            "the retired rule that covered Gemini sessions must stay unavailable"
        );

        let result = CleanupService::execute(CleanupRequest {
            rule_ids: vec![
                "system.stale-partial-downloads".to_string(),
                "ai.huggingface-xet-cache".to_string(),
            ],
            source_selections: Vec::new(),
            dry_run: false,
            project_roots: Vec::new(),
        })
        .expect("isolated stale-download and AI transfer-cache cleanup must succeed");

        assert_eq!(
            result.failed_item_count, 0,
            "isolated cleanup must not fail: {:?}",
            result.actions
        );
        assert_eq!(result.affected_item_count, 3);
        assert!(
            !stale_partial.exists(),
            "a partial download older than seven days must be deleted"
        );
        assert!(
            !xet_chunk.exists(),
            "the Xet download transfer cache must be deleted"
        );
        assert!(
            !xet_shard.exists(),
            "the Xet upload transfer cache must be deleted"
        );
        assert!(downloaded_model.exists(), "Hugging Face models must remain");
        assert!(
            resumable_upload.exists(),
            "resumable Xet uploads must remain"
        );
        assert!(
            recent_partial.exists(),
            "recent partial downloads must remain"
        );
        assert!(
            completed_download.exists(),
            "completed downloads must remain"
        );
        assert!(
            project_model.exists(),
            "models inside projects must remain unchanged"
        );
    }

    /// Verifies the executable-name gates for the newly absorbed VS Code and
    /// Docker Desktop rules against the real signed applications. Both rules
    /// must stop before filesystem traversal while their owners are running.
    #[test]
    #[ignore = "requires real VS Code and Docker Desktop processes"]
    fn real_reference_cache_rules_block_running_owners() {
        assert_eq!(
            std::env::var("MANGODISK_TEST_REAL_MACOS_REFERENCE_CACHE_BLOCK").as_deref(),
            Ok("1"),
            "set MANGODISK_TEST_REAL_MACOS_REFERENCE_CACHE_BLOCK=1 for the real process-gate diagnostic"
        );
        let cases = [
            (
                "container.docker-desktop-diagnostic-cache",
                vec![
                    "Docker".to_string(),
                    "Docker Desktop".to_string(),
                    "com.docker.backend".to_string(),
                ],
            ),
            ("dev.vscode-cache", vec!["Code".to_string()]),
        ];
        let process_snapshot =
            ProcessSnapshot::capture().expect("the macOS process inventory must be available");

        for (rule_id, process_names) in &cases {
            assert!(
                !process_snapshot
                    .matching_processes(process_names)
                    .is_empty(),
                "every reference cache owner must be running"
            );
            let result = CleanupService::execute(CleanupRequest {
                rule_ids: vec![(*rule_id).to_string()],
                source_selections: Vec::new(),
                dry_run: false,
                project_roots: Vec::new(),
            })
            .expect("the blocked reference cache cleanup must return a structured result");
            assert_eq!(result.actions.len(), 1);
            let action = &result.actions[0];
            assert_eq!(action.status, crate::cleanup::CleanupActionStatus::Blocked);
            assert_eq!(
                action.reason_code,
                Some(crate::cleanup::CleanupActionReason::RunningProcesses)
            );
            assert_eq!(action.released_bytes, 0);
            assert_eq!(action.affected_item_count, 0);
            assert!(!action.running_processes.is_empty());
        }
        println!(
            "real_macos_reference_cache_block owner_count={}",
            cases.len()
        );
    }

    /// Proves that every newly verified macOS application rule reaches the
    /// production process gate before a destructive traversal starts. This is
    /// intentionally a real-profile diagnostic: synthetic process fixtures
    /// cannot validate executable-name matching for signed application bundles.
    #[test]
    #[ignore = "requires the real macOS cache-owner applications to be running"]
    fn real_application_cache_rules_block_running_owners() {
        assert_eq!(
            std::env::var("MANGODISK_TEST_REAL_MACOS_APP_CACHE_BLOCK").as_deref(),
            Ok("1"),
            "set MANGODISK_TEST_REAL_MACOS_APP_CACHE_BLOCK=1 for the real process-gate diagnostic"
        );
        let cases = [
            ("app.vlc-cache", "VLC"),
            ("app.postman-cache", "Postman"),
            ("app.discord-cache", "Discord"),
            ("app.telegram-temporary-cache", "Telegram"),
            ("app.slack-cache", "Slack"),
            ("app.lobsterai-update-cache", "LobsterAI"),
            ("dev.qoder-rendering-cache", "Qoder"),
            ("app.qwenwork-cache", "QwenWorkCN"),
        ];
        let process_snapshot =
            ProcessSnapshot::capture().expect("the macOS process inventory must be available");

        for (rule_id, process_name) in cases {
            assert!(
                !process_snapshot
                    .matching_processes(&[process_name.to_string()])
                    .is_empty(),
                "the real process-gate diagnostic requires every cache owner to be running"
            );
            // Use real mode deliberately. A successful assertion proves the
            // process gate, rather than dry-run semantics, prevented mutation.
            let result = CleanupService::execute(CleanupRequest {
                rule_ids: vec![rule_id.to_string()],
                source_selections: Vec::new(),
                dry_run: false,
                project_roots: Vec::new(),
            })
            .expect("the blocked cleanup request must return a structured result");
            assert_eq!(result.actions.len(), 1);
            let action = &result.actions[0];
            assert_eq!(action.status, crate::cleanup::CleanupActionStatus::Blocked);
            assert_eq!(
                action.reason_code,
                Some(crate::cleanup::CleanupActionReason::RunningProcesses)
            );
            assert_eq!(action.released_bytes, 0);
            assert_eq!(action.affected_item_count, 0);
            assert!(!action.running_processes.is_empty());
        }
        println!(
            "real_macos_application_cache_block owner_count={}",
            cases.len()
        );
    }

    /// Clears the four real cache families only after their owners are stopped.
    /// Representative account and application state is hashed before and after
    /// cleanup. Markers live inside already verified cache boundaries, making
    /// the assertion independent of cache contents that vary between launches.
    #[test]
    #[ignore = "permanently clears real VLC, Postman, Discord, and Telegram caches"]
    fn real_application_cache_rules_preserve_durable_state() {
        assert_eq!(
            std::env::var("MANGODISK_TEST_REAL_MACOS_APP_CACHE_CLEANUP").as_deref(),
            Ok("1"),
            "set MANGODISK_TEST_REAL_MACOS_APP_CACHE_CLEANUP=1 to authorize this real cache diagnostic"
        );
        let process_snapshot =
            ProcessSnapshot::capture().expect("the macOS process inventory must be available");
        for process_name in ["VLC", "Postman", "Discord", "Telegram"] {
            assert!(
                process_snapshot
                    .matching_processes(&[process_name.to_string()])
                    .is_empty(),
                "every cache owner must be stopped before the real cleanup diagnostic"
            );
        }

        let home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .expect("HOME must be available");
        let application_support = home.join("Library/Application Support");
        let user_caches = home.join("Library/Caches");
        let postman_root = application_support.join("Postman");
        let postman_partitions = postman_root.join("Partitions");
        let discord_root = application_support.join("discord");
        let telegram_tdata = application_support.join("Telegram Desktop/tdata");
        assert!(postman_partitions.is_dir());
        assert!(discord_root.is_dir());
        assert!(telegram_tdata.is_dir());

        let mut partitions = fs::read_dir(&postman_partitions)
            .expect("the Postman partitions root must be readable")
            .map(|entry| {
                entry
                    .expect("the Postman partition must be readable")
                    .path()
            })
            .filter(|path| path.is_dir())
            .collect::<Vec<_>>();
        partitions.sort();
        assert!(
            partitions.len() >= 2,
            "the real Postman profile must expose representative partitions"
        );

        // Only known durable roots are hashed. Cache leaves are deliberately
        // absent from this list because rebuilding them is the expected result.
        let mut preserved_paths = Vec::new();
        for relative in [
            "storage",
            "Local Storage",
            "Network",
            "IndexedDB",
            "Session Storage",
            "WebStorage",
            "Preferences",
            "Local State",
            "databases",
        ] {
            let path = postman_root.join(relative);
            if path.exists() {
                preserved_paths.push(path);
            }
        }
        for partition in &partitions {
            for relative in [
                "Storage",
                "IndexedDB",
                "Local Storage",
                "Network",
                "Session Storage",
                "WebStorage",
                "Preferences",
            ] {
                let path = partition.join(relative);
                if path.exists() {
                    preserved_paths.push(path);
                }
            }
        }
        for relative in [
            "Local Storage",
            "Network",
            "IndexedDB",
            "Service Worker",
            "Session Storage",
            "WebStorage",
            "Preferences",
            "Local State",
            "settings.json",
            "shared_proto_db",
        ] {
            let path = discord_root.join(relative);
            if path.exists() {
                preserved_paths.push(path);
            }
        }
        for path in [
            application_support.join("org.videolan.vlc"),
            home.join("Library/Preferences/org.videolan.vlc"),
        ] {
            if path.exists() {
                preserved_paths.push(path);
            }
        }
        assert!(
            preserved_paths.len() >= 12,
            "the real profiles must expose representative durable state"
        );
        let preserved_before = preserved_paths
            .iter()
            .map(|path| digest_macos_tree(path, &[]))
            .collect::<Vec<_>>();
        // Telegram's hashed tdata entries are account and message state. Hash
        // the complete tree except the two explicitly selected cache roots.
        let telegram_before = digest_macos_tree(&telegram_tdata, &["temp", "dumps"]);

        let mut markers = vec![
            postman_root.join("Cache/mangodisk-rule-validation.bin"),
            discord_root.join("Cache/mangodisk-rule-validation.bin"),
            user_caches.join("com.hnc.Discord/mangodisk-rule-validation.bin"),
            telegram_tdata.join("dumps/mangodisk-rule-validation.dmp"),
            user_caches.join("org.videolan.vlc/mangodisk-rule-validation.bin"),
        ];
        for partition in &partitions {
            markers.push(partition.join("Cache/mangodisk-rule-validation.bin"));
        }
        for marker in &markers {
            fs::create_dir_all(
                marker
                    .parent()
                    .expect("the cache marker must have a parent"),
            )
            .expect("the verified cache root must be writable");
            fs::write(marker, b"payload").expect("the isolated cache marker must be written");
        }
        let request = |dry_run| CleanupRequest {
            rule_ids: vec![
                "app.vlc-cache".to_string(),
                "app.postman-cache".to_string(),
                "app.discord-cache".to_string(),
                "app.telegram-temporary-cache".to_string(),
            ],
            source_selections: Vec::new(),
            dry_run,
            project_roots: Vec::new(),
        };

        let preview = CleanupService::execute(request(true))
            .expect("the real application cache preview must succeed");
        assert_eq!(preview.failed_item_count, 0, "{:?}", preview.actions);
        assert!(preview.expected_bytes >= markers.len() as u64 * 7);
        assert!(markers.iter().all(|marker| marker.exists()));

        let result = CleanupService::execute(request(false))
            .expect("the real application cache cleanup must succeed");
        assert_eq!(result.failed_item_count, 0, "{:?}", result.actions);
        assert!(result.released_bytes >= markers.len() as u64 * 7);
        assert!(markers.iter().all(|marker| !marker.exists()));

        let preserved_after = preserved_paths
            .iter()
            .map(|path| digest_macos_tree(path, &[]))
            .collect::<Vec<_>>();
        assert_eq!(preserved_after, preserved_before);
        assert_eq!(
            digest_macos_tree(&telegram_tdata, &["temp", "dumps"]),
            telegram_before
        );
        println!(
            "real_macos_application_cache_cleanup expected_bytes={} released_bytes={} affected_item_count={} partition_count={} preserved_root_count={}",
            preview.expected_bytes,
            result.released_bytes,
            result.affected_item_count,
            partitions.len(),
            preserved_paths.len() + 1
        );
    }

    /// Validates the next macOS expansion wave against signed, initialized
    /// applications. The target roots contain large rebuildable web and update
    /// payloads beside account, project, skill, editor, and agent state, so the
    /// diagnostic hashes representative durable paths around production cleanup.
    #[test]
    #[ignore = "permanently clears real Slack, LobsterAI, Qoder, and QwenWork caches"]
    fn real_next_wave_application_caches_preserve_durable_state() {
        assert_eq!(
            std::env::var("MANGODISK_TEST_REAL_MACOS_NEXT_WAVE_CACHE").as_deref(),
            Ok("1"),
            "set MANGODISK_TEST_REAL_MACOS_NEXT_WAVE_CACHE=1 to authorize this real cache diagnostic"
        );
        let required_processes = [
            "Slack",
            "LobsterAI",
            "Qoder",
            "Qoder Helper",
            "QoderCN",
            "Qoder CN Helper",
            "QwenWorkCN",
        ];
        let process_snapshot =
            ProcessSnapshot::capture().expect("the macOS process inventory must be available");
        for process_name in required_processes {
            assert!(
                process_snapshot
                    .matching_processes(&[process_name.to_string()])
                    .is_empty(),
                "every next-wave cache owner must be stopped before cleanup"
            );
        }

        let home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .expect("HOME must be available");
        let application_support = home.join("Library/Application Support");
        let slack_root = application_support.join("Slack");
        let lobster_root = application_support.join("LobsterAI");
        let qoder_root = application_support.join("Qoder");
        let qoder_cn_root = application_support.join("QoderCN");
        let qwen_root = application_support.join("QwenWorkCN");
        for root in [
            &slack_root,
            &lobster_root,
            &qoder_root,
            &qoder_cn_root,
            &qwen_root,
        ] {
            assert!(root.is_dir(), "every real application profile must exist");
        }

        let mut preserved_paths = Vec::new();
        for path in [
            slack_root.join("Cookies"),
            slack_root.join("IndexedDB"),
            slack_root.join("Local Storage"),
            slack_root.join("Session Storage"),
            slack_root.join("WebStorage"),
            slack_root.join("Preferences"),
            slack_root.join("installation"),
            slack_root.join("Service Worker/Database"),
            slack_root.join("Service Worker/ScriptCache"),
            lobster_root.join("lobsterai.sqlite"),
            lobster_root.join("Preferences"),
            lobster_root.join("Cookies"),
            lobster_root.join("Local Storage"),
            lobster_root.join("Session Storage"),
            lobster_root.join("SKILLs/skills.config.json"),
            lobster_root.join("openclaw/state/openclaw.json"),
            qwen_root.join("auth.dat"),
            qwen_root.join("auth-v2.dat"),
            qwen_root.join("Preferences"),
            qwen_root.join("Local Storage"),
            qwen_root.join("Session Storage"),
            qwen_root.join("rum-electron-store"),
            qwen_root.join("data/agents.db"),
            qwen_root.join("data/agents.db-shm"),
            qwen_root.join("data/agents.db-wal"),
        ] {
            if path.exists() {
                preserved_paths.push(path);
            }
        }
        for root in [&qoder_root, &qoder_cn_root] {
            for relative in [
                "User",
                "Backups",
                "Local Storage",
                "Session Storage",
                "WebStorage",
                "Cookies",
                "Preferences",
                "Local State",
                "SharedClientCache",
                "CachedProfilesData",
                "CachedConfigurations",
            ] {
                let path = root.join(relative);
                if path.exists() {
                    preserved_paths.push(path);
                }
            }
        }
        let mut qoder_partitions = direct_directory_children(&qoder_cn_root.join("Partitions"));
        let mut qwen_partitions = direct_directory_children(&qwen_root.join("Partitions"));
        for partition in qoder_partitions.iter().chain(&qwen_partitions) {
            for relative in [
                "Local Storage",
                "Session Storage",
                "WebStorage",
                "Cookies",
                "Preferences",
                "Network Persistent State",
            ] {
                let path = partition.join(relative);
                if path.exists() {
                    preserved_paths.push(path);
                }
            }
        }
        assert!(
            preserved_paths.len() >= 30,
            "the real profiles must expose representative durable state"
        );
        let preserved_before = preserved_paths
            .iter()
            .map(|path| digest_macos_tree(path, &[]))
            .collect::<Vec<_>>();

        let mut markers = vec![
            slack_root.join("Service Worker/CacheStorage/mangodisk-rule-validation.bin"),
            lobster_root.join("updates/lobsterai-update-auto-mangodisk-validation.dmg"),
            qoder_root.join("Cache/mangodisk-rule-validation.bin"),
            qoder_cn_root.join("Cache/mangodisk-rule-validation.bin"),
            qwen_root.join("Cache/mangodisk-rule-validation.bin"),
        ];
        qoder_partitions.sort();
        qwen_partitions.sort();
        for partition in qoder_partitions.iter().chain(&qwen_partitions) {
            markers.push(partition.join("Cache/mangodisk-rule-validation.bin"));
        }
        for marker in &markers {
            fs::create_dir_all(
                marker
                    .parent()
                    .expect("the next-wave marker must have a parent"),
            )
            .expect("the verified next-wave cache root must be writable");
            fs::write(marker, b"payload").expect("the isolated cache marker must be written");
        }
        let request = |dry_run| CleanupRequest {
            rule_ids: vec![
                "app.slack-cache".to_string(),
                "app.lobsterai-update-cache".to_string(),
                "dev.qoder-rendering-cache".to_string(),
                "app.qwenwork-cache".to_string(),
            ],
            source_selections: Vec::new(),
            dry_run,
            project_roots: Vec::new(),
        };

        let preview = CleanupService::execute(request(true))
            .expect("the next-wave real cache preview must succeed");
        assert_eq!(preview.failed_item_count, 0, "{:?}", preview.actions);
        assert!(preview.expected_bytes >= markers.len() as u64 * 7);
        assert!(markers.iter().all(|marker| marker.exists()));

        let result = CleanupService::execute(request(false))
            .expect("the next-wave real cache cleanup must succeed");
        assert_eq!(result.failed_item_count, 0, "{:?}", result.actions);
        assert!(result.released_bytes >= markers.len() as u64 * 7);
        assert!(markers.iter().all(|marker| !marker.exists()));
        let preserved_after = preserved_paths
            .iter()
            .map(|path| digest_macos_tree(path, &[]))
            .collect::<Vec<_>>();
        assert_eq!(preserved_after, preserved_before);
        println!(
            "real_macos_next_wave_cache_cleanup expected_bytes={} released_bytes={} affected_item_count={} qoder_partition_count={} qwen_partition_count={} preserved_root_count={}",
            preview.expected_bytes,
            result.released_bytes,
            result.affected_item_count,
            qoder_partitions.len(),
            qwen_partitions.len(),
            preserved_paths.len()
        );
    }

    /// Proves that the third-wave rules reach the process gate with real,
    /// signed applications.
    ///
    /// The diagnostic deliberately submits a real cleanup request instead of a
    /// dry run. Only a `RunningProcesses` result proves that production stopped
    /// before traversal or mutation; static process-name configuration alone
    /// would be insufficient evidence for the runtime safety boundary.
    #[test]
    #[ignore = "requires the real ZenAion and Xmind applications to be running"]
    fn real_third_wave_application_caches_block_running_owners() {
        assert_eq!(
            std::env::var("MANGODISK_TEST_REAL_MACOS_THIRD_WAVE_BLOCK").as_deref(),
            Ok("1"),
            "set MANGODISK_TEST_REAL_MACOS_THIRD_WAVE_BLOCK=1 for the real process-gate diagnostic"
        );
        let cases = [
            ("app.zenaion-cache", "ZenAI"),
            ("app.xmind-rendering-cache", "Xmind"),
        ];
        let process_snapshot =
            ProcessSnapshot::capture().expect("the macOS process inventory must be available");

        for (rule_id, process_name) in cases {
            assert!(
                !process_snapshot
                    .matching_processes(&[process_name.to_string()])
                    .is_empty(),
                "every third-wave cache owner must be running"
            );
            let result = CleanupService::execute(CleanupRequest {
                rule_ids: vec![rule_id.to_string()],
                source_selections: Vec::new(),
                dry_run: false,
                project_roots: Vec::new(),
            })
            .expect("the blocked cleanup request must return a structured result");
            assert_eq!(result.actions.len(), 1);
            let action = &result.actions[0];
            assert_eq!(action.status, crate::cleanup::CleanupActionStatus::Blocked);
            assert_eq!(
                action.reason_code,
                Some(crate::cleanup::CleanupActionReason::RunningProcesses)
            );
            assert_eq!(action.released_bytes, 0);
            assert_eq!(action.affected_item_count, 0);
            assert!(!action.running_processes.is_empty());
        }
        println!(
            "real_macos_third_wave_cache_block owner_count={}",
            cases.len()
        );
    }

    /// Executes production cleanup against real ZenAion and Xmind profiles.
    /// Tree digests prove that identity, settings, skills, agent state, document
    /// recovery, and persistent browser data remain unchanged. Markers are
    /// written only inside verified cache leaves, proving dry-run behavior
    /// without depending on nondeterministic cache contents after application
    /// startup.
    #[test]
    #[ignore = "permanently clears real ZenAion and Xmind caches"]
    fn real_third_wave_application_caches_preserve_durable_state() {
        assert_eq!(
            std::env::var("MANGODISK_TEST_REAL_MACOS_THIRD_WAVE_CACHE").as_deref(),
            Ok("1"),
            "set MANGODISK_TEST_REAL_MACOS_THIRD_WAVE_CACHE=1 to authorize this real cache diagnostic"
        );
        let required_processes = ["ZenAI", "zenai-host", "Xmind", "Xmind Helper"];
        let process_snapshot =
            ProcessSnapshot::capture().expect("the macOS process inventory must be available");
        for process_name in required_processes {
            assert!(
                process_snapshot
                    .matching_processes(&[process_name.to_string()])
                    .is_empty(),
                "every third-wave cache owner must be stopped before cleanup"
            );
        }

        let home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .expect("HOME must be available");
        let application_support = home.join("Library/Application Support");
        let zen_root = application_support.join("bot.zenai");
        let xmind_root = application_support.join("Xmind/Electron v3");
        assert!(
            zen_root.is_dir(),
            "ZenAion must have completed a first launch"
        );
        assert!(
            xmind_root.is_dir(),
            "Xmind must have completed a first launch"
        );

        // Both digests cover every direct sibling outside the target caches.
        // Xmind's complete Crashpad directory is excluded because the rule owns
        // only its reports child. Crashpad has no document or account state;
        // every other persistent sibling remains part of the digest.
        let zen_before = digest_macos_tree(&zen_root, &[".caches", "logs"]);
        let xmind_before = digest_macos_tree(
            &xmind_root,
            &[
                "Cache",
                "Code Cache",
                "GPUCache",
                "DawnCache",
                "DawnGraphiteCache",
                "DawnWebGPUCache",
                "GrShaderCache",
                "GraphiteDawnCache",
                "Shared Dictionary",
                "Crashpad",
            ],
        );

        let markers = [
            zen_root.join(".caches/mangodisk-rule-validation.json"),
            zen_root.join("logs/mangodisk-rule-validation.log"),
            xmind_root.join("Cache/mangodisk-rule-validation.bin"),
            xmind_root.join("GPUCache/mangodisk-rule-validation.bin"),
        ];
        for marker in &markers {
            fs::create_dir_all(
                marker
                    .parent()
                    .expect("the third-wave marker must have a parent"),
            )
            .expect("the verified third-wave cache root must be writable");
            fs::write(marker, b"payload").expect("the isolated cache marker must be written");
        }
        let request = |dry_run| CleanupRequest {
            rule_ids: vec![
                "app.zenaion-cache".to_string(),
                "app.xmind-rendering-cache".to_string(),
            ],
            source_selections: Vec::new(),
            dry_run,
            project_roots: Vec::new(),
        };

        let preview = CleanupService::execute(request(true))
            .expect("the third-wave real cache preview must succeed");
        assert_eq!(preview.actions.len(), 2);
        assert_eq!(preview.failed_item_count, 0, "{:?}", preview.actions);
        assert!(preview.expected_bytes >= markers.len() as u64 * 7);
        assert!(markers.iter().all(|marker| marker.exists()));

        let result = CleanupService::execute(request(false))
            .expect("the third-wave real cache cleanup must succeed");
        assert_eq!(result.actions.len(), 2);
        assert_eq!(result.failed_item_count, 0, "{:?}", result.actions);
        assert!(result.released_bytes >= markers.len() as u64 * 7);
        assert!(markers.iter().all(|marker| !marker.exists()));
        assert_eq!(
            digest_macos_tree(&zen_root, &[".caches", "logs"]),
            zen_before
        );
        assert_eq!(
            digest_macos_tree(
                &xmind_root,
                &[
                    "Cache",
                    "Code Cache",
                    "GPUCache",
                    "DawnCache",
                    "DawnGraphiteCache",
                    "DawnWebGPUCache",
                    "GrShaderCache",
                    "GraphiteDawnCache",
                    "Shared Dictionary",
                    "Crashpad",
                ],
            ),
            xmind_before
        );
        println!(
            "real_macos_third_wave_cache_cleanup expected_bytes={} released_bytes={} affected_item_count={} preserved_snapshot_count=2",
            preview.expected_bytes, result.released_bytes, result.affected_item_count
        );
    }

    /// Exercises the production process gate against the signed DingTalk
    /// client before any account-scoped cache discovery can start. A real-mode
    /// blocked result is required because dry-run alone cannot prove that the
    /// destructive path stops before traversing dynamic account directories.
    #[test]
    #[ignore = "requires the real DingTalk application to be running"]
    fn real_dingtalk_content_cache_blocks_while_running() {
        assert_eq!(
            std::env::var("MANGODISK_TEST_REAL_DINGTALK_CACHE_BLOCK").as_deref(),
            Ok("1"),
            "set MANGODISK_TEST_REAL_DINGTALK_CACHE_BLOCK=1 for the real process-gate diagnostic"
        );
        let process_snapshot =
            ProcessSnapshot::capture().expect("the macOS process inventory must be available");
        assert!(
            !process_snapshot
                .matching_processes(&["DingTalk".to_string(), "DingTalkMac".to_string()])
                .is_empty(),
            "DingTalk must be running"
        );

        let result = CleanupService::execute(CleanupRequest {
            rule_ids: vec!["app.dingtalk-content-cache".to_string()],
            source_selections: Vec::new(),
            dry_run: false,
            project_roots: Vec::new(),
        })
        .expect("the blocked DingTalk cleanup must return a structured result");
        assert_eq!(result.actions.len(), 1);
        let action = &result.actions[0];
        assert_eq!(action.status, crate::cleanup::CleanupActionStatus::Blocked);
        assert_eq!(
            action.reason_code,
            Some(crate::cleanup::CleanupActionReason::RunningProcesses)
        );
        assert_eq!(action.released_bytes, 0);
        assert_eq!(action.affected_item_count, 0);
        assert!(!action.running_processes.is_empty());
        println!(
            "real_macos_dingtalk_cache_block running_process_count={}",
            action.running_processes.len()
        );
    }

    /// Validates DingTalk's application-classified cache against a real logged-in
    /// profile. Large chat databases and resource files are intentionally hashed
    /// even though their names sit beside cache leaves; this proves that dynamic
    /// account expansion cannot drift into messages, downloads, or account state.
    #[test]
    #[ignore = "permanently clears real DingTalk content caches"]
    fn real_dingtalk_content_cache_preserves_chat_and_account_state() {
        assert_eq!(
            std::env::var("MANGODISK_TEST_REAL_DINGTALK_CACHE").as_deref(),
            Ok("1"),
            "set MANGODISK_TEST_REAL_DINGTALK_CACHE=1 to authorize this real cache diagnostic"
        );
        let process_snapshot =
            ProcessSnapshot::capture().expect("the macOS process inventory must be available");
        assert!(
            process_snapshot
                .matching_processes(&["DingTalk".to_string(), "DingTalkMac".to_string()])
                .is_empty(),
            "DingTalk must be completely stopped before cleanup"
        );

        let home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .expect("HOME must be available");
        let application_root = home.join("Library/Application Support/DingTalkMac");
        let cache_root = home.join("Library/Caches/com.alibaba.DingTalkMac");
        assert!(
            application_root.is_dir(),
            "DingTalk must have completed a first launch"
        );
        let account_roots = direct_directory_children(&application_root)
            .into_iter()
            .filter(|path| {
                path.file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| name.ends_with("_v2"))
            })
            .collect::<Vec<_>>();
        assert!(
            !account_roots.is_empty(),
            "the logged-in DingTalk profile must expose an account root"
        );

        let mut preserved_paths = Vec::new();
        for relative in ["globalStorage", "config", "wukong", "emotions"] {
            let path = application_root.join(relative);
            if path.exists() {
                preserved_paths.push(path);
            }
        }
        for account in &account_roots {
            for relative in [
                "DBFiles",
                "NativeIM",
                "UserStorage",
                "CommonStorage",
                "dtnest_db",
                "resource_cache",
                "SafetyFiles",
                "SyncPoint",
            ] {
                let path = account.join(relative);
                if path.exists() {
                    preserved_paths.push(path);
                }
            }
        }
        assert!(
            preserved_paths.len() >= 10,
            "the real profile must expose representative chat and account state"
        );
        let preserved_before = preserved_paths
            .iter()
            .map(|path| digest_macos_tree(path, &[]))
            .collect::<Vec<_>>();

        let mut target_roots = Vec::new();
        for account in &account_roots {
            for relative in [
                "EAppFiles",
                "ImageFiles",
                "GifEmotionFiles",
                "wave_cards",
                "theme_cache",
                "Sync_v2/cache",
            ] {
                let path = account.join(relative);
                if path.is_dir() {
                    target_roots.push(path);
                }
            }
        }
        for relative in [
            "WebKit/NetworkCache",
            "WebKit/CacheStorage",
            "thumbnails",
            "fsCachedData",
        ] {
            let path = cache_root.join(relative);
            if path.is_dir() {
                target_roots.push(path);
            }
        }
        assert!(
            target_roots.len() >= 8,
            "the real profile must expose the verified DingTalk cache roots"
        );
        let markers = target_roots
            .iter()
            .map(|root| root.join("mangodisk-rule-validation.bin"))
            .collect::<Vec<_>>();
        for marker in &markers {
            fs::write(marker, b"payload").expect("the DingTalk cache marker must be created");
        }
        let request = |dry_run| CleanupRequest {
            rule_ids: vec!["app.dingtalk-content-cache".to_string()],
            source_selections: Vec::new(),
            dry_run,
            project_roots: Vec::new(),
        };

        let preview = CleanupService::execute(request(true))
            .expect("the real DingTalk cache dry run must succeed");
        assert_eq!(preview.actions.len(), 1);
        assert_eq!(preview.failed_item_count, 0, "{:?}", preview.actions);
        assert!(preview.expected_bytes >= markers.len() as u64 * 7);
        assert!(markers.iter().all(|marker| marker.exists()));

        let result = CleanupService::execute(request(false))
            .expect("the real DingTalk content cache cleanup must succeed");
        assert_eq!(result.actions.len(), 1);
        assert_eq!(result.failed_item_count, 0, "{:?}", result.actions);
        assert!(result.released_bytes >= markers.len() as u64 * 7);
        assert!(markers.iter().all(|marker| !marker.exists()));
        let preserved_after = preserved_paths
            .iter()
            .map(|path| digest_macos_tree(path, &[]))
            .collect::<Vec<_>>();
        assert_eq!(preserved_after, preserved_before);
        println!(
            "real_macos_dingtalk_content_cache_cleanup expected_bytes={} released_bytes={} affected_item_count={} account_count={} preserved_root_count={}",
            preview.expected_bytes,
            result.released_bytes,
            result.affected_item_count,
            account_roots.len(),
            preserved_paths.len()
        );
    }

    /// Exercises the production gate against the real signed Lark client.
    /// The application owns several multi-gigabyte account-scoped Chromium
    /// caches, so proving the destructive request stops before dynamic profile
    /// discovery is part of the rule's safety evidence.
    #[test]
    #[ignore = "requires the real Lark application to be running"]
    fn real_lark_renderer_cache_blocks_while_running() {
        assert_eq!(
            std::env::var("MANGODISK_TEST_REAL_LARK_CACHE_BLOCK").as_deref(),
            Ok("1"),
            "set MANGODISK_TEST_REAL_LARK_CACHE_BLOCK=1 for the real process-gate diagnostic"
        );
        let process_snapshot =
            ProcessSnapshot::capture().expect("the macOS process inventory must be available");
        assert!(
            !process_snapshot
                .matching_processes(&[
                    "Lark".to_string(),
                    "Feishu".to_string(),
                    "LarkShell".to_string(),
                    "Lark Helper".to_string(),
                    "Lark Helper (GPU)".to_string(),
                    "Lark Helper (Renderer)".to_string(),
                ])
                .is_empty(),
            "Lark must be running"
        );

        let result = CleanupService::execute(CleanupRequest {
            rule_ids: vec!["app.lark-renderer-cache".to_string()],
            source_selections: Vec::new(),
            dry_run: false,
            project_roots: Vec::new(),
        })
        .expect("the blocked Lark cleanup must return a structured result");
        assert_eq!(result.actions.len(), 1);
        let action = &result.actions[0];
        assert_eq!(action.status, crate::cleanup::CleanupActionStatus::Blocked);
        assert_eq!(
            action.reason_code,
            Some(crate::cleanup::CleanupActionReason::RunningProcesses)
        );
        assert_eq!(action.released_bytes, 0);
        assert_eq!(action.affected_item_count, 0);
        assert!(!action.running_processes.is_empty());
        println!(
            "real_macos_lark_cache_block running_process_count={}",
            action.running_processes.len()
        );
    }

    /// Clears only fixed rendering-cache leaves in real AHA and Iron profiles.
    /// Lark stores cookies, history, workspace state, downloads, IndexedDB, and
    /// Service Worker registrations beside those leaves. Their digests must be
    /// byte-identical after production cleanup, while isolated markers prove
    /// dry-run and dynamic-profile selection behavior.
    #[test]
    #[ignore = "permanently clears real Lark renderer caches"]
    fn real_lark_renderer_cache_preserves_account_and_workspace_state() {
        assert_eq!(
            std::env::var("MANGODISK_TEST_REAL_LARK_CACHE").as_deref(),
            Ok("1"),
            "set MANGODISK_TEST_REAL_LARK_CACHE=1 to authorize this real cache diagnostic"
        );
        let required_processes = [
            "Lark",
            "Feishu",
            "LarkShell",
            "Lark Helper",
            "Lark Helper (GPU)",
            "Lark Helper (Renderer)",
        ];
        let process_snapshot =
            ProcessSnapshot::capture().expect("the macOS process inventory must be available");
        for process_name in required_processes {
            assert!(
                process_snapshot
                    .matching_processes(&[process_name.to_string()])
                    .is_empty(),
                "every Lark cache owner must be stopped before cleanup"
            );
        }

        let home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .expect("HOME must be available");
        let application_root = home.join("Library/Application Support/LarkShell");
        let cache_root = home.join("Library/Caches/LarkShell");
        assert!(
            application_root.is_dir(),
            "Lark must have completed a first launch"
        );

        let mut user_roots = Vec::new();
        let mut application_profiles = Vec::new();
        for area in ["aha", "iron"] {
            let users_root = application_root.join(area).join("users");
            for user_root in direct_directory_children(&users_root) {
                for profile in direct_directory_children(&user_root) {
                    if profile
                        .file_name()
                        .and_then(|name| name.to_str())
                        .is_some_and(|name| name.starts_with("profile_"))
                    {
                        application_profiles.push(profile);
                    }
                }
                user_roots.push(user_root);
            }
        }
        assert!(
            application_profiles.len() >= 6,
            "the logged-in Lark profile must expose AHA and Iron profiles"
        );

        let mut preserved_paths = Vec::new();
        for relative in [
            "update",
            "PC_Gadget",
            "sdk_storage",
            "meego",
            "passport_storage",
            "persistent_storage.db",
            "persistent_storage.enc.db",
            "persistent_storage.preload.db",
            "iron/Local Storage",
            "iron/IndexedDB",
            "iron/Session Storage",
            "iron/WebStorage",
            "iron/Local State",
        ] {
            let path = application_root.join(relative);
            if path.exists() {
                preserved_paths.push(path);
            }
        }
        for user_root in &user_roots {
            for relative in [
                "morpheus",
                "Partitions",
                "PartitionsV2",
                "fgs",
                "AllDownloadHistory",
            ] {
                let path = user_root.join(relative);
                if path.exists() {
                    preserved_paths.push(path);
                }
            }
        }
        for profile in &application_profiles {
            for relative in [
                "History",
                "Cookies",
                "Preferences",
                "Secure Preferences",
                "Network Persistent State",
                "Local Storage",
                "IndexedDB",
                "Session Storage",
                "WebStorage",
                "Network",
                "Web Data",
                "Service Worker/Database",
                "Service Worker/ScriptCache",
            ] {
                let path = profile.join(relative);
                if path.exists() {
                    preserved_paths.push(path);
                }
            }
        }
        assert!(
            preserved_paths.len() >= 60,
            "the real profiles must expose representative account and workspace state"
        );
        let preserved_before = preserved_paths
            .iter()
            .map(|path| digest_macos_tree(path, &[]))
            .collect::<Vec<_>>();

        let dynamic_suffixes = [
            "Cache",
            "Code Cache",
            "GPUCache",
            "DawnCache",
            "DawnGraphiteCache",
            "DawnWebGPUCache",
            "GrShaderCache",
            "GraphiteDawnCache",
            "Shared Dictionary/cache",
            "Service Worker/CacheStorage",
        ];
        let mut target_roots = Vec::new();
        for profile in &application_profiles {
            for suffix in dynamic_suffixes {
                let path = profile.join(suffix);
                if path.is_dir() {
                    target_roots.push(path);
                }
            }
        }
        let cache_users_root = cache_root.join("aha/users");
        for user_root in direct_directory_children(&cache_users_root) {
            for profile in direct_directory_children(&user_root) {
                if !profile
                    .file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| name.starts_with("profile_"))
                {
                    continue;
                }
                for suffix in dynamic_suffixes {
                    let path = profile.join(suffix);
                    if path.is_dir() {
                        target_roots.push(path);
                    }
                }
            }
        }
        for relative in [
            "ShaderCache",
            "GrShaderCache",
            "GraphiteDawnCache",
            "CodeCache",
            "component_crx_cache",
            "iron/Cache",
            "iron/Code Cache",
            "iron/GPUCache",
            "iron/DawnCache",
            "iron/DawnGraphiteCache",
            "iron/DawnWebGPUCache",
            "iron/GrShaderCache",
            "iron/GraphiteDawnCache",
            "iron/Shared Dictionary/cache",
            "iron/Service Worker/CacheStorage",
        ] {
            let path = application_root.join(relative);
            if path.is_dir() {
                target_roots.push(path);
            }
        }
        target_roots.sort();
        target_roots.dedup();
        assert!(
            target_roots.len() >= 30,
            "the real profiles must expose verified Lark cache roots"
        );
        let markers = target_roots
            .iter()
            .map(|root| root.join("mangodisk-rule-validation.bin"))
            .collect::<Vec<_>>();
        for marker in &markers {
            fs::write(marker, b"payload").expect("the Lark cache marker must be created");
        }
        let request = |dry_run| CleanupRequest {
            rule_ids: vec!["app.lark-renderer-cache".to_string()],
            source_selections: Vec::new(),
            dry_run,
            project_roots: Vec::new(),
        };

        let preview = CleanupService::execute(request(true))
            .expect("the real Lark cache dry run must succeed");
        assert_eq!(preview.actions.len(), 1);
        assert_eq!(preview.failed_item_count, 0, "{:?}", preview.actions);
        assert!(preview.expected_bytes >= markers.len() as u64 * 7);
        assert!(markers.iter().all(|marker| marker.exists()));

        let result = CleanupService::execute(request(false))
            .expect("the real Lark renderer cache cleanup must succeed");
        assert_eq!(result.actions.len(), 1);
        assert_eq!(result.failed_item_count, 0, "{:?}", result.actions);
        assert!(result.released_bytes >= markers.len() as u64 * 7);
        assert!(markers.iter().all(|marker| !marker.exists()));
        let preserved_after = preserved_paths
            .iter()
            .map(|path| digest_macos_tree(path, &[]))
            .collect::<Vec<_>>();
        assert_eq!(preserved_after, preserved_before);
        println!(
            "real_macos_lark_renderer_cache_cleanup expected_bytes={} released_bytes={} affected_item_count={} user_count={} profile_count={} preserved_root_count={}",
            preview.expected_bytes,
            result.released_bytes,
            result.affected_item_count,
            user_roots.len(),
            application_profiles.len(),
            preserved_paths.len()
        );
    }

    /// Proves that the current signed WeChat, WeCom, and QQ clients reach the
    /// production process gate before any sandbox profile is traversed. These
    /// applications keep messages and account databases beside Chromium cache
    /// leaves, so real executable-name matching is required safety evidence.
    #[test]
    #[ignore = "requires the real WeChat, WeCom, and QQ applications to be running"]
    fn real_tencent_application_caches_block_running_owners() {
        assert_eq!(
            std::env::var("MANGODISK_TEST_REAL_TENCENT_MACOS_CACHE_BLOCK").as_deref(),
            Ok("1"),
            "set MANGODISK_TEST_REAL_TENCENT_MACOS_CACHE_BLOCK=1 for the real process-gate diagnostic"
        );
        let cases = [
            ("app.wechat-cache", vec!["WeChat".to_string()]),
            (
                "app.wecom-cache",
                vec![
                    "WeCom".to_string(),
                    "WXWork".to_string(),
                    "\u{4f01}\u{4e1a}\u{5fae}\u{4fe1}".to_string(),
                ],
            ),
            ("app.qq-cache", vec!["QQ".to_string()]),
        ];
        let process_snapshot =
            ProcessSnapshot::capture().expect("the macOS process inventory must be available");

        for (rule_id, process_names) in &cases {
            assert!(
                !process_snapshot
                    .matching_processes(process_names)
                    .is_empty(),
                "every Tencent cache owner must be running"
            );
            let result = CleanupService::execute(CleanupRequest {
                rule_ids: vec![(*rule_id).to_string()],
                source_selections: Vec::new(),
                dry_run: false,
                project_roots: Vec::new(),
            })
            .expect("the blocked Tencent cleanup must return a structured result");
            assert_eq!(result.actions.len(), 1);
            let action = &result.actions[0];
            assert_eq!(action.status, crate::cleanup::CleanupActionStatus::Blocked);
            assert_eq!(
                action.reason_code,
                Some(crate::cleanup::CleanupActionReason::RunningProcesses)
            );
            assert_eq!(action.released_bytes, 0);
            assert_eq!(action.affected_item_count, 0);
            assert!(!action.running_processes.is_empty());
        }
        println!("real_macos_tencent_cache_block owner_count={}", cases.len());
    }

    /// Clears fixed renderer-cache leaves in real WeChat, WeCom, and QQ
    /// sandboxes. Large message, account, mail, document, download, package,
    /// and browser-state roots are hashed before and after cleanup. Markers in
    /// every discovered target prove dry-run behavior and dynamic ownership.
    #[test]
    #[ignore = "permanently clears real WeChat, WeCom, and QQ renderer caches"]
    fn real_tencent_application_caches_preserve_durable_state() {
        assert_eq!(
            std::env::var("MANGODISK_TEST_REAL_TENCENT_MACOS_CACHE").as_deref(),
            Ok("1"),
            "set MANGODISK_TEST_REAL_TENCENT_MACOS_CACHE=1 to authorize this real cache diagnostic"
        );
        let required_processes = [
            "WeChat",
            "WeCom",
            "WXWork",
            "\u{4f01}\u{4e1a}\u{5fae}\u{4fe1}",
            "QQ",
        ];
        let process_snapshot =
            ProcessSnapshot::capture().expect("the macOS process inventory must be available");
        for process_name in required_processes {
            assert!(
                process_snapshot
                    .matching_processes(&[process_name.to_string()])
                    .is_empty(),
                "every Tencent cache owner must be stopped before cleanup"
            );
        }

        let home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .expect("HOME must be available");
        let wechat_container = home.join("Library/Containers/com.tencent.xinWeChat/Data");
        let wecom_container = home.join("Library/Containers/com.tencent.WeWorkMac/Data");
        let qq_container = home.join("Library/Containers/com.tencent.qq/Data");
        for root in [&wechat_container, &wecom_container, &qq_container] {
            assert!(root.is_dir(), "every Tencent client must have a sandbox");
        }

        let wechat_web_profiles_root =
            wechat_container.join("Documents/app_data/radium/web/profiles");
        let wechat_cache_profiles_root = wechat_container.join("Library/Caches/profiles");
        let wechat_web_profiles = direct_directory_children(&wechat_web_profiles_root);
        let wechat_cache_profiles = direct_directory_children(&wechat_cache_profiles_root);
        assert!(
            wechat_web_profiles.len() >= 6 && wechat_cache_profiles.len() >= 6,
            "the real WeChat sandbox must expose both profile trees"
        );

        let wecom_cef_root = wecom_container.join("Documents/cefcache");
        // WeCom stores account-scoped CEF profiles under `wew_*`. Filtering
        // siblings prevents global cache directories from entering the durable
        // state digest and keeps this test aligned with the declarative rule.
        let wecom_child_profiles = direct_directory_children(&wecom_cef_root)
            .into_iter()
            .filter(|path| {
                path.file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| name.starts_with("wew_"))
            })
            .collect::<Vec<_>>();
        assert!(
            !wecom_child_profiles.is_empty(),
            "the real WeCom sandbox must expose CEF profile children"
        );

        let qq_root = qq_container.join("Library/Application Support/QQ");
        let qq_partitions = direct_directory_children(&qq_root.join("Partitions"));
        let qq_account_roots = direct_directory_children(&qq_root)
            .into_iter()
            .filter(|path| {
                path.file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| name.starts_with("nt_qq_"))
            })
            .collect::<Vec<_>>();
        assert!(
            qq_partitions.len() >= 4 && qq_account_roots.len() >= 2,
            "the real QQ sandbox must expose renderer partitions and account roots"
        );

        let mut preserved_paths = Vec::new();
        for relative in [
            "Documents/xwechat_files",
            "Documents/app_data/users",
            "Documents/app_data/xplugin",
            "Documents/app_data/net",
            "Documents/app_data/login",
            "Documents/app_data/config",
        ] {
            let path = wechat_container.join(relative);
            if path.exists() {
                preserved_paths.push(path);
            }
        }
        for profile in &wechat_web_profiles {
            for relative in [
                "History",
                "Cookies",
                "Preferences",
                "Network Persistent State",
                "Local Storage",
                "IndexedDB",
                "Session Storage",
                "WebStorage",
                "Web Data",
                "Share Data",
                "Service Worker/Database",
                "Service Worker/ScriptCache",
            ] {
                let path = profile.join(relative);
                if path.exists() {
                    preserved_paths.push(path);
                }
            }
        }

        for relative in [
            "Documents/Profiles",
            "Documents/Network",
            "Documents/local_storage_index.db",
            "Documents/GYLog",
            "Library/Application Support/WXWork",
            "Library/Application Support/WeMail",
            "Library/Application Support/Wedoc",
            "Library/Application Support/WXDrive",
            "Library/Application Support/setting.json",
            "Library/WebKit",
        ] {
            let path = wecom_container.join(relative);
            if path.exists() {
                preserved_paths.push(path);
            }
        }
        for profile in &wecom_child_profiles {
            for relative in [
                "History",
                "Cookies",
                "Preferences",
                "Secure Preferences",
                "Network Persistent State",
                "Local Storage",
                "IndexedDB",
                "Session Storage",
                "WebStorage",
                "Web Data",
                "Service Worker/Database",
                "Service Worker/ScriptCache",
            ] {
                let path = profile.join(relative);
                if path.exists() {
                    preserved_paths.push(path);
                }
            }
        }

        for account in &qq_account_roots {
            preserved_paths.push(account.clone());
        }
        for relative in [
            "global",
            "dynamic_package",
            "dynamic_module",
            "arks",
            "Preferences",
            "Network Persistent State",
            "Local Storage",
            "Cookies",
        ] {
            let path = qq_root.join(relative);
            if path.exists() {
                preserved_paths.push(path);
            }
        }
        for partition in &qq_partitions {
            for relative in [
                "Cookies",
                "Preferences",
                "Network Persistent State",
                "Local Storage",
                "IndexedDB",
                "Session Storage",
                "WebStorage",
                "Service Worker/Database",
                "Service Worker/ScriptCache",
            ] {
                let path = partition.join(relative);
                if path.exists() {
                    preserved_paths.push(path);
                }
            }
        }
        assert!(
            preserved_paths.len() >= 90,
            "the real sandboxes must expose representative durable state"
        );
        let preserved_before = preserved_paths
            .iter()
            .map(|path| digest_macos_tree(path, &[]))
            .collect::<Vec<_>>();

        let dynamic_suffixes = [
            "Cache",
            "Code Cache",
            "GPUCache",
            "DawnCache",
            "DawnGraphiteCache",
            "DawnWebGPUCache",
            "GrShaderCache",
            "GraphiteDawnCache",
            "Shared Dictionary/cache",
            "Service Worker/CacheStorage",
        ];
        let mut target_roots = Vec::new();
        for profile in wechat_web_profiles.iter().chain(&wechat_cache_profiles) {
            for suffix in dynamic_suffixes {
                let path = profile.join(suffix);
                if path.is_dir() {
                    target_roots.push(path);
                }
            }
        }
        let wechat_bundle_cache = home.join("Library/Caches/com.tencent.xinWeChat");
        if wechat_bundle_cache.is_dir() {
            target_roots.push(wechat_bundle_cache);
        }

        for relative in [
            "Cache",
            "Code Cache",
            "GPUCache",
            "DawnCache",
            "DawnGraphiteCache",
            "DawnWebGPUCache",
            "GrShaderCache",
            "GraphiteDawnCache",
            "ShaderCache",
            "Shared Dictionary/cache",
            "Service Worker/CacheStorage",
            "component_crx_cache",
        ] {
            let path = wecom_cef_root.join(relative);
            if path.is_dir() {
                target_roots.push(path);
            }
        }
        for profile in &wecom_child_profiles {
            for suffix in dynamic_suffixes {
                let path = profile.join(suffix);
                if path.is_dir() {
                    target_roots.push(path);
                }
            }
        }
        let wecom_bundle_cache = home.join("Library/Caches/com.tencent.WeWorkMac");
        if wecom_bundle_cache.is_dir() {
            target_roots.push(wecom_bundle_cache);
        }

        for relative in [
            "Cache",
            "Code Cache",
            "GPUCache",
            "DawnCache",
            "DawnGraphiteCache",
            "DawnWebGPUCache",
            "GrShaderCache",
            "GraphiteDawnCache",
            "Shared Dictionary/cache",
        ] {
            let path = qq_root.join(relative);
            if path.is_dir() {
                target_roots.push(path);
            }
        }
        for partition in &qq_partitions {
            for suffix in dynamic_suffixes {
                let path = partition.join(suffix);
                if path.is_dir() {
                    target_roots.push(path);
                }
            }
        }
        let qq_bundle_cache = home.join("Library/Caches/com.tencent.qq");
        if qq_bundle_cache.is_dir() {
            target_roots.push(qq_bundle_cache);
        }

        target_roots.sort();
        target_roots.dedup();
        assert!(
            target_roots.len() >= 55,
            "the real sandboxes must expose verified Tencent cache roots"
        );
        let markers = target_roots
            .iter()
            .map(|root| root.join("mangodisk-rule-validation.bin"))
            .collect::<Vec<_>>();
        for marker in &markers {
            fs::write(marker, b"payload").expect("the Tencent cache marker must be created");
        }
        let request = |dry_run| CleanupRequest {
            rule_ids: vec![
                "app.wechat-cache".to_string(),
                "app.wecom-cache".to_string(),
                "app.qq-cache".to_string(),
            ],
            source_selections: Vec::new(),
            dry_run,
            project_roots: Vec::new(),
        };

        let preview = CleanupService::execute(request(true))
            .expect("the real Tencent cache dry run must succeed");
        assert_eq!(preview.actions.len(), 3);
        assert_eq!(preview.failed_item_count, 0, "{:?}", preview.actions);
        assert!(preview.expected_bytes >= markers.len() as u64 * 7);
        assert!(markers.iter().all(|marker| marker.exists()));

        let result = CleanupService::execute(request(false))
            .expect("the real Tencent cache cleanup must succeed");
        assert_eq!(result.actions.len(), 3);
        assert_eq!(result.failed_item_count, 0, "{:?}", result.actions);
        assert!(result.released_bytes >= markers.len() as u64 * 7);
        assert!(markers.iter().all(|marker| !marker.exists()));
        let preserved_after = preserved_paths
            .iter()
            .map(|path| digest_macos_tree(path, &[]))
            .collect::<Vec<_>>();
        assert_eq!(preserved_after, preserved_before);
        println!(
            "real_macos_tencent_cache_cleanup expected_bytes={} released_bytes={} affected_item_count={} wechat_profile_count={} wecom_child_count={} qq_partition_count={} preserved_root_count={}",
            preview.expected_bytes,
            result.released_bytes,
            result.affected_item_count,
            wechat_web_profiles.len() + wechat_cache_profiles.len(),
            wecom_child_profiles.len(),
            qq_partitions.len(),
            preserved_paths.len()
        );
    }

    /// Sends production cleanup requests while the signed applications are
    /// running. This proves that process preflight blocks both rules before
    /// any notes, offline content, or persistent browser state is traversed.
    #[test]
    #[ignore = "requires the real YNote and QQLive applications to be running"]
    fn real_ynote_qqlive_caches_block_running_owners() {
        assert_eq!(
            std::env::var("MANGODISK_TEST_REAL_YNOTE_QQLIVE_CACHE_BLOCK").as_deref(),
            Ok("1"),
            "set MANGODISK_TEST_REAL_YNOTE_QQLIVE_CACHE_BLOCK=1 for the real process-gate diagnostic"
        );
        let cases = [
            (
                "app.ynote-cache",
                "\u{6709}\u{9053}\u{4e91}\u{7b14}\u{8bb0}",
            ),
            ("app.qqlive-rendering-cache", "QQLive"),
        ];
        let process_snapshot =
            ProcessSnapshot::capture().expect("the macOS process inventory must be available");

        for (rule_id, process_name) in cases {
            assert!(
                !process_snapshot
                    .matching_processes(&[process_name.to_string()])
                    .is_empty(),
                "both real cache owners must be running"
            );
            let result = CleanupService::execute(CleanupRequest {
                rule_ids: vec![rule_id.to_string()],
                source_selections: Vec::new(),
                dry_run: false,
                project_roots: Vec::new(),
            })
            .expect("the blocked cleanup must return a structured result");
            assert_eq!(result.actions.len(), 1);
            let action = &result.actions[0];
            assert_eq!(action.status, crate::cleanup::CleanupActionStatus::Blocked);
            assert_eq!(
                action.reason_code,
                Some(crate::cleanup::CleanupActionReason::RunningProcesses)
            );
            assert_eq!(action.released_bytes, 0);
            assert_eq!(action.affected_item_count, 0);
            assert!(!action.running_processes.is_empty());
        }
        println!(
            "real_macos_ynote_qqlive_cache_block owner_count={}",
            cases.len()
        );
    }

    /// Runs dry-run and real cleanup against initialized YNote and QQLive
    /// profiles. Account and partition roots are discovered without logging
    /// private identifiers. Hashes prove that notes, backups, offline packs,
    /// runtime components, settings, and browser persistence remain unchanged.
    #[test]
    #[ignore = "permanently clears real YNote and QQLive caches"]
    fn real_ynote_qqlive_caches_preserve_durable_state() {
        assert_eq!(
            std::env::var("MANGODISK_TEST_REAL_YNOTE_QQLIVE_CACHE").as_deref(),
            Ok("1"),
            "set MANGODISK_TEST_REAL_YNOTE_QQLIVE_CACHE=1 to authorize this real cache diagnostic"
        );
        let process_snapshot =
            ProcessSnapshot::capture().expect("the macOS process inventory must be available");
        for process_name in ["\u{6709}\u{9053}\u{4e91}\u{7b14}\u{8bb0}", "QQLive"] {
            assert!(
                process_snapshot
                    .matching_processes(&[process_name.to_string()])
                    .is_empty(),
                "both cache owners must be stopped before cleanup"
            );
        }

        let home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .expect("HOME must be available");
        let application_support = home.join("Library/Application Support");
        let ynote_root = application_support.join("ynote-desktop");
        let qqlive_root = application_support.join("com.tencent.mac.marvis");
        assert!(ynote_root.is_dir() && qqlive_root.is_dir());

        // The account directory name is private. Locate it only through the
        // stable ynote-data child and never include its actual name in output.
        let ynote_account_root = direct_directory_children(&ynote_root)
            .into_iter()
            .find(|path| path.join("ynote-data").is_dir())
            .expect("the initialized YNote profile must expose durable note data");
        let ynote_data = ynote_account_root.join("ynote-data");
        let ynote_partitions = direct_directory_children(&ynote_root.join("Partitions"));
        let qqlive_partitions = direct_directory_children(&qqlive_root.join("Partitions"));
        assert!(
            !ynote_partitions.is_empty() && qqlive_partitions.len() >= 2,
            "both initialized applications must expose renderer partitions"
        );

        let mut preserved_paths = vec![ynote_data];
        for relative in [
            "setting.json",
            "browser-settings.json",
            "Cookies",
            "Preferences",
            "Local Storage",
            "IndexedDB",
            "databases",
            "storage",
            "Session Storage",
            "Network Persistent State",
        ] {
            let path = ynote_root.join(relative);
            if path.exists() {
                preserved_paths.push(path);
            }
        }
        for partition in &ynote_partitions {
            for relative in [
                "Cookies",
                "Preferences",
                "Local Storage",
                "IndexedDB",
                "Session Storage",
                "Network Persistent State",
            ] {
                let path = partition.join(relative);
                if path.exists() {
                    preserved_paths.push(path);
                }
            }
        }

        for relative in [
            "components",
            "OfflinePack",
            "Knowledgebase",
            "services",
            "MarvisData",
            "Cookies",
            "Preferences",
            "Local Storage",
            "IndexedDB",
            "Session Storage",
            "WebStorage",
            "marvis-login-state.json",
            "marvis-settings.json",
            "installed.json",
        ] {
            let path = qqlive_root.join(relative);
            if path.exists() {
                preserved_paths.push(path);
            }
        }
        for partition in &qqlive_partitions {
            for relative in [
                "Cookies",
                "Preferences",
                "Local Storage",
                "Session Storage",
                "Network Persistent State",
                "blob_storage",
            ] {
                let path = partition.join(relative);
                if path.exists() {
                    preserved_paths.push(path);
                }
            }
        }
        assert!(
            preserved_paths.len() >= 25,
            "the real profiles must expose representative durable state"
        );
        let preserved_before = preserved_paths
            .iter()
            .map(|path| digest_macos_tree_without_following_links(path))
            .collect::<Vec<_>>();

        let cache_suffixes = [
            "Cache",
            "Code Cache",
            "GPUCache",
            "DawnCache",
            "DawnGraphiteCache",
            "DawnWebGPUCache",
            "GrShaderCache",
            "GraphiteDawnCache",
            "Shared Dictionary/cache",
        ];
        let mut target_roots = Vec::new();
        for relative in [
            "Cache",
            "Code Cache",
            "GPUCache",
            "DawnCache",
            "DawnGraphiteCache",
            "DawnWebGPUCache",
            "GrShaderCache",
            "GraphiteDawnCache",
            "Shared Dictionary/cache",
            "Crashpad/reports",
            "myLogs",
        ] {
            let path = ynote_root.join(relative);
            if path.is_dir() {
                target_roots.push(path);
            }
        }
        let ynote_updater = application_support.join("Caches/ynote-desktop-updater");
        if ynote_updater.is_dir() {
            target_roots.push(ynote_updater);
        }
        for partition in &ynote_partitions {
            for suffix in cache_suffixes {
                let path = partition.join(suffix);
                if path.is_dir() {
                    target_roots.push(path);
                }
            }
        }

        for relative in [
            "Cache",
            "Code Cache",
            "GPUCache",
            "DawnCache",
            "DawnGraphiteCache",
            "DawnWebGPUCache",
            "GrShaderCache",
            "GraphiteDawnCache",
            "Shared Dictionary/cache",
            "Crashpad/reports",
            "icon_cache",
        ] {
            let path = qqlive_root.join(relative);
            if path.is_dir() {
                target_roots.push(path);
            }
        }
        for path in [
            home.join("Library/Caches/com.tencent.tenvideo"),
            application_support.join("Caches/com.tencent.tenvideo"),
        ] {
            if path.is_dir() {
                target_roots.push(path);
            }
        }
        for partition in &qqlive_partitions {
            for suffix in cache_suffixes {
                let path = partition.join(suffix);
                if path.is_dir() {
                    target_roots.push(path);
                }
            }
        }
        target_roots.sort();
        target_roots.dedup();
        assert!(
            target_roots.len() >= 20,
            "the real profiles must expose verified cache leaves"
        );

        let markers = target_roots
            .iter()
            .map(|root| root.join("mangodisk-rule-validation.bin"))
            .collect::<Vec<_>>();
        for marker in &markers {
            fs::write(marker, b"payload").expect("the cache marker must be created");
        }
        let request = |dry_run| CleanupRequest {
            rule_ids: vec![
                "app.ynote-cache".to_string(),
                "app.qqlive-rendering-cache".to_string(),
            ],
            source_selections: Vec::new(),
            dry_run,
            project_roots: Vec::new(),
        };

        let preview =
            CleanupService::execute(request(true)).expect("the real cache dry run must succeed");
        assert_eq!(preview.actions.len(), 2);
        assert_eq!(preview.failed_item_count, 0, "{:?}", preview.actions);
        assert!(preview.expected_bytes >= markers.len() as u64 * 7);
        assert!(markers.iter().all(|marker| marker.exists()));

        let result =
            CleanupService::execute(request(false)).expect("the real cache cleanup must succeed");
        assert_eq!(result.actions.len(), 2);
        assert_eq!(result.failed_item_count, 0, "{:?}", result.actions);
        assert!(result.released_bytes >= markers.len() as u64 * 7);
        assert!(markers.iter().all(|marker| !marker.exists()));
        let preserved_after = preserved_paths
            .iter()
            .map(|path| digest_macos_tree_without_following_links(path))
            .collect::<Vec<_>>();
        assert_eq!(preserved_after, preserved_before);
        println!(
            "real_macos_ynote_qqlive_cache_cleanup expected_bytes={} released_bytes={} affected_item_count={} ynote_partition_count={} qqlive_partition_count={} target_root_count={} preserved_root_count={}",
            preview.expected_bytes,
            result.released_bytes,
            result.affected_item_count,
            ynote_partitions.len(),
            qqlive_partitions.len(),
            target_roots.len(),
            preserved_paths.len()
        );
    }

    /// Proves that the production process snapshot recognizes the executable
    /// from the notarized FlashVoice bundle before its WebKit cache is touched.
    #[test]
    #[ignore = "requires the real FlashVoice application to be running"]
    fn real_flashvoice_cache_blocks_while_running() {
        assert_eq!(
            std::env::var("MANGODISK_TEST_REAL_FLASHVOICE_CACHE_BLOCK").as_deref(),
            Ok("1"),
            "set MANGODISK_TEST_REAL_FLASHVOICE_CACHE_BLOCK=1 for the real process-gate diagnostic"
        );
        let process_snapshot =
            ProcessSnapshot::capture().expect("the macOS process inventory must be available");
        assert!(
            !process_snapshot
                .matching_processes(&["FlashVoice".to_string()])
                .is_empty(),
            "the real FlashVoice application must be running"
        );

        let result = CleanupService::execute(CleanupRequest {
            rule_ids: vec!["app.flashvoice-cache".to_string()],
            source_selections: Vec::new(),
            dry_run: false,
            project_roots: Vec::new(),
        })
        .expect("the blocked cleanup must return a structured result");
        assert_eq!(result.actions.len(), 1);
        let action = &result.actions[0];
        assert_eq!(action.status, crate::cleanup::CleanupActionStatus::Blocked);
        assert_eq!(
            action.reason_code,
            Some(crate::cleanup::CleanupActionReason::RunningProcesses)
        );
        assert_eq!(action.released_bytes, 0);
        assert_eq!(action.affected_item_count, 0);
        assert!(!action.running_processes.is_empty());
        println!("real_macos_flashvoice_cache_block owner_count=1");
    }

    /// Clears the real FlashVoice WebKit cache and diagnostics only after the
    /// application stops. Full hashes cover downloaded speech models, audio
    /// recordings, transcription indexes, configuration, and window state.
    #[test]
    #[ignore = "permanently clears real FlashVoice caches"]
    fn real_flashvoice_cache_preserves_models_and_recordings() {
        assert_eq!(
            std::env::var("MANGODISK_TEST_REAL_FLASHVOICE_CACHE").as_deref(),
            Ok("1"),
            "set MANGODISK_TEST_REAL_FLASHVOICE_CACHE=1 to authorize this real cache diagnostic"
        );
        let process_snapshot =
            ProcessSnapshot::capture().expect("the macOS process inventory must be available");
        assert!(
            process_snapshot
                .matching_processes(&["FlashVoice".to_string()])
                .is_empty(),
            "FlashVoice must be stopped before cleanup"
        );

        let home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .expect("HOME must be available");
        let support = home.join("Library/Application Support/com.flashvoices");
        let cache = home.join("Library/Caches/FlashVoice");
        assert!(support.is_dir() && cache.is_dir());

        let mut preserved_paths = Vec::new();
        for relative in [
            "models",
            "recordings",
            "transcriptions",
            "fv_config.json",
            "fv_onboarding.json",
            "fv_recordings.json",
            "fv_transcriptions.json",
            "installation.json",
            ".persisted-scope",
            ".persisted-scope-asset",
            ".window-state.json",
        ] {
            let path = support.join(relative);
            if path.exists() {
                preserved_paths.push(path);
            }
        }
        assert!(
            preserved_paths.len() >= 10,
            "the initialized application must expose durable voice state"
        );
        let preserved_before = preserved_paths
            .iter()
            .map(|path| digest_macos_tree(path, &[]))
            .collect::<Vec<_>>();

        let logs = support.join("logs");
        assert!(logs.is_dir());
        let target_roots = [cache, logs];
        let markers = target_roots
            .iter()
            .map(|root| root.join("mangodisk-rule-validation.bin"))
            .collect::<Vec<_>>();
        for marker in &markers {
            fs::write(marker, b"payload").expect("the cache marker must be created");
        }
        let request = |dry_run| CleanupRequest {
            rule_ids: vec!["app.flashvoice-cache".to_string()],
            source_selections: Vec::new(),
            dry_run,
            project_roots: Vec::new(),
        };

        let preview = CleanupService::execute(request(true))
            .expect("the real FlashVoice cache dry run must succeed");
        assert_eq!(preview.actions.len(), 1);
        assert_eq!(preview.failed_item_count, 0, "{:?}", preview.actions);
        assert!(preview.expected_bytes >= markers.len() as u64 * 7);
        assert!(markers.iter().all(|marker| marker.exists()));

        let result = CleanupService::execute(request(false))
            .expect("the real FlashVoice cache cleanup must succeed");
        assert_eq!(result.actions.len(), 1);
        assert_eq!(result.failed_item_count, 0, "{:?}", result.actions);
        assert!(result.released_bytes >= markers.len() as u64 * 7);
        assert!(markers.iter().all(|marker| !marker.exists()));
        let preserved_after = preserved_paths
            .iter()
            .map(|path| digest_macos_tree(path, &[]))
            .collect::<Vec<_>>();
        assert_eq!(preserved_after, preserved_before);
        println!(
            "real_macos_flashvoice_cache_cleanup expected_bytes={} released_bytes={} affected_item_count={} target_root_count={} preserved_root_count={}",
            preview.expected_bytes,
            result.released_bytes,
            result.affected_item_count,
            target_roots.len(),
            preserved_paths.len()
        );
    }

    /// Verifies that every newly covered macOS client is recognized by the
    /// production process snapshot before any mixed-purpose data root is read.
    #[test]
    #[ignore = "requires uTools, Clash Verge, and Youdao Dictionary to be running"]
    fn real_utools_clash_youdao_caches_block_running_owners() {
        assert_eq!(
            std::env::var("MANGODISK_TEST_REAL_MACOS_UTILITY_CACHE_BLOCK").as_deref(),
            Ok("1"),
            "set MANGODISK_TEST_REAL_MACOS_UTILITY_CACHE_BLOCK=1 for the real process-gate diagnostic"
        );
        let process_snapshot =
            ProcessSnapshot::capture().expect("the macOS process inventory must be available");
        for process_name in [
            "uTools",
            "clash-verge",
            "\u{7f51}\u{6613}\u{6709}\u{9053}\u{7ffb}\u{8bd1}",
        ] {
            assert!(
                !process_snapshot
                    .matching_processes(&[process_name.to_string()])
                    .is_empty(),
                "the real {process_name} application must be running"
            );
        }

        let result = CleanupService::execute(CleanupRequest {
            rule_ids: vec![
                "app.utools-rendering-cache".to_string(),
                "app.clash-verge-diagnostic-cache".to_string(),
                "app.youdao-translation-cache".to_string(),
            ],
            source_selections: Vec::new(),
            dry_run: false,
            project_roots: Vec::new(),
        })
        .expect("the blocked cleanup must return structured results");
        assert_eq!(result.actions.len(), 3);
        for action in &result.actions {
            assert_eq!(action.status, crate::cleanup::CleanupActionStatus::Blocked);
            assert_eq!(
                action.reason_code,
                Some(crate::cleanup::CleanupActionReason::RunningProcesses)
            );
            assert_eq!(action.released_bytes, 0);
            assert_eq!(action.affected_item_count, 0);
            assert!(!action.running_processes.is_empty());
        }
        println!("real_macos_utility_cache_block application_count=3");
    }

    /// Clears only renderer caches, diagnostics, and the application sandbox
    /// cache directory. Digests deliberately cover clipboard history, plugins,
    /// databases, proxy profiles and configuration, dictionaries, preferences,
    /// documents, and durable browser storage before and after production cleanup.
    #[test]
    #[ignore = "permanently clears real uTools, Clash Verge, and Youdao caches"]
    fn real_utools_clash_youdao_caches_preserve_durable_state() {
        assert_eq!(
            std::env::var("MANGODISK_TEST_REAL_MACOS_UTILITY_CACHE").as_deref(),
            Ok("1"),
            "set MANGODISK_TEST_REAL_MACOS_UTILITY_CACHE=1 to authorize this real cache diagnostic"
        );
        let process_snapshot =
            ProcessSnapshot::capture().expect("the macOS process inventory must be available");
        for process_name in [
            "uTools",
            "clash-verge",
            "\u{7f51}\u{6613}\u{6709}\u{9053}\u{7ffb}\u{8bd1}",
        ] {
            assert!(
                process_snapshot
                    .matching_processes(&[process_name.to_string()])
                    .is_empty(),
                "{process_name} must be stopped before cleanup"
            );
        }

        let home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .expect("HOME must be available");
        let application_support = home.join("Library/Application Support");
        let utools_root = application_support.join("uTools");
        let clash_root = application_support.join("io.github.clash-verge-rev.clash-verge-rev");
        let youdao_data = home.join("Library/Containers/com.youdao.YoudaoDict/Data");
        let youdao_library = youdao_data.join("Library");
        let youdao_cache = youdao_library.join("Caches");
        assert!(utools_root.is_dir() && clash_root.is_dir() && youdao_cache.is_dir());

        let mut preserved_paths = vec![
            (utools_root.join("clipboard-data"), Vec::new()),
            (utools_root.join("plugins"), Vec::new()),
            (utools_root.join("database"), Vec::new()),
            (utools_root.join("Local Storage"), Vec::new()),
            (clash_root.clone(), vec!["logs"]),
            (youdao_library, vec!["Caches"]),
        ];
        let youdao_documents = youdao_data.join("Documents");
        if youdao_documents.exists() {
            preserved_paths.push((youdao_documents, Vec::new()));
        }

        let utools_partitions = direct_directory_children(&utools_root.join("Partitions"));
        for partition in &utools_partitions {
            for relative in [
                "Local Storage",
                "IndexedDB",
                "Session Storage",
                "WebStorage",
                "Network",
                "Cookies",
                "Preferences",
                "History",
                "Service Worker/Database",
                "Service Worker/ScriptCache",
            ] {
                let path = partition.join(relative);
                if path.exists() {
                    preserved_paths.push((path, Vec::new()));
                }
            }
        }
        preserved_paths.retain(|(path, _)| path.exists());
        assert!(
            preserved_paths.len() >= 7,
            "the initialized clients must expose durable application state"
        );
        let preserved_before = preserved_paths
            .iter()
            .map(|(path, excluded)| {
                digest_macos_tree_with_exclusions_without_following_links(path, excluded)
            })
            .collect::<Vec<_>>();

        let cache_suffixes = [
            "Cache",
            "Code Cache",
            "GPUCache",
            "DawnCache",
            "DawnGraphiteCache",
            "DawnWebGPUCache",
            "GrShaderCache",
            "GraphiteDawnCache",
            "Shared Dictionary/cache",
            "Service Worker/CacheStorage",
        ];
        let mut target_roots = Vec::new();
        for relative in [
            "Cache",
            "Code Cache",
            "GPUCache",
            "DawnCache",
            "DawnGraphiteCache",
            "DawnWebGPUCache",
            "GrShaderCache",
            "GraphiteDawnCache",
            "Shared Dictionary/cache",
            "Service Worker/CacheStorage",
            "logs",
            "Crashpad/reports",
        ] {
            let path = utools_root.join(relative);
            if path.is_dir() {
                target_roots.push(path);
            }
        }
        for partition in &utools_partitions {
            for suffix in cache_suffixes {
                let path = partition.join(suffix);
                if path.is_dir() {
                    target_roots.push(path);
                }
            }
        }
        let clash_logs = clash_root.join("logs");
        if clash_logs.is_dir() {
            target_roots.push(clash_logs);
        }
        target_roots.push(youdao_cache);
        target_roots.sort();
        target_roots.dedup();
        assert!(
            target_roots.len() >= 8,
            "the initialized clients must expose verified cache roots"
        );

        let markers = target_roots
            .iter()
            .map(|root| root.join("mangodisk-rule-validation.bin"))
            .collect::<Vec<_>>();
        for marker in &markers {
            fs::write(marker, b"payload").expect("the cache marker must be created");
        }
        let request = |dry_run| CleanupRequest {
            rule_ids: vec![
                "app.utools-rendering-cache".to_string(),
                "app.clash-verge-diagnostic-cache".to_string(),
                "app.youdao-translation-cache".to_string(),
            ],
            source_selections: Vec::new(),
            dry_run,
            project_roots: Vec::new(),
        };

        let preview =
            CleanupService::execute(request(true)).expect("the real cache dry run must succeed");
        assert_eq!(preview.actions.len(), 3);
        assert_eq!(preview.failed_item_count, 0, "{:?}", preview.actions);
        assert!(preview.expected_bytes >= markers.len() as u64 * 7);
        assert!(markers.iter().all(|marker| marker.exists()));

        let result =
            CleanupService::execute(request(false)).expect("the real cache cleanup must succeed");
        assert_eq!(result.actions.len(), 3);
        assert_eq!(result.failed_item_count, 0, "{:?}", result.actions);
        assert!(result.released_bytes >= markers.len() as u64 * 7);
        assert!(markers.iter().all(|marker| !marker.exists()));
        let preserved_after = preserved_paths
            .iter()
            .map(|(path, excluded)| {
                digest_macos_tree_with_exclusions_without_following_links(path, excluded)
            })
            .collect::<Vec<_>>();
        assert_eq!(preserved_after, preserved_before);
        println!(
            "real_macos_utility_cache_cleanup expected_bytes={} released_bytes={} affected_item_count={} utools_partition_count={} target_root_count={} preserved_root_count={}",
            preview.expected_bytes,
            result.released_bytes,
            result.affected_item_count,
            utools_partitions.len(),
            target_roots.len(),
            preserved_paths.len()
        );
    }

    /// Confirms that each newly covered client reaches the production process
    /// gate before MangoDisk traverses a mixed-purpose application data root.
    /// WorkBuddy's main executable is generically named Electron, so its unique
    /// helper process is used to avoid blocking unrelated Electron clients.
    #[test]
    #[ignore = "requires BaiduNetdisk, Manus, and WorkBuddy to be running"]
    fn real_baidu_manus_workbuddy_caches_block_running_owners() {
        assert_eq!(
            std::env::var("MANGODISK_TEST_REAL_MACOS_AI_WORKSPACE_CACHE_BLOCK").as_deref(),
            Ok("1"),
            "set MANGODISK_TEST_REAL_MACOS_AI_WORKSPACE_CACHE_BLOCK=1 for the real process-gate diagnostic"
        );
        let process_snapshot =
            ProcessSnapshot::capture().expect("the macOS process inventory must be available");
        for process_name in ["BaiduNetdisk", "Manus", "WorkBuddy Helper"] {
            assert!(
                !process_snapshot
                    .matching_processes(&[process_name.to_string()])
                    .is_empty(),
                "the real {process_name} process must be running"
            );
        }

        let result = CleanupService::execute(CleanupRequest {
            rule_ids: vec![
                "app.baidu-netdisk-rendering-cache".to_string(),
                "app.manus-rendering-cache".to_string(),
                "app.workbuddy-rendering-cache".to_string(),
            ],
            source_selections: Vec::new(),
            dry_run: false,
            project_roots: Vec::new(),
        })
        .expect("the blocked cleanup must return structured results");
        assert_eq!(result.actions.len(), 3);
        for action in &result.actions {
            assert_eq!(action.status, crate::cleanup::CleanupActionStatus::Blocked);
            assert_eq!(
                action.reason_code,
                Some(crate::cleanup::CleanupActionReason::RunningProcesses)
            );
            assert_eq!(action.released_bytes, 0);
            assert_eq!(action.affected_item_count, 0);
            assert!(!action.running_processes.is_empty());
        }
        println!("real_macos_ai_workspace_cache_block application_count=3");
    }

    /// Clears only fixed Electron renderer caches after all three clients stop.
    /// Opaque-link digests protect Baidu sync/account state, Manus browser and
    /// task state, and WorkBuddy tasks, projects, databases, connectors, and
    /// partition storage without following sandbox or singleton links.
    #[test]
    #[ignore = "permanently clears real BaiduNetdisk, Manus, and WorkBuddy caches"]
    fn real_baidu_manus_workbuddy_caches_preserve_durable_state() {
        assert_eq!(
            std::env::var("MANGODISK_TEST_REAL_MACOS_AI_WORKSPACE_CACHE").as_deref(),
            Ok("1"),
            "set MANGODISK_TEST_REAL_MACOS_AI_WORKSPACE_CACHE=1 to authorize this real cache diagnostic"
        );
        let process_snapshot =
            ProcessSnapshot::capture().expect("the macOS process inventory must be available");
        for process_name in [
            "BaiduNetdisk",
            "Manus",
            "WorkBuddy Helper",
            "WorkBuddy Helper (GPU)",
            "WorkBuddy Helper (Renderer)",
            "WorkBuddyRepair",
        ] {
            assert!(
                process_snapshot
                    .matching_processes(&[process_name.to_string()])
                    .is_empty(),
                "{process_name} must be stopped before cleanup"
            );
        }

        let home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .expect("HOME must be available");
        let baidu_data = home.join("Library/Containers/com.baidu.netdisk/Data");
        let baidu_support = baidu_data.join("Library/Application Support");
        let baidu_renderer = baidu_support.join("baidunetdisk");
        let manus_root = home.join("Library/Application Support/Manus");
        let workbuddy_root = home.join(".workbuddy");
        let workbuddy_session = workbuddy_root.join("app/session");
        assert!(baidu_renderer.is_dir() && manus_root.is_dir() && workbuddy_session.is_dir());

        let mut preserved_paths = vec![
            baidu_support.join("com.baidu.netdisk"),
            home.join("Library/Group Containers/group.com.baidu.BaiduNetdisk-Mac"),
            home.join("Library/Group Containers/group.com.baidu.netdisk"),
            home.join("Library/Group Containers/LKD5676Y5W.com.baidu.netdisk"),
        ];
        for relative in [
            "Preferences",
            "Cookies",
            "IndexedDB",
            "Local Storage",
            "Session Storage",
            "Storage",
            "WebStorage",
            "databases",
        ] {
            preserved_paths.push(baidu_renderer.join(relative));
        }
        for relative in [
            "Preferences",
            "Cookies",
            "IndexedDB",
            "Local Storage",
            "Session Storage",
            "WebStorage",
            "Network Persistent State",
            "localStorage.json",
            "window-state.json",
            ".updaterId",
        ] {
            preserved_paths.push(manus_root.join(relative));
        }
        for relative in [
            "workbuddy.db",
            "settings.json",
            "user-state.json",
            "workspace-state.json",
            ".mcp.json",
            "projects",
            "tasks",
            "automation-backups",
            "local_storage",
            "connectors",
            "memory",
        ] {
            preserved_paths.push(workbuddy_root.join(relative));
        }
        for relative in [
            "Preferences",
            "Cookies",
            "DIPS",
            "IndexedDB",
            "Local Storage",
            "Session Storage",
            "WebStorage",
        ] {
            preserved_paths.push(workbuddy_session.join(relative));
        }
        let workbuddy_partitions = direct_directory_children(&workbuddy_session.join("Partitions"));
        for partition in &workbuddy_partitions {
            for relative in [
                "Preferences",
                "Cookies",
                "DIPS",
                "IndexedDB",
                "Local Storage",
                "Session Storage",
                "WebStorage",
            ] {
                preserved_paths.push(partition.join(relative));
            }
        }
        preserved_paths.retain(|path| path.exists());
        preserved_paths.sort();
        preserved_paths.dedup();
        assert!(
            preserved_paths.len() >= 30,
            "the initialized clients must expose durable account, task, project, and browser state"
        );
        let preserved_before = preserved_paths
            .iter()
            .map(|path| digest_macos_tree_without_following_links(path))
            .collect::<Vec<_>>();

        let cache_suffixes = [
            "Cache",
            "Code Cache",
            "GPUCache",
            "DawnCache",
            "DawnGraphiteCache",
            "DawnWebGPUCache",
            "GrShaderCache",
            "GraphiteDawnCache",
            "Shared Dictionary/cache",
            "Service Worker/CacheStorage",
        ];
        let mut target_roots = Vec::new();
        for root in [&baidu_renderer, &manus_root, &workbuddy_session] {
            for suffix in cache_suffixes {
                let path = root.join(suffix);
                if path.is_dir() {
                    target_roots.push(path);
                }
            }
        }
        for partition in &workbuddy_partitions {
            for suffix in cache_suffixes {
                let path = partition.join(suffix);
                if path.is_dir() {
                    target_roots.push(path);
                }
            }
        }
        target_roots.sort();
        target_roots.dedup();
        assert!(
            target_roots.len() >= 25,
            "the initialized clients must expose verified fixed renderer-cache roots"
        );

        let markers = target_roots
            .iter()
            .map(|root| root.join("mangodisk-rule-validation.bin"))
            .collect::<Vec<_>>();
        for marker in &markers {
            fs::write(marker, b"payload").expect("the cache marker must be created");
        }
        let request = |dry_run| CleanupRequest {
            rule_ids: vec![
                "app.baidu-netdisk-rendering-cache".to_string(),
                "app.manus-rendering-cache".to_string(),
                "app.workbuddy-rendering-cache".to_string(),
            ],
            source_selections: Vec::new(),
            dry_run,
            project_roots: Vec::new(),
        };

        let preview =
            CleanupService::execute(request(true)).expect("the real cache dry run must succeed");
        assert_eq!(preview.actions.len(), 3);
        assert_eq!(preview.failed_item_count, 0, "{:?}", preview.actions);
        assert!(preview.expected_bytes >= markers.len() as u64 * 7);
        assert!(markers.iter().all(|marker| marker.exists()));

        let result =
            CleanupService::execute(request(false)).expect("the real cache cleanup must succeed");
        assert_eq!(result.actions.len(), 3);
        assert_eq!(result.failed_item_count, 0, "{:?}", result.actions);
        assert!(result.released_bytes >= markers.len() as u64 * 7);
        assert!(markers.iter().all(|marker| !marker.exists()));
        let preserved_after = preserved_paths
            .iter()
            .map(|path| digest_macos_tree_without_following_links(path))
            .collect::<Vec<_>>();
        assert_eq!(preserved_after, preserved_before);
        println!(
            "real_macos_ai_workspace_cache_cleanup expected_bytes={} released_bytes={} affected_item_count={} workbuddy_partition_count={} target_root_count={} preserved_root_count={}",
            preview.expected_bytes,
            result.released_bytes,
            result.affected_item_count,
            workbuddy_partitions.len(),
            target_roots.len(),
            preserved_paths.len()
        );
    }

    /// Confirms that update and renderer cleanup never starts while the two
    /// owning desktop clients can still mutate their cache directories.
    #[test]
    #[ignore = "requires the real Manus and Claude applications to be running"]
    fn real_manus_update_and_claude_cache_block_running_owners() {
        assert_eq!(
            std::env::var("MANGODISK_TEST_REAL_MACOS_MANUS_CLAUDE_CACHE_BLOCK").as_deref(),
            Ok("1"),
            "set MANGODISK_TEST_REAL_MACOS_MANUS_CLAUDE_CACHE_BLOCK=1 for the real process-gate diagnostic"
        );
        let process_snapshot =
            ProcessSnapshot::capture().expect("the macOS process inventory must be available");
        for process_name in ["Manus", "Claude"] {
            assert!(
                !process_snapshot
                    .matching_processes(&[process_name.to_string()])
                    .is_empty(),
                "the real {process_name} process must be running"
            );
        }

        let result = CleanupService::execute(CleanupRequest {
            rule_ids: vec![
                "app.manus-update-cache".to_string(),
                "app.claude-cache".to_string(),
            ],
            source_selections: Vec::new(),
            dry_run: false,
            project_roots: Vec::new(),
        })
        .expect("the blocked cleanup must return structured results");
        assert_eq!(result.actions.len(), 2);
        for action in &result.actions {
            assert_eq!(action.status, crate::cleanup::CleanupActionStatus::Blocked);
            assert_eq!(
                action.reason_code,
                Some(crate::cleanup::CleanupActionReason::RunningProcesses)
            );
            assert_eq!(action.released_bytes, 0);
            assert_eq!(action.affected_item_count, 0);
            assert!(!action.running_processes.is_empty());
        }
        println!("real_macos_manus_claude_cache_block application_count=2");
    }

    /// Executes the production rules against a stale downloaded Manus update
    /// and Claude's fixed Electron cache leaves. The digest set covers all
    /// Manus application state and Claude's sessions, credentials, local-agent
    /// work, configuration, and partition storage without following links.
    #[test]
    #[ignore = "permanently clears real Manus update and Claude renderer caches"]
    fn real_manus_update_and_claude_cache_preserve_durable_state() {
        assert_eq!(
            std::env::var("MANGODISK_TEST_REAL_MACOS_MANUS_CLAUDE_CACHE").as_deref(),
            Ok("1"),
            "set MANGODISK_TEST_REAL_MACOS_MANUS_CLAUDE_CACHE=1 to authorize this real cache diagnostic"
        );
        let process_snapshot =
            ProcessSnapshot::capture().expect("the macOS process inventory must be available");
        for process_name in ["Manus", "Claude"] {
            assert!(
                process_snapshot
                    .matching_processes(&[process_name.to_string()])
                    .is_empty(),
                "{process_name} must be stopped before cleanup"
            );
        }

        let home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .expect("HOME must be available");
        let application_support = home.join("Library/Application Support");
        let user_caches = home.join("Library/Caches");
        let manus_root = application_support.join("Manus");
        let claude_root = application_support.join("Claude");
        assert!(manus_root.is_dir() && claude_root.is_dir());

        let mut preserved_paths = vec![(manus_root, Vec::new())];
        for candidate in [
            home.join("Library/Preferences/im.manus.desktop.plist"),
            home.join("Library/Preferences/com.anthropic.claudefordesktop.plist"),
            claude_root.join("Preferences"),
            claude_root.join("Cookies"),
            claude_root.join("IndexedDB"),
            claude_root.join("Local Storage"),
            claude_root.join("Session Storage"),
            claude_root.join("WebStorage"),
            claude_root.join("Network Persistent State"),
            claude_root.join("local-agent-mode-sessions"),
            claude_root.join("claude_desktop_config.json"),
        ] {
            preserved_paths.push((candidate, Vec::new()));
        }
        let shared_dictionary = claude_root.join("Shared Dictionary");
        if shared_dictionary.exists() {
            preserved_paths.push((shared_dictionary, vec!["cache"]));
        }

        let claude_partitions = direct_directory_children(&claude_root.join("Partitions"));
        for partition in &claude_partitions {
            for relative in [
                "Preferences",
                "Cookies",
                "DIPS",
                "IndexedDB",
                "Local Storage",
                "Session Storage",
                "WebStorage",
            ] {
                let candidate = partition.join(relative);
                if candidate.exists() {
                    preserved_paths.push((candidate, Vec::new()));
                }
            }
            let partition_dictionary = partition.join("Shared Dictionary");
            if partition_dictionary.exists() {
                preserved_paths.push((partition_dictionary, vec!["cache"]));
            }
        }
        preserved_paths.retain(|(path, _)| path.exists());
        assert!(
            preserved_paths.len() >= 12,
            "the initialized clients must expose durable application and browser state"
        );
        let preserved_before = preserved_paths
            .iter()
            .map(|(path, excluded)| {
                digest_macos_tree_with_exclusions_without_following_links(path, excluded)
            })
            .collect::<Vec<_>>();

        let cache_suffixes = [
            "Cache",
            "Code Cache",
            "GPUCache",
            "DawnCache",
            "DawnGraphiteCache",
            "DawnWebGPUCache",
            "GrShaderCache",
            "GraphiteDawnCache",
            "Shared Dictionary/cache",
            "Service Worker/CacheStorage",
            "Crashpad/reports",
        ];
        let mut target_roots = Vec::new();
        for candidate in [
            user_caches.join("manus-updater"),
            user_caches.join("im.manus.desktop"),
            user_caches.join("com.anthropic.claudefordesktop"),
        ] {
            if candidate.is_dir() {
                target_roots.push(candidate);
            }
        }
        for cache_suffix in cache_suffixes {
            let candidate = claude_root.join(cache_suffix);
            if candidate.is_dir() {
                target_roots.push(candidate);
            }
        }
        for partition in &claude_partitions {
            for cache_suffix in cache_suffixes {
                let candidate = partition.join(cache_suffix);
                if candidate.is_dir() {
                    target_roots.push(candidate);
                }
            }
        }
        target_roots.sort();
        target_roots.dedup();
        assert!(
            target_roots.len() >= 14,
            "the initialized clients must expose downloaded update and renderer-cache roots"
        );

        let markers = target_roots
            .iter()
            .map(|root| root.join("mangodisk-rule-validation.bin"))
            .collect::<Vec<_>>();
        for marker in &markers {
            fs::write(marker, b"payload").expect("the cache marker must be created");
        }
        let request = |dry_run| CleanupRequest {
            rule_ids: vec![
                "app.manus-update-cache".to_string(),
                "app.claude-cache".to_string(),
            ],
            source_selections: Vec::new(),
            dry_run,
            project_roots: Vec::new(),
        };

        let preview = CleanupService::execute(request(true))
            .expect("the real Manus and Claude cache dry run must succeed");
        assert_eq!(preview.actions.len(), 2);
        assert_eq!(preview.failed_item_count, 0, "{:?}", preview.actions);
        assert!(preview.expected_bytes >= markers.len() as u64 * 7);
        assert!(markers.iter().all(|marker| marker.exists()));

        let result = CleanupService::execute(request(false))
            .expect("the real Manus and Claude cache cleanup must succeed");
        assert_eq!(result.actions.len(), 2);
        assert_eq!(result.failed_item_count, 0, "{:?}", result.actions);
        assert!(result.released_bytes >= markers.len() as u64 * 7);
        assert!(markers.iter().all(|marker| !marker.exists()));
        let preserved_after = preserved_paths
            .iter()
            .map(|(path, excluded)| {
                digest_macos_tree_with_exclusions_without_following_links(path, excluded)
            })
            .collect::<Vec<_>>();
        assert_eq!(preserved_after, preserved_before);
        println!(
            "real_macos_manus_claude_cache_cleanup expected_bytes={} released_bytes={} affected_item_count={} claude_partition_count={} target_root_count={} preserved_root_count={}",
            preview.expected_bytes,
            result.released_bytes,
            result.affected_item_count,
            claude_partitions.len(),
            target_roots.len(),
            preserved_paths.len()
        );
    }

    /// Confirms that the production rule reaches the process gate before it
    /// can traverse Tencent Meeting's cache. The client keeps authenticated
    /// account and meeting state in a separate sandbox container, but it can
    /// still mutate the selected WebKit cache while running.
    #[test]
    #[ignore = "requires the real Tencent Meeting application to be running"]
    fn real_tencent_meeting_cache_blocks_while_running() {
        assert_eq!(
            std::env::var("MANGODISK_TEST_REAL_MACOS_TENCENT_MEETING_CACHE_BLOCK").as_deref(),
            Ok("1"),
            "set MANGODISK_TEST_REAL_MACOS_TENCENT_MEETING_CACHE_BLOCK=1 for the real process-gate diagnostic"
        );
        let running = ProcessSnapshot::capture()
            .expect("the macOS process inventory must be available")
            .matching_processes(&["TencentMeeting".to_string()]);
        assert!(!running.is_empty(), "Tencent Meeting must be running");

        let result = CleanupService::execute(CleanupRequest {
            rule_ids: vec!["app.tencent-meeting-cache".to_string()],
            source_selections: Vec::new(),
            dry_run: false,
            project_roots: Vec::new(),
        })
        .expect("the blocked Tencent Meeting cleanup must return a structured result");
        assert_eq!(result.actions.len(), 1);
        let action = &result.actions[0];
        assert_eq!(action.status, crate::cleanup::CleanupActionStatus::Blocked);
        assert_eq!(
            action.reason_code,
            Some(crate::cleanup::CleanupActionReason::RunningProcesses)
        );
        assert_eq!(action.released_bytes, 0);
        assert_eq!(action.affected_item_count, 0);
        assert!(!action.running_processes.is_empty());
        println!(
            "real_macos_tencent_meeting_cache_block running_process_count={}",
            action.running_processes.len()
        );
    }

    /// Clears only WebKit's fixed NetworkCache. Separate digests cover the
    /// HSTS, CacheStorage, and transport databases plus representative account,
    /// meeting, cookie, browser-storage, preference, and download state inside
    /// the sandbox container.
    #[test]
    #[ignore = "permanently clears the real Tencent Meeting WebKit network cache"]
    fn real_tencent_meeting_cache_preserves_account_and_meeting_state() {
        assert_eq!(
            std::env::var("MANGODISK_TEST_REAL_MACOS_TENCENT_MEETING_CACHE").as_deref(),
            Ok("1"),
            "set MANGODISK_TEST_REAL_MACOS_TENCENT_MEETING_CACHE=1 to authorize this real cache diagnostic"
        );
        let running = ProcessSnapshot::capture()
            .expect("the macOS process inventory must be available")
            .matching_processes(&["TencentMeeting".to_string()]);
        assert!(
            running.is_empty(),
            "Tencent Meeting must be completely stopped before cleanup"
        );

        let home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .expect("HOME must be available");
        let webkit_cache = home.join("Library/Caches/com.tencent.meeting/WebKit");
        let target_root = webkit_cache.join("NetworkCache");
        let container_library = home.join("Library/Containers/com.tencent.meeting/Data/Library");
        let preserved_paths = [
            webkit_cache.join("AlternativeServices"),
            webkit_cache.join("CacheStorage"),
            webkit_cache.join("HSTS"),
            container_library.join("Preferences"),
            container_library.join("Cookies"),
            container_library.join("WebKit/WebsiteData"),
            container_library.join("Users"),
            container_library.join("Global/Database"),
            container_library.join("Global/Preferences"),
            container_library.join("Global/Data/DynamicResource"),
            container_library.join("Global/Data/DynamicResourcePackage"),
            home.join("Library/Containers/com.tencent.meeting/Data/Documents"),
        ];
        assert!(target_root.is_dir());
        assert!(
            preserved_paths.iter().all(|path| path.exists()),
            "the real profile must expose representative account, meeting, and browser state"
        );
        let preserved_before = preserved_paths
            .iter()
            .map(|path| digest_macos_tree_without_following_links(path))
            .collect::<Vec<_>>();

        let marker = target_root.join("mangodisk-rule-validation.bin");
        fs::write(&marker, b"payload").expect("the Tencent Meeting cache marker must be created");
        let request = |dry_run| CleanupRequest {
            rule_ids: vec!["app.tencent-meeting-cache".to_string()],
            source_selections: Vec::new(),
            dry_run,
            project_roots: Vec::new(),
        };

        let preview = CleanupService::execute(request(true))
            .expect("the real Tencent Meeting cache dry run must succeed");
        assert_eq!(preview.failed_item_count, 0, "{:?}", preview.actions);
        assert!(preview.expected_bytes >= 7);
        assert!(marker.exists());

        let result = CleanupService::execute(request(false))
            .expect("the real Tencent Meeting cache cleanup must succeed");
        assert_eq!(result.failed_item_count, 0, "{:?}", result.actions);
        assert!(result.released_bytes >= 7);
        assert!(!marker.exists());
        let preserved_after = preserved_paths
            .iter()
            .map(|path| digest_macos_tree_without_following_links(path))
            .collect::<Vec<_>>();
        assert_eq!(preserved_after, preserved_before);
        println!(
            "real_macos_tencent_meeting_cache_cleanup expected_bytes={} released_bytes={} affected_item_count={} target_root_count=1 preserved_root_count={}",
            preview.expected_bytes,
            result.released_bytes,
            result.affected_item_count,
            preserved_paths.len()
        );
    }

    /// Confirms that Tencent Lemon's foreground client and persistent status
    /// monitor block all three cache containers before traversal. The system
    /// daemon is intentionally absent: live-handle inspection confirms that it
    /// does not own files below these per-user cache roots.
    #[test]
    #[ignore = "requires the real Tencent Lemon application to be running"]
    fn real_tencent_lemon_cache_blocks_while_running() {
        assert_eq!(
            std::env::var("MANGODISK_TEST_REAL_TENCENT_LEMON_CACHE_BLOCK").as_deref(),
            Ok("1"),
            "set MANGODISK_TEST_REAL_TENCENT_LEMON_CACHE_BLOCK=1 for the real process-gate diagnostic"
        );
        let process_names = ["Tencent Lemon", "LemonMonitor", "LemonUpdate"].map(str::to_string);
        let running = ProcessSnapshot::capture()
            .expect("the macOS process inventory must be available")
            .matching_processes(&process_names);
        assert!(
            running.len() >= 2,
            "Tencent Lemon and its status monitor must be running"
        );

        let result = CleanupService::execute(CleanupRequest {
            rule_ids: vec!["app.tencent-lemon-cache".to_string()],
            source_selections: Vec::new(),
            dry_run: false,
            project_roots: Vec::new(),
        })
        .expect("the blocked Tencent Lemon cleanup must return a structured result");
        assert_eq!(result.actions.len(), 1);
        let action = &result.actions[0];
        assert_eq!(action.status, crate::cleanup::CleanupActionStatus::Blocked);
        assert_eq!(
            action.reason_code,
            Some(crate::cleanup::CleanupActionReason::RunningProcesses)
        );
        assert_eq!(action.released_bytes, 0);
        assert_eq!(action.affected_item_count, 0);
        assert!(!action.running_processes.is_empty());
        println!(
            "real_macos_tencent_lemon_cache_block running_process_count={}",
            action.running_processes.len()
        );
    }

    /// Clears only Tencent Lemon's three standard cache containers. Opaque
    /// digests protect cleanup history, scan databases, preferences, monitor
    /// state, HTTP storage, logs, launch configuration, and sandbox data.
    #[test]
    #[ignore = "permanently clears real Tencent Lemon caches"]
    fn real_tencent_lemon_cache_preserves_history_and_monitor_state() {
        assert_eq!(
            std::env::var("MANGODISK_TEST_REAL_TENCENT_LEMON_CACHE").as_deref(),
            Ok("1"),
            "set MANGODISK_TEST_REAL_TENCENT_LEMON_CACHE=1 to authorize this real cache diagnostic"
        );
        let process_names = ["Tencent Lemon", "LemonMonitor", "LemonUpdate"].map(str::to_string);
        let running = ProcessSnapshot::capture()
            .expect("the macOS process inventory must be available")
            .matching_processes(&process_names);
        assert!(
            running.is_empty(),
            "Tencent Lemon and its monitor must be completely stopped before cleanup"
        );

        let home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .expect("HOME must be available");
        let library = home.join("Library");
        let target_roots = [
            library.join("Caches/com.tencent.Lemon"),
            library.join("Caches/com.tencent.LemonMonitor"),
            library.join("Caches/com.tencent.LemonUpdate"),
        ];
        assert!(
            target_roots.iter().all(|root| root.is_dir()),
            "the initialized Lemon components must expose all three cache roots"
        );
        let preserved_candidates = [
            library.join("Application Support/com.tencent.Lemon"),
            library.join("Application Support/com.tencent.LemonMonitor"),
            library.join("Containers/com.tencent.LemonLite"),
            library.join("Application Scripts/com.tencent.LemonLite"),
            library.join("HTTPStorages/com.tencent.Lemon"),
            library.join("HTTPStorages/com.tencent.LemonMonitor"),
            library.join("HTTPStorages/com.tencent.LemonUpdate"),
            library.join("Preferences/com.tencent.Lemon.plist"),
            library.join("Preferences/com.tencent.LemonMonitor.plist"),
            library.join("Preferences/com.tencent.LemonUpdate.plist"),
            library.join("Logs/Tencent Lemon.log"),
            library.join("LaunchAgents/com.tencent.Lemon.trash.plist"),
        ];
        let preserved_paths = preserved_candidates
            .into_iter()
            .filter(|path| path.exists())
            .collect::<Vec<_>>();
        assert!(
            preserved_paths.len() >= 10,
            "the initialized client must expose representative durable state"
        );
        let preserved_before = preserved_paths
            .iter()
            .map(|path| digest_macos_tree_without_following_links(path))
            .collect::<Vec<_>>();

        let markers = target_roots
            .iter()
            .enumerate()
            .map(|(index, root)| root.join(format!("mangodisk-rule-validation-{index}.bin")))
            .collect::<Vec<_>>();
        for marker in &markers {
            fs::write(marker, b"payload").expect("the Tencent Lemon marker must be created");
        }
        let request = |dry_run| CleanupRequest {
            rule_ids: vec!["app.tencent-lemon-cache".to_string()],
            source_selections: Vec::new(),
            dry_run,
            project_roots: Vec::new(),
        };

        let preview = CleanupService::execute(request(true))
            .expect("the real Tencent Lemon cache dry run must succeed");
        assert_eq!(preview.failed_item_count, 0, "{:?}", preview.actions);
        assert!(preview.expected_bytes >= markers.len() as u64 * 7);
        assert!(markers.iter().all(|marker| marker.exists()));

        let result = CleanupService::execute(request(false))
            .expect("the real Tencent Lemon cache cleanup must succeed");
        assert_eq!(result.failed_item_count, 0, "{:?}", result.actions);
        assert!(result.released_bytes >= markers.len() as u64 * 7);
        assert!(markers.iter().all(|marker| !marker.exists()));
        let preserved_after = preserved_paths
            .iter()
            .map(|path| digest_macos_tree_without_following_links(path))
            .collect::<Vec<_>>();
        assert_eq!(preserved_after, preserved_before);
        println!(
            "real_macos_tencent_lemon_cache_cleanup expected_bytes={} released_bytes={} affected_item_count={} target_root_count={} preserved_root_count={}",
            preview.expected_bytes,
            result.released_bytes,
            result.affected_item_count,
            target_roots.len(),
            preserved_paths.len()
        );
    }

    /// Confirms that Thunder and its download helper cannot mutate the cache
    /// while the production cleanup path is measuring or deleting it.
    #[test]
    #[ignore = "requires the real Thunder application to be running"]
    fn real_thunder_cache_blocks_while_running() {
        assert_eq!(
            std::env::var("MANGODISK_TEST_REAL_MACOS_THUNDER_CACHE_BLOCK").as_deref(),
            Ok("1"),
            "set MANGODISK_TEST_REAL_MACOS_THUNDER_CACHE_BLOCK=1 for the real process-gate diagnostic"
        );
        let running = ProcessSnapshot::capture()
            .expect("the macOS process inventory must be available")
            .matching_processes(&["Thunder".to_string(), "DownloadService".to_string()]);
        assert!(!running.is_empty(), "Thunder or its helper must be running");

        let result = CleanupService::execute(CleanupRequest {
            rule_ids: vec!["app.thunder-cache".to_string()],
            source_selections: Vec::new(),
            dry_run: false,
            project_roots: Vec::new(),
        })
        .expect("the blocked Thunder cleanup must return a structured result");
        assert_eq!(result.actions.len(), 1);
        let action = &result.actions[0];
        assert_eq!(action.status, crate::cleanup::CleanupActionStatus::Blocked);
        assert_eq!(
            action.reason_code,
            Some(crate::cleanup::CleanupActionReason::RunningProcesses)
        );
        assert_eq!(action.released_bytes, 0);
        assert_eq!(action.affected_item_count, 0);
        assert!(!action.running_processes.is_empty());
        println!(
            "real_macos_thunder_cache_block running_process_count={}",
            action.running_processes.len()
        );
    }

    /// Clears Thunder's dedicated macOS cache container while opaque digests
    /// protect download tasks, cloud-drive databases, accounts, uploads,
    /// preferences, HTTP storage, and WebKit website state outside that root.
    #[test]
    #[ignore = "permanently clears the real Thunder application cache"]
    fn real_thunder_cache_preserves_download_and_account_state() {
        assert_eq!(
            std::env::var("MANGODISK_TEST_REAL_MACOS_THUNDER_CACHE").as_deref(),
            Ok("1"),
            "set MANGODISK_TEST_REAL_MACOS_THUNDER_CACHE=1 to authorize this real cache diagnostic"
        );
        let running = ProcessSnapshot::capture()
            .expect("the macOS process inventory must be available")
            .matching_processes(&["Thunder".to_string(), "DownloadService".to_string()]);
        assert!(
            running.is_empty(),
            "Thunder and its download helper must be completely stopped before cleanup"
        );

        let home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .expect("HOME must be available");
        let target_root = home.join("Library/Caches/com.xunlei.Thunder");
        let preserved_candidates = [
            home.join("Library/Application Support/com.xunlei.Thunder"),
            home.join("Library/Application Support/Thunder"),
            home.join("Library/WebKit/com.xunlei.Thunder"),
            home.join("Library/HTTPStorages/com.xunlei.Thunder"),
            home.join("Library/Preferences/com.xunlei.Thunder.plist"),
            home.join("Library/Containers/com.xunlei.Thunder.Thunder-Extension"),
        ];
        let preserved_paths = preserved_candidates
            .into_iter()
            .filter(|path| path.exists())
            .collect::<Vec<_>>();
        assert!(target_root.is_dir());
        assert!(
            preserved_paths.len() >= 5,
            "the initialized client must expose representative durable state outside its cache"
        );
        let preserved_before = preserved_paths
            .iter()
            .map(|path| digest_macos_tree_without_following_links(path))
            .collect::<Vec<_>>();

        let marker = target_root.join("mangodisk-rule-validation.bin");
        fs::write(&marker, b"payload").expect("the Thunder cache marker must be created");
        let request = |dry_run| CleanupRequest {
            rule_ids: vec!["app.thunder-cache".to_string()],
            source_selections: Vec::new(),
            dry_run,
            project_roots: Vec::new(),
        };

        let preview = CleanupService::execute(request(true))
            .expect("the real Thunder cache dry run must succeed");
        assert_eq!(preview.failed_item_count, 0, "{:?}", preview.actions);
        assert!(preview.expected_bytes >= 7);
        assert!(marker.exists());

        let result = CleanupService::execute(request(false))
            .expect("the real Thunder cache cleanup must succeed");
        assert_eq!(result.failed_item_count, 0, "{:?}", result.actions);
        assert!(result.released_bytes >= 7);
        assert!(!marker.exists());
        let preserved_after = preserved_paths
            .iter()
            .map(|path| digest_macos_tree_without_following_links(path))
            .collect::<Vec<_>>();
        assert_eq!(preserved_after, preserved_before);
        println!(
            "real_macos_thunder_cache_cleanup expected_bytes={} released_bytes={} affected_item_count={} target_root_count=1 preserved_root_count={}",
            preview.expected_bytes,
            result.released_bytes,
            result.affected_item_count,
            preserved_paths.len()
        );
    }

    /// Confirms that the signed WPS application, cloud service, and CEF hosts
    /// block both cache and diagnostic cleanup before profile traversal.
    #[test]
    #[ignore = "requires the real WPS Office application to be running"]
    fn real_wps_caches_block_while_running() {
        assert_eq!(
            std::env::var("MANGODISK_TEST_REAL_MACOS_WPS_CACHE_BLOCK").as_deref(),
            Ok("1"),
            "set MANGODISK_TEST_REAL_MACOS_WPS_CACHE_BLOCK=1 for the real process-gate diagnostic"
        );
        let process_names = [
            "wpsoffice",
            "WPS Office",
            "wpscloudsvr",
            "promecefpluginhost",
            "promecefpluginhost (GPU)",
            "promecefpluginhost (Renderer)",
        ]
        .map(str::to_string);
        let running = ProcessSnapshot::capture()
            .expect("the macOS process inventory must be available")
            .matching_processes(&process_names);
        assert!(!running.is_empty(), "WPS Office must be running");

        let result = CleanupService::execute(CleanupRequest {
            rule_ids: vec![
                "app.wps-cache".to_string(),
                "app.wps-diagnostic-cache".to_string(),
            ],
            source_selections: Vec::new(),
            dry_run: false,
            project_roots: Vec::new(),
        })
        .expect("the blocked WPS cleanup must return structured results");
        assert_eq!(result.actions.len(), 2);
        for action in &result.actions {
            assert_eq!(action.status, crate::cleanup::CleanupActionStatus::Blocked);
            assert_eq!(
                action.reason_code,
                Some(crate::cleanup::CleanupActionReason::RunningProcesses)
            );
            assert_eq!(action.released_bytes, 0);
            assert_eq!(action.affected_item_count, 0);
            assert!(!action.running_processes.is_empty());
        }
        println!(
            "real_macos_wps_cache_block running_process_count={}",
            running.len()
        );
    }

    /// Clears WPS's sandbox cache and fixed diagnostic leaves. Opaque digests
    /// protect the signed-in cloud profile, cloud file cache, recovery/import
    /// state, preferences, HTTP storage, group container, and CEF website data.
    #[test]
    #[ignore = "permanently clears real WPS Office cache and diagnostic data"]
    fn real_wps_caches_preserve_documents_account_and_recovery_state() {
        assert_eq!(
            std::env::var("MANGODISK_TEST_REAL_MACOS_WPS_CACHE").as_deref(),
            Ok("1"),
            "set MANGODISK_TEST_REAL_MACOS_WPS_CACHE=1 to authorize this real cache diagnostic"
        );
        let process_names = [
            "wpsoffice",
            "WPS Office",
            "wpscloudsvr",
            "promecefpluginhost",
            "promecefpluginhost (GPU)",
            "promecefpluginhost (Renderer)",
        ]
        .map(str::to_string);
        let running = ProcessSnapshot::capture()
            .expect("the macOS process inventory must be available")
            .matching_processes(&process_names);
        assert!(running.is_empty(), "WPS Office must be completely stopped");

        let home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .expect("HOME must be available");
        let container = home.join("Library/Containers/com.kingsoft.wpsoffice.mac.global/Data");
        let library = container.join("Library");
        let app_support = library.join("Application Support");
        let kingsoft = app_support.join("Kingsoft");
        let office6 = kingsoft.join("office6");
        let office_space = office6.join("OfficeSpace");
        let cache_root = library.join("Caches/com.kingsoft.wpsoffice.mac.global");
        let target_roots = [
            cache_root,
            office6.join("log"),
            office_space.join("log"),
            office_space.join("dump"),
        ]
        .into_iter()
        .filter(|path| path.is_dir())
        .collect::<Vec<_>>();
        assert_eq!(
            target_roots.len(),
            4,
            "the initialized profile must expose all verified WPS cache and diagnostic roots"
        );

        let preserved_candidates = [
            container.join("Documents"),
            app_support.join("CEF/User Data"),
            app_support.join("Google"),
            kingsoft.join("qing"),
            kingsoft.join("WPS Cloud Files"),
            library.join("HTTPStorages/com.kingsoft.wpsoffice.mac.global"),
            library.join("Preferences"),
            home.join("Library/Group Containers/2G98R5QYU5.wpsoffice"),
        ];
        let preserved_paths = preserved_candidates
            .into_iter()
            .filter(|path| path.exists())
            .collect::<Vec<_>>();
        assert!(
            preserved_paths.len() >= 7,
            "the signed-in WPS profile must expose representative durable state"
        );
        let preserved_snapshot = || {
            let mut digests = preserved_paths
                .iter()
                .map(|path| digest_macos_tree_without_following_links(path))
                .collect::<Vec<_>>();
            digests.push(digest_macos_tree_with_exclusions_without_following_links(
                &office6,
                &["log", "OfficeSpace"],
            ));
            digests.push(digest_macos_tree_with_exclusions_without_following_links(
                &office_space,
                &["log", "dump"],
            ));
            digests
        };
        let preserved_before = preserved_snapshot();

        let markers = target_roots
            .iter()
            .map(|root| root.join("mangodisk-rule-validation.bin"))
            .collect::<Vec<_>>();
        for marker in &markers {
            fs::write(marker, b"payload").expect("the WPS cache marker must be created");
        }
        let request = |dry_run| CleanupRequest {
            rule_ids: vec![
                "app.wps-cache".to_string(),
                "app.wps-diagnostic-cache".to_string(),
            ],
            source_selections: Vec::new(),
            dry_run,
            project_roots: Vec::new(),
        };

        let preview = CleanupService::execute(request(true))
            .expect("the real WPS cache dry run must succeed");
        assert_eq!(preview.failed_item_count, 0, "{:?}", preview.actions);
        assert!(preview.expected_bytes >= markers.len() as u64 * 7);
        assert!(markers.iter().all(|marker| marker.exists()));

        let result = CleanupService::execute(request(false))
            .expect("the real WPS cache cleanup must succeed");
        assert_eq!(result.failed_item_count, 0, "{:?}", result.actions);
        assert!(result.released_bytes >= markers.len() as u64 * 7);
        assert!(markers.iter().all(|marker| !marker.exists()));
        let preserved_after = preserved_snapshot();
        assert_eq!(preserved_after, preserved_before);
        println!(
            "real_macos_wps_cache_cleanup expected_bytes={} released_bytes={} affected_item_count={} target_root_count={} preserved_root_count={}",
            preview.expected_bytes,
            result.released_bytes,
            result.affected_item_count,
            target_roots.len(),
            preserved_paths.len() + 2
        );
    }

    /// Confirms that the signed UC browser and its Chromium helper processes
    /// block cleanup before any real profile or component cache is traversed.
    #[test]
    #[ignore = "requires the real UC browser to be running"]
    fn real_uc_browser_cache_blocks_while_running() {
        assert_eq!(
            std::env::var("MANGODISK_TEST_REAL_MACOS_UC_CACHE_BLOCK").as_deref(),
            Ok("1"),
            "set MANGODISK_TEST_REAL_MACOS_UC_CACHE_BLOCK=1 for the real process-gate diagnostic"
        );
        let process_names =
            ["UC", "UC Helper", "UC Helper (GPU)", "UC Helper (Renderer)"].map(str::to_string);
        let running = ProcessSnapshot::capture()
            .expect("the macOS process inventory must be available")
            .matching_processes(&process_names);
        assert!(!running.is_empty(), "UC browser must be running");

        let result = CleanupService::execute(CleanupRequest {
            rule_ids: vec!["browser.uc-cache".to_string()],
            source_selections: Vec::new(),
            dry_run: false,
            project_roots: Vec::new(),
        })
        .expect("the blocked UC cleanup must return a structured result");
        assert_eq!(result.actions.len(), 1);
        let action = &result.actions[0];
        assert_eq!(action.status, crate::cleanup::CleanupActionStatus::Blocked);
        assert_eq!(
            action.reason_code,
            Some(crate::cleanup::CleanupActionReason::RunningProcesses)
        );
        assert_eq!(action.released_bytes, 0);
        assert_eq!(action.affected_item_count, 0);
        assert!(!action.running_processes.is_empty());
        println!(
            "real_macos_uc_cache_block running_process_count={}",
            running.len()
        );
    }

    /// Clears only UC's dedicated HTTP, code, GPU, shader, and downloaded
    /// component-package cache roots. Full digests protect representative
    /// credentials, cookies, history, bookmarks, extensions, sessions, local
    /// storage, Service Worker state, downloads, and browser preferences.
    #[test]
    #[ignore = "permanently clears real UC browser caches"]
    fn real_uc_browser_cache_preserves_profile_and_download_state() {
        assert_eq!(
            std::env::var("MANGODISK_TEST_REAL_MACOS_UC_CACHE").as_deref(),
            Ok("1"),
            "set MANGODISK_TEST_REAL_MACOS_UC_CACHE=1 to authorize this real cache diagnostic"
        );
        let process_names =
            ["UC", "UC Helper", "UC Helper (GPU)", "UC Helper (Renderer)"].map(str::to_string);
        let running = ProcessSnapshot::capture()
            .expect("the macOS process inventory must be available")
            .matching_processes(&process_names);
        assert!(running.is_empty(), "UC browser must be completely stopped");

        let home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .expect("HOME must be available");
        let uc_root = home.join("Library/Application Support/UC");
        let profile = uc_root.join("Default");
        assert!(profile.is_dir(), "UC must complete a first launch");

        let preserved_candidates = [
            uc_root.join("Local State"),
            uc_root.join("NativeMessagingHosts"),
            uc_root.join("user_info"),
            profile.join("Bookmarks"),
            profile.join("Cookies"),
            profile.join("History"),
            profile.join("Login Data"),
            profile.join("Network"),
            profile.join("Extensions"),
            profile.join("Local Storage"),
            profile.join("Preferences"),
            profile.join("Service Worker"),
            profile.join("Session Storage"),
            profile.join("Sessions"),
            profile.join("WebStorage"),
        ];
        let preserved_paths = preserved_candidates
            .into_iter()
            .filter(|path| path.exists())
            .collect::<Vec<_>>();
        assert!(
            preserved_paths.len() >= 12,
            "the initialized UC profile must expose representative durable state"
        );
        let preserved_before = preserved_paths
            .iter()
            .map(|path| digest_macos_tree_without_following_links(path))
            .collect::<Vec<_>>();

        let target_candidates = [
            home.join("Library/Caches/UC"),
            home.join("Library/Caches/org.uc.UC"),
            uc_root.join("ShaderCache"),
            uc_root.join("GrShaderCache"),
            uc_root.join("GraphiteDawnCache"),
            uc_root.join("component_crx_cache"),
            profile.join("Cache"),
            profile.join("Code Cache"),
            profile.join("GPUCache"),
            profile.join("DawnGraphiteCache"),
            profile.join("DawnWebGPUCache"),
        ];
        let target_roots = target_candidates
            .into_iter()
            .filter(|path| path.is_dir())
            .collect::<Vec<_>>();
        assert!(
            target_roots.len() >= 9,
            "the initialized UC profile must expose verified cache roots"
        );
        let markers = target_roots
            .iter()
            .map(|root| root.join("mangodisk-rule-validation.bin"))
            .collect::<Vec<_>>();
        for marker in &markers {
            fs::write(marker, b"payload").expect("the UC cache marker must be created");
        }
        let request = |dry_run| CleanupRequest {
            rule_ids: vec!["browser.uc-cache".to_string()],
            source_selections: Vec::new(),
            dry_run,
            project_roots: Vec::new(),
        };

        let preview =
            CleanupService::execute(request(true)).expect("the real UC cache dry run must succeed");
        assert_eq!(preview.failed_item_count, 0, "{:?}", preview.actions);
        assert!(preview.expected_bytes >= markers.len() as u64 * 7);
        assert!(markers.iter().all(|marker| marker.exists()));

        let result = CleanupService::execute(request(false))
            .expect("the real UC cache cleanup must succeed");
        assert_eq!(result.failed_item_count, 0, "{:?}", result.actions);
        assert!(result.released_bytes >= markers.len() as u64 * 7);
        assert!(markers.iter().all(|marker| !marker.exists()));
        let preserved_after = preserved_paths
            .iter()
            .map(|path| digest_macos_tree_without_following_links(path))
            .collect::<Vec<_>>();
        assert_eq!(preserved_after, preserved_before);
        println!(
            "real_macos_uc_cache_cleanup expected_bytes={} released_bytes={} affected_item_count={} target_root_count={} preserved_root_count={}",
            preview.expected_bytes,
            result.released_bytes,
            result.affected_item_count,
            target_roots.len(),
            preserved_paths.len()
        );
    }

    /// Confirms that 360 Extreme Browser and its Chromium helper processes
    /// block cleanup before any real profile or component cache is traversed.
    #[test]
    #[ignore = "requires the real 360 Extreme Browser to be running"]
    fn real_360_speed_browser_cache_blocks_while_running() {
        assert_eq!(
            std::env::var("MANGODISK_TEST_REAL_MACOS_360_CACHE_BLOCK").as_deref(),
            Ok("1"),
            "set MANGODISK_TEST_REAL_MACOS_360_CACHE_BLOCK=1 for the real process-gate diagnostic"
        );
        let process_names = [
            "360Chrome",
            "360Chrome Helper",
            "360Chrome Helper (GPU)",
            "360Chrome Helper (Renderer)",
        ]
        .map(str::to_string);
        let running = ProcessSnapshot::capture()
            .expect("the macOS process inventory must be available")
            .matching_processes(&process_names);
        assert!(!running.is_empty(), "360 Extreme Browser must be running");

        let result = CleanupService::execute(CleanupRequest {
            rule_ids: vec!["browser.360-speed-cache".to_string()],
            source_selections: Vec::new(),
            dry_run: false,
            project_roots: Vec::new(),
        })
        .expect("the blocked 360 cleanup must return a structured result");
        assert_eq!(result.actions.len(), 1);
        let action = &result.actions[0];
        assert_eq!(action.status, crate::cleanup::CleanupActionStatus::Blocked);
        assert_eq!(
            action.reason_code,
            Some(crate::cleanup::CleanupActionReason::RunningProcesses)
        );
        assert_eq!(action.released_bytes, 0);
        assert_eq!(action.affected_item_count, 0);
        assert!(!action.running_processes.is_empty());
        println!(
            "real_macos_360_cache_block running_process_count={}",
            running.len()
        );
    }

    /// Clears only 360 Extreme Browser's dedicated HTTP, code, GPU, shader,
    /// and downloaded component-package caches. Full digests protect profile
    /// credentials, cookies, history, extensions, sessions, local storage, and
    /// preferences from accidental overlap with the cache boundary.
    #[test]
    #[ignore = "permanently clears real 360 Extreme Browser caches"]
    fn real_360_speed_browser_cache_preserves_profile_state() {
        assert_eq!(
            std::env::var("MANGODISK_TEST_REAL_MACOS_360_CACHE").as_deref(),
            Ok("1"),
            "set MANGODISK_TEST_REAL_MACOS_360_CACHE=1 to authorize this real cache diagnostic"
        );
        let process_names = [
            "360Chrome",
            "360Chrome Helper",
            "360Chrome Helper (GPU)",
            "360Chrome Helper (Renderer)",
        ]
        .map(str::to_string);
        let running = ProcessSnapshot::capture()
            .expect("the macOS process inventory must be available")
            .matching_processes(&process_names);
        assert!(
            running.is_empty(),
            "360 Extreme Browser must be completely stopped"
        );

        let home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .expect("HOME must be available");
        let browser_root = home.join("Library/Application Support/360Chrome");
        let profile = browser_root.join("Default");
        assert!(
            profile.is_dir(),
            "360 Extreme Browser must complete a first launch"
        );

        let preserved_candidates = [
            browser_root.join("Local State"),
            browser_root.join("NativeMessagingHosts"),
            profile.join("Bookmarks"),
            profile.join("Cookies"),
            profile.join("History"),
            profile.join("Login Data"),
            profile.join("Network"),
            profile.join("Extensions"),
            profile.join("Local Storage"),
            profile.join("Preferences"),
            profile.join("Service Worker"),
            profile.join("Session Storage"),
            profile.join("Sessions"),
            profile.join("WebStorage"),
        ];
        let preserved_paths = preserved_candidates
            .into_iter()
            .filter(|path| path.exists())
            .collect::<Vec<_>>();
        assert!(
            preserved_paths.len() >= 9,
            "the initialized 360 profile must expose representative durable state"
        );
        let preserved_before = preserved_paths
            .iter()
            .map(|path| digest_macos_tree_without_following_links(path))
            .collect::<Vec<_>>();

        let target_candidates = [
            home.join("Library/Caches/360Chrome"),
            browser_root.join("ShaderCache64"),
            browser_root.join("GrShaderCache64"),
            browser_root.join("GraphiteDawnCache"),
            browser_root.join("component_crx_cache"),
            profile.join("GPUCache64"),
            profile.join("DawnGraphiteCache"),
            profile.join("DawnWebGPUCache"),
        ];
        let target_roots = target_candidates
            .into_iter()
            .filter(|path| path.is_dir())
            .collect::<Vec<_>>();
        assert_eq!(
            target_roots.len(),
            8,
            "the initialized 360 profile must expose every verified cache root"
        );
        let markers = target_roots
            .iter()
            .map(|root| root.join("mangodisk-rule-validation.bin"))
            .collect::<Vec<_>>();
        for marker in &markers {
            fs::write(marker, b"payload").expect("the 360 cache marker must be created");
        }
        let request = |dry_run| CleanupRequest {
            rule_ids: vec!["browser.360-speed-cache".to_string()],
            source_selections: Vec::new(),
            dry_run,
            project_roots: Vec::new(),
        };

        let preview = CleanupService::execute(request(true))
            .expect("the real 360 cache dry run must succeed");
        assert_eq!(preview.failed_item_count, 0, "{:?}", preview.actions);
        assert!(preview.expected_bytes >= markers.len() as u64 * 7);
        assert!(markers.iter().all(|marker| marker.exists()));

        let result = CleanupService::execute(request(false))
            .expect("the real 360 cache cleanup must succeed");
        assert_eq!(result.failed_item_count, 0, "{:?}", result.actions);
        assert!(result.released_bytes >= markers.len() as u64 * 7);
        assert!(markers.iter().all(|marker| !marker.exists()));
        let preserved_after = preserved_paths
            .iter()
            .map(|path| digest_macos_tree_without_following_links(path))
            .collect::<Vec<_>>();
        assert_eq!(preserved_after, preserved_before);
        println!(
            "real_macos_360_cache_cleanup expected_bytes={} released_bytes={} affected_item_count={} target_root_count={} preserved_root_count={}",
            preview.expected_bytes,
            result.released_bytes,
            result.affected_item_count,
            target_roots.len(),
            preserved_paths.len()
        );
    }

    fn direct_directory_children(root: &Path) -> Vec<PathBuf> {
        if !root.is_dir() {
            return Vec::new();
        }
        let mut children = fs::read_dir(root)
            .expect("the dynamic partition root must be readable")
            .map(|entry| entry.expect("the partition entry must be readable").path())
            .filter(|path| path.is_dir())
            .collect::<Vec<_>>();
        children.sort();
        children
    }

    /// Hashes a real application tree without following symbolic links. QQLive
    /// component packages legitimately contain internal links, so the test
    /// records each link path and target as opaque bytes while never reading
    /// through it. A before/after digest therefore detects link replacement as
    /// well as regular-file changes without crossing the preserved boundary.
    fn digest_macos_tree_without_following_links(path: &Path) -> String {
        digest_macos_tree_with_exclusions_without_following_links(path, &[])
    }

    /// Applies the same opaque-link hashing policy while excluding fixed direct
    /// children owned by the cleanup rule. This is needed for mixed-purpose
    /// sandbox Library roots: the selected Caches child must not influence the
    /// preservation digest, while unrelated framework links are still covered.
    fn digest_macos_tree_with_exclusions_without_following_links(
        path: &Path,
        excluded_direct_children: &[&str],
    ) -> String {
        fn collect_entries(
            root: &Path,
            path: &Path,
            excluded_direct_children: &[&str],
            entries: &mut Vec<PathBuf>,
        ) {
            let metadata = fs::symlink_metadata(path)
                .expect("the preserved macOS application state must remain readable");
            entries.push(path.to_path_buf());
            if !metadata.is_dir() || metadata.file_type().is_symlink() {
                return;
            }
            let mut children = fs::read_dir(path)
                .expect("the preserved application directory must remain readable")
                .map(|entry| entry.expect("the directory entry must be readable").path())
                .filter(|child| {
                    child.parent() != Some(root)
                        || !excluded_direct_children
                            .iter()
                            .any(|excluded| child.file_name().is_some_and(|name| name == *excluded))
                })
                .collect::<Vec<_>>();
            children.sort();
            for child in children {
                collect_entries(root, &child, excluded_direct_children, entries);
            }
        }

        let mut entries = Vec::new();
        collect_entries(path, path, excluded_direct_children, &mut entries);
        let mut hasher = Sha256::new();
        for entry in entries {
            let relative = entry.strip_prefix(path).unwrap_or(entry.as_path());
            hasher.update(relative.as_os_str().as_encoded_bytes());
            let metadata = fs::symlink_metadata(&entry)
                .expect("the preserved application entry must remain readable");
            if metadata.file_type().is_symlink() {
                hasher.update(b"symlink");
                let target = fs::read_link(&entry)
                    .expect("the preserved application link target must remain readable");
                hasher.update(target.as_os_str().as_encoded_bytes());
            } else if metadata.is_file() {
                hasher.update(b"file");
                hasher.update(
                    fs::read(&entry).expect("the preserved application file must remain readable"),
                );
            } else if metadata.is_dir() {
                hasher.update(b"directory");
            }
        }
        format!("{:x}", hasher.finalize())
    }

    fn digest_macos_tree(path: &Path, excluded_direct_children: &[&str]) -> String {
        fn collect_files(
            root: &Path,
            path: &Path,
            excluded_direct_children: &[&str],
            files: &mut Vec<PathBuf>,
        ) {
            let metadata = fs::symlink_metadata(path)
                .expect("the preserved macOS application state must remain readable");
            assert!(
                !metadata.file_type().is_symlink(),
                "the preserved application state must not cross a symbolic link"
            );
            if metadata.is_file() {
                files.push(path.to_path_buf());
                return;
            }
            // IDE profiles can retain a Unix-domain socket after shutdown.
            // Sockets and FIFOs have no durable byte content to hash and cannot
            // be opened as directories; ignore them while continuing to reject
            // symbolic links and hash every regular persisted file.
            if !metadata.is_dir() {
                return;
            }
            let mut children = fs::read_dir(path)
                .expect("the preserved application directory must remain readable")
                .map(|entry| entry.expect("the directory entry must be readable").path())
                .filter(|child| {
                    child.parent() != Some(root)
                        || !excluded_direct_children
                            .iter()
                            .any(|excluded| child.file_name().is_some_and(|name| name == *excluded))
                })
                .collect::<Vec<_>>();
            children.sort();
            for child in children {
                collect_files(root, &child, excluded_direct_children, files);
            }
        }

        let mut files = Vec::new();
        collect_files(path, path, excluded_direct_children, &mut files);
        let mut hasher = Sha256::new();
        for file in files {
            let relative = file.strip_prefix(path).unwrap_or(file.as_path());
            hasher.update(relative.as_os_str().as_encoded_bytes());
            hasher.update(
                fs::read(file).expect("the preserved application file must remain readable"),
            );
        }
        format!("{:x}", hasher.finalize())
    }
}

#[cfg(all(test, windows))]
mod windows_cleanup_tests {
    use std::{
        ffi::OsString,
        fs,
        path::PathBuf,
        time::{Duration, Instant, SystemTime},
    };

    use super::*;
    use crate::cleanup::CleanupRequest;

    struct EnvironmentRestore(Vec<(&'static str, Option<OsString>)>);

    struct DirectoryCleanup(PathBuf);

    impl Drop for DirectoryCleanup {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    impl Drop for EnvironmentRestore {
        fn drop(&mut self) {
            for (name, value) in self.0.drain(..) {
                match value {
                    Some(value) => std::env::set_var(name, value),
                    None => std::env::remove_var(name),
                }
            }
        }
    }

    /// Executes the production Dart cache rule in a snapshot-backed Windows VM.
    ///
    /// The test deletes the current account's real `.dartServer` cache and
    /// therefore requires an explicit environment gate. It validates dry-run,
    /// Known Folder resolution, whole-root deletion, live accounting, and final
    /// root absence while printing only aggregate counts and timings.
    #[test]
    #[ignore = "deletes the real Dart analysis cache in an isolated Windows VM"]
    fn real_dart_analysis_cache_uses_whole_root_cleanup() {
        assert_eq!(
            std::env::var("MANGODISK_TEST_REAL_DART_CACHE").as_deref(),
            Ok("1"),
            "set MANGODISK_TEST_REAL_DART_CACHE=1 only in a snapshot-backed Windows VM"
        );
        let local_app_data = std::env::var_os("LOCALAPPDATA")
            .map(PathBuf::from)
            .expect("LOCALAPPDATA must be available");
        let cache_root = local_app_data.join(".dartServer");
        assert!(
            cache_root.is_dir(),
            "the real Dart cache fixture must exist"
        );
        let request = || CleanupRequest {
            rule_ids: vec!["dev.dart-analysis-cache".to_string()],
            source_selections: Vec::new(),
            dry_run: false,
            project_roots: Vec::new(),
        };

        let preview_started = Instant::now();
        let mut preview_request = request();
        preview_request.dry_run = true;
        let preview = CleanupService::execute(preview_request)
            .expect("the real Dart cache preview must succeed");
        let preview_ms = preview_started.elapsed().as_secs_f64() * 1_000.0;
        assert_eq!(preview.actions.len(), 1);
        assert_eq!(preview.failed_item_count, 0, "{:?}", preview.actions);
        assert!(preview.expected_bytes > 0);
        assert!(cache_root.exists(), "dry-run must preserve the Dart cache");

        let cleanup_started = Instant::now();
        let result =
            CleanupService::execute(request()).expect("the real Dart cache cleanup must succeed");
        let cleanup_ms = cleanup_started.elapsed().as_secs_f64() * 1_000.0;
        assert_eq!(result.actions.len(), 1);
        assert_eq!(result.failed_item_count, 0, "{:?}", result.actions);
        assert!(result.released_bytes > 0);
        assert!(result.affected_item_count > 0);
        assert!(
            !cache_root.exists(),
            "the complete Dart cache root must be removed"
        );
        println!(
            "real_dart_analysis_cleanup preview_ms={preview_ms:.2} cleanup_ms={cleanup_ms:.2} expected_bytes={} released_bytes={} affected_item_count={}",
            preview.expected_bytes,
            result.released_bytes,
            result.affected_item_count
        );
    }

    /// Confirms that both the desktop client and its crash handler own the
    /// Tencent Meeting profile. Cleanup must stop before traversing the mixed
    /// tree containing downloaded models, resources, databases, and settings.
    #[test]
    #[ignore = "requires the real Tencent Meeting application to be running"]
    fn real_tencent_meeting_cache_blocks_while_running() {
        assert_eq!(
            std::env::var("MANGODISK_TEST_REAL_WINDOWS_TENCENT_MEETING_CACHE_BLOCK").as_deref(),
            Ok("1"),
            "set MANGODISK_TEST_REAL_WINDOWS_TENCENT_MEETING_CACHE_BLOCK=1 for the real process-gate diagnostic"
        );
        let running = ProcessSnapshot::capture()
            .expect("the Windows process inventory must be available")
            .matching_processes(&[
                "WeMeetApp.exe".to_string(),
                "WeMeetCrashHandler.exe".to_string(),
            ]);
        assert!(!running.is_empty(), "Tencent Meeting must be running");

        let result = CleanupService::execute(CleanupRequest {
            rule_ids: vec!["app.tencent-meeting-cache".to_string()],
            source_selections: Vec::new(),
            dry_run: false,
            project_roots: Vec::new(),
        })
        .expect("the blocked Tencent Meeting cleanup must return a structured result");
        assert_eq!(result.actions.len(), 1);
        let action = &result.actions[0];
        assert_eq!(action.status, crate::cleanup::CleanupActionStatus::Blocked);
        assert_eq!(
            action.reason_code,
            Some(crate::cleanup::CleanupActionReason::RunningProcesses)
        );
        assert_eq!(action.released_bytes, 0);
        assert_eq!(action.affected_item_count, 0);
        assert!(!action.running_processes.is_empty());
        println!(
            "real_windows_tencent_meeting_cache_block running_process_count={}",
            action.running_processes.len()
        );
    }

    /// Clears fixed cache suffixes from every timestamped WebView profile and
    /// both log roots. Digests protect downloaded models and dynamic resources,
    /// account databases and preferences, per-user meeting state, and browser
    /// preferences, network state, and Local/Session Storage.
    #[test]
    #[ignore = "clears real Tencent Meeting caches in an isolated Windows VM"]
    fn real_tencent_meeting_cache_preserves_account_and_meeting_state() {
        assert_eq!(
            std::env::var("MANGODISK_TEST_REAL_WINDOWS_TENCENT_MEETING_CACHE").as_deref(),
            Ok("1"),
            "set MANGODISK_TEST_REAL_WINDOWS_TENCENT_MEETING_CACHE=1 only in an isolated Windows VM"
        );
        let roaming_app_data = std::env::var_os("APPDATA")
            .map(PathBuf::from)
            .expect("APPDATA must be available");
        let local_app_data = std::env::var_os("LOCALAPPDATA")
            .map(PathBuf::from)
            .expect("LOCALAPPDATA must be available");
        let meeting_root = roaming_app_data.join("Tencent/WeMeet");
        let webkit_root = meeting_root.join("Global/Data/WebkitCacheData");
        assert!(
            webkit_root.is_dir(),
            "Tencent Meeting must complete a first launch"
        );
        let running = ProcessSnapshot::capture()
            .expect("the Windows process inventory must be available")
            .matching_processes(&[
                "WeMeetApp.exe".to_string(),
                "WeMeetCrashHandler.exe".to_string(),
            ]);
        assert!(
            running.is_empty(),
            "Tencent Meeting must be completely stopped before cleanup"
        );

        let profiles = fs::read_dir(&webkit_root)
            .expect("the WebView profile root must be readable")
            .map(|entry| {
                entry
                    .expect("the WebView profile entry must be readable")
                    .path()
            })
            .filter(|path| path.is_dir())
            .collect::<Vec<_>>();
        assert!(
            !profiles.is_empty(),
            "the real client must expose a WebView profile"
        );

        let mut preserved_paths = [
            "Global/Data/AudioModel",
            "Global/Data/DynamicResource",
            "Global/Data/DynamicResourcePackage",
            "Global/Data/StartUp",
            "Global/Data/Timeline",
            "Global/Data/Timezone",
            "Global/Data/VirtualBkg",
            "Global/Data/XCast",
            "Global/Database",
            "Global/Preferences",
            "Global/Upgrade",
            "Global/voiceprint_record",
            "Users",
        ]
        .map(|relative| meeting_root.join(relative))
        .to_vec();
        for profile in &profiles {
            for relative in [
                "Default/Local Storage",
                "Default/Session Storage",
                "Default/Network",
                "Default/Preferences",
            ] {
                let path = profile.join(relative);
                if path.exists() {
                    preserved_paths.push(path);
                }
            }
        }
        assert!(
            preserved_paths.iter().all(|path| path.exists()),
            "the initialized client must expose durable account, meeting, and browser state"
        );
        let preserved_before = preserved_paths
            .iter()
            .map(|path| digest_tree(path))
            .collect::<Vec<_>>();

        let cache_suffixes = [
            "BrowserMetrics",
            "Default/Cache",
            "Default/Code Cache",
            "Default/GPUCache",
            "Default/DawnCache",
            "Default/DawnGraphiteCache",
            "Default/DawnWebGPUCache",
            "Default/GrShaderCache",
            "Default/GraphiteDawnCache",
            "Default/Shared Dictionary/cache",
            "GraphiteDawnCache",
            "GrShaderCache",
            "ShaderCache",
        ];
        let mut target_roots = vec![
            meeting_root.join("Global/Logs"),
            local_app_data.join("Tencent/WeMeet/Logs"),
        ];
        for profile in &profiles {
            for suffix in cache_suffixes {
                let path = profile.join(suffix);
                if path.is_dir() {
                    target_roots.push(path);
                }
            }
        }
        target_roots.sort();
        target_roots.dedup();
        assert!(
            target_roots.len() >= 8,
            "the initialized client must expose verified cache and log roots"
        );

        let markers = target_roots
            .iter()
            .map(|root| root.join("mangodisk-rule-validation.bin"))
            .collect::<Vec<_>>();
        for marker in &markers {
            fs::create_dir_all(marker.parent().expect("the marker must have a parent"))
                .expect("the Tencent Meeting target root must be writable");
            fs::write(marker, b"payload")
                .expect("the Tencent Meeting cache marker must be created");
        }
        let request = |dry_run| CleanupRequest {
            rule_ids: vec!["app.tencent-meeting-cache".to_string()],
            source_selections: Vec::new(),
            dry_run,
            project_roots: Vec::new(),
        };

        let preview = CleanupService::execute(request(true))
            .expect("the real Tencent Meeting cache dry run must succeed");
        assert_eq!(preview.failed_item_count, 0, "{:?}", preview.actions);
        assert!(preview.expected_bytes >= markers.len() as u64 * 7);
        assert!(markers.iter().all(|marker| marker.exists()));

        let result = CleanupService::execute(request(false))
            .expect("the real Tencent Meeting cache cleanup must succeed");
        assert_eq!(result.failed_item_count, 0, "{:?}", result.actions);
        assert!(result.released_bytes >= markers.len() as u64 * 7);
        assert!(markers.iter().all(|marker| !marker.exists()));
        let preserved_after = preserved_paths
            .iter()
            .map(|path| digest_tree(path))
            .collect::<Vec<_>>();
        assert_eq!(preserved_after, preserved_before);
        println!(
            "real_windows_tencent_meeting_cache_cleanup expected_bytes={} released_bytes={} affected_item_count={} profile_count={} target_root_count={} preserved_root_count={}",
            preview.expected_bytes,
            result.released_bytes,
            result.affected_item_count,
            profiles.len(),
            target_roots.len(),
            preserved_paths.len()
        );
    }

    /// Confirms that Sogou Input's broker, cloud, tool, and smart-assistant
    /// processes block cleanup before the shared CEF profile is traversed.
    #[test]
    #[ignore = "requires real Sogou Input processes to be running"]
    fn real_sogou_input_cache_blocks_while_running() {
        assert_eq!(
            std::env::var("MANGODISK_TEST_REAL_SOGOU_INPUT_CACHE_BLOCK").as_deref(),
            Ok("1"),
            "set MANGODISK_TEST_REAL_SOGOU_INPUT_CACHE_BLOCK=1 for the real process-gate diagnostic"
        );
        let process_names = [
            "SGMyInput.exe",
            "SGTool.exe",
            "SGWebRender.exe",
            "SogouCloud.exe",
            "SogouImeBroker.exe",
            "SOGOUSmartAssistant.exe",
        ]
        .map(str::to_string);
        let running = ProcessSnapshot::capture()
            .expect("the Windows process inventory must be available")
            .matching_processes(&process_names);
        assert!(!running.is_empty(), "Sogou Input must be running");

        let result = CleanupService::execute(CleanupRequest {
            rule_ids: vec!["app.sogou-input-cache".to_string()],
            source_selections: Vec::new(),
            dry_run: false,
            project_roots: Vec::new(),
        })
        .expect("the blocked Sogou Input cleanup must return a structured result");
        assert_eq!(result.actions.len(), 1);
        let action = &result.actions[0];
        assert_eq!(action.status, crate::cleanup::CleanupActionStatus::Blocked);
        assert_eq!(
            action.reason_code,
            Some(crate::cleanup::CleanupActionReason::RunningProcesses)
        );
        assert_eq!(action.released_bytes, 0);
        assert_eq!(action.affected_item_count, 0);
        assert!(!action.running_processes.is_empty());
        println!(
            "real_windows_sogou_input_cache_block running_process_count={}",
            action.running_processes.len()
        );
    }

    /// Clears only Sogou Input's fixed CEF response and code-cache leaves.
    /// Full digests protect dictionaries, personalization, backups, settings,
    /// models, components, updates, and every adjacent browser-storage family.
    #[test]
    #[ignore = "clears real Sogou Input CEF caches in an isolated Windows VM"]
    fn real_sogou_input_cache_preserves_dictionary_and_personalization_state() {
        assert_eq!(
            std::env::var("MANGODISK_TEST_REAL_SOGOU_INPUT_CACHE").as_deref(),
            Ok("1"),
            "set MANGODISK_TEST_REAL_SOGOU_INPUT_CACHE=1 only in an isolated Windows VM"
        );
        let process_names = [
            "SGMyInput.exe",
            "SGTool.exe",
            "SGWebRender.exe",
            "SogouCloud.exe",
            "SogouImeBroker.exe",
            "SOGOUSmartAssistant.exe",
        ]
        .map(str::to_string);
        let running = ProcessSnapshot::capture()
            .expect("the Windows process inventory must be available")
            .matching_processes(&process_names);
        assert!(
            running.is_empty(),
            "all Sogou Input owner processes must be stopped before cleanup"
        );

        let program_data = std::env::var_os("PROGRAMDATA")
            .map(PathBuf::from)
            .expect("PROGRAMDATA must be available");
        let user_profile = std::env::var_os("USERPROFILE")
            .map(PathBuf::from)
            .expect("USERPROFILE must be available");
        let sogou_root = program_data.join("SogouInput");
        let cef_root = sogou_root.join("SGCefCache/SGMyInput/CefLocalStorage");
        assert!(
            cef_root.is_dir(),
            "Sogou Input must complete a first launch"
        );

        let preserved_candidates = [
            user_profile.join("AppData/LocalLow/SogouPY.users"),
            user_profile.join("AppData/LocalLow/SogouPY/Backup"),
            user_profile.join("AppData/LocalLow/SogouPY/Indv"),
            user_profile.join("AppData/LocalLow/SogouPY/mmkv"),
            user_profile.join("AppData/LocalLow/SogouPY/scd"),
            sogou_root.join("Components"),
            sogou_root.join("SGBizConfig"),
            sogou_root.join("SGSmartAssistant"),
            sogou_root.join("ShiplyUpdate"),
            sogou_root.join("skinrootdir"),
            cef_root.join("blob_storage"),
            cef_root.join("databases"),
            cef_root.join("IndexedDB"),
            cef_root.join("Local Storage"),
            cef_root.join("Session Storage"),
        ];
        let preserved_paths = preserved_candidates
            .into_iter()
            .filter(|path| path.exists())
            .collect::<Vec<_>>();
        assert!(
            preserved_paths.len() >= 12,
            "the initialized input method must expose representative durable state"
        );
        let preserved_before = preserved_paths
            .iter()
            .map(|path| digest_tree(path))
            .collect::<Vec<_>>();

        let target_roots = [cef_root.join("Cache"), cef_root.join("Code Cache")]
            .into_iter()
            .filter(|path| path.is_dir())
            .collect::<Vec<_>>();
        assert_eq!(
            target_roots.len(),
            2,
            "the initialized CEF profile must expose response and code caches"
        );
        let markers = target_roots
            .iter()
            .map(|root| root.join("mangodisk-rule-validation.bin"))
            .collect::<Vec<_>>();
        for marker in &markers {
            fs::write(marker, b"payload").expect("the Sogou Input cache marker must be created");
        }
        let request = |dry_run| CleanupRequest {
            rule_ids: vec!["app.sogou-input-cache".to_string()],
            source_selections: Vec::new(),
            dry_run,
            project_roots: Vec::new(),
        };

        let preview = CleanupService::execute(request(true))
            .expect("the real Sogou Input cache dry run must succeed");
        assert_eq!(preview.failed_item_count, 0, "{:?}", preview.actions);
        assert!(preview.expected_bytes >= markers.len() as u64 * 7);
        assert!(markers.iter().all(|marker| marker.exists()));

        let result = CleanupService::execute(request(false))
            .expect("the real Sogou Input cache cleanup must succeed");
        assert_eq!(result.failed_item_count, 0, "{:?}", result.actions);
        assert!(result.released_bytes >= markers.len() as u64 * 7);
        assert!(markers.iter().all(|marker| !marker.exists()));
        let preserved_after = preserved_paths
            .iter()
            .map(|path| digest_tree(path))
            .collect::<Vec<_>>();
        assert_eq!(preserved_after, preserved_before);
        println!(
            "real_windows_sogou_input_cache_cleanup expected_bytes={} released_bytes={} affected_item_count={} target_root_count={} preserved_root_count={}",
            preview.expected_bytes,
            result.released_bytes,
            result.affected_item_count,
            target_roots.len(),
            preserved_paths.len()
        );
    }

    /// Confirms that every Windows WPS cache boundary fails closed while the
    /// editor, cloud service, renderer, or updater still owns shared profile
    /// files. All three rules use the same exhaustive process list so one
    /// overlooked helper cannot leave only part of the cleanup plan writable.
    #[test]
    #[ignore = "requires real WPS Office processes to be running"]
    fn real_wps_caches_block_while_running() {
        assert_eq!(
            std::env::var("MANGODISK_TEST_REAL_WPS_CACHE_BLOCK").as_deref(),
            Ok("1"),
            "set MANGODISK_TEST_REAL_WPS_CACHE_BLOCK=1 for the real process-gate diagnostic"
        );
        let process_names = [
            "wpsoffice.exe",
            "wps.exe",
            "et.exe",
            "wpp.exe",
            "wpspdf.exe",
            "wpscloudsvr.exe",
            "wpscenter.exe",
            "promecefpluginhost.exe",
            "ksomisc.exe",
            "wpsupdate.exe",
        ]
        .map(str::to_string);
        let running = ProcessSnapshot::capture()
            .expect("the Windows process inventory must be available")
            .matching_processes(&process_names);
        assert!(!running.is_empty(), "WPS Office must be running");

        let result = CleanupService::execute(CleanupRequest {
            rule_ids: vec![
                "app.wps-cache".to_string(),
                "app.wps-rendering-cache".to_string(),
                "app.wps-diagnostic-cache".to_string(),
            ],
            source_selections: Vec::new(),
            dry_run: false,
            project_roots: Vec::new(),
        })
        .expect("the blocked WPS cleanup must return a structured result");
        assert_eq!(result.actions.len(), 3);
        for action in &result.actions {
            assert_eq!(action.status, crate::cleanup::CleanupActionStatus::Blocked);
            assert_eq!(
                action.reason_code,
                Some(crate::cleanup::CleanupActionReason::RunningProcesses)
            );
            assert_eq!(action.released_bytes, 0);
            assert_eq!(action.affected_item_count, 0);
            assert!(!action.running_processes.is_empty());
        }
        println!(
            "real_windows_wps_cache_block running_process_count={} blocked_action_count={}",
            running.len(),
            result.actions.len()
        );
    }

    /// Clears only WPS's dedicated application cache, fixed CEF response/code
    /// leaves, and diagnostic log/dump directories. A full digest of every
    /// adjacent subtree protects the temporary validation document, recovery
    /// and backup state, add-ons, settings, account databases, cloud-file state,
    /// and persistent browser storage from accidental matcher expansion.
    #[test]
    #[ignore = "clears real WPS caches in an isolated Windows VM"]
    fn real_wps_caches_preserve_documents_account_and_recovery_state() {
        assert_eq!(
            std::env::var("MANGODISK_TEST_REAL_WPS_CACHE").as_deref(),
            Ok("1"),
            "set MANGODISK_TEST_REAL_WPS_CACHE=1 only in an isolated Windows VM"
        );
        let process_names = [
            "wpsoffice.exe",
            "wps.exe",
            "et.exe",
            "wpp.exe",
            "wpspdf.exe",
            "wpscloudsvr.exe",
            "wpscenter.exe",
            "promecefpluginhost.exe",
            "ksomisc.exe",
            "wpsupdate.exe",
        ]
        .map(str::to_string);
        let running = ProcessSnapshot::capture()
            .expect("the Windows process inventory must be available")
            .matching_processes(&process_names);
        assert!(
            running.is_empty(),
            "all WPS Office owner processes must be stopped before cleanup"
        );

        let roaming = std::env::var_os("APPDATA")
            .map(PathBuf::from)
            .expect("APPDATA must be available");
        let local = std::env::var_os("LOCALAPPDATA")
            .map(PathBuf::from)
            .expect("LOCALAPPDATA must be available");
        let kingsoft = roaming.join("Kingsoft");
        let office6 = kingsoft.join("office6");
        let wps = kingsoft.join("wps");
        let office_cache = office6.join("cache");
        let cef_root = wps.join("addons/data/win-i386/cef/2");
        let validation_document =
            local.join("Temp/MangoDisk-WPS-Rule-20260811/wps-validation-document.rtf");
        assert!(
            office_cache.is_dir(),
            "WPS must reproduce its application cache"
        );
        assert!(cef_root.is_dir(), "WPS must reproduce its CEF profile root");
        assert!(
            validation_document.is_file(),
            "the temporary validation document must remain available"
        );

        let preserved_roots = [
            kingsoft.join("PDF"),
            kingsoft.join("kaccountsdk"),
            kingsoft.join("qing"),
            kingsoft.join("WPS Cloud Files"),
            local.join("Kingsoft/WPS Cloud Files"),
            local.join("CEF/User Data"),
            validation_document.clone(),
        ]
        .into_iter()
        .filter(|path| path.exists())
        .collect::<Vec<_>>();
        assert!(
            preserved_roots.len() >= 5,
            "the initialized WPS profile must expose representative durable state"
        );
        let preserved_snapshot = || {
            let mut digests = preserved_roots
                .iter()
                .map(|path| digest_tree(path))
                .collect::<Vec<_>>();
            // The mixed-purpose office and add-on trees are hashed as a whole
            // except for the exact path segments owned by these three rules.
            digests.push(digest_tree_excluding_segments(
                &office6,
                &["cache", "log", "dump"],
            ));
            digests.push(digest_tree_excluding_segments(
                &wps,
                &["Cache", "Code Cache"],
            ));
            digests
        };
        let preserved_before = preserved_snapshot();

        let mut target_roots = vec![office_cache, office6.join("log")];
        for candidate in [
            office6.join("OfficeSpace/log"),
            office6.join("OfficeSpace/dump"),
        ] {
            if candidate.is_dir() {
                target_roots.push(candidate);
            }
        }
        let cef_targets = directories_with_leaf_names(&cef_root, &["Cache", "Code Cache"]);
        let cef_partition_count = cef_targets
            .iter()
            .filter_map(|path| path.parent())
            .collect::<std::collections::BTreeSet<_>>()
            .len();
        target_roots.extend(cef_targets);
        target_roots.retain(|path| path.is_dir());
        assert!(
            target_roots.len() >= 5,
            "the initialized WPS profile must expose all three cleanup families"
        );

        let markers = target_roots
            .iter()
            .enumerate()
            .map(|(index, root)| root.join(format!("mangodisk-rule-validation-{index}.bin")))
            .collect::<Vec<_>>();
        for marker in &markers {
            fs::write(marker, b"payload").expect("the WPS cache marker must be created");
        }
        let request = |dry_run| CleanupRequest {
            rule_ids: vec![
                "app.wps-cache".to_string(),
                "app.wps-rendering-cache".to_string(),
                "app.wps-diagnostic-cache".to_string(),
            ],
            source_selections: Vec::new(),
            dry_run,
            project_roots: Vec::new(),
        };

        let preview = CleanupService::execute(request(true))
            .expect("the real WPS cache dry run must succeed");
        assert_eq!(preview.actions.len(), 3);
        assert_eq!(preview.failed_item_count, 0, "{:?}", preview.actions);
        assert!(preview.expected_bytes >= markers.len() as u64 * 7);
        assert!(markers.iter().all(|marker| marker.exists()));

        let result = CleanupService::execute(request(false))
            .expect("the real WPS cache cleanup must succeed");
        assert_eq!(result.actions.len(), 3);
        assert_eq!(result.failed_item_count, 0, "{:?}", result.actions);
        assert!(result.released_bytes >= markers.len() as u64 * 7);
        assert!(markers.iter().all(|marker| !marker.exists()));
        let preserved_after = preserved_snapshot();
        assert_eq!(preserved_after, preserved_before);
        println!(
            "real_windows_wps_cache_cleanup expected_bytes={} released_bytes={} affected_item_count={} cef_partition_count={} target_root_count={} preserved_root_count={}",
            preview.expected_bytes,
            result.released_bytes,
            result.affected_item_count,
            cef_partition_count,
            target_roots.len(),
            preserved_roots.len() + 2
        );
    }

    /// Exercises the NetEase Cloud Music owner boundary while the real client
    /// is running. Its renderer cache shares one application root with music,
    /// account, library, and browser state, so traversal must not start until
    /// every cloudmusic process has stopped.
    #[test]
    #[ignore = "requires the real NetEase Cloud Music application to be running"]
    fn real_netease_cloud_music_cache_blocks_while_running() {
        assert_eq!(
            std::env::var("MANGODISK_TEST_REAL_NETEASE_CLOUD_MUSIC_CACHE_BLOCK").as_deref(),
            Ok("1"),
            "set MANGODISK_TEST_REAL_NETEASE_CLOUD_MUSIC_CACHE_BLOCK=1 for the real process-gate diagnostic"
        );
        let running = ProcessSnapshot::capture()
            .expect("the Windows process inventory must be available")
            .matching_processes(&["cloudmusic.exe".to_string()]);
        assert!(!running.is_empty(), "NetEase Cloud Music must be running");

        let result = CleanupService::execute(CleanupRequest {
            rule_ids: vec!["app.netease-cloud-music-cache".to_string()],
            source_selections: Vec::new(),
            dry_run: false,
            project_roots: Vec::new(),
        })
        .expect("the blocked NetEase Cloud Music cleanup must return a structured result");
        assert_eq!(result.actions.len(), 1);
        let action = &result.actions[0];
        assert_eq!(action.status, crate::cleanup::CleanupActionStatus::Blocked);
        assert_eq!(
            action.reason_code,
            Some(crate::cleanup::CleanupActionReason::RunningProcesses)
        );
        assert_eq!(action.released_bytes, 0);
        assert_eq!(action.affected_item_count, 0);
        assert!(!action.running_processes.is_empty());
        println!(
            "real_windows_netease_cloud_music_cache_block running_process_count={}",
            action.running_processes.len()
        );
    }

    /// Exercises only the fixed Chromium and diagnostic-log leaves produced by
    /// the official NetEase client. The top-level Cache is intentionally hashed
    /// rather than selected because it may contain music data. Library,
    /// downloaded web data, preferences, cookies, IndexedDB, Local/Session
    /// Storage, quota state, and crash dumps are also preserved explicitly.
    #[test]
    #[ignore = "clears real NetEase Cloud Music caches in an isolated Windows VM"]
    fn real_netease_cloud_music_cache_preserves_library_and_account_state() {
        assert_eq!(
            std::env::var("MANGODISK_TEST_REAL_NETEASE_CLOUD_MUSIC_CACHE").as_deref(),
            Ok("1"),
            "set MANGODISK_TEST_REAL_NETEASE_CLOUD_MUSIC_CACHE=1 only in an isolated Windows VM"
        );
        let local_app_data = std::env::var_os("LOCALAPPDATA")
            .map(PathBuf::from)
            .expect("LOCALAPPDATA must be available");
        let cloud_music_root = local_app_data.join("NetEase/CloudMusic");
        let renderer_root = cloud_music_root.join("webapp91x64");
        assert!(
            renderer_root.is_dir(),
            "NetEase Cloud Music must have completed a first launch"
        );
        let running = ProcessSnapshot::capture()
            .expect("the Windows process inventory must be available")
            .matching_processes(&["cloudmusic.exe".to_string()]);
        assert!(
            running.is_empty(),
            "NetEase Cloud Music must be completely stopped before the real cleanup diagnostic"
        );

        let preserved_paths = [
            "Cache",
            "Library",
            "Statics",
            "webdata",
            "dumps",
            "localdata",
            "localware",
            "webapp91x64/Cookies",
            "webapp91x64/IndexedDB",
            "webapp91x64/Local Storage",
            "webapp91x64/Session Storage",
            "webapp91x64/LocalPrefs.json",
            "webapp91x64/Network Persistent State",
            "webapp91x64/QuotaManager",
        ]
        .map(|relative| cloud_music_root.join(relative));
        assert!(
            preserved_paths.iter().all(|path| path.exists()),
            "the real profile must expose representative music, account, and browser state"
        );
        let preserved_before = preserved_paths
            .iter()
            .map(|path| digest_tree(path))
            .collect::<Vec<_>>();

        let target_roots = [renderer_root.join("Cache"), cloud_music_root.join("Log")];
        for root in &target_roots {
            fs::create_dir_all(root).expect("the NetEase Cloud Music cache root must be writable");
        }
        let markers = target_roots
            .iter()
            .map(|root| root.join("mangodisk-rule-validation.bin"))
            .collect::<Vec<_>>();
        for marker in &markers {
            fs::write(marker, b"payload")
                .expect("the isolated NetEase Cloud Music cache marker must be created");
        }
        let request = |dry_run| CleanupRequest {
            rule_ids: vec!["app.netease-cloud-music-cache".to_string()],
            source_selections: Vec::new(),
            dry_run,
            project_roots: Vec::new(),
        };

        let preview = CleanupService::execute(request(true))
            .expect("the real NetEase Cloud Music cache dry run must succeed");
        assert_eq!(preview.failed_item_count, 0, "{:?}", preview.actions);
        assert!(preview.expected_bytes >= markers.len() as u64 * 7);
        assert!(markers.iter().all(|marker| marker.exists()));

        let result = CleanupService::execute(request(false))
            .expect("the real NetEase Cloud Music cache cleanup must succeed");
        assert_eq!(result.failed_item_count, 0, "{:?}", result.actions);
        assert!(result.released_bytes >= markers.len() as u64 * 7);
        assert!(markers.iter().all(|marker| !marker.exists()));
        let preserved_after = preserved_paths
            .iter()
            .map(|path| digest_tree(path))
            .collect::<Vec<_>>();
        assert_eq!(preserved_after, preserved_before);
        println!(
            "real_netease_cloud_music_cache_cleanup expected_bytes={} released_bytes={} affected_item_count={} target_root_count={} preserved_root_count={}",
            preview.expected_bytes,
            result.released_bytes,
            result.affected_item_count,
            target_roots.len(),
            preserved_paths.len()
        );
    }

    /// Exercises the dedicated Notion process boundary while the signed client
    /// is running. The production gate must stop before traversing the mixed
    /// profile that also contains databases, offline pages, and browser state.
    #[test]
    #[ignore = "requires the real Notion application to be running"]
    fn real_notion_cache_blocks_while_running() {
        assert_eq!(
            std::env::var("MANGODISK_TEST_REAL_NOTION_CACHE_BLOCK").as_deref(),
            Ok("1"),
            "set MANGODISK_TEST_REAL_NOTION_CACHE_BLOCK=1 for the real process-gate diagnostic"
        );
        let running = ProcessSnapshot::capture()
            .expect("the Windows process inventory must be available")
            .matching_processes(&["Notion.exe".to_string()]);
        assert!(!running.is_empty(), "Notion must be running");

        let result = CleanupService::execute(CleanupRequest {
            rule_ids: vec!["app.notion-cache".to_string()],
            source_selections: Vec::new(),
            dry_run: false,
            project_roots: Vec::new(),
        })
        .expect("the blocked Notion cleanup must return a structured result");
        assert_eq!(result.actions.len(), 1);
        let action = &result.actions[0];
        assert_eq!(action.status, crate::cleanup::CleanupActionStatus::Blocked);
        assert_eq!(
            action.reason_code,
            Some(crate::cleanup::CleanupActionReason::RunningProcesses)
        );
        assert_eq!(action.released_bytes, 0);
        assert_eq!(action.affected_item_count, 0);
        assert!(!action.running_processes.is_empty());
        println!(
            "real_windows_notion_cache_block running_process_count={}",
            action.running_processes.len()
        );
    }

    /// Exercises the dedicated Notion rule against a real signed installation.
    /// Notion puts disposable renderer caches beside its local database and
    /// offline-capable browser state. Representative top-level state and every
    /// storage family present in each real renderer partition are therefore
    /// hashed before and after production cleanup.
    #[test]
    #[ignore = "clears real Notion rendering caches in an isolated Windows VM"]
    fn real_notion_partition_cache_preserves_database_and_offline_state() {
        assert_eq!(
            std::env::var("MANGODISK_TEST_REAL_NOTION_CACHE").as_deref(),
            Ok("1"),
            "set MANGODISK_TEST_REAL_NOTION_CACHE=1 only in an isolated Windows VM"
        );
        let roaming_app_data = std::env::var_os("APPDATA")
            .map(PathBuf::from)
            .expect("APPDATA must be available");
        let notion_root = roaming_app_data.join("Notion");
        let partitions_root = notion_root.join("Partitions");
        assert!(
            partitions_root.is_dir(),
            "Notion must have completed a first launch"
        );
        let running = ProcessSnapshot::capture()
            .expect("the Windows process inventory must be available")
            .matching_processes(&["Notion.exe".to_string()]);
        assert!(
            running.is_empty(),
            "Notion must be completely stopped before the real cleanup diagnostic"
        );

        let mut partition_roots = fs::read_dir(&partitions_root)
            .expect("the Notion partitions root must be readable")
            .map(|entry| entry.expect("the partition entry must be readable").path())
            .filter(|path| path.is_dir())
            .collect::<Vec<_>>();
        partition_roots.sort();
        assert!(
            !partition_roots.is_empty(),
            "the real Notion profile must expose at least one renderer partition"
        );

        let mut preserved_paths = [
            "notion.db",
            "state.json",
            "Preferences",
            "Local State",
            "Local Storage",
            "Network",
        ]
        .map(|relative| notion_root.join(relative))
        .into_iter()
        .filter(|path| path.exists())
        .collect::<Vec<_>>();
        for partition in &partition_roots {
            for relative in [
                "IndexedDB",
                "Local Storage",
                "Network",
                "Session Storage",
                "WebStorage",
                "Preferences",
                "Service Worker",
            ] {
                let path = partition.join(relative);
                if path.exists() {
                    preserved_paths.push(path);
                }
            }
        }
        assert!(
            preserved_paths.len() >= 10,
            "the real Notion profile must expose representative database and offline state"
        );
        let preserved_before = preserved_paths
            .iter()
            .map(|path| digest_tree(path))
            .collect::<Vec<_>>();

        let mut markers = Vec::with_capacity(partition_roots.len() + 1);
        let top_level_cache = notion_root.join("Cache");
        fs::create_dir_all(&top_level_cache).expect("the Notion top-level cache must be writable");
        markers.push(top_level_cache.join("mangodisk-rule-validation.bin"));
        for partition in &partition_roots {
            let cache = partition.join("Cache");
            fs::create_dir_all(&cache).expect("the Notion partition cache must be writable");
            markers.push(cache.join("mangodisk-rule-validation.bin"));
        }
        for marker in &markers {
            fs::write(marker, b"payload")
                .expect("the isolated Notion cache marker must be created");
        }
        let request = |dry_run| CleanupRequest {
            rule_ids: vec!["app.notion-cache".to_string()],
            source_selections: Vec::new(),
            dry_run,
            project_roots: Vec::new(),
        };

        let preview = CleanupService::execute(request(true))
            .expect("the real Notion cache dry run must succeed");
        assert_eq!(preview.failed_item_count, 0, "{:?}", preview.actions);
        assert!(preview.expected_bytes >= markers.len() as u64 * 7);
        assert!(markers.iter().all(|marker| marker.exists()));

        let result = CleanupService::execute(request(false))
            .expect("the real Notion cache cleanup must succeed");
        assert_eq!(result.failed_item_count, 0, "{:?}", result.actions);
        assert!(result.released_bytes >= markers.len() as u64 * 7);
        assert!(markers.iter().all(|marker| !marker.exists()));
        let preserved_after = preserved_paths
            .iter()
            .map(|path| digest_tree(path))
            .collect::<Vec<_>>();
        assert_eq!(preserved_after, preserved_before);
        println!(
            "real_notion_partition_cleanup expected_bytes={} released_bytes={} affected_item_count={} partition_count={} preserved_root_count={}",
            preview.expected_bytes,
            result.released_bytes,
            result.affected_item_count,
            partition_roots.len(),
            preserved_paths.len()
        );
    }

    /// Exercises the dedicated Signal process boundary while the signed client
    /// is running. Real execution must stop before reading account, database,
    /// optional-resource, or browser-persistence roots.
    #[test]
    #[ignore = "requires the real Signal application to be running"]
    fn real_signal_cache_blocks_while_running() {
        assert_eq!(
            std::env::var("MANGODISK_TEST_REAL_SIGNAL_CACHE_BLOCK").as_deref(),
            Ok("1"),
            "set MANGODISK_TEST_REAL_SIGNAL_CACHE_BLOCK=1 for the real process-gate diagnostic"
        );
        let running = ProcessSnapshot::capture()
            .expect("the Windows process inventory must be available")
            .matching_processes(&["Signal.exe".to_string()]);
        assert!(!running.is_empty(), "Signal must be running");

        let result = CleanupService::execute(CleanupRequest {
            rule_ids: vec!["app.signal-cache".to_string()],
            source_selections: Vec::new(),
            dry_run: false,
            project_roots: Vec::new(),
        })
        .expect("the blocked Signal cleanup must return a structured result");
        assert_eq!(result.actions.len(), 1);
        let action = &result.actions[0];
        assert_eq!(action.status, crate::cleanup::CleanupActionStatus::Blocked);
        assert_eq!(
            action.reason_code,
            Some(crate::cleanup::CleanupActionReason::RunningProcesses)
        );
        assert_eq!(action.released_bytes, 0);
        assert_eq!(action.affected_item_count, 0);
        assert!(!action.running_processes.is_empty());
        println!(
            "real_windows_signal_cache_block running_process_count={}",
            action.running_processes.len()
        );
    }

    /// Exercises the production Signal rule against a real signed Signal
    /// installation after an interactive first launch. The rule intentionally
    /// clears the disposable Chromium leaves, so the environment gate prevents
    /// accidental use on a developer's daily profile. Digests of representative
    /// account, database, storage, and optional-resource paths prove that the
    /// broad Signal user-data directory never becomes the deletion boundary.
    #[test]
    #[ignore = "clears real Signal rendering caches in an isolated Windows VM"]
    fn real_signal_cache_preserves_account_and_message_state_roots() {
        assert_eq!(
            std::env::var("MANGODISK_TEST_REAL_SIGNAL_CACHE").as_deref(),
            Ok("1"),
            "set MANGODISK_TEST_REAL_SIGNAL_CACHE=1 only in an isolated Windows VM"
        );
        let roaming_app_data = std::env::var_os("APPDATA")
            .map(PathBuf::from)
            .expect("APPDATA must be available");
        let signal_root = roaming_app_data.join("Signal");
        assert!(
            signal_root.is_dir(),
            "Signal must have completed a first launch"
        );
        let running = ProcessSnapshot::capture()
            .expect("the Windows process inventory must be available")
            .matching_processes(&["Signal.exe".to_string()]);
        assert!(
            running.is_empty(),
            "Signal must be completely stopped before the real cleanup diagnostic"
        );

        let preserved_paths = [
            "config.json",
            "optionalResources",
            "sql",
            "IndexedDB",
            "Local Storage",
        ]
        .map(|relative| signal_root.join(relative));
        assert!(
            preserved_paths.iter().all(|path| path.exists()),
            "the real profile must expose representative non-cache state"
        );
        let preserved_before = preserved_paths
            .iter()
            .map(|path| digest_tree(path))
            .collect::<Vec<_>>();

        let cache_root = signal_root.join("Cache");
        fs::create_dir_all(&cache_root).expect("the Signal HTTP cache root must be writable");
        let marker = cache_root.join("mangodisk-rule-validation.bin");
        fs::write(&marker, b"payload").expect("the isolated Signal cache marker must be created");
        let request = |dry_run| CleanupRequest {
            rule_ids: vec!["app.signal-cache".to_string()],
            source_selections: Vec::new(),
            dry_run,
            project_roots: Vec::new(),
        };

        let preview = CleanupService::execute(request(true))
            .expect("the real Signal cache dry run must succeed");
        assert_eq!(preview.failed_item_count, 0, "{:?}", preview.actions);
        assert!(preview.expected_bytes >= 7);
        assert!(marker.exists(), "dry-run must preserve the cache marker");

        let result = CleanupService::execute(request(false))
            .expect("the real Signal cache cleanup must succeed");
        assert_eq!(result.failed_item_count, 0, "{:?}", result.actions);
        assert!(result.released_bytes >= 7);
        assert!(
            !marker.exists(),
            "the selected cache marker must be deleted"
        );
        let preserved_after = preserved_paths
            .iter()
            .map(|path| digest_tree(path))
            .collect::<Vec<_>>();
        assert_eq!(preserved_after, preserved_before);
        println!(
            "real_signal_cache_cleanup expected_bytes={} released_bytes={} affected_item_count={} preserved_root_count={}",
            preview.expected_bytes,
            result.released_bytes,
            result.affected_item_count,
            preserved_paths.len()
        );
    }

    /// Validates Telegram's two fixed temporary roots without assigning cleanup
    /// meaning to neighboring hashed tdata entries. Telegram deliberately mixes
    /// account keys, settings, local storage, downloaded emoji resources, and
    /// disposable files in one directory, so every non-target direct child is
    /// hashed before and after the production cleanup.
    #[test]
    #[ignore = "clears real Telegram temporary data in an isolated Windows VM"]
    fn real_telegram_temporary_cache_preserves_all_other_tdata_entries() {
        assert_eq!(
            std::env::var("MANGODISK_TEST_REAL_TELEGRAM_CACHE").as_deref(),
            Ok("1"),
            "set MANGODISK_TEST_REAL_TELEGRAM_CACHE=1 only in an isolated Windows VM"
        );
        let roaming_app_data = std::env::var_os("APPDATA")
            .map(PathBuf::from)
            .expect("APPDATA must be available");
        let tdata_root = roaming_app_data.join("Telegram Desktop/tdata");
        assert!(
            tdata_root.is_dir(),
            "Telegram must have completed a first launch"
        );
        let running = ProcessSnapshot::capture()
            .expect("the Windows process inventory must be available")
            .matching_processes(&["Telegram.exe".to_string()]);
        assert!(
            running.is_empty(),
            "Telegram must be completely stopped before the real cleanup diagnostic"
        );

        let preserved_snapshot = || {
            let mut entries = fs::read_dir(&tdata_root)
                .expect("the Telegram tdata root must remain readable")
                .map(|entry| entry.expect("the tdata entry must be readable").path())
                .filter(|path| {
                    !matches!(
                        path.file_name().and_then(|name| name.to_str()),
                        Some("temp" | "dumps")
                    )
                })
                .map(|path| {
                    let name = path
                        .file_name()
                        .expect("the tdata entry must have a name")
                        .to_os_string();
                    (name, digest_tree(&path))
                })
                .collect::<Vec<_>>();
            entries.sort_by(|left, right| left.0.cmp(&right.0));
            entries
        };
        let preserved_before = preserved_snapshot();
        assert!(
            !preserved_before.is_empty(),
            "the real profile must expose non-cache tdata state"
        );

        let temp_root = tdata_root.join("temp");
        let dumps_root = tdata_root.join("dumps");
        fs::create_dir_all(&temp_root).expect("the Telegram temp root must be writable");
        fs::create_dir_all(&dumps_root).expect("the Telegram dumps root must be writable");
        let temp_marker = temp_root.join("mangodisk-rule-validation.tmp");
        let dump_marker = dumps_root.join("mangodisk-rule-validation.dmp");
        fs::write(&temp_marker, b"payload").expect("the Telegram temp marker must be created");
        fs::write(&dump_marker, b"payload").expect("the Telegram dump marker must be created");
        let request = |dry_run| CleanupRequest {
            rule_ids: vec!["app.telegram-temporary-cache".to_string()],
            source_selections: Vec::new(),
            dry_run,
            project_roots: Vec::new(),
        };

        let preview = CleanupService::execute(request(true))
            .expect("the real Telegram temporary-data dry run must succeed");
        assert_eq!(preview.failed_item_count, 0, "{:?}", preview.actions);
        assert!(preview.expected_bytes >= 14);
        assert!(temp_marker.exists() && dump_marker.exists());

        let result = CleanupService::execute(request(false))
            .expect("the real Telegram temporary-data cleanup must succeed");
        assert_eq!(result.failed_item_count, 0, "{:?}", result.actions);
        assert!(result.released_bytes >= 14);
        assert!(!temp_marker.exists() && !dump_marker.exists());
        assert_eq!(preserved_snapshot(), preserved_before);
        println!(
            "real_telegram_temporary_cleanup expected_bytes={} released_bytes={} affected_item_count={} preserved_entry_count={}",
            preview.expected_bytes,
            result.released_bytes,
            result.affected_item_count,
            preserved_before.len()
        );
    }

    /// Runs the production VLC rule against artwork reproduced from an embedded
    /// cover. A marker beside the two owned roots proves that the shared vlc
    /// parent remains configuration space rather than becoming a broad cache
    /// root. Extension filtering is exercised in both artwork and crashdump.
    #[test]
    #[ignore = "clears real VLC artwork and dump files in an isolated Windows VM"]
    fn real_vlc_cache_preserves_configuration_and_playlist_state() {
        assert_eq!(
            std::env::var("MANGODISK_TEST_REAL_VLC_CACHE").as_deref(),
            Ok("1"),
            "set MANGODISK_TEST_REAL_VLC_CACHE=1 only in an isolated Windows VM"
        );
        let roaming_app_data = std::env::var_os("APPDATA")
            .map(PathBuf::from)
            .expect("APPDATA must be available");
        let vlc_root = roaming_app_data.join("vlc");
        let art_root = vlc_root.join("art");
        assert!(
            art_root.is_dir(),
            "VLC must have cached a real embedded cover before this diagnostic"
        );
        let running = ProcessSnapshot::capture()
            .expect("the Windows process inventory must be available")
            .matching_processes(&["vlc.exe".to_string()]);
        assert!(
            running.is_empty(),
            "VLC must be completely stopped before the real cleanup diagnostic"
        );

        fs::create_dir_all(&vlc_root).expect("the VLC data root must be writable");
        let preserved_marker = vlc_root.join("mangodisk-rule-validation.cfg");
        fs::write(&preserved_marker, b"preserve")
            .expect("the VLC non-cache marker must be created");
        let preserved_before = digest_tree(&preserved_marker);
        let art_marker = art_root.join("mangodisk-rule-validation.png");
        fs::write(&art_marker, b"payload").expect("the VLC artwork marker must be created");
        let dump_root = vlc_root.join("crashdump");
        fs::create_dir_all(&dump_root).expect("the VLC crashdump root must be writable");
        let dump_marker = dump_root.join("mangodisk-rule-validation.dmp");
        fs::write(&dump_marker, b"payload").expect("the VLC dump marker must be created");
        let request = |dry_run| CleanupRequest {
            rule_ids: vec!["app.vlc-cache".to_string()],
            source_selections: Vec::new(),
            dry_run,
            project_roots: Vec::new(),
        };

        let preview = CleanupService::execute(request(true))
            .expect("the real VLC cache dry run must succeed");
        assert_eq!(preview.failed_item_count, 0, "{:?}", preview.actions);
        assert!(preview.expected_bytes >= 14);
        assert!(art_marker.exists() && dump_marker.exists());

        let result = CleanupService::execute(request(false))
            .expect("the real VLC cache cleanup must succeed");
        assert_eq!(result.failed_item_count, 0, "{:?}", result.actions);
        assert!(result.released_bytes >= 14);
        assert!(!art_marker.exists() && !dump_marker.exists());
        assert_eq!(digest_tree(&preserved_marker), preserved_before);
        fs::remove_file(&preserved_marker).expect("the VLC preserved marker must be removed");
        println!(
            "real_vlc_cache_cleanup expected_bytes={} released_bytes={} affected_item_count={}",
            preview.expected_bytes, result.released_bytes, result.affected_item_count
        );
    }

    /// Verifies EA's versioned CEF discovery and its independent QML cache.
    /// Browser storage and offline state are hashed because their neighboring
    /// names resemble Chromium cache data but can carry login or product state.
    /// The persistent background service may remain active; all processes that
    /// own the per-user interface are required to be stopped.
    #[test]
    #[ignore = "clears real EA interface caches in an isolated Windows VM"]
    fn real_ea_rendering_cache_preserves_browser_and_offline_state() {
        assert_eq!(
            std::env::var("MANGODISK_TEST_REAL_EA_CACHE").as_deref(),
            Ok("1"),
            "set MANGODISK_TEST_REAL_EA_CACHE=1 only in an isolated Windows VM"
        );
        let local_app_data = std::env::var_os("LOCALAPPDATA")
            .map(PathBuf::from)
            .expect("LOCALAPPDATA must be available");
        let ea_root = local_app_data.join("Electronic Arts/EA Desktop");
        let cef_root = ea_root.join("CEF");
        let version_root = fs::read_dir(&cef_root)
            .expect("the EA CEF generation root must be readable")
            .map(|entry| entry.expect("the CEF entry must be readable").path())
            .find(|path| path.join("EADesktop/BrowserCache").is_dir())
            .expect("a real EA CEF generation must contain BrowserCache");
        let browser_root = version_root.join("EADesktop/BrowserCache");
        let qml_cache = local_app_data.join("EADesktop/cache/qmlcache");
        assert!(qml_cache.is_dir(), "the real EA QML cache must exist");
        let required_processes =
            ["EADesktop.exe", "EACefSubProcess.exe", "EALocalHostSvc.exe"].map(str::to_string);
        let running = ProcessSnapshot::capture()
            .expect("the Windows process inventory must be available")
            .matching_processes(&required_processes);
        assert!(
            running.is_empty(),
            "EA interface processes must be stopped before the real cleanup diagnostic"
        );

        let preserved_paths = [
            browser_root.join("Local Storage"),
            browser_root.join("Network"),
            browser_root.join("Session Storage"),
            ea_root.join("OfflineCache"),
        ];
        assert!(
            preserved_paths.iter().all(|path| path.exists()),
            "the real EA profile must expose representative non-cache state"
        );
        let preserved_before = preserved_paths
            .iter()
            .map(|path| digest_tree(path))
            .collect::<Vec<_>>();

        let http_cache = browser_root.join("Cache");
        fs::create_dir_all(&http_cache).expect("the EA HTTP cache root must be writable");
        let cef_marker = http_cache.join("mangodisk-rule-validation.bin");
        let qml_marker = qml_cache.join("mangodisk-rule-validation.qmlc");
        fs::write(&cef_marker, b"payload").expect("the EA CEF marker must be created");
        fs::write(&qml_marker, b"payload").expect("the EA QML marker must be created");
        let request = |dry_run| CleanupRequest {
            rule_ids: vec!["app.ea-rendering-cache".to_string()],
            source_selections: Vec::new(),
            dry_run,
            project_roots: Vec::new(),
        };

        let preview =
            CleanupService::execute(request(true)).expect("the real EA cache dry run must succeed");
        assert_eq!(preview.failed_item_count, 0, "{:?}", preview.actions);
        assert!(preview.expected_bytes >= 14);
        assert!(cef_marker.exists() && qml_marker.exists());

        let result = CleanupService::execute(request(false))
            .expect("the real EA cache cleanup must succeed");
        assert_eq!(result.failed_item_count, 0, "{:?}", result.actions);
        assert!(result.released_bytes >= 14);
        assert!(!cef_marker.exists() && !qml_marker.exists());
        let preserved_after = preserved_paths
            .iter()
            .map(|path| digest_tree(path))
            .collect::<Vec<_>>();
        assert_eq!(preserved_after, preserved_before);
        println!(
            "real_ea_rendering_cleanup expected_bytes={} released_bytes={} affected_item_count={} preserved_root_count={}",
            preview.expected_bytes,
            result.released_bytes,
            result.affected_item_count,
            preserved_paths.len()
        );
    }

    /// Steam's `htmlcache` is a Chromium profile that mixes disposable renderer
    /// data with account-adjacent browser state. Hashing the representative
    /// databases, preferences, and storage directories proves that the rule
    /// selects only cache leaves. Game downloads and per-game shader caches are
    /// deliberately outside this test and outside the production rule.
    #[test]
    #[ignore = "clears real Steam interface caches in an isolated Windows VM"]
    fn real_steam_rendering_cache_preserves_browser_and_game_state() {
        assert_eq!(
            std::env::var("MANGODISK_TEST_REAL_STEAM_CACHE").as_deref(),
            Ok("1"),
            "set MANGODISK_TEST_REAL_STEAM_CACHE=1 only in an isolated Windows VM"
        );
        let local_app_data = std::env::var_os("LOCALAPPDATA")
            .map(PathBuf::from)
            .expect("LOCALAPPDATA must be available");
        let html_cache = local_app_data.join("Steam/htmlcache");
        let profile = html_cache.join("Default");
        assert!(profile.is_dir(), "Steam must have completed a first launch");
        let required_processes = ["steam.exe", "steamwebhelper.exe"].map(str::to_string);
        let running = ProcessSnapshot::capture()
            .expect("the Windows process inventory must be available")
            .matching_processes(&required_processes);
        assert!(
            running.is_empty(),
            "Steam and its web helpers must be stopped before the real cleanup diagnostic"
        );

        let preserved_paths = [
            profile.join("Login Data"),
            profile.join("History"),
            profile.join("Preferences"),
            profile.join("Local Storage"),
            profile.join("Network"),
            profile.join("Session Storage"),
        ];
        assert!(
            preserved_paths.iter().all(|path| path.exists()),
            "the real Steam profile must expose representative non-cache state"
        );
        let preserved_before = preserved_paths
            .iter()
            .map(|path| digest_tree(path))
            .collect::<Vec<_>>();

        let http_cache = profile.join("Cache");
        let gpu_cache = profile.join("GPUCache");
        fs::create_dir_all(&http_cache).expect("the Steam HTTP cache root must be writable");
        fs::create_dir_all(&gpu_cache).expect("the Steam GPU cache root must be writable");
        let http_marker = http_cache.join("mangodisk-rule-validation.bin");
        let gpu_marker = gpu_cache.join("mangodisk-rule-validation.bin");
        fs::write(&http_marker, b"payload").expect("the Steam HTTP marker must be created");
        fs::write(&gpu_marker, b"payload").expect("the Steam GPU marker must be created");
        let request = |dry_run| CleanupRequest {
            rule_ids: vec!["app.steam-rendering-cache".to_string()],
            source_selections: Vec::new(),
            dry_run,
            project_roots: Vec::new(),
        };

        let preview = CleanupService::execute(request(true))
            .expect("the real Steam cache dry run must succeed");
        assert_eq!(preview.failed_item_count, 0, "{:?}", preview.actions);
        assert!(preview.expected_bytes >= 14);
        assert!(http_marker.exists() && gpu_marker.exists());

        let result = CleanupService::execute(request(false))
            .expect("the real Steam cache cleanup must succeed");
        assert_eq!(result.failed_item_count, 0, "{:?}", result.actions);
        assert!(result.released_bytes >= 14);
        assert!(!http_marker.exists() && !gpu_marker.exists());
        let preserved_after = preserved_paths
            .iter()
            .map(|path| digest_tree(path))
            .collect::<Vec<_>>();
        assert_eq!(preserved_after, preserved_before);
        println!(
            "real_steam_rendering_cleanup expected_bytes={} released_bytes={} affected_item_count={} preserved_root_count={}",
            preview.expected_bytes,
            result.released_bytes,
            result.affected_item_count,
            preserved_paths.len()
        );
    }

    /// Battle.net keeps a large launcher-content cache beside CachedData.db and
    /// embeds Chromium profiles below BrowserCaches. The production rule owns
    /// the dedicated content root and renderer leaves only. Hashing the launcher
    /// database, account configuration, and browser storage prevents a broad
    /// third-party pattern from silently turning into account-state deletion.
    #[test]
    #[ignore = "clears real Battle.net caches in an isolated Windows VM"]
    fn real_battlenet_cache_preserves_launcher_and_browser_state() {
        assert_eq!(
            std::env::var("MANGODISK_TEST_REAL_BATTLENET_CACHE").as_deref(),
            Ok("1"),
            "set MANGODISK_TEST_REAL_BATTLENET_CACHE=1 only in an isolated Windows VM"
        );
        let local_app_data = std::env::var_os("LOCALAPPDATA")
            .map(PathBuf::from)
            .expect("LOCALAPPDATA must be available");
        let roaming_app_data = std::env::var_os("APPDATA")
            .map(PathBuf::from)
            .expect("APPDATA must be available");
        let battle_net_root = local_app_data.join("Battle.net");
        let browser_profile = battle_net_root.join("BrowserCaches/common");
        let content_cache = battle_net_root.join("Cache");
        assert!(
            content_cache.is_dir() && browser_profile.is_dir(),
            "Battle.net must have completed a first interactive launch"
        );
        let required_processes = ["Battle.net.exe", "Agent.exe"].map(str::to_string);
        let running = ProcessSnapshot::capture()
            .expect("the Windows process inventory must be available")
            .matching_processes(&required_processes);
        assert!(
            running.is_empty(),
            "Battle.net and Agent must be stopped before the real cleanup diagnostic"
        );

        let preserved_paths = [
            battle_net_root.join("CachedData.db"),
            roaming_app_data.join("Battle.net/Battle.net.config"),
            browser_profile.join("Local Storage"),
            browser_profile.join("Network"),
            browser_profile.join("Session Storage"),
        ];
        assert!(
            preserved_paths.iter().all(|path| path.exists()),
            "the real Battle.net profile must expose representative non-cache state"
        );
        let preserved_before = preserved_paths
            .iter()
            .map(|path| digest_tree(path))
            .collect::<Vec<_>>();

        let browser_cache = browser_profile.join("Cache");
        fs::create_dir_all(&browser_cache)
            .expect("the Battle.net browser cache root must be writable");
        let content_marker = content_cache.join("mangodisk-rule-validation.bin");
        let browser_marker = browser_cache.join("mangodisk-rule-validation.bin");
        fs::write(&content_marker, b"payload")
            .expect("the Battle.net content marker must be created");
        fs::write(&browser_marker, b"payload")
            .expect("the Battle.net browser marker must be created");
        let request = |dry_run| CleanupRequest {
            rule_ids: vec!["app.battlenet-cache".to_string()],
            source_selections: Vec::new(),
            dry_run,
            project_roots: Vec::new(),
        };

        let preview = CleanupService::execute(request(true))
            .expect("the real Battle.net cache dry run must succeed");
        assert_eq!(preview.failed_item_count, 0, "{:?}", preview.actions);
        assert!(preview.expected_bytes >= 14);
        assert!(content_marker.exists() && browser_marker.exists());

        let result = CleanupService::execute(request(false))
            .expect("the real Battle.net cache cleanup must succeed");
        assert_eq!(result.failed_item_count, 0, "{:?}", result.actions);
        assert!(result.released_bytes >= 14);
        assert!(!content_marker.exists() && !browser_marker.exists());
        let preserved_after = preserved_paths
            .iter()
            .map(|path| digest_tree(path))
            .collect::<Vec<_>>();
        assert_eq!(preserved_after, preserved_before);
        println!(
            "real_battlenet_cache_cleanup expected_bytes={} released_bytes={} affected_item_count={} preserved_root_count={}",
            preview.expected_bytes,
            result.released_bytes,
            result.affected_item_count,
            preserved_paths.len()
        );
    }

    /// VS Code's user-data root contains both large generated caches and the
    /// user's durable editor state. This test runs the production rule against
    /// the signed installed client and hashes representative settings, backup,
    /// workspace, authentication-adjacent, and storage roots. CachedData and
    /// CachedExtensionVSIXs are included because the matching VS Code source tag
    /// identifies them as generated code and bounded extension-download caches.
    #[test]
    #[ignore = "clears real VS Code caches in an isolated Windows VM"]
    fn real_vscode_cache_preserves_editor_and_workspace_state() {
        assert_eq!(
            std::env::var("MANGODISK_TEST_REAL_VSCODE_CACHE").as_deref(),
            Ok("1"),
            "set MANGODISK_TEST_REAL_VSCODE_CACHE=1 only in an isolated Windows VM"
        );
        let roaming_app_data = std::env::var_os("APPDATA")
            .map(PathBuf::from)
            .expect("APPDATA must be available");
        let code_root = roaming_app_data.join("Code");
        assert!(
            code_root.join("Cache").is_dir() && code_root.join("CachedData").is_dir(),
            "VS Code must have completed a first launch"
        );
        let running = ProcessSnapshot::capture()
            .expect("the Windows process inventory must be available")
            .matching_processes(&["Code.exe".to_string()]);
        assert!(
            running.is_empty(),
            "VS Code must be completely stopped before the real cleanup diagnostic"
        );

        let preserved_paths = [
            code_root.join("User"),
            code_root.join("Backups"),
            code_root.join("Local Storage"),
            code_root.join("Network"),
            code_root.join("Session Storage"),
            code_root.join("WebStorage"),
            code_root.join("CachedConfigurations"),
            code_root.join("CachedProfilesData"),
        ];
        assert!(
            preserved_paths.iter().all(|path| path.exists()),
            "the real VS Code profile must expose representative durable state"
        );
        let preserved_before = preserved_paths
            .iter()
            .map(|path| digest_tree(path))
            .collect::<Vec<_>>();

        let target_roots = [
            code_root.join("Cache"),
            code_root.join("CachedData"),
            code_root.join("CachedExtensionVSIXs"),
            code_root.join("Crashpad/reports"),
        ];
        assert!(
            target_roots.iter().all(|path| path.is_dir()),
            "the real VS Code profile must expose the source-proven target roots"
        );
        let markers = target_roots.map(|root| root.join("mangodisk-rule-validation.bin"));
        for marker in &markers {
            fs::write(marker, b"payload").expect("the VS Code cache marker must be created");
        }
        let request = |dry_run| CleanupRequest {
            rule_ids: vec!["dev.editor-cache".to_string()],
            source_selections: Vec::new(),
            dry_run,
            project_roots: Vec::new(),
        };

        let preview = CleanupService::execute(request(true))
            .expect("the real VS Code cache dry run must succeed");
        assert_eq!(preview.failed_item_count, 0, "{:?}", preview.actions);
        assert!(preview.expected_bytes >= 28);
        assert!(markers.iter().all(|marker| marker.exists()));

        let result = CleanupService::execute(request(false))
            .expect("the real VS Code cache cleanup must succeed");
        assert_eq!(result.failed_item_count, 0, "{:?}", result.actions);
        assert!(result.released_bytes >= 28);
        assert!(markers.iter().all(|marker| !marker.exists()));
        let preserved_after = preserved_paths
            .iter()
            .map(|path| digest_tree(path))
            .collect::<Vec<_>>();
        assert_eq!(preserved_after, preserved_before);
        println!(
            "real_vscode_cache_cleanup expected_bytes={} released_bytes={} affected_item_count={} preserved_root_count={}",
            preview.expected_bytes,
            result.released_bytes,
            result.affected_item_count,
            preserved_paths.len()
        );
    }

    /// Proves that the dedicated Postman rule owns only the Postman process
    /// boundary. The shared Electron rule used to couple this cleanup to every
    /// supported Electron application, so this real-process assertion guards
    /// both safe early blocking and the narrower application-specific design.
    #[test]
    #[ignore = "requires the real Postman application to be running"]
    fn real_postman_cache_blocks_while_running() {
        assert_eq!(
            std::env::var("MANGODISK_TEST_REAL_POSTMAN_CACHE_BLOCK").as_deref(),
            Ok("1"),
            "set MANGODISK_TEST_REAL_POSTMAN_CACHE_BLOCK=1 for the real process-gate diagnostic"
        );
        let running = ProcessSnapshot::capture()
            .expect("the Windows process inventory must be available")
            .matching_processes(&["Postman.exe".to_string()]);
        assert!(!running.is_empty(), "Postman must be running");

        let result = CleanupService::execute(CleanupRequest {
            rule_ids: vec!["app.postman-cache".to_string()],
            source_selections: Vec::new(),
            dry_run: false,
            project_roots: Vec::new(),
        })
        .expect("the blocked Postman cleanup must return a structured result");
        assert_eq!(result.actions.len(), 1);
        let action = &result.actions[0];
        assert_eq!(action.status, crate::cleanup::CleanupActionStatus::Blocked);
        assert_eq!(
            action.reason_code,
            Some(crate::cleanup::CleanupActionReason::RunningProcesses)
        );
        assert_eq!(action.released_bytes, 0);
        assert_eq!(action.affected_item_count, 0);
        assert!(!action.running_processes.is_empty());
        println!(
            "real_windows_postman_cache_block running_process_count={}",
            action.running_processes.len()
        );
    }

    /// Postman stores each Electron session in a dynamic partition directory.
    /// Its official recovery guidance distinguishes Clear Cache and Reload from
    /// deleting local data, and explicitly identifies Partitions plus storage as
    /// data to carry into a fresh profile. This test therefore hashes every
    /// non-cache state family while exercising only fixed cache suffixes across
    /// all real partitions discovered by the production declarative rule.
    #[test]
    #[ignore = "clears real Postman partition caches in an isolated Windows VM"]
    fn real_postman_partition_cache_preserves_workspace_and_session_state() {
        assert_eq!(
            std::env::var("MANGODISK_TEST_REAL_POSTMAN_CACHE").as_deref(),
            Ok("1"),
            "set MANGODISK_TEST_REAL_POSTMAN_CACHE=1 only in an isolated Windows VM"
        );
        let roaming_app_data = std::env::var_os("APPDATA")
            .map(PathBuf::from)
            .expect("APPDATA must be available");
        let postman_root = roaming_app_data.join("Postman");
        let partitions_root = postman_root.join("Partitions");
        assert!(
            partitions_root.is_dir(),
            "Postman must have completed a first launch"
        );
        let running = ProcessSnapshot::capture()
            .expect("the Windows process inventory must be available")
            .matching_processes(&["Postman.exe".to_string()]);
        assert!(
            running.is_empty(),
            "Postman must be completely stopped before the real cleanup diagnostic"
        );

        let mut partition_roots = fs::read_dir(&partitions_root)
            .expect("the Postman partitions root must be readable")
            .map(|entry| entry.expect("the partition entry must be readable").path())
            .filter(|path| path.is_dir())
            .collect::<Vec<_>>();
        partition_roots.sort();
        assert!(
            !partition_roots.is_empty(),
            "the real Postman profile must expose at least one partition"
        );
        let mut preserved_paths = vec![
            postman_root.join("storage"),
            postman_root.join("Local Storage"),
            postman_root.join("Network"),
        ];
        for partition in &partition_roots {
            for relative in [
                "IndexedDB",
                "Local Storage",
                "Network",
                "Session Storage",
                "WebStorage",
                "Preferences",
            ] {
                let path = partition.join(relative);
                if path.exists() {
                    preserved_paths.push(path);
                }
            }
        }
        assert!(
            preserved_paths.len() >= 8,
            "the real Postman profile must expose representative partition state"
        );
        let preserved_before = preserved_paths
            .iter()
            .map(|path| digest_tree(path))
            .collect::<Vec<_>>();

        let markers = partition_roots
            .iter()
            .map(|partition| {
                let cache = partition.join("Cache");
                fs::create_dir_all(&cache).expect("the Postman partition cache must be writable");
                let marker = cache.join("mangodisk-rule-validation.bin");
                fs::write(&marker, b"payload")
                    .expect("the Postman partition marker must be created");
                marker
            })
            .collect::<Vec<_>>();
        let request = |dry_run| CleanupRequest {
            rule_ids: vec!["app.postman-cache".to_string()],
            source_selections: Vec::new(),
            dry_run,
            project_roots: Vec::new(),
        };

        let preview = CleanupService::execute(request(true))
            .expect("the real Postman cache dry run must succeed");
        assert_eq!(preview.failed_item_count, 0, "{:?}", preview.actions);
        assert!(preview.expected_bytes >= (markers.len() as u64 * 7));
        assert!(markers.iter().all(|marker| marker.exists()));

        let result = CleanupService::execute(request(false))
            .expect("the real Postman cache cleanup must succeed");
        assert_eq!(result.failed_item_count, 0, "{:?}", result.actions);
        assert!(result.released_bytes >= (markers.len() as u64 * 7));
        assert!(markers.iter().all(|marker| !marker.exists()));
        let preserved_after = preserved_paths
            .iter()
            .map(|path| digest_tree(path))
            .collect::<Vec<_>>();
        assert_eq!(preserved_after, preserved_before);
        println!(
            "real_postman_partition_cleanup expected_bytes={} released_bytes={} affected_item_count={} partition_count={} preserved_root_count={}",
            preview.expected_bytes,
            result.released_bytes,
            result.affected_item_count,
            partition_roots.len(),
            preserved_paths.len()
        );
    }

    /// Spotify documents cache and offline downloads as separate data types
    /// and allows users to move the cache. The real client keeps two Chromium
    /// profiles, media Storage, and account state beside each other, so a broad
    /// LocalAppData rule would be unsafe. This diagnostic writes markers only
    /// to fixed renderer leaves and hashes login, history, network, media,
    /// Local State, and installation preferences before and after cleanup.
    #[test]
    #[ignore = "clears real Spotify rendering caches in an isolated Windows VM"]
    fn real_spotify_rendering_cache_preserves_account_and_offline_state() {
        assert_eq!(
            std::env::var("MANGODISK_TEST_REAL_SPOTIFY_CACHE").as_deref(),
            Ok("1"),
            "set MANGODISK_TEST_REAL_SPOTIFY_CACHE=1 only in an isolated Windows VM"
        );
        let local_app_data = std::env::var_os("LOCALAPPDATA")
            .map(PathBuf::from)
            .expect("LOCALAPPDATA must be available");
        let roaming_app_data = std::env::var_os("APPDATA")
            .map(PathBuf::from)
            .expect("APPDATA must be available");
        let spotify_root = local_app_data.join("Spotify");
        let browser_profile = spotify_root.join("Browser");
        let default_profile = spotify_root.join("Default");
        assert!(
            browser_profile.is_dir() && default_profile.is_dir(),
            "Spotify must have completed a first interactive launch"
        );
        let running = ProcessSnapshot::capture()
            .expect("the Windows process inventory must be available")
            .matching_processes(&["Spotify.exe".to_string()]);
        assert!(
            running.is_empty(),
            "Spotify must be completely stopped before the real cleanup diagnostic"
        );

        let preserved_paths = [
            spotify_root.join("Local State"),
            spotify_root.join("Storage"),
            browser_profile.join("History"),
            browser_profile.join("Login Data"),
            browser_profile.join("Preferences"),
            browser_profile.join("Local Storage"),
            browser_profile.join("Network"),
            browser_profile.join("Session Storage"),
            default_profile.join("History"),
            default_profile.join("Login Data"),
            default_profile.join("Preferences"),
            default_profile.join("Local Storage"),
            default_profile.join("Network"),
            roaming_app_data.join("Spotify/prefs"),
        ];
        assert!(
            preserved_paths.iter().all(|path| path.exists()),
            "the real Spotify profile must expose representative durable state"
        );
        let preserved_before = preserved_paths
            .iter()
            .map(|path| digest_tree(path))
            .collect::<Vec<_>>();

        let target_roots = [
            browser_profile.join("Cache"),
            browser_profile.join("GPUCache"),
            default_profile.join("Cache"),
            spotify_root.join("ShaderCache"),
        ];
        assert!(
            target_roots.iter().all(|path| path.is_dir()),
            "the real Spotify profile must expose the verified renderer caches"
        );
        let markers = target_roots.map(|root| root.join("mangodisk-rule-validation.bin"));
        for marker in &markers {
            fs::write(marker, b"payload").expect("the Spotify cache marker must be created");
        }
        let request = |dry_run| CleanupRequest {
            rule_ids: vec!["app.spotify-rendering-cache".to_string()],
            source_selections: Vec::new(),
            dry_run,
            project_roots: Vec::new(),
        };

        let preview = CleanupService::execute(request(true))
            .expect("the real Spotify cache dry run must succeed");
        assert_eq!(preview.failed_item_count, 0, "{:?}", preview.actions);
        assert!(preview.expected_bytes >= 28);
        assert!(markers.iter().all(|marker| marker.exists()));

        let result = CleanupService::execute(request(false))
            .expect("the real Spotify cache cleanup must succeed");
        assert_eq!(result.failed_item_count, 0, "{:?}", result.actions);
        assert!(result.released_bytes >= 28);
        assert!(markers.iter().all(|marker| !marker.exists()));
        let preserved_after = preserved_paths
            .iter()
            .map(|path| digest_tree(path))
            .collect::<Vec<_>>();
        assert_eq!(preserved_after, preserved_before);
        println!(
            "real_spotify_rendering_cleanup expected_bytes={} released_bytes={} affected_item_count={} preserved_root_count={}",
            preview.expected_bytes,
            result.released_bytes,
            result.affected_item_count,
            preserved_paths.len()
        );
    }

    /// Dropbox mixes renderer caches and account state in both its Roaming
    /// AppData root and dynamic partitions. This test enumerates the real
    /// partitions, writes markers only to fixed Cache leaves, and hashes each
    /// partition's IndexedDB, local and network storage, sessions, WebStorage,
    /// and preferences so the UI rule cannot reach sign-in or synced content.
    #[test]
    #[ignore = "clears real Dropbox rendering caches in an isolated Windows VM"]
    fn real_dropbox_rendering_cache_preserves_account_and_sync_state() {
        assert_eq!(
            std::env::var("MANGODISK_TEST_REAL_DROPBOX_RENDERING_CACHE").as_deref(),
            Ok("1"),
            "set MANGODISK_TEST_REAL_DROPBOX_RENDERING_CACHE=1 only in an isolated Windows VM"
        );
        let roaming_app_data = std::env::var_os("APPDATA")
            .map(PathBuf::from)
            .expect("APPDATA must be available");
        let dropbox_root = roaming_app_data.join("Dropbox");
        let partitions_root = dropbox_root.join("Partitions");
        assert!(
            partitions_root.is_dir(),
            "Dropbox must be signed in and expose its partition root"
        );
        let running = ProcessSnapshot::capture()
            .expect("the Windows process inventory must be available")
            .matching_processes(&["Dropbox.exe".to_string()]);
        assert!(
            running.is_empty(),
            "Dropbox must be completely stopped before the real cleanup diagnostic"
        );

        let mut partition_roots = fs::read_dir(&partitions_root)
            .expect("the Dropbox partitions root must be readable")
            .map(|entry| entry.expect("the partition entry must be readable").path())
            .filter(|path| path.is_dir())
            .collect::<Vec<_>>();
        partition_roots.sort();
        assert!(
            partition_roots.len() >= 2,
            "the signed-in Dropbox profile must expose representative partitions"
        );
        let mut preserved_paths = vec![
            dropbox_root.join("Local State"),
            dropbox_root.join("Preferences"),
            dropbox_root.join("Local Storage"),
            dropbox_root.join("Network"),
            dropbox_root.join("SharedStorage"),
        ];
        for partition in &partition_roots {
            for relative in [
                "IndexedDB",
                "Local Storage",
                "Network",
                "Session Storage",
                "WebStorage",
                "Preferences",
            ] {
                let path = partition.join(relative);
                if path.exists() {
                    preserved_paths.push(path);
                }
            }
        }
        assert!(
            preserved_paths.len() >= 12,
            "the Dropbox profile must expose representative durable state"
        );
        let preserved_before = preserved_paths
            .iter()
            .map(|path| digest_tree(path))
            .collect::<Vec<_>>();

        let mut markers = vec![dropbox_root.join("Cache/mangodisk-rule-validation.bin")];
        for partition in &partition_roots {
            markers.push(partition.join("Cache/mangodisk-rule-validation.bin"));
        }
        for marker in &markers {
            let parent = marker
                .parent()
                .expect("the Dropbox marker must have a cache parent");
            fs::create_dir_all(parent).expect("the Dropbox cache root must be writable");
            fs::write(marker, b"payload").expect("the Dropbox cache marker must be created");
        }
        let request = |dry_run| CleanupRequest {
            rule_ids: vec!["app.dropbox-rendering-cache".to_string()],
            source_selections: Vec::new(),
            dry_run,
            project_roots: Vec::new(),
        };

        let preview = CleanupService::execute(request(true))
            .expect("the real Dropbox rendering cache dry run must succeed");
        assert_eq!(preview.failed_item_count, 0, "{:?}", preview.actions);
        assert!(preview.expected_bytes >= (markers.len() as u64 * 7));
        assert!(markers.iter().all(|marker| marker.exists()));

        let result = CleanupService::execute(request(false))
            .expect("the real Dropbox rendering cache cleanup must succeed");
        assert_eq!(result.failed_item_count, 0, "{:?}", result.actions);
        assert!(result.released_bytes >= (markers.len() as u64 * 7));
        assert!(markers.iter().all(|marker| !marker.exists()));
        let preserved_after = preserved_paths
            .iter()
            .map(|path| digest_tree(path))
            .collect::<Vec<_>>();
        assert_eq!(preserved_after, preserved_before);
        println!(
            "real_dropbox_rendering_cleanup expected_bytes={} released_bytes={} affected_item_count={} partition_count={} preserved_root_count={}",
            preview.expected_bytes,
            result.released_bytes,
            result.affected_item_count,
            partition_roots.len(),
            preserved_paths.len()
        );
    }

    /// Docker Desktop's dashboard uses an Electron profile, while images,
    /// containers, volumes, build cache, and WSL or Hyper-V disks are separate
    /// high-value boundaries. This test cleans only fixed renderer leaves in
    /// Roaming AppData and hashes dashboard configuration, projects,
    /// notifications, network, and session state around the operation.
    #[test]
    #[ignore = "clears real Docker Desktop rendering caches in an isolated Windows VM"]
    fn real_docker_desktop_rendering_cache_preserves_engine_and_dashboard_state() {
        assert_eq!(
            std::env::var("MANGODISK_TEST_REAL_DOCKER_DESKTOP_CACHE").as_deref(),
            Ok("1"),
            "set MANGODISK_TEST_REAL_DOCKER_DESKTOP_CACHE=1 only in an isolated Windows VM"
        );
        let roaming_app_data = std::env::var_os("APPDATA")
            .map(PathBuf::from)
            .expect("APPDATA must be available");
        let dashboard_root = roaming_app_data.join("Docker Desktop");
        assert!(
            dashboard_root.is_dir(),
            "Docker Desktop must have completed a first launch"
        );
        let running = ProcessSnapshot::capture()
            .expect("the Windows process inventory must be available")
            .matching_processes(&["Docker Desktop.exe".to_string()]);
        assert!(
            running.is_empty(),
            "Docker Desktop must be stopped before the real cleanup diagnostic"
        );

        let preserved_paths = [
            dashboard_root.join("Local State"),
            dashboard_root.join("Preferences"),
            dashboard_root.join("Local Storage"),
            dashboard_root.join("Network"),
            dashboard_root.join("Session Storage"),
            dashboard_root.join("SharedStorage"),
            dashboard_root.join("install-state.json"),
            dashboard_root.join("notifications.json"),
            dashboard_root.join("projects.json"),
            dashboard_root.join("window-management.json"),
        ];
        assert!(
            preserved_paths.iter().all(|path| path.exists()),
            "the Docker Desktop profile must expose representative durable state"
        );
        let preserved_before = preserved_paths
            .iter()
            .map(|path| digest_tree(path))
            .collect::<Vec<_>>();

        let target_roots = [
            dashboard_root.join("Cache"),
            dashboard_root.join("GPUCache"),
            dashboard_root.join("DawnCache"),
        ];
        assert!(
            target_roots.iter().all(|path| path.is_dir()),
            "the Docker Desktop profile must expose the verified renderer caches"
        );
        let markers = target_roots.map(|root| root.join("mangodisk-rule-validation.bin"));
        for marker in &markers {
            fs::write(marker, b"payload").expect("the Docker Desktop cache marker must be created");
        }
        let request = |dry_run| CleanupRequest {
            rule_ids: vec!["container.docker-desktop-rendering-cache".to_string()],
            source_selections: Vec::new(),
            dry_run,
            project_roots: Vec::new(),
        };

        let preview = CleanupService::execute(request(true))
            .expect("the real Docker Desktop cache dry run must succeed");
        assert_eq!(preview.failed_item_count, 0, "{:?}", preview.actions);
        assert!(preview.expected_bytes >= 21);
        assert!(markers.iter().all(|marker| marker.exists()));

        let result = CleanupService::execute(request(false))
            .expect("the real Docker Desktop cache cleanup must succeed");
        assert_eq!(result.failed_item_count, 0, "{:?}", result.actions);
        assert!(result.released_bytes >= 21);
        assert!(markers.iter().all(|marker| !marker.exists()));
        let preserved_after = preserved_paths
            .iter()
            .map(|path| digest_tree(path))
            .collect::<Vec<_>>();
        assert_eq!(preserved_after, preserved_before);
        println!(
            "real_docker_desktop_rendering_cleanup expected_bytes={} released_bytes={} affected_item_count={} preserved_root_count={}",
            preview.expected_bytes,
            result.released_bytes,
            result.affected_item_count,
            preserved_paths.len()
        );
    }

    /// Exercises the dedicated Discord process boundary with the signed client
    /// running. Real mode is intentional: a blocked result proves that cleanup
    /// stops before traversing account, chat, or browser-state directories.
    #[test]
    #[ignore = "requires the real Discord application to be running"]
    fn real_discord_cache_blocks_while_running() {
        assert_eq!(
            std::env::var("MANGODISK_TEST_REAL_DISCORD_CACHE_BLOCK").as_deref(),
            Ok("1"),
            "set MANGODISK_TEST_REAL_DISCORD_CACHE_BLOCK=1 for the real process-gate diagnostic"
        );
        let running = ProcessSnapshot::capture()
            .expect("the Windows process inventory must be available")
            .matching_processes(&["Discord.exe".to_string()]);
        assert!(!running.is_empty(), "Discord must be running");

        let result = CleanupService::execute(CleanupRequest {
            rule_ids: vec!["app.discord-cache".to_string()],
            source_selections: Vec::new(),
            dry_run: false,
            project_roots: Vec::new(),
        })
        .expect("the blocked Discord cleanup must return a structured result");
        assert_eq!(result.actions.len(), 1);
        let action = &result.actions[0];
        assert_eq!(action.status, crate::cleanup::CleanupActionStatus::Blocked);
        assert_eq!(
            action.reason_code,
            Some(crate::cleanup::CleanupActionReason::RunningProcesses)
        );
        assert_eq!(action.released_bytes, 0);
        assert_eq!(action.affected_item_count, 0);
        assert!(!action.running_processes.is_empty());
        println!(
            "real_windows_discord_cache_block running_process_count={}",
            action.running_processes.len()
        );
    }

    /// A directory name alone cannot justify an application in the shared
    /// Electron rule. Discord's current profile mixes account network state,
    /// Local Storage, Service Worker data, sessions, and WebStorage. This test
    /// writes markers only to fixed renderer and log leaves and hashes the
    /// representative durable state around the production cleanup.
    #[test]
    #[ignore = "clears real Discord rendering caches in an isolated Windows VM"]
    fn real_discord_cache_preserves_account_and_session_state() {
        assert_eq!(
            std::env::var("MANGODISK_TEST_REAL_DISCORD_CACHE").as_deref(),
            Ok("1"),
            "set MANGODISK_TEST_REAL_DISCORD_CACHE=1 only in an isolated Windows VM"
        );
        let roaming_app_data = std::env::var_os("APPDATA")
            .map(PathBuf::from)
            .expect("APPDATA must be available");
        let discord_root = roaming_app_data.join("discord");
        assert!(
            discord_root.is_dir(),
            "Discord must have completed a first launch"
        );
        let running = ProcessSnapshot::capture()
            .expect("the Windows process inventory must be available")
            .matching_processes(&["Discord.exe".to_string()]);
        assert!(
            running.is_empty(),
            "Discord must be completely stopped before the real cleanup diagnostic"
        );

        let preserved_paths = [
            discord_root.join("Local State"),
            discord_root.join("Preferences"),
            discord_root.join("settings.json"),
            discord_root.join("Local Storage"),
            discord_root.join("Network"),
            discord_root.join("Service Worker"),
            discord_root.join("Session Storage"),
            discord_root.join("WebStorage"),
            discord_root.join("shared_proto_db"),
        ];
        assert!(
            preserved_paths.iter().all(|path| path.exists()),
            "the Discord profile must expose representative durable state"
        );
        let preserved_before = preserved_paths
            .iter()
            .map(|path| digest_tree(path))
            .collect::<Vec<_>>();

        let target_roots = [
            discord_root.join("Cache"),
            discord_root.join("GPUCache"),
            discord_root.join("logs"),
        ];
        assert!(
            target_roots.iter().all(|path| path.is_dir()),
            "the Discord profile must expose the verified cache and log roots"
        );
        let markers = target_roots.map(|root| root.join("mangodisk-rule-validation.bin"));
        for marker in &markers {
            fs::write(marker, b"payload").expect("the Discord cache marker must be created");
        }
        let request = |dry_run| CleanupRequest {
            rule_ids: vec!["app.discord-cache".to_string()],
            source_selections: Vec::new(),
            dry_run,
            project_roots: Vec::new(),
        };

        let preview = CleanupService::execute(request(true))
            .expect("the real Discord cache dry run must succeed");
        assert_eq!(preview.failed_item_count, 0, "{:?}", preview.actions);
        assert!(preview.expected_bytes >= 21);
        assert!(markers.iter().all(|marker| marker.exists()));

        let result = CleanupService::execute(request(false))
            .expect("the real Discord cache cleanup must succeed");
        assert_eq!(result.failed_item_count, 0, "{:?}", result.actions);
        assert!(result.released_bytes >= 21);
        assert!(markers.iter().all(|marker| !marker.exists()));
        let preserved_after = preserved_paths
            .iter()
            .map(|path| digest_tree(path))
            .collect::<Vec<_>>();
        assert_eq!(preserved_after, preserved_before);
        println!(
            "real_discord_cache_cleanup expected_bytes={} released_bytes={} affected_item_count={} preserved_root_count={}",
            preview.expected_bytes,
            result.released_bytes,
            result.affected_item_count,
            preserved_paths.len()
        );
    }

    /// Submits production cleanup while the signed ZenAion process is running.
    /// The assertion proves that the process gate blocks the WebView2 rule
    /// before scanning or deletion and never reports released bytes.
    #[test]
    #[ignore = "requires the real ZenAion application to be running"]
    fn real_zenaion_cache_blocks_while_running() {
        assert_eq!(
            std::env::var("MANGODISK_TEST_REAL_ZENAION_CACHE_BLOCK").as_deref(),
            Ok("1"),
            "set MANGODISK_TEST_REAL_ZENAION_CACHE_BLOCK=1 for the real process-gate diagnostic"
        );
        let running = ProcessSnapshot::capture()
            .expect("the Windows process inventory must be available")
            .matching_processes(&["ZenAI.exe".to_string(), "zenai-host.exe".to_string()]);
        assert!(
            !running.is_empty(),
            "ZenAion or its agent host must be running"
        );

        let result = CleanupService::execute(CleanupRequest {
            rule_ids: vec!["app.zenaion-cache".to_string()],
            source_selections: Vec::new(),
            dry_run: false,
            project_roots: Vec::new(),
        })
        .expect("the blocked ZenAion cleanup must return a structured result");
        assert_eq!(result.actions.len(), 1);
        let action = &result.actions[0];
        assert_eq!(action.status, crate::cleanup::CleanupActionStatus::Blocked);
        assert_eq!(
            action.reason_code,
            Some(crate::cleanup::CleanupActionReason::RunningProcesses)
        );
        assert_eq!(action.released_bytes, 0);
        assert_eq!(action.affected_item_count, 0);
        assert!(!action.running_processes.is_empty());
        println!(
            "real_windows_zenaion_cache_block running_process_count={}",
            action.running_processes.len()
        );
    }

    /// Clears fixed cache leaves from a real ZenAion WebView2 profile and hashes
    /// account, cookie, history, browser-setting, and site-storage state. This
    /// proves that the rule never treats the complete WebView2 user data folder
    /// as disposable while exercising both dry-run and production execution.
    #[test]
    #[ignore = "clears real ZenAion WebView2 caches in an isolated Windows VM"]
    fn real_zenaion_cache_preserves_account_and_browser_state() {
        assert_eq!(
            std::env::var("MANGODISK_TEST_REAL_ZENAION_CACHE").as_deref(),
            Ok("1"),
            "set MANGODISK_TEST_REAL_ZENAION_CACHE=1 only in an isolated Windows VM"
        );
        let local_app_data = std::env::var_os("LOCALAPPDATA")
            .map(PathBuf::from)
            .expect("LOCALAPPDATA must be available");
        let zen_root = local_app_data.join("bot.zenai");
        let webview_root = zen_root.join("EBWebView");
        let default_root = webview_root.join("Default");
        assert!(
            default_root.is_dir(),
            "ZenAion must have completed a first launch"
        );
        let running = ProcessSnapshot::capture()
            .expect("the Windows process inventory must be available")
            .matching_processes(&["ZenAI.exe".to_string(), "zenai-host.exe".to_string()]);
        assert!(
            running.is_empty(),
            "ZenAion and its agent host must be stopped before cleanup"
        );

        let preserved_paths = [
            zen_root.join(".cookies"),
            webview_root.join("Local State"),
            default_root.join("History"),
            default_root.join("Preferences"),
            default_root.join("Web Data"),
            default_root.join("Local Storage"),
            default_root.join("Network"),
            default_root.join("IndexedDB"),
        ]
        .into_iter()
        .filter(|path| path.exists())
        .collect::<Vec<_>>();
        assert!(
            preserved_paths.len() >= 6,
            "the real profile must expose representative durable browser state"
        );
        let preserved_before = preserved_paths
            .iter()
            .map(|path| digest_tree(path))
            .collect::<Vec<_>>();

        let target_roots = [
            default_root.join("Cache"),
            default_root.join("Code Cache"),
            default_root.join("GPUCache"),
            webview_root.join("GraphiteDawnCache"),
        ];
        assert!(
            target_roots.iter().all(|path| path.is_dir()),
            "the real profile must expose the verified WebView2 cache roots"
        );
        let markers = target_roots.map(|root| root.join("mangodisk-rule-validation.bin"));
        for marker in &markers {
            fs::write(marker, b"payload").expect("the ZenAion cache marker must be created");
        }
        let request = |dry_run| CleanupRequest {
            rule_ids: vec!["app.zenaion-cache".to_string()],
            source_selections: Vec::new(),
            dry_run,
            project_roots: Vec::new(),
        };

        let preview = CleanupService::execute(request(true))
            .expect("the real ZenAion cache dry run must succeed");
        assert_eq!(preview.actions.len(), 1);
        assert_eq!(preview.failed_item_count, 0, "{:?}", preview.actions);
        assert!(preview.expected_bytes >= markers.len() as u64 * 7);
        assert!(markers.iter().all(|marker| marker.exists()));

        let result = CleanupService::execute(request(false))
            .expect("the real ZenAion cache cleanup must succeed");
        assert_eq!(result.actions.len(), 1);
        assert_eq!(result.failed_item_count, 0, "{:?}", result.actions);
        assert!(result.released_bytes >= markers.len() as u64 * 7);
        assert!(markers.iter().all(|marker| !marker.exists()));
        let preserved_after = preserved_paths
            .iter()
            .map(|path| digest_tree(path))
            .collect::<Vec<_>>();
        assert_eq!(preserved_after, preserved_before);
        println!(
            "real_windows_zenaion_cache_cleanup expected_bytes={} released_bytes={} affected_item_count={} preserved_root_count={}",
            preview.expected_bytes,
            result.released_bytes,
            result.affected_item_count,
            preserved_paths.len()
        );
    }

    /// Sends a real cleanup request while the signed Weixin client owns its
    /// renderer profiles. The blocked result proves that dynamic profile
    /// expansion never begins while Chromium files can still be open.
    #[test]
    #[ignore = "requires the real Weixin application to be running"]
    fn real_wechat_rendering_cache_blocks_while_running() {
        assert_eq!(
            std::env::var("MANGODISK_TEST_REAL_WECHAT_RENDERING_BLOCK").as_deref(),
            Ok("1"),
            "set MANGODISK_TEST_REAL_WECHAT_RENDERING_BLOCK=1 for the real process-gate diagnostic"
        );
        let running = ProcessSnapshot::capture()
            .expect("the Windows process inventory must be available")
            .matching_processes(&["Weixin.exe".to_string(), "WeChat.exe".to_string()]);
        assert!(!running.is_empty(), "Weixin or WeChat must be running");

        let result = CleanupService::execute(CleanupRequest {
            rule_ids: vec!["app.wechat-rendering-cache".to_string()],
            source_selections: Vec::new(),
            dry_run: false,
            project_roots: Vec::new(),
        })
        .expect("the blocked Weixin cleanup must return a structured result");
        assert_eq!(result.actions.len(), 1);
        let action = &result.actions[0];
        assert_eq!(action.status, crate::cleanup::CleanupActionStatus::Blocked);
        assert_eq!(
            action.reason_code,
            Some(crate::cleanup::CleanupActionReason::RunningProcesses)
        );
        assert_eq!(action.released_bytes, 0);
        assert_eq!(action.affected_item_count, 0);
        assert!(!action.running_processes.is_empty());
        println!(
            "real_windows_wechat_rendering_block running_process_count={}",
            action.running_processes.len()
        );
    }

    /// Clears fixed renderer-cache and mini-program compiled-code leaves below
    /// real Weixin profiles. Per-profile browser state and every user-root file
    /// except the exact `codecache` segment are hashed around production cleanup,
    /// proving that applet data, history, cookies, storage, service workers,
    /// messages, and downloaded files stay outside the rule.
    #[test]
    #[ignore = "clears real Weixin renderer caches in an isolated Windows VM"]
    fn real_wechat_rendering_cache_preserves_account_and_profile_state() {
        assert_eq!(
            std::env::var("MANGODISK_TEST_REAL_WECHAT_RENDERING_CACHE").as_deref(),
            Ok("1"),
            "set MANGODISK_TEST_REAL_WECHAT_RENDERING_CACHE=1 only in an isolated Windows VM"
        );
        let roaming_app_data = std::env::var_os("APPDATA")
            .map(PathBuf::from)
            .expect("APPDATA must be available");
        let tencent_root = roaming_app_data.join("Tencent");
        let current_root = tencent_root.join("xwechat/radium");
        let profiles_root = current_root.join("web/profiles");
        let users_root = current_root.join("users");
        let legacy_radium_root = tencent_root.join("WeChat/radium");
        let legacy_wmpf_cache = legacy_radium_root.join("WmpfCache");
        assert!(
            profiles_root.is_dir(),
            "Weixin must have completed a first launch"
        );
        assert!(
            users_root.is_dir(),
            "the real account state root must exist"
        );
        let running = ProcessSnapshot::capture()
            .expect("the Windows process inventory must be available")
            .matching_processes(&["Weixin.exe".to_string(), "WeChat.exe".to_string()]);
        assert!(
            running.is_empty(),
            "Weixin and WeChat must be stopped before cleanup"
        );

        let profile_roots = fs::read_dir(&profiles_root)
            .expect("the renderer profile root must remain readable")
            .map(|entry| entry.expect("the profile entry must be readable").path())
            .filter(|path| path.is_dir())
            .collect::<Vec<_>>();
        assert!(
            profile_roots.len() >= 7,
            "the initialized client must expose the observed renderer profiles"
        );

        let preserved_user_state_before =
            digest_tree_excluding_segments(&users_root, &["codecache"]);
        let legacy_applet = legacy_radium_root.join("Applet");
        let preserved_legacy_applet_before =
            legacy_applet.is_dir().then(|| digest_tree(&legacy_applet));
        let mut preserved_paths = [
            tencent_root.join("xwechat/login"),
            tencent_root.join("xwechat/config"),
            tencent_root.join("xwechat/All Users/config"),
            tencent_root.join("WeChat/All Users/config"),
        ]
        .into_iter()
        .filter(|path| path.exists())
        .collect::<Vec<_>>();
        for profile in &profile_roots {
            for relative in [
                "History",
                "History_encrypted",
                "History.wxbak",
                "Preferences",
                "Local Storage",
                "Network",
                "IndexedDB",
                "Session Storage",
                "WebStorage",
                "Service Worker",
            ] {
                let path = profile.join(relative);
                if path.exists() {
                    preserved_paths.push(path);
                }
            }
        }
        assert!(
            preserved_paths.len() >= 30,
            "the real profiles must expose representative persistent state"
        );
        let preserved_before = preserved_paths
            .iter()
            .map(|path| digest_tree(path))
            .collect::<Vec<_>>();

        let mut markers = Vec::new();
        for profile in &profile_roots {
            let cache = profile.join("Cache");
            if cache.is_dir() {
                let marker = cache.join("mangodisk-rule-validation.bin");
                fs::write(&marker, b"payload")
                    .expect("the Weixin profile cache marker must be created");
                markers.push(marker);
            }
        }
        let renderer_marker_count = markers.len();
        let user_roots = fs::read_dir(&users_root)
            .expect("the Weixin account root must remain readable")
            .map(|entry| {
                entry
                    .expect("the Weixin account entry must be readable")
                    .path()
            })
            .filter(|path| path.is_dir())
            .collect::<Vec<_>>();
        let applet_code_caches = user_roots
            .iter()
            .map(|user| user.join("applet/codecache"))
            .filter(|path| path.is_dir())
            .collect::<Vec<_>>();
        assert!(
            !applet_code_caches.is_empty(),
            "the initialized account must expose a mini-program code cache"
        );
        for cache in &applet_code_caches {
            let marker = cache.join("mangodisk-rule-validation.bin");
            fs::write(&marker, b"payload")
                .expect("the Weixin applet code-cache marker must be created");
            markers.push(marker);
        }
        assert!(
            legacy_wmpf_cache.is_dir(),
            "the migrated profile must expose the legacy WMPF cache"
        );
        let legacy_marker = legacy_wmpf_cache.join("mangodisk-rule-validation.bin");
        fs::write(&legacy_marker, b"payload")
            .expect("the legacy WMPF cache marker must be created");
        markers.push(legacy_marker);
        assert_eq!(
            renderer_marker_count,
            profile_roots.len(),
            "every observed renderer profile must expose a Cache root"
        );
        let request = |dry_run| CleanupRequest {
            rule_ids: vec!["app.wechat-rendering-cache".to_string()],
            source_selections: Vec::new(),
            dry_run,
            project_roots: Vec::new(),
        };

        let preview = CleanupService::execute(request(true))
            .expect("the real Weixin cache dry run must succeed");
        assert_eq!(preview.actions.len(), 1);
        assert_eq!(preview.failed_item_count, 0, "{:?}", preview.actions);
        assert!(preview.expected_bytes >= markers.len() as u64 * 7);
        assert!(markers.iter().all(|marker| marker.exists()));

        let result = CleanupService::execute(request(false))
            .expect("the real Weixin rendering cache cleanup must succeed");
        assert_eq!(result.actions.len(), 1);
        assert_eq!(result.failed_item_count, 0, "{:?}", result.actions);
        assert!(result.released_bytes >= markers.len() as u64 * 7);
        assert!(markers.iter().all(|marker| !marker.exists()));
        let preserved_after = preserved_paths
            .iter()
            .map(|path| digest_tree(path))
            .collect::<Vec<_>>();
        assert_eq!(preserved_after, preserved_before);
        assert_eq!(
            digest_tree_excluding_segments(&users_root, &["codecache"]),
            preserved_user_state_before
        );
        assert_eq!(
            legacy_applet.is_dir().then(|| digest_tree(&legacy_applet)),
            preserved_legacy_applet_before
        );
        println!(
            "real_windows_wechat_rendering_cleanup expected_bytes={} released_bytes={} affected_item_count={} profile_count={} applet_code_cache_count={} preserved_root_count={}",
            preview.expected_bytes,
            result.released_bytes,
            result.affected_item_count,
            profile_roots.len(),
            applet_code_caches.len(),
            preserved_paths.len()
        );
    }

    /// Sends production cleanup requests while the real Windows clients are
    /// running. Both executable names must trigger process preflight, and each
    /// blocked request must report zero deleted bytes and items.
    #[test]
    #[ignore = "requires the real QQ and WeCom applications to be running"]
    fn real_qq_wecom_caches_block_running_owners() {
        assert_eq!(
            std::env::var("MANGODISK_TEST_REAL_QQ_WECOM_CACHE_BLOCK").as_deref(),
            Ok("1"),
            "set MANGODISK_TEST_REAL_QQ_WECOM_CACHE_BLOCK=1 for the real process-gate diagnostic"
        );
        let cases = [
            ("app.qq-rendering-cache", "QQ.exe"),
            ("app.wecom-diagnostic-cache", "WXWork.exe"),
        ];
        let process_snapshot =
            ProcessSnapshot::capture().expect("the Windows process inventory must be available");

        for (rule_id, process_name) in cases {
            assert!(
                !process_snapshot
                    .matching_processes(&[process_name.to_string()])
                    .is_empty(),
                "both real cache owners must be running"
            );
            let result = CleanupService::execute(CleanupRequest {
                rule_ids: vec![rule_id.to_string()],
                source_selections: Vec::new(),
                dry_run: false,
                project_roots: Vec::new(),
            })
            .expect("the blocked cleanup must return a structured result");
            assert_eq!(result.actions.len(), 1);
            let action = &result.actions[0];
            assert_eq!(action.status, crate::cleanup::CleanupActionStatus::Blocked);
            assert_eq!(
                action.reason_code,
                Some(crate::cleanup::CleanupActionReason::RunningProcesses)
            );
            assert_eq!(action.released_bytes, 0);
            assert_eq!(action.affected_item_count, 0);
            assert!(!action.running_processes.is_empty());
        }
        println!(
            "real_windows_qq_wecom_cache_block owner_count={}",
            cases.len()
        );
    }

    /// Runs dry-run and real cleanup against QQ and WeCom in the isolated VM.
    /// Cache markers must be deleted while hashes prove that account/message
    /// roots, browser persistence, dictionary databases, and Crashpad metadata
    /// remain unchanged.
    #[test]
    #[ignore = "clears real QQ and WeCom caches in an isolated Windows VM"]
    fn real_qq_wecom_caches_preserve_durable_state() {
        assert_eq!(
            std::env::var("MANGODISK_TEST_REAL_QQ_WECOM_CACHE").as_deref(),
            Ok("1"),
            "set MANGODISK_TEST_REAL_QQ_WECOM_CACHE=1 to authorize this real cache diagnostic"
        );
        let process_snapshot =
            ProcessSnapshot::capture().expect("the Windows process inventory must be available");
        for process_name in ["QQ.exe", "WXWork.exe"] {
            assert!(
                process_snapshot
                    .matching_processes(&[process_name.to_string()])
                    .is_empty(),
                "both cache owners must be stopped before cleanup"
            );
        }

        let roaming = std::env::var_os("APPDATA")
            .map(PathBuf::from)
            .expect("APPDATA must be available");
        let qq_root = roaming.join("QQ");
        let wecom_root = roaming.join("Tencent/WXWork");
        assert!(qq_root.is_dir() && wecom_root.is_dir());

        let mut preserved_paths = Vec::new();
        for relative in [
            "blob_storage",
            "Dictionaries",
            "Local Storage",
            "Network",
            "Shared Dictionary/db",
            "Shared Dictionary/db-journal",
            "Crashpad/attachments",
            "Crashpad/metadata",
            "Crashpad/settings.dat",
        ] {
            let path = qq_root.join(relative);
            if path.exists() {
                preserved_paths.push(path);
            }
        }
        for relative in ["Data", "Applet"] {
            let path = wecom_root.join(relative);
            if path.exists() {
                preserved_paths.push(path);
            }
        }
        assert!(
            preserved_paths.len() >= 8,
            "the initialized clients must expose representative durable state"
        );
        let preserved_before = preserved_paths
            .iter()
            .map(|path| digest_tree(path))
            .collect::<Vec<_>>();

        let mut target_roots = Vec::new();
        for relative in [
            "Cache",
            "Code Cache",
            "GPUCache",
            "DawnCache",
            "DawnGraphiteCache",
            "DawnWebGPUCache",
            "GrShaderCache",
            "GraphiteDawnCache",
            "Shared Dictionary/cache",
            "log",
            "Crashpad/reports",
        ] {
            let path = qq_root.join(relative);
            if path.is_dir() {
                target_roots.push(path);
            }
        }
        let wecom_log = wecom_root.join("Log");
        if wecom_log.is_dir() {
            target_roots.push(wecom_log);
        }
        target_roots.sort();
        target_roots.dedup();
        assert!(
            target_roots.len() >= 6,
            "the initialized clients must expose verified cache roots"
        );

        let markers = target_roots
            .iter()
            .map(|root| root.join("mangodisk-rule-validation.bin"))
            .collect::<Vec<_>>();
        for marker in &markers {
            fs::write(marker, b"payload").expect("the cache marker must be created");
        }
        let request = |dry_run| CleanupRequest {
            rule_ids: vec![
                "app.qq-rendering-cache".to_string(),
                "app.wecom-diagnostic-cache".to_string(),
            ],
            source_selections: Vec::new(),
            dry_run,
            project_roots: Vec::new(),
        };

        let preview =
            CleanupService::execute(request(true)).expect("the real cache dry run must succeed");
        assert_eq!(preview.actions.len(), 2);
        assert_eq!(preview.failed_item_count, 0, "{:?}", preview.actions);
        assert!(preview.expected_bytes >= markers.len() as u64 * 7);
        assert!(markers.iter().all(|marker| marker.exists()));

        let result =
            CleanupService::execute(request(false)).expect("the real cache cleanup must succeed");
        assert_eq!(result.actions.len(), 2);
        assert_eq!(result.failed_item_count, 0, "{:?}", result.actions);
        assert!(result.released_bytes >= markers.len() as u64 * 7);
        assert!(markers.iter().all(|marker| !marker.exists()));
        let preserved_after = preserved_paths
            .iter()
            .map(|path| digest_tree(path))
            .collect::<Vec<_>>();
        assert_eq!(preserved_after, preserved_before);
        println!(
            "real_windows_qq_wecom_cache_cleanup expected_bytes={} released_bytes={} affected_item_count={} target_root_count={} preserved_root_count={}",
            preview.expected_bytes,
            result.released_bytes,
            result.affected_item_count,
            target_roots.len(),
            preserved_paths.len()
        );
    }

    /// Proves that the signed Windows executable reaches process preflight
    /// before any WebView2 profile or diagnostic root is traversed.
    #[test]
    #[ignore = "requires the real FlashVoice application to be running"]
    fn real_flashvoice_cache_blocks_while_running() {
        assert_eq!(
            std::env::var("MANGODISK_TEST_REAL_FLASHVOICE_CACHE_BLOCK").as_deref(),
            Ok("1"),
            "set MANGODISK_TEST_REAL_FLASHVOICE_CACHE_BLOCK=1 for the real process-gate diagnostic"
        );
        let process_snapshot =
            ProcessSnapshot::capture().expect("the Windows process inventory must be available");
        assert!(
            !process_snapshot
                .matching_processes(&["FlashVoice.exe".to_string()])
                .is_empty(),
            "the real FlashVoice application must be running"
        );

        let result = CleanupService::execute(CleanupRequest {
            rule_ids: vec!["app.flashvoice-cache".to_string()],
            source_selections: Vec::new(),
            dry_run: false,
            project_roots: Vec::new(),
        })
        .expect("the blocked cleanup must return a structured result");
        assert_eq!(result.actions.len(), 1);
        let action = &result.actions[0];
        assert_eq!(action.status, crate::cleanup::CleanupActionStatus::Blocked);
        assert_eq!(
            action.reason_code,
            Some(crate::cleanup::CleanupActionReason::RunningProcesses)
        );
        assert_eq!(action.released_bytes, 0);
        assert_eq!(action.affected_item_count, 0);
        assert!(!action.running_processes.is_empty());
        println!("real_windows_flashvoice_cache_block owner_count=1");
    }

    /// Clears fixed WebView2 cache leaves and diagnostics in the isolated VM.
    /// Full hashes cover voice models, recordings, transcription indexes,
    /// settings, browser history, storage, sessions, and credentials.
    #[test]
    #[ignore = "clears real FlashVoice caches in an isolated Windows VM"]
    fn real_flashvoice_cache_preserves_models_and_recordings() {
        assert_eq!(
            std::env::var("MANGODISK_TEST_REAL_FLASHVOICE_CACHE").as_deref(),
            Ok("1"),
            "set MANGODISK_TEST_REAL_FLASHVOICE_CACHE=1 to authorize this real cache diagnostic"
        );
        let process_snapshot =
            ProcessSnapshot::capture().expect("the Windows process inventory must be available");
        assert!(
            process_snapshot
                .matching_processes(&["FlashVoice.exe".to_string()])
                .is_empty(),
            "FlashVoice must be stopped before cleanup"
        );

        let roaming = std::env::var_os("APPDATA")
            .map(PathBuf::from)
            .expect("APPDATA must be available");
        let local = std::env::var_os("LOCALAPPDATA")
            .map(PathBuf::from)
            .expect("LOCALAPPDATA must be available");
        let support = roaming.join("com.flashvoices");
        let webview = local.join("com.flashvoices/EBWebView");
        assert!(support.is_dir() && webview.is_dir());

        let mut preserved_paths = Vec::new();
        for relative in [
            "models",
            "recordings",
            "transcriptions",
            "config.json",
            "fv_config.json",
            "fv_onboarding.json",
            "fv_recordings.json",
            "installation.json",
            "onboarding.json",
            "recordings.json",
            "transcriptions.json",
        ] {
            let path = support.join(relative);
            if path.exists() {
                preserved_paths.push(path);
            }
        }
        for relative in [
            "Local State",
            "Last Version",
            "Default/Preferences",
            "Default/Secure Preferences",
            "Default/History",
            "Default/Login Data",
            "Default/Login Data For Account",
            "Default/Local Storage",
            "Default/IndexedDB",
            "Default/Network",
            "Default/Session Storage",
            "Default/WebStorage",
            "Default/Extension Cookies",
            "Default/Web Data",
        ] {
            let path = webview.join(relative);
            if path.exists() {
                preserved_paths.push(path);
            }
        }
        assert!(
            preserved_paths.len() >= 20,
            "the initialized application must expose durable voice and browser state"
        );
        let preserved_before = preserved_paths
            .iter()
            .map(|path| digest_tree(path))
            .collect::<Vec<_>>();

        let mut target_roots = Vec::new();
        for relative in [
            "component_crx_cache",
            "extensions_crx_cache",
            "GPUPersistentCache",
            "GraphiteDawnCache",
            "GrShaderCache",
            "ShaderCache",
            "Crashpad/reports",
            "Default/Cache",
            "Default/Code Cache",
            "Default/GPUCache",
            "Default/DawnCache",
            "Default/DawnGraphiteCache",
            "Default/DawnWebGPUCache",
            "Default/GrShaderCache",
            "Default/GraphiteDawnCache",
            "Default/Shared Dictionary/cache",
            "Default/Service Worker/CacheStorage",
        ] {
            let path = webview.join(relative);
            if path.is_dir() {
                target_roots.push(path);
            }
        }
        let logs = support.join("logs");
        if logs.is_dir() {
            target_roots.push(logs);
        }
        target_roots.sort();
        target_roots.dedup();
        assert!(
            target_roots.len() >= 12,
            "the real WebView2 profile must expose verified cache leaves"
        );

        let markers = target_roots
            .iter()
            .map(|root| root.join("mangodisk-rule-validation.bin"))
            .collect::<Vec<_>>();
        for marker in &markers {
            fs::write(marker, b"payload").expect("the cache marker must be created");
        }
        let request = |dry_run| CleanupRequest {
            rule_ids: vec!["app.flashvoice-cache".to_string()],
            source_selections: Vec::new(),
            dry_run,
            project_roots: Vec::new(),
        };

        let preview = CleanupService::execute(request(true))
            .expect("the real FlashVoice cache dry run must succeed");
        assert_eq!(preview.actions.len(), 1);
        assert_eq!(preview.failed_item_count, 0, "{:?}", preview.actions);
        assert!(preview.expected_bytes >= markers.len() as u64 * 7);
        assert!(markers.iter().all(|marker| marker.exists()));

        let result = CleanupService::execute(request(false))
            .expect("the real FlashVoice cache cleanup must succeed");
        assert_eq!(result.actions.len(), 1);
        assert_eq!(result.failed_item_count, 0, "{:?}", result.actions);
        assert!(result.released_bytes >= markers.len() as u64 * 7);
        assert!(markers.iter().all(|marker| !marker.exists()));
        let preserved_after = preserved_paths
            .iter()
            .map(|path| digest_tree(path))
            .collect::<Vec<_>>();
        assert_eq!(preserved_after, preserved_before);
        println!(
            "real_windows_flashvoice_cache_cleanup expected_bytes={} released_bytes={} affected_item_count={} target_root_count={} preserved_root_count={}",
            preview.expected_bytes,
            result.released_bytes,
            result.affected_item_count,
            target_roots.len(),
            preserved_paths.len()
        );
    }

    /// Confirms that the Microsoft Store GitMind process blocks both renderer
    /// cache cleanup and shared updater-staging cleanup before traversal.
    #[test]
    #[ignore = "requires the real GitMind application to be running"]
    fn real_gitmind_caches_block_while_running() {
        assert_eq!(
            std::env::var("MANGODISK_TEST_REAL_GITMIND_CACHE_BLOCK").as_deref(),
            Ok("1"),
            "set MANGODISK_TEST_REAL_GITMIND_CACHE_BLOCK=1 for the real process-gate diagnostic"
        );
        let process_snapshot =
            ProcessSnapshot::capture().expect("the Windows process inventory must be available");
        assert!(
            !process_snapshot
                .matching_processes(&["GitMind.exe".to_string()])
                .is_empty(),
            "the real GitMind application must be running"
        );

        let result = CleanupService::execute(CleanupRequest {
            rule_ids: vec![
                "app.gitmind-rendering-cache".to_string(),
                "app.electron-updater-cache".to_string(),
            ],
            source_selections: Vec::new(),
            dry_run: false,
            project_roots: Vec::new(),
        })
        .expect("the blocked cleanup must return structured results");
        assert_eq!(result.actions.len(), 2);
        for action in &result.actions {
            assert_eq!(action.status, crate::cleanup::CleanupActionStatus::Blocked);
            assert_eq!(
                action.reason_code,
                Some(crate::cleanup::CleanupActionReason::RunningProcesses)
            );
            assert_eq!(action.released_bytes, 0);
            assert_eq!(action.affected_item_count, 0);
            assert!(!action.running_processes.is_empty());
        }
        println!("real_windows_gitmind_cache_block owner_count=1");
    }

    /// Clears fixed GitMind renderer leaves and three dedicated updater roots.
    /// Full hashes cover installed binaries, product configuration, mind-map
    /// state, credentials, history, cookies, network data, and browser storage.
    #[test]
    #[ignore = "clears real GitMind rendering and Electron updater caches in an isolated Windows VM"]
    fn real_gitmind_and_electron_updater_caches_preserve_durable_state() {
        assert_eq!(
            std::env::var("MANGODISK_TEST_REAL_GITMIND_CACHE").as_deref(),
            Ok("1"),
            "set MANGODISK_TEST_REAL_GITMIND_CACHE=1 to authorize this real cache diagnostic"
        );
        let process_snapshot =
            ProcessSnapshot::capture().expect("the Windows process inventory must be available");
        assert!(
            process_snapshot
                .matching_processes(&["GitMind.exe".to_string()])
                .is_empty(),
            "GitMind must be stopped before cleanup"
        );

        let roaming = std::env::var_os("APPDATA")
            .map(PathBuf::from)
            .expect("APPDATA must be available");
        let local = std::env::var_os("LOCALAPPDATA")
            .map(PathBuf::from)
            .expect("LOCALAPPDATA must be available");
        let program_files_x86 = std::env::var_os("ProgramFiles(x86)")
            .map(PathBuf::from)
            .expect("ProgramFiles(x86) must be available");
        let gitmind_root = roaming.join("GitMind");
        let webview = gitmind_root.join("webview/EBWebView");
        assert!(gitmind_root.is_dir() && webview.is_dir());

        let mut preserved_paths = Vec::new();
        for relative in [
            "Local Storage",
            "Session Storage",
            "IndexedDB",
            "shared_proto_db",
            "blob_storage",
            "Dictionaries",
            "GitMind",
            "Service Worker/Database",
            "Service Worker/ScriptCache",
        ] {
            let path = gitmind_root.join(relative);
            if path.exists() {
                preserved_paths.push(path);
            }
        }
        for relative in [
            "Local State",
            "Last Version",
            "Default/Preferences",
            "Default/Secure Preferences",
            "Default/History",
            "Default/Login Data",
            "Default/Login Data For Account",
            "Default/Local Storage",
            "Default/IndexedDB",
            "Default/Network",
            "Default/Session Storage",
            "Default/WebStorage",
            "Default/Extension Cookies",
            "Default/Web Data",
            "Default/Service Worker/Database",
            "Default/Service Worker/ScriptCache",
        ] {
            let path = webview.join(relative);
            if path.exists() {
                preserved_paths.push(path);
            }
        }
        for path in [
            roaming.join("com.wangxutech.gitmind.desktop"),
            roaming.join("weflow"),
            program_files_x86.join("Apowersoft/GitMind/GitMind.exe"),
        ] {
            if path.exists() {
                preserved_paths.push(path);
            }
        }
        assert!(
            preserved_paths.len() >= 18,
            "the initialized application must expose durable product and browser state"
        );
        let preserved_before = preserved_paths
            .iter()
            .map(|path| digest_tree(path))
            .collect::<Vec<_>>();

        let mut target_roots = Vec::new();
        for relative in [
            "Code Cache",
            "GPUCache",
            "Shared Dictionary/cache",
            "Service Worker/CacheStorage",
            "logs",
            "Crashpad/reports",
        ] {
            let path = gitmind_root.join(relative);
            if path.is_dir() {
                target_roots.push(path);
            }
        }
        for relative in [
            "component_crx_cache",
            "extensions_crx_cache",
            "GPUPersistentCache",
            "GraphiteDawnCache",
            "GrShaderCache",
            "ShaderCache",
            "Crashpad/reports",
            "Default/Cache",
            "Default/Code Cache",
            "Default/GPUCache",
            "Default/DawnCache",
            "Default/DawnGraphiteCache",
            "Default/DawnWebGPUCache",
            "Default/GrShaderCache",
            "Default/GraphiteDawnCache",
            "Default/Shared Dictionary/cache",
            "Default/Service Worker/CacheStorage",
        ] {
            let path = webview.join(relative);
            if path.is_dir() {
                target_roots.push(path);
            }
        }
        let updater_roots = [
            local.join("gowhisper-updater"),
            local.join("weflow-updater"),
            local.join("gitmind-updater"),
        ];
        assert!(updater_roots.iter().all(|root| root.is_dir()));
        target_roots.extend(updater_roots.iter().cloned());
        target_roots.sort();
        target_roots.dedup();
        assert!(
            target_roots.len() >= 17,
            "GitMind and the updater fixtures must expose verified cache roots"
        );

        let markers = target_roots
            .iter()
            .map(|root| root.join("mangodisk-rule-validation.bin"))
            .collect::<Vec<_>>();
        for marker in &markers {
            fs::write(marker, b"payload").expect("the cache marker must be created");
        }
        let request = |dry_run| CleanupRequest {
            rule_ids: vec![
                "app.gitmind-rendering-cache".to_string(),
                "app.electron-updater-cache".to_string(),
            ],
            source_selections: Vec::new(),
            dry_run,
            project_roots: Vec::new(),
        };

        let preview =
            CleanupService::execute(request(true)).expect("the real cache dry run must succeed");
        assert_eq!(preview.actions.len(), 2);
        assert_eq!(preview.failed_item_count, 0, "{:?}", preview.actions);
        assert!(preview.expected_bytes >= markers.len() as u64 * 7);
        assert!(markers.iter().all(|marker| marker.exists()));

        let result =
            CleanupService::execute(request(false)).expect("the real cache cleanup must succeed");
        assert_eq!(result.actions.len(), 2);
        assert_eq!(result.failed_item_count, 0, "{:?}", result.actions);
        assert!(result.released_bytes >= markers.len() as u64 * 7);
        assert!(markers.iter().all(|marker| !marker.exists()));
        assert!(updater_roots.iter().all(|root| !root.exists()));
        let preserved_after = preserved_paths
            .iter()
            .map(|path| digest_tree(path))
            .collect::<Vec<_>>();
        assert_eq!(preserved_after, preserved_before);
        println!(
            "real_windows_gitmind_updater_cache_cleanup expected_bytes={} released_bytes={} affected_item_count={} target_root_count={} preserved_root_count={}",
            preview.expected_bytes,
            result.released_bytes,
            result.affected_item_count,
            target_roots.len(),
            preserved_paths.len()
        );
    }

    /// Confirms that the Store-package host process blocks WhatsApp cache
    /// cleanup before MangoDisk traverses any WebView2 profile directory.
    #[test]
    #[ignore = "requires the real WhatsApp application to be running"]
    fn real_whatsapp_rendering_cache_blocks_while_running() {
        assert_eq!(
            std::env::var("MANGODISK_TEST_REAL_WHATSAPP_CACHE_BLOCK").as_deref(),
            Ok("1"),
            "set MANGODISK_TEST_REAL_WHATSAPP_CACHE_BLOCK=1 for the real process-gate diagnostic"
        );
        let process_snapshot =
            ProcessSnapshot::capture().expect("the Windows process inventory must be available");
        assert!(
            !process_snapshot
                .matching_processes(&["WhatsApp.Root.exe".to_string()])
                .is_empty(),
            "the real WhatsApp application must be running"
        );

        let result = CleanupService::execute(CleanupRequest {
            rule_ids: vec!["app.whatsapp-rendering-cache".to_string()],
            source_selections: Vec::new(),
            dry_run: false,
            project_roots: Vec::new(),
        })
        .expect("the blocked cleanup must return a structured result");
        assert_eq!(result.actions.len(), 1);
        let action = &result.actions[0];
        assert_eq!(action.status, crate::cleanup::CleanupActionStatus::Blocked);
        assert_eq!(
            action.reason_code,
            Some(crate::cleanup::CleanupActionReason::RunningProcesses)
        );
        assert_eq!(action.released_bytes, 0);
        assert_eq!(action.affected_item_count, 0);
        assert!(!action.running_processes.is_empty());
        println!("real_windows_whatsapp_cache_block owner_count=1");
    }

    /// Clears only fixed WebView2 cache leaves in the isolated VM. Full-tree
    /// hashes prove that package state, product extensions, preferences,
    /// browser storage, network data, and Service Worker scripts remain exact.
    #[test]
    #[ignore = "clears real WhatsApp caches in an isolated Windows VM"]
    fn real_whatsapp_rendering_cache_preserves_durable_state() {
        assert_eq!(
            std::env::var("MANGODISK_TEST_REAL_WHATSAPP_CACHE").as_deref(),
            Ok("1"),
            "set MANGODISK_TEST_REAL_WHATSAPP_CACHE=1 to authorize this real cache diagnostic"
        );
        let process_snapshot =
            ProcessSnapshot::capture().expect("the Windows process inventory must be available");
        assert!(
            process_snapshot
                .matching_processes(&["WhatsApp.Root.exe".to_string(), "WhatsApp.exe".to_string(),])
                .is_empty(),
            "WhatsApp must be stopped before cleanup"
        );

        let local = std::env::var_os("LOCALAPPDATA")
            .map(PathBuf::from)
            .expect("LOCALAPPDATA must be available");
        let package = local.join("Packages/5319275A.WhatsAppDesktop_cv1g1gvanyjgm");
        let local_cache = package.join("LocalCache");
        let webview = local_cache.join("EBWebView");
        assert!(package.is_dir() && webview.is_dir());

        let mut preserved_paths = Vec::new();
        for relative in [
            "LocalState",
            "Settings",
            "RoamingState",
            "LocalCache/ChromeCodeVerifyExtension",
            "LocalCache/ZoomExtension",
        ] {
            let path = package.join(relative);
            if path.exists() {
                preserved_paths.push(path);
            }
        }
        for relative in [
            "Local State",
            "Last Version",
            "Default/Preferences",
            "Default/Secure Preferences",
            "Default/History",
            "Default/Login Data",
            "Default/Login Data For Account",
            "Default/Local Storage",
            "Default/IndexedDB",
            "Default/Network",
            "Default/Session Storage",
            "Default/WebStorage",
            "Default/Extension Cookies",
            "Default/Web Data",
            "Default/Service Worker/Database",
            "Default/Service Worker/ScriptCache",
        ] {
            let path = webview.join(relative);
            if path.exists() {
                preserved_paths.push(path);
            }
        }
        assert!(
            preserved_paths.len() >= 12,
            "the initialized application must expose durable package and browser state"
        );
        let preserved_before = preserved_paths
            .iter()
            .map(|path| digest_tree(path))
            .collect::<Vec<_>>();

        let mut target_roots = Vec::new();
        for relative in [
            "component_crx_cache",
            "extensions_crx_cache",
            "GPUPersistentCache",
            "GraphiteDawnCache",
            "GrShaderCache",
            "ShaderCache",
            "Crashpad/reports",
            "Default/Cache",
            "Default/Code Cache",
            "Default/GPUCache",
            "Default/DawnCache",
            "Default/DawnGraphiteCache",
            "Default/DawnWebGPUCache",
            "Default/GrShaderCache",
            "Default/GraphiteDawnCache",
            "Default/Shared Dictionary/cache",
            "Default/Service Worker/CacheStorage",
        ] {
            let path = webview.join(relative);
            if path.is_dir() {
                target_roots.push(path);
            }
        }
        assert!(
            target_roots.len() >= 13,
            "WhatsApp must expose the verified WebView2 cache leaves"
        );

        let markers = target_roots
            .iter()
            .map(|root| root.join("mangodisk-rule-validation.bin"))
            .collect::<Vec<_>>();
        for marker in &markers {
            fs::write(marker, b"payload").expect("the cache marker must be created");
        }
        let request = |dry_run| CleanupRequest {
            rule_ids: vec!["app.whatsapp-rendering-cache".to_string()],
            source_selections: Vec::new(),
            dry_run,
            project_roots: Vec::new(),
        };

        let preview =
            CleanupService::execute(request(true)).expect("the real cache dry run must succeed");
        assert_eq!(preview.actions.len(), 1);
        assert_eq!(preview.failed_item_count, 0, "{:?}", preview.actions);
        assert!(preview.expected_bytes >= markers.len() as u64 * 7);
        assert!(markers.iter().all(|marker| marker.exists()));

        let result =
            CleanupService::execute(request(false)).expect("the real cache cleanup must succeed");
        assert_eq!(result.actions.len(), 1);
        assert_eq!(result.failed_item_count, 0, "{:?}", result.actions);
        assert!(result.released_bytes >= markers.len() as u64 * 7);
        assert!(markers.iter().all(|marker| !marker.exists()));
        let preserved_after = preserved_paths
            .iter()
            .map(|path| digest_tree(path))
            .collect::<Vec<_>>();
        assert_eq!(preserved_after, preserved_before);
        println!(
            "real_windows_whatsapp_cache_cleanup expected_bytes={} released_bytes={} affected_item_count={} target_root_count={} preserved_root_count={}",
            preview.expected_bytes,
            result.released_bytes,
            result.affected_item_count,
            target_roots.len(),
            preserved_paths.len()
        );
    }

    /// Confirms that the Store-package host process blocks Codex cache cleanup
    /// before MangoDisk traverses logs or any WebView2 profile directory.
    #[test]
    #[ignore = "requires the real Codex application to be running"]
    fn real_codex_windows_cache_blocks_while_running() {
        assert_eq!(
            std::env::var("MANGODISK_TEST_REAL_CODEX_WINDOWS_CACHE_BLOCK").as_deref(),
            Ok("1"),
            "set MANGODISK_TEST_REAL_CODEX_WINDOWS_CACHE_BLOCK=1 for the real process-gate diagnostic"
        );
        let process_snapshot =
            ProcessSnapshot::capture().expect("the Windows process inventory must be available");
        assert!(
            !process_snapshot
                .matching_processes(&["ChatGPT.exe".to_string()])
                .is_empty(),
            "the real Codex application must be running"
        );

        let result = CleanupService::execute(CleanupRequest {
            rule_ids: vec!["app.chatgpt-cache".to_string()],
            source_selections: Vec::new(),
            dry_run: false,
            project_roots: Vec::new(),
        })
        .expect("the blocked cleanup must return a structured result");
        assert_eq!(result.actions.len(), 1);
        let action = &result.actions[0];
        assert_eq!(action.status, crate::cleanup::CleanupActionStatus::Blocked);
        assert_eq!(
            action.reason_code,
            Some(crate::cleanup::CleanupActionReason::RunningProcesses)
        );
        assert_eq!(action.released_bytes, 0);
        assert_eq!(action.affected_item_count, 0);
        assert!(!action.running_processes.is_empty());
        println!("real_windows_codex_cache_block owner_count=1");
    }

    /// Clears only fixed WebView2 cache leaves and diagnostic logs in the
    /// isolated VM. Full-tree hashes cover durable browser state and product
    /// components, while explicit file hashes protect the user-owned .codex
    /// credentials and configuration without traversing that large tree.
    #[test]
    #[ignore = "clears real Codex caches in an isolated Windows VM"]
    fn real_codex_windows_cache_preserves_projects_and_browser_state() {
        assert_eq!(
            std::env::var("MANGODISK_TEST_REAL_CODEX_WINDOWS_CACHE").as_deref(),
            Ok("1"),
            "set MANGODISK_TEST_REAL_CODEX_WINDOWS_CACHE=1 to authorize this real cache diagnostic"
        );
        let process_snapshot =
            ProcessSnapshot::capture().expect("the Windows process inventory must be available");
        assert!(
            process_snapshot
                .matching_processes(&["ChatGPT.exe".to_string()])
                .is_empty(),
            "Codex must be stopped before cleanup"
        );

        let local = std::env::var_os("LOCALAPPDATA")
            .map(PathBuf::from)
            .expect("LOCALAPPDATA must be available");
        let user_profile = std::env::var_os("USERPROFILE")
            .map(PathBuf::from)
            .expect("USERPROFILE must be available");
        let package = local.join("Packages/OpenAI.Codex_2p2nqsd0c76g0");
        let webview = package.join("LocalCache/Roaming/Codex/web/Codex");
        let logs = package.join("LocalCache/Local/Codex/Logs");
        assert!(package.is_dir() && webview.is_dir() && logs.is_dir());

        let mut preserved_paths = Vec::new();
        for relative in [
            "Settings",
            "LocalCache/Roaming/Codex/web/Codex/WasmTtsEngine",
            "LocalCache/Roaming/Codex/web/Codex/WidevineCdm",
            "LocalCache/Roaming/Codex/web/Codex/CertificateRevocation",
            "LocalCache/Roaming/Codex/web/Codex/ActorSafetyLists",
            "LocalCache/Roaming/Codex/web/Codex/ZxcvbnData",
        ] {
            let path = package.join(relative);
            if path.exists() {
                preserved_paths.push(path);
            }
        }
        for relative in [
            "Local State",
            "Last Version",
            "owl-feature-bootstrap-cache.json",
            "browser-sidebar-page-states.json",
            "Default/Preferences",
            "Default/Secure Preferences",
            "Default/History",
            "Default/Login Data",
            "Default/Login Data For Account",
            "Default/Local Storage",
            "Default/IndexedDB",
            "Default/Network",
            "Default/Session Storage",
            "Default/WebStorage",
            "Default/Extension Cookies",
            "Default/Web Data",
            "Default/Service Worker/Database",
            "Default/Service Worker/ScriptCache",
        ] {
            let path = webview.join(relative);
            if path.exists() {
                preserved_paths.push(path);
            }
        }

        let partitions = webview.join("Default/Partitions");
        let mut partition_count = 0_usize;
        if partitions.is_dir() {
            let mut children = fs::read_dir(&partitions)
                .expect("the Codex partition root must be readable")
                .map(|entry| entry.expect("the partition entry must be readable").path())
                .filter(|path| path.is_dir())
                .collect::<Vec<_>>();
            children.sort();
            partition_count = children.len();
            for partition in children {
                for relative in [
                    "Preferences",
                    "Secure Preferences",
                    "History",
                    "Login Data",
                    "Login Data For Account",
                    "Local Storage",
                    "IndexedDB",
                    "Network",
                    "Session Storage",
                    "WebStorage",
                    "Extension Cookies",
                    "Web Data",
                    "Service Worker/Database",
                    "Service Worker/ScriptCache",
                ] {
                    let path = partition.join(relative);
                    if path.exists() {
                        preserved_paths.push(path);
                    }
                }
            }
        }
        for relative in [".codex/auth.json", ".codex/config.toml"] {
            let path = user_profile.join(relative);
            if path.exists() {
                preserved_paths.push(path);
            }
        }
        preserved_paths.sort();
        preserved_paths.dedup();
        assert!(
            preserved_paths.len() >= 25,
            "the initialized app must expose durable package, browser, component, and .codex state"
        );
        let preserved_before = preserved_paths
            .iter()
            .map(|path| digest_tree(path))
            .collect::<Vec<_>>();

        let mut target_roots = Vec::new();
        if logs.is_dir() {
            target_roots.push(logs);
        }
        for relative in [
            "component_crx_cache",
            "extensions_crx_cache",
            "GPUPersistentCache",
            "GraphiteDawnCache",
            "GrShaderCache",
            "ShaderCache",
            "Crashpad/reports",
            "Default/Cache",
            "Default/Code Cache",
            "Default/GPUCache",
            "Default/DawnCache",
            "Default/DawnGraphiteCache",
            "Default/DawnWebGPUCache",
            "Default/GrShaderCache",
            "Default/GraphiteDawnCache",
            "Default/Shared Dictionary/cache",
            "Default/Service Worker/CacheStorage",
        ] {
            let path = webview.join(relative);
            if path.is_dir() {
                target_roots.push(path);
            }
        }
        if partitions.is_dir() {
            let mut children = fs::read_dir(&partitions)
                .expect("the Codex partition root must be readable")
                .map(|entry| entry.expect("the partition entry must be readable").path())
                .filter(|path| path.is_dir())
                .collect::<Vec<_>>();
            children.sort();
            for partition in children {
                for relative in [
                    "Cache",
                    "Code Cache",
                    "GPUCache",
                    "DawnCache",
                    "DawnGraphiteCache",
                    "DawnWebGPUCache",
                    "GrShaderCache",
                    "GraphiteDawnCache",
                    "Shared Dictionary/cache",
                    "Service Worker/CacheStorage",
                ] {
                    let path = partition.join(relative);
                    if path.is_dir() {
                        target_roots.push(path);
                    }
                }
            }
        }
        target_roots.sort();
        target_roots.dedup();
        assert!(
            target_roots.len() >= 19,
            "Codex must expose the verified log, WebView2, and partition cache leaves"
        );

        let markers = target_roots
            .iter()
            .map(|root| root.join("mangodisk-rule-validation.bin"))
            .collect::<Vec<_>>();
        for marker in &markers {
            fs::write(marker, b"payload").expect("the cache marker must be created");
        }
        let request = |dry_run| CleanupRequest {
            rule_ids: vec!["app.chatgpt-cache".to_string()],
            source_selections: Vec::new(),
            dry_run,
            project_roots: Vec::new(),
        };

        let preview =
            CleanupService::execute(request(true)).expect("the real cache dry run must succeed");
        assert_eq!(preview.actions.len(), 1);
        assert_eq!(preview.failed_item_count, 0, "{:?}", preview.actions);
        assert!(preview.expected_bytes >= markers.len() as u64 * 7);
        assert!(markers.iter().all(|marker| marker.exists()));

        let result =
            CleanupService::execute(request(false)).expect("the real cache cleanup must succeed");
        assert_eq!(result.actions.len(), 1);
        assert_eq!(result.failed_item_count, 0, "{:?}", result.actions);
        assert!(result.released_bytes >= markers.len() as u64 * 7);
        assert!(markers.iter().all(|marker| !marker.exists()));
        let preserved_after = preserved_paths
            .iter()
            .map(|path| digest_tree(path))
            .collect::<Vec<_>>();
        assert_eq!(preserved_after, preserved_before);
        println!(
            "real_windows_codex_cache_cleanup expected_bytes={} released_bytes={} affected_item_count={} partition_count={} target_root_count={} preserved_root_count={}",
            preview.expected_bytes,
            result.released_bytes,
            result.affected_item_count,
            partition_count,
            target_roots.len(),
            preserved_paths.len()
        );
    }

    /// Confirms that UC's browser and proxy processes block cleanup before
    /// the real Chromium profile or downloaded component cache is traversed.
    #[test]
    #[ignore = "requires the real UC browser to be running"]
    fn real_windows_uc_browser_cache_blocks_while_running() {
        assert_eq!(
            std::env::var("MANGODISK_TEST_REAL_WINDOWS_UC_CACHE_BLOCK").as_deref(),
            Ok("1"),
            "set MANGODISK_TEST_REAL_WINDOWS_UC_CACHE_BLOCK=1 for the real process-gate diagnostic"
        );
        let process_names = ["uc.exe", "uc_proxy.exe"].map(str::to_string);
        let running = ProcessSnapshot::capture()
            .expect("the Windows process inventory must be available")
            .matching_processes(&process_names);
        assert!(!running.is_empty(), "UC browser must be running");

        let result = CleanupService::execute(CleanupRequest {
            rule_ids: vec!["browser.uc-cache".to_string()],
            source_selections: Vec::new(),
            dry_run: false,
            project_roots: Vec::new(),
        })
        .expect("the blocked UC cleanup must return a structured result");
        assert_eq!(result.actions.len(), 1);
        let action = &result.actions[0];
        assert_eq!(action.status, crate::cleanup::CleanupActionStatus::Blocked);
        assert_eq!(
            action.reason_code,
            Some(crate::cleanup::CleanupActionReason::RunningProcesses)
        );
        assert_eq!(action.released_bytes, 0);
        assert_eq!(action.affected_item_count, 0);
        assert!(!action.running_processes.is_empty());
        println!(
            "real_windows_uc_cache_block running_process_count={}",
            running.len()
        );
    }

    /// Clears UC's fixed response, code, GPU, shader, and downloaded component
    /// caches. Full digests protect representative credentials, cookies,
    /// history, bookmarks, extensions, sessions, local storage, Service Worker
    /// state, and browser settings around the production cleanup.
    #[test]
    #[ignore = "clears real UC browser caches in an isolated Windows VM"]
    fn real_windows_uc_browser_cache_preserves_profile_state() {
        assert_eq!(
            std::env::var("MANGODISK_TEST_REAL_WINDOWS_UC_CACHE").as_deref(),
            Ok("1"),
            "set MANGODISK_TEST_REAL_WINDOWS_UC_CACHE=1 only in an isolated Windows VM"
        );
        let process_names = ["uc.exe", "uc_proxy.exe"].map(str::to_string);
        let running = ProcessSnapshot::capture()
            .expect("the Windows process inventory must be available")
            .matching_processes(&process_names);
        assert!(running.is_empty(), "UC browser must be completely stopped");

        let local_app_data = std::env::var_os("LOCALAPPDATA")
            .map(PathBuf::from)
            .expect("LOCALAPPDATA must be available");
        let user_data = local_app_data.join("UC/User Data");
        let profile = user_data.join("Default");
        assert!(profile.is_dir(), "UC must complete a first launch");

        let preserved_candidates = [
            user_data.join("Local State"),
            user_data.join("NativeMessagingHosts"),
            user_data.join("user_info"),
            profile.join("Bookmarks"),
            profile.join("History"),
            profile.join("Login Data"),
            profile.join("Network"),
            profile.join("Extensions"),
            profile.join("Local Storage"),
            profile.join("Preferences"),
            profile.join("Service Worker"),
            profile.join("Session Storage"),
            profile.join("Sessions"),
            profile.join("WebStorage"),
        ];
        let preserved_paths = preserved_candidates
            .into_iter()
            .filter(|path| path.exists())
            .collect::<Vec<_>>();
        assert!(
            preserved_paths.len() >= 11,
            "the initialized UC profile must expose representative durable state"
        );
        let preserved_before = preserved_paths
            .iter()
            .map(|path| digest_tree(path))
            .collect::<Vec<_>>();

        let target_candidates = [
            user_data.join("ShaderCache"),
            user_data.join("GrShaderCache"),
            user_data.join("GraphiteDawnCache"),
            user_data.join("component_crx_cache"),
            profile.join("Cache"),
            profile.join("Code Cache"),
            profile.join("GPUCache"),
            profile.join("DawnGraphiteCache"),
            profile.join("DawnWebGPUCache"),
        ];
        let expected_target_count = target_candidates.len();
        let target_roots = target_candidates
            .into_iter()
            .filter(|path| path.is_dir())
            .collect::<Vec<_>>();
        assert_eq!(
            target_roots.len(),
            expected_target_count,
            "the initialized UC profile must expose every verified cache root"
        );
        let markers = target_roots
            .iter()
            .map(|root| root.join("mangodisk-rule-validation.bin"))
            .collect::<Vec<_>>();
        for marker in &markers {
            fs::write(marker, b"payload").expect("the UC cache marker must be created");
        }
        let request = |dry_run| CleanupRequest {
            rule_ids: vec!["browser.uc-cache".to_string()],
            source_selections: Vec::new(),
            dry_run,
            project_roots: Vec::new(),
        };

        let preview =
            CleanupService::execute(request(true)).expect("the real UC cache dry run must succeed");
        assert_eq!(preview.failed_item_count, 0, "{:?}", preview.actions);
        assert!(preview.expected_bytes >= markers.len() as u64 * 7);
        assert!(markers.iter().all(|marker| marker.exists()));

        let result = CleanupService::execute(request(false))
            .expect("the real UC cache cleanup must succeed");
        assert_eq!(result.failed_item_count, 0, "{:?}", result.actions);
        assert!(result.released_bytes >= markers.len() as u64 * 7);
        assert!(markers.iter().all(|marker| !marker.exists()));
        let preserved_after = preserved_paths
            .iter()
            .map(|path| digest_tree(path))
            .collect::<Vec<_>>();
        assert_eq!(preserved_after, preserved_before);
        println!(
            "real_windows_uc_cache_cleanup expected_bytes={} released_bytes={} affected_item_count={} target_root_count={} preserved_root_count={}",
            preview.expected_bytes,
            result.released_bytes,
            result.affected_item_count,
            target_roots.len(),
            preserved_paths.len()
        );
    }

    fn assert_windows_browser_cache_blocked(
        rule_id: &str,
        process_names: &[&str],
        browser_name: &str,
    ) {
        let process_names = process_names
            .iter()
            .map(|name| (*name).to_string())
            .collect::<Vec<_>>();
        let running = ProcessSnapshot::capture()
            .expect("the Windows process inventory must be available")
            .matching_processes(&process_names);
        assert!(!running.is_empty(), "{browser_name} must be running");

        let result = CleanupService::execute(CleanupRequest {
            rule_ids: vec![rule_id.to_string()],
            source_selections: Vec::new(),
            dry_run: false,
            project_roots: Vec::new(),
        })
        .expect("the blocked browser cleanup must return a structured result");
        assert_eq!(result.actions.len(), 1);
        let action = &result.actions[0];
        assert_eq!(action.status, crate::cleanup::CleanupActionStatus::Blocked);
        assert_eq!(
            action.reason_code,
            Some(crate::cleanup::CleanupActionReason::RunningProcesses)
        );
        assert_eq!(action.released_bytes, 0);
        assert_eq!(action.affected_item_count, 0);
        assert!(!action.running_processes.is_empty());
        println!(
            "real_windows_browser_cache_block browser={} running_process_count={}",
            browser_name,
            running.len()
        );
    }

    fn validate_real_windows_browser_cache_cleanup(
        rule_id: &str,
        process_names: &[&str],
        profile_environment_variable: &str,
        user_data_relative: &str,
        preserved_relatives: &[&str],
        target_relatives: &[&str],
        minimum_preserved_count: usize,
    ) {
        let process_names = process_names
            .iter()
            .map(|name| (*name).to_string())
            .collect::<Vec<_>>();
        let running = ProcessSnapshot::capture()
            .expect("the Windows process inventory must be available")
            .matching_processes(&process_names);
        assert!(running.is_empty(), "{rule_id} must be completely stopped");

        let profile_base = std::env::var_os(profile_environment_variable)
            .map(PathBuf::from)
            .unwrap_or_else(|| panic!("{profile_environment_variable} must be available"));
        let user_data = profile_base.join(user_data_relative);
        assert!(
            user_data.join("Default").is_dir(),
            "{rule_id} must complete a first launch"
        );

        let preserved_paths = preserved_relatives
            .iter()
            .map(|relative| user_data.join(relative))
            .filter(|path| path.exists())
            .collect::<Vec<_>>();
        assert!(
            preserved_paths.len() >= minimum_preserved_count,
            "{rule_id} must expose representative durable profile state"
        );
        let preserved_before = preserved_paths
            .iter()
            .map(|path| digest_tree(path))
            .collect::<Vec<_>>();

        let target_roots = target_relatives
            .iter()
            .map(|relative| user_data.join(relative))
            .filter(|path| path.is_dir())
            .collect::<Vec<_>>();
        assert_eq!(
            target_roots.len(),
            target_relatives.len(),
            "{rule_id} must expose every verified cache root"
        );
        let markers = target_roots
            .iter()
            .map(|root| root.join("mangodisk-rule-validation.bin"))
            .collect::<Vec<_>>();
        for marker in &markers {
            fs::write(marker, b"payload").expect("the browser cache marker must be created");
        }
        let request = |dry_run| CleanupRequest {
            rule_ids: vec![rule_id.to_string()],
            source_selections: Vec::new(),
            dry_run,
            project_roots: Vec::new(),
        };

        let preview = CleanupService::execute(request(true))
            .expect("the real browser cache dry run must succeed");
        assert_eq!(preview.failed_item_count, 0, "{:?}", preview.actions);
        assert!(preview.expected_bytes >= markers.len() as u64 * 7);
        assert!(markers.iter().all(|marker| marker.exists()));

        let result = CleanupService::execute(request(false))
            .expect("the real browser cache cleanup must succeed");
        assert_eq!(result.failed_item_count, 0, "{:?}", result.actions);
        assert!(result.released_bytes >= markers.len() as u64 * 7);
        assert!(markers.iter().all(|marker| !marker.exists()));
        let preserved_after = preserved_paths
            .iter()
            .map(|path| digest_tree(path))
            .collect::<Vec<_>>();
        assert_eq!(preserved_after, preserved_before);
        println!(
            "real_windows_browser_cache_cleanup browser={} expected_bytes={} released_bytes={} affected_item_count={} target_root_count={} preserved_root_count={}",
            rule_id,
            preview.expected_bytes,
            result.released_bytes,
            result.affected_item_count,
            target_roots.len(),
            preserved_paths.len()
        );
    }

    /// Confirms that 360 Extreme Browser X blocks cleanup while any of its
    /// renderer processes still owns the real Chromium profile.
    #[test]
    #[ignore = "requires 360 Extreme Browser X to be running in the Windows VM"]
    fn real_windows_360_speed_browser_cache_blocks_while_running() {
        assert_eq!(
            std::env::var("MANGODISK_TEST_REAL_WINDOWS_360_CACHE_BLOCK").as_deref(),
            Ok("1"),
            "set MANGODISK_TEST_REAL_WINDOWS_360_CACHE_BLOCK=1 for the real process-gate diagnostic"
        );
        assert_windows_browser_cache_blocked(
            "browser.360-speed-cache",
            &["360ChromeX.exe"],
            "360-speed",
        );
    }

    /// Clears only 360 Extreme Browser X response, code, GPU, Dawn, and shader
    /// caches while hashing durable profile state around the production action.
    #[test]
    #[ignore = "clears real 360 Extreme Browser X caches in the Windows VM"]
    fn real_windows_360_speed_browser_cache_preserves_profile_state() {
        assert_eq!(
            std::env::var("MANGODISK_TEST_REAL_WINDOWS_360_CACHE").as_deref(),
            Ok("1"),
            "set MANGODISK_TEST_REAL_WINDOWS_360_CACHE=1 only in the isolated Windows VM"
        );
        validate_real_windows_browser_cache_cleanup(
            "browser.360-speed-cache",
            &["360ChromeX.exe"],
            "LOCALAPPDATA",
            "360ChromeX/Chrome/User Data",
            &[
                "Local State",
                "Default/Login Data",
                "Default/Network",
                "Default/Extensions",
                "Default/Local Storage",
                "Default/Preferences",
                "Default/Session Storage",
                "Default/Sessions",
                "Default/WebStorage",
                "Default/Download Service",
                "Default/Extension State",
            ],
            &[
                "ShaderCache64",
                "GrShaderCache64",
                "GraphiteDawnCache",
                "Default/Cache",
                "Default/Code Cache",
                "Default/GPUCache64",
                "Default/DawnGraphiteCache",
                "Default/DawnWebGPUCache",
            ],
            9,
        );
    }

    /// Confirms that Sogou Explorer blocks cleanup while its browser processes
    /// still own the real profile. Sogou Input processes are intentionally not
    /// part of this browser-specific gate.
    #[test]
    #[ignore = "requires Sogou Explorer to be running in the Windows VM"]
    fn real_windows_sogou_browser_cache_blocks_while_running() {
        assert_eq!(
            std::env::var("MANGODISK_TEST_REAL_WINDOWS_SOGOU_CACHE_BLOCK").as_deref(),
            Ok("1"),
            "set MANGODISK_TEST_REAL_WINDOWS_SOGOU_CACHE_BLOCK=1 for the real process-gate diagnostic"
        );
        assert_windows_browser_cache_blocked(
            "browser.sogou-cache",
            &["SogouExplorer.exe"],
            "sogou",
        );
    }

    /// Clears only Sogou Explorer response, code, GPU, Dawn, and shader caches
    /// while hashing credentials, history, storage, downloads, and settings.
    #[test]
    #[ignore = "clears real Sogou Explorer caches in the Windows VM"]
    fn real_windows_sogou_browser_cache_preserves_profile_state() {
        assert_eq!(
            std::env::var("MANGODISK_TEST_REAL_WINDOWS_SOGOU_CACHE").as_deref(),
            Ok("1"),
            "set MANGODISK_TEST_REAL_WINDOWS_SOGOU_CACHE=1 only in the isolated Windows VM"
        );
        validate_real_windows_browser_cache_cleanup(
            "browser.sogou-cache",
            &["SogouExplorer.exe"],
            "LOCALAPPDATA",
            "Sogou/SogouExplorer/User Data",
            &[
                "Local State",
                "Default/History",
                "Default/Login Data",
                "Default/Network",
                "Default/Extensions",
                "Default/Local Storage",
                "Default/Session Storage",
                "Default/Sessions",
                "Default/WebStorage",
                "Default/IndexedDB",
                "Default/Download Service",
                "Default/Extension State",
            ],
            &[
                "ShaderCache",
                "GrShaderCache",
                "GraphiteDawnCache",
                "Default/Cache",
                "Default/Code Cache",
                "Default/GPUCache",
                "Default/DawnCache",
            ],
            10,
        );
    }

    #[test]
    #[ignore = "requires 360 Safe Browser to be running in the Windows VM"]
    fn real_windows_360_safe_browser_cache_blocks_while_running() {
        assert_eq!(
            std::env::var("MANGODISK_TEST_REAL_WINDOWS_360_SAFE_CACHE_BLOCK").as_deref(),
            Ok("1"),
            "set MANGODISK_TEST_REAL_WINDOWS_360_SAFE_CACHE_BLOCK=1 for the real process-gate diagnostic"
        );
        assert_windows_browser_cache_blocked("browser.360-safe-cache", &["360se.exe"], "360-safe");
    }

    #[test]
    #[ignore = "clears real 360 Safe Browser caches in the Windows VM"]
    fn real_windows_360_safe_browser_cache_preserves_profile_state() {
        assert_eq!(
            std::env::var("MANGODISK_TEST_REAL_WINDOWS_360_SAFE_CACHE").as_deref(),
            Ok("1"),
            "set MANGODISK_TEST_REAL_WINDOWS_360_SAFE_CACHE=1 only in the isolated Windows VM"
        );
        validate_real_windows_browser_cache_cleanup(
            "browser.360-safe-cache",
            &["360se.exe"],
            "APPDATA",
            "360se6/User Data",
            &[
                "Local State",
                "Default/History",
                "Default/Login Data",
                "Default/Network",
                "Default/Extensions",
                "Default/Local Storage",
                "Default/Preferences",
                "Default/Session Storage",
                "Default/Sessions",
                "Default/WebStorage",
            ],
            &[
                "GraphiteDawnCache",
                "Default/Cache",
                "Default/Code Cache",
                "Default/DawnCache",
                "Default/Shared Dictionary/cache",
            ],
            5,
        );
    }

    #[test]
    #[ignore = "requires 2345 Browser to be running in the Windows VM"]
    fn real_windows_2345_browser_cache_blocks_while_running() {
        assert_eq!(
            std::env::var("MANGODISK_TEST_REAL_WINDOWS_2345_CACHE_BLOCK").as_deref(),
            Ok("1"),
            "set MANGODISK_TEST_REAL_WINDOWS_2345_CACHE_BLOCK=1 for the real process-gate diagnostic"
        );
        assert_windows_browser_cache_blocked("browser.2345-cache", &["2345Explorer.exe"], "2345");
    }

    #[test]
    #[ignore = "clears real 2345 Browser caches in the Windows VM"]
    fn real_windows_2345_browser_cache_preserves_profile_state() {
        assert_eq!(
            std::env::var("MANGODISK_TEST_REAL_WINDOWS_2345_CACHE").as_deref(),
            Ok("1"),
            "set MANGODISK_TEST_REAL_WINDOWS_2345_CACHE=1 only in the isolated Windows VM"
        );
        validate_real_windows_browser_cache_cleanup(
            "browser.2345-cache",
            &["2345Explorer.exe"],
            "LOCALAPPDATA",
            "2345Explorer/User Data",
            &[
                "Local State",
                "Default/History",
                "Default/Login Data",
                "Default/Network",
                "Default/Extensions",
                "Default/Local Storage",
                "Default/Preferences",
                "Default/Session Storage",
                "Default/Sessions",
                "Default/WebStorage",
            ],
            &[
                "ShaderCache",
                "GrShaderCache",
                "Default/Cache",
                "Default/Code Cache",
                "Default/DawnCache",
                "Default/GPUCache",
            ],
            5,
        );
    }

    fn directories_with_leaf_names(path: &Path, leaf_names: &[&str]) -> Vec<PathBuf> {
        fn collect(path: &Path, leaf_names: &[&str], directories: &mut Vec<PathBuf>) {
            let metadata =
                fs::symlink_metadata(path).expect("the WPS profile path must remain readable");
            assert!(
                !metadata.file_type().is_symlink(),
                "the WPS profile fixture must not cross a symbolic link"
            );
            if !metadata.is_dir() {
                return;
            }
            if path.file_name().is_some_and(|name| {
                leaf_names
                    .iter()
                    .any(|leaf| name.eq_ignore_ascii_case(leaf))
            }) {
                directories.push(path.to_path_buf());
                return;
            }
            let mut children = fs::read_dir(path)
                .expect("the WPS profile directory must remain readable")
                .map(|entry| entry.expect("the directory entry must be readable").path())
                .filter(|child| child.is_dir())
                .collect::<Vec<_>>();
            children.sort();
            for child in children {
                collect(&child, leaf_names, directories);
            }
        }

        let mut directories = Vec::new();
        collect(path, leaf_names, &mut directories);
        directories.sort();
        directories
    }

    fn digest_tree_excluding_segments(path: &Path, excluded_segments: &[&str]) -> String {
        fn collect_files(path: &Path, excluded_segments: &[&str], files: &mut Vec<PathBuf>) {
            let metadata = fs::symlink_metadata(path)
                .expect("the preserved WPS application state must remain readable");
            assert!(
                !metadata.file_type().is_symlink(),
                "the preserved WPS fixture must not contain links"
            );
            if metadata.is_file() {
                files.push(path.to_path_buf());
                return;
            }
            let mut children = fs::read_dir(path)
                .expect("the preserved WPS directory must remain readable")
                .map(|entry| entry.expect("the directory entry must be readable").path())
                .filter(|child| {
                    !child.file_name().is_some_and(|name| {
                        excluded_segments
                            .iter()
                            .any(|segment| name.eq_ignore_ascii_case(segment))
                    })
                })
                .collect::<Vec<_>>();
            children.sort();
            for child in children {
                collect_files(&child, excluded_segments, files);
            }
        }

        let mut files = Vec::new();
        collect_files(path, excluded_segments, &mut files);
        let mut hasher = Sha256::new();
        for file in files {
            let relative = file
                .strip_prefix(path)
                .unwrap_or(file.as_path())
                .to_string_lossy();
            hasher.update(relative.as_bytes());
            hasher.update(fs::read(file).expect("the preserved WPS file must remain readable"));
        }
        format!("{:x}", hasher.finalize())
    }

    fn digest_tree(path: &Path) -> String {
        fn collect_files(path: &Path, files: &mut Vec<PathBuf>) {
            let metadata =
                fs::symlink_metadata(path).expect("the preserved Signal path must remain readable");
            assert!(
                !metadata.file_type().is_symlink(),
                "the preserved Signal fixture must not contain links"
            );
            if metadata.is_file() {
                files.push(path.to_path_buf());
                return;
            }
            let mut children = fs::read_dir(path)
                .expect("the preserved Signal directory must remain readable")
                .map(|entry| entry.expect("the directory entry must be readable").path())
                .collect::<Vec<_>>();
            children.sort();
            for child in children {
                collect_files(&child, files);
            }
        }

        let mut files = Vec::new();
        collect_files(path, &mut files);
        let mut hasher = Sha256::new();
        for file in files {
            let relative = file
                .strip_prefix(path)
                .unwrap_or(file.as_path())
                .to_string_lossy();
            hasher.update(relative.as_bytes());
            hasher.update(fs::read(file).expect("the preserved Signal file must remain readable"));
        }
        format!("{:x}", hasher.finalize())
    }

    #[test]
    #[ignore = "modifies process environment; run this test alone"]
    fn zoom_diagnostic_rule_preserves_recent_logs_and_recordings() {
        let _operation_lock = crate::shared::operation::test_operation_lock();
        let sandbox = std::env::current_dir()
            .expect("test process should have a working directory")
            .join("target")
            .join(format!(
                "mangodisk-zoom-cleanup-test-{}-{}",
                std::process::id(),
                now_ms()
            ));
        let _sandbox_cleanup = DirectoryCleanup(sandbox.clone());
        let profile = sandbox.join("UserProfile");
        let roaming = sandbox.join("RoamingAppData");
        let old_log = roaming.join("Zoom/logs/old-diagnostic.log");
        let recent_log = roaming.join("Zoom/logs/recent-diagnostic.log");
        let recording = profile.join("Documents/Zoom/meeting-recording.mp4");
        for fixture in [&old_log, &recent_log, &recording] {
            fs::create_dir_all(fixture.parent().expect("fixture must have a parent"))
                .expect("should create the isolated Zoom directory");
            fs::write(fixture, b"MangoDisk Zoom cleanup fixture")
                .expect("should write the isolated Zoom fixture");
        }
        let old_time = SystemTime::now()
            .checked_sub(Duration::from_secs(15 * 86_400))
            .expect("test time should move back by fifteen days");
        fs::File::options()
            .write(true)
            .open(&old_log)
            .expect("should open the old Zoom log fixture")
            .set_times(fs::FileTimes::new().set_modified(old_time))
            .expect("should set the old Zoom log modification time");

        let _restore = EnvironmentRestore(vec![
            ("USERPROFILE", std::env::var_os("USERPROFILE")),
            ("APPDATA", std::env::var_os("APPDATA")),
        ]);
        std::env::set_var("USERPROFILE", &profile);
        std::env::set_var("APPDATA", &roaming);

        let preview = CleanupService::execute(CleanupRequest {
            rule_ids: vec!["app.zoom-diagnostic-cache".to_string()],
            source_selections: Vec::new(),
            dry_run: true,
            project_roots: Vec::new(),
        })
        .expect("isolated Zoom diagnostic preview should succeed");
        assert_eq!(preview.failed_item_count, 0);
        assert!(old_log.exists());
        assert!(recent_log.exists());
        assert!(recording.exists());

        let result = CleanupService::execute(CleanupRequest {
            rule_ids: vec!["app.zoom-diagnostic-cache".to_string()],
            source_selections: Vec::new(),
            dry_run: false,
            project_roots: Vec::new(),
        })
        .expect("isolated Zoom diagnostic cleanup should succeed");

        assert_eq!(result.failed_item_count, 0, "{:?}", result.actions);
        assert_eq!(result.affected_item_count, 1);
        assert!(
            !old_log.exists(),
            "Zoom diagnostic logs older than two weeks should be deleted"
        );
        assert!(
            recent_log.exists(),
            "recent Zoom diagnostic logs should remain available"
        );
        assert!(
            recording.exists(),
            "Zoom recordings must remain outside the cleanup boundary"
        );
    }

    #[test]
    #[ignore = "modifies process environment; run this test alone"]
    fn crash_dumps_and_windows_error_reports_are_actually_cleaned_in_isolated_roots() {
        const FIXTURE_CONTENT: &[u8] = b"MangoDisk safe cleanup fixture";

        let _operation_lock = crate::shared::operation::test_operation_lock();
        let sandbox = std::env::current_dir()
            .expect("test process should have a working directory")
            .join("target")
            .join(format!(
                "mangodisk-windows-cleanup-test-{}-{}",
                std::process::id(),
                now_ms()
            ));
        let _sandbox_cleanup = DirectoryCleanup(sandbox.clone());
        let local = sandbox.join("LocalAppData");
        let program_data = sandbox.join("ProgramData");
        let crash_dump = local.join("CrashDumps/fixture crash.dmp");
        let user_report =
            local.join("Microsoft/Windows/WER/ReportArchive/MangoDisk_User_Fixture/Report.wer");
        let system_report = program_data
            .join("Microsoft/Windows/WER/ReportQueue/MangoDisk_System_Fixture/Report.wer");
        let temporary_report = program_data.join("Microsoft/Windows/WER/Temp/fixture.tmp");
        for fixture in [&crash_dump, &user_report, &system_report, &temporary_report] {
            fs::create_dir_all(fixture.parent().expect("fixture must have a parent"))
                .expect("should create isolated diagnostic directory");
            fs::write(fixture, FIXTURE_CONTENT).expect("should write isolated diagnostic fixture");
        }

        let _restore = EnvironmentRestore(vec![
            ("LOCALAPPDATA", std::env::var_os("LOCALAPPDATA")),
            ("PROGRAMDATA", std::env::var_os("PROGRAMDATA")),
        ]);
        std::env::set_var("LOCALAPPDATA", &local);
        std::env::set_var("PROGRAMDATA", &program_data);

        let result = CleanupService::execute(CleanupRequest {
            rule_ids: vec![
                "system.crash-dumps".to_string(),
                "system.error-reports".to_string(),
            ],
            source_selections: Vec::new(),
            dry_run: false,
            project_roots: Vec::new(),
        })
        .expect("isolated CrashDumps and WER cleanup should succeed");

        assert_eq!(result.failed_item_count, 0);
        assert_eq!(result.affected_item_count, 4);
        assert_eq!(
            result.released_bytes,
            4 * u64::try_from(FIXTURE_CONTENT.len()).expect("fixture length should fit in u64")
        );
        assert!([crash_dump, user_report, system_report, temporary_report]
            .into_iter()
            .all(|fixture| !fixture.exists()));
    }

    #[test]
    #[ignore = "modifies process environment; run this test alone"]
    fn developer_cache_rules_preserve_windows_configuration_and_credentials() {
        let _operation_lock = crate::shared::operation::test_operation_lock();
        let sandbox = std::env::current_dir()
            .expect("test process should have a working directory")
            .join("target")
            .join(format!(
                "mangodisk-developer-cache-windows-cleanup-test-{}-{}",
                std::process::id(),
                now_ms()
            ));
        let _sandbox_cleanup = DirectoryCleanup(sandbox.clone());
        let profile = sandbox.join("UserProfile");
        let local = sandbox.join("LocalAppData");
        let roaming = sandbox.join("RoamingAppData");
        let cache_files = [
            roaming.join("ccache/a/result"),
            local.join("Mozilla/sccache/cache/0/compile-result"),
            profile.join(".hex/cache/registry.ets"),
            local.join("copilot/marketplace/index.json"),
            local.join("pypoetry/Cache/artifacts/aa/package.whl"),
            local.join("pypoetry/Cache/cache/repositories/PyPI/index.json"),
        ];
        let protected_files = [
            roaming.join("ccache/ccache.conf"),
            roaming.join("Mozilla/sccache/config/config"),
            profile.join(".hex/hex.config"),
            profile.join(".copilot/settings.json"),
            local.join("pypoetry/Cache/virtualenvs/project-py3.13/pyvenv.cfg"),
        ];
        for fixture in cache_files.iter().chain(&protected_files) {
            fs::create_dir_all(fixture.parent().expect("fixture must have a parent"))
                .expect("should create the isolated developer tool directory");
            fs::write(fixture, b"MangoDisk developer cache fixture")
                .expect("should write the isolated developer tool fixture");
        }

        let _restore = EnvironmentRestore(vec![
            ("USERPROFILE", std::env::var_os("USERPROFILE")),
            ("LOCALAPPDATA", std::env::var_os("LOCALAPPDATA")),
            ("APPDATA", std::env::var_os("APPDATA")),
        ]);
        std::env::set_var("USERPROFILE", &profile);
        std::env::set_var("LOCALAPPDATA", &local);
        std::env::set_var("APPDATA", &roaming);
        let rule_ids = [
            "dev.ccache-cache",
            "dev.sccache-cache",
            "dev.hex-cache",
            "dev.copilot-cli-cache",
            "dev.python-tooling-cache",
        ]
        .map(str::to_string)
        .to_vec();

        let preview = CleanupService::execute(CleanupRequest {
            rule_ids: rule_ids.clone(),
            source_selections: Vec::new(),
            dry_run: true,
            project_roots: Vec::new(),
        })
        .expect("isolated developer cache preview should succeed");
        assert_eq!(preview.failed_item_count, 0, "{:?}", preview.actions);
        assert!(cache_files.iter().all(|fixture| fixture.exists()));
        assert!(protected_files.iter().all(|fixture| fixture.exists()));

        let result = CleanupService::execute(CleanupRequest {
            rule_ids,
            source_selections: Vec::new(),
            dry_run: false,
            project_roots: Vec::new(),
        })
        .expect("isolated developer cache cleanup should succeed");
        assert_eq!(result.failed_item_count, 0, "{:?}", result.actions);
        assert_eq!(result.affected_item_count, 6);
        assert!(cache_files.iter().all(|fixture| !fixture.exists()));
        assert!(protected_files.iter().all(|fixture| fixture.exists()));
    }

    #[test]
    #[ignore = "clears real Poetry caches in the Windows VM"]
    fn real_windows_poetry_cache_preserves_virtual_environments() {
        assert_eq!(
            std::env::var("MANGODISK_TEST_REAL_WINDOWS_POETRY_CACHE").as_deref(),
            Ok("1"),
            "set MANGODISK_TEST_REAL_WINDOWS_POETRY_CACHE=1 only in the isolated Windows VM"
        );
        let local_app_data = std::env::var_os("LOCALAPPDATA")
            .map(PathBuf::from)
            .expect("LOCALAPPDATA must be available");
        let poetry_cache = local_app_data.join("pypoetry/Cache");
        let virtualenvs = poetry_cache.join("virtualenvs");
        assert!(virtualenvs.is_dir(), "Poetry must create a real virtualenv");
        let virtualenv = fs::read_dir(&virtualenvs)
            .expect("the Poetry virtualenv directory must remain readable")
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .find(|path| {
                path.join("pyvenv.cfg").is_file()
                    && path.join("Lib/site-packages/idna/__init__.py").is_file()
            })
            .expect("Poetry must create a virtualenv containing the test dependency");
        let protected_files = [
            virtualenv.join("pyvenv.cfg"),
            virtualenv.join("Lib/site-packages/idna/__init__.py"),
        ];
        let protected_before = protected_files
            .iter()
            .map(|path| fs::read(path).expect("the Poetry virtualenv file must be readable"))
            .collect::<Vec<_>>();

        let target_roots = [poetry_cache.join("artifacts"), poetry_cache.join("cache")];
        assert!(
            target_roots.iter().all(|root| root.is_dir()),
            "Poetry must populate both rebuildable cache roots"
        );
        let markers = target_roots
            .iter()
            .map(|root| root.join("mangodisk-rule-validation.bin"))
            .collect::<Vec<_>>();
        for marker in &markers {
            fs::write(marker, b"payload").expect("the Poetry cache marker must be created");
        }
        let request = |dry_run| CleanupRequest {
            rule_ids: vec!["dev.python-tooling-cache".to_string()],
            source_selections: Vec::new(),
            dry_run,
            project_roots: Vec::new(),
        };

        let preview = CleanupService::execute(request(true))
            .expect("the real Poetry cache dry run must succeed");
        assert_eq!(preview.failed_item_count, 0, "{:?}", preview.actions);
        assert!(markers.iter().all(|marker| marker.exists()));

        let result = CleanupService::execute(request(false))
            .expect("the real Poetry cache cleanup must succeed");
        assert_eq!(result.failed_item_count, 0, "{:?}", result.actions);
        assert!(markers.iter().all(|marker| !marker.exists()));
        let protected_after = protected_files
            .iter()
            .map(|path| fs::read(path).expect("the Poetry virtualenv file must remain readable"))
            .collect::<Vec<_>>();
        assert_eq!(protected_after, protected_before);
        println!(
            "real_windows_poetry_cache_cleanup expected_bytes={} released_bytes={} affected_item_count={}",
            preview.expected_bytes, result.released_bytes, result.affected_item_count
        );
    }

    #[test]
    #[ignore = "modifies process environment; run this test alone"]
    fn ai_cache_rules_clean_only_rebuildable_data_and_preserve_models() {
        let _operation_lock = crate::shared::operation::test_operation_lock();
        let sandbox = std::env::current_dir()
            .expect("test process should have a working directory")
            .join("target")
            .join(format!(
                "mangodisk-ai-cache-windows-cleanup-test-{}-{}",
                std::process::id(),
                now_ms()
            ));
        let _sandbox_cleanup = DirectoryCleanup(sandbox.clone());
        let profile = sandbox.join("UserProfile");
        let local = sandbox.join("LocalAppData");
        let roaming = sandbox.join("RoamingAppData");
        let downloads = profile.join("Downloads");
        let huggingface_hub = profile.join(".cache/huggingface/hub/models--fixture/blobs");
        let xet_environment = profile.join(".cache/huggingface/xet/environment");
        let xet_chunk_cache = xet_environment.join("chunk_cache");
        let xet_shard_cache = xet_environment.join("shard_cache");
        let xet_staging = xet_environment.join("staging");
        let project = profile.join("project");
        let adobe_local = local.join("Adobe/Common/Media Cache Files");
        let adobe_roaming = roaming.join("Adobe/Common/Media Cache Files");
        for directory in [
            &downloads,
            &huggingface_hub,
            &xet_chunk_cache,
            &xet_shard_cache,
            &xet_staging,
            &project,
            &adobe_local,
            &adobe_roaming,
        ] {
            fs::create_dir_all(directory).expect("should create isolated rule directory");
        }

        let stale_partial = downloads.join("old-model.crdownload");
        let recent_partial = downloads.join("active-model.crdownload");
        let completed_download = downloads.join("archive.zip");
        let downloaded_model = huggingface_hub.join("downloaded-model.bin");
        let xet_chunk = xet_chunk_cache.join("chunk.bin");
        let xet_shard = xet_shard_cache.join("shard.mdb");
        let resumable_upload = xet_staging.join("upload.mdb");
        let project_model = project.join("model.bin");
        let local_media_cache = adobe_local.join("local-cache.bin");
        let roaming_media_cache = adobe_roaming.join("roaming-cache.bin");
        for fixture in [
            &stale_partial,
            &recent_partial,
            &completed_download,
            &downloaded_model,
            &xet_chunk,
            &xet_shard,
            &resumable_upload,
            &project_model,
            &local_media_cache,
            &roaming_media_cache,
        ] {
            fs::write(fixture, b"MangoDisk round 04 fixture")
                .expect("should write isolated cleanup fixture");
        }
        let stale_time = SystemTime::now()
            .checked_sub(Duration::from_secs(8 * 86_400))
            .expect("test time should move back by eight days");
        fs::File::options()
            .write(true)
            .open(&stale_partial)
            .expect("should open stale download fixture")
            .set_times(fs::FileTimes::new().set_modified(stale_time))
            .expect("should set stale download modification time");

        let _restore = EnvironmentRestore(vec![
            ("USERPROFILE", std::env::var_os("USERPROFILE")),
            ("LOCALAPPDATA", std::env::var_os("LOCALAPPDATA")),
            ("APPDATA", std::env::var_os("APPDATA")),
        ]);
        std::env::set_var("USERPROFILE", &profile);
        std::env::set_var("LOCALAPPDATA", &local);
        std::env::set_var("APPDATA", &roaming);

        assert!(
            validate_rule_root(&downloads, &MatcherSpec::All).is_err(),
            "Downloads must never be authorized for full-root cleanup"
        );

        let retired_rule = CleanupService::execute(CleanupRequest {
            rule_ids: vec![
                "system.stale-partial-downloads".to_string(),
                "ai.model-cache".to_string(),
            ],
            source_selections: Vec::new(),
            dry_run: false,
            project_roots: Vec::new(),
        });
        assert!(retired_rule.is_err());
        assert!(stale_partial.exists());
        assert!(
            CleanupService::execute(CleanupRequest {
                rule_ids: vec!["ai.gemini-temp-files".to_string()],
                source_selections: Vec::new(),
                dry_run: false,
                project_roots: Vec::new(),
            })
            .is_err(),
            "retired Gemini session cleanup rule must remain unavailable"
        );

        let result = CleanupService::execute(CleanupRequest {
            rule_ids: vec![
                "system.stale-partial-downloads".to_string(),
                "app.adobe-media-cache".to_string(),
                "ai.huggingface-xet-cache".to_string(),
            ],
            source_selections: Vec::new(),
            dry_run: false,
            project_roots: Vec::new(),
        })
        .expect("isolated AI cache cleanup should succeed");

        assert_eq!(
            result.failed_item_count, 0,
            "isolated cleanup should not fail: {:?}",
            result.actions
        );
        assert_eq!(result.affected_item_count, 5);
        assert!(!stale_partial.exists());
        assert!(!xet_chunk.exists());
        assert!(!xet_shard.exists());
        assert!(!local_media_cache.exists());
        assert!(!roaming_media_cache.exists());
        assert!(downloaded_model.exists());
        assert!(resumable_upload.exists());
        assert!(recent_partial.exists());
        assert!(completed_download.exists());
        assert!(project_model.exists());
    }
}

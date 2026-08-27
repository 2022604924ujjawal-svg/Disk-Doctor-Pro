use std::collections::HashMap;

use super::models::{
    ApplicationUninstallActionReason, ApplicationUninstallActionResult,
    ApplicationUninstallActionStatus, ApplicationUninstallInspection, ApplicationUninstallPlan,
    ApplicationUninstallResult,
};

pub(super) fn compare(
    plan: &ApplicationUninstallPlan,
    inspection: &ApplicationUninstallInspection,
) -> ApplicationUninstallResult {
    let current = inspection
        .components
        .iter()
        .map(|component| (&component.component_id, component))
        .collect::<HashMap<_, _>>();
    let mut actions = Vec::with_capacity(plan.items.len());
    let mut previewed_bytes = 0_u64;
    let mut previewed_item_count = 0_u64;
    let mut failed_item_count = 0_u64;

    for item in &plan.items {
        let Some(component) = current.get(&item.component_id) else {
            failed_item_count += 1;
            actions.push(failed_action(
                item,
                ApplicationUninstallActionReason::ComponentUnavailable,
            ));
            continue;
        };
        if component.kind != item.kind
            || component.bytes != item.expected_bytes
            || component.file_count != item.expected_file_count
            || component.snapshot_fingerprint != item.expected_snapshot_fingerprint
        {
            failed_item_count += 1;
            actions.push(failed_action(
                item,
                ApplicationUninstallActionReason::ComponentChanged,
            ));
            continue;
        }
        previewed_bytes = previewed_bytes.saturating_add(item.expected_bytes);
        previewed_item_count += 1;
        actions.push(ApplicationUninstallActionResult {
            component_id: item.component_id.clone(),
            kind: item.kind,
            status: ApplicationUninstallActionStatus::Previewed,
            reason: None,
            expected_bytes: item.expected_bytes,
            released_bytes: 0,
        });
    }

    ApplicationUninstallResult {
        plan_id: plan.plan_id.clone(),
        application_id: plan.application_id.clone(),
        application_name: Some(inspection.application_name.clone()),
        expected_bytes: plan.expected_bytes,
        previewed_bytes,
        released_bytes: 0,
        previewed_item_count,
        affected_item_count: 0,
        failed_item_count,
        released_bytes_is_estimate: false,
        restart_required: false,
        dry_run: true,
        actions,
        history_saved: false,
    }
}

pub(super) fn fail_all(
    plan: &ApplicationUninstallPlan,
    application_name: Option<String>,
    reason: ApplicationUninstallActionReason,
) -> ApplicationUninstallResult {
    ApplicationUninstallResult {
        plan_id: plan.plan_id.clone(),
        application_id: plan.application_id.clone(),
        application_name,
        expected_bytes: plan.expected_bytes,
        previewed_bytes: 0,
        released_bytes: 0,
        previewed_item_count: 0,
        affected_item_count: 0,
        failed_item_count: plan.items.len() as u64,
        released_bytes_is_estimate: false,
        restart_required: false,
        dry_run: true,
        actions: plan
            .items
            .iter()
            .map(|item| failed_action(item, reason))
            .collect(),
        history_saved: false,
    }
}

/// Builds a terminal result for an application that was never started.
///
/// Cancellation remains distinct from a failed preflight so adapters and
/// history can explain that the user stopped the batch intentionally. The
/// prepared evidence supplies the display name without another filesystem or
/// registry read after cancellation has been requested.
pub(super) fn cancel_all(
    plan: &ApplicationUninstallPlan,
    application_name: Option<String>,
    reason: Option<ApplicationUninstallActionReason>,
) -> ApplicationUninstallResult {
    ApplicationUninstallResult {
        plan_id: plan.plan_id.clone(),
        application_id: plan.application_id.clone(),
        application_name,
        expected_bytes: plan.expected_bytes,
        previewed_bytes: 0,
        released_bytes: 0,
        previewed_item_count: 0,
        affected_item_count: 0,
        failed_item_count: 0,
        released_bytes_is_estimate: false,
        restart_required: false,
        dry_run: false,
        actions: plan
            .items
            .iter()
            .map(|item| ApplicationUninstallActionResult {
                component_id: item.component_id.clone(),
                kind: item.kind,
                status: ApplicationUninstallActionStatus::Cancelled,
                reason,
                expected_bytes: item.expected_bytes,
                released_bytes: 0,
            })
            .collect(),
        history_saved: false,
    }
}

fn failed_action(
    item: &super::models::ApplicationUninstallPlanItem,
    reason: ApplicationUninstallActionReason,
) -> ApplicationUninstallActionResult {
    ApplicationUninstallActionResult {
        component_id: item.component_id.clone(),
        kind: item.kind,
        status: ApplicationUninstallActionStatus::Failed,
        reason: Some(reason),
        expected_bytes: item.expected_bytes,
        released_bytes: 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::applications::uninstall::models::{
        ApplicationUninstallCapability, ApplicationUninstallComponent,
        ApplicationUninstallComponentKind, ApplicationUninstallPlanItem, ApplicationUninstallRisk,
        APPLICATION_UNINSTALL_INSPECTION_SCHEMA_VERSION, APPLICATION_UNINSTALL_PLAN_SCHEMA_VERSION,
    };

    fn plan() -> ApplicationUninstallPlan {
        ApplicationUninstallPlan {
            schema_version: APPLICATION_UNINSTALL_PLAN_SCHEMA_VERSION,
            plan_id: "plan-1".to_string(),
            plan_hash: "hash".to_string(),
            created_at_ms: 1,
            application_id: "application-1".to_string(),
            catalog_revision: "revision-1".to_string(),
            items: vec![ApplicationUninstallPlanItem {
                component_id: "component-binary".to_string(),
                kind: ApplicationUninstallComponentKind::ApplicationBinary,
                expected_bytes: 10,
                expected_file_count: 1,
                expected_snapshot_fingerprint: "a".repeat(64),
            }],
            expected_bytes: 10,
        }
    }

    fn inspection() -> ApplicationUninstallInspection {
        ApplicationUninstallInspection {
            schema_version: APPLICATION_UNINSTALL_INSPECTION_SCHEMA_VERSION,
            inspected_at_ms: 1,
            application_id: "application-1".to_string(),
            application_name: "Example".to_string(),
            primary_identifier: "com.example.app".to_string(),
            platform:
                crate::applications::uninstall::models::ApplicationUninstallPlatform::MacosBundle,
            installer_kind: None,
            capability: ApplicationUninstallCapability::Ready,
            catalog_revision: "revision-1".to_string(),
            components: vec![ApplicationUninstallComponent {
                component_id: "component-binary".to_string(),
                kind: ApplicationUninstallComponentKind::ApplicationBinary,
                risk: ApplicationUninstallRisk::Required,
                path: Some("/Applications/Example.app".to_string()),
                bytes: 10,
                file_count: 1,
                default_selected: true,
                snapshot_fingerprint: "a".repeat(64),
            }],
            total_bytes: 10,
            default_selected_bytes: 10,
            elapsed_ms: 1,
            #[cfg(windows)]
            uninstall_registration: None,
        }
    }

    #[test]
    fn unchanged_components_are_previewed() {
        let result = compare(&plan(), &inspection());

        assert_eq!(result.previewed_item_count, 1);
        assert_eq!(result.previewed_bytes, 10);
        assert_eq!(result.failed_item_count, 0);
    }

    #[test]
    fn unrelated_catalog_revision_change_keeps_matching_component_valid() {
        let mut inspection = inspection();
        inspection.catalog_revision = "revision-2".to_string();

        let result = compare(&plan(), &inspection);

        assert_eq!(result.previewed_item_count, 1);
        assert_eq!(result.failed_item_count, 0);
    }

    #[test]
    fn changed_components_fail_preflight() {
        let mut inspection = inspection();
        inspection.components[0].snapshot_fingerprint = "b".repeat(64);

        let result = compare(&plan(), &inspection);

        assert_eq!(result.previewed_item_count, 0);
        assert_eq!(result.failed_item_count, 1);
        assert_eq!(
            result.actions[0].reason,
            Some(ApplicationUninstallActionReason::ComponentChanged)
        );
    }

    #[test]
    fn cancellation_keeps_unstarted_items_out_of_failure_counts() {
        let result = cancel_all(&plan(), Some("Example".to_string()), None);

        assert_eq!(result.affected_item_count, 0);
        assert_eq!(result.failed_item_count, 0);
        assert_eq!(
            result.actions[0].status,
            ApplicationUninstallActionStatus::Cancelled
        );
    }
}

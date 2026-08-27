use std::collections::HashMap;

use super::models::{
    ApplicationUninstallActionReason, ApplicationUninstallActionResult,
    ApplicationUninstallActionStatus, ApplicationUninstallComponent,
    ApplicationUninstallComponentKind, ApplicationUninstallInspection, ApplicationUninstallPlan,
    ApplicationUninstallResult,
};

pub(super) struct DeleteFailure {
    reason: Option<ApplicationUninstallActionReason>,
    released_bytes: u64,
    cancelled: bool,
}

impl DeleteFailure {
    pub(super) fn new(reason: ApplicationUninstallActionReason, released_bytes: u64) -> Self {
        Self {
            reason: Some(reason),
            released_bytes,
            cancelled: false,
        }
    }

    pub(super) fn cancelled() -> Self {
        Self {
            reason: None,
            released_bytes: 0,
            cancelled: true,
        }
    }
}

impl From<ApplicationUninstallActionReason> for DeleteFailure {
    fn from(reason: ApplicationUninstallActionReason) -> Self {
        Self::new(reason, 0)
    }
}

pub(super) fn execute_with<Validate, Delete, Verify>(
    plan: &ApplicationUninstallPlan,
    inspection: &ApplicationUninstallInspection,
    mut validate: Validate,
    mut delete_permanently: Delete,
    mut verify_absent: Verify,
) -> ApplicationUninstallResult
where
    Validate: FnMut(&ApplicationUninstallComponent) -> bool,
    Delete: FnMut(&ApplicationUninstallComponent) -> Result<(), DeleteFailure>,
    Verify: FnMut(&ApplicationUninstallComponent) -> bool,
{
    let ordered = ordered_components(plan, inspection);
    if let Err(reason) = validate_all(&ordered, &mut validate) {
        return failed_result(plan, inspection, reason);
    }

    let mut actions = Vec::with_capacity(ordered.len());
    let mut released_bytes = 0_u64;
    let mut affected_item_count = 0_u64;
    let mut failure = None;
    let mut cancelled = false;
    for component in ordered {
        if cancelled {
            actions.push(action(
                component,
                ApplicationUninstallActionStatus::Cancelled,
                None,
                0,
            ));
            continue;
        }
        if failure.is_some() {
            actions.push(action(
                component,
                ApplicationUninstallActionStatus::Failed,
                Some(ApplicationUninstallActionReason::ExecutionAborted),
                0,
            ));
            continue;
        }
        // Components may change while earlier items are being deleted. Validate
        // again immediately before each irreversible boundary instead of
        // relying only on the batch validation performed before the loop.
        if !validate(component) {
            failure = Some(ApplicationUninstallActionReason::ComponentChanged);
            actions.push(action(
                component,
                ApplicationUninstallActionStatus::Failed,
                failure,
                0,
            ));
            continue;
        }
        match delete_permanently(component) {
            Ok(()) if verify_absent(component) => {
                released_bytes = released_bytes.saturating_add(component.bytes);
                affected_item_count += 1;
                actions.push(action(
                    component,
                    ApplicationUninstallActionStatus::Completed,
                    None,
                    component.bytes,
                ));
            }
            Ok(()) => {
                failure = Some(ApplicationUninstallActionReason::VerificationFailed);
                actions.push(action(
                    component,
                    ApplicationUninstallActionStatus::Failed,
                    failure,
                    0,
                ));
            }
            Err(delete_failure) => {
                if delete_failure.cancelled {
                    cancelled = true;
                    actions.push(action(
                        component,
                        ApplicationUninstallActionStatus::Cancelled,
                        None,
                        0,
                    ));
                    continue;
                }
                released_bytes = released_bytes.saturating_add(delete_failure.released_bytes);
                failure = delete_failure.reason;
                actions.push(action(
                    component,
                    ApplicationUninstallActionStatus::Failed,
                    failure,
                    delete_failure.released_bytes,
                ));
            }
        }
    }
    let failed_item_count = actions
        .iter()
        .filter(|action| action.status == ApplicationUninstallActionStatus::Failed)
        .count() as u64;
    ApplicationUninstallResult {
        plan_id: plan.plan_id.clone(),
        application_id: plan.application_id.clone(),
        application_name: Some(inspection.application_name.clone()),
        expected_bytes: plan.expected_bytes,
        previewed_bytes: 0,
        released_bytes,
        previewed_item_count: 0,
        affected_item_count,
        failed_item_count,
        released_bytes_is_estimate: false,
        restart_required: false,
        dry_run: false,
        actions,
        history_saved: false,
    }
}

fn ordered_components<'a>(
    plan: &ApplicationUninstallPlan,
    inspection: &'a ApplicationUninstallInspection,
) -> Vec<&'a ApplicationUninstallComponent> {
    let selected = plan
        .items
        .iter()
        .map(|item| (&item.component_id, item.kind))
        .collect::<HashMap<_, _>>();
    let mut components = inspection
        .components
        .iter()
        .filter(|component| selected.contains_key(&component.component_id))
        .collect::<Vec<_>>();
    // Deleting the application first leaves only harmless residual data if a
    // later association fails. Deleting user data first could leave an installed
    // application without its settings or documents after a partial failure.
    components.sort_by_key(|component| {
        (
            component.kind != ApplicationUninstallComponentKind::ApplicationBinary,
            component.kind.stable_code(),
            component.component_id.as_str(),
        )
    });
    components
}

fn validate_all<Validate>(
    components: &[&ApplicationUninstallComponent],
    validate: &mut Validate,
) -> Result<(), ApplicationUninstallActionReason>
where
    Validate: FnMut(&ApplicationUninstallComponent) -> bool,
{
    if components.iter().all(|component| validate(component)) {
        Ok(())
    } else {
        Err(ApplicationUninstallActionReason::ComponentChanged)
    }
}

fn failed_result(
    plan: &ApplicationUninstallPlan,
    inspection: &ApplicationUninstallInspection,
    reason: ApplicationUninstallActionReason,
) -> ApplicationUninstallResult {
    ApplicationUninstallResult {
        plan_id: plan.plan_id.clone(),
        application_id: plan.application_id.clone(),
        application_name: Some(inspection.application_name.clone()),
        expected_bytes: plan.expected_bytes,
        previewed_bytes: 0,
        released_bytes: 0,
        previewed_item_count: 0,
        affected_item_count: 0,
        failed_item_count: plan.items.len() as u64,
        released_bytes_is_estimate: false,
        restart_required: false,
        dry_run: false,
        actions: inspection
            .components
            .iter()
            .filter(|component| {
                plan.items
                    .iter()
                    .any(|item| item.component_id == component.component_id)
            })
            .map(|component| {
                action(
                    component,
                    ApplicationUninstallActionStatus::Failed,
                    Some(reason),
                    0,
                )
            })
            .collect(),
        history_saved: false,
    }
}

fn action(
    component: &ApplicationUninstallComponent,
    status: ApplicationUninstallActionStatus,
    reason: Option<ApplicationUninstallActionReason>,
    released_bytes: u64,
) -> ApplicationUninstallActionResult {
    ApplicationUninstallActionResult {
        component_id: component.component_id.clone(),
        kind: component.kind,
        status,
        reason,
        expected_bytes: component.bytes,
        released_bytes,
    }
}

#[cfg(test)]
mod tests {
    #[cfg(target_os = "macos")]
    use std::{
        fs,
        path::{Path, PathBuf},
        sync::atomic::{AtomicU64, Ordering},
    };

    use super::*;
    use crate::applications::uninstall::models::{
        ApplicationUninstallCapability, ApplicationUninstallPlanItem, ApplicationUninstallRisk,
        APPLICATION_UNINSTALL_INSPECTION_SCHEMA_VERSION, APPLICATION_UNINSTALL_PLAN_SCHEMA_VERSION,
    };

    fn component(
        id: &str,
        kind: ApplicationUninstallComponentKind,
    ) -> ApplicationUninstallComponent {
        ApplicationUninstallComponent {
            component_id: id.to_string(),
            kind,
            risk: ApplicationUninstallRisk::Rebuildable,
            path: Some(format!("/{id}")),
            bytes: 10,
            file_count: 1,
            default_selected: true,
            snapshot_fingerprint: "a".repeat(64),
        }
    }

    fn fixture() -> (ApplicationUninstallPlan, ApplicationUninstallInspection) {
        let binary = component(
            "component-binary",
            ApplicationUninstallComponentKind::ApplicationBinary,
        );
        let cache = component("component-cache", ApplicationUninstallComponentKind::Cache);
        let items = [&cache, &binary]
            .into_iter()
            .map(|component| ApplicationUninstallPlanItem {
                component_id: component.component_id.clone(),
                kind: component.kind,
                expected_bytes: component.bytes,
                expected_file_count: component.file_count,
                expected_snapshot_fingerprint: component.snapshot_fingerprint.clone(),
            })
            .collect::<Vec<_>>();
        (
            ApplicationUninstallPlan {
                schema_version: APPLICATION_UNINSTALL_PLAN_SCHEMA_VERSION,
                plan_id: "plan-1".to_string(),
                plan_hash: "hash".to_string(),
                created_at_ms: 1,
                application_id: "application-1".to_string(),
                catalog_revision: "revision-1".to_string(),
                expected_bytes: 20,
                items,
            },
            ApplicationUninstallInspection {
                schema_version: APPLICATION_UNINSTALL_INSPECTION_SCHEMA_VERSION,
                inspected_at_ms: 1,
                application_id: "application-1".to_string(),
                application_name: "Example".to_string(),
                primary_identifier: "com.example.app".to_string(),
                platform: crate::applications::uninstall::models::ApplicationUninstallPlatform::MacosBundle,
                installer_kind: None,
                capability: ApplicationUninstallCapability::Ready,
                catalog_revision: "revision-1".to_string(),
                components: vec![cache, binary],
                total_bytes: 20,
                default_selected_bytes: 20,
                elapsed_ms: 1,
                #[cfg(windows)]
                uninstall_registration: None,
            },
        )
    }

    #[test]
    fn application_binary_deletes_before_associations() {
        let (plan, inspection) = fixture();
        let mut order = Vec::new();

        let result = execute_with(
            &plan,
            &inspection,
            |_| true,
            |component| {
                order.push(component.kind);
                Ok(())
            },
            |_| true,
        );

        assert_eq!(
            order,
            vec![
                ApplicationUninstallComponentKind::ApplicationBinary,
                ApplicationUninstallComponentKind::Cache
            ]
        );
        assert_eq!(result.affected_item_count, 2);
        assert_eq!(result.failed_item_count, 0);
    }

    #[test]
    fn failed_delete_aborts_remaining_components() {
        let (plan, inspection) = fixture();

        let result = execute_with(
            &plan,
            &inspection,
            |_| true,
            |_| {
                Err(DeleteFailure::new(
                    ApplicationUninstallActionReason::PermanentDeleteFailed,
                    0,
                ))
            },
            |_| true,
        );

        assert_eq!(result.affected_item_count, 0);
        assert_eq!(result.failed_item_count, 2);
        assert_eq!(
            result.actions[1].reason,
            Some(ApplicationUninstallActionReason::ExecutionAborted)
        );
    }

    #[test]
    fn partial_delete_reports_released_bytes_before_aborting() {
        let (plan, inspection) = fixture();

        let result = execute_with(
            &plan,
            &inspection,
            |_| true,
            |_| {
                Err(DeleteFailure::new(
                    ApplicationUninstallActionReason::PermanentDeleteFailed,
                    5,
                ))
            },
            |_| false,
        );

        assert_eq!(result.released_bytes, 5);
        assert_eq!(result.actions[0].released_bytes, 5);
        assert_eq!(result.failed_item_count, 2);
    }

    #[test]
    fn cancelled_delete_marks_current_and_remaining_components_as_cancelled() {
        let (plan, inspection) = fixture();

        let result = execute_with(
            &plan,
            &inspection,
            |_| true,
            |_| Err(DeleteFailure::cancelled()),
            |_| false,
        );

        assert_eq!(result.affected_item_count, 0);
        assert_eq!(result.failed_item_count, 0);
        assert!(result
            .actions
            .iter()
            .all(|action| action.status == ApplicationUninstallActionStatus::Cancelled));
    }

    #[test]
    fn second_validation_fails_before_any_delete() {
        let (plan, inspection) = fixture();
        let mut delete_count = 0;

        let result = execute_with(
            &plan,
            &inspection,
            |component| component.kind != ApplicationUninstallComponentKind::Cache,
            |_| {
                delete_count += 1;
                Ok(())
            },
            |_| true,
        );

        assert_eq!(delete_count, 0);
        assert_eq!(result.failed_item_count, 2);
    }

    #[test]
    fn component_change_between_deletes_aborts_remaining_work() {
        let (plan, inspection) = fixture();
        let mut validation_count = 0;
        let mut delete_count = 0;

        let result = execute_with(
            &plan,
            &inspection,
            |_| {
                validation_count += 1;
                // The first two calls are the batch validation. The application
                // still matches immediately before its deletion, but the cache
                // changes before its just-in-time validation.
                validation_count != 4
            },
            |_| {
                delete_count += 1;
                Ok(())
            },
            |_| true,
        );

        assert_eq!(delete_count, 1);
        assert_eq!(result.affected_item_count, 1);
        assert_eq!(result.failed_item_count, 1);
        assert_eq!(
            result.actions[1].reason,
            Some(ApplicationUninstallActionReason::ComponentChanged)
        );
    }

    #[cfg(target_os = "macos")]
    static NEXT_DELETE_FIXTURE: AtomicU64 = AtomicU64::new(1);

    #[cfg(target_os = "macos")]
    struct DeleteFixture {
        root: PathBuf,
        prefix: String,
    }

    #[cfg(target_os = "macos")]
    impl DeleteFixture {
        fn new() -> Self {
            let prefix = format!(
                "mangodisk-uninstall-delete-{}-{}",
                std::process::id(),
                NEXT_DELETE_FIXTURE.fetch_add(1, Ordering::Relaxed)
            );
            let root = std::env::temp_dir().join(&prefix);
            fs::create_dir_all(&root).expect("create the owned deletion fixture");
            Self { root, prefix }
        }

        fn directory(&self, suffix: &str) -> PathBuf {
            let path = self.root.join(format!("{}-{suffix}", self.prefix));
            fs::create_dir_all(&path).expect("create an owned component directory");
            fs::write(path.join("payload.bin"), b"fixture")
                .expect("write an owned component fixture");
            path
        }
    }

    #[cfg(target_os = "macos")]
    impl Drop for DeleteFixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.root);
        }
    }

    #[cfg(target_os = "macos")]
    fn delete_component(
        id: &str,
        kind: ApplicationUninstallComponentKind,
        path: &Path,
    ) -> ApplicationUninstallComponent {
        ApplicationUninstallComponent {
            component_id: id.to_string(),
            kind,
            risk: ApplicationUninstallRisk::Rebuildable,
            path: Some(path.to_string_lossy().into_owned()),
            bytes: 7,
            file_count: 1,
            default_selected: true,
            snapshot_fingerprint: "a".repeat(64),
        }
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn permanent_delete_removes_every_selected_component() {
        let fixture = DeleteFixture::new();
        let binary_path = fixture.directory("Example.app");
        let cache_path = fixture.directory("cache");
        let binary = delete_component(
            "component-binary",
            ApplicationUninstallComponentKind::ApplicationBinary,
            &binary_path,
        );
        let cache = delete_component(
            "component-cache",
            ApplicationUninstallComponentKind::Cache,
            &cache_path,
        );
        let inspection = ApplicationUninstallInspection {
            schema_version: APPLICATION_UNINSTALL_INSPECTION_SCHEMA_VERSION,
            inspected_at_ms: 1,
            application_id: "application-delete-fixture".to_string(),
            application_name: "Delete Fixture".to_string(),
            primary_identifier: "com.example.delete-fixture".to_string(),
            platform:
                crate::applications::uninstall::models::ApplicationUninstallPlatform::MacosBundle,
            installer_kind: None,
            capability: ApplicationUninstallCapability::Ready,
            catalog_revision: "revision-1".to_string(),
            components: vec![cache.clone(), binary.clone()],
            total_bytes: 14,
            default_selected_bytes: 14,
            elapsed_ms: 1,
            #[cfg(windows)]
            uninstall_registration: None,
        };
        let plan = ApplicationUninstallPlan {
            schema_version: APPLICATION_UNINSTALL_PLAN_SCHEMA_VERSION,
            plan_id: "plan-delete-fixture".to_string(),
            plan_hash: "hash".to_string(),
            created_at_ms: 1,
            application_id: inspection.application_id.clone(),
            catalog_revision: inspection.catalog_revision.clone(),
            expected_bytes: 14,
            items: [&cache, &binary]
                .into_iter()
                .map(|component| ApplicationUninstallPlanItem {
                    component_id: component.component_id.clone(),
                    kind: component.kind,
                    expected_bytes: component.bytes,
                    expected_file_count: component.file_count,
                    expected_snapshot_fingerprint: component.snapshot_fingerprint.clone(),
                })
                .collect(),
        };

        let result = execute_with(
            &plan,
            &inspection,
            |_| true,
            |component| {
                let path = component.path.as_deref().expect("fixture path must exist");
                let prepared =
                    crate::filesystem::permanent_delete::prepare_path_for_permanent_delete(
                        Path::new(path),
                    )
                    .expect("fixture target must be prepared");
                crate::filesystem::permanent_delete::delete_path_permanently(
                    prepared,
                    component.bytes,
                    component.file_count,
                )
                .map_err(|error| {
                    DeleteFailure::new(
                        ApplicationUninstallActionReason::PermanentDeleteFailed,
                        error.released_bytes(),
                    )
                })
            },
            |component| {
                !Path::new(component.path.as_deref().expect("fixture path must exist")).exists()
            },
        );

        assert_eq!(result.affected_item_count, 2);
        assert_eq!(result.failed_item_count, 0);
        assert!(!binary_path.exists());
        assert!(!cache_path.exists());
    }
}

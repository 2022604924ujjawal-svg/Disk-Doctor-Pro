use std::collections::{HashMap, HashSet};

use crate::filesystem::metadata::now_ms;

use super::models::{
    ApplicationUninstallComponent, ApplicationUninstallComponentKind,
    ApplicationUninstallInspection, ApplicationUninstallPlan, ApplicationUninstallPlanItem,
    APPLICATION_UNINSTALL_INSPECTION_SCHEMA_VERSION, APPLICATION_UNINSTALL_PLAN_SCHEMA_VERSION,
};

const MAX_PLAN_ITEMS: usize = 32;
const MAX_APPLICATION_ID_BYTES: usize = 128;
const MAX_CATALOG_REVISION_BYTES: usize = 512;
const MAX_COMPONENT_ID_BYTES: usize = 128;

pub(super) fn create_plan(
    inspection: &ApplicationUninstallInspection,
    component_ids: &[String],
) -> Result<ApplicationUninstallPlan, String> {
    if inspection.schema_version != APPLICATION_UNINSTALL_INSPECTION_SCHEMA_VERSION {
        return Err(format!(
            "unsupported application uninstall inspection schema version: {}",
            inspection.schema_version
        ));
    }
    if !inspection.capability.supports_execution() {
        return Err("application is not ready for uninstall planning".to_string());
    }
    let components = inspection
        .components
        .iter()
        .map(|component| (&component.component_id, component))
        .collect::<HashMap<_, _>>();
    let mut items = Vec::with_capacity(component_ids.len());
    for component_id in unique_component_ids(component_ids)? {
        let component = components.get(component_id).ok_or_else(|| {
            format!("application uninstall component is unavailable: {component_id}")
        })?;
        items.push(plan_item(component));
    }
    create_reviewed_plan(
        inspection.application_id.clone(),
        inspection.catalog_revision.clone(),
        items,
    )
}

pub(super) fn create_reviewed_plan(
    application_id: String,
    catalog_revision: String,
    mut items: Vec<ApplicationUninstallPlanItem>,
) -> Result<ApplicationUninstallPlan, String> {
    if application_id.trim().is_empty() || application_id.len() > MAX_APPLICATION_ID_BYTES {
        return Err("application uninstall plan application identifier is invalid".to_string());
    }
    if catalog_revision.trim().is_empty() || catalog_revision.len() > MAX_CATALOG_REVISION_BYTES {
        return Err("application uninstall plan catalog revision is invalid".to_string());
    }
    validate_items(&items)?;
    items.sort_by(|left, right| left.component_id.cmp(&right.component_id));
    let expected_bytes = expected_bytes(&items);
    let created_at_ms = now_ms();
    let plan_hash = plan_hash(
        created_at_ms,
        &application_id,
        &catalog_revision,
        &items,
        expected_bytes,
    );
    Ok(ApplicationUninstallPlan {
        schema_version: APPLICATION_UNINSTALL_PLAN_SCHEMA_VERSION,
        plan_id: format!("application-uninstall-plan-{}", &plan_hash[..16]),
        plan_hash,
        created_at_ms,
        application_id,
        catalog_revision,
        items,
        expected_bytes,
    })
}

pub(super) fn validate_plan(plan: &ApplicationUninstallPlan) -> Result<(), String> {
    if plan.schema_version != APPLICATION_UNINSTALL_PLAN_SCHEMA_VERSION {
        return Err(format!(
            "unsupported application uninstall plan schema version: {}",
            plan.schema_version
        ));
    }
    if plan.application_id.trim().is_empty()
        || plan.application_id.len() > MAX_APPLICATION_ID_BYTES
        || plan.catalog_revision.trim().is_empty()
        || plan.catalog_revision.len() > MAX_CATALOG_REVISION_BYTES
    {
        return Err("application uninstall plan identity is incomplete".to_string());
    }
    validate_items(&plan.items)?;
    let expected_bytes = expected_bytes(&plan.items);
    let expected_hash = plan_hash(
        plan.created_at_ms,
        &plan.application_id,
        &plan.catalog_revision,
        &plan.items,
        expected_bytes,
    );
    if plan.expected_bytes != expected_bytes
        || plan.plan_hash != expected_hash
        || plan.plan_id != format!("application-uninstall-plan-{}", &expected_hash[..16])
    {
        return Err("application uninstall plan integrity validation failed".to_string());
    }
    Ok(())
}

fn unique_component_ids(component_ids: &[String]) -> Result<Vec<&String>, String> {
    if component_ids.is_empty() {
        return Err("application uninstall plan contains no components".to_string());
    }
    if component_ids.len() > MAX_PLAN_ITEMS {
        return Err("application uninstall plan contains too many components".to_string());
    }
    let unique = component_ids.iter().collect::<HashSet<_>>();
    if unique.len() != component_ids.len() {
        return Err("application uninstall plan contains duplicate components".to_string());
    }
    Ok(component_ids.iter().collect())
}

fn validate_items(items: &[ApplicationUninstallPlanItem]) -> Result<(), String> {
    if items.is_empty() {
        return Err("application uninstall plan contains no components".to_string());
    }
    if items.len() > MAX_PLAN_ITEMS {
        return Err("application uninstall plan contains too many components".to_string());
    }
    let unique = items
        .iter()
        .map(|item| &item.component_id)
        .collect::<HashSet<_>>();
    if unique.len() != items.len() {
        return Err("application uninstall plan contains duplicate components".to_string());
    }
    if items.iter().any(|item| {
        item.component_id.trim().is_empty()
            || item.component_id.len() > MAX_COMPONENT_ID_BYTES
            || !item.component_id.starts_with("component-")
            || !is_blake3_digest(&item.expected_snapshot_fingerprint)
    }) {
        return Err(
            "application uninstall plan contains incomplete component evidence".to_string(),
        );
    }
    let primary_component_count = items
        .iter()
        .filter(|item| {
            matches!(
                item.kind,
                ApplicationUninstallComponentKind::ApplicationBinary
                    | ApplicationUninstallComponentKind::NativeInstaller
            )
        })
        .count();
    if primary_component_count != 1 {
        return Err(
            "application uninstall plan must contain exactly one primary component".to_string(),
        );
    }
    Ok(())
}

fn plan_item(component: &ApplicationUninstallComponent) -> ApplicationUninstallPlanItem {
    ApplicationUninstallPlanItem {
        component_id: component.component_id.clone(),
        kind: component.kind,
        expected_bytes: component.bytes,
        expected_file_count: component.file_count,
        expected_snapshot_fingerprint: component.snapshot_fingerprint.clone(),
    }
}

fn expected_bytes(items: &[ApplicationUninstallPlanItem]) -> u64 {
    items.iter().fold(0_u64, |total, item| {
        total.saturating_add(item.expected_bytes)
    })
}

fn plan_hash(
    created_at_ms: u64,
    application_id: &str,
    catalog_revision: &str,
    items: &[ApplicationUninstallPlanItem],
    expected_bytes: u64,
) -> String {
    let mut canonical_items = items.to_vec();
    canonical_items.sort_by(|left, right| left.component_id.cmp(&right.component_id));
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"mangodisk-application-uninstall-plan-v1");
    hasher.update(&created_at_ms.to_le_bytes());
    hash_text(&mut hasher, application_id);
    hash_text(&mut hasher, catalog_revision);
    for item in &canonical_items {
        hash_text(&mut hasher, &item.component_id);
        hash_text(&mut hasher, item.kind.stable_code());
        hasher.update(&item.expected_bytes.to_le_bytes());
        hasher.update(&item.expected_file_count.to_le_bytes());
        hash_text(&mut hasher, &item.expected_snapshot_fingerprint);
    }
    hasher.update(&expected_bytes.to_le_bytes());
    hasher.finalize().to_hex().to_string()
}

fn hash_text(hasher: &mut blake3::Hasher, value: &str) {
    hasher.update(&(value.len() as u64).to_le_bytes());
    hasher.update(value.as_bytes());
}

fn is_blake3_digest(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::applications::uninstall::models::{
        ApplicationUninstallCapability, ApplicationUninstallComponent, ApplicationUninstallRisk,
    };

    fn inspection() -> ApplicationUninstallInspection {
        ApplicationUninstallInspection {
            schema_version: APPLICATION_UNINSTALL_INSPECTION_SCHEMA_VERSION,
            inspected_at_ms: 10,
            application_id: "application-1".to_string(),
            application_name: "Example".to_string(),
            primary_identifier: "com.example.app".to_string(),
            platform:
                crate::applications::uninstall::models::ApplicationUninstallPlatform::MacosBundle,
            installer_kind: None,
            capability: ApplicationUninstallCapability::Ready,
            catalog_revision: "revision-1".to_string(),
            components: vec![
                ApplicationUninstallComponent {
                    component_id: "component-binary".to_string(),
                    kind: ApplicationUninstallComponentKind::ApplicationBinary,
                    risk: ApplicationUninstallRisk::Required,
                    path: Some("/Applications/Example.app".to_string()),
                    bytes: 100,
                    file_count: 2,
                    default_selected: true,
                    snapshot_fingerprint: "a".repeat(64),
                },
                ApplicationUninstallComponent {
                    component_id: "component-cache".to_string(),
                    kind: ApplicationUninstallComponentKind::Cache,
                    risk: ApplicationUninstallRisk::Rebuildable,
                    path: Some("/Users/example/Library/Caches/com.example.app".to_string()),
                    bytes: 50,
                    file_count: 1,
                    default_selected: true,
                    snapshot_fingerprint: "b".repeat(64),
                },
            ],
            total_bytes: 150,
            default_selected_bytes: 150,
            elapsed_ms: 1,
            #[cfg(windows)]
            uninstall_registration: None,
        }
    }

    #[test]
    fn plan_requires_the_application_binary() {
        let error = create_plan(&inspection(), &["component-cache".to_string()])
            .expect_err("a plan without the application must fail");
        assert!(error.contains("primary component"));
    }

    #[test]
    fn plan_accepts_one_native_installer_as_the_primary_component() {
        let mut inspection = inspection();
        inspection.platform =
            crate::applications::uninstall::models::ApplicationUninstallPlatform::WindowsRegistry;
        inspection.installer_kind = Some(
            crate::applications::uninstall::models::ApplicationUninstallInstallerKind::WindowsMsi,
        );
        inspection.components = vec![ApplicationUninstallComponent {
            component_id: "component-native-installer".to_string(),
            kind: ApplicationUninstallComponentKind::NativeInstaller,
            risk: ApplicationUninstallRisk::Required,
            path: None,
            bytes: 1024,
            file_count: 1,
            default_selected: true,
            snapshot_fingerprint: "c".repeat(64),
        }];
        inspection.total_bytes = 1024;
        inspection.default_selected_bytes = 1024;

        let plan = create_plan(&inspection, &["component-native-installer".to_string()])
            .expect("one native installer must form a valid uninstall plan");

        assert_eq!(plan.items.len(), 1);
        assert_eq!(
            plan.items[0].kind,
            ApplicationUninstallComponentKind::NativeInstaller
        );
        validate_plan(&plan).expect("the native installer plan must remain valid");
    }

    #[test]
    fn plan_integrity_detects_modified_evidence() {
        let mut plan = create_plan(
            &inspection(),
            &[
                "component-binary".to_string(),
                "component-cache".to_string(),
            ],
        )
        .expect("fixture plan must be created");
        validate_plan(&plan).expect("fixture plan must be valid");

        plan.items[0].expected_bytes += 1;

        assert!(validate_plan(&plan).is_err());
    }

    #[test]
    fn ready_capability_is_required_for_planning() {
        let mut inspection = inspection();
        inspection.capability = ApplicationUninstallCapability::ApplicationRunning;

        assert!(create_plan(&inspection, &["component-binary".to_string()]).is_err());
    }

    #[test]
    fn plan_hash_is_independent_of_selection_order() {
        let left = create_plan(
            &inspection(),
            &[
                "component-binary".to_string(),
                "component-cache".to_string(),
            ],
        )
        .expect("left plan must be created");
        let right_items = left.items.iter().rev().cloned().collect::<Vec<_>>();
        let right = ApplicationUninstallPlan {
            items: right_items,
            ..left.clone()
        };

        validate_plan(&right).expect("canonical validation must accept reordered items");
    }
}

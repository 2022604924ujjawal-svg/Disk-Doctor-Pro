use std::collections::HashSet;

use crate::filesystem::metadata::now_ms;

use super::{
    models::{
        ApplicationUninstallBatchPlan, ApplicationUninstallPlan,
        APPLICATION_UNINSTALL_BATCH_PLAN_SCHEMA_VERSION,
    },
    plan,
};

const MAX_BATCH_APPLICATIONS: usize = 128;
const MAX_CATALOG_REVISION_BYTES: usize = 512;

pub(super) fn create_plan(
    catalog_revision: String,
    mut plans: Vec<ApplicationUninstallPlan>,
) -> Result<ApplicationUninstallBatchPlan, String> {
    validate_plan_collection(&catalog_revision, &plans)?;
    plans.sort_by(|left, right| left.application_id.cmp(&right.application_id));
    let expected_bytes = expected_bytes(&plans);
    let created_at_ms = now_ms();
    let batch_hash = batch_hash(created_at_ms, &catalog_revision, &plans, expected_bytes);
    Ok(ApplicationUninstallBatchPlan {
        schema_version: APPLICATION_UNINSTALL_BATCH_PLAN_SCHEMA_VERSION,
        batch_id: format!("application-uninstall-batch-{}", &batch_hash[..16]),
        batch_hash,
        created_at_ms,
        catalog_revision,
        plans,
        expected_bytes,
    })
}

pub(super) fn validate_plan(batch: &ApplicationUninstallBatchPlan) -> Result<(), String> {
    if batch.schema_version != APPLICATION_UNINSTALL_BATCH_PLAN_SCHEMA_VERSION {
        return Err(format!(
            "unsupported application uninstall batch plan schema version: {}",
            batch.schema_version
        ));
    }
    validate_plan_collection(&batch.catalog_revision, &batch.plans)?;
    let expected_bytes = expected_bytes(&batch.plans);
    let expected_hash = batch_hash(
        batch.created_at_ms,
        &batch.catalog_revision,
        &batch.plans,
        expected_bytes,
    );
    if batch.expected_bytes != expected_bytes
        || batch.batch_hash != expected_hash
        || batch.batch_id != format!("application-uninstall-batch-{}", &expected_hash[..16])
    {
        return Err("application uninstall batch plan integrity validation failed".to_string());
    }
    Ok(())
}

pub(super) fn validate_application_count(count: usize) -> Result<(), String> {
    if count == 0 {
        return Err("application uninstall batch contains no applications".to_string());
    }
    if count > MAX_BATCH_APPLICATIONS {
        return Err("application uninstall batch contains too many applications".to_string());
    }
    Ok(())
}

fn validate_plan_collection(
    catalog_revision: &str,
    plans: &[ApplicationUninstallPlan],
) -> Result<(), String> {
    if catalog_revision.trim().is_empty() || catalog_revision.len() > MAX_CATALOG_REVISION_BYTES {
        return Err("application uninstall batch catalog revision is invalid".to_string());
    }
    validate_application_count(plans.len())?;
    let unique = plans
        .iter()
        .map(|application_plan| &application_plan.application_id)
        .collect::<HashSet<_>>();
    if unique.len() != plans.len() {
        return Err("application uninstall batch contains duplicate applications".to_string());
    }
    for application_plan in plans {
        plan::validate_plan(application_plan)?;
        if application_plan.catalog_revision != catalog_revision {
            return Err(
                "application uninstall batch contains inconsistent catalog revisions".to_string(),
            );
        }
    }
    Ok(())
}

fn expected_bytes(plans: &[ApplicationUninstallPlan]) -> u64 {
    plans.iter().fold(0_u64, |total, plan| {
        total.saturating_add(plan.expected_bytes)
    })
}

fn batch_hash(
    created_at_ms: u64,
    catalog_revision: &str,
    plans: &[ApplicationUninstallPlan],
    expected_bytes: u64,
) -> String {
    let mut canonical_plans = plans.to_vec();
    canonical_plans.sort_by(|left, right| left.application_id.cmp(&right.application_id));
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"mangodisk-application-uninstall-batch-plan-v1");
    hasher.update(&created_at_ms.to_le_bytes());
    hash_text(&mut hasher, catalog_revision);
    for plan in &canonical_plans {
        hash_text(&mut hasher, &plan.application_id);
        hash_text(&mut hasher, &plan.plan_hash);
    }
    hasher.update(&expected_bytes.to_le_bytes());
    hasher.finalize().to_hex().to_string()
}

fn hash_text(hasher: &mut blake3::Hasher, value: &str) {
    hasher.update(&(value.len() as u64).to_le_bytes());
    hasher.update(value.as_bytes());
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::applications::uninstall::models::{
        ApplicationUninstallComponentKind, ApplicationUninstallPlanItem,
        APPLICATION_UNINSTALL_PLAN_SCHEMA_VERSION,
    };

    fn application_plan(application_id: &str, catalog_revision: &str) -> ApplicationUninstallPlan {
        plan::create_reviewed_plan(
            application_id.to_string(),
            catalog_revision.to_string(),
            vec![ApplicationUninstallPlanItem {
                component_id: format!("component-{application_id}"),
                kind: ApplicationUninstallComponentKind::ApplicationBinary,
                expected_bytes: 10,
                expected_file_count: 1,
                expected_snapshot_fingerprint: "a".repeat(64),
            }],
        )
        .expect("fixture plan should be valid")
    }

    #[test]
    fn batch_plan_is_canonical_and_valid() {
        let batch = create_plan(
            "revision-1".to_string(),
            vec![
                application_plan("application-b", "revision-1"),
                application_plan("application-a", "revision-1"),
            ],
        )
        .expect("batch should be valid");

        assert_eq!(
            batch.schema_version,
            APPLICATION_UNINSTALL_BATCH_PLAN_SCHEMA_VERSION
        );
        assert_eq!(batch.expected_bytes, 20);
        assert_eq!(batch.plans[0].application_id, "application-a");
        validate_plan(&batch).expect("created batch should validate");
    }

    #[test]
    fn batch_rejects_duplicate_applications() {
        let result = create_plan(
            "revision-1".to_string(),
            vec![
                application_plan("application-a", "revision-1"),
                application_plan("application-a", "revision-1"),
            ],
        );

        assert!(result.is_err());
    }

    #[test]
    fn batch_rejects_application_count_outside_limits() {
        assert!(validate_application_count(0).is_err());
        assert!(validate_application_count(MAX_BATCH_APPLICATIONS + 1).is_err());
    }

    #[test]
    fn batch_rejects_mixed_catalog_revisions() {
        let result = create_plan(
            "revision-1".to_string(),
            vec![
                application_plan("application-a", "revision-1"),
                application_plan("application-b", "revision-2"),
            ],
        );

        assert!(result.is_err());
    }

    #[test]
    fn batch_rejects_tampering() {
        let mut batch = create_plan(
            "revision-1".to_string(),
            vec![application_plan("application-a", "revision-1")],
        )
        .expect("batch should be valid");
        batch.expected_bytes += 1;

        assert!(validate_plan(&batch).is_err());
    }

    #[test]
    fn plan_schema_fixture_is_current() {
        assert_eq!(
            application_plan("application-a", "revision-1").schema_version,
            APPLICATION_UNINSTALL_PLAN_SCHEMA_VERSION
        );
    }
}

mod declarative_catalog;
mod declarative_schema;
mod matcher;
mod models;
pub(crate) mod protected_paths;
pub(crate) mod root_validation;
mod scan_plan;
mod validation;

pub(crate) use matcher::{matches_rule, maximum_match_depth};
pub(crate) use models::{ApplicabilityProbe, CompiledRule, MatcherSpec, RuleRiskLevel};
pub(crate) use scan_plan::{compile_scan_plan, RootScanTask, ScanPlan};

use std::collections::BTreeMap;

/// Catalog diagnostics contain metadata only, never user paths or scan results.
pub(crate) struct RuleCatalogDiagnostics {
    pub total_count: usize,
    pub lifecycle_counts: BTreeMap<&'static str, usize>,
    pub metadata_digest: String,
}

/// The only registry entry point used by scanning and cleanup services.
pub(crate) fn registry() -> Result<Vec<CompiledRule>, String> {
    // Declarative sources are cached after validation, but dynamic profile and
    // IDE roots are compiled for each operation. A long-running application
    // therefore observes roots created after startup without retaining stale
    // filesystem paths in a process-wide registry.
    compile_registry()
}

fn compile_registry() -> Result<Vec<CompiledRule>, String> {
    let specs = declarative_catalog::load_current_platform()?;
    let rules = validation::compile_rules(specs)?;
    let metadata_digest = catalog_metadata_digest(&rules);
    log::debug!(
        "cleanup_rule_catalog_compiled rule_count={} schema_version={} metadata_digest={}",
        rules.len(),
        models::RULE_SCHEMA_VERSION,
        metadata_digest
    );
    Ok(rules)
}

pub(crate) fn catalog_diagnostics() -> Result<RuleCatalogDiagnostics, String> {
    let rules = registry()?;
    let mut lifecycle_counts = BTreeMap::new();
    for rule in &rules {
        *lifecycle_counts
            .entry(rule.verification.lifecycle.as_str())
            .or_insert(0) += 1;
    }
    Ok(RuleCatalogDiagnostics {
        total_count: rules.len(),
        lifecycle_counts,
        metadata_digest: catalog_metadata_digest(&rules),
    })
}

/// Digest of scan boundaries and execution risk used for baseline comparability.
pub(crate) fn compatibility_digest() -> Result<String, String> {
    let mut rules = registry()?;
    rules.sort_by(|left, right| left.id.cmp(&right.id));
    let mut hasher = blake3::Hasher::new();
    for rule in rules {
        hasher.update(rule.id.as_bytes());
        hasher.update(rule.category.as_str().as_bytes());
        hasher.update(format!("{:?}", rule.risk).as_bytes());
        hasher.update(&[
            u8::from(rule.default_selected),
            u8::from(rule.recommended_selected),
            u8::from(rule.requires_app_close()),
            u8::from(rule.deletes_whole_root()),
        ]);
        hasher.update(format!("{:?}", rule.matcher).as_bytes());
        hasher.update(format!("{:?}", rule.applicability).as_bytes());
        for process_name in &rule.required_stopped_processes {
            hasher.update(process_name.as_bytes());
            hasher.update(&[0]);
        }
        let mut roots = rule.roots;
        roots.sort();
        for root in roots {
            hasher.update(root.to_string_lossy().as_bytes());
            hasher.update(&[0]);
        }
    }
    Ok(hasher.finalize().to_hex().to_string())
}

/// Metadata digest independent from filesystem scan results.
fn catalog_metadata_digest(rules: &[CompiledRule]) -> String {
    let mut hasher = blake3::Hasher::new();
    for rule in rules {
        hasher.update(rule.id.as_bytes());
        hasher.update(&rule.schema_version.to_le_bytes());
        hasher.update(&rule.rule_version.to_le_bytes());
        hasher.update(rule.platform.as_str().as_bytes());
        hasher.update(rule.category.as_str().as_bytes());
        hasher.update(rule.verification.lifecycle.as_str().as_bytes());
        hasher.update(rule.verification.evidence.as_bytes());
        hasher.update(rule.verification.verified_at.as_bytes());
        hasher.update(rule.verification.verified_platform.as_str().as_bytes());
    }
    hasher.finalize().to_hex().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn current_platform_catalog_compiles_from_declarative_sources() {
        let rules = registry().expect("the current platform catalog must compile");

        assert!(!rules.is_empty(), "the current platform must provide rules");
        assert!(rules
            .iter()
            .all(|rule| { !rule.default_selected || matches!(rule.risk, RuleRiskLevel::Safe) }));
        assert!(rules
            .iter()
            .all(|rule| !rule.verification.evidence.is_empty()));
    }

    #[test]
    fn current_platform_application_cache_rules_are_recommended() {
        let rules = registry().expect("the current platform catalog must compile");
        let application_rules = rules
            .iter()
            .filter(|rule| rule.category.as_str() == "application")
            .collect::<Vec<_>>();

        assert!(
            !application_rules.is_empty(),
            "the current platform must provide application cache rules"
        );
        assert!(
            application_rules
                .iter()
                .all(|rule| rule.recommended_selected),
            "every application cache rule must participate in the shared recommendation"
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_catalog_never_owns_the_entire_user_cache_directory() {
        let cache_root =
            std::path::PathBuf::from(std::env::var_os("HOME").unwrap()).join("Library/Caches");
        let rules = registry().expect("the macOS catalog must compile");
        assert!(rules
            .iter()
            .flat_map(|rule| &rule.roots)
            .all(|root| root != &cache_root));
    }
}

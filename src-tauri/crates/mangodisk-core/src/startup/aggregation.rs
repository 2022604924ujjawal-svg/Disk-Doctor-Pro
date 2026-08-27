use std::collections::BTreeMap;

use mangodisk_platform::{
    PlatformStartupArtifact, PlatformStartupConfiguredState, PlatformStartupControlCapability,
    PlatformStartupCoverageReason, PlatformStartupCoverageStatus, PlatformStartupDiagnosticCode,
    PlatformStartupIdentityConfidence, PlatformStartupRuntimeState, PlatformStartupScope,
    PlatformStartupSourceKind, PlatformStartupSourceResult, PlatformStartupSummarySource,
    PlatformStartupTargetKind, PlatformStartupTrigger, PlatformStartupTrustState,
};

use crate::filesystem::metadata::display_path;

use super::models::{
    StartupAggregateConfiguredState, StartupAggregateControlState, StartupArtifact,
    StartupCatalogSummary, StartupConfiguredState, StartupControlCapability, StartupCoverageReason,
    StartupCoverageStatus, StartupDiagnosticCode, StartupIdentityConfidence, StartupOwnerGroup,
    StartupRuntimeState, StartupScope, StartupSourceCoverage, StartupSourceKind,
    StartupSummarySource, StartupTarget, StartupTargetKind, StartupTrigger, StartupTrustState,
};
use super::policy::is_removable_orphan;

pub(super) struct AggregatedCatalog {
    pub artifacts: Vec<StartupArtifact>,
    pub groups: Vec<StartupOwnerGroup>,
    pub coverage: Vec<StartupSourceCoverage>,
    pub summary: StartupCatalogSummary,
    pub complete: bool,
    pub revision: String,
}

pub(super) fn aggregate(sources: Vec<PlatformStartupSourceResult>) -> AggregatedCatalog {
    let mut artifacts = Vec::new();
    let mut coverage = Vec::with_capacity(sources.len());
    let mut revision_hasher = blake3::Hasher::new();
    revision_hasher.update(b"mangodisk-startup-catalog-v1");

    for source in sources {
        let item_count = source.items.len() as u64;
        revision_hasher.update(&(source.source_id.len() as u64).to_le_bytes());
        revision_hasher.update(source.source_id.as_bytes());
        revision_hasher.update(&item_count.to_le_bytes());
        for item in source.items {
            let artifact = artifact_from_platform(&source.source_id, item);
            revision_hasher.update(artifact.fingerprint.as_bytes());
            artifacts.push(artifact);
        }
        coverage.push(StartupSourceCoverage {
            source_id: source.source_id,
            required: source.required,
            status: source.status.into(),
            reason: source.reason.map(Into::into),
            item_count,
            elapsed_ms: source.elapsed_ms,
        });
    }

    artifacts.sort_by(|left, right| left.item_id.cmp(&right.item_id));
    coverage.sort_by(|left, right| left.source_id.cmp(&right.source_id));
    let groups = group_artifacts(&artifacts);
    let complete = coverage
        .iter()
        .filter(|source| source.required)
        .all(|source| source.status == StartupCoverageStatus::Complete);
    let summary = summarize(&artifacts, &groups);

    AggregatedCatalog {
        artifacts,
        groups,
        coverage,
        summary,
        complete,
        revision: revision_hasher.finalize().to_hex().to_string(),
    }
}

fn artifact_from_platform(source_id: &str, item: PlatformStartupArtifact) -> StartupArtifact {
    let item_id = public_item_id(source_id, &item.provider_item_id);
    let group_identity_key = if let Some(owner_identity) = item.owner.identity_key.as_deref() {
        digest_id("owner", &[owner_identity.as_bytes()])
    } else if !item.target.identity_key.is_empty() {
        digest_id("target", &[item.target.identity_key.as_bytes()])
    } else {
        digest_id("unresolved", &[item_id.as_bytes()])
    };
    let fingerprint = artifact_fingerprint(source_id, &item);
    let removable_orphan = is_removable_orphan(&item);

    StartupArtifact {
        item_id,
        source_id: source_id.to_owned(),
        source_kind: item.source_kind.into(),
        scope: item.scope.into(),
        triggers: item.triggers.into_iter().map(Into::into).collect(),
        display_name: item.display_name,
        configuration_path: item.configuration_path.map(|path| display_path(&path)),
        target: StartupTarget {
            kind: item.target.kind.into(),
            path: item.target.path.map(|path| display_path(&path)),
            executable_name: item.target.executable_name,
            arguments: item.target.arguments,
        },
        owner_name: item.owner.name,
        publisher: item.owner.publisher,
        summary: item.owner.summary,
        summary_source: item.owner.summary_source.into(),
        version: item.owner.version,
        icon_path: item.owner.icon_path.map(|path| display_path(&path)),
        identity_confidence: if item.owner.identity_key.is_none()
            && !item.target.identity_key.is_empty()
        {
            StartupIdentityConfidence::Probable
        } else {
            item.owner.confidence.into()
        },
        configured_state: item.configured_state.into(),
        runtime_state: item.runtime_state.into(),
        control_capability: item.control_capability.into(),
        trust: item.trust.into(),
        modified_at_ms: item.modified_at_ms,
        diagnostics: item.diagnostics.into_iter().map(Into::into).collect(),
        removable_orphan,
        group_identity_key,
        fingerprint,
    }
}

fn group_artifacts(artifacts: &[StartupArtifact]) -> Vec<StartupOwnerGroup> {
    let mut grouped: BTreeMap<&str, Vec<&StartupArtifact>> = BTreeMap::new();
    for artifact in artifacts {
        grouped
            .entry(&artifact.group_identity_key)
            .or_default()
            .push(artifact);
    }

    grouped
        .into_iter()
        .map(|(identity_key, items)| {
            let representative = items
                .iter()
                .copied()
                .min_by_key(|item| item.identity_confidence)
                .expect("a startup group is created only from a non-empty item list");
            let mut source_kinds: Vec<_> = items.iter().map(|item| item.source_kind).collect();
            source_kinds.sort_unstable();
            source_kinds.dedup();
            let mut triggers: Vec<_> = items
                .iter()
                .flat_map(|item| item.triggers.iter().copied())
                .collect();
            triggers.sort_unstable();
            triggers.dedup();
            let mut scopes: Vec<_> = items.iter().map(|item| item.scope).collect();
            scopes.sort_unstable();
            scopes.dedup();
            let item_ids = items.iter().map(|item| item.item_id.clone()).collect();
            StartupOwnerGroup {
                group_id: digest_id("group", &[identity_key.as_bytes()]),
                name: representative
                    .owner_name
                    .clone()
                    .unwrap_or_else(|| representative.display_name.clone()),
                publisher: representative.publisher.clone(),
                summary: representative.summary.clone(),
                summary_source: representative.summary_source,
                version: representative.version.clone(),
                icon_path: representative.icon_path.clone(),
                identity_confidence: representative.identity_confidence,
                item_ids,
                source_kinds,
                triggers,
                scopes,
                configured_state: aggregate_configured_state(&items),
                control_state: aggregate_control_state(&items),
                system_item: items
                    .iter()
                    .all(|item| item.trust == StartupTrustState::System),
            }
        })
        .collect()
}

fn aggregate_configured_state(items: &[&StartupArtifact]) -> StartupAggregateConfiguredState {
    let enabled = items
        .iter()
        .filter(|item| item.configured_state == StartupConfiguredState::Enabled)
        .count();
    let disabled = items
        .iter()
        .filter(|item| item.configured_state == StartupConfiguredState::Disabled)
        .count();
    if enabled == items.len() {
        StartupAggregateConfiguredState::AllEnabled
    } else if disabled == items.len() {
        StartupAggregateConfiguredState::AllDisabled
    } else if enabled > 0 && disabled > 0 {
        StartupAggregateConfiguredState::PartiallyEnabled
    } else {
        StartupAggregateConfiguredState::Unknown
    }
}

fn aggregate_control_state(items: &[&StartupArtifact]) -> StartupAggregateControlState {
    let toggleable = items
        .iter()
        .filter(|item| item.control_capability == StartupControlCapability::Toggleable)
        .count();
    let elevation = items
        .iter()
        .filter(|item| item.control_capability == StartupControlCapability::ElevationRequired)
        .count();
    if toggleable == items.len() {
        StartupAggregateControlState::AllToggleable
    } else if elevation == items.len() {
        StartupAggregateControlState::RequiresElevation
    } else if toggleable + elevation > 0 {
        StartupAggregateControlState::PartiallyManageable
    } else {
        StartupAggregateControlState::ViewOnly
    }
}

fn summarize(artifacts: &[StartupArtifact], groups: &[StartupOwnerGroup]) -> StartupCatalogSummary {
    StartupCatalogSummary {
        item_count: artifacts.len() as u64,
        group_count: groups.len() as u64,
        enabled_count: artifacts
            .iter()
            .filter(|item| item.configured_state == StartupConfiguredState::Enabled)
            .count() as u64,
        disabled_count: artifacts
            .iter()
            .filter(|item| item.configured_state == StartupConfiguredState::Disabled)
            .count() as u64,
        unknown_state_count: artifacts
            .iter()
            .filter(|item| {
                matches!(
                    item.configured_state,
                    StartupConfiguredState::Unknown | StartupConfiguredState::NotApplicable
                )
            })
            .count() as u64,
        elevation_required_count: artifacts
            .iter()
            .filter(|item| item.control_capability == StartupControlCapability::ElevationRequired)
            .count() as u64,
        system_item_count: artifacts
            .iter()
            .filter(|item| item.trust == StartupTrustState::System)
            .count() as u64,
    }
}

fn artifact_fingerprint(source_id: &str, item: &PlatformStartupArtifact) -> String {
    let configured_state = format!("{:?}", item.configured_state);
    let runtime_state = format!("{:?}", item.runtime_state);
    let modified_at = item.modified_at_ms.unwrap_or_default().to_le_bytes();
    digest_id(
        "fingerprint",
        &[
            source_id.as_bytes(),
            item.provider_item_id.as_bytes(),
            item.target.identity_key.as_bytes(),
            configured_state.as_bytes(),
            runtime_state.as_bytes(),
            &modified_at,
        ],
    )
}

pub(super) fn public_item_id(source_id: &str, provider_item_id: &str) -> String {
    digest_id("item", &[source_id.as_bytes(), provider_item_id.as_bytes()])
}

fn digest_id(namespace: &str, values: &[&[u8]]) -> String {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"mangodisk-startup-id-v1");
    hasher.update(&(namespace.len() as u64).to_le_bytes());
    hasher.update(namespace.as_bytes());
    for value in values {
        hasher.update(&(value.len() as u64).to_le_bytes());
        hasher.update(value);
    }
    hasher.finalize().to_hex().to_string()
}

impl From<PlatformStartupSourceKind> for StartupSourceKind {
    fn from(value: PlatformStartupSourceKind) -> Self {
        match value {
            PlatformStartupSourceKind::RegistryRun => Self::RegistryRun,
            PlatformStartupSourceKind::StartupFolder => Self::StartupFolder,
            PlatformStartupSourceKind::ScheduledTask => Self::ScheduledTask,
            PlatformStartupSourceKind::Service => Self::Service,
            PlatformStartupSourceKind::PackagedStartupTask => Self::PackagedStartupTask,
            PlatformStartupSourceKind::LaunchAgent => Self::LaunchAgent,
            PlatformStartupSourceKind::LaunchDaemon => Self::LaunchDaemon,
            PlatformStartupSourceKind::LoginItem => Self::LoginItem,
            PlatformStartupSourceKind::BackgroundTask => Self::BackgroundTask,
            PlatformStartupSourceKind::EmbeddedItem => Self::EmbeddedItem,
            PlatformStartupSourceKind::AdvancedAutoRun => Self::AdvancedAutoRun,
        }
    }
}

impl From<PlatformStartupScope> for StartupScope {
    fn from(value: PlatformStartupScope) -> Self {
        match value {
            PlatformStartupScope::CurrentUser => Self::CurrentUser,
            PlatformStartupScope::User => Self::User,
            PlatformStartupScope::AllUsers => Self::AllUsers,
            PlatformStartupScope::Machine => Self::Machine,
            PlatformStartupScope::System => Self::System,
        }
    }
}

impl From<PlatformStartupTrigger> for StartupTrigger {
    fn from(value: PlatformStartupTrigger) -> Self {
        match value {
            PlatformStartupTrigger::Boot => Self::Boot,
            PlatformStartupTrigger::UserLogon => Self::UserLogon,
            PlatformStartupTrigger::Scheduled => Self::Scheduled,
            PlatformStartupTrigger::Event => Self::Event,
            PlatformStartupTrigger::KeepAlive => Self::KeepAlive,
            PlatformStartupTrigger::ShellLoad => Self::ShellLoad,
            PlatformStartupTrigger::ApplicationLaunch => Self::ApplicationLaunch,
            PlatformStartupTrigger::Unknown => Self::Unknown,
        }
    }
}

impl From<PlatformStartupConfiguredState> for StartupConfiguredState {
    fn from(value: PlatformStartupConfiguredState) -> Self {
        match value {
            PlatformStartupConfiguredState::Enabled => Self::Enabled,
            PlatformStartupConfiguredState::Disabled => Self::Disabled,
            PlatformStartupConfiguredState::Unknown => Self::Unknown,
            PlatformStartupConfiguredState::NotApplicable => Self::NotApplicable,
        }
    }
}

impl From<PlatformStartupRuntimeState> for StartupRuntimeState {
    fn from(value: PlatformStartupRuntimeState) -> Self {
        match value {
            PlatformStartupRuntimeState::Running => Self::Running,
            PlatformStartupRuntimeState::Stopped => Self::Stopped,
            PlatformStartupRuntimeState::Loaded => Self::Loaded,
            PlatformStartupRuntimeState::Unloaded => Self::Unloaded,
            PlatformStartupRuntimeState::Unknown => Self::Unknown,
        }
    }
}

impl From<PlatformStartupControlCapability> for StartupControlCapability {
    fn from(value: PlatformStartupControlCapability) -> Self {
        match value {
            PlatformStartupControlCapability::Toggleable => Self::Toggleable,
            PlatformStartupControlCapability::ElevationRequired => Self::ElevationRequired,
            PlatformStartupControlCapability::RemoveOnly => Self::RemoveOnly,
            PlatformStartupControlCapability::SystemManaged => Self::SystemManaged,
            PlatformStartupControlCapability::PolicyManaged => Self::PolicyManaged,
            PlatformStartupControlCapability::ViewOnly => Self::ViewOnly,
        }
    }
}

impl From<PlatformStartupTrustState> for StartupTrustState {
    fn from(value: PlatformStartupTrustState) -> Self {
        match value {
            PlatformStartupTrustState::System => Self::System,
            PlatformStartupTrustState::Verified => Self::Verified,
            PlatformStartupTrustState::Invalid => Self::Invalid,
            PlatformStartupTrustState::Unsigned => Self::Unsigned,
            PlatformStartupTrustState::Unknown => Self::Unknown,
        }
    }
}

impl From<PlatformStartupIdentityConfidence> for StartupIdentityConfidence {
    fn from(value: PlatformStartupIdentityConfidence) -> Self {
        match value {
            PlatformStartupIdentityConfidence::Exact => Self::Exact,
            PlatformStartupIdentityConfidence::Strong => Self::Strong,
            PlatformStartupIdentityConfidence::Probable => Self::Probable,
            PlatformStartupIdentityConfidence::Unresolved => Self::Unresolved,
        }
    }
}

impl From<PlatformStartupSummarySource> for StartupSummarySource {
    fn from(value: PlatformStartupSummarySource) -> Self {
        match value {
            PlatformStartupSummarySource::ServiceDescription => Self::ServiceDescription,
            PlatformStartupSummarySource::TaskDescription => Self::TaskDescription,
            PlatformStartupSummarySource::PackageManifest => Self::PackageManifest,
            PlatformStartupSummarySource::VersionInfo => Self::VersionInfo,
            PlatformStartupSummarySource::BundleMetadata => Self::BundleMetadata,
            PlatformStartupSummarySource::SourceLabel => Self::SourceLabel,
            PlatformStartupSummarySource::Unavailable => Self::Unavailable,
        }
    }
}

impl From<PlatformStartupTargetKind> for StartupTargetKind {
    fn from(value: PlatformStartupTargetKind) -> Self {
        match value {
            PlatformStartupTargetKind::Executable => Self::Executable,
            PlatformStartupTargetKind::Application => Self::Application,
            PlatformStartupTargetKind::Script => Self::Script,
            PlatformStartupTargetKind::Service => Self::Service,
            PlatformStartupTargetKind::Task => Self::Task,
            PlatformStartupTargetKind::Other => Self::Other,
            PlatformStartupTargetKind::Unknown => Self::Unknown,
        }
    }
}

impl From<PlatformStartupDiagnosticCode> for StartupDiagnosticCode {
    fn from(value: PlatformStartupDiagnosticCode) -> Self {
        match value {
            PlatformStartupDiagnosticCode::AccessDenied => Self::AccessDenied,
            PlatformStartupDiagnosticCode::InvalidData => Self::InvalidData,
            PlatformStartupDiagnosticCode::MissingIdentity => Self::MissingIdentity,
            PlatformStartupDiagnosticCode::MissingTarget => Self::MissingTarget,
            PlatformStartupDiagnosticCode::StateUnavailable => Self::StateUnavailable,
            PlatformStartupDiagnosticCode::UnsupportedFormat => Self::UnsupportedFormat,
        }
    }
}

impl From<PlatformStartupCoverageStatus> for StartupCoverageStatus {
    fn from(value: PlatformStartupCoverageStatus) -> Self {
        match value {
            PlatformStartupCoverageStatus::Complete => Self::Complete,
            PlatformStartupCoverageStatus::Partial => Self::Partial,
            PlatformStartupCoverageStatus::Unavailable => Self::Unavailable,
            PlatformStartupCoverageStatus::Failed => Self::Failed,
            PlatformStartupCoverageStatus::Cancelled => Self::Cancelled,
        }
    }
}

impl From<PlatformStartupCoverageReason> for StartupCoverageReason {
    fn from(value: PlatformStartupCoverageReason) -> Self {
        match value {
            PlatformStartupCoverageReason::AccessDenied => Self::AccessDenied,
            PlatformStartupCoverageReason::ApiUnavailable => Self::ApiUnavailable,
            PlatformStartupCoverageReason::Cancelled => Self::Cancelled,
            PlatformStartupCoverageReason::InvalidData => Self::InvalidData,
            PlatformStartupCoverageReason::NotImplemented => Self::NotImplemented,
            PlatformStartupCoverageReason::StateUnavailable => Self::StateUnavailable,
            PlatformStartupCoverageReason::UnsupportedOperatingSystem => {
                Self::UnsupportedOperatingSystem
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use mangodisk_platform::{PlatformStartupOwner, PlatformStartupTarget};

    use super::*;

    fn artifact(provider_id: &str, name: &str, target: &str) -> PlatformStartupArtifact {
        PlatformStartupArtifact {
            provider_item_id: provider_id.to_owned(),
            source_kind: PlatformStartupSourceKind::LaunchAgent,
            scope: PlatformStartupScope::CurrentUser,
            triggers: vec![PlatformStartupTrigger::UserLogon],
            display_name: name.to_owned(),
            configuration_path: None,
            target: PlatformStartupTarget {
                kind: PlatformStartupTargetKind::Executable,
                identity_key: target.to_owned(),
                path: Some(PathBuf::from(target)),
                executable_name: Some(name.to_owned()),
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
            configured_state: PlatformStartupConfiguredState::Enabled,
            runtime_state: PlatformStartupRuntimeState::Unknown,
            control_capability: PlatformStartupControlCapability::Toggleable,
            trust: PlatformStartupTrustState::Unknown,
            modified_at_ms: None,
            diagnostics: Vec::new(),
        }
    }

    fn source(items: Vec<PlatformStartupArtifact>) -> PlatformStartupSourceResult {
        PlatformStartupSourceResult {
            source_id: "test.launchd".to_owned(),
            required: true,
            status: PlatformStartupCoverageStatus::Complete,
            reason: None,
            items,
            elapsed_ms: 1,
        }
    }

    #[test]
    fn equal_display_names_do_not_merge_different_targets() {
        let result = aggregate(vec![source(vec![
            artifact("one", "Helper", "/Applications/One/Helper"),
            artifact("two", "Helper", "/Applications/Two/Helper"),
        ])]);

        assert_eq!(result.groups.len(), 2);
    }

    #[test]
    fn catalog_exposes_core_verified_orphan_removal_capability() {
        let mut removable = artifact(
            "orphan",
            "Orphan",
            "/Applications/Missing/Contents/MacOS/Missing",
        );
        removable.configuration_path = Some(PathBuf::from(
            "/Users/fixture/Library/LaunchAgents/com.example.orphan.plist",
        ));
        removable
            .diagnostics
            .push(PlatformStartupDiagnosticCode::MissingTarget);
        let safe = artifact("safe", "Safe", "/Applications/Safe/Contents/MacOS/Safe");

        let result = aggregate(vec![source(vec![removable, safe])]);

        assert!(result
            .artifacts
            .iter()
            .find(|item| item.display_name == "Orphan")
            .is_some_and(|item| item.removable_orphan));
        assert!(result
            .artifacts
            .iter()
            .find(|item| item.display_name == "Safe")
            .is_some_and(|item| !item.removable_orphan));
    }

    #[test]
    fn exact_owner_identity_merges_multiple_native_items() {
        let mut first = artifact("one", "Example", "/Applications/Example/A");
        let mut second = artifact("two", "Example", "/Applications/Example/B");
        for item in [&mut first, &mut second] {
            item.owner.identity_key = Some("com.example.app".to_owned());
            item.owner.name = Some("Example".to_owned());
            item.owner.confidence = PlatformStartupIdentityConfidence::Exact;
        }

        let result = aggregate(vec![source(vec![first, second])]);

        assert_eq!(result.groups.len(), 1);
        assert_eq!(result.groups[0].item_ids.len(), 2);
        assert_eq!(
            result.groups[0].identity_confidence,
            StartupIdentityConfidence::Exact
        );
    }

    #[test]
    fn group_prefers_the_strongest_available_owner_metadata() {
        let mut exact = artifact("one", "Fallback", "/Applications/Example/A");
        exact.owner.identity_key = Some("com.example.app".to_owned());
        exact.owner.name = Some("Example".to_owned());
        exact.owner.publisher = Some("Example Publisher".to_owned());
        exact.owner.confidence = PlatformStartupIdentityConfidence::Exact;

        let mut unresolved = artifact("two", "Technical Helper", "/Applications/Example/B");
        unresolved.owner.identity_key = Some("com.example.app".to_owned());
        unresolved.owner.confidence = PlatformStartupIdentityConfidence::Unresolved;

        let result = aggregate(vec![source(vec![unresolved, exact])]);

        assert_eq!(result.groups.len(), 1);
        assert_eq!(result.groups[0].name, "Example");
        assert_eq!(
            result.groups[0].publisher.as_deref(),
            Some("Example Publisher")
        );
        assert_eq!(
            result.groups[0].identity_confidence,
            StartupIdentityConfidence::Exact
        );
    }

    #[test]
    fn mixed_native_states_produce_partial_group_state() {
        let first = artifact("one", "Example", "/Applications/Example/A");
        let mut second = artifact("two", "Example", "/Applications/Example/A");
        second.configured_state = PlatformStartupConfiguredState::Disabled;

        let result = aggregate(vec![source(vec![first, second])]);

        assert_eq!(result.groups.len(), 1);
        assert_eq!(
            result.groups[0].configured_state,
            StartupAggregateConfiguredState::PartiallyEnabled
        );
    }

    #[test]
    fn optional_unavailable_source_does_not_make_catalog_incomplete() {
        let result = aggregate(vec![
            source(vec![artifact("one", "Example", "/Applications/Example")]),
            PlatformStartupSourceResult::unavailable(
                "test.experimental",
                false,
                PlatformStartupCoverageReason::NotImplemented,
            ),
        ]);

        assert!(result.complete);
    }
}

use std::path::Path;

use mangodisk_platform::{
    PlatformStartupArtifact, PlatformStartupControlCapability, PlatformStartupDiagnosticCode,
    PlatformStartupSourceKind,
};

/// Returns whether an artifact has enough current, provider-owned evidence for permanent removal.
///
/// A missing application association is intentionally insufficient. Application inventories are
/// best-effort and may omit custom, nested, external, or temporarily unreadable bundles. Requiring
/// a missing concrete target keeps orphan cleanup fail-closed without expanding startup scans into
/// broad filesystem traversal.
pub(super) fn is_removable_orphan(artifact: &PlatformStartupArtifact) -> bool {
    if !artifact
        .diagnostics
        .contains(&PlatformStartupDiagnosticCode::MissingTarget)
    {
        return false;
    }
    match artifact.source_kind {
        PlatformStartupSourceKind::LaunchAgent | PlatformStartupSourceKind::LaunchDaemon => {
            has_extension(artifact, "plist")
                && matches!(
                    artifact.control_capability,
                    PlatformStartupControlCapability::Toggleable
                        | PlatformStartupControlCapability::ElevationRequired
                )
        }
        PlatformStartupSourceKind::RegistryRun => matches!(
            artifact.control_capability,
            PlatformStartupControlCapability::Toggleable
                | PlatformStartupControlCapability::ElevationRequired
                | PlatformStartupControlCapability::RemoveOnly
        ),
        PlatformStartupSourceKind::StartupFolder => {
            has_extension(artifact, "lnk")
                && matches!(
                    artifact.control_capability,
                    PlatformStartupControlCapability::Toggleable
                        | PlatformStartupControlCapability::ElevationRequired
                )
        }
        _ => false,
    }
}

fn has_extension(artifact: &PlatformStartupArtifact, expected: &str) -> bool {
    artifact
        .configuration_path
        .as_deref()
        .and_then(Path::extension)
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| extension.eq_ignore_ascii_case(expected))
}

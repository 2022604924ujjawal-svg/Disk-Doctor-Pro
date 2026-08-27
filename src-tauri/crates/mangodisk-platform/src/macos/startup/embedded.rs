use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Instant, UNIX_EPOCH};

use plist::{Dictionary, Value};

use crate::{
    PlatformCancellation, PlatformStartupArtifact, PlatformStartupConfiguredState,
    PlatformStartupControlCapability, PlatformStartupCoverageReason, PlatformStartupCoverageStatus,
    PlatformStartupDiagnosticCode, PlatformStartupIdentityConfidence, PlatformStartupOwner,
    PlatformStartupRuntimeState, PlatformStartupScope, PlatformStartupSourceKind,
    PlatformStartupSourceResult, PlatformStartupSummarySource, PlatformStartupTarget,
    PlatformStartupTargetKind, PlatformStartupTrigger,
};

use super::metadata::code_signature_metadata;

struct ApplicationBundle {
    path: PathBuf,
    metadata: Dictionary,
    system_owned: bool,
}

pub(super) fn scan(cancellation: &PlatformCancellation) -> PlatformStartupSourceResult {
    let started = Instant::now();
    let mut items = Vec::new();
    let mut partial_reason = None;
    for (root, system_owned) in application_roots() {
        if cancellation.is_cancelled() {
            return result(
                items,
                PlatformStartupCoverageStatus::Cancelled,
                Some(PlatformStartupCoverageReason::Cancelled),
                started,
            );
        }
        let entries = match fs::read_dir(&root) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => {
                partial_reason.get_or_insert(
                    if error.kind() == std::io::ErrorKind::PermissionDenied {
                        PlatformStartupCoverageReason::AccessDenied
                    } else {
                        PlatformStartupCoverageReason::InvalidData
                    },
                );
                continue;
            }
        };
        for entry in entries {
            if cancellation.is_cancelled() {
                return result(
                    items,
                    PlatformStartupCoverageStatus::Cancelled,
                    Some(PlatformStartupCoverageReason::Cancelled),
                    started,
                );
            }
            let Ok(entry) = entry else {
                partial_reason.get_or_insert(PlatformStartupCoverageReason::InvalidData);
                continue;
            };
            let path = entry.path();
            if !is_application_bundle(&path)
                || entry.file_type().is_ok_and(|kind| kind.is_symlink())
            {
                continue;
            }
            let Some(metadata) = read_bundle_metadata(&path) else {
                continue;
            };
            let owner = ApplicationBundle {
                path,
                metadata,
                system_owned,
            };
            scan_login_items(&owner, &mut items, &mut partial_reason);
        }
    }
    result(
        items,
        if partial_reason.is_some() {
            PlatformStartupCoverageStatus::Partial
        } else {
            PlatformStartupCoverageStatus::Complete
        },
        partial_reason,
        started,
    )
}

pub(super) fn application_roots() -> Vec<(PathBuf, bool)> {
    let mut roots = vec![
        (PathBuf::from("/Applications"), false),
        (PathBuf::from("/System/Applications"), true),
        (
            PathBuf::from("/System/Library/CoreServices/Applications"),
            true,
        ),
    ];
    if let Some(home) = dirs::home_dir() {
        roots.push((home.join("Applications"), false));
    }
    roots
}

fn scan_login_items(
    owner: &ApplicationBundle,
    items: &mut Vec<PlatformStartupArtifact>,
    partial_reason: &mut Option<PlatformStartupCoverageReason>,
) {
    let directory = owner.path.join("Contents/Library/LoginItems");
    let entries = match fs::read_dir(&directory) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return,
        Err(error) => {
            partial_reason.get_or_insert(if error.kind() == std::io::ErrorKind::PermissionDenied {
                PlatformStartupCoverageReason::AccessDenied
            } else {
                PlatformStartupCoverageReason::InvalidData
            });
            return;
        }
    };
    for entry in entries {
        let Ok(entry) = entry else {
            partial_reason.get_or_insert(PlatformStartupCoverageReason::InvalidData);
            continue;
        };
        let path = entry.path();
        if !is_application_bundle(&path) || entry.file_type().is_ok_and(|kind| kind.is_symlink()) {
            continue;
        }
        let Some(metadata) = read_bundle_metadata(&path) else {
            partial_reason.get_or_insert(PlatformStartupCoverageReason::InvalidData);
            continue;
        };
        items.push(login_item_artifact(owner, &path, &metadata));
    }
}

fn login_item_artifact(
    owner: &ApplicationBundle,
    path: &Path,
    metadata: &Dictionary,
) -> PlatformStartupArtifact {
    let owner_bundle_id = string_value(&owner.metadata, "CFBundleIdentifier");
    let item_bundle_id = string_value(metadata, "CFBundleIdentifier");
    let executable = string_value(metadata, "CFBundleExecutable");
    let target_path = executable
        .as_deref()
        .map(|name| path.join("Contents/MacOS").join(name));
    let signature =
        code_signature_metadata(target_path.as_deref().unwrap_or(path), owner.system_owned);
    let display_name = bundle_name(metadata)
        .or_else(|| {
            path.file_stem()
                .map(|value| value.to_string_lossy().into_owned())
        })
        .unwrap_or_else(|| "Embedded login item".to_string());
    let owner_name = bundle_name(&owner.metadata).or_else(|| {
        owner
            .path
            .file_stem()
            .map(|value| value.to_string_lossy().into_owned())
    });
    let mut diagnostics = Vec::new();
    if item_bundle_id.is_none() {
        diagnostics.push(PlatformStartupDiagnosticCode::MissingIdentity);
    }
    if target_path.as_deref().is_none_or(|target| !target.exists()) {
        diagnostics.push(PlatformStartupDiagnosticCode::MissingTarget);
    }
    let target_identity = item_bundle_id
        .clone()
        .map(|value| format!("bundle:{value}"))
        .or_else(|| {
            target_path
                .as_deref()
                .map(|target| format!("path:{}", target.to_string_lossy()))
        })
        .unwrap_or_else(|| format!("bundle-path:{}", path.to_string_lossy()));
    PlatformStartupArtifact {
        provider_item_id: format!(
            "embedded:{}:{}",
            owner_bundle_id.as_deref().unwrap_or("unknown-owner"),
            item_bundle_id.as_deref().unwrap_or(&target_identity)
        ),
        source_kind: PlatformStartupSourceKind::EmbeddedItem,
        scope: if owner.system_owned {
            PlatformStartupScope::System
        } else {
            PlatformStartupScope::User
        },
        triggers: vec![PlatformStartupTrigger::UserLogon],
        display_name,
        configuration_path: None,
        target: PlatformStartupTarget {
            kind: PlatformStartupTargetKind::Application,
            identity_key: target_identity,
            path: target_path.clone().or_else(|| Some(path.to_path_buf())),
            executable_name: executable,
            arguments: Vec::new(),
        },
        owner: PlatformStartupOwner {
            identity_key: owner_bundle_id.map(|value| format!("bundle:{value}")),
            name: owner_name,
            publisher: signature.publisher,
            summary: None,
            summary_source: PlatformStartupSummarySource::BundleMetadata,
            version: string_value(&owner.metadata, "CFBundleShortVersionString"),
            icon_path: Some(owner.path.clone()),
            confidence: PlatformStartupIdentityConfidence::Exact,
        },
        // An embedded declaration proves that the item exists, not that the user currently allows
        // it to run. A separate system-state provider must supply enabled or disabled status.
        configured_state: PlatformStartupConfiguredState::Unknown,
        runtime_state: PlatformStartupRuntimeState::Unknown,
        control_capability: PlatformStartupControlCapability::ViewOnly,
        trust: signature.trust,
        modified_at_ms: modified_at_ms(path),
        diagnostics,
    }
}

fn is_application_bundle(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| extension.eq_ignore_ascii_case("app"))
        && path.is_dir()
}

pub(super) fn read_bundle_metadata(path: &Path) -> Option<Dictionary> {
    Value::from_file(path.join("Contents/Info.plist"))
        .ok()?
        .into_dictionary()
}

pub(super) fn bundle_name(metadata: &Dictionary) -> Option<String> {
    string_value(metadata, "CFBundleDisplayName").or_else(|| string_value(metadata, "CFBundleName"))
}

pub(super) fn string_value(dictionary: &Dictionary, key: &str) -> Option<String> {
    dictionary
        .get(key)
        .and_then(Value::as_string)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
}

fn modified_at_ms(path: &Path) -> Option<u64> {
    path.metadata()
        .ok()?
        .modified()
        .ok()?
        .duration_since(UNIX_EPOCH)
        .ok()
        .map(|duration| duration.as_millis() as u64)
}

fn result(
    items: Vec<PlatformStartupArtifact>,
    status: PlatformStartupCoverageStatus,
    reason: Option<PlatformStartupCoverageReason>,
    started: Instant,
) -> PlatformStartupSourceResult {
    PlatformStartupSourceResult {
        source_id: "macos.embedded_items".to_string(),
        required: true,
        status,
        reason,
        items,
        elapsed_ms: started.elapsed().as_millis() as u64,
    }
}

#[cfg(test)]
mod tests {
    use plist::{Dictionary, Value};

    use super::bundle_name;

    #[test]
    fn display_name_takes_precedence_over_bundle_name() {
        let mut metadata = Dictionary::new();
        metadata.insert(
            "CFBundleName".to_string(),
            Value::String("Internal".to_string()),
        );
        metadata.insert(
            "CFBundleDisplayName".to_string(),
            Value::String("Visible".to_string()),
        );

        assert_eq!(bundle_name(&metadata).as_deref(), Some("Visible"));
    }
}

use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    process::Command,
    time::{Instant, SystemTime, UNIX_EPOCH},
};

use plist::{Dictionary, Value};

use crate::{
    PlatformCancellation, PlatformError, PlatformErrorCode, PlatformResult,
    PlatformStartupArtifact, PlatformStartupChangeRequest, PlatformStartupChangeResult,
    PlatformStartupConfiguredState, PlatformStartupControlCapability,
    PlatformStartupCoverageReason, PlatformStartupCoverageStatus, PlatformStartupDesiredState,
    PlatformStartupDiagnosticCode, PlatformStartupIdentityConfidence, PlatformStartupOwner,
    PlatformStartupRuntimeState, PlatformStartupScope, PlatformStartupSourceKind,
    PlatformStartupSourceResult, PlatformStartupSummarySource, PlatformStartupTarget,
    PlatformStartupTargetKind, PlatformStartupTrigger, PlatformStartupTrustState,
};

use super::{bundle_index::BundleIndex, metadata::code_signature_metadata};

struct LaunchdSource {
    source_id: &'static str,
    paths: Vec<PathBuf>,
    source_kind: PlatformStartupSourceKind,
    scope: PlatformStartupScope,
    capability: PlatformStartupControlCapability,
    domain: LaunchdDomain,
    system_owned: bool,
}

#[derive(Clone, Copy)]
enum LaunchdDomain {
    Gui,
    System,
}

#[derive(Clone, Copy)]
enum ScanIssue {
    AccessDenied,
    InvalidData,
    StateUnavailable,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TargetPresence {
    Present,
    Missing,
    Unknown,
}

pub(super) fn scan(cancellation: &PlatformCancellation) -> Vec<PlatformStartupSourceResult> {
    // Rebuild the application index for every scan so uninstalling or installing an application
    // is reflected without restarting MangoDisk.
    let bundle_index = BundleIndex::discover();
    scan_with_bundle_index(cancellation, &bundle_index)
}

pub(super) fn scan_with_bundle_index(
    cancellation: &PlatformCancellation,
    bundle_index: &BundleIndex,
) -> Vec<PlatformStartupSourceResult> {
    let sources = launchd_sources();
    let gui_overrides = disabled_overrides(LaunchdDomain::Gui, None);
    let system_overrides = disabled_overrides(LaunchdDomain::System, None);

    sources
        .iter()
        .map(|source| {
            if cancellation.is_cancelled() {
                return PlatformStartupSourceResult {
                    source_id: source.source_id.to_owned(),
                    required: true,
                    status: PlatformStartupCoverageStatus::Cancelled,
                    reason: Some(PlatformStartupCoverageReason::Cancelled),
                    items: Vec::new(),
                    elapsed_ms: 0,
                };
            }
            let overrides = match source.domain {
                LaunchdDomain::Gui => gui_overrides.as_ref(),
                LaunchdDomain::System => system_overrides.as_ref(),
            };
            scan_source(source, overrides, cancellation, bundle_index)
        })
        .collect()
}

fn launchd_sources() -> Vec<LaunchdSource> {
    let home = dirs::home_dir();
    vec![
        LaunchdSource {
            source_id: "macos.launchd.user_agents",
            paths: home
                .map(|path| vec![path.join("Library/LaunchAgents")])
                .unwrap_or_default(),
            source_kind: PlatformStartupSourceKind::LaunchAgent,
            scope: PlatformStartupScope::CurrentUser,
            capability: PlatformStartupControlCapability::Toggleable,
            domain: LaunchdDomain::Gui,
            system_owned: false,
        },
        LaunchdSource {
            source_id: "macos.launchd.local_agents",
            paths: vec![PathBuf::from("/Library/LaunchAgents")],
            source_kind: PlatformStartupSourceKind::LaunchAgent,
            scope: PlatformStartupScope::Machine,
            capability: PlatformStartupControlCapability::ElevationRequired,
            domain: LaunchdDomain::Gui,
            system_owned: false,
        },
        LaunchdSource {
            source_id: "macos.launchd.local_daemons",
            paths: vec![PathBuf::from("/Library/LaunchDaemons")],
            source_kind: PlatformStartupSourceKind::LaunchDaemon,
            scope: PlatformStartupScope::Machine,
            capability: PlatformStartupControlCapability::ElevationRequired,
            domain: LaunchdDomain::System,
            system_owned: false,
        },
        LaunchdSource {
            source_id: "macos.launchd.system_agents",
            paths: vec![
                PathBuf::from("/System/Library/LaunchAgents"),
                PathBuf::from("/Library/Apple/System/Library/LaunchAgents"),
            ],
            source_kind: PlatformStartupSourceKind::LaunchAgent,
            scope: PlatformStartupScope::System,
            capability: PlatformStartupControlCapability::ViewOnly,
            domain: LaunchdDomain::Gui,
            system_owned: true,
        },
        LaunchdSource {
            source_id: "macos.launchd.system_daemons",
            paths: vec![
                PathBuf::from("/System/Library/LaunchDaemons"),
                PathBuf::from("/Library/Apple/System/Library/LaunchDaemons"),
            ],
            source_kind: PlatformStartupSourceKind::LaunchDaemon,
            scope: PlatformStartupScope::System,
            capability: PlatformStartupControlCapability::ViewOnly,
            domain: LaunchdDomain::System,
            system_owned: true,
        },
    ]
}

pub(super) fn change(
    request: &PlatformStartupChangeRequest,
) -> PlatformResult<PlatformStartupChangeResult> {
    let bundle_index = BundleIndex::discover();
    change_with_context(request, None, false, &bundle_index)
}

pub(super) fn privileged_change_with_bundle_index(
    request: &PlatformStartupChangeRequest,
    interactive_user_id: u32,
    bundle_index: &BundleIndex,
) -> PlatformResult<PlatformStartupChangeResult> {
    change_with_context(request, Some(interactive_user_id), true, bundle_index)
}

fn change_with_context(
    request: &PlatformStartupChangeRequest,
    interactive_user_id: Option<u32>,
    privileged: bool,
    bundle_index: &BundleIndex,
) -> PlatformResult<PlatformStartupChangeResult> {
    let source = launchd_sources()
        .into_iter()
        .find(|source| source.source_id == request.source_id)
        .ok_or_else(|| {
            PlatformError::new(
                PlatformErrorCode::Unsupported,
                "launchd source is not available for configured-state changes",
            )
        })?;
    if (!privileged && source.scope != PlatformStartupScope::CurrentUser)
        || (privileged
            && !matches!(
                source.source_id,
                "macos.launchd.local_agents" | "macos.launchd.local_daemons"
            ))
    {
        return Err(PlatformError::new(
            PlatformErrorCode::AccessDenied,
            "launchd item requires a privileged operation",
        ));
    }
    let overrides = disabled_overrides(source.domain, interactive_user_id).ok_or_else(|| {
        PlatformError::new(
            PlatformErrorCode::OperationFailed,
            "launchd disabled state is unavailable",
        )
    })?;
    let (label, current) = find_item(&source, &overrides, &request.provider_item_id, bundle_index)?
        .ok_or_else(|| PlatformError::item_changed("launchd item no longer exists"))?;
    if current != request.expected_artifact {
        return Err(PlatformError::item_changed(
            "launchd item changed after preflight",
        ));
    }
    let expected_capability = if privileged {
        PlatformStartupControlCapability::ElevationRequired
    } else {
        PlatformStartupControlCapability::Toggleable
    };
    if current.control_capability != expected_capability {
        return Err(PlatformError::new(
            PlatformErrorCode::Unsupported,
            "launchd item is not toggleable",
        ));
    }
    if request.desired_state == PlatformStartupDesiredState::Removed {
        return remove_orphaned_item(&source, &current);
    }
    let desired = match request.desired_state {
        PlatformStartupDesiredState::Enabled => PlatformStartupConfiguredState::Enabled,
        PlatformStartupDesiredState::Disabled => PlatformStartupConfiguredState::Disabled,
        PlatformStartupDesiredState::Removed => {
            unreachable!("removed launchd items return before state changes")
        }
    };
    if current.configured_state != desired {
        let action = match request.desired_state {
            PlatformStartupDesiredState::Enabled => "enable",
            PlatformStartupDesiredState::Disabled => "disable",
            PlatformStartupDesiredState::Removed => {
                unreachable!("removed launchd items return before launchctl changes")
            }
        };
        let target = match source.domain {
            LaunchdDomain::Gui => format!(
                "gui/{}/{}",
                interactive_user_id.unwrap_or_else(|| unsafe { libc::geteuid() }),
                label
            ),
            LaunchdDomain::System => format!("system/{label}"),
        };
        let output = Command::new("/bin/launchctl")
            .args([action, &target])
            .output()
            .map_err(|error| PlatformError::io("change launchd configured state", &error))?;
        if !output.status.success() {
            return Err(PlatformError::new(
                PlatformErrorCode::OperationFailed,
                "launchd rejected the configured-state change",
            ));
        }
    }
    let verified_overrides =
        disabled_overrides(source.domain, interactive_user_id).ok_or_else(|| {
            PlatformError::new(
                PlatformErrorCode::OperationFailed,
                "launchd state verification is unavailable",
            )
        })?;
    let (_, verified) = find_item(
        &source,
        &verified_overrides,
        &request.provider_item_id,
        bundle_index,
    )?
    .ok_or_else(|| PlatformError::item_changed("launchd item disappeared during verification"))?;
    Ok(PlatformStartupChangeResult {
        previous_state: current.configured_state,
        configured_state: verified.configured_state,
        verified: verified.configured_state == desired,
    })
}

fn remove_orphaned_item(
    source: &LaunchdSource,
    current: &PlatformStartupArtifact,
) -> PlatformResult<PlatformStartupChangeResult> {
    if !current
        .diagnostics
        .contains(&PlatformStartupDiagnosticCode::MissingTarget)
    {
        return Err(PlatformError::item_changed(
            "launchd item is no longer an orphaned startup configuration",
        ));
    }
    let configuration_path = current.configuration_path.as_deref().ok_or_else(|| {
        PlatformError::new(
            PlatformErrorCode::InvalidData,
            "launchd item has no configuration path",
        )
    })?;
    let parent_is_allowlisted = source
        .paths
        .iter()
        .any(|directory| configuration_path.parent() == Some(directory.as_path()));
    let is_property_list = configuration_path
        .extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| extension.eq_ignore_ascii_case("plist"));
    let metadata = fs::symlink_metadata(configuration_path)
        .map_err(|error| PlatformError::io("inspect orphaned launchd item", &error))?;
    if !parent_is_allowlisted
        || !is_property_list
        || !metadata.is_file()
        || metadata.file_type().is_symlink()
    {
        return Err(PlatformError::invalid_path(
            "launchd configuration is outside the removable source boundary",
        ));
    }

    fs::remove_file(configuration_path)
        .map_err(|error| PlatformError::io("remove orphaned launchd item", &error))?;
    let verified = matches!(
        fs::symlink_metadata(configuration_path),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound
    );
    Ok(PlatformStartupChangeResult {
        previous_state: current.configured_state,
        configured_state: PlatformStartupConfiguredState::NotApplicable,
        verified,
    })
}

fn find_item(
    source: &LaunchdSource,
    overrides: &BTreeMap<String, bool>,
    provider_item_id: &str,
    bundle_index: &BundleIndex,
) -> PlatformResult<Option<(String, PlatformStartupArtifact)>> {
    for directory in &source.paths {
        let entries = match fs::read_dir(directory) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(PlatformError::io("read launchd source", &error)),
        };
        for entry in entries {
            let Ok(entry) = entry else {
                continue;
            };
            let path = entry.path();
            if !path
                .extension()
                .is_some_and(|extension| extension.eq_ignore_ascii_case("plist"))
            {
                continue;
            }
            let Ok(metadata) = fs::symlink_metadata(&path) else {
                continue;
            };
            if !metadata.is_file() || metadata.file_type().is_symlink() {
                continue;
            }
            let Ok(value) = Value::from_file(&path) else {
                continue;
            };
            let Some(dictionary) = value.as_dictionary() else {
                continue;
            };
            let artifact = artifact_from_dictionary(
                dictionary,
                &path,
                metadata.modified().ok(),
                source,
                Some(overrides),
                bundle_index,
            );
            if artifact.provider_item_id == provider_item_id {
                let label = string_value(dictionary, "Label").ok_or_else(|| {
                    PlatformError::new(
                        PlatformErrorCode::InvalidData,
                        "launchd item has no service label",
                    )
                })?;
                return Ok(Some((label, artifact)));
            }
        }
    }
    Ok(None)
}

fn scan_source(
    source: &LaunchdSource,
    overrides: Option<&BTreeMap<String, bool>>,
    cancellation: &PlatformCancellation,
    bundle_index: &BundleIndex,
) -> PlatformStartupSourceResult {
    let started = Instant::now();
    let mut items = Vec::new();
    let mut issues = if overrides.is_none() {
        vec![ScanIssue::StateUnavailable]
    } else {
        Vec::new()
    };

    for path in &source.paths {
        if cancellation.is_cancelled() {
            return PlatformStartupSourceResult {
                source_id: source.source_id.to_owned(),
                required: true,
                status: PlatformStartupCoverageStatus::Cancelled,
                reason: Some(PlatformStartupCoverageReason::Cancelled),
                items,
                elapsed_ms: started.elapsed().as_millis() as u64,
            };
        }
        let entries = match fs::read_dir(path) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) if error.kind() == std::io::ErrorKind::PermissionDenied => {
                issues.push(ScanIssue::AccessDenied);
                continue;
            }
            Err(_) => {
                issues.push(ScanIssue::InvalidData);
                continue;
            }
        };
        for entry in entries {
            if cancellation.is_cancelled() {
                return PlatformStartupSourceResult {
                    source_id: source.source_id.to_owned(),
                    required: true,
                    status: PlatformStartupCoverageStatus::Cancelled,
                    reason: Some(PlatformStartupCoverageReason::Cancelled),
                    items,
                    elapsed_ms: started.elapsed().as_millis() as u64,
                };
            }
            let entry = match entry {
                Ok(entry) => entry,
                Err(_) => {
                    issues.push(ScanIssue::InvalidData);
                    continue;
                }
            };
            let plist_path = entry.path();
            if !plist_path
                .extension()
                .is_some_and(|extension| extension.eq_ignore_ascii_case("plist"))
            {
                continue;
            }
            let Ok(metadata) = fs::symlink_metadata(&plist_path) else {
                issues.push(ScanIssue::InvalidData);
                continue;
            };
            if !metadata.is_file() || metadata.file_type().is_symlink() {
                continue;
            }
            let value = match Value::from_file(&plist_path) {
                Ok(value) => value,
                Err(_) => {
                    issues.push(ScanIssue::InvalidData);
                    continue;
                }
            };
            let Some(dictionary) = value.as_dictionary() else {
                issues.push(ScanIssue::InvalidData);
                continue;
            };
            if !has_launchd_identity(dictionary) {
                // Some uninstallers leave an empty launchd property list behind. It is not a
                // runnable startup item and cannot be changed safely, so keep it out of the
                // user-facing catalog while retaining partial-coverage diagnostics.
                issues.push(ScanIssue::InvalidData);
                continue;
            }
            items.push(artifact_from_dictionary(
                dictionary,
                &plist_path,
                metadata.modified().ok(),
                source,
                overrides,
                bundle_index,
            ));
        }
    }

    let reason = coverage_reason(&issues);
    PlatformStartupSourceResult {
        source_id: source.source_id.to_owned(),
        required: true,
        status: if reason.is_some() {
            PlatformStartupCoverageStatus::Partial
        } else {
            PlatformStartupCoverageStatus::Complete
        },
        reason,
        items,
        elapsed_ms: started.elapsed().as_millis() as u64,
    }
}

fn has_launchd_identity(dictionary: &Dictionary) -> bool {
    string_value(dictionary, "Label").is_some()
        || string_value(dictionary, "Program").is_some()
        || !string_array(dictionary, "ProgramArguments").is_empty()
}

fn artifact_from_dictionary(
    dictionary: &Dictionary,
    plist_path: &Path,
    modified: Option<SystemTime>,
    source: &LaunchdSource,
    overrides: Option<&BTreeMap<String, bool>>,
    bundle_index: &BundleIndex,
) -> PlatformStartupArtifact {
    let label = string_value(dictionary, "Label");
    let arguments = string_array(dictionary, "ProgramArguments");
    let program = string_value(dictionary, "Program").or_else(|| arguments.first().cloned());
    let target_path = program.as_deref().map(PathBuf::from);
    let target_presence = target_path
        .as_deref()
        .map(target_presence)
        .unwrap_or(TargetPresence::Missing);
    let target_exists = target_presence == TargetPresence::Present;
    let target_is_missing = target_presence == TargetPresence::Missing;
    let (mut owner, trust) = target_path
        .as_deref()
        .map(|target| {
            owner_from_target(
                target,
                target_exists,
                label.as_deref(),
                source.system_owned,
                bundle_index,
            )
        })
        .unwrap_or_else(|| {
            (
                unresolved_owner(),
                if source.system_owned {
                    PlatformStartupTrustState::System
                } else {
                    PlatformStartupTrustState::Unknown
                },
            )
        });
    let mut diagnostics = Vec::new();
    if let Some(bundle_identifier) = associated_bundle_identifier(dictionary) {
        // Launchd jobs can outlive their main application bundle. The explicit association keeps
        // that residual job grouped with the matching macOS background-task record, allowing the
        // surviving property list to remain discoverable after the application is uninstalled.
        owner.identity_key = Some(format!("bundle:{bundle_identifier}"));
        owner.confidence = PlatformStartupIdentityConfidence::Exact;
    }
    let display_name = owner
        .name
        .clone()
        .or_else(|| label.clone())
        .or_else(|| {
            plist_path
                .file_stem()
                .map(|value| value.to_string_lossy().into_owned())
        })
        .unwrap_or_else(|| "launchd item".to_owned());
    if label.is_none() {
        diagnostics.push(PlatformStartupDiagnosticCode::MissingIdentity);
    }
    if target_is_missing {
        diagnostics.push(PlatformStartupDiagnosticCode::MissingTarget);
    }
    if overrides.is_none() {
        diagnostics.push(PlatformStartupDiagnosticCode::StateUnavailable);
    }
    let configured_state = configured_state(dictionary, label.as_deref(), overrides);
    let target_identity = target_path
        .as_deref()
        .map(|path| path.to_string_lossy().into_owned())
        .unwrap_or_else(|| {
            label
                .clone()
                .unwrap_or_else(|| plist_path.to_string_lossy().into_owned())
        });

    PlatformStartupArtifact {
        provider_item_id: format!(
            "{}:{}",
            label.as_deref().unwrap_or("missing-label"),
            plist_path.to_string_lossy()
        ),
        source_kind: source.source_kind,
        scope: source.scope,
        triggers: launchd_triggers(dictionary, source.source_kind),
        display_name,
        configuration_path: Some(plist_path.to_path_buf()),
        target: PlatformStartupTarget {
            kind: target_kind(target_path.as_deref()),
            identity_key: target_identity,
            path: target_path.clone(),
            executable_name: target_path.as_deref().and_then(|path| {
                path.file_name()
                    .map(|value| value.to_string_lossy().into_owned())
            }),
            arguments: if arguments.is_empty() {
                Vec::new()
            } else {
                arguments.into_iter().skip(1).collect()
            },
        },
        owner,
        configured_state,
        runtime_state: PlatformStartupRuntimeState::Unknown,
        control_capability: if label.is_some() {
            source.capability
        } else {
            PlatformStartupControlCapability::ViewOnly
        },
        trust,
        modified_at_ms: modified.and_then(system_time_ms),
        diagnostics,
    }
}

fn target_presence(path: &Path) -> TargetPresence {
    if !path.is_absolute() || external_volume_is_unavailable(path) {
        return TargetPresence::Unknown;
    }
    match path.try_exists() {
        Ok(true) => TargetPresence::Present,
        Ok(false) => TargetPresence::Missing,
        Err(_) => TargetPresence::Unknown,
    }
}

fn external_volume_is_unavailable(path: &Path) -> bool {
    let Ok(relative) = path.strip_prefix("/Volumes") else {
        return false;
    };
    let Some(volume_name) = relative.components().next() else {
        return false;
    };
    let volume_root = Path::new("/Volumes").join(volume_name.as_os_str());
    !matches!(volume_root.try_exists(), Ok(true))
}

fn configured_state(
    dictionary: &Dictionary,
    label: Option<&str>,
    overrides: Option<&BTreeMap<String, bool>>,
) -> PlatformStartupConfiguredState {
    if let (Some(label), Some(overrides)) = (label, overrides) {
        if let Some(disabled) = overrides.get(label) {
            return if *disabled {
                PlatformStartupConfiguredState::Disabled
            } else {
                PlatformStartupConfiguredState::Enabled
            };
        }
    }
    match dictionary.get("Disabled").and_then(Value::as_boolean) {
        Some(true) => PlatformStartupConfiguredState::Disabled,
        Some(false) => PlatformStartupConfiguredState::Enabled,
        None if overrides.is_some() => PlatformStartupConfiguredState::Enabled,
        None => PlatformStartupConfiguredState::Unknown,
    }
}

fn associated_bundle_identifier(dictionary: &Dictionary) -> Option<String> {
    let value = dictionary.get("AssociatedBundleIdentifiers")?;
    if let Some(identifier) = value.as_string() {
        return non_empty_string(identifier);
    }
    let identifiers = value
        .as_array()?
        .iter()
        .filter_map(Value::as_string)
        .filter_map(non_empty_string)
        .collect::<Vec<_>>();
    (identifiers.len() == 1).then(|| identifiers[0].clone())
}

fn non_empty_string(value: &str) -> Option<String> {
    let value = value.trim();
    (!value.is_empty()).then(|| value.to_owned())
}

fn launchd_triggers(
    dictionary: &Dictionary,
    source_kind: PlatformStartupSourceKind,
) -> Vec<PlatformStartupTrigger> {
    let mut triggers = Vec::new();
    if dictionary
        .get("RunAtLoad")
        .and_then(Value::as_boolean)
        .unwrap_or(false)
    {
        triggers.push(if source_kind == PlatformStartupSourceKind::LaunchDaemon {
            PlatformStartupTrigger::Boot
        } else {
            PlatformStartupTrigger::UserLogon
        });
    }
    if dictionary.contains_key("KeepAlive") {
        triggers.push(PlatformStartupTrigger::KeepAlive);
    }
    if dictionary.contains_key("StartInterval") || dictionary.contains_key("StartCalendarInterval")
    {
        triggers.push(PlatformStartupTrigger::Scheduled);
    }
    if [
        "WatchPaths",
        "QueueDirectories",
        "MachServices",
        "Sockets",
        "StartOnMount",
    ]
    .into_iter()
    .any(|key| dictionary.contains_key(key))
    {
        triggers.push(PlatformStartupTrigger::Event);
    }
    if triggers.is_empty() {
        triggers.push(PlatformStartupTrigger::Unknown);
    }
    triggers.sort_unstable();
    triggers.dedup();
    triggers
}

fn owner_from_target(
    target: &Path,
    target_exists: bool,
    label: Option<&str>,
    system_candidate: bool,
    bundle_index: &BundleIndex,
) -> (PlatformStartupOwner, PlatformStartupTrustState) {
    let signature = code_signature_metadata(target, system_candidate);
    if let Some(owner) = bundle_index.resolve_owner(label, signature.team_id.as_deref()) {
        return (owner, signature.trust);
    }
    let Some(bundle) = target.ancestors().find(|candidate| {
        candidate
            .extension()
            .is_some_and(|extension| extension.eq_ignore_ascii_case("app"))
    }) else {
        let owner = PlatformStartupOwner {
            identity_key: signature
                .team_id
                .as_ref()
                .map(|team_id| format!("team:{team_id}")),
            name: target
                .file_name()
                .map(|value| value.to_string_lossy().into_owned()),
            publisher: signature.publisher,
            summary: None,
            summary_source: PlatformStartupSummarySource::Unavailable,
            version: None,
            icon_path: target_exists.then(|| target.to_path_buf()),
            confidence: if signature.team_id.is_some() {
                PlatformStartupIdentityConfidence::Strong
            } else {
                PlatformStartupIdentityConfidence::Unresolved
            },
        };
        return (owner, signature.trust);
    };
    let Ok(value) = Value::from_file(bundle.join("Contents/Info.plist")) else {
        return (unresolved_owner(), PlatformStartupTrustState::Unknown);
    };
    let Some(dictionary) = value.as_dictionary() else {
        return (unresolved_owner(), PlatformStartupTrustState::Unknown);
    };
    let identity_key = string_value(dictionary, "CFBundleIdentifier")
        .map(|bundle_id| format!("bundle:{bundle_id}"));
    let name = string_value(dictionary, "CFBundleDisplayName")
        .or_else(|| string_value(dictionary, "CFBundleName"))
        .or_else(|| {
            bundle
                .file_stem()
                .map(|value| value.to_string_lossy().into_owned())
        });
    (
        PlatformStartupOwner {
            confidence: if identity_key.is_some() {
                PlatformStartupIdentityConfidence::Exact
            } else {
                PlatformStartupIdentityConfidence::Strong
            },
            identity_key,
            name,
            publisher: signature.publisher,
            summary: None,
            summary_source: PlatformStartupSummarySource::BundleMetadata,
            version: string_value(dictionary, "CFBundleShortVersionString"),
            icon_path: Some(bundle.to_path_buf()),
        },
        signature.trust,
    )
}

fn unresolved_owner() -> PlatformStartupOwner {
    PlatformStartupOwner {
        identity_key: None,
        name: None,
        publisher: None,
        summary: None,
        summary_source: PlatformStartupSummarySource::Unavailable,
        version: None,
        icon_path: None,
        confidence: PlatformStartupIdentityConfidence::Unresolved,
    }
}

fn target_kind(path: Option<&Path>) -> PlatformStartupTargetKind {
    let Some(path) = path else {
        return PlatformStartupTargetKind::Unknown;
    };
    if path
        .extension()
        .is_some_and(|extension| extension.eq_ignore_ascii_case("app"))
        || path.ancestors().any(|candidate| {
            candidate
                .extension()
                .is_some_and(|extension| extension.eq_ignore_ascii_case("app"))
        })
    {
        return PlatformStartupTargetKind::Application;
    }
    if path.extension().is_some_and(|extension| {
        ["sh", "zsh", "bash", "py", "pl", "rb", "js"]
            .into_iter()
            .any(|value| extension.eq_ignore_ascii_case(value))
    }) {
        PlatformStartupTargetKind::Script
    } else {
        PlatformStartupTargetKind::Executable
    }
}

fn disabled_overrides(
    domain: LaunchdDomain,
    interactive_user_id: Option<u32>,
) -> Option<BTreeMap<String, bool>> {
    let target = match domain {
        LaunchdDomain::Gui => format!(
            "gui/{}",
            interactive_user_id.unwrap_or_else(|| unsafe { libc::geteuid() })
        ),
        LaunchdDomain::System => "system".to_owned(),
    };
    let output = Command::new("/bin/launchctl")
        .args(["print-disabled", &target])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8(output.stdout).ok()?;
    Some(parse_disabled_overrides(&text))
}

fn parse_disabled_overrides(text: &str) -> BTreeMap<String, bool> {
    let mut values = BTreeMap::new();
    for line in text.lines() {
        let Some((label, disabled)) = line.split_once("=>") else {
            continue;
        };
        let label = label.trim().trim_matches('"');
        let disabled = disabled.trim().trim_end_matches([';', ',']).trim();
        if label.is_empty() {
            continue;
        }
        match disabled {
            // Current launchctl versions print words while older releases used booleans.
            // Accepting both formats is required because this value is the verification source
            // immediately after an enable or disable operation.
            "true" | "disabled" => {
                values.insert(label.to_owned(), true);
            }
            "false" | "enabled" => {
                values.insert(label.to_owned(), false);
            }
            _ => {}
        }
    }
    values
}

fn coverage_reason(issues: &[ScanIssue]) -> Option<PlatformStartupCoverageReason> {
    if issues
        .iter()
        .any(|issue| matches!(issue, ScanIssue::AccessDenied))
    {
        Some(PlatformStartupCoverageReason::AccessDenied)
    } else if issues
        .iter()
        .any(|issue| matches!(issue, ScanIssue::InvalidData))
    {
        Some(PlatformStartupCoverageReason::InvalidData)
    } else if issues
        .iter()
        .any(|issue| matches!(issue, ScanIssue::StateUnavailable))
    {
        Some(PlatformStartupCoverageReason::StateUnavailable)
    } else {
        None
    }
}

fn string_value(dictionary: &Dictionary, key: &str) -> Option<String> {
    dictionary
        .get(key)
        .and_then(Value::as_string)
        .filter(|value| !value.trim().is_empty())
        .map(str::to_owned)
}

fn string_array(dictionary: &Dictionary, key: &str) -> Vec<String> {
    dictionary
        .get(key)
        .and_then(Value::as_array)
        .map(|values| {
            values
                .iter()
                .filter_map(Value::as_string)
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

fn system_time_ms(value: SystemTime) -> Option<u64> {
    value
        .duration_since(UNIX_EPOCH)
        .ok()
        .map(|duration| duration.as_millis() as u64)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn source(kind: PlatformStartupSourceKind) -> LaunchdSource {
        LaunchdSource {
            source_id: "test.launchd",
            paths: Vec::new(),
            source_kind: kind,
            scope: PlatformStartupScope::CurrentUser,
            capability: PlatformStartupControlCapability::Toggleable,
            domain: LaunchdDomain::Gui,
            system_owned: false,
        }
    }

    #[test]
    fn run_at_load_uses_scope_appropriate_trigger() {
        let mut dictionary = Dictionary::new();
        dictionary.insert("RunAtLoad".to_owned(), Value::Boolean(true));

        assert_eq!(
            launchd_triggers(&dictionary, PlatformStartupSourceKind::LaunchAgent),
            vec![PlatformStartupTrigger::UserLogon]
        );
        assert_eq!(
            launchd_triggers(&dictionary, PlatformStartupSourceKind::LaunchDaemon),
            vec![PlatformStartupTrigger::Boot]
        );
    }

    #[test]
    fn event_only_item_is_not_reported_as_login_startup() {
        let mut dictionary = Dictionary::new();
        dictionary.insert(
            "WatchPaths".to_owned(),
            Value::Array(vec![Value::String("/tmp/example".to_owned())]),
        );

        assert_eq!(
            launchd_triggers(&dictionary, PlatformStartupSourceKind::LaunchAgent),
            vec![PlatformStartupTrigger::Event]
        );
    }

    #[test]
    fn disabled_override_takes_precedence_over_plist_default() {
        let mut dictionary = Dictionary::new();
        dictionary.insert("Disabled".to_owned(), Value::Boolean(false));
        let overrides = BTreeMap::from([("com.example.agent".to_owned(), true)]);

        assert_eq!(
            configured_state(&dictionary, Some("com.example.agent"), Some(&overrides)),
            PlatformStartupConfiguredState::Disabled
        );
    }

    #[test]
    fn disabled_overrides_accept_current_launchctl_words() {
        let values = parse_disabled_overrides(
            r#"
disabled services = {
    "homebrew.mxcl.mysql@8.4" => disabled,
    "com.example.enabled" => enabled
}
"#,
        );

        assert_eq!(values.get("homebrew.mxcl.mysql@8.4"), Some(&true));
        assert_eq!(values.get("com.example.enabled"), Some(&false));
    }

    #[test]
    fn disabled_overrides_keep_legacy_boolean_compatibility() {
        let values = parse_disabled_overrides(
            r#"
disabled services = {
    "com.example.disabled" => true;
    "com.example.enabled" => false;
}
"#,
        );

        assert_eq!(values.get("com.example.disabled"), Some(&true));
        assert_eq!(values.get("com.example.enabled"), Some(&false));
    }

    #[test]
    fn missing_target_remains_visible_with_diagnostics() {
        let mut dictionary = Dictionary::new();
        dictionary.insert(
            "Label".to_owned(),
            Value::String("com.example.missing".to_owned()),
        );

        let artifact = artifact_from_dictionary(
            &dictionary,
            Path::new("/Library/LaunchAgents/com.example.missing.plist"),
            None,
            &source(PlatformStartupSourceKind::LaunchAgent),
            Some(&BTreeMap::new()),
            &BundleIndex::default(),
        );

        assert_eq!(artifact.display_name, "com.example.missing");
        assert_eq!(
            artifact.configuration_path.as_deref(),
            Some(Path::new("/Library/LaunchAgents/com.example.missing.plist"))
        );
        assert!(artifact
            .diagnostics
            .contains(&PlatformStartupDiagnosticCode::MissingTarget));
        assert_eq!(artifact.target.kind, PlatformStartupTargetKind::Unknown);
    }

    #[test]
    fn missing_launchd_label_is_visible_but_not_manageable() {
        let mut dictionary = Dictionary::new();
        dictionary.insert(
            "Program".to_owned(),
            Value::String("/usr/bin/example".to_owned()),
        );

        let artifact = artifact_from_dictionary(
            &dictionary,
            Path::new("/Library/LaunchAgents/example.plist"),
            None,
            &source(PlatformStartupSourceKind::LaunchAgent),
            Some(&BTreeMap::new()),
            &BundleIndex::default(),
        );

        assert!(artifact
            .diagnostics
            .contains(&PlatformStartupDiagnosticCode::MissingIdentity));
        assert_eq!(
            artifact.control_capability,
            PlatformStartupControlCapability::ViewOnly
        );
    }

    #[test]
    fn explicit_bundle_association_groups_a_residual_launchd_item() {
        let mut dictionary = Dictionary::new();
        dictionary.insert(
            "Label".to_owned(),
            Value::String("com.macpaw.CleanMyMac5.Updater".to_owned()),
        );
        dictionary.insert(
            "Program".to_owned(),
            Value::String("/Applications/CleanMyMac_5.app/Contents/MacOS/CleanMyMac_5".to_owned()),
        );
        dictionary.insert(
            "AssociatedBundleIdentifiers".to_owned(),
            Value::String("com.macpaw.CleanMyMac5".to_owned()),
        );

        let artifact = artifact_from_dictionary(
            &dictionary,
            Path::new("/Library/LaunchAgents/com.macpaw.CleanMyMac5.Updater.plist"),
            None,
            &source(PlatformStartupSourceKind::LaunchAgent),
            Some(&BTreeMap::new()),
            &BundleIndex::default(),
        );

        assert_eq!(
            artifact.owner.identity_key.as_deref(),
            Some("bundle:com.macpaw.CleanMyMac5")
        );
        assert!(artifact
            .diagnostics
            .contains(&PlatformStartupDiagnosticCode::MissingTarget));
        assert_eq!(
            artifact.owner.confidence,
            PlatformStartupIdentityConfidence::Exact
        );
    }

    #[test]
    fn unresolved_bundle_association_keeps_an_existing_target_safe() {
        let mut dictionary = Dictionary::new();
        dictionary.insert(
            "Label".to_owned(),
            Value::String("com.example.external.helper".to_owned()),
        );
        dictionary.insert("Program".to_owned(), Value::String("/bin/sh".to_owned()));
        dictionary.insert(
            "AssociatedBundleIdentifiers".to_owned(),
            Value::String("com.example.ExternalApplication".to_owned()),
        );

        let artifact = artifact_from_dictionary(
            &dictionary,
            Path::new("/Library/LaunchAgents/com.example.external.helper.plist"),
            None,
            &source(PlatformStartupSourceKind::LaunchAgent),
            Some(&BTreeMap::new()),
            &BundleIndex::default(),
        );

        assert!(!artifact
            .diagnostics
            .contains(&PlatformStartupDiagnosticCode::MissingTarget));
        assert_eq!(
            artifact.owner.identity_key.as_deref(),
            Some("bundle:com.example.ExternalApplication")
        );
    }

    #[test]
    fn relative_program_target_is_not_claimed_as_missing() {
        let mut dictionary = Dictionary::new();
        dictionary.insert(
            "Label".to_owned(),
            Value::String("com.example.relative.helper".to_owned()),
        );
        dictionary.insert(
            "ProgramArguments".to_owned(),
            Value::Array(vec![
                Value::String("sh".to_owned()),
                Value::String("-c".to_owned()),
                Value::String("true".to_owned()),
            ]),
        );

        let artifact = artifact_from_dictionary(
            &dictionary,
            Path::new("/Library/LaunchAgents/com.example.relative.helper.plist"),
            None,
            &source(PlatformStartupSourceKind::LaunchAgent),
            Some(&BTreeMap::new()),
            &BundleIndex::default(),
        );

        assert!(!artifact
            .diagnostics
            .contains(&PlatformStartupDiagnosticCode::MissingTarget));
    }

    #[test]
    fn unavailable_external_volume_is_not_claimed_as_a_missing_target() {
        let path = PathBuf::from(format!(
            "/Volumes/MangoDiskUnavailableVolume{}/agent",
            std::process::id()
        ));

        assert_eq!(target_presence(&path), TargetPresence::Unknown);
    }

    #[test]
    fn orphan_removal_deletes_only_the_preflighted_property_list() {
        let directory = std::env::temp_dir().join(format!(
            "mangodisk-startup-orphan-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("system time must be available")
                .as_nanos()
        ));
        fs::create_dir(&directory).expect("fixture directory must be created");
        let configuration_path = directory.join("com.example.removed.plist");
        let mut dictionary = Dictionary::new();
        dictionary.insert(
            "Label".to_owned(),
            Value::String("com.example.removed".to_owned()),
        );
        Value::Dictionary(dictionary.clone())
            .to_file_xml(&configuration_path)
            .expect("fixture property list must be written");
        let mut launchd_source = source(PlatformStartupSourceKind::LaunchAgent);
        launchd_source.paths = vec![directory.clone()];
        let artifact = artifact_from_dictionary(
            &dictionary,
            &configuration_path,
            None,
            &launchd_source,
            Some(&BTreeMap::new()),
            &BundleIndex::default(),
        );

        let result = remove_orphaned_item(&launchd_source, &artifact)
            .expect("the orphaned property list should be removed");

        assert!(result.verified);
        assert_eq!(
            result.configured_state,
            PlatformStartupConfiguredState::NotApplicable
        );
        assert!(!configuration_path.exists());
        fs::remove_dir(directory).expect("fixture directory must be removed");
    }

    #[test]
    fn ambiguous_bundle_associations_do_not_merge_unrelated_items() {
        let mut dictionary = Dictionary::new();
        dictionary.insert(
            "AssociatedBundleIdentifiers".to_owned(),
            Value::Array(vec![
                Value::String("com.example.First".to_owned()),
                Value::String("com.example.Second".to_owned()),
            ]),
        );

        assert_eq!(associated_bundle_identifier(&dictionary), None);
    }

    #[test]
    fn empty_launchd_property_lists_are_not_startup_items() {
        assert!(!has_launchd_identity(&Dictionary::new()));

        let mut labeled = Dictionary::new();
        labeled.insert(
            "Label".to_owned(),
            Value::String("com.example.agent".to_owned()),
        );
        assert!(has_launchd_identity(&labeled));

        let mut argument_only = Dictionary::new();
        argument_only.insert(
            "ProgramArguments".to_owned(),
            Value::Array(vec![Value::String("/usr/bin/example".to_owned())]),
        );
        assert!(has_launchd_identity(&argument_only));
    }
}

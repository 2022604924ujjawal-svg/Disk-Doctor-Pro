use std::path::{Path, PathBuf};
use std::time::Instant;

use winreg::{
    enums::{
        HKEY_CLASSES_ROOT, HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE, KEY_READ, KEY_WOW64_32KEY,
        KEY_WOW64_64KEY,
    },
    types::FromRegValue,
    RegKey, HKEY,
};

use crate::{
    PlatformCancellation, PlatformStartupArtifact, PlatformStartupConfiguredState,
    PlatformStartupControlCapability, PlatformStartupCoverageReason, PlatformStartupCoverageStatus,
    PlatformStartupDiagnosticCode, PlatformStartupIdentityConfidence, PlatformStartupOwner,
    PlatformStartupRuntimeState, PlatformStartupScope, PlatformStartupSourceKind,
    PlatformStartupSourceResult, PlatformStartupSummarySource, PlatformStartupTarget,
    PlatformStartupTrigger,
};

use super::{
    metadata::{file_version_metadata, startup_trust},
    registry::{split_command_line, target_kind},
};
use crate::windows::path_identity;

const SOURCE_ID: &str = "windows.advanced_autoruns";

#[derive(Clone, Copy)]
struct RegistryValueSource {
    root: HKEY,
    root_name: &'static str,
    key_path: &'static str,
    value_names: &'static [&'static str],
    view: u32,
    view_name: &'static str,
    scope: PlatformStartupScope,
    trigger: PlatformStartupTrigger,
}

#[derive(Clone, Copy)]
struct RegistryValueNameSource {
    root: HKEY,
    root_name: &'static str,
    key_path: &'static str,
    view: u32,
    view_name: &'static str,
    scope: PlatformStartupScope,
}

pub(super) fn scan(cancellation: &PlatformCancellation) -> PlatformStartupSourceResult {
    let started = Instant::now();
    let mut items = Vec::new();
    let mut reason = None;
    for source in sources() {
        if cancellation.is_cancelled() {
            return result(
                items,
                PlatformStartupCoverageStatus::Cancelled,
                Some(PlatformStartupCoverageReason::Cancelled),
                started,
            );
        }
        if let Err(source_reason) = scan_source(source, &mut items) {
            reason.get_or_insert(source_reason);
        }
    }
    for source in subkey_sources() {
        if cancellation.is_cancelled() {
            return result(
                items,
                PlatformStartupCoverageStatus::Cancelled,
                Some(PlatformStartupCoverageReason::Cancelled),
                started,
            );
        }
        if let Err(source_reason) = scan_subkeys(source, &mut items) {
            reason.get_or_insert(source_reason);
        }
    }
    for source in value_name_sources() {
        if cancellation.is_cancelled() {
            return result(
                items,
                PlatformStartupCoverageStatus::Cancelled,
                Some(PlatformStartupCoverageReason::Cancelled),
                started,
            );
        }
        if let Err(source_reason) = scan_value_names(source, &mut items) {
            reason.get_or_insert(source_reason);
        }
    }
    // WMI permanent subscriptions and Winsock providers require dedicated structured
    // readers. Preserve a truthful partial status until those parsers and fixtures exist.
    reason.get_or_insert(PlatformStartupCoverageReason::NotImplemented);
    result(
        items,
        if reason.is_some() {
            PlatformStartupCoverageStatus::Partial
        } else {
            PlatformStartupCoverageStatus::Complete
        },
        reason,
        started,
    )
}

fn result(
    items: Vec<PlatformStartupArtifact>,
    status: PlatformStartupCoverageStatus,
    reason: Option<PlatformStartupCoverageReason>,
    started: Instant,
) -> PlatformStartupSourceResult {
    PlatformStartupSourceResult {
        source_id: SOURCE_ID.to_string(),
        required: false,
        status,
        reason,
        items,
        elapsed_ms: started.elapsed().as_millis() as u64,
    }
}

fn sources() -> [RegistryValueSource; 16] {
    [
        source(
            (HKEY_LOCAL_MACHINE, "hklm"),
            r"SOFTWARE\Microsoft\Windows NT\CurrentVersion\Winlogon",
            &["Shell", "Userinit", "Taskman"],
            (KEY_WOW64_64KEY, "64"),
            PlatformStartupScope::Machine,
            PlatformStartupTrigger::ShellLoad,
        ),
        source(
            (HKEY_CURRENT_USER, "hkcu"),
            r"SOFTWARE\Microsoft\Windows NT\CurrentVersion\Winlogon",
            &["Shell", "Userinit", "Taskman"],
            (KEY_WOW64_64KEY, "64"),
            PlatformStartupScope::CurrentUser,
            PlatformStartupTrigger::ShellLoad,
        ),
        source(
            (HKEY_LOCAL_MACHINE, "hklm"),
            r"SOFTWARE\Microsoft\Windows NT\CurrentVersion\Windows",
            &["AppInit_DLLs", "Load", "Run"],
            (KEY_WOW64_64KEY, "64"),
            PlatformStartupScope::Machine,
            PlatformStartupTrigger::UserLogon,
        ),
        source(
            (HKEY_LOCAL_MACHINE, "hklm"),
            r"SOFTWARE\Microsoft\Windows NT\CurrentVersion\Windows",
            &["AppInit_DLLs", "Load", "Run"],
            (KEY_WOW64_32KEY, "32"),
            PlatformStartupScope::Machine,
            PlatformStartupTrigger::UserLogon,
        ),
        source(
            (HKEY_CURRENT_USER, "hkcu"),
            r"SOFTWARE\Microsoft\Windows NT\CurrentVersion\Windows",
            &["Load", "Run"],
            (KEY_WOW64_64KEY, "64"),
            PlatformStartupScope::CurrentUser,
            PlatformStartupTrigger::UserLogon,
        ),
        source(
            (HKEY_LOCAL_MACHINE, "hklm"),
            r"SYSTEM\CurrentControlSet\Control\Session Manager",
            &["BootExecute"],
            (KEY_WOW64_64KEY, "64"),
            PlatformStartupScope::System,
            PlatformStartupTrigger::Boot,
        ),
        source(
            (HKEY_LOCAL_MACHINE, "hklm"),
            r"SYSTEM\CurrentControlSet\Control\Lsa",
            &[
                "Authentication Packages",
                "Notification Packages",
                "Security Packages",
            ],
            (KEY_WOW64_64KEY, "64"),
            PlatformStartupScope::System,
            PlatformStartupTrigger::Boot,
        ),
        source(
            (HKEY_LOCAL_MACHINE, "hklm"),
            r"SYSTEM\CurrentControlSet\Control\SecurityProviders",
            &["SecurityProviders"],
            (KEY_WOW64_64KEY, "64"),
            PlatformStartupScope::System,
            PlatformStartupTrigger::Boot,
        ),
        source(
            (HKEY_LOCAL_MACHINE, "hklm"),
            r"SYSTEM\CurrentControlSet\Control\Session Manager\KnownDLLs",
            &[""],
            (KEY_WOW64_64KEY, "64"),
            PlatformStartupScope::System,
            PlatformStartupTrigger::Boot,
        ),
        source(
            (HKEY_LOCAL_MACHINE, "hklm"),
            r"SOFTWARE\Microsoft\Windows\CurrentVersion\Explorer\ShellExecuteHooks",
            &[""],
            (KEY_WOW64_64KEY, "64"),
            PlatformStartupScope::Machine,
            PlatformStartupTrigger::ShellLoad,
        ),
        source(
            (HKEY_LOCAL_MACHINE, "hklm"),
            r"SOFTWARE\Microsoft\Windows\CurrentVersion\ShellServiceObjectDelayLoad",
            &[""],
            (KEY_WOW64_64KEY, "64"),
            PlatformStartupScope::Machine,
            PlatformStartupTrigger::ShellLoad,
        ),
        source(
            (HKEY_CURRENT_USER, "hkcu"),
            r"SOFTWARE\Microsoft\Windows\CurrentVersion\ShellServiceObjectDelayLoad",
            &[""],
            (KEY_WOW64_64KEY, "64"),
            PlatformStartupScope::CurrentUser,
            PlatformStartupTrigger::ShellLoad,
        ),
        source(
            (HKEY_LOCAL_MACHINE, "hklm"),
            r"SOFTWARE\Microsoft\Windows NT\CurrentVersion\Drivers32",
            &[""],
            (KEY_WOW64_64KEY, "64"),
            PlatformStartupScope::Machine,
            PlatformStartupTrigger::ApplicationLaunch,
        ),
        source(
            (HKEY_LOCAL_MACHINE, "hklm"),
            r"SOFTWARE\Microsoft\Windows NT\CurrentVersion\Drivers32",
            &[""],
            (KEY_WOW64_32KEY, "32"),
            PlatformStartupScope::Machine,
            PlatformStartupTrigger::ApplicationLaunch,
        ),
        source(
            (HKEY_CURRENT_USER, "hkcu"),
            r"SOFTWARE\Microsoft\Windows NT\CurrentVersion\Drivers32",
            &[""],
            (KEY_WOW64_64KEY, "64"),
            PlatformStartupScope::CurrentUser,
            PlatformStartupTrigger::ApplicationLaunch,
        ),
        source(
            (HKEY_CURRENT_USER, "hkcu"),
            r"SOFTWARE\Microsoft\Windows NT\CurrentVersion\Drivers32",
            &[""],
            (KEY_WOW64_32KEY, "32"),
            PlatformStartupScope::CurrentUser,
            PlatformStartupTrigger::ApplicationLaunch,
        ),
    ]
}

fn value_name_sources() -> [RegistryValueNameSource; 4] {
    [
        value_name_source(
            (HKEY_LOCAL_MACHINE, "hklm"),
            (KEY_WOW64_64KEY, "64"),
            PlatformStartupScope::Machine,
        ),
        value_name_source(
            (HKEY_LOCAL_MACHINE, "hklm"),
            (KEY_WOW64_32KEY, "32"),
            PlatformStartupScope::Machine,
        ),
        value_name_source(
            (HKEY_CURRENT_USER, "hkcu"),
            (KEY_WOW64_64KEY, "64"),
            PlatformStartupScope::CurrentUser,
        ),
        value_name_source(
            (HKEY_CURRENT_USER, "hkcu"),
            (KEY_WOW64_32KEY, "32"),
            PlatformStartupScope::CurrentUser,
        ),
    ]
}

const fn value_name_source(
    root: (HKEY, &'static str),
    view: (u32, &'static str),
    scope: PlatformStartupScope,
) -> RegistryValueNameSource {
    RegistryValueNameSource {
        root: root.0,
        root_name: root.1,
        key_path: r"SOFTWARE\Microsoft\Windows\CurrentVersion\Shell Extensions\Approved",
        view: view.0,
        view_name: view.1,
        scope,
    }
}

#[derive(Clone, Copy)]
struct SubkeyValueSource {
    root: HKEY,
    root_name: &'static str,
    base_key_path: &'static str,
    value_name: &'static str,
    view: u32,
    view_name: &'static str,
    scope: PlatformStartupScope,
    trigger: PlatformStartupTrigger,
    driver_filter: bool,
}

#[derive(Clone, Copy)]
struct ArtifactLocation<'a> {
    root_name: &'a str,
    view: u32,
    view_name: &'a str,
    key_path: &'a str,
    scope: PlatformStartupScope,
    trigger: PlatformStartupTrigger,
}

fn subkey_sources() -> [SubkeyValueSource; 6] {
    [
        subkey_source(
            r"SOFTWARE\Microsoft\Windows NT\CurrentVersion\Image File Execution Options",
            "Debugger",
            KEY_WOW64_64KEY,
            "64",
            PlatformStartupTrigger::ShellLoad,
            false,
        ),
        subkey_source(
            r"SOFTWARE\Microsoft\Windows NT\CurrentVersion\Image File Execution Options",
            "Debugger",
            KEY_WOW64_32KEY,
            "32",
            PlatformStartupTrigger::ShellLoad,
            false,
        ),
        subkey_source(
            r"SOFTWARE\Microsoft\Windows NT\CurrentVersion\Winlogon\Notify",
            "DLLName",
            KEY_WOW64_64KEY,
            "64",
            PlatformStartupTrigger::ShellLoad,
            false,
        ),
        subkey_source(
            r"SYSTEM\CurrentControlSet\Control\Print\Monitors",
            "Driver",
            KEY_WOW64_64KEY,
            "64",
            PlatformStartupTrigger::Boot,
            false,
        ),
        subkey_source(
            r"SYSTEM\CurrentControlSet\Services",
            "ImagePath",
            KEY_WOW64_64KEY,
            "64",
            PlatformStartupTrigger::Boot,
            true,
        ),
        subkey_source(
            r"SOFTWARE\Microsoft\Windows\CurrentVersion\Explorer\Browser Helper Objects",
            "",
            KEY_WOW64_64KEY,
            "64",
            PlatformStartupTrigger::ShellLoad,
            false,
        ),
    ]
}

const fn subkey_source(
    base_key_path: &'static str,
    value_name: &'static str,
    view: u32,
    view_name: &'static str,
    trigger: PlatformStartupTrigger,
    driver_filter: bool,
) -> SubkeyValueSource {
    SubkeyValueSource {
        root: HKEY_LOCAL_MACHINE,
        root_name: "hklm",
        base_key_path,
        value_name,
        view,
        view_name,
        scope: PlatformStartupScope::System,
        trigger,
        driver_filter,
    }
}

fn scan_subkeys(
    source: SubkeyValueSource,
    items: &mut Vec<PlatformStartupArtifact>,
) -> Result<(), PlatformStartupCoverageReason> {
    let root = RegKey::predef(source.root);
    let key = match root.open_subkey_with_flags(source.base_key_path, KEY_READ | source.view) {
        Ok(key) => key,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::PermissionDenied => {
            return Err(PlatformStartupCoverageReason::AccessDenied);
        }
        Err(_) => return Err(PlatformStartupCoverageReason::InvalidData),
    };
    for child_name in key.enum_keys() {
        let child_name = child_name.map_err(|_| PlatformStartupCoverageReason::InvalidData)?;
        let Ok(child) = key.open_subkey_with_flags(&child_name, KEY_READ | source.view) else {
            continue;
        };
        if source.driver_filter && !is_automatic_driver(&child) {
            continue;
        }
        let full_key_path = format!(r"{}\{}", source.base_key_path, child_name);
        if source.value_name.is_empty() {
            push_command_at(
                ArtifactLocation {
                    root_name: source.root_name,
                    view: source.view,
                    view_name: source.view_name,
                    key_path: &full_key_path,
                    scope: source.scope,
                    trigger: source.trigger,
                },
                &child_name,
                0,
                &child_name,
                &child_name,
                items,
            );
            continue;
        }
        let Ok(raw) = child.get_raw_value(source.value_name) else {
            continue;
        };
        if let Ok(command) = String::from_reg_value(&raw) {
            for (index, part) in split_multi_value(&command).into_iter().enumerate() {
                push_command_at(
                    ArtifactLocation {
                        root_name: source.root_name,
                        view: source.view,
                        view_name: source.view_name,
                        key_path: &full_key_path,
                        scope: source.scope,
                        trigger: source.trigger,
                    },
                    source.value_name,
                    index,
                    part,
                    &child_name,
                    items,
                );
            }
        }
    }
    Ok(())
}

fn is_automatic_driver(key: &RegKey) -> bool {
    let service_type = key.get_value::<u32, _>("Type").unwrap_or_default();
    let start_type = key.get_value::<u32, _>("Start").unwrap_or(u32::MAX);
    service_type & 0x03 != 0 && start_type <= 2
}

const fn source(
    root: (HKEY, &'static str),
    key_path: &'static str,
    value_names: &'static [&'static str],
    view: (u32, &'static str),
    scope: PlatformStartupScope,
    trigger: PlatformStartupTrigger,
) -> RegistryValueSource {
    RegistryValueSource {
        root: root.0,
        root_name: root.1,
        key_path,
        value_names,
        view: view.0,
        view_name: view.1,
        scope,
        trigger,
    }
}

fn scan_source(
    source: RegistryValueSource,
    items: &mut Vec<PlatformStartupArtifact>,
) -> Result<(), PlatformStartupCoverageReason> {
    let key = match RegKey::predef(source.root)
        .open_subkey_with_flags(source.key_path, KEY_READ | source.view)
    {
        Ok(key) => key,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::PermissionDenied => {
            return Err(PlatformStartupCoverageReason::AccessDenied);
        }
        Err(_) => return Err(PlatformStartupCoverageReason::InvalidData),
    };

    if source.value_names == [""] {
        for value in key.enum_values() {
            let Ok((value_name, raw)) = value else {
                return Err(PlatformStartupCoverageReason::InvalidData);
            };
            if let Ok(command) = String::from_reg_value(&raw) {
                push_commands(source, &value_name, &command, items);
            }
        }
        return Ok(());
    }

    for value_name in source.value_names {
        let Ok(raw) = key.get_raw_value(value_name) else {
            continue;
        };
        if let Ok(command) = String::from_reg_value(&raw) {
            push_commands(source, value_name, &command, items);
        } else if let Ok(commands) = Vec::<String>::from_reg_value(&raw) {
            for (index, command) in commands.iter().enumerate() {
                push_command(source, value_name, index, command, items);
            }
        }
    }
    Ok(())
}

fn scan_value_names(
    source: RegistryValueNameSource,
    items: &mut Vec<PlatformStartupArtifact>,
) -> Result<(), PlatformStartupCoverageReason> {
    let key = match RegKey::predef(source.root)
        .open_subkey_with_flags(source.key_path, KEY_READ | source.view)
    {
        Ok(key) => key,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::PermissionDenied => {
            return Err(PlatformStartupCoverageReason::AccessDenied);
        }
        Err(_) => return Err(PlatformStartupCoverageReason::InvalidData),
    };
    for (index, value) in key.enum_values().enumerate() {
        let (clsid, raw) = value.map_err(|_| PlatformStartupCoverageReason::InvalidData)?;
        if !looks_like_clsid(&clsid) {
            continue;
        }
        let label = String::from_reg_value(&raw)
            .ok()
            .filter(|value| !value.trim().is_empty())
            .unwrap_or_else(|| clsid.clone());
        push_command_at(
            ArtifactLocation {
                root_name: source.root_name,
                view: source.view,
                view_name: source.view_name,
                key_path: source.key_path,
                scope: source.scope,
                trigger: PlatformStartupTrigger::ShellLoad,
            },
            &clsid,
            index,
            &clsid,
            &label,
            items,
        );
    }
    Ok(())
}

fn push_commands(
    source: RegistryValueSource,
    value_name: &str,
    command: &str,
    items: &mut Vec<PlatformStartupArtifact>,
) {
    for (index, part) in split_multi_value(command).into_iter().enumerate() {
        push_command(source, value_name, index, part, items);
    }
}

fn push_command(
    source: RegistryValueSource,
    value_name: &str,
    index: usize,
    command: &str,
    items: &mut Vec<PlatformStartupArtifact>,
) {
    push_command_at(
        ArtifactLocation {
            root_name: source.root_name,
            view: source.view,
            view_name: source.view_name,
            key_path: source.key_path,
            scope: source.scope,
            trigger: source.trigger,
        },
        value_name,
        index,
        command,
        value_name,
        items,
    );
}

fn push_command_at(
    location: ArtifactLocation<'_>,
    value_name: &str,
    index: usize,
    command: &str,
    display_label: &str,
    items: &mut Vec<PlatformStartupArtifact>,
) {
    let command = command.trim();
    if command.is_empty() {
        return;
    }
    let expanded = expand_environment(command);
    let command_parts = split_command_line(&expanded);
    let target_path = resolve_target_path(&expanded, command_parts.first(), location.view);
    let metadata = target_path.as_deref().and_then(file_version_metadata);
    let executable_name = target_path
        .as_deref()
        .and_then(Path::file_name)
        .and_then(|name| name.to_str())
        .map(ToOwned::to_owned)
        .or_else(|| {
            Path::new(&expanded)
                .file_name()?
                .to_str()
                .map(ToOwned::to_owned)
        });
    let display_name = metadata
        .as_ref()
        .and_then(|value| value.product_name.clone().or(value.description.clone()))
        .or_else(|| executable_name.clone())
        .unwrap_or_else(|| display_label.to_string());
    let identity_key = target_path
        .as_deref()
        .map(path_identity::comparison_key)
        .unwrap_or_else(|| expanded.to_lowercase());
    let missing = target_path.as_deref().is_some_and(|path| !path.exists());
    let summary = metadata
        .as_ref()
        .and_then(|value| value.description.clone());
    let publisher = metadata
        .as_ref()
        .and_then(|value| value.company_name.clone());
    let version = metadata
        .as_ref()
        .and_then(|value| value.product_version.clone());
    let summary_source = if metadata.is_some() {
        PlatformStartupSummarySource::VersionInfo
    } else {
        PlatformStartupSummarySource::SourceLabel
    };

    items.push(PlatformStartupArtifact {
        provider_item_id: format!(
            "{}|{}|{}|{}|{}|{}",
            location.root_name, location.view_name, location.key_path, value_name, index, expanded
        ),
        source_kind: PlatformStartupSourceKind::AdvancedAutoRun,
        scope: location.scope,
        triggers: vec![location.trigger],
        display_name,
        configuration_path: None,
        target: PlatformStartupTarget {
            kind: target_kind(target_path.as_deref()),
            identity_key,
            path: target_path.clone(),
            executable_name,
            arguments: command_parts.into_iter().skip(1).collect(),
        },
        owner: PlatformStartupOwner {
            identity_key: None,
            name: metadata
                .as_ref()
                .and_then(|value| value.product_name.clone()),
            publisher,
            summary,
            summary_source,
            version,
            icon_path: None,
            confidence: if metadata.is_some() {
                PlatformStartupIdentityConfidence::Strong
            } else {
                PlatformStartupIdentityConfidence::Unresolved
            },
        },
        configured_state: PlatformStartupConfiguredState::Enabled,
        runtime_state: PlatformStartupRuntimeState::Unknown,
        control_capability: PlatformStartupControlCapability::ViewOnly,
        trust: startup_trust(
            target_path.as_deref(),
            location.scope == PlatformStartupScope::System,
        ),
        modified_at_ms: None,
        diagnostics: if missing {
            vec![PlatformStartupDiagnosticCode::MissingTarget]
        } else {
            Vec::new()
        },
    });
}

fn split_multi_value(value: &str) -> Vec<&str> {
    value
        .split([';', ','])
        .map(str::trim)
        .filter(|part| !part.is_empty())
        .collect()
}

fn expand_environment(value: &str) -> String {
    let mut expanded = value.to_string();
    for (name, replacement) in std::env::vars() {
        let token = format!("%{name}%");
        if expanded
            .to_ascii_lowercase()
            .contains(&token.to_ascii_lowercase())
        {
            expanded = replace_ascii_case_insensitive(&expanded, &token, &replacement);
        }
    }
    expanded
}

fn replace_ascii_case_insensitive(value: &str, needle: &str, replacement: &str) -> String {
    let lowercase = value.to_ascii_lowercase();
    let needle = needle.to_ascii_lowercase();
    let mut output = String::new();
    let mut cursor = 0;
    while let Some(relative) = lowercase[cursor..].find(&needle) {
        let start = cursor + relative;
        output.push_str(&value[cursor..start]);
        output.push_str(replacement);
        cursor = start + needle.len();
    }
    output.push_str(&value[cursor..]);
    output
}

fn resolve_target_path(value: &str, first_argument: Option<&String>, view: u32) -> Option<PathBuf> {
    if looks_like_clsid(value) {
        return resolve_clsid_server(value, view);
    }
    let whole_path = normalize_windows_system_path(value);
    if whole_path.exists() {
        return Some(whole_path);
    }
    let argument = first_argument.map(String::as_str).unwrap_or(value);
    if looks_like_clsid(argument) {
        return resolve_clsid_server(argument, view);
    }
    let candidate = normalize_windows_system_path(argument);
    if candidate.is_absolute() || argument.contains(['\\', '/']) {
        return Some(candidate);
    }
    resolve_system_library(argument)
}

fn normalize_windows_system_path(value: &str) -> PathBuf {
    let trimmed = value.trim().trim_matches('"');
    let lowercase = trimmed.to_ascii_lowercase();
    let Some(system_root) = std::env::var_os("SystemRoot") else {
        return PathBuf::from(trimmed);
    };
    if lowercase.starts_with(r"\systemroot\") {
        return PathBuf::from(system_root).join(&trimmed[12..]);
    }
    if lowercase.starts_with(r"system32\") {
        return PathBuf::from(system_root).join(trimmed);
    }
    PathBuf::from(trimmed)
}

fn resolve_system_library(value: &str) -> Option<PathBuf> {
    let system_root = std::env::var_os("SystemRoot")?;
    let directory = PathBuf::from(system_root).join("System32");
    let candidate = directory.join(value);
    if candidate.exists() {
        return Some(candidate);
    }
    if Path::new(value).extension().is_none() {
        let dll = directory.join(format!("{value}.dll"));
        if dll.exists() {
            return Some(dll);
        }
    }
    None
}

fn looks_like_clsid(value: &str) -> bool {
    let value = value.trim();
    value.len() == 38 && value.starts_with('{') && value.ends_with('}')
}

fn resolve_clsid_server(clsid: &str, view: u32) -> Option<PathBuf> {
    let classes = RegKey::predef(HKEY_CLASSES_ROOT);
    let key = classes
        .open_subkey_with_flags(
            format!(r"CLSID\{}\InprocServer32", clsid.trim()),
            KEY_READ | view,
        )
        .ok()?;
    let server = key.get_value::<String, _>("").ok()?;
    let expanded = expand_environment(&server);
    Some(normalize_windows_system_path(&expanded))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_multi_value_without_empty_entries() {
        assert_eq!(
            split_multi_value("alpha.dll, beta.dll;;gamma.dll"),
            vec!["alpha.dll", "beta.dll", "gamma.dll"]
        );
    }

    #[test]
    fn replacement_ignores_environment_token_case() {
        assert_eq!(
            replace_ascii_case_insensitive(r"%systemroot%\tool.exe", "%SystemRoot%", r"C:\Windows"),
            r"C:\Windows\tool.exe"
        );
    }

    #[test]
    fn quoted_command_uses_the_executable_instead_of_the_full_command() {
        let command = r#""C:\Windows\System32\notepad.exe" --fixture"#;
        let parts = split_command_line(command);
        let path = resolve_target_path(command, parts.first(), KEY_WOW64_64KEY)
            .expect("the quoted executable path must be resolved");

        assert!(path.ends_with(r"System32\notepad.exe"));
        assert_eq!(parts[1], "--fixture");
    }

    #[test]
    fn kernel_system_root_path_is_normalized() {
        let path = normalize_windows_system_path(r"\SystemRoot\System32\drivers\fixture.sys");

        assert!(path.is_absolute());
        assert!(path.ends_with(r"System32\drivers\fixture.sys"));
    }
}

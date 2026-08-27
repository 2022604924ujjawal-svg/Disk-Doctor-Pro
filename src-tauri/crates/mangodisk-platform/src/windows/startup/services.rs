use std::mem::size_of;
use std::path::{Path, PathBuf};
use std::slice;
use std::time::Instant;

use windows::Win32::System::Services::{
    CloseServiceHandle, EnumServicesStatusExW, OpenSCManagerW, OpenServiceW, QueryServiceConfig2W,
    QueryServiceConfigW, ENUM_SERVICE_STATUS_PROCESSW, QUERY_SERVICE_CONFIGW, SC_ENUM_PROCESS_INFO,
    SC_MANAGER_ENUMERATE_SERVICE, SERVICE_AUTO_START, SERVICE_CONFIG_DESCRIPTION,
    SERVICE_DESCRIPTIONW, SERVICE_DISABLED, SERVICE_QUERY_CONFIG, SERVICE_QUERY_STATUS,
    SERVICE_RUNNING, SERVICE_START_TYPE, SERVICE_STATE_ALL, SERVICE_WIN32,
};
use windows_core::PCWSTR;

use crate::{
    PlatformCancellation, PlatformStartupArtifact, PlatformStartupConfiguredState,
    PlatformStartupControlCapability, PlatformStartupCoverageReason, PlatformStartupCoverageStatus,
    PlatformStartupDiagnosticCode, PlatformStartupIdentityConfidence, PlatformStartupOwner,
    PlatformStartupRuntimeState, PlatformStartupScope, PlatformStartupSourceKind,
    PlatformStartupSourceResult, PlatformStartupSummarySource, PlatformStartupTarget,
    PlatformStartupTargetKind, PlatformStartupTrigger,
};

use super::metadata::{file_version_metadata, startup_trust};
use super::registry::{
    expand_environment_variables, modified_at_ms, normalized_path, split_command_line,
};

pub(super) fn scan(cancellation: &PlatformCancellation) -> PlatformStartupSourceResult {
    let started = Instant::now();
    let manager = match unsafe {
        OpenSCManagerW(PCWSTR::null(), PCWSTR::null(), SC_MANAGER_ENUMERATE_SERVICE)
    } {
        Ok(manager) => manager,
        Err(error) => {
            return result(
                Vec::new(),
                PlatformStartupCoverageStatus::Unavailable,
                Some(coverage_reason(&error)),
                started,
            );
        }
    };
    let enumeration = enumerate_services(manager);
    let (services, mut partial_reason) = match enumeration {
        Ok(services) => (services, None),
        Err(reason) => {
            unsafe {
                let _ = CloseServiceHandle(manager);
            }
            return result(
                Vec::new(),
                PlatformStartupCoverageStatus::Unavailable,
                Some(reason),
                started,
            );
        }
    };
    let mut items = Vec::new();
    for service in services {
        if cancellation.is_cancelled() {
            unsafe {
                let _ = CloseServiceHandle(manager);
            }
            return result(
                items,
                PlatformStartupCoverageStatus::Cancelled,
                Some(PlatformStartupCoverageReason::Cancelled),
                started,
            );
        }
        match inspect_service(manager, &service) {
            Ok(Some(item)) => items.push(item),
            Ok(None) => {}
            Err(reason) => {
                partial_reason.get_or_insert(reason);
            }
        }
    }
    unsafe {
        let _ = CloseServiceHandle(manager);
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

#[derive(Clone)]
struct EnumeratedService {
    name: String,
    display_name: String,
    running: bool,
}

fn enumerate_services(
    manager: windows::Win32::System::Services::SC_HANDLE,
) -> Result<Vec<EnumeratedService>, PlatformStartupCoverageReason> {
    let mut needed = 0;
    let mut returned = 0;
    unsafe {
        let _ = EnumServicesStatusExW(
            manager,
            SC_ENUM_PROCESS_INFO,
            SERVICE_WIN32,
            SERVICE_STATE_ALL,
            None,
            &mut needed,
            &mut returned,
            None,
            PCWSTR::null(),
        );
    }
    if needed == 0 {
        return Ok(Vec::new());
    }
    let capacity = needed as usize / size_of::<ENUM_SERVICE_STATUS_PROCESSW>() + 1;
    let mut buffer = vec![ENUM_SERVICE_STATUS_PROCESSW::default(); capacity];
    let bytes = unsafe {
        slice::from_raw_parts_mut(
            buffer.as_mut_ptr().cast::<u8>(),
            buffer.len() * size_of::<ENUM_SERVICE_STATUS_PROCESSW>(),
        )
    };
    unsafe {
        EnumServicesStatusExW(
            manager,
            SC_ENUM_PROCESS_INFO,
            SERVICE_WIN32,
            SERVICE_STATE_ALL,
            Some(bytes),
            &mut needed,
            &mut returned,
            None,
            PCWSTR::null(),
        )
        .map_err(|error| coverage_reason(&error))?;
    }
    Ok(buffer
        .into_iter()
        .take(returned as usize)
        .filter_map(|service| {
            let name = pwstr_string(service.lpServiceName)?;
            let display_name = pwstr_string(service.lpDisplayName).unwrap_or_else(|| name.clone());
            Some(EnumeratedService {
                name,
                display_name,
                running: service.ServiceStatusProcess.dwCurrentState == SERVICE_RUNNING,
            })
        })
        .collect())
}

fn inspect_service(
    manager: windows::Win32::System::Services::SC_HANDLE,
    service: &EnumeratedService,
) -> Result<Option<PlatformStartupArtifact>, PlatformStartupCoverageReason> {
    let service_name = windows_core::HSTRING::from(&service.name);
    let handle = unsafe {
        OpenServiceW(
            manager,
            &service_name,
            SERVICE_QUERY_CONFIG | SERVICE_QUERY_STATUS,
        )
        .map_err(|error| coverage_reason(&error))?
    };
    let config = query_service_config(handle);
    let description = query_service_description(handle);
    unsafe {
        let _ = CloseServiceHandle(handle);
    }
    let config = config?;
    if config.start_type != SERVICE_AUTO_START && config.start_type != SERVICE_DISABLED {
        return Ok(None);
    }
    let command_parts = split_command_line(&config.binary_path);
    let target_path = command_parts
        .first()
        .map(|path| PathBuf::from(expand_environment_variables(path)));
    let target_exists = target_path.as_deref().is_some_and(Path::exists);
    let mut diagnostics = Vec::new();
    if target_path.is_none() {
        diagnostics.push(PlatformStartupDiagnosticCode::InvalidData);
    } else if !target_exists {
        diagnostics.push(PlatformStartupDiagnosticCode::MissingTarget);
    }
    let system_item = target_path
        .as_deref()
        .is_some_and(|path| normalized_path(path).starts_with("c:\\windows\\"));
    let target_identity = target_path
        .as_deref()
        .map(|path| format!("path:{}", normalized_path(path)))
        .unwrap_or_else(|| format!("service:{}", service.name.to_lowercase()));
    let version_metadata = target_path
        .as_deref()
        .and_then(file_version_metadata)
        .unwrap_or_default();
    let service_description_available = description.is_some();
    let summary = description.or(version_metadata.description);
    Ok(Some(PlatformStartupArtifact {
        provider_item_id: format!("service:{}", service.name.to_lowercase()),
        source_kind: PlatformStartupSourceKind::Service,
        scope: PlatformStartupScope::Machine,
        triggers: vec![PlatformStartupTrigger::Boot],
        display_name: service.display_name.clone(),
        configuration_path: None,
        target: PlatformStartupTarget {
            kind: PlatformStartupTargetKind::Service,
            identity_key: target_identity.clone(),
            path: target_path.clone(),
            executable_name: target_path
                .as_deref()
                .and_then(Path::file_name)
                .and_then(|name| name.to_str())
                .map(ToOwned::to_owned),
            arguments: command_parts.into_iter().skip(1).collect(),
        },
        owner: PlatformStartupOwner {
            identity_key: Some(target_identity),
            name: version_metadata
                .product_name
                .or_else(|| Some(service.display_name.clone())),
            publisher: version_metadata.company_name,
            summary: summary.clone(),
            summary_source: if service_description_available {
                PlatformStartupSummarySource::ServiceDescription
            } else if summary.is_some() {
                PlatformStartupSummarySource::VersionInfo
            } else {
                PlatformStartupSummarySource::SourceLabel
            },
            version: version_metadata.product_version,
            icon_path: target_path.clone().filter(|path| path.exists()),
            confidence: PlatformStartupIdentityConfidence::Strong,
        },
        configured_state: if config.start_type == SERVICE_AUTO_START {
            PlatformStartupConfiguredState::Enabled
        } else {
            PlatformStartupConfiguredState::Disabled
        },
        runtime_state: if service.running {
            PlatformStartupRuntimeState::Running
        } else {
            PlatformStartupRuntimeState::Stopped
        },
        control_capability: service_control_capability(&config),
        trust: startup_trust(target_path.as_deref(), system_item),
        modified_at_ms: target_path.as_deref().and_then(modified_at_ms),
        diagnostics,
    }))
}

struct ServiceConfig {
    start_type: SERVICE_START_TYPE,
    binary_path: String,
}

fn query_service_config(
    service: windows::Win32::System::Services::SC_HANDLE,
) -> Result<ServiceConfig, PlatformStartupCoverageReason> {
    let mut needed = 0;
    unsafe {
        let _ = QueryServiceConfigW(service, None, 0, &mut needed);
    }
    if needed < size_of::<QUERY_SERVICE_CONFIGW>() as u32 {
        return Err(PlatformStartupCoverageReason::InvalidData);
    }
    let word_count = needed as usize / size_of::<usize>() + 1;
    let mut buffer = vec![0usize; word_count];
    unsafe {
        QueryServiceConfigW(
            service,
            Some(buffer.as_mut_ptr().cast::<QUERY_SERVICE_CONFIGW>()),
            (buffer.len() * size_of::<usize>()) as u32,
            &mut needed,
        )
        .map_err(|error| coverage_reason(&error))?;
    }
    let config = unsafe { &*buffer.as_ptr().cast::<QUERY_SERVICE_CONFIGW>() };
    Ok(ServiceConfig {
        start_type: config.dwStartType,
        binary_path: pwstr_string(config.lpBinaryPathName).unwrap_or_default(),
    })
}

fn service_control_capability(_config: &ServiceConfig) -> PlatformStartupControlCapability {
    PlatformStartupControlCapability::ViewOnly
}

fn query_service_description(
    service: windows::Win32::System::Services::SC_HANDLE,
) -> Option<String> {
    let mut needed = 0;
    unsafe {
        let _ = QueryServiceConfig2W(service, SERVICE_CONFIG_DESCRIPTION, None, &mut needed);
    }
    if needed < size_of::<SERVICE_DESCRIPTIONW>() as u32 {
        return None;
    }
    let word_count = needed as usize / size_of::<usize>() + 1;
    let mut buffer = vec![0usize; word_count];
    let bytes = unsafe {
        slice::from_raw_parts_mut(
            buffer.as_mut_ptr().cast::<u8>(),
            buffer.len() * size_of::<usize>(),
        )
    };
    unsafe {
        QueryServiceConfig2W(
            service,
            SERVICE_CONFIG_DESCRIPTION,
            Some(bytes),
            &mut needed,
        )
        .ok()?;
    }
    let description = unsafe { &*buffer.as_ptr().cast::<SERVICE_DESCRIPTIONW>() };
    pwstr_string(description.lpDescription).filter(|value| !value.trim().is_empty())
}

fn pwstr_string(value: windows_core::PWSTR) -> Option<String> {
    if value.is_null() {
        return None;
    }
    let mut length = 0;
    unsafe {
        while *value.0.add(length) != 0 {
            length += 1;
        }
        Some(String::from_utf16_lossy(slice::from_raw_parts(
            value.0, length,
        )))
    }
}

fn coverage_reason(error: &windows_core::Error) -> PlatformStartupCoverageReason {
    if error.code().0 as u32 == 0x8007_0005 {
        PlatformStartupCoverageReason::AccessDenied
    } else {
        PlatformStartupCoverageReason::ApiUnavailable
    }
}

fn result(
    items: Vec<PlatformStartupArtifact>,
    status: PlatformStartupCoverageStatus,
    reason: Option<PlatformStartupCoverageReason>,
    started: Instant,
) -> PlatformStartupSourceResult {
    PlatformStartupSourceResult {
        source_id: "windows.services".to_string(),
        required: true,
        status,
        reason,
        items,
        elapsed_ms: started.elapsed().as_millis() as u64,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn services_are_always_view_only() {
        let automatic = ServiceConfig {
            start_type: SERVICE_AUTO_START,
            binary_path: String::new(),
        };

        assert_eq!(
            service_control_capability(&automatic),
            PlatformStartupControlCapability::ViewOnly
        );
    }
}

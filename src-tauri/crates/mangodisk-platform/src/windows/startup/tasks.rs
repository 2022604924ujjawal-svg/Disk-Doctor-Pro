use std::cell::Cell;
use std::time::Instant;
use std::{ffi::OsStr, iter, os::windows::ffi::OsStrExt, path::PathBuf};

use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CoUninitialize, CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED,
};
use windows::Win32::System::TaskScheduler::{
    IAction, IExecAction, IRegisteredTask, ITaskFolder, ITaskService, TaskScheduler,
    TASK_ACTION_EXEC, TASK_ENUM_HIDDEN, TASK_STATE_RUNNING, TASK_TRIGGER_BOOT, TASK_TRIGGER_DAILY,
    TASK_TRIGGER_EVENT, TASK_TRIGGER_LOGON, TASK_TRIGGER_MONTHLY, TASK_TRIGGER_MONTHLYDOW,
    TASK_TRIGGER_TIME, TASK_TRIGGER_TYPE2, TASK_TRIGGER_WEEKLY,
};
use windows::Win32::System::Variant::VARIANT;
use windows_core::{Interface, BSTR};
use windows_sys::Win32::{
    Foundation::{CloseHandle, LocalFree, HANDLE, HLOCAL},
    Security::{
        AccessCheck,
        Authorization::{ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1},
        DuplicateToken, GetTokenInformation, SecurityIdentification, TokenElevationType,
        TokenElevationTypeFull, TokenLinkedToken, DACL_SECURITY_INFORMATION, GENERIC_MAPPING,
        GROUP_SECURITY_INFORMATION, OWNER_SECURITY_INFORMATION, PRIVILEGE_SET,
        PSECURITY_DESCRIPTOR, TOKEN_DUPLICATE, TOKEN_ELEVATION_TYPE, TOKEN_LINKED_TOKEN,
        TOKEN_QUERY,
    },
    Storage::FileSystem::{
        FILE_ALL_ACCESS, FILE_GENERIC_EXECUTE, FILE_GENERIC_READ, FILE_GENERIC_WRITE,
    },
    System::Threading::{GetCurrentProcess, OpenProcessToken},
};

use crate::{
    PlatformCancellation, PlatformError, PlatformErrorCode, PlatformResult,
    PlatformStartupArtifact, PlatformStartupChangeRequest, PlatformStartupChangeResult,
    PlatformStartupConfiguredState, PlatformStartupControlCapability,
    PlatformStartupCoverageReason, PlatformStartupCoverageStatus, PlatformStartupDesiredState,
    PlatformStartupDiagnosticCode, PlatformStartupIdentityConfidence, PlatformStartupOwner,
    PlatformStartupRuntimeState, PlatformStartupScope, PlatformStartupSourceKind,
    PlatformStartupSourceResult, PlatformStartupSummarySource, PlatformStartupTarget,
    PlatformStartupTargetKind, PlatformStartupTrigger,
};

use super::metadata::{file_version_metadata, startup_trust};
use super::registry::{
    expand_environment_variables, normalized_path, split_command_line, target_kind,
};

struct ComGuard;

struct OwnedHandle(HANDLE);

impl OwnedHandle {
    fn raw(&self) -> HANDLE {
        self.0
    }
}

impl Drop for OwnedHandle {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe {
                CloseHandle(self.0);
            }
        }
    }
}

struct OwnedSecurityDescriptor(PSECURITY_DESCRIPTOR);

impl Drop for OwnedSecurityDescriptor {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe {
                LocalFree(self.0 as HLOCAL);
            }
        }
    }
}

struct TaskAccessContext {
    interactive_token: Option<OwnedHandle>,
    security_descriptor_failure_count: Cell<u64>,
    descriptor_conversion_failure_count: Cell<u64>,
    access_check_failure_count: Cell<u64>,
}

impl TaskAccessContext {
    fn current() -> Self {
        // A privileged helper must classify the task exactly as the interactive desktop process
        // did before UAC. For a split-token administrator, use the linked limited token rather
        // than the helper's administrator token; otherwise every ACL would appear writable after
        // elevation and the preflight digest could never match. Failure to obtain a safe token is
        // intentionally represented as no direct access so the caller falls back to elevation.
        Self {
            interactive_token: interactive_user_impersonation_token(),
            security_descriptor_failure_count: Cell::new(0),
            descriptor_conversion_failure_count: Cell::new(0),
            access_check_failure_count: Cell::new(0),
        }
    }
}

impl ComGuard {
    fn initialize() -> windows_core::Result<Self> {
        unsafe {
            CoInitializeEx(None, COINIT_MULTITHREADED).ok()?;
        }
        Ok(Self)
    }
}

impl Drop for ComGuard {
    fn drop(&mut self) {
        unsafe {
            CoUninitialize();
        }
    }
}

pub(super) fn scan(cancellation: &PlatformCancellation) -> PlatformStartupSourceResult {
    let started = Instant::now();
    let Ok(_com) = ComGuard::initialize() else {
        return unavailable(started, PlatformStartupCoverageReason::ApiUnavailable);
    };
    let scheduler: ITaskService =
        match unsafe { CoCreateInstance(&TaskScheduler, None, CLSCTX_INPROC_SERVER) } {
            Ok(scheduler) => scheduler,
            Err(error) => return unavailable(started, coverage_reason(&error)),
        };
    let empty = VARIANT::default();
    if let Err(error) = unsafe { scheduler.Connect(&empty, &empty, &empty, &empty) } {
        return unavailable(started, coverage_reason(&error));
    }
    let root = match unsafe { scheduler.GetFolder(&BSTR::from("\\")) } {
        Ok(root) => root,
        Err(error) => return unavailable(started, coverage_reason(&error)),
    };
    let access_context = TaskAccessContext::current();
    let mut items = Vec::new();
    let mut partial_reason = None;
    visit_folder(
        &root,
        cancellation,
        &access_context,
        &mut items,
        &mut partial_reason,
    );
    log_task_access_summary(&items, &access_context);
    if cancellation.is_cancelled() {
        return result(
            items,
            PlatformStartupCoverageStatus::Cancelled,
            Some(PlatformStartupCoverageReason::Cancelled),
            started,
        );
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

pub(super) fn change(
    request: &PlatformStartupChangeRequest,
) -> PlatformResult<PlatformStartupChangeResult> {
    let _com = ComGuard::initialize().map_err(task_platform_error)?;
    let scheduler: ITaskService =
        unsafe { CoCreateInstance(&TaskScheduler, None, CLSCTX_INPROC_SERVER) }
            .map_err(task_platform_error)?;
    let empty = VARIANT::default();
    unsafe { scheduler.Connect(&empty, &empty, &empty, &empty) }.map_err(task_platform_error)?;
    let root = unsafe { scheduler.GetFolder(&BSTR::from("\\")) }.map_err(task_platform_error)?;
    let access_context = TaskAccessContext::current();
    let (task, current) = find_task(&root, &request.provider_item_id, &access_context)?
        .ok_or_else(|| PlatformError::item_changed("scheduled task no longer exists"))?;
    if crate::startup_helper::artifact_digest(&current)
        != crate::startup_helper::artifact_digest(&request.expected_artifact)
    {
        return Err(PlatformError::item_changed(
            "scheduled task changed after preflight",
        ));
    }
    if !matches!(
        current.control_capability,
        PlatformStartupControlCapability::Toggleable
            | PlatformStartupControlCapability::ElevationRequired
    ) {
        return Err(PlatformError::new(
            PlatformErrorCode::Unsupported,
            "scheduled task is not available for controlled changes",
        ));
    }
    if request.desired_state == PlatformStartupDesiredState::Removed {
        return Err(PlatformError::new(
            PlatformErrorCode::Unsupported,
            "scheduled tasks cannot be removed by startup management",
        ));
    }
    let desired_enabled = request.desired_state == PlatformStartupDesiredState::Enabled;
    if (current.configured_state == PlatformStartupConfiguredState::Enabled) != desired_enabled {
        unsafe { task.SetEnabled(desired_enabled.into()) }.map_err(task_platform_error)?;
    }
    let verified = inspect_task(&task, &access_context)
        .map_err(|reason| {
            PlatformError::new(
                PlatformErrorCode::OperationFailed,
                format!("verify scheduled task: {reason:?}"),
            )
        })?
        .ok_or_else(|| PlatformError::item_changed("scheduled task trigger changed"))?;
    let desired = if desired_enabled {
        PlatformStartupConfiguredState::Enabled
    } else {
        PlatformStartupConfiguredState::Disabled
    };
    Ok(PlatformStartupChangeResult {
        previous_state: current.configured_state,
        configured_state: verified.configured_state,
        verified: verified.configured_state == desired,
    })
}

fn find_task(
    folder: &ITaskFolder,
    provider_item_id: &str,
    access_context: &TaskAccessContext,
) -> PlatformResult<Option<(IRegisteredTask, PlatformStartupArtifact)>> {
    let tasks = unsafe { folder.GetTasks(TASK_ENUM_HIDDEN.0) }.map_err(task_platform_error)?;
    let count = unsafe { tasks.Count() }.map_err(task_platform_error)?;
    for index in 1..=count {
        let task = unsafe { tasks.get_Item(&VARIANT::from(index)) }.map_err(task_platform_error)?;
        if let Some(artifact) = inspect_task(&task, access_context).map_err(|reason| {
            PlatformError::new(
                PlatformErrorCode::OperationFailed,
                format!("inspect scheduled task: {reason:?}"),
            )
        })? {
            if artifact.provider_item_id == provider_item_id {
                return Ok(Some((task, artifact)));
            }
        }
    }
    let folders = unsafe { folder.GetFolders(0) }.map_err(task_platform_error)?;
    let count = unsafe { folders.Count() }.map_err(task_platform_error)?;
    for index in 1..=count {
        let child =
            unsafe { folders.get_Item(&VARIANT::from(index)) }.map_err(task_platform_error)?;
        if let Some(found) = find_task(&child, provider_item_id, access_context)? {
            return Ok(Some(found));
        }
    }
    Ok(None)
}

fn task_platform_error(error: windows_core::Error) -> PlatformError {
    let code = if error.code().0 as u32 == 0x8007_0005 {
        PlatformErrorCode::AccessDenied
    } else {
        PlatformErrorCode::OperationFailed
    };
    PlatformError::new(code, format!("task scheduler operation failed: {error}"))
}

fn visit_folder(
    folder: &ITaskFolder,
    cancellation: &PlatformCancellation,
    access_context: &TaskAccessContext,
    items: &mut Vec<PlatformStartupArtifact>,
    partial_reason: &mut Option<PlatformStartupCoverageReason>,
) {
    if cancellation.is_cancelled() {
        return;
    }
    match unsafe { folder.GetTasks(TASK_ENUM_HIDDEN.0) } {
        Ok(tasks) => {
            let count = unsafe { tasks.Count() }.unwrap_or_default();
            for index in 1..=count {
                if cancellation.is_cancelled() {
                    return;
                }
                let task = match unsafe { tasks.get_Item(&VARIANT::from(index)) } {
                    Ok(task) => task,
                    Err(error) => {
                        partial_reason.get_or_insert(coverage_reason(&error));
                        continue;
                    }
                };
                match inspect_task(&task, access_context) {
                    Ok(Some(item)) => items.push(item),
                    Ok(None) => {}
                    Err(reason) => {
                        partial_reason.get_or_insert(reason);
                    }
                }
            }
        }
        Err(error) => {
            partial_reason.get_or_insert(coverage_reason(&error));
        }
    }
    match unsafe { folder.GetFolders(0) } {
        Ok(folders) => {
            let count = unsafe { folders.Count() }.unwrap_or_default();
            for index in 1..=count {
                if cancellation.is_cancelled() {
                    return;
                }
                match unsafe { folders.get_Item(&VARIANT::from(index)) } {
                    Ok(child) => {
                        visit_folder(&child, cancellation, access_context, items, partial_reason)
                    }
                    Err(error) => {
                        partial_reason.get_or_insert(coverage_reason(&error));
                    }
                }
            }
        }
        Err(error) => {
            partial_reason.get_or_insert(coverage_reason(&error));
        }
    }
}

fn inspect_task(
    task: &IRegisteredTask,
    access_context: &TaskAccessContext,
) -> Result<Option<PlatformStartupArtifact>, PlatformStartupCoverageReason> {
    let definition = unsafe { task.Definition() }.map_err(|error| coverage_reason(&error))?;
    let triggers = task_triggers(&definition)?;
    if !triggers.iter().any(|trigger| {
        matches!(
            trigger,
            PlatformStartupTrigger::Boot | PlatformStartupTrigger::UserLogon
        )
    }) {
        return Ok(None);
    }
    let task_path = unsafe { task.Path() }
        .map(|value| value.to_string())
        .map_err(|error| coverage_reason(&error))?;
    let task_name = unsafe { task.Name() }
        .map(|value| value.to_string())
        .unwrap_or_else(|_| task_path.clone());
    let enabled = unsafe { task.Enabled() }.map(bool::from).unwrap_or(false);
    let running = unsafe { task.State() }
        .map(|state| state == TASK_STATE_RUNNING)
        .unwrap_or(false);
    let (target, action_count, unsupported_action) = task_target(&definition, &task_path)?;
    let registration = unsafe { definition.RegistrationInfo() }.ok();
    let description = registration
        .as_ref()
        .and_then(|info| output_bstr(|value| unsafe { info.Description(value) }));
    let author = registration
        .as_ref()
        .and_then(|info| output_bstr(|value| unsafe { info.Author(value) }));
    let system_item = task_path
        .to_ascii_lowercase()
        .starts_with("\\microsoft\\windows\\");
    let mut diagnostics = Vec::new();
    if unsupported_action || action_count > 1 {
        diagnostics.push(PlatformStartupDiagnosticCode::UnsupportedFormat);
    }
    if target.path.as_deref().is_some_and(|path| !path.exists()) {
        diagnostics.push(PlatformStartupDiagnosticCode::MissingTarget);
    }
    let scope = if triggers.contains(&PlatformStartupTrigger::Boot) {
        PlatformStartupScope::Machine
    } else {
        PlatformStartupScope::User
    };
    let icon_path = target.path.clone().filter(|path| path.exists());
    let trust = startup_trust(target.path.as_deref(), system_item);
    let version_metadata = target
        .path
        .as_deref()
        .and_then(file_version_metadata)
        .unwrap_or_default();
    let owner_identity = target.identity_key.clone();
    let task_description_available = description.is_some();
    let summary = description.or(version_metadata.description);
    Ok(Some(PlatformStartupArtifact {
        provider_item_id: format!("task:{}", task_path.to_ascii_lowercase()),
        source_kind: PlatformStartupSourceKind::ScheduledTask,
        scope,
        triggers,
        display_name: task_name.clone(),
        configuration_path: None,
        target,
        owner: PlatformStartupOwner {
            identity_key: Some(owner_identity),
            name: version_metadata.product_name.or(Some(task_name)),
            publisher: author.or(version_metadata.company_name),
            summary: summary.clone(),
            summary_source: if task_description_available {
                PlatformStartupSummarySource::TaskDescription
            } else if summary.is_some() {
                PlatformStartupSummarySource::VersionInfo
            } else {
                PlatformStartupSummarySource::SourceLabel
            },
            version: version_metadata.product_version,
            icon_path,
            confidence: PlatformStartupIdentityConfidence::Strong,
        },
        configured_state: if enabled {
            PlatformStartupConfiguredState::Enabled
        } else {
            PlatformStartupConfiguredState::Disabled
        },
        runtime_state: if running {
            PlatformStartupRuntimeState::Running
        } else {
            PlatformStartupRuntimeState::Stopped
        },
        control_capability: scheduled_task_control_capability(
            system_item,
            task_is_writable(task, access_context),
        ),
        trust,
        modified_at_ms: None,
        diagnostics,
    }))
}

fn scheduled_task_control_capability(
    system_item: bool,
    writable: bool,
) -> PlatformStartupControlCapability {
    if system_item {
        return PlatformStartupControlCapability::SystemManaged;
    }
    if writable {
        PlatformStartupControlCapability::Toggleable
    } else {
        PlatformStartupControlCapability::ElevationRequired
    }
}

fn task_is_writable(task: &IRegisteredTask, access_context: &TaskAccessContext) -> bool {
    let Some(token) = access_context.interactive_token.as_ref() else {
        return false;
    };
    let security_information =
        DACL_SECURITY_INFORMATION | OWNER_SECURITY_INFORMATION | GROUP_SECURITY_INFORMATION;
    let sddl = match unsafe { task.GetSecurityDescriptor(security_information as i32) } {
        Ok(sddl) => sddl,
        Err(_) => {
            increment_counter(&access_context.security_descriptor_failure_count);
            return false;
        }
    };
    let sddl = OsStr::new(&sddl.to_string())
        .encode_wide()
        .chain(iter::once(0))
        .collect::<Vec<_>>();
    let mut raw_descriptor = std::ptr::null_mut();
    if unsafe {
        ConvertStringSecurityDescriptorToSecurityDescriptorW(
            sddl.as_ptr(),
            SDDL_REVISION_1,
            &mut raw_descriptor,
            std::ptr::null_mut(),
        )
    } == 0
    {
        increment_counter(&access_context.descriptor_conversion_failure_count);
        return false;
    }
    let descriptor = OwnedSecurityDescriptor(raw_descriptor);
    let mapping = GENERIC_MAPPING {
        GenericRead: FILE_GENERIC_READ,
        GenericWrite: FILE_GENERIC_WRITE,
        GenericExecute: FILE_GENERIC_EXECUTE,
        GenericAll: FILE_ALL_ACCESS,
    };
    let mut privileges = PRIVILEGE_SET::default();
    let mut privilege_bytes = std::mem::size_of::<PRIVILEGE_SET>() as u32;
    let mut granted_access = 0;
    let mut access_status = 0;
    let checked = unsafe {
        AccessCheck(
            descriptor.0,
            token.raw(),
            FILE_GENERIC_WRITE,
            &mapping,
            &mut privileges,
            &mut privilege_bytes,
            &mut granted_access,
            &mut access_status,
        )
    };
    if checked == 0 {
        increment_counter(&access_context.access_check_failure_count);
        return false;
    }
    access_status != 0
}

fn increment_counter(counter: &Cell<u64>) {
    counter.set(counter.get().saturating_add(1));
}

fn log_task_access_summary(items: &[PlatformStartupArtifact], access_context: &TaskAccessContext) {
    let toggleable_count = items
        .iter()
        .filter(|item| item.control_capability == PlatformStartupControlCapability::Toggleable)
        .count();
    let elevation_required_count = items
        .iter()
        .filter(|item| {
            item.control_capability == PlatformStartupControlCapability::ElevationRequired
        })
        .count();
    let system_managed_count = items
        .iter()
        .filter(|item| item.control_capability == PlatformStartupControlCapability::SystemManaged)
        .count();
    log::info!(
        "windows_scheduled_task_access_classified item_count={} toggleable_count={} elevation_required_count={} system_managed_count={} interactive_token_available={} security_descriptor_failure_count={} descriptor_conversion_failure_count={} access_check_failure_count={}",
        items.len(),
        toggleable_count,
        elevation_required_count,
        system_managed_count,
        access_context.interactive_token.is_some(),
        access_context.security_descriptor_failure_count.get(),
        access_context.descriptor_conversion_failure_count.get(),
        access_context.access_check_failure_count.get()
    );
}

fn interactive_user_impersonation_token() -> Option<OwnedHandle> {
    let mut process_token = std::ptr::null_mut();
    if unsafe {
        OpenProcessToken(
            GetCurrentProcess(),
            TOKEN_QUERY | TOKEN_DUPLICATE,
            &mut process_token,
        )
    } == 0
    {
        return None;
    }
    let process_token = OwnedHandle(process_token);
    let elevation_type = token_elevation_type(process_token.raw())?;
    if elevation_type == TokenElevationTypeFull {
        let linked_token = limited_linked_token(process_token.raw())?;
        return duplicate_impersonation_token(linked_token.raw());
    }
    duplicate_impersonation_token(process_token.raw())
}

fn token_elevation_type(token: HANDLE) -> Option<TOKEN_ELEVATION_TYPE> {
    let mut elevation_type = 0;
    let mut returned_bytes = 0;
    let queried = unsafe {
        GetTokenInformation(
            token,
            TokenElevationType,
            &mut elevation_type as *mut TOKEN_ELEVATION_TYPE as *mut _,
            std::mem::size_of::<TOKEN_ELEVATION_TYPE>() as u32,
            &mut returned_bytes,
        )
    };
    (queried != 0).then_some(elevation_type)
}

fn limited_linked_token(token: HANDLE) -> Option<OwnedHandle> {
    let mut linked = TOKEN_LINKED_TOKEN::default();
    let mut returned_bytes = 0;
    let queried = unsafe {
        GetTokenInformation(
            token,
            TokenLinkedToken,
            &mut linked as *mut TOKEN_LINKED_TOKEN as *mut _,
            std::mem::size_of::<TOKEN_LINKED_TOKEN>() as u32,
            &mut returned_bytes,
        )
    };
    (queried != 0 && !linked.LinkedToken.is_null()).then(|| OwnedHandle(linked.LinkedToken))
}

fn duplicate_impersonation_token(token: HANDLE) -> Option<OwnedHandle> {
    let mut duplicated = std::ptr::null_mut();
    // AccessCheck only needs to identify the caller. Requesting SecurityIdentification also works
    // with the linked limited token returned by UAC and avoids acquiring unnecessary authority.
    let duplicated_ok = unsafe { DuplicateToken(token, SecurityIdentification, &mut duplicated) };
    (duplicated_ok != 0 && !duplicated.is_null()).then(|| OwnedHandle(duplicated))
}

fn task_triggers(
    definition: &windows::Win32::System::TaskScheduler::ITaskDefinition,
) -> Result<Vec<PlatformStartupTrigger>, PlatformStartupCoverageReason> {
    let collection = unsafe { definition.Triggers() }.map_err(|error| coverage_reason(&error))?;
    let mut count = 0;
    unsafe { collection.Count(&mut count) }.map_err(|error| coverage_reason(&error))?;
    let mut triggers = Vec::new();
    for index in 1..=count {
        let trigger =
            unsafe { collection.get_Item(index) }.map_err(|error| coverage_reason(&error))?;
        let mut kind = TASK_TRIGGER_TYPE2::default();
        unsafe { trigger.Type(&mut kind) }.map_err(|error| coverage_reason(&error))?;
        let mapped = if kind == TASK_TRIGGER_BOOT {
            PlatformStartupTrigger::Boot
        } else if kind == TASK_TRIGGER_LOGON {
            PlatformStartupTrigger::UserLogon
        } else if [
            TASK_TRIGGER_TIME,
            TASK_TRIGGER_DAILY,
            TASK_TRIGGER_WEEKLY,
            TASK_TRIGGER_MONTHLY,
            TASK_TRIGGER_MONTHLYDOW,
        ]
        .contains(&kind)
        {
            PlatformStartupTrigger::Scheduled
        } else if kind == TASK_TRIGGER_EVENT {
            PlatformStartupTrigger::Event
        } else {
            PlatformStartupTrigger::Unknown
        };
        if !triggers.contains(&mapped) {
            triggers.push(mapped);
        }
    }
    Ok(triggers)
}

fn task_target(
    definition: &windows::Win32::System::TaskScheduler::ITaskDefinition,
    task_path: &str,
) -> Result<(PlatformStartupTarget, i32, bool), PlatformStartupCoverageReason> {
    let actions = unsafe { definition.Actions() }.map_err(|error| coverage_reason(&error))?;
    let mut count = 0;
    unsafe { actions.Count(&mut count) }.map_err(|error| coverage_reason(&error))?;
    let mut unsupported = false;
    for index in 1..=count {
        let action: IAction =
            unsafe { actions.get_Item(index) }.map_err(|error| coverage_reason(&error))?;
        let mut kind = Default::default();
        unsafe { action.Type(&mut kind) }.map_err(|error| coverage_reason(&error))?;
        if kind != TASK_ACTION_EXEC {
            unsupported = true;
            continue;
        }
        let Ok(exec) = action.cast::<IExecAction>() else {
            unsupported = true;
            continue;
        };
        let path = output_bstr(|value| unsafe { exec.Path(value) }).unwrap_or_default();
        let path = PathBuf::from(expand_environment_variables(&path));
        let arguments = output_bstr(|value| unsafe { exec.Arguments(value) })
            .map(|value| split_command_line(&value))
            .unwrap_or_default();
        let identity = normalized_path(&path);
        return Ok((
            PlatformStartupTarget {
                kind: target_kind(Some(&path)),
                identity_key: format!("path:{identity}"),
                path: Some(path.clone()),
                executable_name: path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .map(ToOwned::to_owned),
                arguments,
            },
            count,
            unsupported,
        ));
    }
    Ok((
        PlatformStartupTarget {
            kind: PlatformStartupTargetKind::Task,
            identity_key: format!("task:{}", task_path.to_ascii_lowercase()),
            path: None,
            executable_name: None,
            arguments: Vec::new(),
        },
        count,
        true,
    ))
}

fn output_bstr(reader: impl FnOnce(*mut BSTR) -> windows_core::Result<()>) -> Option<String> {
    let mut value = BSTR::new();
    reader(&mut value).ok()?;
    let value = value.to_string();
    (!value.trim().is_empty()).then_some(value)
}

fn coverage_reason(error: &windows_core::Error) -> PlatformStartupCoverageReason {
    if error.code().0 as u32 == 0x8007_0005 {
        PlatformStartupCoverageReason::AccessDenied
    } else {
        PlatformStartupCoverageReason::ApiUnavailable
    }
}

fn unavailable(
    started: Instant,
    reason: PlatformStartupCoverageReason,
) -> PlatformStartupSourceResult {
    result(
        Vec::new(),
        PlatformStartupCoverageStatus::Unavailable,
        Some(reason),
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
        source_id: "windows.scheduled_tasks".to_string(),
        required: true,
        status,
        reason,
        items,
        elapsed_ms: started.elapsed().as_millis() as u64,
    }
}

#[cfg(test)]
mod tests {
    use std::process::Command;
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::*;

    #[test]
    fn writable_third_party_tasks_remain_toggleable_without_elevation() {
        assert_eq!(
            scheduled_task_control_capability(false, true),
            PlatformStartupControlCapability::Toggleable
        );
    }

    #[test]
    fn read_only_third_party_tasks_require_elevation() {
        assert_eq!(
            scheduled_task_control_capability(false, false),
            PlatformStartupControlCapability::ElevationRequired
        );
    }

    #[test]
    fn microsoft_system_tasks_remain_view_only() {
        assert_eq!(
            scheduled_task_control_capability(true, true),
            PlatformStartupControlCapability::SystemManaged
        );
    }

    #[test]
    #[ignore = "requires a UAC-linked elevated Windows fixture"]
    fn actual_task_acl_routes_only_read_only_items_through_the_helper() {
        let suffix = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system time must be available")
            .as_nanos();
        let writable_name = format!("MangoDiskWritableTaskFixture{suffix}");
        let restricted_name = format!("MangoDiskRestrictedTaskFixture{suffix}");
        let _fixture = ScheduledTaskFixture::create(&writable_name, &restricted_name);

        let writable = fixture_artifact(&writable_name);
        assert_eq!(
            writable.control_capability,
            PlatformStartupControlCapability::Toggleable
        );
        let writable_disabled = change(&PlatformStartupChangeRequest {
            provider_item_id: writable.provider_item_id.clone(),
            source_id: "windows.scheduled_tasks".to_owned(),
            expected_artifact: writable,
            desired_state: PlatformStartupDesiredState::Disabled,
        })
        .expect("the writable task must change without the helper");
        assert!(writable_disabled.verified);

        let restricted = fixture_artifact(&restricted_name);
        assert_eq!(
            restricted.control_capability,
            PlatformStartupControlCapability::ElevationRequired
        );
        let restricted_disabled = super::super::helper_change(
            "windows.scheduled_tasks",
            &restricted.provider_item_id,
            &crate::startup_helper::artifact_digest(&restricted),
            PlatformStartupDesiredState::Disabled,
        )
        .expect("the elevated helper boundary must disable the read-only task");
        assert!(restricted_disabled.verified);

        restore_fixture(&writable_name, false);
        restore_fixture(&restricted_name, true);
    }

    fn fixture_artifact(task_name: &str) -> PlatformStartupArtifact {
        let provider_item_id = format!("task:\\{}", task_name.to_ascii_lowercase());
        let cancellation = PlatformCancellation::new(|| false);
        scan(&cancellation)
            .items
            .into_iter()
            .find(|artifact| artifact.provider_item_id == provider_item_id)
            .expect("the scheduled task fixture must be discoverable")
    }

    fn restore_fixture(task_name: &str, through_helper: bool) {
        let artifact = fixture_artifact(task_name);
        let result = if through_helper {
            super::super::helper_change(
                "windows.scheduled_tasks",
                &artifact.provider_item_id,
                &crate::startup_helper::artifact_digest(&artifact),
                PlatformStartupDesiredState::Enabled,
            )
        } else {
            change(&PlatformStartupChangeRequest {
                provider_item_id: artifact.provider_item_id.clone(),
                source_id: "windows.scheduled_tasks".to_owned(),
                expected_artifact: artifact,
                desired_state: PlatformStartupDesiredState::Enabled,
            })
        };
        assert!(
            result.expect("the fixture state must be restored").verified,
            "the fixture restore must be verified"
        );
    }

    struct ScheduledTaskFixture {
        writable_name: String,
        restricted_name: String,
    }

    impl ScheduledTaskFixture {
        fn create(writable_name: &str, restricted_name: &str) -> Self {
            let script = format!(
                r#"
$ErrorActionPreference = 'Stop'
$service = New-Object -ComObject 'Schedule.Service'
$service.Connect()
$root = $service.GetFolder('\')
$sid = [System.Security.Principal.WindowsIdentity]::GetCurrent().User.Value
$account = [System.Security.Principal.WindowsIdentity]::GetCurrent().Name
function New-MangoDiskFixture([string] $name, [string] $rights) {{
    $definition = $service.NewTask(0)
    $definition.RegistrationInfo.Description = 'MangoDisk scheduled-task ACL fixture'
    $definition.Settings.Enabled = $true
    $trigger = $definition.Triggers.Create(9)
    $trigger.UserId = $account
    $action = $definition.Actions.Create(0)
    $action.Path = "$env:SystemRoot\System32\cmd.exe"
    $action.Arguments = '/c exit 0'
    $task = $root.RegisterTaskDefinition($name, $definition, 6, $account, $null, 3, $null)
    $task.SetSecurityDescriptor("D:P(A;;FA;;;SY)(A;;FA;;;BA)(A;;$rights;;;$sid)", 0)
}}
New-MangoDiskFixture '{writable_name}' 'FA'
New-MangoDiskFixture '{restricted_name}' 'FR'
"#
            );
            run_powershell(&script, "create scheduled task fixtures");
            Self {
                writable_name: writable_name.to_owned(),
                restricted_name: restricted_name.to_owned(),
            }
        }
    }

    impl Drop for ScheduledTaskFixture {
        fn drop(&mut self) {
            let script = format!(
                r#"
$ErrorActionPreference = 'SilentlyContinue'
$service = New-Object -ComObject 'Schedule.Service'
$service.Connect()
$root = $service.GetFolder('\')
$root.DeleteTask('{}', 0)
$root.DeleteTask('{}', 0)
"#,
                self.writable_name, self.restricted_name
            );
            let _ = Command::new("powershell.exe")
                .args(["-NoProfile", "-NonInteractive", "-Command", &script])
                .output();
        }
    }

    fn run_powershell(script: &str, operation: &str) {
        let output = Command::new("powershell.exe")
            .args(["-NoProfile", "-NonInteractive", "-Command", script])
            .output()
            .unwrap_or_else(|error| panic!("{operation}: {error}"));
        assert!(
            output.status.success(),
            "{operation}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

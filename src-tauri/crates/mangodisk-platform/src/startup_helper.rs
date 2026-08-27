use std::{
    collections::BTreeSet,
    ffi::OsString,
    fs::{self, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
    time::{SystemTime, UNIX_EPOCH},
};

use serde::{Deserialize, Serialize};

use crate::{
    PlatformError, PlatformErrorCode, PlatformResult, PlatformStartupArtifact,
    PlatformStartupChangeRequest, PlatformStartupChangeResult, PlatformStartupConfiguredState,
    PlatformStartupDesiredState,
};

const HELPER_FLAG: &str = "--mangodisk-startup-helper-v2";
const PROTOCOL: &str = "mangodisk-startup-helper-v2";
const MAX_MESSAGE_BYTES: u64 = 1024 * 1024;
const MAX_BATCH_ITEMS: usize = 128;
const HELPER_SUCCESS_EXIT_CODE: i32 = 0;
const HELPER_FAILURE_EXIT_CODE: i32 = 70;
static NEXT_REQUEST_ID: AtomicU64 = AtomicU64::new(1);

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct HelperRequest {
    protocol: String,
    nonce: String,
    items: Vec<HelperRequestItem>,
    #[cfg(target_os = "macos")]
    interactive_user_id: u32,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct HelperRequestItem {
    source_id: String,
    provider_item_id: String,
    expected_artifact_digest: String,
    desired_state: WireState,
}

#[derive(Debug, Clone)]
pub(crate) struct StartupHelperChangeRequest {
    pub(crate) source_id: String,
    pub(crate) provider_item_id: String,
    pub(crate) expected_artifact_digest: String,
    pub(crate) desired_state: PlatformStartupDesiredState,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct HelperResponse {
    protocol: String,
    nonce: String,
    items: Vec<HelperResponseItem>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct HelperResponseItem {
    outcome: Option<WireOutcome>,
    error_code: Option<WireErrorCode>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
enum WireState {
    Enabled,
    Disabled,
    Removed,
    Unknown,
    NotApplicable,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct WireOutcome {
    previous_state: WireState,
    configured_state: WireState,
    verified: bool,
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
enum WireErrorCode {
    AccessDenied,
    UserCancelled,
    ItemChanged,
    InvalidData,
    InvalidPath,
    Io,
    OperationFailed,
    Unsupported,
}

/// Runs the narrow startup helper mode before the desktop runtime is initialized.
///
/// Returning `None` means the process was launched normally. Helper mode accepts
/// exactly two absolute message paths and never starts Tauri or a WebView.
pub fn run_startup_helper_mode<I>(arguments: I) -> Option<i32>
where
    I: IntoIterator<Item = OsString>,
{
    let arguments = arguments.into_iter().collect::<Vec<_>>();
    if arguments.get(1).and_then(|value| value.to_str()) != Some(HELPER_FLAG) {
        return None;
    }
    let exit_code = match helper_paths(&arguments).and_then(|(request, response)| {
        execute_helper_request(&request, &response)
            .and_then(|value| write_helper_response(&request, &response, &value))
    }) {
        Ok(()) => HELPER_SUCCESS_EXIT_CODE,
        Err(_) => HELPER_FAILURE_EXIT_CODE,
    };
    Some(exit_code)
}

pub(crate) fn change_with_privileges(
    request: &PlatformStartupChangeRequest,
    authorization_prompt: Option<&str>,
) -> PlatformResult<PlatformStartupChangeResult> {
    let mut results = change_many_with_privileges(&[request], authorization_prompt)?;
    results.pop().ok_or_else(|| {
        PlatformError::new(
            PlatformErrorCode::InvalidData,
            "startup helper returned no change result",
        )
    })?
}

/// Applies all privileged requests in one elevated process.
///
/// Each item is still independently allowlisted, re-read, mutated, and verified by the helper.
/// Sharing only the process boundary prevents repeated authorization prompts without granting the
/// desktop process reusable administrator authority.
pub(crate) fn change_many_with_privileges(
    requests: &[&PlatformStartupChangeRequest],
    authorization_prompt: Option<&str>,
) -> PlatformResult<Vec<PlatformResult<PlatformStartupChangeResult>>> {
    if requests.is_empty() || requests.len() > MAX_BATCH_ITEMS {
        return Err(PlatformError::new(
            PlatformErrorCode::InvalidData,
            "startup helper batch size is invalid",
        ));
    }
    let source_count = requests
        .iter()
        .map(|request| request.source_id.as_str())
        .collect::<BTreeSet<_>>()
        .len();
    log::info!(
        "startup_helper_change_started item_count={} source_count={}",
        requests.len(),
        source_count
    );
    let nonce = unique_nonce();
    let paths = message_paths(&nonce)?;
    let request_message = HelperRequest {
        protocol: PROTOCOL.to_owned(),
        nonce: nonce.clone(),
        items: requests
            .iter()
            .map(|request| HelperRequestItem {
                source_id: request.source_id.clone(),
                provider_item_id: request.provider_item_id.clone(),
                expected_artifact_digest: artifact_digest(&request.expected_artifact),
                desired_state: request.desired_state.into(),
            })
            .collect(),
        #[cfg(target_os = "macos")]
        interactive_user_id: unsafe { libc::geteuid() },
    };
    if let Err(error) = write_private_message(&paths.request, &request_message) {
        let _ = fs::remove_file(&paths.request);
        return Err(error);
    }
    let launch_result = launch_elevated(&paths.request, &paths.response, authorization_prompt);
    let response_result = launch_result.and_then(|()| read_message(&paths.response));
    let _ = fs::remove_file(&paths.request);
    let _ = fs::remove_file(&paths.response);
    let response: HelperResponse = match response_result {
        Ok(response) => response,
        Err(error) => {
            log::warn!(
                "startup_helper_change_failed item_count={} source_count={} failure_stage=launch_or_response reason={:?} mutation_state={:?} diagnostic_digest={}",
                requests.len(),
                source_count,
                error.code(),
                error.mutation_state(),
                blake3::hash(error.as_bytes()).to_hex()
            );
            return Err(error);
        }
    };
    if response.protocol != PROTOCOL || response.nonce != nonce {
        log::warn!(
            "startup_helper_change_failed item_count={} source_count={} failure_stage=response_validation reason={:?} validation_reason=correlation_mismatch",
            requests.len(),
            source_count,
            PlatformErrorCode::InvalidData
        );
        return Err(PlatformError::new(
            PlatformErrorCode::InvalidData,
            "startup helper response correlation failed",
        ));
    }
    if response.items.len() != requests.len() {
        log::warn!(
            "startup_helper_change_failed item_count={} source_count={} failure_stage=response_validation reason={:?} validation_reason=result_count_mismatch response_item_count={}",
            requests.len(),
            source_count,
            PlatformErrorCode::InvalidData,
            response.items.len()
        );
        return Err(PlatformError::new(
            PlatformErrorCode::InvalidData,
            "startup helper response item count is invalid",
        ));
    }
    let failed_count = response
        .items
        .iter()
        .filter(|item| item.error_code.is_some() || item.outcome.is_none())
        .count();
    log::info!(
        "startup_helper_change_finished item_count={} source_count={} succeeded_count={} failed_count={}",
        requests.len(),
        source_count,
        requests.len().saturating_sub(failed_count),
        failed_count
    );
    Ok(response
        .items
        .into_iter()
        .map(|item| {
            if let Some(error_code) = item.error_code {
                return Err(PlatformError::new(
                    error_code.into(),
                    "startup helper rejected the requested change",
                ));
            }
            let outcome = item.outcome.ok_or_else(|| {
                PlatformError::new(
                    PlatformErrorCode::InvalidData,
                    "startup helper returned no change outcome",
                )
            })?;
            Ok(PlatformStartupChangeResult {
                previous_state: outcome.previous_state.into(),
                configured_state: outcome.configured_state.into(),
                verified: outcome.verified,
            })
        })
        .collect())
}

struct MessagePaths {
    request: PathBuf,
    response: PathBuf,
}

fn message_paths(nonce: &str) -> PlatformResult<MessagePaths> {
    let directory = helper_temporary_directory();
    if !directory.is_absolute() {
        return Err(PlatformError::invalid_path(
            "startup helper temporary directory is not absolute",
        ));
    }
    Ok(MessagePaths {
        request: directory.join(format!("mangodisk-startup-{nonce}.request.json")),
        response: directory.join(format!("mangodisk-startup-{nonce}.response.json")),
    })
}

fn helper_paths(arguments: &[OsString]) -> PlatformResult<(PathBuf, PathBuf)> {
    if arguments.len() != 4 {
        return Err(PlatformError::new(
            PlatformErrorCode::InvalidData,
            "startup helper argument count is invalid",
        ));
    }
    let request = PathBuf::from(&arguments[2]);
    let response = PathBuf::from(&arguments[3]);
    if !request.is_absolute() || !response.is_absolute() || request == response {
        return Err(PlatformError::invalid_path(
            "startup helper message paths are invalid",
        ));
    }
    let temporary_directory = helper_temporary_directory();
    if request.parent() != Some(temporary_directory.as_path())
        || response.parent() != Some(temporary_directory.as_path())
    {
        return Err(PlatformError::invalid_path(
            "startup helper messages are outside the temporary directory",
        ));
    }
    Ok((request, response))
}

#[cfg(target_os = "macos")]
fn helper_temporary_directory() -> PathBuf {
    PathBuf::from("/private/tmp")
}

#[cfg(not(target_os = "macos"))]
fn helper_temporary_directory() -> PathBuf {
    std::env::temp_dir()
}

fn execute_helper_request(
    request_path: &Path,
    response_path: &Path,
) -> PlatformResult<HelperResponse> {
    validate_request_file(request_path)?;
    let request: HelperRequest = read_message(request_path)?;
    if request.protocol != PROTOCOL || !valid_nonce(&request.nonce) {
        return Err(PlatformError::new(
            PlatformErrorCode::InvalidData,
            "startup helper request protocol is invalid",
        ));
    }
    let expected_paths = message_paths(&request.nonce)?;
    if request_path != expected_paths.request || response_path != expected_paths.response {
        return Err(PlatformError::invalid_path(
            "startup helper message names do not match the request nonce",
        ));
    }
    let has_invalid_state = request.items.iter().any(|item| {
        !matches!(
            item.desired_state,
            WireState::Enabled | WireState::Disabled | WireState::Removed
        )
    });
    if request.items.is_empty() || request.items.len() > MAX_BATCH_ITEMS || has_invalid_state {
        return Err(PlatformError::new(
            PlatformErrorCode::InvalidData,
            "startup helper batch payload is invalid",
        ));
    }
    #[cfg(target_os = "macos")]
    validate_macos_request_owner(request_path, request.interactive_user_id)?;
    let dispatch_items = helper_dispatch_items(&request.items);
    let outcomes = platform_helper_change_many(&dispatch_items, interactive_user_id(&request));
    if outcomes.len() != dispatch_items.len() {
        return Err(PlatformError::new(
            PlatformErrorCode::InvalidData,
            "startup helper platform batch result count is invalid",
        ));
    }
    let items = outcomes
        .into_iter()
        .map(|outcome| match outcome {
            Ok(outcome) => HelperResponseItem {
                outcome: Some(WireOutcome {
                    previous_state: outcome.previous_state.into(),
                    configured_state: outcome.configured_state.into(),
                    verified: outcome.verified,
                }),
                error_code: None,
            },
            Err(error) => HelperResponseItem {
                outcome: None,
                error_code: Some(error.code().into()),
            },
        })
        .collect();
    Ok(HelperResponse {
        protocol: PROTOCOL.to_owned(),
        nonce: request.nonce,
        items,
    })
}

fn helper_dispatch_items(items: &[HelperRequestItem]) -> Vec<StartupHelperChangeRequest> {
    items
        .iter()
        .map(|item| StartupHelperChangeRequest {
            source_id: item.source_id.clone(),
            provider_item_id: item.provider_item_id.clone(),
            expected_artifact_digest: item.expected_artifact_digest.clone(),
            desired_state: item.desired_state.into(),
        })
        .collect()
}

#[cfg(target_os = "macos")]
fn interactive_user_id(request: &HelperRequest) -> u32 {
    request.interactive_user_id
}

#[cfg(not(target_os = "macos"))]
fn interactive_user_id(_request: &HelperRequest) -> u32 {
    0
}

fn platform_helper_change_many(
    requests: &[StartupHelperChangeRequest],
    _interactive_user_id: u32,
) -> Vec<PlatformResult<PlatformStartupChangeResult>> {
    #[cfg(windows)]
    return crate::windows::startup_helper_change_many(requests);
    #[cfg(target_os = "macos")]
    return crate::macos::startup_helper_change_many(requests, _interactive_user_id);
    #[cfg(not(any(windows, target_os = "macos")))]
    requests
        .iter()
        .map(|_| {
            Err(PlatformError::new(
                PlatformErrorCode::Unsupported,
                "startup helper is unavailable on this platform",
            ))
        })
        .collect()
}

pub(crate) fn artifact_digest(artifact: &PlatformStartupArtifact) -> String {
    // The digest authorizes a configured-state mutation, so it deliberately excludes volatile
    // runtime observations and display metadata. A scheduled task may start or stop while the
    // user reads the confirmation or accepts UAC; that does not change the target being toggled.
    // Provider identity, target, triggers, scope, capability, configured state, backing path,
    // modification token, and safety diagnostics remain covered so meaningful drift fails closed.
    let mutation_snapshot = (
        &artifact.provider_item_id,
        artifact.source_kind,
        artifact.scope,
        &artifact.triggers,
        &artifact.configuration_path,
        &artifact.target,
        artifact.configured_state,
        artifact.control_capability,
        artifact.modified_at_ms,
        &artifact.diagnostics,
    );
    blake3::hash(format!("{mutation_snapshot:?}").as_bytes())
        .to_hex()
        .to_string()
}

fn validate_request_file(path: &Path) -> PlatformResult<()> {
    let metadata = fs::symlink_metadata(path)
        .map_err(|error| PlatformError::io("inspect startup helper request", &error))?;
    if !metadata.is_file()
        || metadata.file_type().is_symlink()
        || metadata.len() > MAX_MESSAGE_BYTES
    {
        return Err(PlatformError::new(
            PlatformErrorCode::InvalidData,
            "startup helper request file is invalid",
        ));
    }
    Ok(())
}

#[cfg(target_os = "macos")]
fn validate_macos_request_owner(path: &Path, interactive_user_id: u32) -> PlatformResult<()> {
    use std::os::unix::fs::MetadataExt;

    let metadata = fs::metadata(path)
        .map_err(|error| PlatformError::io("inspect startup helper request owner", &error))?;
    if interactive_user_id == 0 || metadata.uid() != interactive_user_id {
        return Err(PlatformError::new(
            PlatformErrorCode::AccessDenied,
            "startup helper request owner is invalid",
        ));
    }
    Ok(())
}

fn read_message<T: for<'de> Deserialize<'de>>(path: &Path) -> PlatformResult<T> {
    let mut file = fs::File::open(path)
        .map_err(|error| PlatformError::io("open startup helper message", &error))?;
    let metadata = file
        .metadata()
        .map_err(|error| PlatformError::io("inspect startup helper message", &error))?;
    if metadata.len() > MAX_MESSAGE_BYTES {
        return Err(PlatformError::new(
            PlatformErrorCode::InvalidData,
            "startup helper message exceeds its size limit",
        ));
    }
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    file.read_to_end(&mut bytes)
        .map_err(|error| PlatformError::io("read startup helper message", &error))?;
    serde_json::from_slice(&bytes).map_err(|_| {
        PlatformError::new(
            PlatformErrorCode::InvalidData,
            "startup helper message is invalid",
        )
    })
}

fn write_private_message<T: Serialize>(path: &Path, message: &T) -> PlatformResult<()> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options
        .open(path)
        .map_err(|error| PlatformError::io("create startup helper request", &error))?;
    serde_json::to_writer(&mut file, message).map_err(|_| {
        PlatformError::new(
            PlatformErrorCode::InvalidData,
            "serialize startup helper request failed",
        )
    })?;
    file.flush()
        .and_then(|()| file.sync_all())
        .map_err(|error| PlatformError::io("persist startup helper request", &error))
}

fn write_message_new<T: Serialize>(
    path: &Path,
    message: &T,
    #[cfg(unix)] owner_id: u32,
    #[cfg(not(unix))] _owner_id: (),
) -> PlatformResult<()> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options
        .open(path)
        .map_err(|error| PlatformError::io("create startup helper response", &error))?;
    serde_json::to_writer(&mut file, message).map_err(|_| {
        PlatformError::new(
            PlatformErrorCode::InvalidData,
            "serialize startup helper response failed",
        )
    })?;
    file.flush()
        .and_then(|()| file.sync_all())
        .map_err(|error| PlatformError::io("persist startup helper response", &error))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        let result =
            unsafe { libc::fchown(std::os::fd::AsRawFd::as_raw_fd(&file), owner_id, u32::MAX) };
        if result != 0 {
            return Err(PlatformError::io(
                "assign startup helper response owner",
                &std::io::Error::last_os_error(),
            ));
        }
        fs::set_permissions(path, fs::Permissions::from_mode(0o600))
            .map_err(|error| PlatformError::io("protect startup helper response", &error))?;
    }
    Ok(())
}

#[cfg(unix)]
fn write_helper_response<T: Serialize>(
    request_path: &Path,
    response_path: &Path,
    message: &T,
) -> PlatformResult<()> {
    write_message_new(response_path, message, response_owner(request_path)?)
}

#[cfg(not(unix))]
fn write_helper_response<T: Serialize>(
    _request_path: &Path,
    response_path: &Path,
    message: &T,
) -> PlatformResult<()> {
    write_message_new(response_path, message, ())
}

#[cfg(unix)]
fn response_owner(request_path: &Path) -> PlatformResult<u32> {
    use std::os::unix::fs::MetadataExt;

    fs::metadata(request_path)
        .map(|metadata| metadata.uid())
        .map_err(|error| PlatformError::io("inspect startup helper response owner", &error))
}

fn unique_nonce() -> String {
    let sequence = NEXT_REQUEST_ID.fetch_add(1, Ordering::Relaxed);
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let digest = blake3::hash(format!("{}-{timestamp}-{sequence}", std::process::id()).as_bytes());
    digest.to_hex()[..32].to_string()
}

fn valid_nonce(value: &str) -> bool {
    value.len() == 32 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

impl From<PlatformStartupDesiredState> for WireState {
    fn from(value: PlatformStartupDesiredState) -> Self {
        match value {
            PlatformStartupDesiredState::Enabled => Self::Enabled,
            PlatformStartupDesiredState::Disabled => Self::Disabled,
            PlatformStartupDesiredState::Removed => Self::Removed,
        }
    }
}

impl From<WireState> for PlatformStartupDesiredState {
    fn from(value: WireState) -> Self {
        match value {
            WireState::Enabled => Self::Enabled,
            WireState::Disabled => Self::Disabled,
            WireState::Removed => Self::Removed,
            WireState::Unknown | WireState::NotApplicable => Self::Disabled,
        }
    }
}

impl From<PlatformStartupConfiguredState> for WireState {
    fn from(value: PlatformStartupConfiguredState) -> Self {
        match value {
            PlatformStartupConfiguredState::Enabled => Self::Enabled,
            PlatformStartupConfiguredState::Disabled => Self::Disabled,
            PlatformStartupConfiguredState::Unknown => Self::Unknown,
            PlatformStartupConfiguredState::NotApplicable => Self::NotApplicable,
        }
    }
}

impl From<WireState> for PlatformStartupConfiguredState {
    fn from(value: WireState) -> Self {
        match value {
            WireState::Enabled => Self::Enabled,
            WireState::Disabled => Self::Disabled,
            WireState::Removed => Self::NotApplicable,
            WireState::Unknown => Self::Unknown,
            WireState::NotApplicable => Self::NotApplicable,
        }
    }
}

impl From<PlatformErrorCode> for WireErrorCode {
    fn from(value: PlatformErrorCode) -> Self {
        match value {
            PlatformErrorCode::AccessDenied => Self::AccessDenied,
            PlatformErrorCode::UserCancelled => Self::UserCancelled,
            PlatformErrorCode::ItemChanged => Self::ItemChanged,
            PlatformErrorCode::InvalidData => Self::InvalidData,
            PlatformErrorCode::InvalidPath => Self::InvalidPath,
            PlatformErrorCode::Io => Self::Io,
            PlatformErrorCode::OperationFailed => Self::OperationFailed,
            PlatformErrorCode::Unsupported => Self::Unsupported,
        }
    }
}

impl From<WireErrorCode> for PlatformErrorCode {
    fn from(value: WireErrorCode) -> Self {
        match value {
            WireErrorCode::AccessDenied => Self::AccessDenied,
            WireErrorCode::UserCancelled => Self::UserCancelled,
            WireErrorCode::ItemChanged => Self::ItemChanged,
            WireErrorCode::InvalidData => Self::InvalidData,
            WireErrorCode::InvalidPath => Self::InvalidPath,
            WireErrorCode::Io => Self::Io,
            WireErrorCode::OperationFailed => Self::OperationFailed,
            WireErrorCode::Unsupported => Self::Unsupported,
        }
    }
}

#[cfg(windows)]
fn launch_elevated(
    request: &Path,
    response: &Path,
    _authorization_prompt: Option<&str>,
) -> PlatformResult<()> {
    windows_launcher::launch(request, response)
}

#[cfg(target_os = "macos")]
fn launch_elevated(
    request: &Path,
    response: &Path,
    authorization_prompt: Option<&str>,
) -> PlatformResult<()> {
    macos_launcher::launch(request, response, authorization_prompt)
}

#[cfg(not(any(windows, target_os = "macos")))]
fn launch_elevated(
    _request: &Path,
    _response: &Path,
    _authorization_prompt: Option<&str>,
) -> PlatformResult<()> {
    Err(PlatformError::new(
        PlatformErrorCode::Unsupported,
        "startup helper is unavailable on this platform",
    ))
}

#[cfg(windows)]
mod windows_launcher {
    use std::os::windows::ffi::OsStrExt;

    use windows_sys::Win32::{
        Foundation::{CloseHandle, GetLastError, ERROR_CANCELLED, WAIT_OBJECT_0},
        System::Threading::{GetExitCodeProcess, WaitForSingleObject, INFINITE},
        UI::{
            Shell::{ShellExecuteExW, SEE_MASK_NOCLOSEPROCESS, SHELLEXECUTEINFOW},
            WindowsAndMessaging::SW_HIDE,
        },
    };

    use super::*;

    pub(super) fn launch(request: &Path, response: &Path) -> PlatformResult<()> {
        let executable = std::env::current_exe()
            .map_err(|error| PlatformError::io("resolve startup helper executable", &error))?;
        if !executable.is_absolute() || !executable.is_file() {
            return Err(PlatformError::invalid_path(
                "startup helper executable is invalid",
            ));
        }
        let executable = wide(executable.as_os_str());
        let verb = wide(std::ffi::OsStr::new("runas"));
        let arguments = wide(std::ffi::OsStr::new(&format!(
            "{HELPER_FLAG} {} {}",
            quote_argument(request)?,
            quote_argument(response)?
        )));
        let mut execution = SHELLEXECUTEINFOW {
            cbSize: size_of::<SHELLEXECUTEINFOW>() as u32,
            fMask: SEE_MASK_NOCLOSEPROCESS,
            lpVerb: verb.as_ptr(),
            lpFile: executable.as_ptr(),
            lpParameters: arguments.as_ptr(),
            nShow: SW_HIDE,
            ..unsafe { std::mem::zeroed() }
        };
        if unsafe { ShellExecuteExW(&mut execution) } == 0 {
            let code = unsafe { GetLastError() };
            return Err(PlatformError::new(
                if code == ERROR_CANCELLED {
                    PlatformErrorCode::UserCancelled
                } else {
                    PlatformErrorCode::OperationFailed
                },
                "startup helper elevation request failed",
            ));
        }
        if execution.hProcess.is_null() {
            return Err(PlatformError::operation_failed(
                "startup helper process handle is unavailable",
            ));
        }
        let wait = unsafe { WaitForSingleObject(execution.hProcess, INFINITE) };
        let mut exit_code = HELPER_FAILURE_EXIT_CODE as u32;
        let exit_read = unsafe { GetExitCodeProcess(execution.hProcess, &mut exit_code) };
        unsafe {
            CloseHandle(execution.hProcess);
        }
        if wait != WAIT_OBJECT_0 || exit_read == 0 || exit_code != HELPER_SUCCESS_EXIT_CODE as u32 {
            return Err(PlatformError::operation_failed(
                "startup helper process failed",
            ));
        }
        Ok(())
    }

    fn quote_argument(path: &Path) -> PlatformResult<String> {
        let value = path.to_str().ok_or_else(|| {
            PlatformError::invalid_path("startup helper message path is not valid UTF-8")
        })?;
        if value.contains(['\r', '\n', '"']) {
            return Err(PlatformError::invalid_path(
                "startup helper message path contains unsupported characters",
            ));
        }
        Ok(format!("\"{value}\""))
    }

    fn wide(value: &std::ffi::OsStr) -> Vec<u16> {
        value.encode_wide().chain(std::iter::once(0)).collect()
    }
}

#[cfg(target_os = "macos")]
mod macos_launcher {
    use std::process::Command;

    use super::*;

    const AUTHORIZATION_SCRIPT: &str = include_str!("macos/privileged_startup_change.applescript");
    const MAX_AUTHORIZATION_PROMPT_CHARS: usize = 120;

    pub(super) fn launch(
        request: &Path,
        response: &Path,
        authorization_prompt: Option<&str>,
    ) -> PlatformResult<()> {
        let executable = std::env::current_exe()
            .map_err(|error| PlatformError::io("resolve startup helper executable", &error))?;
        if !executable.is_absolute() || !executable.is_file() {
            return Err(PlatformError::invalid_path(
                "startup helper executable is invalid",
            ));
        }
        let authorization_prompt = validate_authorization_prompt(authorization_prompt)?;
        let output = Command::new("/usr/bin/osascript")
            .args(["-e", AUTHORIZATION_SCRIPT, "--"])
            .arg(executable)
            .arg(request)
            .arg(response)
            // The localized prompt remains process data and is never inserted
            // into AppleScript source or the privileged shell command.
            .arg(authorization_prompt.unwrap_or_default())
            .output()
            .map_err(|error| PlatformError::io("launch startup helper authorization", &error))?;
        if !output.status.success() {
            let code = if String::from_utf8_lossy(&output.stderr).contains("(-128)") {
                PlatformErrorCode::UserCancelled
            } else {
                PlatformErrorCode::AccessDenied
            };
            return Err(PlatformError::new(
                code,
                "startup helper authorization was rejected",
            ));
        }
        Ok(())
    }

    fn validate_authorization_prompt(prompt: Option<&str>) -> PlatformResult<Option<&str>> {
        let Some(prompt) = prompt.map(str::trim) else {
            return Ok(None);
        };
        if prompt.is_empty()
            || prompt.chars().count() > MAX_AUTHORIZATION_PROMPT_CHARS
            || prompt.chars().any(char::is_control)
        {
            return Err(PlatformError::new(
                PlatformErrorCode::InvalidData,
                "startup helper authorization prompt is invalid",
            ));
        }
        Ok(Some(prompt))
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn authorization_prompt_is_bounded_and_passed_as_script_data() {
            assert_eq!(
                validate_authorization_prompt(Some("  MangoDisk needs approval.  "))
                    .expect("a concise prompt should be accepted"),
                Some("MangoDisk needs approval.")
            );
            assert!(validate_authorization_prompt(Some("\n")).is_err());
            assert!(validate_authorization_prompt(Some("line one\nline two")).is_err());
            assert!(validate_authorization_prompt(Some(
                &"a".repeat(MAX_AUTHORIZATION_PROMPT_CHARS + 1)
            ))
            .is_err());

            assert!(AUTHORIZATION_SCRIPT.contains("set promptText to item 4 of argv"));
            assert!(AUTHORIZATION_SCRIPT.contains(
                "do shell script helperCommand with prompt promptText with administrator privileges"
            ));
            assert!(!AUTHORIZATION_SCRIPT.contains("MangoDisk needs approval."));
        }

        #[test]
        fn authorization_script_compiles_with_macos_tool() {
            let compiled_script = std::env::temp_dir().join(format!(
                "mangodisk-startup-authorization-script-{}-{}.scpt",
                std::process::id(),
                NEXT_REQUEST_ID.fetch_add(1, Ordering::Relaxed)
            ));
            let output = Command::new("/usr/bin/osacompile")
                .arg("-e")
                .arg(AUTHORIZATION_SCRIPT)
                .arg("-o")
                .arg(&compiled_script)
                .output()
                .expect("launch the macOS AppleScript compiler");
            let diagnostic = String::from_utf8_lossy(&output.stderr).into_owned();
            let _ = fs::remove_file(compiled_script);

            assert!(
                output.status.success(),
                "authorization script should compile: {diagnostic}"
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        PlatformStartupControlCapability, PlatformStartupDiagnosticCode,
        PlatformStartupIdentityConfidence, PlatformStartupOwner, PlatformStartupRuntimeState,
        PlatformStartupScope, PlatformStartupSourceKind, PlatformStartupSummarySource,
        PlatformStartupTarget, PlatformStartupTargetKind, PlatformStartupTrigger,
        PlatformStartupTrustState,
    };

    #[test]
    fn helper_mode_ignores_normal_application_arguments() {
        assert_eq!(run_startup_helper_mode([OsString::from("MangoDisk")]), None);
    }

    #[test]
    fn helper_paths_reject_relative_message_paths() {
        let arguments = vec![
            OsString::from("MangoDisk"),
            OsString::from(HELPER_FLAG),
            OsString::from("request.json"),
            OsString::from("response.json"),
        ];
        assert!(helper_paths(&arguments).is_err());
    }

    #[test]
    fn helper_paths_reject_messages_outside_the_fixed_directory() {
        let outside = helper_temporary_directory().join("mangodisk-helper-outside");
        let arguments = vec![
            OsString::from("MangoDisk"),
            OsString::from(HELPER_FLAG),
            outside.join("request.json").into_os_string(),
            outside.join("response.json").into_os_string(),
        ];

        assert!(helper_paths(&arguments).is_err());
    }

    #[test]
    fn helper_request_rejects_file_names_that_do_not_match_the_nonce() {
        let request_nonce = unique_nonce();
        let path_nonce = unique_nonce();
        let paths = message_paths(&path_nonce).expect("helper paths must be available");
        let request = HelperRequest {
            protocol: PROTOCOL.to_owned(),
            nonce: request_nonce,
            items: vec![HelperRequestItem {
                source_id: "test.startup".to_owned(),
                provider_item_id: "fixture".to_owned(),
                expected_artifact_digest: "fixture-digest".to_owned(),
                desired_state: WireState::Disabled,
            }],
            #[cfg(target_os = "macos")]
            interactive_user_id: unsafe { libc::geteuid() },
        };
        write_private_message(&paths.request, &request)
            .expect("the isolated helper request fixture must be created");

        let result = execute_helper_request(&paths.request, &paths.response);
        let _ = fs::remove_file(&paths.request);
        let _ = fs::remove_file(&paths.response);

        assert!(result.is_err());
    }

    #[test]
    fn helper_request_rejects_an_empty_batch_before_platform_dispatch() {
        let nonce = unique_nonce();
        let paths = message_paths(&nonce).expect("helper paths must be available");
        let request = HelperRequest {
            protocol: PROTOCOL.to_owned(),
            nonce,
            items: Vec::new(),
            #[cfg(target_os = "macos")]
            interactive_user_id: unsafe { libc::geteuid() },
        };
        write_private_message(&paths.request, &request)
            .expect("the isolated helper request fixture must be created");

        let result = execute_helper_request(&paths.request, &paths.response);
        let _ = fs::remove_file(&paths.request);
        let _ = fs::remove_file(&paths.response);

        assert!(result.is_err());
        let error = result.expect_err("an empty helper payload must fail");
        assert_eq!(error.code(), PlatformErrorCode::InvalidData);
    }

    #[test]
    fn privileged_batch_rejects_empty_requests_before_launching() {
        let result = change_many_with_privileges(&[], None);

        assert!(result.is_err());
        assert_eq!(
            result.expect_err("an empty helper batch must fail").code(),
            PlatformErrorCode::InvalidData
        );
    }

    #[test]
    fn helper_v2_batch_preserves_removed_state_and_item_order() {
        let request = HelperRequest {
            protocol: PROTOCOL.to_owned(),
            nonce: "0123456789abcdef0123456789abcdef".to_owned(),
            items: vec![
                HelperRequestItem {
                    source_id: "test.first".to_owned(),
                    provider_item_id: "first".to_owned(),
                    expected_artifact_digest: "first-digest".to_owned(),
                    desired_state: WireState::Removed,
                },
                HelperRequestItem {
                    source_id: "test.second".to_owned(),
                    provider_item_id: "second".to_owned(),
                    expected_artifact_digest: "second-digest".to_owned(),
                    desired_state: WireState::Disabled,
                },
            ],
            #[cfg(target_os = "macos")]
            interactive_user_id: unsafe { libc::geteuid() },
        };

        let encoded = serde_json::to_vec(&request).expect("helper request must serialize");
        let decoded: HelperRequest =
            serde_json::from_slice(&encoded).expect("helper request must deserialize");
        let dispatch = helper_dispatch_items(&decoded.items);

        assert_eq!(decoded.protocol, "mangodisk-startup-helper-v2");
        assert_eq!(dispatch.len(), 2);
        assert_eq!(dispatch[0].provider_item_id, "first");
        assert_eq!(
            dispatch[0].desired_state,
            PlatformStartupDesiredState::Removed
        );
        assert_eq!(dispatch[1].provider_item_id, "second");
        assert_eq!(
            PlatformStartupConfiguredState::from(WireState::Removed),
            PlatformStartupConfiguredState::NotApplicable
        );
    }

    #[test]
    fn artifact_digest_ignores_runtime_and_display_only_drift() {
        let original = digest_fixture();
        let mut observed_later = original.clone();
        observed_later.runtime_state = PlatformStartupRuntimeState::Running;
        observed_later.display_name = "Updated display label".to_owned();
        observed_later.owner.version = Some("2.0".to_owned());
        observed_later.trust = PlatformStartupTrustState::Verified;

        assert_eq!(artifact_digest(&original), artifact_digest(&observed_later));
    }

    #[test]
    fn artifact_digest_rejects_mutation_relevant_drift() {
        let original = digest_fixture();
        let mut changed_state = original.clone();
        changed_state.configured_state = PlatformStartupConfiguredState::Disabled;
        let mut changed_target = original.clone();
        changed_target.target.arguments.push("--changed".to_owned());
        let mut changed_capability = original.clone();
        changed_capability.control_capability = PlatformStartupControlCapability::SystemManaged;

        assert_ne!(artifact_digest(&original), artifact_digest(&changed_state));
        assert_ne!(artifact_digest(&original), artifact_digest(&changed_target));
        assert_ne!(
            artifact_digest(&original),
            artifact_digest(&changed_capability)
        );
    }

    fn digest_fixture() -> PlatformStartupArtifact {
        PlatformStartupArtifact {
            provider_item_id: "task:\\mangodisk-digest-fixture".to_owned(),
            source_kind: PlatformStartupSourceKind::ScheduledTask,
            scope: PlatformStartupScope::User,
            triggers: vec![PlatformStartupTrigger::UserLogon],
            display_name: "Digest fixture".to_owned(),
            configuration_path: None,
            target: PlatformStartupTarget {
                kind: PlatformStartupTargetKind::Executable,
                identity_key: "path:c:\\windows\\system32\\cmd.exe".to_owned(),
                path: Some(PathBuf::from(r"C:\Windows\System32\cmd.exe")),
                executable_name: Some("cmd.exe".to_owned()),
                arguments: vec!["/c".to_owned(), "exit 0".to_owned()],
            },
            owner: PlatformStartupOwner {
                identity_key: Some("path:c:\\windows\\system32\\cmd.exe".to_owned()),
                name: Some("Digest fixture".to_owned()),
                publisher: None,
                summary: None,
                summary_source: PlatformStartupSummarySource::SourceLabel,
                version: Some("1.0".to_owned()),
                icon_path: None,
                confidence: PlatformStartupIdentityConfidence::Strong,
            },
            configured_state: PlatformStartupConfiguredState::Enabled,
            runtime_state: PlatformStartupRuntimeState::Stopped,
            control_capability: PlatformStartupControlCapability::ElevationRequired,
            trust: PlatformStartupTrustState::Unknown,
            modified_at_ms: None,
            diagnostics: vec![PlatformStartupDiagnosticCode::UnsupportedFormat],
        }
    }
}

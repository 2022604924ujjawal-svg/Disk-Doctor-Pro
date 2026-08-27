use std::{
    ffi::{OsStr, OsString},
    fs::{self, OpenOptions},
    io::{Read, Write},
    os::windows::ffi::OsStrExt,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
    time::{SystemTime, UNIX_EPOCH},
};

use serde::{Deserialize, Serialize};
use windows_sys::Win32::{
    Foundation::{CloseHandle, GetLastError, ERROR_CANCELLED, WAIT_OBJECT_0},
    System::Threading::{GetExitCodeProcess, WaitForSingleObject, INFINITE},
    UI::{
        Shell::{ShellExecuteExW, SEE_MASK_NOCLOSEPROCESS, SHELLEXECUTEINFOW},
        WindowsAndMessaging::SW_HIDE,
    },
};

use crate::{
    PlatformError, PlatformErrorCode, PlatformMutationState, PlatformResult,
    PlatformSystemSettingChangeRequest, PlatformSystemSettingChangeResult,
};

const HELPER_FLAG: &str = "--mangodisk-system-settings-helper-v2";
const PROTOCOL: &str = "mangodisk-system-settings-helper-v2";
const MAX_MESSAGE_BYTES: u64 = 1024 * 1024;
const MAX_BATCH_ITEMS: usize = 256;
const HELPER_SUCCESS_EXIT_CODE: i32 = 0;
const HELPER_FAILURE_EXIT_CODE: i32 = 70;
static NEXT_REQUEST_ID: AtomicU64 = AtomicU64::new(1);

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct HelperRequest {
    protocol: String,
    nonce: String,
    items: Vec<PlatformSystemSettingChangeRequest>,
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
    outcome: Option<PlatformSystemSettingChangeResult>,
    error_code: Option<WireErrorCode>,
    mutation_possible: bool,
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

struct MessagePaths {
    request: PathBuf,
    response: PathBuf,
}

/// Runs the narrow privileged-settings helper before Tauri initializes.
///
/// The helper accepts only typed setting identifiers compiled into MangoDisk. It never accepts a
/// registry path or command from the desktop process, which keeps the elevation boundary finite
/// even if the WebView payload is compromised.
pub fn run_system_settings_helper_mode<I>(arguments: I) -> Option<i32>
where
    I: IntoIterator<Item = OsString>,
{
    let arguments = arguments.into_iter().collect::<Vec<_>>();
    if arguments.get(1).and_then(|value| value.to_str()) != Some(HELPER_FLAG) {
        return None;
    }
    let exit_code = match helper_paths(&arguments).and_then(|(paths, request_digest)| {
        execute_helper_request(&paths.request, &paths.response, &request_digest)
            .and_then(|response| write_message_new(&paths.response, &response).map(|_| ()))
    }) {
        Ok(()) => HELPER_SUCCESS_EXIT_CODE,
        Err(_) => HELPER_FAILURE_EXIT_CODE,
    };
    Some(exit_code)
}

/// Applies every privilege-required setting through one temporary elevated process.
///
/// A batch avoids repeated UAC prompts. The elevated process still re-reads each value, checks the
/// optimistic-concurrency snapshot, applies the allowlisted mutation, and verifies the result.
pub(crate) fn change_many_with_privileges(
    requests: &[&PlatformSystemSettingChangeRequest],
) -> PlatformResult<Vec<PlatformResult<PlatformSystemSettingChangeResult>>> {
    if requests.is_empty() || requests.len() > MAX_BATCH_ITEMS {
        return Err(PlatformError::new(
            PlatformErrorCode::InvalidData,
            "system settings helper batch size is invalid",
        ));
    }
    let nonce = unique_nonce();
    let paths = message_paths(&nonce);
    let request = HelperRequest {
        protocol: PROTOCOL.to_owned(),
        nonce: nonce.clone(),
        items: requests.iter().map(|request| (*request).clone()).collect(),
    };
    let request_digest = match write_message_new(&paths.request, &request) {
        Ok(digest) => digest,
        Err(error) => {
            let _ = fs::remove_file(&paths.request);
            return Err(error);
        }
    };
    let response = launch_elevated(&paths, &request_digest).and_then(|()| {
        read_message::<HelperResponse>(&paths.response)
            .map_err(PlatformError::with_possible_side_effects)
    });
    let _ = fs::remove_file(&paths.request);
    let _ = fs::remove_file(&paths.response);
    let response = response?;
    if response.protocol != PROTOCOL || response.nonce != nonce {
        return Err(PlatformError::new(
            PlatformErrorCode::InvalidData,
            "system settings helper response correlation failed",
        )
        .with_possible_side_effects());
    }
    if response.items.len() != requests.len() {
        return Err(PlatformError::new(
            PlatformErrorCode::InvalidData,
            "system settings helper response item count is invalid",
        )
        .with_possible_side_effects());
    }
    Ok(response
        .items
        .into_iter()
        .map(|item| match (item.outcome, item.error_code) {
            (Some(outcome), None) if !item.mutation_possible => Ok(outcome),
            (None, Some(code)) => {
                let error = PlatformError::new(
                    code.into(),
                    "system settings helper rejected the requested change",
                );
                Err(if item.mutation_possible {
                    error.with_possible_side_effects()
                } else {
                    error
                })
            }
            _ => Err(PlatformError::new(
                PlatformErrorCode::InvalidData,
                "system settings helper returned an invalid item",
            )
            .with_possible_side_effects()),
        })
        .collect())
}

fn execute_helper_request(
    request_path: &Path,
    response_path: &Path,
    request_digest: &str,
) -> PlatformResult<HelperResponse> {
    validate_request_file(request_path)?;
    let request: HelperRequest = read_message_verified(request_path, request_digest)?;
    if request.protocol != PROTOCOL || !valid_nonce(&request.nonce) {
        return Err(PlatformError::new(
            PlatformErrorCode::InvalidData,
            "system settings helper request protocol is invalid",
        ));
    }
    let expected = message_paths(&request.nonce);
    if request_path != expected.request || response_path != expected.response {
        return Err(PlatformError::new(
            PlatformErrorCode::InvalidPath,
            "system settings helper message paths are invalid",
        ));
    }
    if request.items.is_empty() || request.items.len() > MAX_BATCH_ITEMS {
        return Err(PlatformError::new(
            PlatformErrorCode::InvalidData,
            "system settings helper request batch is invalid",
        ));
    }
    let outcomes = crate::windows::system_settings_helper_change_many(&request.items);
    let items = outcomes
        .into_iter()
        .map(|outcome| match outcome {
            Ok(outcome) => HelperResponseItem {
                outcome: Some(outcome),
                error_code: None,
                mutation_possible: false,
            },
            Err(error) => HelperResponseItem {
                outcome: None,
                error_code: Some(error.code().into()),
                mutation_possible: error.mutation_state() == PlatformMutationState::MayHaveChanged,
            },
        })
        .collect();
    Ok(HelperResponse {
        protocol: PROTOCOL.to_owned(),
        nonce: request.nonce,
        items,
    })
}

fn helper_paths(arguments: &[OsString]) -> PlatformResult<(MessagePaths, String)> {
    if arguments.len() != 5 {
        return Err(PlatformError::new(
            PlatformErrorCode::InvalidData,
            "system settings helper argument count is invalid",
        ));
    }
    let paths = MessagePaths {
        request: PathBuf::from(&arguments[2]),
        response: PathBuf::from(&arguments[3]),
    };
    let request_digest = arguments[4].to_str().ok_or_else(|| {
        PlatformError::new(
            PlatformErrorCode::InvalidData,
            "system settings helper request digest is invalid",
        )
    })?;
    if !paths.request.is_absolute()
        || !paths.response.is_absolute()
        || paths.request == paths.response
    {
        return Err(PlatformError::new(
            PlatformErrorCode::InvalidPath,
            "system settings helper paths are invalid",
        ));
    }
    if !valid_digest(request_digest) {
        return Err(PlatformError::new(
            PlatformErrorCode::InvalidData,
            "system settings helper request digest is invalid",
        ));
    }
    Ok((paths, request_digest.to_string()))
}

fn message_paths(nonce: &str) -> MessagePaths {
    let directory = std::env::temp_dir();
    MessagePaths {
        request: directory.join(format!("mangodisk-system-settings-{nonce}.request.json")),
        response: directory.join(format!("mangodisk-system-settings-{nonce}.response.json")),
    }
}

fn validate_request_file(path: &Path) -> PlatformResult<()> {
    let metadata = fs::symlink_metadata(path)
        .map_err(|error| PlatformError::io("inspect system settings helper request", &error))?;
    if !metadata.is_file()
        || metadata.file_type().is_symlink()
        || metadata.len() > MAX_MESSAGE_BYTES
    {
        return Err(PlatformError::new(
            PlatformErrorCode::InvalidData,
            "system settings helper request file is invalid",
        ));
    }
    Ok(())
}

fn read_message<T: for<'de> Deserialize<'de>>(path: &Path) -> PlatformResult<T> {
    let bytes = read_message_bytes(path)?;
    deserialize_message(&bytes)
}

fn read_message_verified<T: for<'de> Deserialize<'de>>(
    path: &Path,
    expected_digest: &str,
) -> PlatformResult<T> {
    let bytes = read_message_bytes(path)?;
    if !message_digest_matches(&bytes, expected_digest) {
        return Err(PlatformError::new(
            PlatformErrorCode::InvalidData,
            "system settings helper request integrity check failed",
        ));
    }
    deserialize_message(&bytes)
}

fn read_message_bytes(path: &Path) -> PlatformResult<Vec<u8>> {
    let file = fs::File::open(path)
        .map_err(|error| PlatformError::io("open system settings helper message", &error))?;
    let metadata = file
        .metadata()
        .map_err(|error| PlatformError::io("inspect system settings helper message", &error))?;
    if metadata.len() > MAX_MESSAGE_BYTES {
        return Err(PlatformError::new(
            PlatformErrorCode::InvalidData,
            "system settings helper message is too large",
        ));
    }
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    file.take(MAX_MESSAGE_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| PlatformError::io("read system settings helper message", &error))?;
    if bytes.len() as u64 > MAX_MESSAGE_BYTES {
        return Err(PlatformError::new(
            PlatformErrorCode::InvalidData,
            "system settings helper message is too large",
        ));
    }
    Ok(bytes)
}

fn deserialize_message<T: for<'de> Deserialize<'de>>(bytes: &[u8]) -> PlatformResult<T> {
    serde_json::from_slice(bytes).map_err(|_| {
        PlatformError::new(
            PlatformErrorCode::InvalidData,
            "system settings helper message is invalid",
        )
    })
}

fn write_message_new<T: Serialize>(path: &Path, message: &T) -> PlatformResult<String> {
    let bytes = serde_json::to_vec(message).map_err(|_| {
        PlatformError::new(
            PlatformErrorCode::InvalidData,
            "serialize system settings helper message failed",
        )
    })?;
    if bytes.len() as u64 > MAX_MESSAGE_BYTES {
        return Err(PlatformError::new(
            PlatformErrorCode::InvalidData,
            "system settings helper message is too large",
        ));
    }
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|error| PlatformError::io("create system settings helper message", &error))?;
    file.write_all(&bytes)
        .map_err(|error| PlatformError::io("write system settings helper message", &error))?;
    file.flush()
        .and_then(|()| file.sync_all())
        .map_err(|error| PlatformError::io("persist system settings helper message", &error))?;
    Ok(blake3::hash(&bytes).to_hex().to_string())
}

fn launch_elevated(paths: &MessagePaths, request_digest: &str) -> PlatformResult<()> {
    let executable = std::env::current_exe()
        .map_err(|error| PlatformError::io("resolve system settings helper executable", &error))?;
    if !executable.is_absolute() || !executable.is_file() {
        return Err(PlatformError::new(
            PlatformErrorCode::InvalidPath,
            "system settings helper executable is invalid",
        ));
    }
    let executable = wide(executable.as_os_str());
    let verb = wide(OsStr::new("runas"));
    let arguments = wide(OsStr::new(&format!(
        "{HELPER_FLAG} {} {} {request_digest}",
        quote_argument(&paths.request)?,
        quote_argument(&paths.response)?
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
            "system settings helper elevation request failed",
        ));
    }
    if execution.hProcess.is_null() {
        return Err(PlatformError::operation_failed(
            "system settings helper process handle is unavailable",
        ));
    }
    let wait = unsafe { WaitForSingleObject(execution.hProcess, INFINITE) };
    let mut exit_code = HELPER_FAILURE_EXIT_CODE as u32;
    let exit_read = unsafe { GetExitCodeProcess(execution.hProcess, &mut exit_code) };
    unsafe { CloseHandle(execution.hProcess) };
    if wait != WAIT_OBJECT_0 || exit_read == 0 || exit_code != HELPER_SUCCESS_EXIT_CODE as u32 {
        return Err(
            PlatformError::operation_failed("system settings helper process failed")
                .with_possible_side_effects(),
        );
    }
    Ok(())
}

fn quote_argument(path: &Path) -> PlatformResult<String> {
    let value = path.to_str().ok_or_else(|| {
        PlatformError::new(
            PlatformErrorCode::InvalidPath,
            "system settings helper path is not valid UTF-8",
        )
    })?;
    if value.contains(['\r', '\n', '"']) {
        return Err(PlatformError::new(
            PlatformErrorCode::InvalidPath,
            "system settings helper path contains unsupported characters",
        ));
    }
    Ok(format!("\"{value}\""))
}

fn wide(value: &OsStr) -> Vec<u16> {
    value.encode_wide().chain(std::iter::once(0)).collect()
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

fn valid_digest(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn message_digest_matches(bytes: &[u8], expected_digest: &str) -> bool {
    blake3::hash(bytes).to_hex().as_str() == expected_digest
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normal_launch_does_not_enter_helper_mode() {
        assert_eq!(
            run_system_settings_helper_mode([OsString::from("MangoDisk")]),
            None
        );
    }

    #[test]
    fn helper_nonce_is_fixed_width_hex() {
        assert!(valid_nonce(&unique_nonce()));
    }

    #[test]
    fn helper_digest_requires_a_full_blake3_hex_value() {
        let digest = blake3::hash(b"request").to_hex().to_string();

        assert!(valid_digest(&digest));
        assert!(!valid_digest(&digest[..32]));
        assert!(!valid_digest(&"z".repeat(64)));
    }

    #[test]
    fn helper_digest_rejects_tampered_request_content() {
        let digest = blake3::hash(b"original request").to_hex().to_string();

        assert!(message_digest_matches(b"original request", &digest));
        assert!(!message_digest_matches(b"modified request", &digest));
    }
}

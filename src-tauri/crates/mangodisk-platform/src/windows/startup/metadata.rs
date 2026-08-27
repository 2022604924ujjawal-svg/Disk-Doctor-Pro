use std::ffi::c_void;
use std::mem::size_of;
use std::path::Path;
use std::slice;

use windows::Win32::Storage::FileSystem::{
    GetFileVersionInfoSizeW, GetFileVersionInfoW, VerQueryValueW,
};
use windows::Win32::{
    Foundation::{HANDLE, HWND, TRUST_E_NOSIGNATURE},
    Security::WinTrust::{
        WinVerifyTrust, WINTRUST_ACTION_GENERIC_VERIFY_V2, WINTRUST_DATA, WINTRUST_DATA_0,
        WINTRUST_FILE_INFO, WTD_CACHE_ONLY_URL_RETRIEVAL, WTD_CHOICE_FILE, WTD_REVOKE_NONE,
        WTD_STATEACTION_IGNORE, WTD_UI_NONE,
    },
};
use windows_core::{HSTRING, PCWSTR, PWSTR};

use crate::PlatformStartupTrustState;

const MAX_VERSION_RESOURCE_BYTES: u32 = 16 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum FilesystemTargetState {
    Present,
    Missing,
    Unknown,
}

#[derive(Debug, Default)]
pub(super) struct FileVersionMetadata {
    pub product_name: Option<String>,
    pub description: Option<String>,
    pub company_name: Option<String>,
    pub product_version: Option<String>,
}

pub(super) fn file_version_metadata(path: &Path) -> Option<FileVersionMetadata> {
    if !path.is_file() {
        return None;
    }
    let path = HSTRING::from(path.as_os_str());
    let size = unsafe { GetFileVersionInfoSizeW(&path, None) };
    if size == 0 || size > MAX_VERSION_RESOURCE_BYTES {
        return None;
    }
    let mut buffer = vec![0u8; size as usize];
    unsafe { GetFileVersionInfoW(&path, None, size, buffer.as_mut_ptr().cast()) }.ok()?;
    let translations = version_translations(&buffer);
    let (language, code_page) = translations.first().copied().unwrap_or((0x0409, 0x04b0));
    Some(FileVersionMetadata {
        product_name: version_string(&buffer, language, code_page, "ProductName"),
        description: version_string(&buffer, language, code_page, "FileDescription"),
        company_name: version_string(&buffer, language, code_page, "CompanyName"),
        product_version: version_string(&buffer, language, code_page, "ProductVersion"),
    })
}

pub(super) fn startup_trust(
    path: Option<&Path>,
    system_candidate: bool,
) -> PlatformStartupTrustState {
    let Some(path) = path.filter(|path| path.is_file()) else {
        return PlatformStartupTrustState::Unknown;
    };
    let path = HSTRING::from(path.as_os_str());
    let mut file = WINTRUST_FILE_INFO {
        cbStruct: size_of::<WINTRUST_FILE_INFO>() as u32,
        pcwszFilePath: PCWSTR(path.as_ptr()),
        hFile: HANDLE::default(),
        pgKnownSubject: std::ptr::null_mut(),
    };
    let mut data = WINTRUST_DATA {
        cbStruct: size_of::<WINTRUST_DATA>() as u32,
        pPolicyCallbackData: std::ptr::null_mut(),
        pSIPClientData: std::ptr::null_mut(),
        dwUIChoice: WTD_UI_NONE,
        fdwRevocationChecks: WTD_REVOKE_NONE,
        dwUnionChoice: WTD_CHOICE_FILE,
        Anonymous: WINTRUST_DATA_0 { pFile: &mut file },
        dwStateAction: WTD_STATEACTION_IGNORE,
        hWVTStateData: HANDLE::default(),
        pwszURLReference: PWSTR::null(),
        dwProvFlags: WTD_CACHE_ONLY_URL_RETRIEVAL,
        dwUIContext: Default::default(),
        pSignatureSettings: std::ptr::null_mut(),
    };
    let mut action = WINTRUST_ACTION_GENERIC_VERIFY_V2;
    let status = unsafe {
        WinVerifyTrust(
            HWND::default(),
            &mut action,
            (&mut data as *mut WINTRUST_DATA).cast(),
        )
    };
    if status == 0 {
        if system_candidate {
            PlatformStartupTrustState::System
        } else {
            PlatformStartupTrustState::Verified
        }
    } else if status == TRUST_E_NOSIGNATURE.0 {
        PlatformStartupTrustState::Unsigned
    } else {
        PlatformStartupTrustState::Invalid
    }
}

/// Distinguishes a missing local target from an unavailable filesystem boundary.
///
/// Remote paths are intentionally never classified as missing because a disconnected share is
/// indistinguishable from a deleted target without performing network I/O. Local drive roots must
/// also be available before absence can become evidence for destructive orphan cleanup.
pub(super) fn filesystem_target_state(path: &Path) -> FilesystemTargetState {
    if !path.is_absolute() || path.to_string_lossy().starts_with(r"\\") {
        return FilesystemTargetState::Unknown;
    }
    let Some(root) = path
        .ancestors()
        .last()
        .filter(|root| !root.as_os_str().is_empty())
    else {
        return FilesystemTargetState::Unknown;
    };
    if !matches!(root.try_exists(), Ok(true)) {
        return FilesystemTargetState::Unknown;
    }
    match path.try_exists() {
        Ok(true) => FilesystemTargetState::Present,
        Ok(false) => FilesystemTargetState::Missing,
        Err(_) => FilesystemTargetState::Unknown,
    }
}

fn version_translations(buffer: &[u8]) -> Vec<(u16, u16)> {
    let query = HSTRING::from(r"\VarFileInfo\Translation");
    let mut pointer = std::ptr::null_mut::<c_void>();
    let mut byte_length = 0u32;
    if !unsafe {
        VerQueryValueW(
            buffer.as_ptr().cast(),
            &query,
            &mut pointer,
            &mut byte_length,
        )
    }
    .as_bool()
        || pointer.is_null()
        || byte_length < 4
    {
        return Vec::new();
    }
    let words = unsafe { slice::from_raw_parts(pointer.cast::<u16>(), byte_length as usize / 2) };
    words
        .chunks_exact(2)
        .map(|pair| (pair[0], pair[1]))
        .collect()
}

fn version_string(buffer: &[u8], language: u16, code_page: u16, key: &str) -> Option<String> {
    let query = HSTRING::from(format!(
        r"\StringFileInfo\{language:04x}{code_page:04x}\{key}"
    ));
    let mut pointer = std::ptr::null_mut::<c_void>();
    let mut character_length = 0u32;
    if !unsafe {
        VerQueryValueW(
            buffer.as_ptr().cast(),
            &query,
            &mut pointer,
            &mut character_length,
        )
    }
    .as_bool()
        || pointer.is_null()
        || character_length == 0
    {
        return None;
    }
    let value = unsafe {
        slice::from_raw_parts(
            pointer.cast::<u16>(),
            character_length.saturating_sub(1) as usize,
        )
    };
    let value = String::from_utf16_lossy(value).trim().to_owned();
    (!value.is_empty()).then_some(value)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn missing_file_has_no_version_metadata() {
        assert!(file_version_metadata(Path::new(r"C:\missing\fixture.exe")).is_none());
    }

    #[test]
    fn remote_target_is_never_claimed_as_missing() {
        assert_eq!(
            filesystem_target_state(Path::new(r"\\unavailable\share\agent.exe")),
            FilesystemTargetState::Unknown
        );
    }

    #[test]
    #[ignore = "requires a Windows system image"]
    fn system_binary_has_version_metadata_and_valid_trust() {
        let system_root = std::env::var_os("SystemRoot").expect("SystemRoot must be available");
        let target = PathBuf::from(system_root).join(r"System32\kernel32.dll");
        let metadata =
            file_version_metadata(&target).expect("kernel32 must expose version metadata");
        assert!(metadata.product_version.is_some());
        assert_eq!(
            startup_trust(Some(&target), true),
            PlatformStartupTrustState::System
        );
    }
}

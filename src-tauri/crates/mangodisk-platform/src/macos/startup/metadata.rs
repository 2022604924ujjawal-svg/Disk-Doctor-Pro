use std::{
    collections::HashMap,
    ffi::{c_char, c_void},
    os::unix::ffi::OsStrExt,
    path::{Path, PathBuf},
    ptr,
    sync::{Mutex, OnceLock},
};

use crate::PlatformStartupTrustState;

type CFAllocatorRef = *const c_void;
type CFDictionaryRef = *const c_void;
type CFStringRef = *const c_void;
type CFTypeRef = *const c_void;
type CFURLRef = *const c_void;
type SecRequirementRef = *const c_void;
type SecStaticCodeRef = *const c_void;
type OSStatus = i32;

const K_CF_STRING_ENCODING_UTF8: u32 = 0x0800_0100;
const K_SEC_CS_SIGNING_INFORMATION: u32 = 1 << 1;
const ERR_SEC_SUCCESS: OSStatus = 0;
const ERR_SEC_CS_UNSIGNED: OSStatus = -67062;
const METADATA_TEXT_BYTES: usize = 1024;

#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    fn CFDictionaryGetValue(dictionary: CFDictionaryRef, key: *const c_void) -> *const c_void;
    fn CFRelease(value: CFTypeRef);
    fn CFStringGetCString(
        string: CFStringRef,
        buffer: *mut c_char,
        buffer_size: isize,
        encoding: u32,
    ) -> bool;
    fn CFURLCreateFromFileSystemRepresentation(
        allocator: CFAllocatorRef,
        bytes: *const u8,
        length: isize,
        is_directory: bool,
    ) -> CFURLRef;
}

#[link(name = "Security", kind = "framework")]
unsafe extern "C" {
    static kSecCodeInfoTeamIdentifier: CFStringRef;
    fn SecCodeCopySigningInformation(
        code: SecStaticCodeRef,
        flags: u32,
        information: *mut CFDictionaryRef,
    ) -> OSStatus;
    fn SecStaticCodeCheckValidity(
        code: SecStaticCodeRef,
        flags: u32,
        requirement: SecRequirementRef,
    ) -> OSStatus;
    fn SecStaticCodeCreateWithPath(
        path: CFURLRef,
        flags: u32,
        code: *mut SecStaticCodeRef,
    ) -> OSStatus;
}

#[derive(Clone, Debug)]
pub(super) struct CodeSignatureMetadata {
    pub publisher: Option<String>,
    pub team_id: Option<String>,
    pub trust: PlatformStartupTrustState,
}

pub(super) fn code_signature_metadata(
    target: &Path,
    system_candidate: bool,
) -> CodeSignatureMetadata {
    if system_candidate {
        return CodeSignatureMetadata {
            publisher: Some("Apple".to_owned()),
            team_id: None,
            trust: PlatformStartupTrustState::System,
        };
    }
    if !target.exists() {
        return unknown_signature();
    }
    let key = target.to_path_buf();
    if let Some(metadata) = signature_cache()
        .lock()
        .ok()
        .and_then(|cache| cache.get(&key).cloned())
    {
        return metadata;
    }
    let metadata = inspect_signature(target);
    if let Ok(mut cache) = signature_cache().lock() {
        cache.insert(key, metadata.clone());
    }
    metadata
}

fn inspect_signature(target: &Path) -> CodeSignatureMetadata {
    let bytes = target.as_os_str().as_bytes();
    let url = unsafe {
        CFURLCreateFromFileSystemRepresentation(
            ptr::null(),
            bytes.as_ptr(),
            bytes.len() as isize,
            target.is_dir(),
        )
    };
    if url.is_null() {
        return unknown_signature();
    }
    let mut code = ptr::null();
    let create_status = unsafe { SecStaticCodeCreateWithPath(url, 0, &mut code) };
    unsafe {
        CFRelease(url);
    }
    if create_status != ERR_SEC_SUCCESS || code.is_null() {
        return unknown_signature();
    }
    let validity = unsafe { SecStaticCodeCheckValidity(code, 0, ptr::null()) };
    let team_id = signing_team_id(code);
    unsafe {
        CFRelease(code);
    }
    CodeSignatureMetadata {
        publisher: team_id.clone(),
        team_id,
        trust: match validity {
            ERR_SEC_SUCCESS => PlatformStartupTrustState::Verified,
            ERR_SEC_CS_UNSIGNED => PlatformStartupTrustState::Unsigned,
            _ => PlatformStartupTrustState::Invalid,
        },
    }
}

fn signing_team_id(code: SecStaticCodeRef) -> Option<String> {
    let mut information = ptr::null();
    if unsafe {
        SecCodeCopySigningInformation(code, K_SEC_CS_SIGNING_INFORMATION, &mut information)
    } != ERR_SEC_SUCCESS
        || information.is_null()
    {
        return None;
    }
    let value = unsafe { CFDictionaryGetValue(information, kSecCodeInfoTeamIdentifier.cast()) };
    let result = cf_string(value.cast());
    unsafe {
        CFRelease(information);
    }
    result
}

fn cf_string(value: CFStringRef) -> Option<String> {
    if value.is_null() {
        return None;
    }
    let mut buffer = [0u8; METADATA_TEXT_BYTES];
    let copied = unsafe {
        CFStringGetCString(
            value,
            buffer.as_mut_ptr().cast(),
            buffer.len() as isize,
            K_CF_STRING_ENCODING_UTF8,
        )
    };
    if !copied {
        return None;
    }
    let length = buffer.iter().position(|byte| *byte == 0)?;
    String::from_utf8(buffer[..length].to_vec()).ok()
}

fn unknown_signature() -> CodeSignatureMetadata {
    CodeSignatureMetadata {
        publisher: None,
        team_id: None,
        trust: PlatformStartupTrustState::Unknown,
    }
}

fn signature_cache() -> &'static Mutex<HashMap<PathBuf, CodeSignatureMetadata>> {
    static CACHE: OnceLock<Mutex<HashMap<PathBuf, CodeSignatureMetadata>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn system_candidate_uses_sealed_system_trust() {
        let metadata = code_signature_metadata(Path::new("/System/Library/CoreServices"), true);
        assert_eq!(metadata.trust, PlatformStartupTrustState::System);
    }

    #[test]
    fn unsigned_temporary_file_is_not_reported_as_verified() {
        let path = std::env::temp_dir().join(format!(
            "mangodisk-signature-fixture-{}",
            std::process::id()
        ));
        std::fs::write(&path, b"fixture").expect("write signature fixture");
        let metadata = code_signature_metadata(&path, false);
        let _ = std::fs::remove_file(path);
        assert_ne!(metadata.trust, PlatformStartupTrustState::Verified);
    }
}

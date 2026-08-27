use std::collections::BTreeSet;
use std::ffi::{c_char, c_void, CStr};
use std::os::unix::ffi::OsStrExt;
use std::path::Path;
use std::path::PathBuf;
use std::ptr;
use std::time::Instant;

use crate::{
    PlatformCancellation, PlatformError, PlatformErrorCode, PlatformResult,
    PlatformStartupArtifact, PlatformStartupConfiguredState, PlatformStartupControlCapability,
    PlatformStartupCoverageReason, PlatformStartupCoverageStatus, PlatformStartupDiagnosticCode,
    PlatformStartupIdentityConfidence, PlatformStartupOwner, PlatformStartupRuntimeState,
    PlatformStartupScope, PlatformStartupSourceKind, PlatformStartupSourceResult,
    PlatformStartupSummarySource, PlatformStartupTarget, PlatformStartupTargetKind,
    PlatformStartupTrigger, PlatformStartupTrustState,
};

use super::embedded::{bundle_name, read_bundle_metadata, string_value};

type CFIndex = isize;
type CFAllocatorRef = *const c_void;
type CFArrayRef = *const c_void;
type CFErrorRef = *const c_void;
type CFStringRef = *const c_void;
type CFTypeRef = *const c_void;
type CFURLRef = *const c_void;
type LSSharedFileListRef = *const c_void;
type LSSharedFileListItemRef = *const c_void;

const K_CF_STRING_ENCODING_UTF8: u32 = 0x0800_0100;
const K_CF_URL_POSIX_PATH_STYLE: i32 = 0;
const K_LS_SHARED_FILE_LIST_NO_USER_INTERACTION: u32 = 1;
const K_LS_SHARED_FILE_LIST_DO_NOT_MOUNT_VOLUMES: u32 = 2;
const PATH_BUFFER_BYTES: usize = 32_768;

#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    fn CFArrayGetCount(array: CFArrayRef) -> CFIndex;
    fn CFArrayGetValueAtIndex(array: CFArrayRef, index: CFIndex) -> *const c_void;
    fn CFRelease(value: CFTypeRef);
    fn CFStringGetCString(
        string: CFStringRef,
        buffer: *mut c_char,
        buffer_size: CFIndex,
        encoding: u32,
    ) -> bool;
    fn CFURLCopyFileSystemPath(url: CFURLRef, path_style: i32) -> CFStringRef;
    fn CFURLCreateFromFileSystemRepresentation(
        allocator: CFAllocatorRef,
        buffer: *const u8,
        buffer_length: CFIndex,
        is_directory: bool,
    ) -> CFURLRef;
}

#[link(name = "CoreServices", kind = "framework")]
unsafe extern "C" {
    static kLSSharedFileListSessionLoginItems: CFStringRef;
    static kLSSharedFileListItemLast: LSSharedFileListItemRef;
    fn LSSharedFileListCreate(
        allocator: CFAllocatorRef,
        list_type: CFStringRef,
        list_options: CFTypeRef,
    ) -> LSSharedFileListRef;
    fn LSSharedFileListCopySnapshot(list: LSSharedFileListRef, seed: *mut u32) -> CFArrayRef;
    fn LSSharedFileListItemCopyResolvedURL(
        item: LSSharedFileListItemRef,
        flags: u32,
        error: *mut CFErrorRef,
    ) -> CFURLRef;
    fn LSSharedFileListItemRemove(list: LSSharedFileListRef, item: LSSharedFileListItemRef) -> i32;
    fn LSSharedFileListInsertItemURL(
        list: LSSharedFileListRef,
        after_item: LSSharedFileListItemRef,
        display_name: CFStringRef,
        icon: CFTypeRef,
        url: CFURLRef,
        properties_to_set: CFTypeRef,
        properties_to_clear: CFTypeRef,
    ) -> LSSharedFileListItemRef;
}

pub(super) fn scan(cancellation: &PlatformCancellation) -> PlatformStartupSourceResult {
    let started = Instant::now();
    let list = unsafe {
        LSSharedFileListCreate(ptr::null(), kLSSharedFileListSessionLoginItems, ptr::null())
    };
    if list.is_null() {
        return unavailable(started, PlatformStartupCoverageReason::ApiUnavailable);
    }
    let snapshot = unsafe { LSSharedFileListCopySnapshot(list, ptr::null_mut()) };
    if snapshot.is_null() {
        unsafe {
            CFRelease(list);
        }
        return unavailable(started, PlatformStartupCoverageReason::ApiUnavailable);
    }
    let count = unsafe { CFArrayGetCount(snapshot) };
    let mut items = Vec::new();
    let mut partial_reason = None;
    for index in 0..count {
        if cancellation.is_cancelled() {
            unsafe {
                CFRelease(snapshot);
                CFRelease(list);
            }
            return result(
                items,
                PlatformStartupCoverageStatus::Cancelled,
                Some(PlatformStartupCoverageReason::Cancelled),
                started,
            );
        }
        let item = unsafe { CFArrayGetValueAtIndex(snapshot, index) };
        let Some(path) = resolve_item_path(item) else {
            partial_reason.get_or_insert(PlatformStartupCoverageReason::InvalidData);
            continue;
        };
        items.push(login_item_artifact(path));
    }
    unsafe {
        CFRelease(snapshot);
        CFRelease(list);
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

pub(super) fn enabled_paths() -> PlatformResult<BTreeSet<PathBuf>> {
    let (list, snapshot) = list_snapshot()?;
    let count = unsafe { CFArrayGetCount(snapshot) };
    let mut paths = BTreeSet::new();
    for index in 0..count {
        let item = unsafe { CFArrayGetValueAtIndex(snapshot, index) };
        if let Some(path) = resolve_item_path(item) {
            paths.insert(path);
        }
    }
    unsafe {
        CFRelease(snapshot);
        CFRelease(list);
    }
    Ok(paths)
}

pub(super) fn set_enabled(path: &Path, enabled: bool) -> PlatformResult<()> {
    let (list, snapshot) = list_snapshot()?;
    let count = unsafe { CFArrayGetCount(snapshot) };
    let mut matching_item = None;
    for index in 0..count {
        let item = unsafe { CFArrayGetValueAtIndex(snapshot, index) };
        if resolve_item_path(item).as_deref() == Some(path) {
            matching_item = Some(item);
            break;
        }
    }

    let result = if enabled {
        if matching_item.is_some() {
            Ok(())
        } else {
            insert_item(list, path)
        }
    } else if let Some(item) = matching_item {
        let status = unsafe { LSSharedFileListItemRemove(list, item) };
        if status == 0 {
            Ok(())
        } else {
            Err(PlatformError::new(
                PlatformErrorCode::OperationFailed,
                "macOS rejected the login item removal",
            ))
        }
    } else {
        Ok(())
    };

    unsafe {
        CFRelease(snapshot);
        CFRelease(list);
    }
    result
}

fn list_snapshot() -> PlatformResult<(LSSharedFileListRef, CFArrayRef)> {
    let list = unsafe {
        LSSharedFileListCreate(ptr::null(), kLSSharedFileListSessionLoginItems, ptr::null())
    };
    if list.is_null() {
        return Err(PlatformError::new(
            PlatformErrorCode::Unsupported,
            "macOS login item list is unavailable",
        ));
    }
    let snapshot = unsafe { LSSharedFileListCopySnapshot(list, ptr::null_mut()) };
    if snapshot.is_null() {
        unsafe {
            CFRelease(list);
        }
        return Err(PlatformError::new(
            PlatformErrorCode::OperationFailed,
            "macOS login item snapshot is unavailable",
        ));
    }
    Ok((list, snapshot))
}

fn insert_item(list: LSSharedFileListRef, path: &Path) -> PlatformResult<()> {
    let bytes = path.as_os_str().as_bytes();
    if bytes.is_empty() || !path.exists() {
        return Err(PlatformError::invalid_path(
            "login item application path is unavailable",
        ));
    }
    let url = unsafe {
        CFURLCreateFromFileSystemRepresentation(
            ptr::null(),
            bytes.as_ptr(),
            bytes.len() as CFIndex,
            path.is_dir(),
        )
    };
    if url.is_null() {
        return Err(PlatformError::invalid_path(
            "login item application URL is invalid",
        ));
    }
    let item = unsafe {
        LSSharedFileListInsertItemURL(
            list,
            kLSSharedFileListItemLast,
            ptr::null(),
            ptr::null(),
            url,
            ptr::null(),
            ptr::null(),
        )
    };
    unsafe {
        CFRelease(url);
    }
    if item.is_null() {
        return Err(PlatformError::new(
            PlatformErrorCode::OperationFailed,
            "macOS rejected the login item insertion",
        ));
    }
    unsafe {
        CFRelease(item);
    }
    Ok(())
}

fn resolve_item_path(item: *const c_void) -> Option<PathBuf> {
    if item.is_null() {
        return None;
    }
    let url = unsafe {
        LSSharedFileListItemCopyResolvedURL(
            item,
            K_LS_SHARED_FILE_LIST_NO_USER_INTERACTION | K_LS_SHARED_FILE_LIST_DO_NOT_MOUNT_VOLUMES,
            ptr::null_mut(),
        )
    };
    if url.is_null() {
        return None;
    }
    let path_string = unsafe { CFURLCopyFileSystemPath(url, K_CF_URL_POSIX_PATH_STYLE) };
    unsafe {
        CFRelease(url);
    }
    if path_string.is_null() {
        return None;
    }
    let mut buffer = vec![0u8; PATH_BUFFER_BYTES];
    let copied = unsafe {
        CFStringGetCString(
            path_string,
            buffer.as_mut_ptr().cast(),
            buffer.len() as CFIndex,
            K_CF_STRING_ENCODING_UTF8,
        )
    };
    unsafe {
        CFRelease(path_string);
    }
    if !copied {
        return None;
    }
    let value = CStr::from_bytes_until_nul(&buffer).ok()?.to_str().ok()?;
    (!value.is_empty()).then(|| PathBuf::from(value))
}

fn login_item_artifact(path: PathBuf) -> PlatformStartupArtifact {
    let metadata = read_bundle_metadata(&path);
    let bundle_id = metadata
        .as_ref()
        .and_then(|metadata| string_value(metadata, "CFBundleIdentifier"));
    let name = metadata
        .as_ref()
        .and_then(bundle_name)
        .or_else(|| {
            path.file_stem()
                .map(|value| value.to_string_lossy().into_owned())
        })
        .unwrap_or_else(|| "Login item".to_string());
    let executable_name = metadata
        .as_ref()
        .and_then(|metadata| string_value(metadata, "CFBundleExecutable"));
    let target_path = executable_name
        .as_deref()
        .map(|executable| path.join("Contents/MacOS").join(executable))
        .filter(|target| target.exists())
        .or_else(|| Some(path.clone()));
    let identity = bundle_id
        .clone()
        .map(|value| format!("bundle:{value}"))
        .unwrap_or_else(|| format!("path:{}", path.to_string_lossy()));
    let mut diagnostics = Vec::new();
    if bundle_id.is_none() {
        diagnostics.push(PlatformStartupDiagnosticCode::MissingIdentity);
    }
    if !path.exists() {
        diagnostics.push(PlatformStartupDiagnosticCode::MissingTarget);
    }
    PlatformStartupArtifact {
        provider_item_id: format!("login-item:{identity}"),
        source_kind: PlatformStartupSourceKind::LoginItem,
        scope: PlatformStartupScope::CurrentUser,
        triggers: vec![PlatformStartupTrigger::UserLogon],
        display_name: name.clone(),
        configuration_path: None,
        target: PlatformStartupTarget {
            kind: if path.extension().is_some_and(|value| value == "app") {
                PlatformStartupTargetKind::Application
            } else {
                PlatformStartupTargetKind::Executable
            },
            identity_key: identity.clone(),
            path: target_path,
            executable_name,
            arguments: Vec::new(),
        },
        owner: PlatformStartupOwner {
            identity_key: Some(identity),
            name: Some(name),
            publisher: None,
            summary: None,
            summary_source: PlatformStartupSummarySource::BundleMetadata,
            version: metadata
                .as_ref()
                .and_then(|metadata| string_value(metadata, "CFBundleShortVersionString")),
            icon_path: Some(path),
            confidence: if bundle_id.is_some() {
                PlatformStartupIdentityConfidence::Exact
            } else {
                PlatformStartupIdentityConfidence::Probable
            },
        },
        configured_state: PlatformStartupConfiguredState::Enabled,
        runtime_state: PlatformStartupRuntimeState::Unknown,
        control_capability: PlatformStartupControlCapability::RemoveOnly,
        trust: PlatformStartupTrustState::Unknown,
        modified_at_ms: None,
        diagnostics,
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
        source_id: "macos.login_items".to_string(),
        required: true,
        status,
        reason,
        items,
        elapsed_ms: started.elapsed().as_millis() as u64,
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use std::path::PathBuf;

    use super::{enabled_paths, login_item_artifact, set_enabled};
    use crate::{PlatformStartupConfiguredState, PlatformStartupControlCapability};

    #[test]
    fn legacy_login_item_presence_is_enabled_but_remove_only() {
        let item = login_item_artifact(PathBuf::from("/missing/Example.app"));

        assert_eq!(
            item.configured_state,
            PlatformStartupConfiguredState::Enabled
        );
        assert_eq!(
            item.control_capability,
            PlatformStartupControlCapability::RemoveOnly
        );
    }

    #[test]
    #[ignore = "changes the login item selected by MANGODISK_TEST_LOGIN_ITEM_PATH and restores it"]
    fn toggles_and_restores_an_installed_login_item() {
        let path = std::env::var_os("MANGODISK_TEST_LOGIN_ITEM_PATH")
            .map(PathBuf::from)
            .expect("MANGODISK_TEST_LOGIN_ITEM_PATH must select a test application");
        let originally_enabled = enabled_paths().unwrap().contains(Path::new(&path));
        let desired = !originally_enabled;
        let change_result = set_enabled(&path, desired)
            .and_then(|()| enabled_paths().map(|paths| paths.contains(Path::new(&path))));
        let restore_result = set_enabled(&path, originally_enabled)
            .and_then(|()| enabled_paths().map(|paths| paths.contains(Path::new(&path))));

        assert_eq!(change_result.unwrap(), desired);
        assert_eq!(restore_result.unwrap(), originally_enabled);
    }
}

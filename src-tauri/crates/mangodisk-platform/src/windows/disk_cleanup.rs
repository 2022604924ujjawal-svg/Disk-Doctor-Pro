use std::{
    collections::HashMap,
    ffi::{c_void, OsString},
    fs,
    os::windows::ffi::OsStringExt,
    os::windows::fs::MetadataExt,
    path::{Component, Path, Prefix},
    sync::{Mutex, OnceLock},
    time::{Duration, Instant, SystemTime},
};

use windows::{
    core::{implement, Interface, GUID, HRESULT, PCWSTR, PWSTR},
    Win32::{
        Foundation::{E_ABORT, RPC_E_CHANGED_MODE, S_FALSE},
        System::{
            Com::{
                CoCreateInstance, CoInitializeEx, CoTaskMemFree, CoUninitialize,
                CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED,
            },
            Registry::HKEY,
        },
        UI::LegacyWindowsEnvironmentFeatures::{
            IEmptyVolumeCache, IEmptyVolumeCache2, IEmptyVolumeCacheCallBack,
            IEmptyVolumeCacheCallBack_Impl,
        },
        UI::Shell::{
            SHEmptyRecycleBinW, SHQueryRecycleBinW, SHERB_NOCONFIRMATION, SHERB_NOPROGRESSUI,
            SHERB_NOSOUND, SHQUERYRBINFO,
        },
    },
};
use winreg::{enums::HKEY_LOCAL_MACHINE, RegKey};

use crate::{
    PlatformCancellation, WindowsDiskCleanupAvailability, WindowsDiskCleanupEstimate,
    WindowsDiskCleanupExecution, WindowsDiskCleanupExecutionStatus, WindowsDiskCleanupKind,
};

const VOLUME_CACHES_KEY: &str = r"SOFTWARE\Microsoft\Windows\CurrentVersion\Explorer\VolumeCaches";
const ESTIMATE_CACHE_TTL: Duration = Duration::from_secs(30);
const SYSTEM_LOG_MINIMUM_AGE: Duration = Duration::from_secs(3 * 24 * 60 * 60);
const SYSTEM_LOG_MAXIMUM_DEPTH: usize = 16;
const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
const SYSTEM_LOG_EXTENSIONS: &[&str] = &[
    "bak", "cab", "dmp", "err", "etl", "lo_", "log", "old", "out", "tmp", "txt", "xml",
];

const INTERNET_CACHE_HANDLERS: &[&str] = &["Internet Cache Files"];
const DELIVERY_OPTIMIZATION_HANDLERS: &[&str] = &["Delivery Optimization Files"];
const DEFENDER_CACHE_HANDLERS: &[&str] = &["Windows Defender"];
const UPDATE_CLEANUP_HANDLERS: &[&str] = &["Update Cleanup"];

#[derive(Debug, Clone, Copy)]
struct CachedEstimate {
    measured_at: Instant,
    estimate: WindowsDiskCleanupEstimate,
}

static ESTIMATE_CACHE: OnceLock<Mutex<HashMap<WindowsDiskCleanupKind, CachedEstimate>>> =
    OnceLock::new();

#[derive(Debug)]
struct SystemLogCandidate {
    path: std::path::PathBuf,
    bytes: u64,
}

#[derive(Debug, Default)]
struct SystemLogInventory {
    candidates: Vec<SystemLogCandidate>,
    root_count: u64,
    skipped_count: u64,
}

#[derive(Debug, Clone, Copy)]
struct RecycleBinSnapshot {
    bytes: u64,
    item_count: u64,
}

struct ComApartment {
    uninitialize: bool,
}

impl ComApartment {
    fn initialize() -> Result<Self, String> {
        // Cleanup previews run on ordinary worker threads. Initializing COM at
        // this narrow boundary avoids relying on Tauri's UI-thread apartment.
        let result = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
        if result.is_ok() {
            return Ok(Self { uninitialize: true });
        }
        if result == RPC_E_CHANGED_MODE {
            // The current thread already owns another valid apartment. COM is
            // usable, but this call did not increment its initialization count.
            return Ok(Self {
                uninitialize: false,
            });
        }
        Err(format!(
            "native disk cleanup COM initialization failed: {result:?}"
        ))
    }
}

impl Drop for ComApartment {
    fn drop(&mut self) {
        if self.uninitialize {
            unsafe { CoUninitialize() };
        }
    }
}

struct InitializedHandler {
    handler: IEmptyVolumeCache,
    callback: IEmptyVolumeCacheCallBack,
    handler_name: String,
}

#[implement(IEmptyVolumeCacheCallBack)]
struct CleanupCallback {
    cancellation: PlatformCancellation,
}

#[allow(non_snake_case)]
impl IEmptyVolumeCacheCallBack_Impl for CleanupCallback_Impl {
    fn ScanProgress(
        &self,
        _space_used: u64,
        _flags: u32,
        _status: &PCWSTR,
    ) -> windows::core::Result<()> {
        self.continue_or_cancel()
    }

    fn PurgeProgress(
        &self,
        _space_freed: u64,
        _space_to_free: u64,
        _flags: u32,
        _status: &PCWSTR,
    ) -> windows::core::Result<()> {
        self.continue_or_cancel()
    }
}

impl CleanupCallback_Impl {
    fn continue_or_cancel(&self) -> windows::core::Result<()> {
        if self.cancellation.is_cancelled() {
            Err(windows::core::Error::from_hresult(E_ABORT))
        } else {
            Ok(())
        }
    }
}

impl InitializedHandler {
    fn open(
        registry_key: &RegKey,
        handler_name: &str,
        volume: &str,
        cancellation: PlatformCancellation,
    ) -> Result<Self, String> {
        let handler_label = handler_name.to_string();
        let clsid_text = registry_key
            .get_value::<String, _>("")
            .map_err(|error| format!("cleanup handler class is unavailable: {error:?}"))?;
        let clsid_text = clsid_text
            .strip_prefix('{')
            .and_then(|value| value.strip_suffix('}'))
            .unwrap_or(clsid_text.as_str());
        let clsid = GUID::try_from(clsid_text)
            .map_err(|error| format!("cleanup handler class is invalid: {error:?}"))?;
        let handler =
            unsafe { CoCreateInstance::<_, IEmptyVolumeCache>(&clsid, None, CLSCTX_INPROC_SERVER) }
                .map_err(|error| format!("cleanup handler activation failed: {error:?}"))?;

        let volume = wide_null(volume);
        let handler_name = wide_null(handler_name);
        let mut display_name = PWSTR::null();
        let mut description = PWSTR::null();
        let mut button_text = PWSTR::null();
        let mut flags = Default::default();
        let native_key = HKEY(registry_key.raw_handle());
        let initialize_result = match handler.cast::<IEmptyVolumeCache2>() {
            Ok(handler_v2) => unsafe {
                handler_v2.InitializeEx(
                    native_key,
                    PCWSTR(volume.as_ptr()),
                    PCWSTR(handler_name.as_ptr()),
                    &mut display_name,
                    &mut description,
                    &mut button_text,
                    &mut flags,
                )
            },
            Err(_) => unsafe {
                handler.Initialize(
                    native_key,
                    PCWSTR(volume.as_ptr()),
                    &mut display_name,
                    &mut description,
                    &mut flags,
                )
            },
        };
        free_com_string(display_name);
        free_com_string(description);
        free_com_string(button_text);
        initialize_result
            .map_err(|error| format!("cleanup handler initialization failed: {error:?}"))?;
        Ok(Self {
            handler,
            callback: CleanupCallback { cancellation }.into(),
            handler_name: handler_label,
        })
    }

    fn measure(&self) -> Result<u64, NativeCallError> {
        let mut bytes = 0_u64;
        // The generated projection calls `HRESULT::ok`, which accepts S_FALSE.
        // Disk Cleanup uses S_FALSE to report an unavailable measurement, so
        // retain the raw HRESULT and reject it explicitly.
        let result = unsafe {
            (Interface::vtable(&self.handler).GetSpaceUsed)(
                Interface::as_raw(&self.handler),
                &mut bytes,
                Interface::as_raw(&self.callback),
            )
        };
        measurement_result(result, bytes)
    }

    fn purge(&self, bytes: u64) -> Result<(), NativeCallError> {
        let result = unsafe {
            (Interface::vtable(&self.handler).Purge)(
                Interface::as_raw(&self.handler),
                bytes,
                Interface::as_raw(&self.callback),
            )
        };
        native_call_result(result, "cleanup handler execution failed")
    }
}

trait CleanupHandler {
    fn measure(&self) -> Result<u64, NativeCallError>;
    fn purge(&self, bytes: u64) -> Result<(), NativeCallError>;
}

impl CleanupHandler for InitializedHandler {
    fn measure(&self) -> Result<u64, NativeCallError> {
        InitializedHandler::measure(self)
    }

    fn purge(&self, bytes: u64) -> Result<(), NativeCallError> {
        InitializedHandler::purge(self, bytes)
    }
}

#[derive(Debug)]
enum NativeCallError {
    Cancelled,
    Failed(String),
}

impl NativeCallError {
    fn diagnostic(&self) -> &str {
        match self {
            Self::Cancelled => "cleanup handler operation was cancelled",
            Self::Failed(error) => error,
        }
    }
}

#[derive(Debug)]
enum HandlerExecution {
    Noop,
    Released {
        expected_bytes: u64,
        released_bytes: u64,
    },
    Failed {
        stage: &'static str,
        expected_bytes: u64,
        diagnostic: Option<String>,
    },
    Cancelled {
        expected_bytes: u64,
    },
}

fn execute_handler(handler: &impl CleanupHandler) -> HandlerExecution {
    let before = match handler.measure() {
        Ok(bytes) => bytes,
        Err(NativeCallError::Cancelled) => {
            return HandlerExecution::Cancelled { expected_bytes: 0 };
        }
        Err(error) => {
            return HandlerExecution::Failed {
                stage: "preflight",
                expected_bytes: 0,
                diagnostic: Some(error.diagnostic().to_string()),
            };
        }
    };
    if before == 0 {
        return HandlerExecution::Noop;
    }

    // Purge receives a requested byte target, not an unconditional-delete
    // flag. Passing the measured amount keeps the request bounded to the
    // preflight snapshot and avoids vendor handlers interpreting MAX as an
    // invalid or overflowing quota.
    match handler.purge(before) {
        Ok(()) => {}
        Err(NativeCallError::Cancelled) => {
            return HandlerExecution::Cancelled {
                expected_bytes: before,
            };
        }
        Err(error) => {
            return HandlerExecution::Failed {
                stage: "purge",
                expected_bytes: before,
                diagnostic: Some(error.diagnostic().to_string()),
            };
        }
    }

    let after = match handler.measure() {
        Ok(bytes) => bytes,
        Err(NativeCallError::Cancelled) => {
            return HandlerExecution::Cancelled {
                expected_bytes: before,
            };
        }
        Err(error) => {
            return HandlerExecution::Failed {
                stage: "verify",
                expected_bytes: before,
                diagnostic: Some(error.diagnostic().to_string()),
            };
        }
    };
    let released_bytes = before.saturating_sub(after);
    if released_bytes == 0 {
        // A successful HRESULT only confirms that the handler accepted the
        // request. Treat an unchanged measurement as a verification failure so
        // the product never reports unobserved disk savings.
        HandlerExecution::Failed {
            stage: "verify",
            expected_bytes: before,
            diagnostic: None,
        }
    } else {
        HandlerExecution::Released {
            expected_bytes: before,
            released_bytes,
        }
    }
}

fn measurement_result(result: HRESULT, bytes: u64) -> Result<u64, NativeCallError> {
    // Data-driven handlers keep optional roots in the registry. A root that
    // does not exist means the category currently has zero bytes, not that the
    // whole multi-handler product result is incomplete.
    const FILE_NOT_FOUND: HRESULT = HRESULT(0x80070002_u32 as i32);
    const PATH_NOT_FOUND: HRESULT = HRESULT(0x80070003_u32 as i32);
    if matches!(result, FILE_NOT_FOUND | PATH_NOT_FOUND) {
        return Ok(0);
    }
    if result == E_ABORT {
        return Err(NativeCallError::Cancelled);
    }
    if result == S_FALSE {
        return Err(NativeCallError::Failed(
            "cleanup handler measurement is unavailable".to_string(),
        ));
    }
    if result.is_err() {
        return Err(NativeCallError::Failed(format!(
            "cleanup handler measurement failed: {result:?}"
        )));
    }
    if bytes == u64::MAX {
        return Err(NativeCallError::Failed(
            "cleanup handler returned an unknown size".to_string(),
        ));
    }
    Ok(bytes)
}

fn native_call_result(result: HRESULT, context: &str) -> Result<(), NativeCallError> {
    if result == E_ABORT {
        return Err(NativeCallError::Cancelled);
    }
    if result != HRESULT(0) {
        return Err(NativeCallError::Failed(format!("{context}: {result:?}")));
    }
    Ok(())
}

impl Drop for InitializedHandler {
    fn drop(&mut self) {
        if let Err(error) = unsafe { self.handler.Deactivate() } {
            log::warn!(
                "windows_disk_cleanup_handler_deactivate_failed handler={} error_digest={}",
                self.handler_name,
                blake3::hash(format!("{error:?}").as_bytes()).to_hex()
            );
        }
    }
}

#[derive(Debug)]
enum HandlerEstimate {
    Absent,
    Ready(u64),
    Cancelled,
    Failed(String),
}

pub(crate) fn estimates(
    kinds: &[WindowsDiskCleanupKind],
    cancellation: &PlatformCancellation,
    use_cache: bool,
) -> Vec<WindowsDiskCleanupEstimate> {
    if !use_cache {
        // A fresh preflight must also evict the previous snapshot. Otherwise a
        // cancelled or failed re-measurement could leave an older successful
        // estimate available to the next cached preview.
        invalidate_estimate_cache_for(kinds);
    }
    let mut results = Vec::with_capacity(kinds.len());
    let mut missing = Vec::new();
    for kind in kinds {
        match use_cache.then(|| cached_estimate(*kind)).flatten() {
            Some(mut estimate) => {
                estimate.elapsed_ms = 0;
                results.push(estimate);
            }
            None => missing.push(*kind),
        }
    }
    if missing.is_empty() {
        log::info!(
            "windows_disk_cleanup_scan_cache_hit kind_count={}",
            kinds.len()
        );
        return ordered_estimates(kinds, results);
    }

    let mut measured = Vec::with_capacity(missing.len());
    let native_missing = missing
        .into_iter()
        .filter(|kind| match kind {
            WindowsDiskCleanupKind::RecycleBin => {
                measured.push(estimate_recycle_bin(cancellation));
                false
            }
            WindowsDiskCleanupKind::SystemLogs => {
                measured.push(estimate_system_logs(cancellation));
                false
            }
            _ => true,
        })
        .collect::<Vec<_>>();
    if native_missing.is_empty() {
        cache_estimates(&measured);
        results.extend(measured);
        return ordered_estimates(kinds, results);
    }

    let started = Instant::now();
    let apartment = ComApartment::initialize();
    let registry = volume_caches_registry();
    let volume = system_volume();
    if apartment.is_err() || registry.is_err() || volume.is_err() {
        let diagnostic = apartment
            .err()
            .or_else(|| registry.as_ref().err().cloned())
            .or_else(|| volume.as_ref().err().cloned());
        if let Some(diagnostic) = diagnostic {
            log::warn!(
                "windows_disk_cleanup_scan_limited reason=initialization error_digest={}",
                blake3::hash(diagnostic.as_bytes()).to_hex()
            );
        }
        measured.extend(
            native_missing
                .iter()
                .copied()
                .map(|kind| limited_estimate(kind, started.elapsed().as_millis() as u64)),
        );
        cache_estimates(&measured);
        results.extend(measured);
        return ordered_estimates(kinds, results);
    }
    let _apartment = apartment.expect("the checked COM apartment must be available");
    let registry = registry.expect("the checked cleanup registry must be available");
    let volume = volume.expect("the checked system volume must be available");
    measured.extend(
        native_missing
            .iter()
            .copied()
            .map(|kind| estimate_kind(&registry, &volume, kind, cancellation)),
    );
    cache_estimates(&measured);
    results.extend(measured);
    ordered_estimates(kinds, results)
}

pub(crate) fn execute(
    kind: WindowsDiskCleanupKind,
    cancellation: &PlatformCancellation,
) -> WindowsDiskCleanupExecution {
    invalidate_estimate_cache();
    if kind == WindowsDiskCleanupKind::RecycleBin {
        return execute_recycle_bin(cancellation);
    }
    if kind == WindowsDiskCleanupKind::SystemLogs {
        return execute_system_logs(cancellation);
    }
    let Ok(_apartment) = ComApartment::initialize() else {
        return failed_execution(kind, 0);
    };
    let Ok(registry) = volume_caches_registry() else {
        return failed_execution(kind, 0);
    };
    let Ok(volume) = system_volume() else {
        return failed_execution(kind, 0);
    };
    let mut bytes_expected = 0_u64;
    let mut released_bytes = 0_u64;
    let mut affected_item_count = 0_u64;
    let mut failed_item_count = 0_u64;

    for handler_name in handler_names(kind) {
        if cancellation.is_cancelled() {
            return WindowsDiskCleanupExecution {
                kind,
                status: WindowsDiskCleanupExecutionStatus::Cancelled,
                bytes_expected,
                released_bytes,
                affected_item_count,
                failed_item_count,
            };
        }
        let Ok(handler_key) = registry.open_subkey(handler_name) else {
            continue;
        };
        let handler = match InitializedHandler::open(
            &handler_key,
            handler_name,
            &volume,
            cancellation.clone(),
        ) {
            Ok(handler) => handler,
            Err(error) => {
                log_handler_error(kind, "initialize", &error);
                failed_item_count = failed_item_count.saturating_add(1);
                continue;
            }
        };
        match execute_handler(&handler) {
            HandlerExecution::Noop => {}
            HandlerExecution::Released {
                expected_bytes,
                released_bytes: observed_release,
            } => {
                bytes_expected = bytes_expected.saturating_add(expected_bytes);
                released_bytes = released_bytes.saturating_add(observed_release);
                affected_item_count = affected_item_count.saturating_add(1);
            }
            HandlerExecution::Cancelled { expected_bytes } => {
                bytes_expected = bytes_expected.saturating_add(expected_bytes);
                return cancelled_execution(
                    kind,
                    bytes_expected,
                    released_bytes,
                    affected_item_count,
                    failed_item_count,
                );
            }
            HandlerExecution::Failed {
                stage,
                expected_bytes,
                diagnostic,
            } => {
                bytes_expected = bytes_expected.saturating_add(expected_bytes);
                if let Some(diagnostic) = diagnostic {
                    log_handler_error(kind, stage, &diagnostic);
                } else {
                    log::warn!(
                        "windows_disk_cleanup_verification_failed kind={} reason=no_release",
                        kind.stable_id()
                    );
                }
                failed_item_count = failed_item_count.saturating_add(1);
            }
        }
    }

    let status = execution_status(affected_item_count, failed_item_count);
    let result = WindowsDiskCleanupExecution {
        kind,
        status,
        bytes_expected,
        released_bytes,
        affected_item_count,
        failed_item_count,
    };
    // Native handlers may share underlying stores. Clearing all estimates
    // prevents a successful cleanup from leaving another category stale.
    invalidate_estimate_cache();
    result
}

fn query_recycle_bin() -> Result<RecycleBinSnapshot, String> {
    let mut info = SHQUERYRBINFO {
        cbSize: u32::try_from(std::mem::size_of::<SHQUERYRBINFO>())
            .map_err(|_| "recycle bin query structure is too large".to_string())?,
        ..SHQUERYRBINFO::default()
    };
    unsafe { SHQueryRecycleBinW(PCWSTR::null(), &mut info) }
        .map_err(|error| format!("recycle bin query failed: {error:?}"))?;
    let bytes = u64::try_from(info.i64Size)
        .map_err(|_| "recycle bin query returned a negative size".to_string())?;
    let item_count = u64::try_from(info.i64NumItems)
        .map_err(|_| "recycle bin query returned a negative item count".to_string())?;
    Ok(RecycleBinSnapshot { bytes, item_count })
}

fn estimate_recycle_bin(cancellation: &PlatformCancellation) -> WindowsDiskCleanupEstimate {
    let started = Instant::now();
    if cancellation.is_cancelled() {
        return limited_estimate(
            WindowsDiskCleanupKind::RecycleBin,
            started.elapsed().as_millis() as u64,
        );
    }
    match query_recycle_bin() {
        Ok(snapshot) => {
            log::info!(
                "windows_recycle_bin_scanned bytes={} item_count={} elapsed_ms={}",
                snapshot.bytes,
                snapshot.item_count,
                started.elapsed().as_millis()
            );
            WindowsDiskCleanupEstimate {
                kind: WindowsDiskCleanupKind::RecycleBin,
                availability: WindowsDiskCleanupAvailability::Ready,
                bytes: snapshot.bytes,
                item_count: snapshot.item_count,
                elapsed_ms: started.elapsed().as_millis() as u64,
            }
        }
        Err(error) => {
            log::warn!(
                "windows_recycle_bin_scan_limited error_digest={}",
                blake3::hash(error.as_bytes()).to_hex()
            );
            limited_estimate(
                WindowsDiskCleanupKind::RecycleBin,
                started.elapsed().as_millis() as u64,
            )
        }
    }
}

/// Builds the conservative result for cleanup that ran but could not be verified.
///
/// Centralizing this state prevents accidental fallback to ordinary execution
/// failure and makes the hard-to-inject Shell API branch unit-testable.
fn recycle_bin_verification_failed_execution(
    kind: WindowsDiskCleanupKind,
    before: RecycleBinSnapshot,
) -> WindowsDiskCleanupExecution {
    WindowsDiskCleanupExecution {
        kind,
        status: WindowsDiskCleanupExecutionStatus::VerificationFailed,
        bytes_expected: before.bytes,
        released_bytes: 0,
        affected_item_count: 0,
        failed_item_count: 1,
    }
}

fn execute_recycle_bin(cancellation: &PlatformCancellation) -> WindowsDiskCleanupExecution {
    let kind = WindowsDiskCleanupKind::RecycleBin;
    let before = match query_recycle_bin() {
        Ok(snapshot) => snapshot,
        Err(error) => {
            log::warn!(
                "windows_recycle_bin_preflight_failed error_digest={}",
                blake3::hash(error.as_bytes()).to_hex()
            );
            return failed_execution(kind, 0);
        }
    };
    if cancellation.is_cancelled() {
        return cancelled_execution(kind, before.bytes, 0, 0, 0);
    }
    if before.bytes == 0 && before.item_count == 0 {
        return WindowsDiskCleanupExecution {
            kind,
            status: WindowsDiskCleanupExecutionStatus::Completed,
            bytes_expected: 0,
            released_bytes: 0,
            affected_item_count: 0,
            failed_item_count: 0,
        };
    }

    // The Shell owns every per-volume Recycle Bin layout and current-user
    // identity check. Suppressing its second confirmation is intentional: the
    // deep-cleanup plan is the explicit user confirmation, while hiding the
    // native progress window keeps execution inside MangoDisk's progress UI.
    let purge_result = unsafe {
        SHEmptyRecycleBinW(
            None,
            PCWSTR::null(),
            SHERB_NOCONFIRMATION | SHERB_NOPROGRESSUI | SHERB_NOSOUND,
        )
    };
    // SHEmptyRecycleBinW is not cancellable. Always verify after it returns so
    // a cancellation request cannot hide files that were already released.
    let after = match query_recycle_bin() {
        Ok(snapshot) => snapshot,
        Err(error) => {
            log::warn!(
                "windows_recycle_bin_verification_failed purge_succeeded={} error_digest={}",
                purge_result.is_ok(),
                blake3::hash(error.as_bytes()).to_hex()
            );
            // The emptying call already returned, so a failed reconciliation
            // cannot be presented as an execution failure. Keep the unknown
            // released amount at zero and let Core ask for a rescan instead of
            // incorrectly claiming that nothing was removed.
            return recycle_bin_verification_failed_execution(kind, before);
        }
    };
    let released_bytes = before.bytes.saturating_sub(after.bytes);
    let affected_item_count = before.item_count.saturating_sub(after.item_count);
    let failed_item_count = if purge_result.is_ok() && after.item_count == 0 && after.bytes == 0 {
        0
    } else {
        after.item_count.max(1)
    };
    let status = if failed_item_count == 0 {
        WindowsDiskCleanupExecutionStatus::Completed
    } else if released_bytes > 0 || affected_item_count > 0 {
        WindowsDiskCleanupExecutionStatus::Partial
    } else {
        WindowsDiskCleanupExecutionStatus::Failed
    };
    if let Err(error) = purge_result {
        log::warn!(
            "windows_recycle_bin_empty_failed error_digest={}",
            blake3::hash(format!("{error:?}").as_bytes()).to_hex()
        );
    }
    log::info!(
        "windows_recycle_bin_cleanup_finished status={status:?} expected_bytes={} released_bytes={} affected_item_count={} failed_item_count={}",
        before.bytes,
        released_bytes,
        affected_item_count,
        failed_item_count
    );
    WindowsDiskCleanupExecution {
        kind,
        status,
        bytes_expected: before.bytes,
        released_bytes,
        affected_item_count,
        failed_item_count,
    }
}

fn estimate_system_logs(cancellation: &PlatformCancellation) -> WindowsDiskCleanupEstimate {
    let started = Instant::now();
    let inventory = match discover_system_logs(cancellation) {
        Ok(inventory) => inventory,
        Err(SystemLogDiscoveryError::Cancelled) => {
            return limited_estimate(
                WindowsDiskCleanupKind::SystemLogs,
                started.elapsed().as_millis() as u64,
            );
        }
        Err(SystemLogDiscoveryError::Unavailable(error)) => {
            log::warn!(
                "windows_system_log_scan_limited error_digest={}",
                blake3::hash(error.as_bytes()).to_hex()
            );
            return limited_estimate(
                WindowsDiskCleanupKind::SystemLogs,
                started.elapsed().as_millis() as u64,
            );
        }
    };
    let bytes = inventory.candidates.iter().fold(0_u64, |total, candidate| {
        total.saturating_add(candidate.bytes)
    });
    let item_count = u64::try_from(inventory.candidates.len()).unwrap_or(u64::MAX);
    let availability = if inventory.root_count == 0 {
        WindowsDiskCleanupAvailability::NotApplicable
    } else {
        WindowsDiskCleanupAvailability::Ready
    };
    log::info!(
        "windows_system_log_scanned availability={:?} bytes={} item_count={} root_count={} skipped_count={} elapsed_ms={}",
        availability,
        bytes,
        item_count,
        inventory.root_count,
        inventory.skipped_count,
        started.elapsed().as_millis()
    );
    WindowsDiskCleanupEstimate {
        kind: WindowsDiskCleanupKind::SystemLogs,
        availability,
        bytes,
        item_count,
        elapsed_ms: started.elapsed().as_millis() as u64,
    }
}

fn execute_system_logs(cancellation: &PlatformCancellation) -> WindowsDiskCleanupExecution {
    let before = match discover_system_logs(cancellation) {
        Ok(inventory) => inventory,
        Err(SystemLogDiscoveryError::Cancelled) => {
            return cancelled_execution(WindowsDiskCleanupKind::SystemLogs, 0, 0, 0, 0);
        }
        Err(SystemLogDiscoveryError::Unavailable(error)) => {
            log::warn!(
                "windows_system_log_preflight_failed error_digest={}",
                blake3::hash(error.as_bytes()).to_hex()
            );
            return failed_execution(WindowsDiskCleanupKind::SystemLogs, 0);
        }
    };
    let bytes_expected = before.candidates.iter().fold(0_u64, |total, candidate| {
        total.saturating_add(candidate.bytes)
    });
    let mut affected_item_count = 0_u64;
    let mut failed_item_count = 0_u64;
    let mut cancelled = false;
    for candidate in before.candidates {
        if cancellation.is_cancelled() {
            cancelled = true;
            break;
        }
        let metadata = match fs::symlink_metadata(&candidate.path) {
            Ok(metadata)
                if !is_reparse_point(&metadata)
                    && is_system_log_candidate(&candidate.path, &metadata, SystemTime::now()) =>
            {
                metadata
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            _ => {
                failed_item_count = failed_item_count.saturating_add(1);
                continue;
            }
        };
        if metadata.len() != candidate.bytes {
            failed_item_count = failed_item_count.saturating_add(1);
            continue;
        }
        match fs::remove_file(&candidate.path) {
            Ok(()) => affected_item_count = affected_item_count.saturating_add(1),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => failed_item_count = failed_item_count.saturating_add(1),
        }
    }

    /*
     * Verification must finish after the first mutation even if the caller
     * cancels. Otherwise the result could report zero released bytes after
     * files were already removed. The traversal remains bounded to three
     * fixed Windows-owned roots and sixteen directory levels.
     */
    let verification_cancellation = PlatformCancellation::new(|| false);
    let after = discover_system_logs(&verification_cancellation);
    let released_bytes = after
        .as_ref()
        .map(|inventory| {
            let remaining = inventory.candidates.iter().fold(0_u64, |total, candidate| {
                total.saturating_add(candidate.bytes)
            });
            bytes_expected.saturating_sub(remaining)
        })
        .unwrap_or(0);
    if after.is_err() {
        failed_item_count = failed_item_count.saturating_add(1);
    }
    let status = if cancelled {
        WindowsDiskCleanupExecutionStatus::Cancelled
    } else {
        execution_status(affected_item_count, failed_item_count)
    };
    log::info!(
        "windows_system_log_cleanup_finished status={status:?} expected_bytes={bytes_expected} released_bytes={released_bytes} affected_item_count={affected_item_count} failed_item_count={failed_item_count}"
    );
    WindowsDiskCleanupExecution {
        kind: WindowsDiskCleanupKind::SystemLogs,
        status,
        bytes_expected,
        released_bytes,
        affected_item_count,
        failed_item_count,
    }
}

#[derive(Debug)]
enum SystemLogDiscoveryError {
    Cancelled,
    Unavailable(String),
}

fn discover_system_logs(
    cancellation: &PlatformCancellation,
) -> Result<SystemLogInventory, SystemLogDiscoveryError> {
    let system_directory = system_directory().map_err(SystemLogDiscoveryError::Unavailable)?;
    let roots = system_log_roots_from_directory(&system_directory);
    let now = SystemTime::now();
    let mut inventory = SystemLogInventory::default();

    for root in roots {
        if cancellation.is_cancelled() {
            return Err(SystemLogDiscoveryError::Cancelled);
        }
        let root_metadata = match fs::symlink_metadata(&root) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(_) => {
                inventory.skipped_count = inventory.skipped_count.saturating_add(1);
                continue;
            }
        };
        if !root_metadata.is_dir() || is_reparse_point(&root_metadata) {
            inventory.skipped_count = inventory.skipped_count.saturating_add(1);
            continue;
        }
        inventory.root_count = inventory.root_count.saturating_add(1);
        let mut pending = vec![(root, 0_usize)];
        while let Some((directory, depth)) = pending.pop() {
            if cancellation.is_cancelled() {
                return Err(SystemLogDiscoveryError::Cancelled);
            }
            let entries = match fs::read_dir(&directory) {
                Ok(entries) => entries,
                Err(_) => {
                    inventory.skipped_count = inventory.skipped_count.saturating_add(1);
                    continue;
                }
            };
            for entry in entries {
                let entry = match entry {
                    Ok(entry) => entry,
                    Err(_) => {
                        inventory.skipped_count = inventory.skipped_count.saturating_add(1);
                        continue;
                    }
                };
                let path = entry.path();
                let metadata = match fs::symlink_metadata(&path) {
                    Ok(metadata) => metadata,
                    Err(_) => {
                        inventory.skipped_count = inventory.skipped_count.saturating_add(1);
                        continue;
                    }
                };
                if is_reparse_point(&metadata) {
                    inventory.skipped_count = inventory.skipped_count.saturating_add(1);
                    continue;
                }
                if metadata.is_dir() {
                    if depth < SYSTEM_LOG_MAXIMUM_DEPTH {
                        pending.push((path, depth + 1));
                    } else {
                        inventory.skipped_count = inventory.skipped_count.saturating_add(1);
                    }
                    continue;
                }
                if is_system_log_candidate(&path, &metadata, now) {
                    inventory.candidates.push(SystemLogCandidate {
                        path,
                        bytes: metadata.len(),
                    });
                }
            }
        }
    }
    Ok(inventory)
}

fn system_log_roots_from_directory(system_directory: &Path) -> [std::path::PathBuf; 3] {
    [
        system_directory.join("Panther"),
        system_directory.join("Logs"),
        system_directory.join("System32").join("LogFiles"),
    ]
}

fn is_system_log_candidate(path: &Path, metadata: &fs::Metadata, now: SystemTime) -> bool {
    if !metadata.is_file() || metadata.permissions().readonly() {
        return false;
    }
    let Some(extension) = path.extension().and_then(|value| value.to_str()) else {
        return false;
    };
    if !SYSTEM_LOG_EXTENSIONS
        .iter()
        .any(|allowed| extension.eq_ignore_ascii_case(allowed))
    {
        return false;
    }
    metadata
        .modified()
        .ok()
        .and_then(|modified| now.duration_since(modified).ok())
        .is_some_and(|age| age >= SYSTEM_LOG_MINIMUM_AGE)
}

fn is_reparse_point(metadata: &fs::Metadata) -> bool {
    metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
}

fn estimate_kind(
    registry: &RegKey,
    volume: &str,
    kind: WindowsDiskCleanupKind,
    cancellation: &PlatformCancellation,
) -> WindowsDiskCleanupEstimate {
    let started = Instant::now();
    let mut bytes = 0_u64;
    let mut item_count = 0_u64;
    let mut present_count = 0_u64;
    let mut failed_count = 0_u64;
    for handler_name in handler_names(kind) {
        if cancellation.is_cancelled() {
            return limited_estimate(kind, started.elapsed().as_millis() as u64);
        }
        match estimate_handler(registry, handler_name, volume, cancellation) {
            HandlerEstimate::Absent => {}
            HandlerEstimate::Ready(handler_bytes) => {
                present_count = present_count.saturating_add(1);
                bytes = bytes.saturating_add(handler_bytes);
                if handler_bytes > 0 {
                    item_count = item_count.saturating_add(1);
                }
            }
            HandlerEstimate::Failed(error) => {
                present_count = present_count.saturating_add(1);
                failed_count = failed_count.saturating_add(1);
                log_handler_error(kind, "scan", &error);
            }
            HandlerEstimate::Cancelled => {
                return limited_estimate(kind, started.elapsed().as_millis() as u64);
            }
        }
    }
    let availability = if present_count == 0 {
        WindowsDiskCleanupAvailability::NotApplicable
    } else if failed_count > 0 {
        WindowsDiskCleanupAvailability::Limited
    } else {
        WindowsDiskCleanupAvailability::Ready
    };
    log::info!(
        "windows_disk_cleanup_scanned kind={} availability={:?} bytes={} item_count={} handler_count={} failed_handler_count={} elapsed_ms={}",
        kind.stable_id(),
        availability,
        bytes,
        item_count,
        present_count,
        failed_count,
        started.elapsed().as_millis()
    );
    WindowsDiskCleanupEstimate {
        kind,
        availability,
        bytes,
        item_count,
        elapsed_ms: started.elapsed().as_millis() as u64,
    }
}

fn estimate_handler(
    registry: &RegKey,
    handler_name: &str,
    volume: &str,
    cancellation: &PlatformCancellation,
) -> HandlerEstimate {
    let handler_key = match registry.open_subkey(handler_name) {
        Ok(key) => key,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return HandlerEstimate::Absent;
        }
        Err(error) => {
            return HandlerEstimate::Failed(format!(
                "cleanup handler registry access failed: {error:?}"
            ));
        }
    };
    let handler =
        match InitializedHandler::open(&handler_key, handler_name, volume, cancellation.clone()) {
            Ok(handler) => handler,
            Err(error) => return HandlerEstimate::Failed(error),
        };
    match handler.measure() {
        Ok(bytes) => HandlerEstimate::Ready(bytes),
        Err(NativeCallError::Cancelled) => HandlerEstimate::Cancelled,
        Err(error) => HandlerEstimate::Failed(error.diagnostic().to_string()),
    }
}

fn handler_names(kind: WindowsDiskCleanupKind) -> &'static [&'static str] {
    match kind {
        WindowsDiskCleanupKind::RecycleBin => &[],
        // System logs use the bounded filesystem inventory above because the
        // registered setup handlers return zero outside an active upgrade even
        // when old diagnostic logs still occupy substantial space.
        WindowsDiskCleanupKind::SystemLogs => &[],
        WindowsDiskCleanupKind::InternetCache => INTERNET_CACHE_HANDLERS,
        WindowsDiskCleanupKind::DeliveryOptimization => DELIVERY_OPTIMIZATION_HANDLERS,
        WindowsDiskCleanupKind::DefenderCache => DEFENDER_CACHE_HANDLERS,
        WindowsDiskCleanupKind::UpdateCleanup => UPDATE_CLEANUP_HANDLERS,
    }
}

fn cached_estimate(kind: WindowsDiskCleanupKind) -> Option<WindowsDiskCleanupEstimate> {
    let cache = ESTIMATE_CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    let Ok(mut cache) = cache.lock() else {
        log::warn!("windows_disk_cleanup_cache_unavailable reason=poisoned");
        return None;
    };
    cache.retain(|_, cached| cached.measured_at.elapsed() <= ESTIMATE_CACHE_TTL);
    cache.get(&kind).map(|cached| cached.estimate)
}

fn cache_estimates(estimates: &[WindowsDiskCleanupEstimate]) {
    let cache = ESTIMATE_CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    let Ok(mut cache) = cache.lock() else {
        log::warn!("windows_disk_cleanup_cache_update_failed reason=poisoned");
        return;
    };
    let measured_at = Instant::now();
    for estimate in estimates
        .iter()
        .filter(|estimate| estimate.availability != WindowsDiskCleanupAvailability::Limited)
    {
        // Limited results often come from a transient COM, permission, or
        // cancellation failure. Caching them would keep a later healthy scan
        // unavailable for the full TTL without saving useful work.
        cache.insert(
            estimate.kind,
            CachedEstimate {
                measured_at,
                estimate: *estimate,
            },
        );
    }
}

fn invalidate_estimate_cache() {
    let Some(cache) = ESTIMATE_CACHE.get() else {
        return;
    };
    if let Ok(mut cache) = cache.lock() {
        cache.clear();
    } else {
        log::warn!("windows_disk_cleanup_cache_invalidate_failed reason=poisoned");
    }
}

fn invalidate_estimate_cache_for(kinds: &[WindowsDiskCleanupKind]) {
    let Some(cache) = ESTIMATE_CACHE.get() else {
        return;
    };
    if let Ok(mut cache) = cache.lock() {
        cache.retain(|kind, _| !kinds.contains(kind));
    } else {
        log::warn!("windows_disk_cleanup_cache_invalidate_failed reason=poisoned");
    }
}

fn ordered_estimates(
    kinds: &[WindowsDiskCleanupKind],
    estimates: Vec<WindowsDiskCleanupEstimate>,
) -> Vec<WindowsDiskCleanupEstimate> {
    let by_kind = estimates
        .into_iter()
        .map(|estimate| (estimate.kind, estimate))
        .collect::<HashMap<_, _>>();
    kinds
        .iter()
        .filter_map(|kind| by_kind.get(kind).copied())
        .collect()
}

fn volume_caches_registry() -> Result<RegKey, String> {
    RegKey::predef(HKEY_LOCAL_MACHINE)
        .open_subkey(VOLUME_CACHES_KEY)
        .map_err(|error| format!("Windows disk cleanup registry is unavailable: {error:?}"))
}

fn system_volume() -> Result<String, String> {
    // Environment variables are process-controlled and can be stale or
    // malformed after an abnormal launch. Query the OS-owned Windows directory
    // and accept only an absolute local drive before calling cleanup handlers.
    system_volume_from_directory(&system_directory()?)
}

fn system_directory() -> Result<std::path::PathBuf, String> {
    let mut buffer = vec![0_u16; 32_768];
    let length = unsafe {
        windows_sys::Win32::System::SystemInformation::GetSystemWindowsDirectoryW(
            buffer.as_mut_ptr(),
            u32::try_from(buffer.len()).expect("the fixed Windows path buffer must fit in u32"),
        )
    };
    if length == 0 || length as usize >= buffer.len() {
        return Err("Windows system directory is unavailable".to_string());
    }
    Ok(std::path::PathBuf::from(OsString::from_wide(
        &buffer[..length as usize],
    )))
}

fn system_volume_from_directory(directory: &Path) -> Result<String, String> {
    let mut components = directory.components();
    let drive = match components.next() {
        Some(Component::Prefix(prefix)) => match prefix.kind() {
            Prefix::Disk(drive) | Prefix::VerbatimDisk(drive) => drive,
            _ => return Err("Windows system directory is not on a local drive".to_string()),
        },
        _ => return Err("Windows system directory is not absolute".to_string()),
    };
    if !matches!(components.next(), Some(Component::RootDir)) {
        return Err("Windows system directory has no drive root".to_string());
    }
    Ok(format!("{}:", char::from(drive).to_ascii_uppercase()))
}

fn wide_null(value: &str) -> Vec<u16> {
    value.encode_utf16().chain(std::iter::once(0)).collect()
}

fn free_com_string(value: PWSTR) {
    if !value.is_null() {
        unsafe { CoTaskMemFree(Some(value.0.cast::<c_void>())) };
    }
}

fn limited_estimate(kind: WindowsDiskCleanupKind, elapsed_ms: u64) -> WindowsDiskCleanupEstimate {
    WindowsDiskCleanupEstimate {
        kind,
        availability: WindowsDiskCleanupAvailability::Limited,
        bytes: 0,
        item_count: 0,
        elapsed_ms,
    }
}

fn failed_execution(
    kind: WindowsDiskCleanupKind,
    bytes_expected: u64,
) -> WindowsDiskCleanupExecution {
    WindowsDiskCleanupExecution {
        kind,
        status: WindowsDiskCleanupExecutionStatus::Failed,
        bytes_expected,
        released_bytes: 0,
        affected_item_count: 0,
        failed_item_count: 1,
    }
}

fn cancelled_execution(
    kind: WindowsDiskCleanupKind,
    bytes_expected: u64,
    released_bytes: u64,
    affected_item_count: u64,
    failed_item_count: u64,
) -> WindowsDiskCleanupExecution {
    WindowsDiskCleanupExecution {
        kind,
        status: WindowsDiskCleanupExecutionStatus::Cancelled,
        bytes_expected,
        released_bytes,
        affected_item_count,
        failed_item_count,
    }
}

fn execution_status(
    affected_item_count: u64,
    failed_item_count: u64,
) -> WindowsDiskCleanupExecutionStatus {
    if failed_item_count == 0 {
        WindowsDiskCleanupExecutionStatus::Completed
    } else if affected_item_count > 0 {
        WindowsDiskCleanupExecutionStatus::Partial
    } else {
        WindowsDiskCleanupExecutionStatus::Failed
    }
}

fn log_handler_error(kind: WindowsDiskCleanupKind, stage: &'static str, error: &str) {
    log::warn!(
        "windows_disk_cleanup_handler_failed kind={} stage={} error_digest={}",
        kind.stable_id(),
        stage,
        blake3::hash(error.as_bytes()).to_hex()
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        collections::{HashSet, VecDeque},
        fs::{File, FileTimes},
        sync::{
            atomic::{AtomicBool, AtomicU64, Ordering},
            Arc, Mutex,
        },
        time::UNIX_EPOCH,
    };

    struct MockCleanupHandler {
        measurements: Mutex<VecDeque<Result<u64, NativeCallError>>>,
        purge_result: Mutex<Option<Result<(), NativeCallError>>>,
        purge_request: AtomicU64,
    }

    impl MockCleanupHandler {
        fn new(
            measurements: impl IntoIterator<Item = Result<u64, NativeCallError>>,
            purge_result: Result<(), NativeCallError>,
        ) -> Self {
            Self {
                measurements: Mutex::new(measurements.into_iter().collect()),
                purge_result: Mutex::new(Some(purge_result)),
                purge_request: AtomicU64::new(0),
            }
        }
    }

    impl CleanupHandler for MockCleanupHandler {
        fn measure(&self) -> Result<u64, NativeCallError> {
            self.measurements
                .lock()
                .expect("the mock measurement queue must remain available")
                .pop_front()
                .expect("the execution path must not measure more than expected")
        }

        fn purge(&self, bytes: u64) -> Result<(), NativeCallError> {
            self.purge_request.store(bytes, Ordering::Relaxed);
            self.purge_result
                .lock()
                .expect("the mock purge result must remain available")
                .take()
                .expect("the execution path must purge at most once")
        }
    }

    #[test]
    fn stable_groups_do_not_share_native_handlers() {
        let mut names = HashSet::new();
        for kind in [
            WindowsDiskCleanupKind::RecycleBin,
            WindowsDiskCleanupKind::SystemLogs,
            WindowsDiskCleanupKind::InternetCache,
            WindowsDiskCleanupKind::DeliveryOptimization,
            WindowsDiskCleanupKind::DefenderCache,
            WindowsDiskCleanupKind::UpdateCleanup,
        ] {
            for name in handler_names(kind) {
                assert!(names.insert(*name), "duplicate cleanup handler: {name}");
            }
        }
    }

    #[test]
    fn system_log_inventory_accepts_only_old_log_files_in_fixed_roots() {
        let fixture = std::env::temp_dir().join(format!(
            "mangodisk-system-log-fixture-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("the fixture timestamp must follow the Unix epoch")
                .as_nanos()
        ));
        fs::create_dir_all(&fixture).expect("the system-log fixture must be created");
        let old_log = fixture.join("setup.log");
        let young_log = fixture.join("active.log");
        let old_database = fixture.join("state.db");
        fs::write(&old_log, b"old log").expect("the old log fixture must be written");
        fs::write(&young_log, b"young log").expect("the young log fixture must be written");
        fs::write(&old_database, b"state").expect("the database fixture must be written");
        let old_timestamp = SystemTime::now()
            .checked_sub(SYSTEM_LOG_MINIMUM_AGE + Duration::from_secs(60))
            .expect("the old fixture timestamp must be representable");
        for path in [&old_log, &old_database] {
            File::options()
                .write(true)
                .open(path)
                .expect("the fixture file must open")
                .set_times(FileTimes::new().set_modified(old_timestamp))
                .expect("the fixture timestamp must be updated");
        }

        let now = SystemTime::now();
        assert!(is_system_log_candidate(
            &old_log,
            &fs::metadata(&old_log).expect("the old log metadata must exist"),
            now
        ));
        assert!(!is_system_log_candidate(
            &young_log,
            &fs::metadata(&young_log).expect("the young log metadata must exist"),
            now
        ));
        assert!(!is_system_log_candidate(
            &old_database,
            &fs::metadata(&old_database).expect("the database metadata must exist"),
            now
        ));
        assert_eq!(
            system_log_roots_from_directory(Path::new(r"C:\Windows")),
            [
                std::path::PathBuf::from(r"C:\Windows\Panther"),
                std::path::PathBuf::from(r"C:\Windows\Logs"),
                std::path::PathBuf::from(r"C:\Windows\System32\LogFiles"),
            ]
        );
        fs::remove_dir_all(&fixture).expect("the system-log fixture must be removed");
    }

    #[test]
    fn system_volume_accepts_only_an_absolute_local_drive() {
        assert_eq!(
            system_volume_from_directory(Path::new(r"C:\Windows")),
            Ok("C:".to_string())
        );
        assert!(system_volume_from_directory(Path::new(r"\\server\share\Windows")).is_err());
        assert!(system_volume_from_directory(Path::new(r"Windows")).is_err());
    }

    #[test]
    fn measurement_rejects_unavailable_and_unknown_sizes() {
        assert!(matches!(
            measurement_result(S_FALSE, 128),
            Err(NativeCallError::Failed(_))
        ));
        assert!(matches!(
            measurement_result(HRESULT(0), u64::MAX),
            Err(NativeCallError::Failed(_))
        ));
        assert!(matches!(
            measurement_result(E_ABORT, 0),
            Err(NativeCallError::Cancelled)
        ));
        assert_eq!(measurement_result(HRESULT(0), 128).unwrap(), 128);
    }

    #[test]
    fn native_progress_callback_propagates_cancellation() {
        let cancelled = Arc::new(AtomicBool::new(false));
        let cancellation_flag = Arc::clone(&cancelled);
        let callback: IEmptyVolumeCacheCallBack = CleanupCallback {
            cancellation: PlatformCancellation::new(move || {
                cancellation_flag.load(Ordering::Relaxed)
            }),
        }
        .into();

        unsafe {
            callback
                .ScanProgress(0, 0, PCWSTR::null())
                .expect("an active operation must continue");
        }
        cancelled.store(true, Ordering::Relaxed);
        let error = unsafe { callback.PurgeProgress(0, 1, 0, PCWSTR::null()) }
            .expect_err("a cancelled operation must abort the native handler");
        assert_eq!(error.code(), E_ABORT);
    }

    #[test]
    fn execution_status_never_hides_failed_verification() {
        assert_eq!(
            execution_status(0, 1),
            WindowsDiskCleanupExecutionStatus::Failed
        );
        assert_eq!(
            execution_status(1, 1),
            WindowsDiskCleanupExecutionStatus::Partial
        );
        assert_eq!(
            execution_status(1, 0),
            WindowsDiskCleanupExecutionStatus::Completed
        );
    }

    #[test]
    fn recycle_bin_verification_failure_stays_distinct_from_execution_failure() {
        let result = recycle_bin_verification_failed_execution(
            WindowsDiskCleanupKind::RecycleBin,
            RecycleBinSnapshot {
                bytes: 512,
                item_count: 3,
            },
        );

        assert_eq!(
            result.status,
            WindowsDiskCleanupExecutionStatus::VerificationFailed
        );
        assert_eq!(result.bytes_expected, 512);
        assert_eq!(result.released_bytes, 0);
        assert_eq!(result.affected_item_count, 0);
        assert_eq!(result.failed_item_count, 1);
    }

    /// Calls the real Windows Shell APIs with populated, pre-cancelled,
    /// successfully emptied, and already-empty Recycle Bin states.
    ///
    /// This empties every drive's Recycle Bin for the current account. It
    /// requires an explicit environment gate and a snapshot-backed VM. An
    /// external fixture script populates the bin so production code does not
    /// gain a test-only recycling capability.
    #[test]
    #[ignore = "empties the real Windows Recycle Bin; requires an isolated VM fixture"]
    fn real_recycle_bin_cleanup_handles_cancellation_success_and_empty_state() {
        assert_eq!(
            std::env::var("MANGODISK_TEST_REAL_RECYCLE_BIN").as_deref(),
            Ok("1"),
            "set MANGODISK_TEST_REAL_RECYCLE_BIN=1 only in an isolated Windows VM"
        );
        let before = query_recycle_bin().expect("query the prepared Recycle Bin fixture");
        assert!(
            before.item_count > 0,
            "the Recycle Bin fixture must contain items"
        );
        assert!(
            before.bytes > 0,
            "the Recycle Bin fixture must contain data"
        );

        let cancelled = execute_recycle_bin(&PlatformCancellation::new(|| true));
        assert_eq!(
            cancelled.status,
            WindowsDiskCleanupExecutionStatus::Cancelled
        );
        let after_cancel = query_recycle_bin().expect("query after pre-cancelled cleanup");
        assert_eq!(after_cancel.item_count, before.item_count);
        assert_eq!(after_cancel.bytes, before.bytes);

        let completed = execute_recycle_bin(&PlatformCancellation::new(|| false));
        assert_eq!(
            completed.status,
            WindowsDiskCleanupExecutionStatus::Completed
        );
        assert_eq!(completed.bytes_expected, before.bytes);
        assert_eq!(completed.released_bytes, before.bytes);
        assert_eq!(completed.affected_item_count, before.item_count);
        assert_eq!(completed.failed_item_count, 0);
        let after = query_recycle_bin().expect("verify the Recycle Bin after cleanup");
        assert_eq!(after.item_count, 0);
        assert_eq!(after.bytes, 0);

        let empty = execute_recycle_bin(&PlatformCancellation::new(|| false));
        assert_eq!(empty.status, WindowsDiskCleanupExecutionStatus::Completed);
        assert_eq!(empty.bytes_expected, 0);
        assert_eq!(empty.released_bytes, 0);
        assert_eq!(empty.affected_item_count, 0);
        assert_eq!(empty.failed_item_count, 0);
    }

    #[test]
    fn execution_purges_only_the_fresh_measurement_and_verifies_release() {
        let handler = MockCleanupHandler::new([Ok(500), Ok(125)], Ok(()));

        assert!(matches!(
            execute_handler(&handler),
            HandlerExecution::Released {
                expected_bytes: 500,
                released_bytes: 375
            }
        ));
        assert_eq!(handler.purge_request.load(Ordering::Relaxed), 500);
    }

    #[test]
    fn execution_does_not_report_an_unverified_release() {
        let handler = MockCleanupHandler::new([Ok(500), Ok(500)], Ok(()));

        assert!(matches!(
            execute_handler(&handler),
            HandlerExecution::Failed {
                stage: "verify",
                expected_bytes: 500,
                diagnostic: None
            }
        ));
    }

    #[test]
    fn execution_preserves_cancellation_from_purge() {
        let handler = MockCleanupHandler::new([Ok(500)], Err(NativeCallError::Cancelled));

        assert!(matches!(
            execute_handler(&handler),
            HandlerExecution::Cancelled {
                expected_bytes: 500
            }
        ));
    }

    /// This ignored test invokes only the read-only `GetSpaceUsed` methods of
    /// Windows' registered handlers. It is used by the Windows VM validation
    /// stage to diagnose OS-version and permission differences.
    #[test]
    #[ignore = "requires Windows disk-cleanup handlers from the host OS"]
    fn real_registered_handlers_report_estimates() {
        let _apartment = ComApartment::initialize().expect("COM must initialize");
        let registry = volume_caches_registry().expect("VolumeCaches must be readable");
        let volume = system_volume().expect("the system volume must be available");
        let cancellation = PlatformCancellation::new(|| false);
        for handler_name in registry.enum_keys().filter_map(Result::ok) {
            println!(
                "{}\t{:?}",
                handler_name,
                estimate_handler(&registry, &handler_name, &volume, &cancellation)
            );
        }
    }
}

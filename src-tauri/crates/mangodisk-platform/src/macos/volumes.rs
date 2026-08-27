use std::{
    collections::HashMap,
    ffi::CString,
    fs,
    io::Cursor,
    mem::MaybeUninit,
    os::unix::ffi::OsStrExt,
    path::{Path, PathBuf},
    process::Command,
    sync::{Mutex, OnceLock},
};

use plist::Value;

use crate::{ScanConcurrency, ScanDeviceClass, VolumeInfo};

static SCAN_CONCURRENCY_CACHE: OnceLock<Mutex<HashMap<PathBuf, ScanConcurrency>>> = OnceLock::new();

pub fn system_volume() -> Result<VolumeInfo, String> {
    volume_info(PathBuf::from("/"), "Macintosh HD".to_string())
}

pub fn volumes() -> Result<Vec<VolumeInfo>, String> {
    let mut result = vec![system_volume()?];
    if let Ok(entries) = fs::read_dir("/Volumes") {
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            // macOS may expose a `Macintosh HD -> /` symbolic link under /Volumes, while Time
            // Machine uses the hidden `.timemachine` mount entry. Neither represents a local
            // volume that users should analyze separately. Filtering them at the platform boundary
            // prevents duplicate system disks and backup internals from reaching the frontend.
            let is_visible_directory = entry
                .file_type()
                .is_ok_and(|file_type| file_type.is_dir() && !file_type.is_symlink());
            if name.starts_with('.') || !is_visible_directory {
                continue;
            }
            let path = entry.path();
            // Parallels mounts Windows drive letters under /Volumes as hidden SMB shares. Network
            // and virtual-machine shares are not local disks and have different scan performance,
            // permissions, and deletion semantics. MNT_LOCAL reliably keeps APFS and physical USB
            // volumes while excluding these SMB mounts.
            if !is_local_volume(&path) {
                log::debug!("non_local_volume_skipped platform=macos");
                continue;
            }
            if let Ok(volume) = volume_info(path, name) {
                result.push(volume);
            }
        }
    }
    Ok(result)
}

fn is_local_volume(path: &Path) -> bool {
    let Ok(c_path) = CString::new(path.as_os_str().as_bytes()) else {
        return false;
    };
    let mut stats = MaybeUninit::<libc::statfs>::uninit();
    if unsafe { libc::statfs(c_path.as_ptr(), stats.as_mut_ptr()) } != 0 {
        return false;
    }
    let stats = unsafe { stats.assume_init() };
    stats.f_flags & libc::MNT_LOCAL as u32 != 0
}

fn volume_info(path: PathBuf, name: String) -> Result<VolumeInfo, String> {
    let (total_bytes, available_bytes) = disk_space(&path)?;
    let scan_concurrency = scan_concurrency(&path);
    Ok(VolumeInfo {
        name,
        mount_point: path.display().to_string(),
        total_bytes,
        available_bytes,
        used_bytes: total_bytes.saturating_sub(available_bytes),
        scan_concurrency,
    })
}

fn scan_concurrency(path: &Path) -> ScanConcurrency {
    let cache = SCAN_CONCURRENCY_CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    if let Ok(cache) = cache.lock() {
        if let Some(cached) = cache.get(path) {
            return *cached;
        }
    }
    let measured = measure_scan_concurrency(path);
    if let Ok(mut cache) = cache.lock() {
        // The system disk class is stable for the application session. Removable media always uses
        // one worker even if replaced, so caching by mount point cannot increase concurrency. It
        // also prevents system_volume and volumes from invoking diskutil twice for the same volume.
        cache.insert(path.to_path_buf(), measured);
    }
    measured
}

fn measure_scan_concurrency(path: &Path) -> ScanConcurrency {
    let output = Command::new("/usr/sbin/diskutil")
        .arg("info")
        .arg("-plist")
        .arg(path)
        .output();
    let Ok(output) = output else {
        return ScanConcurrency::conservative(ScanDeviceClass::Unknown);
    };
    if !output.status.success() {
        return ScanConcurrency::conservative(ScanDeviceClass::Unknown);
    }
    let Ok(value) = Value::from_reader(Cursor::new(output.stdout)) else {
        return ScanConcurrency::conservative(ScanDeviceClass::Unknown);
    };
    let Some(info) = value.as_dictionary() else {
        return ScanConcurrency::conservative(ScanDeviceClass::Unknown);
    };
    // An external SSD may provide sufficient random access, but USB/SATA bridges, sleep behavior,
    // and power delivery are unpredictable. Cleanup favors stable responsiveness, so all removable
    // or external devices use a single worker.
    let removable = [
        "Removable",
        "RemovableMedia",
        "RemovableMediaOrExternalDevice",
    ]
    .into_iter()
    .any(|key| info.get(key).and_then(Value::as_boolean).unwrap_or(false));
    if removable {
        return ScanConcurrency::conservative(ScanDeviceClass::Removable);
    }
    match info.get("SolidState").and_then(Value::as_boolean) {
        Some(true) => ScanConcurrency::solid_state(),
        Some(false) => ScanConcurrency::rotational(),
        None => ScanConcurrency::conservative(ScanDeviceClass::Unknown),
    }
}

fn disk_space(path: &Path) -> Result<(u64, u64), String> {
    let c_path = CString::new(path.as_os_str().as_bytes()).map_err(|error| error.to_string())?;
    let mut stats = MaybeUninit::<libc::statvfs>::uninit();
    if unsafe { libc::statvfs(c_path.as_ptr(), stats.as_mut_ptr()) } != 0 {
        return Err(std::io::Error::last_os_error().to_string());
    }
    let stats = unsafe { stats.assume_init() };
    let block_size = stats.f_frsize;
    Ok((
        (stats.f_blocks as u64).saturating_mul(block_size),
        (stats.f_bavail as u64).saturating_mul(block_size),
    ))
}

use std::{
    collections::HashMap,
    env,
    ffi::OsString,
    io,
    mem::{size_of, zeroed},
    os::windows::ffi::OsStrExt,
    os::windows::ffi::OsStringExt,
    path::{Path, PathBuf},
    ptr,
    sync::{Mutex, OnceLock},
};

use windows_sys::Win32::{
    Foundation::{CloseHandle, INVALID_HANDLE_VALUE},
    Storage::FileSystem::{CreateFileW, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING},
    System::{
        Ioctl::{
            PropertyStandardQuery, StorageDeviceSeekPenaltyProperty,
            DEVICE_SEEK_PENALTY_DESCRIPTOR, IOCTL_STORAGE_QUERY_PROPERTY, STORAGE_PROPERTY_QUERY,
        },
        IO::DeviceIoControl,
    },
    UI::Shell::{SHGetFileInfoW, SHFILEINFOW, SHGFI_DISPLAYNAME},
};

use crate::{ScanConcurrency, ScanDeviceClass, VolumeInfo};

use super::path_identity;

static SCAN_CONCURRENCY_CACHE: OnceLock<Mutex<HashMap<PathBuf, ScanConcurrency>>> = OnceLock::new();

pub fn system_drive_path() -> PathBuf {
    let drive = env::var("SystemDrive").unwrap_or_else(|_| "C:".to_string());
    PathBuf::from(format!("{drive}\\"))
}

pub fn system_volume() -> Result<VolumeInfo, String> {
    volume_info(system_drive_path())
}

pub fn volumes() -> Result<Vec<VolumeInfo>, String> {
    let disks = logical_drive_paths()
        .map_err(|error| error.to_string())?
        .into_iter()
        .filter_map(|root| volume_info(root).ok())
        .collect::<Vec<_>>();
    if disks.is_empty() {
        return Err("windows_local_volume_unavailable".to_string());
    }
    Ok(disks)
}

fn volume_info(root: PathBuf) -> Result<VolumeInfo, String> {
    let (total_bytes, available_bytes) = disk_space(&root).map_err(|error| error.to_string())?;
    let scan_concurrency = scan_concurrency(&root);
    let drive = root
        .display()
        .to_string()
        .trim_end_matches('\\')
        .to_string();
    let label = volume_label(&root).unwrap_or_default();
    Ok(VolumeInfo {
        // The Shell display name is the same localized label shown by File
        // Explorer, including its generated name for an unlabeled volume.
        // Raw volume metadata remains a deterministic fallback for restricted
        // or non-interactive Windows sessions where Shell lookup is absent.
        name: shell_display_name(&root).unwrap_or_else(|| {
            if label.is_empty() {
                drive.clone()
            } else {
                format!("{label} ({drive})")
            }
        }),
        mount_point: path_identity::display(&root),
        total_bytes,
        available_bytes,
        used_bytes: total_bytes.saturating_sub(available_bytes),
        scan_concurrency,
    })
}

fn shell_display_name(root: &Path) -> Option<String> {
    let wide_root = root
        .as_os_str()
        .encode_wide()
        .chain(Some(0))
        .collect::<Vec<_>>();
    let mut info: SHFILEINFOW = unsafe { zeroed() };
    let result = unsafe {
        SHGetFileInfoW(
            wide_root.as_ptr(),
            0,
            &mut info,
            size_of::<SHFILEINFOW>() as u32,
            SHGFI_DISPLAYNAME,
        )
    };
    if result == 0 {
        return None;
    }
    let length = info
        .szDisplayName
        .iter()
        .position(|value| *value == 0)
        .unwrap_or(info.szDisplayName.len());
    let value = OsString::from_wide(&info.szDisplayName[..length])
        .to_string_lossy()
        .trim()
        .to_string();
    (!value.is_empty()).then_some(value)
}

fn scan_concurrency(root: &Path) -> ScanConcurrency {
    let cache = SCAN_CONCURRENCY_CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    if let Ok(cache) = cache.lock() {
        if let Some(cached) = cache.get(root) {
            return *cached;
        }
    }
    let measured = measure_scan_concurrency(root);
    if let Ok(mut cache) = cache.lock() {
        // Media classification is stable during an application session.
        // Removable media always remains single-worker. Caching by root avoids
        // reopening a volume device for both `system_volume` and `volumes`.
        cache.insert(root.to_path_buf(), measured);
    }
    measured
}

fn measure_scan_concurrency(root: &Path) -> ScanConcurrency {
    match drive_type(root) {
        Ok(DRIVE_REMOVABLE) => {
            return ScanConcurrency::conservative(ScanDeviceClass::Removable);
        }
        Ok(DRIVE_FIXED) => {}
        Ok(DRIVE_REMOTE) => {
            return ScanConcurrency::conservative(ScanDeviceClass::Network);
        }
        Ok(_) | Err(_) => {
            return ScanConcurrency::conservative(ScanDeviceClass::Unknown);
        }
    }
    match incurs_seek_penalty(root) {
        Ok(false) => ScanConcurrency::solid_state(),
        Ok(true) => ScanConcurrency::rotational(),
        Err(_) => ScanConcurrency::conservative(ScanDeviceClass::Unknown),
    }
}

fn incurs_seek_penalty(root: &Path) -> io::Result<bool> {
    let drive = root
        .to_string_lossy()
        .trim_end_matches(['\\', '/'])
        .to_string();
    let device = format!(r"\\.\{drive}")
        .encode_utf16()
        .chain(Some(0))
        .collect::<Vec<_>>();
    let handle = unsafe {
        CreateFileW(
            device.as_ptr(),
            0,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            ptr::null(),
            OPEN_EXISTING,
            0,
            ptr::null_mut(),
        )
    };
    if handle == INVALID_HANDLE_VALUE {
        return Err(io::Error::last_os_error());
    }
    let query = STORAGE_PROPERTY_QUERY {
        PropertyId: StorageDeviceSeekPenaltyProperty,
        QueryType: PropertyStandardQuery,
        AdditionalParameters: [0],
    };
    let mut descriptor: DEVICE_SEEK_PENALTY_DESCRIPTOR = unsafe { zeroed() };
    let mut returned = 0u32;
    let succeeded = unsafe {
        DeviceIoControl(
            handle,
            IOCTL_STORAGE_QUERY_PROPERTY,
            ptr::from_ref(&query).cast(),
            size_of::<STORAGE_PROPERTY_QUERY>() as u32,
            ptr::from_mut(&mut descriptor).cast(),
            size_of::<DEVICE_SEEK_PENALTY_DESCRIPTOR>() as u32,
            &mut returned,
            ptr::null_mut(),
        )
    };
    unsafe {
        CloseHandle(handle);
    }
    if succeeded == 0 || returned != size_of::<DEVICE_SEEK_PENALTY_DESCRIPTOR>() as u32 {
        return Err(io::Error::last_os_error());
    }
    Ok(descriptor.IncursSeekPenalty)
}

fn volume_label(root: &Path) -> io::Result<String> {
    #[link(name = "kernel32")]
    extern "system" {
        fn GetVolumeInformationW(
            root_path_name: *const u16,
            volume_name_buffer: *mut u16,
            volume_name_size: u32,
            volume_serial_number: *mut u32,
            maximum_component_length: *mut u32,
            file_system_flags: *mut u32,
            file_system_name_buffer: *mut u16,
            file_system_name_size: u32,
        ) -> i32;
    }

    let root = root
        .as_os_str()
        .encode_wide()
        .chain(Some(0))
        .collect::<Vec<_>>();
    let mut label = [0_u16; 261];
    let succeeded = unsafe {
        GetVolumeInformationW(
            root.as_ptr(),
            label.as_mut_ptr(),
            label.len() as u32,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            0,
        )
    };
    if succeeded == 0 {
        return Err(io::Error::last_os_error());
    }
    let length = label
        .iter()
        .position(|value| *value == 0)
        .unwrap_or(label.len());
    Ok(OsString::from_wide(&label[..length])
        .to_string_lossy()
        .into_owned())
}

fn logical_drive_paths() -> io::Result<Vec<PathBuf>> {
    #[link(name = "kernel32")]
    extern "system" {
        fn GetLogicalDrives() -> u32;
    }
    let mask = unsafe { GetLogicalDrives() };
    if mask == 0 {
        return Err(io::Error::last_os_error());
    }
    let mut roots = Vec::new();
    for index in 0..26_u32 {
        if mask & (1 << index) == 0 {
            continue;
        }
        let letter = char::from_u32(u32::from(b'A') + index).unwrap_or('C');
        let root = format!("{letter}:\\");
        let Ok(drive_type) = drive_type(Path::new(&root)) else {
            continue;
        };
        if drive_type == DRIVE_FIXED || drive_type == DRIVE_REMOVABLE {
            roots.push(PathBuf::from(root));
        }
    }
    Ok(roots)
}

const DRIVE_REMOVABLE: u32 = 2;
const DRIVE_FIXED: u32 = 3;
const DRIVE_REMOTE: u32 = 4;

fn drive_type(root: &Path) -> io::Result<u32> {
    #[link(name = "kernel32")]
    extern "system" {
        fn GetDriveTypeW(root_path_name: *const u16) -> u32;
    }
    const DRIVE_UNKNOWN: u32 = 0;
    let wide_root = root
        .as_os_str()
        .encode_wide()
        .chain(Some(0))
        .collect::<Vec<_>>();
    let drive_type = unsafe { GetDriveTypeW(wide_root.as_ptr()) };
    if drive_type == DRIVE_UNKNOWN {
        return Err(io::Error::last_os_error());
    }
    Ok(drive_type)
}

fn disk_space(path: &Path) -> io::Result<(u64, u64)> {
    #[link(name = "kernel32")]
    extern "system" {
        fn GetDiskFreeSpaceExW(
            directory_name: *const u16,
            free_bytes_available: *mut u64,
            total_number_of_bytes: *mut u64,
            total_number_of_free_bytes: *mut u64,
        ) -> i32;
    }
    let wide_path = path
        .as_os_str()
        .encode_wide()
        .chain(Some(0))
        .collect::<Vec<_>>();
    let (mut available, mut total, mut total_free) = (0_u64, 0_u64, 0_u64);
    let succeeded = unsafe {
        GetDiskFreeSpaceExW(
            wide_path.as_ptr(),
            &mut available,
            &mut total,
            &mut total_free,
        )
    };
    if succeeded == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok((total, available.min(total_free)))
}

#[cfg(test)]
mod tests {
    #[test]
    #[ignore = "reads localized Shell volume names from the Windows host"]
    fn real_volumes_use_shell_display_names() {
        let volumes = super::volumes().expect("Windows volumes should be readable");
        for volume in &volumes {
            eprintln!(
                "windows_volume name={:?} mount_point={:?}",
                volume.name, volume.mount_point
            );
            assert!(!volume.name.trim().is_empty());
            assert_ne!(volume.name, volume.mount_point);
        }
    }
}

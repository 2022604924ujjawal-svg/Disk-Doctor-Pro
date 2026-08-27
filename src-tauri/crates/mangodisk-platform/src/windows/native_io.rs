use std::{
    ffi::{c_void, OsString},
    io,
    mem::size_of,
    os::windows::ffi::{OsStrExt, OsStringExt},
    path::{Component, Path, PathBuf, Prefix},
    ptr,
};

use windows_sys::Win32::{
    Foundation::{CloseHandle, GetLastError, GENERIC_READ, HANDLE, INVALID_HANDLE_VALUE},
    Storage::FileSystem::{
        CreateFileW, FileIdType, GetFileInformationByHandle, GetFinalPathNameByHandleW,
        GetVolumeInformationW, GetVolumeNameForVolumeMountPointW, OpenFileById,
        BY_HANDLE_FILE_INFORMATION, FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT,
        FILE_ID_DESCRIPTOR, FILE_ID_DESCRIPTOR_0, FILE_LIST_DIRECTORY, FILE_NAME_NORMALIZED,
        FILE_READ_ATTRIBUTES, FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
        VOLUME_NAME_DOS,
    },
    System::IO::DeviceIoControl,
};

pub(in crate::windows) struct VolumePaths {
    pub(in crate::windows) device: String,
    pub(in crate::windows) root: PathBuf,
}

impl VolumePaths {
    pub(in crate::windows) fn from_path(path: &Path) -> Option<Self> {
        let mut components = path.components();
        let Component::Prefix(prefix) = components.next()? else {
            return None;
        };
        let drive = match prefix.kind() {
            Prefix::Disk(letter) | Prefix::VerbatimDisk(letter) => letter,
            _ => return None,
        };
        if !matches!(components.next(), Some(Component::RootDir)) {
            return None;
        }
        let letter = char::from(drive).to_ascii_uppercase();
        Some(Self {
            device: format!(r"\\.\{letter}:"),
            root: PathBuf::from(format!(r"{letter}:\")),
        })
    }

    pub(in crate::windows) fn from_scan_root(scan_root: &Path) -> Option<Self> {
        let volume = Self::from_path(scan_root)?;
        if scan_root.components().count() != 2 {
            return None;
        }
        Some(volume)
    }
}

pub(in crate::windows) fn file_id(path: &Path) -> io::Result<u64> {
    let handle = OwnedHandle::open_directory(path)?;
    let mut information = BY_HANDLE_FILE_INFORMATION::default();
    if unsafe { GetFileInformationByHandle(handle.raw(), &mut information) } == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok((u64::from(information.nFileIndexHigh) << 32) | u64::from(information.nFileIndexLow))
}

/// Exclusively owns every volume and directory handle. Callers borrow the raw
/// handle for synchronous APIs without duplicating ownership, so success,
/// cancellation, and error paths each close the handle exactly once.
pub(in crate::windows) struct OwnedHandle(HANDLE);

impl OwnedHandle {
    pub(in crate::windows) fn open_volume(device: &str) -> io::Result<Self> {
        let wide = device.encode_utf16().chain(Some(0)).collect::<Vec<_>>();
        Self::open(
            &wide,
            GENERIC_READ,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            0,
        )
    }

    pub(in crate::windows) fn open_directory(path: &Path) -> io::Result<Self> {
        let wide = path
            .as_os_str()
            .encode_wide()
            .chain(Some(0))
            .collect::<Vec<_>>();
        Self::open(
            &wide,
            0,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT,
        )
    }

    pub(in crate::windows) fn open_directory_listing(path: &Path) -> io::Result<Self> {
        let wide = path
            .as_os_str()
            .encode_wide()
            .chain(Some(0))
            .collect::<Vec<_>>();
        Self::open(
            &wide,
            FILE_LIST_DIRECTORY,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT,
        )
    }

    pub(in crate::windows) fn open_file_by_id(volume: &Self, file_id: u64) -> io::Result<Self> {
        let descriptor = FILE_ID_DESCRIPTOR {
            dwSize: size_of::<FILE_ID_DESCRIPTOR>() as u32,
            Type: FileIdType,
            Anonymous: FILE_ID_DESCRIPTOR_0 {
                FileId: file_id as i64,
            },
        };
        let handle = unsafe {
            OpenFileById(
                volume.raw(),
                &descriptor,
                FILE_READ_ATTRIBUTES,
                FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
                ptr::null(),
                FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT,
            )
        };
        if handle == INVALID_HANDLE_VALUE {
            Err(io::Error::last_os_error())
        } else {
            Ok(Self(handle))
        }
    }

    pub(in crate::windows) fn final_dos_path(&self) -> io::Result<PathBuf> {
        let flags = FILE_NAME_NORMALIZED | VOLUME_NAME_DOS;
        let required = unsafe { GetFinalPathNameByHandleW(self.raw(), ptr::null_mut(), 0, flags) };
        if required == 0 {
            return Err(io::Error::last_os_error());
        }
        let mut buffer = vec![0_u16; required as usize + 1];
        let written = unsafe {
            GetFinalPathNameByHandleW(self.raw(), buffer.as_mut_ptr(), buffer.len() as u32, flags)
        };
        if written == 0 || written as usize >= buffer.len() {
            return Err(io::Error::last_os_error());
        }
        Ok(PathBuf::from(OsString::from_wide(
            &buffer[..written as usize],
        )))
    }

    fn open(wide: &[u16], access: u32, share: u32, flags: u32) -> io::Result<Self> {
        let handle = unsafe {
            CreateFileW(
                wide.as_ptr(),
                access,
                share,
                ptr::null(),
                OPEN_EXISTING,
                flags,
                ptr::null_mut(),
            )
        };
        if handle == INVALID_HANDLE_VALUE {
            Err(io::Error::last_os_error())
        } else {
            Ok(Self(handle))
        }
    }

    pub(in crate::windows) fn raw(&self) -> HANDLE {
        self.0
    }
}

pub(in crate::windows) fn stable_volume_id(volume_root: &Path) -> io::Result<[u8; 16]> {
    let root = volume_root
        .as_os_str()
        .encode_wide()
        .chain(Some(0))
        .collect::<Vec<_>>();
    // A volume GUID path fits `\\?\Volume{GUID}\`. Reserving MAX_PATH avoids
    // coupling this boundary to the current textual representation.
    let mut name = [0_u16; 261];
    let succeeded = unsafe {
        GetVolumeNameForVolumeMountPointW(root.as_ptr(), name.as_mut_ptr(), name.len() as u32)
    };
    if succeeded == 0 {
        return Err(io::Error::last_os_error());
    }
    let length = name.iter().position(|value| *value == 0).ok_or_else(|| {
        io::Error::new(io::ErrorKind::InvalidData, "volume GUID is not terminated")
    })?;
    let normalized = OsString::from_wide(&name[..length])
        .to_string_lossy()
        .to_ascii_lowercase();
    let digest = blake3::hash(normalized.as_bytes());
    let mut volume_id = [0_u8; 16];
    volume_id.copy_from_slice(&digest.as_bytes()[..16]);
    Ok(volume_id)
}

pub(in crate::windows) fn is_ntfs(volume_root: &Path) -> io::Result<bool> {
    let root = volume_root
        .as_os_str()
        .encode_wide()
        .chain(Some(0))
        .collect::<Vec<_>>();
    let mut filesystem = [0_u16; 64];
    let succeeded = unsafe {
        GetVolumeInformationW(
            root.as_ptr(),
            ptr::null_mut(),
            0,
            ptr::null_mut(),
            ptr::null_mut(),
            ptr::null_mut(),
            filesystem.as_mut_ptr(),
            filesystem.len() as u32,
        )
    };
    if succeeded == 0 {
        return Err(io::Error::last_os_error());
    }
    let length = filesystem
        .iter()
        .position(|value| *value == 0)
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "filesystem name is not terminated",
            )
        })?;
    Ok(OsString::from_wide(&filesystem[..length])
        .to_string_lossy()
        .eq_ignore_ascii_case("ntfs"))
}

impl Drop for OwnedHandle {
    fn drop(&mut self) {
        unsafe {
            CloseHandle(self.0);
        }
    }
}

/// Stores Windows layout output in `u64` words to satisfy 64-bit field
/// alignment while exposing only bytes initialized by `DeviceIoControl`.
pub(in crate::windows) struct AlignedBuffer {
    words: Vec<u64>,
}

impl AlignedBuffer {
    pub(in crate::windows) fn new(bytes: usize) -> Self {
        Self {
            words: vec![0; bytes.div_ceil(size_of::<u64>())],
        }
    }

    pub(in crate::windows) fn as_mut_ptr(&mut self) -> *mut c_void {
        self.words.as_mut_ptr().cast()
    }

    pub(in crate::windows) fn capacity_bytes(&self) -> usize {
        self.words.len() * size_of::<u64>()
    }

    pub(in crate::windows) fn as_bytes(&self, initialized_bytes: usize) -> Option<&[u8]> {
        // DeviceIoControl should not report more bytes than the output buffer,
        // but this value still crosses the FFI boundary. Fail closed instead
        // of truncating a page that a parser could mistake for valid output.
        if initialized_bytes > self.capacity_bytes() {
            return None;
        }
        Some(unsafe {
            std::slice::from_raw_parts(self.words.as_ptr().cast::<u8>(), initialized_bytes)
        })
    }
}

/// Implementors guarantee that every bit pattern is valid and that the layout
/// matches the kernel ABI. This keeps `read_copy` safe while preventing future
/// use with restricted bit-pattern types such as `bool` or Rust enums.
///
/// # Safety
///
/// Implement only for fixed-layout types whose fields accept every bit
/// pattern. Structure size, field order, and alignment must exactly match the
/// Windows ABI returned by `DeviceIoControl`.
pub(in crate::windows) unsafe trait RawLayoutValue: Copy {}

pub(in crate::windows) fn read_copy<T: RawLayoutValue>(bytes: &[u8], offset: usize) -> Option<T> {
    let end = offset.checked_add(size_of::<T>())?;
    let source = bytes.get(offset..end)?;
    Some(unsafe { ptr::read_unaligned(source.as_ptr().cast::<T>()) })
}

pub(in crate::windows) fn device_io_control(
    handle: HANDLE,
    control_code: u32,
    input: *const c_void,
    input_bytes: usize,
    output: *mut c_void,
    output_bytes: usize,
) -> Result<usize, u32> {
    let mut returned = 0u32;
    let input_bytes = u32::try_from(input_bytes).map_err(|_| u32::MAX)?;
    let output_bytes = u32::try_from(output_bytes).map_err(|_| u32::MAX)?;
    let succeeded = unsafe {
        DeviceIoControl(
            handle,
            control_code,
            input,
            input_bytes,
            output,
            output_bytes,
            &mut returned,
            ptr::null_mut(),
        )
    };
    if succeeded == 0 {
        Err(unsafe { GetLastError() })
    } else {
        Ok(returned as usize)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn volume_root_accepts_drive_and_verbatim_drive_only() {
        let drive =
            VolumePaths::from_scan_root(Path::new(r"C:\")).expect("drive root should be supported");
        assert_eq!(drive.device, r"\\.\C:");
        assert_eq!(drive.root, Path::new(r"C:\"));
        assert!(VolumePaths::from_scan_root(Path::new(r"C:\Users")).is_none());
        assert!(VolumePaths::from_scan_root(Path::new(r"\\server\share")).is_none());
        let nested = VolumePaths::from_path(Path::new(r"c:\Users\sample-user-Δ"))
            .expect("nested path should resolve its volume");
        assert_eq!(nested.device, r"\\.\C:");
        assert_eq!(nested.root, Path::new(r"C:\"));
    }

    #[test]
    fn aligned_buffer_rejects_initialized_length_beyond_capacity() {
        let buffer = AlignedBuffer::new(8);

        assert_eq!(buffer.as_bytes(8).map(<[u8]>::len), Some(8));
        assert!(buffer.as_bytes(9).is_none());
    }
}

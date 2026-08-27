use std::{
    ffi::{CStr, CString, OsString},
    io,
    mem::size_of,
    os::{
        fd::{AsRawFd, FromRawFd, OwnedFd},
        unix::{ffi::OsStrExt, ffi::OsStringExt},
    },
    path::Path,
    ptr, thread,
};

const ATTR_CMN_ERROR: libc::attrgroup_t = 0x2000_0000;
const DIRECTORY_BUFFER_BYTES: usize = 64 * 1024;

pub(super) type FileSystemObjectType = u32;

pub(super) const VNODE_TYPE_REGULAR_FILE: FileSystemObjectType = 1;
pub(super) const VNODE_TYPE_DIRECTORY: FileSystemObjectType = 2;
pub(super) const VNODE_TYPE_SYMBOLIC_LINK: FileSystemObjectType = 5;

/// One physical directory entry returned by Darwin's bulk attribute API.
///
/// The shared representation intentionally contains only attributes required by MangoDisk's
/// analysis and cleanup scanners. Keeping parsing here prevents the two scan paths from drifting
/// on link handling, logical-size semantics, mount boundaries, or timestamp conversion.
#[derive(Debug)]
pub(super) struct BulkDirectoryEntry {
    pub(super) name: OsString,
    pub(super) device: u64,
    pub(super) object_type: FileSystemObjectType,
    pub(super) mount_status: u32,
    pub(super) flags: u32,
    pub(super) logical_bytes: u64,
    pub(super) modified_at_ms: Option<u64>,
    pub(super) attribute_error: u32,
    pub(super) record_length: usize,
}

/// Lightweight directory entry for scanners that only classify names and object types.
///
/// Project discovery does not need sizes, timestamps, device IDs, or mount metadata. Requesting
/// those attributes for millions of entries increases both kernel work and bytes copied to user
/// space. This smaller record keeps the shared parser and link semantics while avoiding analysis-
/// only metadata.
#[derive(Debug)]
pub(super) struct BulkNameDirectoryEntry {
    pub(super) name: OsString,
    pub(super) object_type: FileSystemObjectType,
    pub(super) attribute_error: u32,
    pub(super) record_length: usize,
}

pub(super) struct BulkDirectory(OwnedFd);

impl BulkDirectory {
    pub(super) fn open(path: &Path) -> io::Result<Self> {
        let path = CString::new(path.as_os_str().as_bytes())
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "path contains null byte"))?;
        let descriptor = unsafe {
            libc::open(
                path.as_ptr(),
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
            )
        };
        if descriptor < 0 {
            return Err(io::Error::last_os_error());
        }
        // `open` returned a new descriptor and this value becomes its only owner.
        Ok(Self(unsafe { OwnedFd::from_raw_fd(descriptor) }))
    }

    /// Reads the next kernel-filled page. An empty vector means end of directory.
    pub(super) fn read_page(
        &self,
        buffer: &mut AlignedBuffer,
    ) -> io::Result<Vec<BulkDirectoryEntry>> {
        let mut attributes = bulk_attributes();
        let entry_count = unsafe {
            // The descriptor and attrlist remain valid for this call. AlignedBuffer owns writable,
            // 8-byte-aligned storage matching the exact byte count supplied to the kernel.
            libc::getattrlistbulk(
                self.0.as_raw_fd(),
                ptr::from_mut(&mut attributes).cast(),
                buffer.as_mut_ptr().cast(),
                buffer.byte_len(),
                0,
            )
        };
        if entry_count < 0 {
            return Err(io::Error::last_os_error());
        }
        let entry_count = usize::try_from(entry_count)
            .map_err(|_| invalid_data("entry count conversion failed"))?;
        let mut entries = Vec::with_capacity(entry_count);
        let mut offset = 0_usize;
        for _ in 0..entry_count {
            let entry = parse_entry(buffer.as_bytes(), offset)?;
            offset = offset
                .checked_add(entry.record_length)
                .ok_or_else(|| invalid_data("record offset overflow"))?;
            entries.push(entry);
        }
        Ok(entries)
    }

    /// Reads the next page with only the attributes required for name-based discovery.
    pub(super) fn read_name_page(
        &self,
        buffer: &mut AlignedBuffer,
    ) -> io::Result<Vec<BulkNameDirectoryEntry>> {
        let mut attributes = bulk_name_attributes();
        let entry_count = unsafe {
            // The descriptor, attrlist, and aligned writable buffer remain valid for this call.
            libc::getattrlistbulk(
                self.0.as_raw_fd(),
                ptr::from_mut(&mut attributes).cast(),
                buffer.as_mut_ptr().cast(),
                buffer.byte_len(),
                0,
            )
        };
        if entry_count < 0 {
            return Err(io::Error::last_os_error());
        }
        let entry_count = usize::try_from(entry_count)
            .map_err(|_| invalid_data("entry count conversion failed"))?;
        let mut entries = Vec::with_capacity(entry_count);
        let mut offset = 0_usize;
        for _ in 0..entry_count {
            let entry = parse_name_entry(buffer.as_bytes(), offset)?;
            offset = offset
                .checked_add(entry.record_length)
                .ok_or_else(|| invalid_data("record offset overflow"))?;
            entries.push(entry);
        }
        Ok(entries)
    }
}

pub(super) struct AlignedBuffer(Vec<u64>);

impl AlignedBuffer {
    pub(super) fn new() -> Self {
        Self(vec![0; DIRECTORY_BUFFER_BYTES.div_ceil(size_of::<u64>())])
    }

    fn as_mut_ptr(&mut self) -> *mut u8 {
        self.0.as_mut_ptr().cast()
    }

    fn byte_len(&self) -> usize {
        self.0.len() * size_of::<u64>()
    }

    fn as_bytes(&self) -> &[u8] {
        unsafe {
            // Vec<u64> owns initialized contiguous storage and the byte slice cannot outlive it.
            std::slice::from_raw_parts(self.0.as_ptr().cast(), self.byte_len())
        }
    }
}

/// Returns one bounded worker count for every Darwin bulk-directory consumer.
///
/// Directory enumeration benefits from overlapping metadata reads across independent
/// subdirectories, while an unbounded pool competes with foreground work and can make scans less
/// predictable on fast storage. Keeping the policy beside the shared reader prevents analysis and
/// cleanup from drifting to different concurrency limits.
pub(super) fn worker_count(maximum: usize) -> usize {
    thread::available_parallelism()
        .map(|parallelism| parallelism.get())
        .unwrap_or(4)
        .clamp(1, maximum.max(1))
}

fn bulk_attributes() -> libc::attrlist {
    libc::attrlist {
        bitmapcount: libc::ATTR_BIT_MAP_COUNT,
        reserved: 0,
        commonattr: libc::ATTR_CMN_RETURNED_ATTRS
            | ATTR_CMN_ERROR
            | libc::ATTR_CMN_NAME
            | libc::ATTR_CMN_DEVID
            | libc::ATTR_CMN_OBJTYPE
            | libc::ATTR_CMN_MODTIME
            | libc::ATTR_CMN_FLAGS,
        volattr: 0,
        dirattr: libc::ATTR_DIR_MOUNTSTATUS,
        fileattr: libc::ATTR_FILE_DATALENGTH,
        forkattr: 0,
    }
}

fn bulk_name_attributes() -> libc::attrlist {
    libc::attrlist {
        bitmapcount: libc::ATTR_BIT_MAP_COUNT,
        reserved: 0,
        commonattr: libc::ATTR_CMN_RETURNED_ATTRS
            | ATTR_CMN_ERROR
            | libc::ATTR_CMN_NAME
            | libc::ATTR_CMN_OBJTYPE,
        volattr: 0,
        dirattr: 0,
        fileattr: 0,
        forkattr: 0,
    }
}

fn parse_entry(buffer: &[u8], offset: usize) -> io::Result<BulkDirectoryEntry> {
    let record_length = usize::try_from(read_unaligned::<u32>(buffer, offset)?)
        .map_err(|_| invalid_data("record length conversion failed"))?;
    let record_end = offset
        .checked_add(record_length)
        .filter(|end| *end <= buffer.len())
        .ok_or_else(|| invalid_data("record exceeds buffer"))?;
    let mut cursor = offset + size_of::<u32>();
    let returned = read_unaligned::<libc::attribute_set_t>(buffer, cursor)?;
    cursor += size_of::<libc::attribute_set_t>();
    let attribute_error = if returned.commonattr & ATTR_CMN_ERROR != 0 {
        let value = read_unaligned::<u32>(buffer, cursor)?;
        cursor += size_of::<u32>();
        value
    } else {
        0
    };
    if returned.commonattr & libc::ATTR_CMN_NAME == 0 {
        return Err(invalid_data("name attribute missing"));
    }
    let name_reference_offset = cursor;
    let name_reference = read_unaligned::<libc::attrreference_t>(buffer, cursor)?;
    cursor += size_of::<libc::attrreference_t>();
    let raw_device = required_attribute(
        returned.commonattr & libc::ATTR_CMN_DEVID != 0,
        read_unaligned::<libc::dev_t>(buffer, cursor),
        "device attribute missing",
    )?;
    cursor += size_of::<libc::dev_t>();
    let object_type = required_attribute(
        returned.commonattr & libc::ATTR_CMN_OBJTYPE != 0,
        read_unaligned::<FileSystemObjectType>(buffer, cursor),
        "object type attribute missing",
    )?;
    cursor += size_of::<FileSystemObjectType>();
    let modified_at_ms = if returned.commonattr & libc::ATTR_CMN_MODTIME != 0 {
        let value = read_unaligned::<libc::timespec>(buffer, cursor)?;
        cursor += size_of::<libc::timespec>();
        timestamp_ms(value)
    } else {
        None
    };
    let flags = if returned.commonattr & libc::ATTR_CMN_FLAGS != 0 {
        let value = read_unaligned::<u32>(buffer, cursor)?;
        cursor += size_of::<u32>();
        value
    } else {
        0
    };
    let mount_status = if returned.dirattr & libc::ATTR_DIR_MOUNTSTATUS != 0 {
        let value = read_unaligned::<u32>(buffer, cursor)?;
        cursor += size_of::<u32>();
        value
    } else {
        0
    };
    let data_length = if returned.fileattr & libc::ATTR_FILE_DATALENGTH != 0 {
        read_unaligned::<libc::off_t>(buffer, cursor)?
    } else {
        0
    };
    let name_start = signed_offset(name_reference_offset, name_reference.attr_dataoffset)?;
    let name_length = usize::try_from(name_reference.attr_length)
        .map_err(|_| invalid_data("name length conversion failed"))?;
    let name_end = name_start
        .checked_add(name_length)
        .filter(|end| *end <= record_end)
        .ok_or_else(|| invalid_data("name exceeds record"))?;
    let name = CStr::from_bytes_until_nul(&buffer[name_start..name_end])
        .map_err(|_| invalid_data("name is not null terminated"))?
        .to_bytes()
        .to_vec();

    Ok(BulkDirectoryEntry {
        name: OsString::from_vec(name),
        device: u64::try_from(raw_device).map_err(|_| invalid_data("negative device id"))?,
        object_type,
        mount_status,
        flags,
        logical_bytes: u64::try_from(data_length)
            .map_err(|_| invalid_data("negative file length"))?,
        modified_at_ms,
        attribute_error,
        record_length,
    })
}

fn parse_name_entry(buffer: &[u8], offset: usize) -> io::Result<BulkNameDirectoryEntry> {
    let record_length = usize::try_from(read_unaligned::<u32>(buffer, offset)?)
        .map_err(|_| invalid_data("record length conversion failed"))?;
    let record_end = offset
        .checked_add(record_length)
        .filter(|end| *end <= buffer.len())
        .ok_or_else(|| invalid_data("record exceeds buffer"))?;
    let mut cursor = offset + size_of::<u32>();
    let returned = read_unaligned::<libc::attribute_set_t>(buffer, cursor)?;
    cursor += size_of::<libc::attribute_set_t>();
    let attribute_error = if returned.commonattr & ATTR_CMN_ERROR != 0 {
        let value = read_unaligned::<u32>(buffer, cursor)?;
        cursor += size_of::<u32>();
        value
    } else {
        0
    };
    if returned.commonattr & libc::ATTR_CMN_NAME == 0 {
        return Err(invalid_data("name attribute missing"));
    }
    let name_reference_offset = cursor;
    let name_reference = read_unaligned::<libc::attrreference_t>(buffer, cursor)?;
    cursor += size_of::<libc::attrreference_t>();
    let object_type = required_attribute(
        returned.commonattr & libc::ATTR_CMN_OBJTYPE != 0,
        read_unaligned::<FileSystemObjectType>(buffer, cursor),
        "object type attribute missing",
    )?;
    let name = parse_name(buffer, record_end, name_reference_offset, name_reference)?;

    Ok(BulkNameDirectoryEntry {
        name,
        object_type,
        attribute_error,
        record_length,
    })
}

fn parse_name(
    buffer: &[u8],
    record_end: usize,
    name_reference_offset: usize,
    name_reference: libc::attrreference_t,
) -> io::Result<OsString> {
    let name_start = signed_offset(name_reference_offset, name_reference.attr_dataoffset)?;
    let name_length = usize::try_from(name_reference.attr_length)
        .map_err(|_| invalid_data("name length conversion failed"))?;
    let name_end = name_start
        .checked_add(name_length)
        .filter(|end| *end <= record_end)
        .ok_or_else(|| invalid_data("name exceeds record"))?;
    let name = CStr::from_bytes_until_nul(&buffer[name_start..name_end])
        .map_err(|_| invalid_data("name is not null terminated"))?
        .to_bytes()
        .to_vec();
    Ok(OsString::from_vec(name))
}

fn required_attribute<T>(present: bool, value: io::Result<T>, message: &str) -> io::Result<T> {
    if present {
        value
    } else {
        Err(invalid_data(message))
    }
}

fn read_unaligned<T: Copy>(buffer: &[u8], offset: usize) -> io::Result<T> {
    let end = offset
        .checked_add(size_of::<T>())
        .filter(|end| *end <= buffer.len())
        .ok_or_else(|| invalid_data("attribute exceeds buffer"))?;
    let pointer = buffer[offset..end].as_ptr().cast::<T>();
    // Darwin packs attributes at four-byte boundaries, so wider values require unaligned reads.
    Ok(unsafe { pointer.read_unaligned() })
}

fn signed_offset(base: usize, relative: i32) -> io::Result<usize> {
    if relative >= 0 {
        base.checked_add(relative as usize)
    } else {
        base.checked_sub(relative.unsigned_abs() as usize)
    }
    .ok_or_else(|| invalid_data("attribute reference overflow"))
}

fn timestamp_ms(value: libc::timespec) -> Option<u64> {
    let seconds = u64::try_from(value.tv_sec).ok()?;
    let nanoseconds = u64::try_from(value.tv_nsec).ok()?;
    Some(
        seconds
            .saturating_mul(1_000)
            .saturating_add(nanoseconds / 1_000_000),
    )
}

fn invalid_data(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

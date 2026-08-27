use std::{
    ffi::{c_void, OsString},
    io,
    mem::{offset_of, size_of},
    os::windows::ffi::OsStringExt,
    path::Path,
    ptr,
};

use windows_sys::Win32::{
    Foundation::{GetLastError, ERROR_NO_MORE_FILES},
    Storage::FileSystem::{
        FileIdBothDirectoryInfo, GetFileInformationByHandle, GetFileInformationByHandleEx,
        BY_HANDLE_FILE_INFORMATION, FILE_ID_BOTH_DIR_INFO,
    },
};

use crate::{DirectoryEntryIdentities, PhysicalFileIdentity, PlatformCancellation};

use super::native_io::OwnedHandle;

const DIRECTORY_QUERY_BUFFER_BYTES: usize = 64 * 1024;

pub(super) fn read(
    directory: &Path,
    cancellation: &PlatformCancellation,
) -> io::Result<DirectoryEntryIdentities> {
    ensure_not_cancelled(cancellation)?;
    let handle = OwnedHandle::open_directory_listing(directory)?;
    let serial_number = directory_volume_serial_number(&handle)?;
    let mut entries = DirectoryEntryIdentities::new();
    // `u64` storage gives the variable-length Win32 records their required native alignment while
    // still allowing the API to fill the buffer as bytes.
    let mut storage = vec![0_u64; DIRECTORY_QUERY_BUFFER_BYTES / size_of::<u64>()];

    loop {
        // Native enumeration may require multiple buffer-sized calls for one directory. Checking
        // between calls keeps cancellation responsive without abandoning the faster batch API.
        ensure_not_cancelled(cancellation)?;
        let succeeded = unsafe {
            GetFileInformationByHandleEx(
                handle.raw(),
                FileIdBothDirectoryInfo,
                storage.as_mut_ptr().cast::<c_void>(),
                DIRECTORY_QUERY_BUFFER_BYTES as u32,
            )
        };
        if succeeded == 0 {
            let error = unsafe { GetLastError() };
            if error == ERROR_NO_MORE_FILES {
                break;
            }
            return Err(io::Error::from_raw_os_error(error as i32));
        }
        parse_records(&storage, serial_number, &mut entries)?;
    }
    Ok(entries)
}

fn ensure_not_cancelled(cancellation: &PlatformCancellation) -> io::Result<()> {
    if cancellation.is_cancelled() {
        Err(io::Error::new(
            io::ErrorKind::Interrupted,
            "directory identity enumeration cancelled",
        ))
    } else {
        Ok(())
    }
}

fn directory_volume_serial_number(handle: &OwnedHandle) -> io::Result<u64> {
    let mut information = BY_HANDLE_FILE_INFORMATION::default();
    if unsafe { GetFileInformationByHandle(handle.raw(), &mut information) } == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(u64::from(information.dwVolumeSerialNumber))
}

fn parse_records(
    storage: &[u64],
    serial_number: u64,
    entries: &mut DirectoryEntryIdentities,
) -> io::Result<()> {
    let bytes = storage.len().saturating_mul(size_of::<u64>());
    let base = storage.as_ptr().cast::<u8>();
    let mut offset = 0_usize;
    loop {
        if offset.saturating_add(size_of::<FILE_ID_BOTH_DIR_INFO>()) > bytes {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "directory identity record header exceeds the native buffer",
            ));
        }
        let record =
            unsafe { ptr::read_unaligned(base.add(offset).cast::<FILE_ID_BOTH_DIR_INFO>()) };
        let name_bytes = usize::try_from(record.FileNameLength).unwrap_or(usize::MAX);
        let name_offset = offset.saturating_add(offset_of!(FILE_ID_BOTH_DIR_INFO, FileName));
        if name_bytes % size_of::<u16>() != 0
            || name_offset % std::mem::align_of::<u16>() != 0
            || name_offset.saturating_add(name_bytes) > bytes
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "directory identity record name exceeds the native buffer",
            ));
        }
        let name = unsafe {
            std::slice::from_raw_parts(
                base.add(name_offset).cast::<u16>(),
                name_bytes / size_of::<u16>(),
            )
        };
        let name = OsString::from_wide(name);
        if name != "." && name != ".." {
            entries.insert(
                name,
                PhysicalFileIdentity {
                    volume: serial_number,
                    index: record.FileId as u64,
                },
            );
        }
        if record.NextEntryOffset == 0 {
            return Ok(());
        }
        let next = usize::try_from(record.NextEntryOffset).unwrap_or(usize::MAX);
        if next < size_of::<FILE_ID_BOTH_DIR_INFO>() || offset.saturating_add(next) >= bytes {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "directory identity record offset is invalid",
            ));
        }
        offset += next;
    }
}

#[cfg(test)]
mod tests {
    use std::{
        fs,
        sync::{
            atomic::{AtomicUsize, Ordering},
            Arc,
        },
        time::{SystemTime, UNIX_EPOCH},
    };

    use crate::PlatformCancellation;

    use super::read;

    fn fixture_root(label: &str) -> std::path::PathBuf {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("read the directory identity fixture clock")
            .as_nanos();
        std::env::temp_dir().join(format!(
            "mangodisk-directory-identity-{label}-{}-{unique}",
            std::process::id()
        ))
    }

    fn never_cancelled() -> PlatformCancellation {
        PlatformCancellation::new(|| false)
    }

    #[test]
    fn directory_batch_reports_equal_identity_for_hard_links() {
        let root = fixture_root("hard-links");
        fs::create_dir_all(&root).expect("create the directory identity fixture");
        let original = root.join("original.bin");
        let alias = root.join("alias.bin");
        fs::write(&original, b"identity").expect("write the identity fixture");
        fs::hard_link(&original, &alias).expect("create the hard-link fixture");

        let identities = read(&root, &never_cancelled()).expect("enumerate directory identities");
        assert_eq!(
            identities.get(original.file_name().expect("original file name")),
            identities.get(alias.file_name().expect("alias file name"))
        );

        fs::remove_dir_all(root).expect("remove the directory identity fixture");
    }

    #[test]
    fn directory_batch_reads_entries_across_multiple_native_buffers() {
        const ENTRY_COUNT: usize = 1_024;

        let root = fixture_root("multiple-buffers");
        fs::create_dir_all(&root).expect("create the multi-buffer identity fixture");
        let names = (0..ENTRY_COUNT)
            .map(|index| format!("entry-{index:04}-long-name-for-native-buffer-coverage.bin"))
            .collect::<Vec<_>>();
        for name in &names {
            fs::write(root.join(name), b"identity").expect("write a multi-buffer identity entry");
        }

        let identities = read(&root, &never_cancelled()).expect("enumerate every identity buffer");

        assert_eq!(identities.len(), ENTRY_COUNT);
        assert!(names
            .iter()
            .all(|name| identities.contains_key(std::ffi::OsStr::new(name))));
        fs::remove_dir_all(root).expect("remove the multi-buffer identity fixture");
    }

    #[test]
    fn directory_batch_checks_cancellation_between_native_buffers() {
        const ENTRY_COUNT: usize = 1_024;

        let root = fixture_root("cancellation");
        fs::create_dir_all(&root).expect("create the cancellation identity fixture");
        for index in 0..ENTRY_COUNT {
            fs::write(
                root.join(format!(
                    "entry-{index:04}-long-name-for-cancellation-buffer-coverage.bin"
                )),
                b"identity",
            )
            .expect("write a cancellation identity entry");
        }
        let probes = Arc::new(AtomicUsize::new(0));
        let cancellation_probes = Arc::clone(&probes);
        let cancellation = PlatformCancellation::new(move || {
            cancellation_probes.fetch_add(1, Ordering::Relaxed) >= 2
        });

        let error = read(&root, &cancellation)
            .expect_err("multi-buffer enumeration should observe cancellation");

        assert_eq!(error.kind(), std::io::ErrorKind::Interrupted);
        assert!(probes.load(Ordering::Relaxed) >= 3);
        fs::remove_dir_all(root).expect("remove the cancellation identity fixture");
    }
}

use std::{
    ffi::{c_void, OsString},
    os::windows::ffi::OsStringExt,
    path::PathBuf,
};

use windows::{
    core::{GUID, PWSTR},
    Win32::{
        System::{Com::CoTaskMemFree, SystemInformation::GetWindowsDirectoryW},
        UI::Shell::{
            FOLDERID_ProgramData, FOLDERID_ProgramFiles, FOLDERID_ProgramFilesX86,
            SHGetKnownFolderPath, KF_FLAG_DEFAULT,
        },
    },
};

use crate::{ApplicationDirectories, PlatformError, PlatformResult, UserDirectories};

const MAX_WINDOWS_DIRECTORY_UTF16_LENGTH: usize = 32_768;

pub(super) fn application_directories(identifier: &str) -> PlatformResult<ApplicationDirectories> {
    let local_data = local_data_directory()?.join(identifier);
    Ok(ApplicationDirectories {
        cache_directory: local_data.clone(),
        local_data_directory: local_data,
    })
}

pub(super) fn user_directories() -> PlatformResult<UserDirectories> {
    let home_directory = dirs::home_dir()
        .ok_or_else(|| PlatformError::invalid_path("Windows user profile is unavailable"))?;
    let local_data = local_data_directory()?;
    let roaming_data = dirs::data_dir().ok_or_else(|| {
        PlatformError::invalid_path("Windows roaming application data is unavailable")
    })?;
    // Windows defines both cache and local application data under the same
    // Known Folder. Preserve that identity so the shared contract de-duplicates
    // the root before Core consumes it.
    Ok(UserDirectories::new(
        home_directory,
        std::env::temp_dir(),
        local_data.clone(),
        [local_data, roaming_data],
    ))
}

pub(super) fn system_directory() -> PlatformResult<PathBuf> {
    let mut buffer = vec![0_u16; MAX_WINDOWS_DIRECTORY_UTF16_LENGTH];
    let length = unsafe { GetWindowsDirectoryW(Some(&mut buffer)) } as usize;
    if length == 0 || length >= buffer.len() {
        return Err(PlatformError::operation_failed(
            "Windows system directory lookup failed",
        ));
    }
    Ok(PathBuf::from(OsString::from_wide(&buffer[..length])))
}

pub(super) fn program_files_directories() -> PlatformResult<Vec<PathBuf>> {
    let primary = known_folder(FOLDERID_ProgramFiles, "Program Files")?;
    let mut directories = vec![primary];
    let x86 = known_folder(FOLDERID_ProgramFilesX86, "Program Files (x86)")?;
    if !directories.contains(&x86) {
        directories.push(x86);
    }
    Ok(directories)
}

pub(super) fn program_data_directory() -> PlatformResult<PathBuf> {
    known_folder(FOLDERID_ProgramData, "ProgramData")
}

pub(super) fn local_data_directory() -> PlatformResult<PathBuf> {
    dirs::data_local_dir()
        .ok_or_else(|| PlatformError::invalid_path("Windows local application data is unavailable"))
}

fn known_folder(identifier: GUID, diagnostic_name: &'static str) -> PlatformResult<PathBuf> {
    let value =
        unsafe { SHGetKnownFolderPath(&identifier, KF_FLAG_DEFAULT, None) }.map_err(|error| {
            PlatformError::operation_failed(format!(
                "Windows {diagnostic_name} lookup failed code={}",
                error.code().0
            ))
        })?;
    let decoded = unsafe { value.to_string() };
    free_com_string(value);
    decoded.map(PathBuf::from).map_err(|_| {
        PlatformError::operation_failed(format!(
            "Windows {diagnostic_name} path is not valid UTF-16"
        ))
    })
}

fn free_com_string(value: PWSTR) {
    if !value.is_null() {
        unsafe { CoTaskMemFree(Some(value.0.cast::<c_void>())) };
    }
}

#[cfg(test)]
mod tests {
    use super::{program_files_directories, system_directory, user_directories};

    #[test]
    fn native_standard_directories_are_absolute_and_deduplicated() {
        let user = user_directories().expect("Windows user directories should be available");
        assert!(user.home_directory().is_absolute());
        assert!(user.temporary_directory().is_absolute());
        assert!(user.cache_directory().is_absolute());
        assert!(user
            .application_storage_directories()
            .iter()
            .all(|directory| directory.is_absolute()));

        let program_files =
            program_files_directories().expect("Program Files directories should be available");
        assert!(!program_files.is_empty());
        assert!(program_files
            .iter()
            .all(|directory| directory.is_absolute()));
        for (index, directory) in program_files.iter().enumerate() {
            assert!(!program_files[..index].contains(directory));
        }
        assert!(system_directory()
            .expect("Windows system directory should be available")
            .is_absolute());
    }
}

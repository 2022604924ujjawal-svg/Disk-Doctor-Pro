use std::path::{Path, PathBuf};

use serde::Serialize;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VolumeInfo {
    pub name: String,
    pub mount_point: String,
    pub total_bytes: u64,
    pub available_bytes: u64,
    pub used_bytes: u64,
    /// Scheduling metadata is Rust-only and is not part of the serialized disk
    /// display model. Device classification stays in the platform layer so core
    /// does not depend on diskutil or Windows IOCTL details.
    #[serde(skip)]
    pub scan_concurrency: ScanConcurrency,
}

/// Root scans use the most conservative concurrency limit among involved
/// volumes. The class is diagnostic only; `worker_limit` drives scheduling.
/// Detection failures must remain `Unknown` rather than assuming solid state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScanConcurrency {
    pub class: ScanDeviceClass,
    pub worker_limit: usize,
}

impl ScanConcurrency {
    pub const fn solid_state() -> Self {
        Self {
            class: ScanDeviceClass::SolidState,
            worker_limit: 4,
        }
    }

    pub const fn rotational() -> Self {
        Self {
            class: ScanDeviceClass::Rotational,
            worker_limit: 2,
        }
    }

    pub const fn conservative(class: ScanDeviceClass) -> Self {
        Self {
            class,
            worker_limit: 1,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScanDeviceClass {
    SolidState,
    Rotational,
    Removable,
    Network,
    Unknown,
}

impl ScanDeviceClass {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::SolidState => "solid_state",
            Self::Rotational => "rotational",
            Self::Removable => "removable",
            Self::Network => "network",
            Self::Unknown => "unknown",
        }
    }
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct UserDirectories {
    home_directory: PathBuf,
    temporary_directory: PathBuf,
    cache_directory: PathBuf,
    application_storage_directories: Vec<PathBuf>,
}

impl UserDirectories {
    pub(crate) fn new(
        home_directory: PathBuf,
        temporary_directory: PathBuf,
        cache_directory: PathBuf,
        application_data_directories: impl IntoIterator<Item = PathBuf>,
    ) -> Self {
        let mut application_storage_directories = vec![cache_directory.clone()];
        for directory in application_data_directories {
            if !application_storage_directories.contains(&directory) {
                application_storage_directories.push(directory);
            }
        }
        Self {
            home_directory,
            temporary_directory,
            cache_directory,
            application_storage_directories,
        }
    }

    pub fn home_directory(&self) -> &Path {
        &self.home_directory
    }

    pub fn temporary_directory(&self) -> &Path {
        &self.temporary_directory
    }

    pub fn cache_directory(&self) -> &Path {
        &self.cache_directory
    }

    /// Returns roots that contain per-user application state or disposable
    /// application data. The list is de-duplicated because some platforms use
    /// the same standard directory for both local data and caches.
    pub fn application_storage_directories(&self) -> &[PathBuf] {
        &self.application_storage_directories
    }
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct ApplicationDirectories {
    pub local_data_directory: PathBuf,
    pub cache_directory: PathBuf,
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use super::UserDirectories;

    #[test]
    fn user_directories_deduplicate_cache_from_application_storage() {
        let directories = UserDirectories::new(
            PathBuf::from("/home/example"),
            PathBuf::from("/tmp"),
            PathBuf::from("/data/local"),
            [PathBuf::from("/data/local"), PathBuf::from("/data/roaming")],
        );

        assert_eq!(
            directories.application_storage_directories(),
            [PathBuf::from("/data/local"), PathBuf::from("/data/roaming")]
        );
        assert_eq!(directories.cache_directory(), Path::new("/data/local"));
    }
}

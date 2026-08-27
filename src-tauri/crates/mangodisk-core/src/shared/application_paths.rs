use std::{
    path::{Path, PathBuf},
    sync::OnceLock,
};

use super::{CoreError, CoreResult};

static APPLICATION_PATHS: OnceLock<ApplicationPaths> = OnceLock::new();

/// Application-owned storage roots supplied by the active adapter.
///
/// Core owns the data lifecycle below these roots but never derives operating
/// system directories or depends on Tauri's path resolver.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ApplicationPaths {
    data_directory: PathBuf,
    cache_directory: PathBuf,
    runtime_directory: PathBuf,
}

impl ApplicationPaths {
    pub fn from_base_directories(
        local_data_directory: PathBuf,
        cache_directory: PathBuf,
    ) -> CoreResult<Self> {
        Self::new(
            local_data_directory.join("data"),
            cache_directory.join("cache"),
            local_data_directory.join("runtime"),
        )
    }

    pub fn new(
        data_directory: PathBuf,
        cache_directory: PathBuf,
        runtime_directory: PathBuf,
    ) -> CoreResult<Self> {
        for (name, path) in [
            ("data", data_directory.as_path()),
            ("cache", cache_directory.as_path()),
            ("runtime", runtime_directory.as_path()),
        ] {
            if !path.is_absolute() {
                return Err(CoreError::invalid_input(format!(
                    "application {name} directory must be absolute"
                )));
            }
        }
        if data_directory == cache_directory
            || data_directory == runtime_directory
            || cache_directory == runtime_directory
        {
            return Err(CoreError::invalid_input(
                "application storage directories must be distinct",
            ));
        }
        Ok(Self {
            data_directory,
            cache_directory,
            runtime_directory,
        })
    }

    pub fn data_directory(&self) -> &Path {
        &self.data_directory
    }

    pub fn cache_directory(&self) -> &Path {
        &self.cache_directory
    }

    pub fn runtime_directory(&self) -> &Path {
        &self.runtime_directory
    }
}

pub fn configure_application_paths(paths: ApplicationPaths) -> CoreResult<()> {
    if let Some(existing) = APPLICATION_PATHS.get() {
        return if existing == &paths {
            Ok(())
        } else {
            Err(CoreError::operation_failed(
                "application storage paths are already configured",
            ))
        };
    }
    APPLICATION_PATHS
        .set(paths)
        .map_err(|_| CoreError::operation_failed("failed to configure application storage paths"))
}

#[cfg(not(test))]
pub(crate) fn application_paths() -> CoreResult<&'static ApplicationPaths> {
    APPLICATION_PATHS
        .get()
        .ok_or_else(|| CoreError::operation_failed("application storage paths are not configured"))
}

#[cfg(test)]
pub(crate) fn application_paths() -> CoreResult<&'static ApplicationPaths> {
    static TEST_PATHS: OnceLock<ApplicationPaths> = OnceLock::new();
    Ok(TEST_PATHS.get_or_init(|| {
        let root =
            std::env::temp_dir().join(format!("mangodisk-core-tests-{}", std::process::id()));
        ApplicationPaths::new(root.join("data"), root.join("cache"), root.join("runtime"))
            .expect("test application paths must be valid")
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_require_absolute_distinct_directories() {
        let root = std::env::temp_dir().join("mangodisk-application-paths");
        assert!(
            ApplicationPaths::new(root.join("data"), root.join("cache"), root.join("runtime"))
                .is_ok()
        );
        assert!(ApplicationPaths::new(
            PathBuf::from("data"),
            root.join("cache"),
            root.join("runtime")
        )
        .is_err());
        assert!(
            ApplicationPaths::new(root.join("data"), root.join("data"), root.join("runtime"))
                .is_err()
        );
    }

    #[test]
    fn base_directories_expand_to_semantic_subdirectories() {
        let root = std::env::temp_dir().join("mangodisk-application-path-layout");
        let paths =
            ApplicationPaths::from_base_directories(root.join("local"), root.join("system-cache"))
                .expect("application paths should be derived from absolute roots");

        assert_eq!(paths.data_directory(), root.join("local").join("data"));
        assert_eq!(
            paths.runtime_directory(),
            root.join("local").join("runtime")
        );
        assert_eq!(
            paths.cache_directory(),
            root.join("system-cache").join("cache")
        );
    }
}

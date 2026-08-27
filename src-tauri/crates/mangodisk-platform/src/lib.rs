mod command;
mod contracts;
mod current;
mod file_icon;
mod inventory;
#[cfg(target_os = "macos")]
mod macos;
mod startup_helper;
#[cfg(windows)]
mod system_settings_helper;
#[cfg(windows)]
mod windows;

#[cfg(windows)]
pub use command::configure_background_process;
pub use command::{
    run_controlled_command, ControlledCommandError, ControlledCommandLimits,
    ControlledCommandOutput, ControlledEnvironmentPolicy, ControlledExecutable,
};
pub use contracts::*;
pub use current::{application_directories, current_platform, CurrentPlatform};
pub use file_icon::{
    NativeFileIconAsset, NativeFileIconAssignment, NativeFileIconItemKind,
    NativeFileIconLoadResult, NativeFileIconMode, NativeFileIconRequest, NativeFileIconService,
};
#[cfg(target_os = "macos")]
pub use macos::{
    macos_privileged_application_removal_supported, remove_application_bundle_with_privileges,
};
pub use startup_helper::run_startup_helper_mode;
#[cfg(windows)]
pub use system_settings_helper::run_system_settings_helper_mode;
#[cfg(windows)]
pub use windows::{
    execute_windows_disk_cleanup, fresh_windows_disk_cleanup_estimates,
    windows_disk_cleanup_estimates,
};

#[cfg(test)]
mod startup_baseline_tests {
    use std::collections::BTreeSet;

    use super::{current_platform, PlatformCancellation, StartupPlatform};

    #[test]
    #[ignore = "requires the host startup configuration"]
    fn actual_startup_source_baseline_has_unique_source_ids() {
        let cancellation = PlatformCancellation::new(|| false);
        let results = current_platform()
            .scan_startup_sources(&cancellation)
            .expect("the host startup scan should return a catalog");
        let mut source_ids = BTreeSet::new();
        for source in results {
            println!(
                "source_id={} status={:?} item_count={} elapsed_ms={}",
                source.source_id,
                source.status,
                source.items.len(),
                source.elapsed_ms
            );
            assert!(
                source_ids.insert(source.source_id),
                "startup source identifiers must be unique"
            );
        }
        assert!(
            !source_ids.is_empty(),
            "at least one source must be reported"
        );
    }
}

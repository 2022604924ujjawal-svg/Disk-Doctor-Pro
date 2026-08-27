use std::{env, fs, path::PathBuf};

use mangodisk_core::{configure_application_paths, ApplicationPaths, CleanupScanService};
use mangodisk_platform::{current_platform, Platform};

const TEST_VOLUME_ENV: &str = "MANGODISK_TEST_SELECTED_VOLUME";

struct MountedVolumeFixture {
    project_root: PathBuf,
}

impl MountedVolumeFixture {
    fn create(volume_root: PathBuf) -> Self {
        let project_root = volume_root.join(format!(
            "mangodisk-selected-volume-test-{}",
            std::process::id()
        ));
        let artifact_root = project_root.join("target/debug");
        fs::create_dir_all(&artifact_root).expect("create the mounted-volume fixture");
        fs::write(
            project_root.join("Cargo.toml"),
            "[package]\nname = \"mangodisk-selected-volume-test\"\nversion = \"0.1.0\"\n",
        )
        .expect("write the project marker");
        fs::write(artifact_root.join("fixture.bin"), vec![0x5a; 64 * 1024])
            .expect("write the build artifact fixture");
        Self { project_root }
    }
}

impl Drop for MountedVolumeFixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.project_root);
    }
}

#[test]
#[ignore = "requires a dedicated mounted volume supplied through MANGODISK_TEST_SELECTED_VOLUME"]
fn selected_volume_scan_preserves_standard_projects_and_adds_the_requested_volume() {
    let volume_root = env::var_os(TEST_VOLUME_ENV)
        .map(PathBuf::from)
        .expect("set MANGODISK_TEST_SELECTED_VOLUME to a dedicated mounted test volume");
    let live_volume = current_platform()
        .volumes()
        .expect("enumerate mounted volumes")
        .into_iter()
        .find(|volume| {
            current_platform().paths_equal(
                volume_root.as_path(),
                PathBuf::from(&volume.mount_point).as_path(),
            )
        })
        .expect("the supplied path must be a live volume root");
    let selected_volume_fixture =
        MountedVolumeFixture::create(PathBuf::from(&live_volume.mount_point));
    let user_directories = current_platform()
        .user_directories()
        .expect("read the current user directories");
    let standard_fixture =
        MountedVolumeFixture::create(user_directories.home_directory().to_path_buf());
    let storage_root = env::temp_dir().join(format!(
        "mangodisk-selected-volume-storage-{}",
        std::process::id()
    ));
    configure_application_paths(
        ApplicationPaths::new(
            storage_root.join("data"),
            storage_root.join("cache"),
            storage_root.join("runtime"),
        )
        .expect("create application storage paths"),
    )
    .expect("configure application storage paths");

    let result =
        CleanupScanService::scan_with_selected_volumes(vec![live_volume.mount_point], |_| {})
            .expect("scan the selected volume");
    let rust_artifacts = result
        .rules
        .iter()
        .find(|rule| rule.rule_id == "project.rust-build-artifacts")
        .expect("return the Rust build artifact rule");

    for fixture in [&standard_fixture, &selected_volume_fixture] {
        assert!(rust_artifacts.sources.iter().any(|source| {
            current_platform().paths_equal(
                PathBuf::from(&source.path).as_path(),
                fixture.project_root.join("target").as_path(),
            )
        }));
    }
    let _ = fs::remove_dir_all(storage_root);
}

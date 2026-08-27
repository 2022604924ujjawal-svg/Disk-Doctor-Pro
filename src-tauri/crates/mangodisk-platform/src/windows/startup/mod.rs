mod advanced;
mod metadata;
mod packaged;
mod registry;
mod services;
mod startup_folder;
mod tasks;

use crate::{
    PlatformCancellation, PlatformError, PlatformErrorCode, PlatformResult,
    PlatformStartupChangeRequest, PlatformStartupChangeResult, PlatformStartupControlCapability,
    PlatformStartupCoverageReason, PlatformStartupDesiredState, PlatformStartupSourceResult,
};

pub(super) fn scan(
    cancellation: &PlatformCancellation,
) -> PlatformResult<Vec<PlatformStartupSourceResult>> {
    if cancellation.is_cancelled() {
        return Ok(vec![PlatformStartupSourceResult::unavailable(
            "windows.startup",
            true,
            PlatformStartupCoverageReason::Cancelled,
        )]);
    }

    let registry_cancellation = cancellation.clone();
    let packaged_cancellation = cancellation.clone();
    let folder_cancellation = cancellation.clone();
    let service_cancellation = cancellation.clone();
    let task_cancellation = cancellation.clone();
    let advanced_cancellation = cancellation.clone();
    let results = std::thread::scope(|scope| {
        let registry = scope.spawn(|| registry::scan(&registry_cancellation));
        let packaged = scope.spawn(|| packaged::scan(&packaged_cancellation));
        let folders = scope.spawn(|| startup_folder::scan(&folder_cancellation));
        let services = scope.spawn(|| services::scan(&service_cancellation));
        let tasks = scope.spawn(|| tasks::scan(&task_cancellation));
        let advanced = scope.spawn(|| advanced::scan(&advanced_cancellation));
        let mut results = vec![
            joined_source(registry, "windows.registry.run", true),
            joined_source(packaged, "windows.packaged_startup_tasks", false),
        ];
        results.extend(folders.join().unwrap_or_else(|_| {
            vec![PlatformStartupSourceResult::unavailable(
                "windows.startup_folder",
                true,
                PlatformStartupCoverageReason::InvalidData,
            )]
        }));
        results.push(joined_source(services, "windows.services", true));
        results.push(joined_source(tasks, "windows.scheduled_tasks", true));
        results.push(joined_source(advanced, "windows.advanced_autoruns", false));
        results
    });
    Ok(results)
}

fn joined_source(
    handle: std::thread::ScopedJoinHandle<'_, PlatformStartupSourceResult>,
    source_id: &'static str,
    required: bool,
) -> PlatformStartupSourceResult {
    handle.join().unwrap_or_else(|_| {
        PlatformStartupSourceResult::unavailable(
            source_id,
            required,
            PlatformStartupCoverageReason::InvalidData,
        )
    })
}

pub(super) fn change(
    request: &PlatformStartupChangeRequest,
) -> PlatformResult<PlatformStartupChangeResult> {
    if requires_privileges(request) {
        return crate::startup_helper::change_with_privileges(request, None);
    }
    change_direct(request)
}

pub(super) fn change_many(
    requests: &[PlatformStartupChangeRequest],
) -> PlatformResult<Vec<PlatformResult<PlatformStartupChangeResult>>> {
    let privileged = requests
        .iter()
        .filter(|request| requires_privileges(request))
        .collect::<Vec<_>>();
    let mut privileged_results = if privileged.is_empty() {
        Vec::new()
    } else {
        crate::startup_helper::change_many_with_privileges(&privileged, None)?
    }
    .into_iter();

    Ok(requests
        .iter()
        .map(|request| {
            if requires_privileges(request) {
                privileged_results.next().unwrap_or_else(|| {
                    Err(PlatformError::new(
                        PlatformErrorCode::InvalidData,
                        "startup helper returned too few batch results",
                    ))
                })
            } else {
                change_direct(request)
            }
        })
        .collect())
}

fn requires_privileges(request: &PlatformStartupChangeRequest) -> bool {
    request.expected_artifact.control_capability
        == PlatformStartupControlCapability::ElevationRequired
        || (request.desired_state == PlatformStartupDesiredState::Removed
            && matches!(
                request.expected_artifact.scope,
                crate::PlatformStartupScope::AllUsers | crate::PlatformStartupScope::Machine
            ))
}

fn change_direct(
    request: &PlatformStartupChangeRequest,
) -> PlatformResult<PlatformStartupChangeResult> {
    match request.source_id.as_str() {
        "windows.registry.run" => registry::change(request),
        "windows.startup_folder.user" | "windows.startup_folder.common" => {
            startup_folder::change(request)
        }
        "windows.scheduled_tasks" => tasks::change(request),
        _ => Err(PlatformError::new(
            PlatformErrorCode::Unsupported,
            "startup source does not support configured-state changes",
        )),
    }
}

#[cfg(test)]
pub(super) fn helper_change(
    source_id: &str,
    provider_item_id: &str,
    expected_artifact_digest: &str,
    desired_state: PlatformStartupDesiredState,
) -> PlatformResult<PlatformStartupChangeResult> {
    let cancellation = PlatformCancellation::new(|| false);
    let results = scan(&cancellation)?;
    helper_change_from_snapshot(
        source_id,
        provider_item_id,
        expected_artifact_digest,
        desired_state,
        &results,
    )
}

pub(super) fn helper_change_many(
    requests: &[crate::startup_helper::StartupHelperChangeRequest],
) -> Vec<PlatformResult<PlatformStartupChangeResult>> {
    let cancellation = PlatformCancellation::new(|| false);
    let mut results = Vec::new();
    if requests
        .iter()
        .any(|request| request.source_id == "windows.registry.run")
    {
        results.push(registry::scan(&cancellation));
    }
    if requests
        .iter()
        .any(|request| request.source_id.starts_with("windows.startup_folder."))
    {
        results.extend(startup_folder::scan(&cancellation));
    }
    if requests
        .iter()
        .any(|request| request.source_id == "windows.scheduled_tasks")
    {
        results.push(tasks::scan(&cancellation));
    }
    requests
        .iter()
        .map(|request| {
            helper_change_from_snapshot(
                &request.source_id,
                &request.provider_item_id,
                &request.expected_artifact_digest,
                request.desired_state,
                &results,
            )
        })
        .collect()
}

fn helper_change_from_snapshot(
    source_id: &str,
    provider_item_id: &str,
    expected_artifact_digest: &str,
    desired_state: PlatformStartupDesiredState,
    results: &[PlatformStartupSourceResult],
) -> PlatformResult<PlatformStartupChangeResult> {
    if !helper_source_is_allowlisted(source_id) {
        return Err(PlatformError::new(
            PlatformErrorCode::Unsupported,
            "startup helper source is not allowlisted",
        ));
    }
    let artifact = results
        .iter()
        .find(|source| source.source_id == source_id)
        .and_then(|source| {
            source
                .items
                .iter()
                .find(|artifact| artifact.provider_item_id == provider_item_id)
        })
        .cloned()
        .ok_or_else(|| PlatformError::item_changed("startup helper target no longer exists"))?;
    let authorized = artifact.control_capability
        == PlatformStartupControlCapability::ElevationRequired
        || (desired_state == PlatformStartupDesiredState::Removed
            && artifact.control_capability == PlatformStartupControlCapability::RemoveOnly
            && matches!(artifact.scope, crate::PlatformStartupScope::Machine));
    if !authorized || crate::startup_helper::artifact_digest(&artifact) != expected_artifact_digest
    {
        return Err(PlatformError::item_changed(
            "startup helper target changed after preflight",
        ));
    }
    change_direct(&PlatformStartupChangeRequest {
        provider_item_id: provider_item_id.to_owned(),
        source_id: source_id.to_owned(),
        expected_artifact: artifact,
        desired_state,
    })
}

fn helper_source_is_allowlisted(source_id: &str) -> bool {
    matches!(
        source_id,
        "windows.registry.run"
            | "windows.startup_folder.user"
            | "windows.startup_folder.common"
            | "windows.scheduled_tasks"
    )
}

#[cfg(test)]
mod tests {
    use std::time::{SystemTime, UNIX_EPOCH};

    use winreg::{
        enums::{HKEY_LOCAL_MACHINE, KEY_READ, KEY_SET_VALUE, KEY_WOW64_64KEY},
        RegKey,
    };

    use super::*;

    const RUN_PATH: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
    const APPROVAL_PATH: &str =
        r"Software\Microsoft\Windows\CurrentVersion\Explorer\StartupApproved\Run";

    #[test]
    fn helper_allowlist_includes_scheduled_tasks_but_not_view_only_sources() {
        assert!(helper_source_is_allowlisted("windows.scheduled_tasks"));
        assert!(!helper_source_is_allowlisted("windows.services"));
        assert!(!helper_source_is_allowlisted("windows.advanced_autoruns"));
    }

    #[test]
    #[ignore = "requires an elevated Windows fixture"]
    fn actual_machine_registry_helper_revalidates_and_toggles() {
        let suffix = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system time must be available")
            .as_nanos();
        let value_name = format!("MangoDiskMachineStartupFixture{suffix}");
        let root = RegKey::predef(HKEY_LOCAL_MACHINE);
        let (run, _) = root
            .create_subkey_with_flags(RUN_PATH, KEY_READ | KEY_SET_VALUE | KEY_WOW64_64KEY)
            .expect("the machine Run fixture key must be writable");
        let (approval, _) = root
            .create_subkey_with_flags(APPROVAL_PATH, KEY_READ | KEY_SET_VALUE | KEY_WOW64_64KEY)
            .expect("the machine approval fixture key must be writable");
        let _cleanup = MachineRegistryFixture {
            value_name: value_name.clone(),
        };
        run.set_value(
            &value_name,
            &r#""C:\Windows\System32\notepad.exe" --mangodisk-machine-fixture"#,
        )
        .expect("the machine Run fixture must be created");
        let _ = approval.delete_value(&value_name);

        let enabled = machine_fixture(&value_name);
        assert_eq!(
            enabled.control_capability,
            PlatformStartupControlCapability::ElevationRequired
        );
        let disabled = helper_change(
            "windows.registry.run",
            &enabled.provider_item_id,
            &crate::startup_helper::artifact_digest(&enabled),
            PlatformStartupDesiredState::Disabled,
        )
        .expect("the elevated helper boundary must disable the fixture");
        assert!(disabled.verified);

        let disabled_artifact = machine_fixture(&value_name);
        let restored = helper_change(
            "windows.registry.run",
            &disabled_artifact.provider_item_id,
            &crate::startup_helper::artifact_digest(&disabled_artifact),
            PlatformStartupDesiredState::Enabled,
        )
        .expect("the elevated helper boundary must restore the fixture");
        assert!(restored.verified);
        assert!(approval.get_raw_value(&value_name).is_err());
    }

    fn machine_fixture(value_name: &str) -> crate::PlatformStartupArtifact {
        let cancellation = PlatformCancellation::new(|| false);
        scan(&cancellation)
            .expect("the startup scan must complete")
            .into_iter()
            .find(|source| source.source_id == "windows.registry.run")
            .and_then(|source| {
                source
                    .items
                    .into_iter()
                    .find(|artifact| artifact.display_name == value_name)
            })
            .expect("the machine fixture must be discoverable")
    }

    struct MachineRegistryFixture {
        value_name: String,
    }

    impl Drop for MachineRegistryFixture {
        fn drop(&mut self) {
            let root = RegKey::predef(HKEY_LOCAL_MACHINE);
            if let Ok(run) = root.open_subkey_with_flags(RUN_PATH, KEY_SET_VALUE | KEY_WOW64_64KEY)
            {
                let _ = run.delete_value(&self.value_name);
            }
            if let Ok(approval) =
                root.open_subkey_with_flags(APPROVAL_PATH, KEY_SET_VALUE | KEY_WOW64_64KEY)
            {
                let _ = approval.delete_value(&self.value_name);
            }
        }
    }
}

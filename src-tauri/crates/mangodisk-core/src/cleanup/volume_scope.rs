use std::path::Path;

use mangodisk_platform::{current_platform, Platform, ScanDeviceClass, VolumeInfo};

use crate::shared::{CoreError, CoreResult};

const MAX_SELECTED_VOLUMES: usize = 32;

#[derive(Clone, Copy)]
pub(crate) enum SelectedVolumeScopeOperation {
    Scan,
    Cleanup,
}

impl SelectedVolumeScopeOperation {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Scan => "scan",
            Self::Cleanup => "cleanup",
        }
    }
}

pub(crate) fn resolve_selected_volume_roots(
    requested_mount_points: &[String],
    operation: SelectedVolumeScopeOperation,
) -> CoreResult<Vec<String>> {
    let requested_count = requested_mount_points.len();
    if requested_count == 0 || requested_count > MAX_SELECTED_VOLUMES {
        log::warn!(
            "cleanup_selected_volume_scope_rejected operation={} reason=invalidCount requested_count={requested_count} maximum_count={MAX_SELECTED_VOLUMES}",
            operation.as_str()
        );
        return Err(CoreError::invalid_input(
            "the cleanup scan must select a supported number of volumes",
        ));
    }

    let volumes = current_platform().volumes().map_err(|error| {
        log::warn!(
            "cleanup_selected_volume_inventory_failed operation={} requested_count={} error_digest={}",
            operation.as_str(),
            requested_count,
            blake3::hash(error.as_bytes()).to_hex()
        );
        CoreError::from(error)
    })?;
    let selected = match_live_volume_roots(requested_mount_points, &volumes, operation)?;
    let selected_volumes = selected
        .iter()
        .filter_map(|mount_point| {
            volumes.iter().find(|volume| {
                current_platform()
                    .paths_equal(Path::new(mount_point), Path::new(&volume.mount_point))
            })
        })
        .collect::<Vec<_>>();
    let device_count = |class| {
        selected_volumes
            .iter()
            .filter(|volume| volume.scan_concurrency.class == class)
            .count()
    };
    log::info!(
        "cleanup_selected_volume_scope_resolved operation={} requested_count={} selected_count={} solid_state_count={} rotational_count={} removable_count={} network_count={} unknown_count={}",
        operation.as_str(),
        requested_count,
        selected.len(),
        device_count(ScanDeviceClass::SolidState),
        device_count(ScanDeviceClass::Rotational),
        device_count(ScanDeviceClass::Removable),
        device_count(ScanDeviceClass::Network),
        device_count(ScanDeviceClass::Unknown)
    );
    Ok(selected)
}

fn match_live_volume_roots(
    requested_mount_points: &[String],
    volumes: &[VolumeInfo],
    operation: SelectedVolumeScopeOperation,
) -> CoreResult<Vec<String>> {
    let mut selected = Vec::<String>::new();
    for requested in requested_mount_points {
        let requested = requested.trim();
        if requested.is_empty() {
            log::warn!(
                "cleanup_selected_volume_scope_rejected operation={} reason=emptyMountPoint requested_count={} matched_count={}",
                operation.as_str(),
                requested_mount_points.len(),
                selected.len()
            );
            return Err(CoreError::invalid_input(
                "the cleanup scan contains an empty volume mount point",
            ));
        }
        let Some(volume) = volumes.iter().find(|volume| {
            current_platform().paths_equal(Path::new(requested), Path::new(&volume.mount_point))
        }) else {
            log::warn!(
                "cleanup_selected_volume_scope_rejected operation={} reason=unavailable requested_count={} matched_count={}",
                operation.as_str(),
                requested_mount_points.len(),
                selected.len()
            );
            return Err(CoreError::invalid_input(
                "a selected cleanup volume is no longer available",
            ));
        };
        if !selected.iter().any(|mount_point| {
            current_platform().paths_equal(Path::new(mount_point), Path::new(&volume.mount_point))
        }) {
            selected.push(volume.mount_point.clone());
        }
    }
    Ok(selected)
}

#[cfg(test)]
mod tests {
    use mangodisk_platform::{ScanConcurrency, VolumeInfo};

    use crate::shared::CoreErrorCode;

    use super::{
        match_live_volume_roots, resolve_selected_volume_roots, SelectedVolumeScopeOperation,
    };

    fn volume(mount_point: &str) -> VolumeInfo {
        VolumeInfo {
            name: mount_point.to_string(),
            mount_point: mount_point.to_string(),
            total_bytes: 1_000,
            available_bytes: 500,
            used_bytes: 500,
            scan_concurrency: ScanConcurrency::solid_state(),
        }
    }

    #[test]
    fn live_volume_roots_use_inventory_values_and_deduplicate() {
        let mount_point = if cfg!(windows) { "C:\\" } else { "/" };
        let roots = match_live_volume_roots(
            &[mount_point.to_string(), mount_point.to_string()],
            &[volume(mount_point)],
            SelectedVolumeScopeOperation::Scan,
        )
        .expect("resolve selected volume roots");

        assert_eq!(roots, vec![mount_point.to_string()]);
    }

    #[test]
    fn live_volume_roots_reject_unavailable_mounts() {
        let missing = if cfg!(windows) {
            "Z:\\"
        } else {
            "/Volumes/Missing"
        };
        let available = if cfg!(windows) { "C:\\" } else { "/" };

        assert!(match_live_volume_roots(
            &[missing.to_string()],
            &[volume(available)],
            SelectedVolumeScopeOperation::Scan,
        )
        .is_err());
    }

    #[test]
    fn selected_volume_scope_rejects_an_empty_selection_before_inventory_access() {
        let error = resolve_selected_volume_roots(&[], SelectedVolumeScopeOperation::Scan)
            .expect_err("reject an empty volume scope");

        assert_eq!(error.code(), CoreErrorCode::InvalidInput);
    }
}

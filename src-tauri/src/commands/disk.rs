use mangodisk_core::DiskInfo;
use mangodisk_platform::{current_platform, Platform};

use super::error::{run_blocking, CommandResult};

#[tauri::command]
pub async fn get_system_disk() -> CommandResult<DiskInfo> {
    run_blocking("get_system_disk", || {
        current_platform().system_volume().map(DiskInfo::from)
    })
    .await
}

#[tauri::command]
pub async fn list_disks() -> CommandResult<Vec<DiskInfo>> {
    run_blocking("list_disks", || {
        current_platform()
            .volumes()
            .map(|volumes| volumes.into_iter().map(DiskInfo::from).collect())
    })
    .await
}

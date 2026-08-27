use std::{
    env,
    process::Command,
    time::{SystemTime, UNIX_EPOCH},
};

use serde::Serialize;

use crate::filesystem::DiskInfo;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BenchmarkSourceInfo {
    pub application_version: String,
    pub source_commit: String,
    pub source_dirty_at_build: bool,
    pub build_profile: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct BenchmarkEnvironment {
    pub(crate) environment_id: String,
    pub(crate) user_identity: String,
    pub(crate) os: String,
    pub(crate) architecture: String,
    pub(crate) os_version: String,
    pub(crate) cpu_model: String,
    pub(crate) logical_cpu_count: usize,
    pub(crate) physical_memory_bytes: Option<u64>,
}

pub(crate) fn environment_info(
    environment_id: Option<&str>,
    disk: &DiskInfo,
) -> BenchmarkEnvironment {
    let os = env::consts::OS.to_string();
    let architecture = env::consts::ARCH.to_string();
    let cpu_model = cpu_model();
    let logical_cpu_count = std::thread::available_parallelism()
        .map(|value| value.get())
        .unwrap_or(1);
    let physical_memory_bytes = physical_memory_bytes();
    let user_identity = env::var("USER")
        .or_else(|_| env::var("USERNAME"))
        .unwrap_or_else(|_| "unknown".to_string());
    let environment_id = environment_id.map(str::to_string).unwrap_or_else(|| {
        automatic_environment_id(
            &os,
            &architecture,
            &cpu_model,
            logical_cpu_count,
            physical_memory_bytes,
            &user_identity,
            disk,
        )
    });
    BenchmarkEnvironment {
        environment_id,
        user_identity,
        os,
        architecture,
        os_version: os_version(),
        cpu_model,
        logical_cpu_count,
        physical_memory_bytes,
    }
}

/// Callers may provide a stable environment ID. Otherwise, an opaque digest incorporates the
/// hardware, execution user, and target disk identity without embedding their raw values in the
/// ID. Fixed benchmark machines should still use a maintained explicit ID so hardware changes do
/// not silently create a different machine identity.
fn automatic_environment_id(
    os: &str,
    architecture: &str,
    cpu_model: &str,
    logical_cpu_count: usize,
    physical_memory_bytes: Option<u64>,
    user_identity: &str,
    disk: &DiskInfo,
) -> String {
    let mut hasher = blake3::Hasher::new();
    for value in [
        os,
        architecture,
        cpu_model,
        &logical_cpu_count.to_string(),
        &physical_memory_bytes.unwrap_or_default().to_string(),
        user_identity,
        &disk.mount_point,
        &disk.total_bytes.to_string(),
    ] {
        hasher.update(value.as_bytes());
        hasher.update(&[0]);
    }
    format!("auto-{}", &hasher.finalize().to_hex()[..16])
}

#[cfg(target_os = "macos")]
fn os_version() -> String {
    command_output("/usr/bin/sw_vers", &[])
        .map(|value| value.replace('\n', "; "))
        .unwrap_or_else(|| "unknown".to_string())
}

#[cfg(windows)]
fn os_version() -> String {
    // `cmd /C ver` uses the active Windows OEM code page, which is often not UTF-8 on localized
    // systems and would make strict decoding fall back to `unknown`. PowerShell returns only the
    // numeric version, independent of the display language and the Windows 10/11 compatibility
    // product-name field.
    command_output(
        "powershell.exe",
        &[
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            "[System.Environment]::OSVersion.Version.ToString()",
        ],
    )
    .unwrap_or_else(|| "unknown".to_string())
}

#[cfg(not(any(target_os = "macos", windows)))]
fn os_version() -> String {
    command_output("uname", &["-a"]).unwrap_or_else(|| "unknown".to_string())
}

#[cfg(target_os = "macos")]
fn cpu_model() -> String {
    command_output("/usr/sbin/sysctl", &["-n", "machdep.cpu.brand_string"])
        .filter(|value| !value.is_empty())
        .or_else(|| command_output("/usr/sbin/sysctl", &["-n", "hw.model"]))
        .unwrap_or_else(|| "unknown".to_string())
}

#[cfg(windows)]
fn cpu_model() -> String {
    env::var("PROCESSOR_IDENTIFIER").unwrap_or_else(|_| "unknown".to_string())
}

#[cfg(not(any(target_os = "macos", windows)))]
fn cpu_model() -> String {
    "unknown".to_string()
}

#[cfg(target_os = "macos")]
fn physical_memory_bytes() -> Option<u64> {
    command_output("/usr/sbin/sysctl", &["-n", "hw.memsize"])?
        .parse()
        .ok()
}

#[cfg(windows)]
fn physical_memory_bytes() -> Option<u64> {
    command_output(
        "powershell.exe",
        &[
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            "(Get-CimInstance Win32_ComputerSystem).TotalPhysicalMemory",
        ],
    )?
    .parse()
    .ok()
}

#[cfg(not(any(target_os = "macos", windows)))]
fn physical_memory_bytes() -> Option<u64> {
    None
}

#[cfg(target_os = "macos")]
pub(crate) fn local_timestamp(fallback_ms: u64) -> String {
    command_output("/bin/date", &["+%Y-%m-%dT%H:%M:%S%z"])
        .unwrap_or_else(|| fallback_ms.to_string())
}

#[cfg(windows)]
pub(crate) fn local_timestamp(fallback_ms: u64) -> String {
    command_output(
        "powershell.exe",
        &[
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            "Get-Date -Format o",
        ],
    )
    .unwrap_or_else(|| fallback_ms.to_string())
}

#[cfg(not(any(target_os = "macos", windows)))]
pub(crate) fn local_timestamp(fallback_ms: u64) -> String {
    fallback_ms.to_string()
}

pub(crate) fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_millis() as u64)
}

fn command_output(program: &str, arguments: &[&str]) -> Option<String> {
    let output = Command::new(program).args(arguments).output().ok()?;
    if !output.status.success() {
        return None;
    }
    String::from_utf8(output.stdout)
        .ok()
        .map(|value| value.trim().to_string())
}

use std::{
    process::{Command, ExitStatus, Stdio},
    time::Instant,
};

use windows_sys::Win32::Foundation::ERROR_GEN_FAILURE;

use crate::{
    configure_background_process, ApplicationUninstallExecutionOutcome,
    ApplicationUninstallPlatformError, ApplicationUninstallRegistrationState,
};

use super::{exit_code_or_fallback, system_powershell_path};

const PACKAGE_ENV: &str = "MANGODISK_APPX_PACKAGE_FULL_NAME";
const PACKAGE_ABSENT_EXIT_CODE: i32 = 3;
const STATE_SCRIPT: &str = r#"
$ProgressPreference = 'SilentlyContinue'
$target = $env:MANGODISK_APPX_PACKAGE_FULL_NAME
$package = Get-AppxPackage -ErrorAction Stop |
  Where-Object { $_.PackageFullName -ceq $target } |
  Select-Object -First 1
if ($null -eq $package) { exit 3 }
exit 0
"#;
const REMOVE_SCRIPT: &str = r#"
$ProgressPreference = 'SilentlyContinue'
$target = $env:MANGODISK_APPX_PACKAGE_FULL_NAME
$package = Get-AppxPackage -ErrorAction Stop |
  Where-Object { $_.PackageFullName -ceq $target } |
  Select-Object -First 1
if ($null -eq $package) { exit 3 }
Remove-AppxPackage -Package $package.PackageFullName -ErrorAction Stop
"#;

pub(super) fn package_state(
    package_full_name: &str,
) -> Result<ApplicationUninstallRegistrationState, ApplicationUninstallPlatformError> {
    let status = run_script(STATE_SCRIPT, package_full_name)?;
    match status.code() {
        Some(0) => Ok(ApplicationUninstallRegistrationState::Installed),
        Some(PACKAGE_ABSENT_EXIT_CODE) => Ok(ApplicationUninstallRegistrationState::Absent),
        code => Err(ApplicationUninstallPlatformError::NativeFailure(
            exit_code_or_fallback(code),
        )),
    }
}

pub(super) fn execute(
    package_full_name: &str,
) -> Result<ApplicationUninstallExecutionOutcome, ApplicationUninstallPlatformError> {
    let started = Instant::now();
    log::info!("application_uninstall_appx_requested");
    let result = execute_inner(package_full_name);
    match result {
        Ok(outcome) => {
            log::info!(
                "application_uninstall_appx_finished outcome={} elapsed_ms={}",
                match outcome {
                    ApplicationUninstallExecutionOutcome::Completed => "completed",
                    ApplicationUninstallExecutionOutcome::RestartRequired => "restart_required",
                },
                started.elapsed().as_millis()
            );
            Ok(outcome)
        }
        Err(error) => {
            log::warn!(
                "application_uninstall_appx_failed platform_error={} native_code={} elapsed_ms={}",
                error.stable_code(),
                error
                    .native_code()
                    .map_or_else(|| "none".to_string(), |code| code.to_string()),
                started.elapsed().as_millis()
            );
            Err(error)
        }
    }
}

fn execute_inner(
    package_full_name: &str,
) -> Result<ApplicationUninstallExecutionOutcome, ApplicationUninstallPlatformError> {
    if package_state(package_full_name)? != ApplicationUninstallRegistrationState::Installed {
        return Err(ApplicationUninstallPlatformError::RegistrationChanged);
    }
    let status = run_script(REMOVE_SCRIPT, package_full_name)?;
    if !status.success() {
        return Err(ApplicationUninstallPlatformError::NativeFailure(
            exit_code_or_fallback(status.code()),
        ));
    }
    if package_state(package_full_name)? != ApplicationUninstallRegistrationState::Absent {
        return Err(ApplicationUninstallPlatformError::RegistrationChanged);
    }
    Ok(ApplicationUninstallExecutionOutcome::Completed)
}

fn run_script(
    script: &str,
    package_full_name: &str,
) -> Result<ExitStatus, ApplicationUninstallPlatformError> {
    let mut command = Command::new(system_powershell_path()?);
    configure_background_process(&mut command);
    command
        .args(["-NoProfile", "-NonInteractive", "-Command", script])
        .env(PACKAGE_ENV, package_full_name)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map_err(|error| {
            ApplicationUninstallPlatformError::NativeFailure(
                error
                    .raw_os_error()
                    .and_then(|code| u32::try_from(code).ok())
                    .unwrap_or(ERROR_GEN_FAILURE),
            )
        })
}

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde::Deserialize;

use crate::{
    run_controlled_command, ControlledCommandLimits, ControlledEnvironmentPolicy,
    ControlledExecutable, PlatformCancellation, PlatformStartupArtifact,
    PlatformStartupConfiguredState, PlatformStartupControlCapability,
    PlatformStartupCoverageReason, PlatformStartupCoverageStatus, PlatformStartupDiagnosticCode,
    PlatformStartupIdentityConfidence, PlatformStartupOwner, PlatformStartupRuntimeState,
    PlatformStartupScope, PlatformStartupSourceKind, PlatformStartupSourceResult,
    PlatformStartupSummarySource, PlatformStartupTarget, PlatformStartupTargetKind,
    PlatformStartupTrigger, PlatformStartupTrustState,
};

use super::registry::modified_at_ms;

const COMMAND_TIMEOUT: Duration = Duration::from_secs(30);
const OUTPUT_LIMIT: usize = 8 * 1024 * 1024;
const STARTUP_TASK_SCRIPT: &str = r#"
[Console]::OutputEncoding = [System.Text.UTF8Encoding]::new($false)
$ProgressPreference = 'SilentlyContinue'
$items = @(
  Get-AppxPackage -ErrorAction SilentlyContinue |
    Where-Object { -not $_.IsFramework -and -not $_.IsResourcePackage -and -not $_.IsPartiallyStaged } |
    ForEach-Object {
      $package = $_
      $manifest = Get-AppxPackageManifest -Package $package.PackageFullName -ErrorAction SilentlyContinue
      if ($null -eq $manifest) { return }
      foreach ($application in @($manifest.Package.Applications.Application)) {
        foreach ($extension in @($application.Extensions.Extension)) {
          if ([string]$extension.Category -ne 'windows.startupTask') { continue }
          $startup = $extension.StartupTask
          if ($null -eq $startup) { continue }
          $displayName = [string]$startup.DisplayName
          if (-not $displayName -or $displayName.StartsWith('ms-resource:')) { $displayName = [string]$package.Name }
          $description = [string]$manifest.Package.Properties.Description
          if ($description.StartsWith('ms-resource:')) { $description = '' }
          $publisher = [string]$manifest.Package.Properties.PublisherDisplayName
          if (-not $publisher -or $publisher.StartsWith('ms-resource:')) { $publisher = [string]$package.Publisher }
          [pscustomobject]@{
            packageFamilyName = [string]$package.PackageFamilyName
            packageName = [string]$package.Name
            version = [string]$package.Version
            publisher = $publisher
            description = $description
            installLocation = [string]$package.InstallLocation
            applicationId = [string]$application.Id
            taskId = [string]$startup.TaskId
            displayName = $displayName
            executable = [string]$extension.Executable
          }
        }
      }
    }
)
ConvertTo-Json -InputObject $items -Compress
"#;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct StartupTaskRecord {
    package_family_name: String,
    package_name: String,
    version: String,
    publisher: String,
    description: String,
    install_location: String,
    application_id: String,
    task_id: String,
    display_name: String,
    executable: String,
}

pub(super) fn scan(cancellation: &PlatformCancellation) -> PlatformStartupSourceResult {
    let started = Instant::now();
    if cancellation.is_cancelled() {
        return result(
            Vec::new(),
            PlatformStartupCoverageStatus::Cancelled,
            Some(PlatformStartupCoverageReason::Cancelled),
            started,
        );
    }
    let json = match powershell_json(cancellation) {
        Ok(json) => json,
        Err(reason) => {
            return result(
                Vec::new(),
                PlatformStartupCoverageStatus::Unavailable,
                Some(reason),
                started,
            );
        }
    };
    let records: Vec<StartupTaskRecord> = match serde_json::from_str(&json) {
        Ok(records) => records,
        Err(_) => {
            return result(
                Vec::new(),
                PlatformStartupCoverageStatus::Failed,
                Some(PlatformStartupCoverageReason::InvalidData),
                started,
            );
        }
    };
    let items: Vec<_> = records.into_iter().filter_map(record_artifact).collect();
    let state_unavailable = !items.is_empty();
    result(
        items,
        if state_unavailable {
            PlatformStartupCoverageStatus::Partial
        } else {
            PlatformStartupCoverageStatus::Complete
        },
        state_unavailable.then_some(PlatformStartupCoverageReason::StateUnavailable),
        started,
    )
}

fn powershell_json(
    cancellation: &PlatformCancellation,
) -> Result<String, PlatformStartupCoverageReason> {
    let powershell = super::super::native_uninstall::system_powershell_path()
        .map_err(|_| PlatformStartupCoverageReason::ApiUnavailable)?;
    let executable = ControlledExecutable::capture(&powershell)
        .map_err(|_| PlatformStartupCoverageReason::ApiUnavailable)?;
    let output = run_controlled_command(
        "windows-startup-task-inventory",
        &executable,
        &[
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            STARTUP_TASK_SCRIPT,
        ],
        ControlledEnvironmentPolicy::Inherit,
        ControlledCommandLimits {
            timeout: COMMAND_TIMEOUT,
            stdout_bytes: OUTPUT_LIMIT,
            stderr_bytes: 256 * 1024,
        },
        &|| cancellation.is_cancelled(),
    )
    .map_err(|_| {
        if cancellation.is_cancelled() {
            PlatformStartupCoverageReason::Cancelled
        } else {
            PlatformStartupCoverageReason::ApiUnavailable
        }
    })?;
    if !output.status.success() {
        return Err(PlatformStartupCoverageReason::ApiUnavailable);
    }
    String::from_utf8(output.stdout)
        .map(|value| value.trim().to_string())
        .map_err(|_| PlatformStartupCoverageReason::InvalidData)
}

fn record_artifact(record: StartupTaskRecord) -> Option<PlatformStartupArtifact> {
    if record.package_family_name.trim().is_empty() || record.task_id.trim().is_empty() {
        return None;
    }
    let executable_path = (!record.executable.trim().is_empty()).then(|| {
        PathBuf::from(&record.install_location)
            .join(record.executable.trim_start_matches(['\\', '/']))
    });
    let mut diagnostics = vec![PlatformStartupDiagnosticCode::StateUnavailable];
    if executable_path
        .as_deref()
        .is_some_and(|path| !path.exists())
    {
        diagnostics.push(PlatformStartupDiagnosticCode::MissingTarget);
    }
    let package_identity = format!(
        "package:{}",
        record.package_family_name.to_ascii_lowercase()
    );
    let display_name = if record.display_name.trim().is_empty() {
        record.package_name.clone()
    } else {
        record.display_name.clone()
    };
    Some(PlatformStartupArtifact {
        provider_item_id: format!(
            "packaged-task:{}:{}",
            record.package_family_name.to_ascii_lowercase(),
            format!("{}:{}", record.application_id, record.task_id).to_ascii_lowercase()
        ),
        source_kind: PlatformStartupSourceKind::PackagedStartupTask,
        scope: PlatformStartupScope::CurrentUser,
        triggers: vec![PlatformStartupTrigger::UserLogon],
        display_name: display_name.clone(),
        configuration_path: None,
        target: PlatformStartupTarget {
            kind: PlatformStartupTargetKind::Application,
            identity_key: package_identity.clone(),
            path: executable_path.clone(),
            executable_name: executable_path
                .as_deref()
                .and_then(Path::file_name)
                .and_then(|name| name.to_str())
                .map(ToOwned::to_owned),
            arguments: Vec::new(),
        },
        owner: PlatformStartupOwner {
            identity_key: Some(package_identity),
            name: Some(display_name),
            publisher: nonempty(record.publisher),
            summary: nonempty(record.description),
            summary_source: PlatformStartupSummarySource::PackageManifest,
            version: nonempty(record.version),
            icon_path: executable_path.clone().filter(|path| path.exists()),
            confidence: PlatformStartupIdentityConfidence::Exact,
        },
        // Windows intentionally restricts third-party re-enabling after a user disables a packaged
        // startup task. Until a supported cross-package state API exists, preserve the declaration
        // and expose the state limitation instead of inferring enabled from manifest presence.
        configured_state: PlatformStartupConfiguredState::Unknown,
        runtime_state: PlatformStartupRuntimeState::Unknown,
        control_capability: PlatformStartupControlCapability::SystemManaged,
        trust: PlatformStartupTrustState::Unknown,
        modified_at_ms: executable_path.as_deref().and_then(modified_at_ms),
        diagnostics,
    })
}

fn nonempty(value: String) -> Option<String> {
    (!value.trim().is_empty()).then_some(value)
}

fn result(
    items: Vec<PlatformStartupArtifact>,
    status: PlatformStartupCoverageStatus,
    reason: Option<PlatformStartupCoverageReason>,
    started: Instant,
) -> PlatformStartupSourceResult {
    PlatformStartupSourceResult {
        source_id: "windows.packaged_startup_tasks".to_string(),
        required: true,
        status,
        reason,
        items,
        elapsed_ms: started.elapsed().as_millis() as u64,
    }
}

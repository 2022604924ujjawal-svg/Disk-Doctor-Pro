use std::{
    collections::BTreeMap,
    sync::atomic::Ordering,
    time::{Duration, Instant},
};

use mangodisk_platform::{
    run_controlled_command, ControlledCommandError, ControlledCommandLimits,
    ControlledCommandOutput, ControlledEnvironmentPolicy, ControlledExecutable,
};
use serde::Deserialize;

use crate::{
    cleanup::{
        cleaners::{CleanerPreview, CleanerPreviewStatus, CleanupCleaner},
        CleanupActionKind, CleanupActionReason, CleanupActionResult, CleanupActionStatus,
    },
    shared::operation::OperationGuard,
};

const CLEANER_ID: &str = "special.conda-cache";
const CLEANER_REVISION: &str = "conda-cache-v1-safe-metadata-tarballs-logs";
const EXECUTABLE_ALIASES: &[&str] = &["conda", "conda.exe"];
const PREVIEW_ARGS: &[&str] = &[
    "clean",
    "--index-cache",
    "--tarballs",
    "--logfiles",
    "--dry-run",
    "--json",
];
const CLEAN_ARGS: &[&str] = &[
    "clean",
    "--index-cache",
    "--tarballs",
    "--logfiles",
    "--yes",
    "--json",
];
const PREVIEW_COMMAND_ID: &str = "conda.clean-preview";
const CLEAN_COMMAND_ID: &str = "conda.clean";
const PREVIEW_TIMEOUT: Duration = Duration::from_secs(30);
const CLEAN_TIMEOUT: Duration = Duration::from_secs(5 * 60);
const JSON_OUTPUT_LIMIT: usize = 4 * 1024 * 1024;
const STDERR_OUTPUT_LIMIT: usize = 256 * 1024;

pub(super) struct CondaCacheCleaner;

#[derive(Debug, Deserialize)]
struct CondaCleanReport {
    success: bool,
    #[serde(default)]
    index_cache: CondaPathList,
    #[serde(default)]
    logfiles: Vec<String>,
    #[serde(default)]
    tarballs: CondaPackageCache,
}

#[derive(Debug, Default, Deserialize)]
struct CondaPathList {
    #[serde(default)]
    files: Vec<String>,
}

#[derive(Debug, Default, Deserialize)]
struct CondaPackageCache {
    #[serde(default)]
    pkg_sizes: BTreeMap<String, BTreeMap<String, u64>>,
    #[serde(default)]
    total_size: u64,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct CondaCacheUsage {
    bytes: u64,
    item_count: u64,
}

impl CleanupCleaner for CondaCacheCleaner {
    fn id(&self) -> &'static str {
        CLEANER_ID
    }

    fn revision(&self) -> &'static str {
        CLEANER_REVISION
    }

    fn category(&self) -> crate::cleanup::CleanupCategory {
        crate::cleanup::CleanupCategory::Development
    }

    fn executable_aliases(&self) -> &'static [&'static str] {
        EXECUTABLE_ALIASES
    }

    fn preview(
        &self,
        executable: &ControlledExecutable,
        is_cancelled: &(dyn Fn() -> bool + Sync),
    ) -> CleanerPreview {
        let started = Instant::now();
        let result = run_conda(
            PREVIEW_COMMAND_ID,
            executable,
            PREVIEW_ARGS,
            PREVIEW_TIMEOUT,
            is_cancelled,
        )
        .and_then(parse_preview_output);
        let elapsed_ms = started.elapsed().as_millis() as u64;
        match result {
            Ok(usage) => CleanerPreview {
                status: CleanerPreviewStatus::Ready,
                bytes: usage.bytes,
                item_count: usage.item_count,
                elapsed_ms,
            },
            Err(error) => {
                log_conda_failure("preview", error);
                CleanerPreview {
                    status: CleanerPreviewStatus::Limited,
                    bytes: 0,
                    item_count: 0,
                    elapsed_ms,
                }
            }
        }
    }

    fn execute(
        &self,
        executable: &ControlledExecutable,
        dry_run: bool,
        operation: &OperationGuard,
    ) -> CleanupActionResult {
        let before = match preview_usage(executable, operation) {
            Ok(usage) => usage,
            Err(error) => {
                log_conda_failure("execute_preflight", error);
                return failed_action(0, CleanupActionReason::PreflightFailed);
            }
        };
        if dry_run {
            return completed_action(CleanupActionStatus::Previewed, before, before);
        }
        if before.bytes == 0 && before.item_count == 0 {
            return completed_action(CleanupActionStatus::Completed, before, before);
        }

        let cleaned = run_conda(
            CLEAN_COMMAND_ID,
            executable,
            CLEAN_ARGS,
            CLEAN_TIMEOUT,
            &|| operation.cancelled().load(Ordering::Relaxed),
        )
        .and_then(parse_execution_output);
        match cleaned {
            Ok(()) => {}
            Err(error) => {
                log_conda_failure("clean", error);
                return failed_action(before.bytes, CleanupActionReason::ExecutionFailed);
            }
        }

        let after = match preview_usage(executable, operation) {
            Ok(usage) => usage,
            Err(error) => {
                log_conda_failure("execute_verify", error);
                return CleanupActionResult {
                    rule_id: CLEANER_ID.to_string(),
                    action_kind: CleanupActionKind::Command,
                    status: CleanupActionStatus::Partial,
                    reason_code: Some(CleanupActionReason::VerificationFailed),
                    bytes_expected: before.bytes,
                    released_bytes: 0,
                    affected_item_count: 0,
                    failed_item_count: 1,
                    running_processes: Vec::new(),
                };
            }
        };
        log::info!(
            "conda_cache_clean_completed bytes_before={} bytes_after={} items_before={} items_after={}",
            before.bytes,
            after.bytes,
            before.item_count,
            after.item_count
        );
        verified_action(before, after)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CondaCleanerError {
    Cancelled,
    ExecutableChanged,
    CommandUnavailable,
    NonZeroExit,
    InvalidOutput,
}

fn preview_usage(
    executable: &ControlledExecutable,
    operation: &OperationGuard,
) -> Result<CondaCacheUsage, CondaCleanerError> {
    run_conda(
        PREVIEW_COMMAND_ID,
        executable,
        PREVIEW_ARGS,
        PREVIEW_TIMEOUT,
        &|| operation.cancelled().load(Ordering::Relaxed),
    )
    .and_then(parse_preview_output)
}

fn run_conda(
    command_id: &'static str,
    executable: &ControlledExecutable,
    args: &[&str],
    timeout: Duration,
    is_cancelled: &(dyn Fn() -> bool + Sync),
) -> Result<ControlledCommandOutput, CondaCleanerError> {
    run_controlled_command(
        command_id,
        executable,
        args,
        ControlledEnvironmentPolicy::Isolated,
        ControlledCommandLimits {
            timeout,
            stdout_bytes: JSON_OUTPUT_LIMIT,
            stderr_bytes: STDERR_OUTPUT_LIMIT,
        },
        is_cancelled,
    )
    .map_err(|error| match error {
        ControlledCommandError::Cancelled => CondaCleanerError::Cancelled,
        ControlledCommandError::ExecutableChanged => CondaCleanerError::ExecutableChanged,
        _ => CondaCleanerError::CommandUnavailable,
    })
}

fn parse_preview_output(
    output: ControlledCommandOutput,
) -> Result<CondaCacheUsage, CondaCleanerError> {
    if !output.status.success() {
        return Err(CondaCleanerError::NonZeroExit);
    }
    let report = parse_report(&output.stdout)?;
    let calculated_bytes = report
        .tarballs
        .pkg_sizes
        .values()
        .flat_map(|packages| packages.values())
        .try_fold(0_u64, |total, bytes| total.checked_add(*bytes))
        .ok_or(CondaCleanerError::InvalidOutput)?;
    if calculated_bytes != report.tarballs.total_size {
        return Err(CondaCleanerError::InvalidOutput);
    }
    let tarball_count = report
        .tarballs
        .pkg_sizes
        .values()
        .try_fold(0_usize, |total, packages| total.checked_add(packages.len()))
        .and_then(|count| u64::try_from(count).ok())
        .ok_or(CondaCleanerError::InvalidOutput)?;
    let other_count = report
        .index_cache
        .files
        .len()
        .checked_add(report.logfiles.len())
        .and_then(|count| u64::try_from(count).ok())
        .ok_or(CondaCleanerError::InvalidOutput)?;
    Ok(CondaCacheUsage {
        bytes: report.tarballs.total_size,
        item_count: tarball_count
            .checked_add(other_count)
            .ok_or(CondaCleanerError::InvalidOutput)?,
    })
}

fn parse_execution_output(output: ControlledCommandOutput) -> Result<(), CondaCleanerError> {
    if !output.status.success() {
        return Err(CondaCleanerError::NonZeroExit);
    }
    parse_report(&output.stdout).map(|_| ())
}

fn parse_report(bytes: &[u8]) -> Result<CondaCleanReport, CondaCleanerError> {
    let report = serde_json::from_slice::<CondaCleanReport>(bytes)
        .map_err(|_| CondaCleanerError::InvalidOutput)?;
    if !report.success {
        return Err(CondaCleanerError::InvalidOutput);
    }
    Ok(report)
}

fn completed_action(
    status: CleanupActionStatus,
    before: CondaCacheUsage,
    after: CondaCacheUsage,
) -> CleanupActionResult {
    CleanupActionResult {
        rule_id: CLEANER_ID.to_string(),
        action_kind: CleanupActionKind::Command,
        status,
        reason_code: None,
        bytes_expected: before.bytes,
        released_bytes: before.bytes.saturating_sub(after.bytes),
        affected_item_count: before.item_count.saturating_sub(after.item_count),
        failed_item_count: 0,
        running_processes: Vec::new(),
    }
}

fn verified_action(before: CondaCacheUsage, after: CondaCacheUsage) -> CleanupActionResult {
    if after.bytes == 0 && after.item_count == 0 {
        return completed_action(CleanupActionStatus::Completed, before, after);
    }
    CleanupActionResult {
        rule_id: CLEANER_ID.to_string(),
        action_kind: CleanupActionKind::Command,
        status: CleanupActionStatus::Partial,
        reason_code: Some(CleanupActionReason::VerificationFailed),
        bytes_expected: before.bytes,
        released_bytes: before.bytes.saturating_sub(after.bytes),
        affected_item_count: before.item_count.saturating_sub(after.item_count),
        failed_item_count: after.item_count.max(1),
        running_processes: Vec::new(),
    }
}

fn failed_action(bytes_expected: u64, reason_code: CleanupActionReason) -> CleanupActionResult {
    CleanupActionResult {
        rule_id: CLEANER_ID.to_string(),
        action_kind: CleanupActionKind::Command,
        status: CleanupActionStatus::Failed,
        reason_code: Some(reason_code),
        bytes_expected,
        released_bytes: 0,
        affected_item_count: 0,
        failed_item_count: 1,
        running_processes: Vec::new(),
    }
}

fn log_conda_failure(stage: &str, error: CondaCleanerError) {
    log::warn!("conda_cache_cleaner_failed stage={stage} reason={error:?}");
}

#[cfg(test)]
mod tests {
    use super::*;

    fn output(json: &str) -> ControlledCommandOutput {
        ControlledCommandOutput {
            status: success_status(),
            stdout: json.as_bytes().to_vec(),
            stderr_bytes: 0,
            elapsed_ms: 3,
        }
    }

    #[test]
    fn preview_counts_only_selected_safe_cache_categories() {
        let usage = parse_preview_output(output(
            r#"{
                "success": true,
                "index_cache": {"files": ["/cache/pkgs/cache"]},
                "logfiles": ["/cache/pkgs/.logs/one.log", "/cache/pkgs/.logs/two.log"],
                "tarballs": {
                    "pkg_sizes": {
                        "/cache/pkgs": {"python.conda": 100, "rust.tar.bz2": 250}
                    },
                    "total_size": 350
                }
            }"#,
        ))
        .expect("valid Conda JSON must parse");
        assert_eq!(usage.bytes, 350);
        assert_eq!(usage.item_count, 5);
    }

    #[test]
    fn inconsistent_sizes_and_invalid_reports_fail_closed() {
        assert!(parse_preview_output(output(
            r#"{
                "success": true,
                "index_cache": {"files": []},
                "logfiles": [],
                "tarballs": {
                    "pkg_sizes": {"/cache/pkgs": {"python.conda": 100}},
                    "total_size": 99
                }
            }"#
        ))
        .is_err());
        assert!(parse_preview_output(output(r#"{"success": false}"#)).is_err());
        assert!(parse_preview_output(output(r#"{"success":"true"}"#)).is_err());
        assert!(parse_preview_output(output(r#"{"success":"#)).is_err());
    }

    #[test]
    fn nonzero_exit_fails_closed() {
        let mut failed = output(r#"{"success":true}"#);
        failed.status = failure_status();
        assert_eq!(
            parse_preview_output(failed).unwrap_err(),
            CondaCleanerError::NonZeroExit
        );
    }

    #[test]
    fn arguments_never_remove_packages_environments_or_all_caches() {
        assert!(PREVIEW_ARGS.contains(&"--dry-run"));
        assert!(CLEAN_ARGS.contains(&"--yes"));
        for arguments in [PREVIEW_ARGS, CLEAN_ARGS] {
            for required in ["--index-cache", "--tarballs", "--logfiles", "--json"] {
                assert!(arguments.contains(&required));
            }
            for forbidden in [
                "--all",
                "--packages",
                "--force-pkgs-dirs",
                "--tempfiles",
                "remove",
                "env",
            ] {
                assert!(!arguments.contains(&forbidden));
            }
        }
    }

    #[test]
    fn verification_reports_remaining_items_as_partial() {
        let before = CondaCacheUsage {
            bytes: 1_000,
            item_count: 4,
        };
        let partial = verified_action(
            before,
            CondaCacheUsage {
                bytes: 250,
                item_count: 1,
            },
        );
        assert_eq!(partial.status, CleanupActionStatus::Partial);
        assert!(matches!(
            partial.reason_code,
            Some(CleanupActionReason::VerificationFailed)
        ));
        assert_eq!(partial.released_bytes, 750);
        assert_eq!(partial.affected_item_count, 3);
        assert_eq!(partial.failed_item_count, 1);

        let completed = verified_action(before, CondaCacheUsage::default());
        assert_eq!(completed.status, CleanupActionStatus::Completed);
        assert_eq!(completed.released_bytes, 1_000);
        assert_eq!(completed.affected_item_count, 4);
        assert_eq!(completed.failed_item_count, 0);
    }

    #[cfg(unix)]
    fn success_status() -> std::process::ExitStatus {
        use std::os::unix::process::ExitStatusExt;
        std::process::ExitStatus::from_raw(0)
    }

    #[cfg(unix)]
    fn failure_status() -> std::process::ExitStatus {
        use std::os::unix::process::ExitStatusExt;
        std::process::ExitStatus::from_raw(1 << 8)
    }

    #[cfg(windows)]
    fn success_status() -> std::process::ExitStatus {
        use std::os::windows::process::ExitStatusExt;
        std::process::ExitStatus::from_raw(0)
    }

    #[cfg(windows)]
    fn failure_status() -> std::process::ExitStatus {
        use std::os::windows::process::ExitStatusExt;
        std::process::ExitStatus::from_raw(1)
    }
}

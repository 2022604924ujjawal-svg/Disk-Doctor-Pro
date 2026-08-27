use std::{
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

const CLEANER_ID: &str = "special.docker-build-cache";
const CLEANER_REVISION: &str = "docker-build-cache-v2-local-default-context";
const EXECUTABLE_ALIASES: &[&str] = &["docker", "docker.exe"];
// Always target the local default daemon. An explicit context overrides both
// DOCKER_HOST and a user-selected remote context, preventing accidental cleanup
// on another machine.
const PREVIEW_ARGS: &[&str] = &[
    "--context",
    "default",
    "system",
    "df",
    "--verbose",
    "--format",
    "json",
];
const PRUNE_ARGS: &[&str] = &[
    "--context",
    "default",
    "builder",
    "prune",
    "--all",
    "--force",
];
const PREVIEW_COMMAND_ID: &str = "docker.system-df";
const PRUNE_COMMAND_ID: &str = "docker.builder-prune";
const PREVIEW_TIMEOUT: Duration = Duration::from_secs(10);
const PRUNE_TIMEOUT: Duration = Duration::from_secs(5 * 60);
const PREVIEW_STDOUT_LIMIT: usize = 4 * 1024 * 1024;
const COMMAND_OUTPUT_LIMIT: usize = 256 * 1024;

pub(super) struct DockerBuildCacheCleaner;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "PascalCase")]
struct DockerSystemDiskUsage {
    build_cache: Vec<DockerBuildCacheRecord>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "PascalCase")]
struct DockerBuildCacheRecord {
    in_use: String,
    shared: String,
    size: String,
}

#[derive(Debug)]
struct DockerBuildCacheUsage {
    bytes: u64,
    record_count: u64,
}

impl CleanupCleaner for DockerBuildCacheCleaner {
    fn id(&self) -> &'static str {
        CLEANER_ID
    }

    fn revision(&self) -> &'static str {
        CLEANER_REVISION
    }

    fn category(&self) -> crate::cleanup::CleanupCategory {
        crate::cleanup::CleanupCategory::Container
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
        let result = run_docker(
            PREVIEW_COMMAND_ID,
            executable,
            PREVIEW_ARGS,
            ControlledCommandLimits {
                timeout: PREVIEW_TIMEOUT,
                stdout_bytes: PREVIEW_STDOUT_LIMIT,
                stderr_bytes: COMMAND_OUTPUT_LIMIT,
            },
            is_cancelled,
        )
        .and_then(parse_preview_output);
        let elapsed_ms = started.elapsed().as_millis() as u64;
        match result {
            Ok(usage) => CleanerPreview {
                status: CleanerPreviewStatus::Ready,
                bytes: usage.bytes,
                item_count: usage.record_count,
                elapsed_ms,
            },
            Err(error) => {
                log_docker_failure("preview", error);
                CleanerPreview {
                    // The scan coordinator observes cancellation after this call.
                    // The cleaner only terminates the child process and fails closed.
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
                log_docker_failure("execute_preflight", error);
                return failed_action(0, CleanupActionReason::PreflightFailed);
            }
        };
        if dry_run {
            return CleanupActionResult {
                rule_id: CLEANER_ID.to_string(),
                action_kind: CleanupActionKind::Command,
                status: CleanupActionStatus::Previewed,
                reason_code: None,
                bytes_expected: before.bytes,
                released_bytes: 0,
                affected_item_count: 0,
                failed_item_count: 0,
                running_processes: Vec::new(),
            };
        }
        if before.bytes == 0 {
            return CleanupActionResult {
                rule_id: CLEANER_ID.to_string(),
                action_kind: CleanupActionKind::Command,
                status: CleanupActionStatus::Completed,
                reason_code: None,
                bytes_expected: 0,
                released_bytes: 0,
                affected_item_count: 0,
                failed_item_count: 0,
                running_processes: Vec::new(),
            };
        }

        let prune = run_docker(
            PRUNE_COMMAND_ID,
            executable,
            PRUNE_ARGS,
            ControlledCommandLimits {
                timeout: PRUNE_TIMEOUT,
                stdout_bytes: COMMAND_OUTPUT_LIMIT,
                stderr_bytes: COMMAND_OUTPUT_LIMIT,
            },
            &|| operation.cancelled().load(Ordering::Relaxed),
        );
        let prune = match prune {
            Ok(output) if output.status.success() => output,
            Ok(output) => {
                log::warn!(
                    "docker_build_cache_prune_failed reason=nonZeroExit stderr_bytes={} elapsed_ms={}",
                    output.stderr_bytes,
                    output.elapsed_ms
                );
                return failed_action(before.bytes, CleanupActionReason::ExecutionFailed);
            }
            Err(error) => {
                log_docker_failure("prune", error);
                return failed_action(before.bytes, CleanupActionReason::ExecutionFailed);
            }
        };
        log::info!(
            "docker_build_cache_prune_completed stdout_bytes={} stderr_bytes={} elapsed_ms={}",
            prune.stdout.len(),
            prune.stderr_bytes,
            prune.elapsed_ms
        );

        let after = match preview_usage(executable, operation) {
            Ok(usage) => usage,
            Err(error) => {
                log_docker_failure("execute_verify", error);
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
        CleanupActionResult {
            rule_id: CLEANER_ID.to_string(),
            action_kind: CleanupActionKind::Command,
            status: CleanupActionStatus::Completed,
            reason_code: None,
            bytes_expected: before.bytes,
            released_bytes: before.bytes.saturating_sub(after.bytes),
            affected_item_count: before.record_count.saturating_sub(after.record_count),
            failed_item_count: 0,
            running_processes: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DockerCleanerError {
    Cancelled,
    ExecutableChanged,
    CommandUnavailable,
    NonZeroExit,
    InvalidOutput,
}

fn preview_usage(
    executable: &ControlledExecutable,
    operation: &OperationGuard,
) -> Result<DockerBuildCacheUsage, DockerCleanerError> {
    let output = run_docker(
        PREVIEW_COMMAND_ID,
        executable,
        PREVIEW_ARGS,
        ControlledCommandLimits {
            timeout: PREVIEW_TIMEOUT,
            stdout_bytes: PREVIEW_STDOUT_LIMIT,
            stderr_bytes: COMMAND_OUTPUT_LIMIT,
        },
        &|| operation.cancelled().load(Ordering::Relaxed),
    )?;
    parse_preview_output(output)
}

fn run_docker(
    command_id: &'static str,
    executable: &ControlledExecutable,
    args: &[&str],
    limits: ControlledCommandLimits,
    is_cancelled: &(dyn Fn() -> bool + Sync),
) -> Result<ControlledCommandOutput, DockerCleanerError> {
    run_controlled_command(
        command_id,
        executable,
        args,
        ControlledEnvironmentPolicy::Isolated,
        limits,
        is_cancelled,
    )
    .map_err(|error| match error {
        ControlledCommandError::Cancelled => DockerCleanerError::Cancelled,
        ControlledCommandError::ExecutableChanged => DockerCleanerError::ExecutableChanged,
        _ => DockerCleanerError::CommandUnavailable,
    })
}

fn parse_preview_output(
    output: ControlledCommandOutput,
) -> Result<DockerBuildCacheUsage, DockerCleanerError> {
    if !output.status.success() {
        return Err(DockerCleanerError::NonZeroExit);
    }
    parse_disk_usage(&output.stdout)
}

fn parse_disk_usage(bytes: &[u8]) -> Result<DockerBuildCacheUsage, DockerCleanerError> {
    let usage = serde_json::from_slice::<DockerSystemDiskUsage>(bytes)
        .map_err(|_| DockerCleanerError::InvalidOutput)?;
    let mut total_bytes = 0_u64;
    let mut record_count = 0_u64;
    for record in usage.build_cache {
        let in_use = parse_boolean(&record.in_use)?;
        let shared = parse_boolean(&record.shared)?;
        let bytes = parse_size(&record.size)?;
        if in_use || shared || bytes == 0 {
            continue;
        }
        total_bytes = total_bytes
            .checked_add(bytes)
            .ok_or(DockerCleanerError::InvalidOutput)?;
        record_count = record_count
            .checked_add(1)
            .ok_or(DockerCleanerError::InvalidOutput)?;
    }
    Ok(DockerBuildCacheUsage {
        bytes: total_bytes,
        record_count,
    })
}

fn parse_boolean(value: &str) -> Result<bool, DockerCleanerError> {
    match value {
        "true" => Ok(true),
        "false" => Ok(false),
        _ => Err(DockerCleanerError::InvalidOutput),
    }
}

fn parse_size(value: &str) -> Result<u64, DockerCleanerError> {
    const UNITS: [(&str, u128); 9] = [
        ("TiB", 1024_u128.pow(4)),
        ("GiB", 1024_u128.pow(3)),
        ("MiB", 1024_u128.pow(2)),
        ("KiB", 1024),
        ("TB", 1_000_000_000_000),
        ("GB", 1_000_000_000),
        ("MB", 1_000_000),
        ("kB", 1_000),
        ("B", 1),
    ];
    let (number, multiplier) = UNITS
        .iter()
        .find_map(|(unit, multiplier)| value.strip_suffix(unit).map(|number| (number, *multiplier)))
        .ok_or(DockerCleanerError::InvalidOutput)?;
    let (whole, fraction) = number.split_once('.').unwrap_or((number, ""));
    if whole.is_empty()
        || !whole.bytes().all(|byte| byte.is_ascii_digit())
        || !fraction.bytes().all(|byte| byte.is_ascii_digit())
        || fraction.len() > 9
    {
        return Err(DockerCleanerError::InvalidOutput);
    }
    let whole = whole
        .parse::<u128>()
        .map_err(|_| DockerCleanerError::InvalidOutput)?;
    let fraction_value = if fraction.is_empty() {
        0
    } else {
        let numerator = fraction
            .parse::<u128>()
            .map_err(|_| DockerCleanerError::InvalidOutput)?;
        let denominator = 10_u128
            .checked_pow(fraction.len() as u32)
            .ok_or(DockerCleanerError::InvalidOutput)?;
        numerator
            .checked_mul(multiplier)
            .ok_or(DockerCleanerError::InvalidOutput)?
            / denominator
    };
    let bytes = whole
        .checked_mul(multiplier)
        .and_then(|value| value.checked_add(fraction_value))
        .ok_or(DockerCleanerError::InvalidOutput)?;
    u64::try_from(bytes).map_err(|_| DockerCleanerError::InvalidOutput)
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

fn log_docker_failure(stage: &str, error: DockerCleanerError) {
    log::warn!("docker_build_cache_cleaner_failed stage={stage} reason={error:?}");
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
    fn preview_counts_only_unused_unshared_build_cache() {
        let usage = parse_preview_output(output(
            r#"{"BuildCache":[
                {"InUse":"false","Shared":"false","Size":"1.5GB"},
                {"InUse":"true","Shared":"false","Size":"2GB"},
                {"InUse":"false","Shared":"true","Size":"3GB"},
                {"InUse":"false","Shared":"false","Size":"512MiB"},
                {"InUse":"false","Shared":"false","Size":"0B"}
            ]}"#,
        ))
        .expect("valid Docker JSON must parse");
        assert_eq!(usage.record_count, 2);
        assert_eq!(usage.bytes, 1_500_000_000 + 512 * 1024 * 1024);
    }

    #[test]
    fn size_parser_accepts_known_units_and_rejects_overflow() {
        assert_eq!(parse_size("4.554kB").unwrap(), 4_554);
        assert_eq!(parse_size("1.25GiB").unwrap(), 1_342_177_280);
        assert_eq!(parse_size("88B").unwrap(), 88);
        assert!(parse_size("12XB").is_err());
        assert!(parse_size("999999999999999999999TB").is_err());
    }

    #[test]
    fn invalid_booleans_and_truncated_json_fail_closed() {
        assert!(parse_preview_output(output(
            r#"{"BuildCache":[{"InUse":"yes","Shared":"false","Size":"1GB"}]}"#
        ))
        .is_err());
        assert!(parse_preview_output(output(r#"{"BuildCache":["#)).is_err());
    }

    #[test]
    fn malformed_records_and_nonzero_exit_fail_closed() {
        assert!(
            parse_preview_output(output(r#"{"BuildCache":[{"InUse":"false","Size":"1GB"}]}"#))
                .is_err()
        );
        assert!(parse_preview_output(output(
            r#"{"BuildCache":[{"InUse":false,"Shared":"false","Size":"1GB"}]}"#
        ))
        .is_err());
        assert!(parse_preview_output(output(
            r#"{"BuildCache":[{"InUse":"false","Shared":"false","Shared":"false","Size":"1GB"}]}"#
        ))
        .is_err());
        let mut failed = output(r#"{"BuildCache":[]}"#);
        failed.status = failure_status();
        assert_eq!(
            parse_preview_output(failed).unwrap_err(),
            DockerCleanerError::NonZeroExit
        );
    }

    #[test]
    fn command_arguments_never_target_images_containers_networks_or_volumes() {
        assert_eq!(
            PREVIEW_ARGS,
            [
                "--context",
                "default",
                "system",
                "df",
                "--verbose",
                "--format",
                "json"
            ]
        );
        assert_eq!(
            PRUNE_ARGS,
            [
                "--context",
                "default",
                "builder",
                "prune",
                "--all",
                "--force"
            ]
        );
        let joined = PRUNE_ARGS.join(" ");
        for forbidden in ["image", "container", "network", "volume", "system prune"] {
            assert!(!joined.contains(forbidden));
        }
        assert!(joined.starts_with("--context default "));
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

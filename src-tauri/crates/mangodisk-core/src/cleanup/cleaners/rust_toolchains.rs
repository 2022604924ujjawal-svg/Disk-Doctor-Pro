use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
    sync::atomic::Ordering,
    time::{Duration, Instant},
};

use mangodisk_platform::{
    run_controlled_command, ControlledCommandError, ControlledCommandLimits,
    ControlledEnvironmentPolicy, ControlledExecutable,
};
use serde::Deserialize;

use crate::{
    applications::catalog::ApplicationInventory,
    cleanup::{
        measurement::measure_path_filtered, source_selection::SourceScope, CleanupActionKind,
        CleanupActionReason, CleanupActionResult, CleanupActionStatus, CleanupCategory,
        CleanupGroup, CleanupSourceBlockReason, CleanupSourceDetail, RiskLevel, ScanItemStatus,
        ScanRuleResult,
    },
    filesystem::metadata::display_path,
    shared::operation::OperationGuard,
};

use super::project_root_index;

pub(super) const CLEANER_ID: &str = "special.rust-toolchains";
pub(super) const CLEANER_REVISION: &str = "rust-toolchains-v1-rustup-protected-context";

const EXECUTABLE_ALIASES: &[&str] = &["rustup", "rustup.exe"];
const LIST_ARGS: &[&str] = &["toolchain", "list", "-v"];
const OVERRIDE_ARGS: &[&str] = &["override", "list"];
const COMMAND_TIMEOUT: Duration = Duration::from_secs(30);
const UNINSTALL_TIMEOUT: Duration = Duration::from_secs(5 * 60);
const STDOUT_LIMIT: usize = 2 * 1024 * 1024;
const STDERR_LIMIT: usize = 256 * 1024;
const MAX_PREVIEW_SOURCES: usize = 256;
/*
 * Rustup searches parent directories for project toolchain files. Keep the
 * same inheritance semantics, but cap traversal so malformed or unusually
 * deep paths cannot turn cleanup discovery into unbounded filesystem work.
 * Reaching the cap fails closed instead of exposing a possibly required
 * toolchain as removable.
 */
const MAX_PROJECT_TOOLCHAIN_ANCESTORS: usize = 64;

#[derive(Debug, Clone, PartialEq, Eq)]
struct InstalledToolchain {
    name: String,
    path: PathBuf,
    is_default: bool,
    is_active: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ToolchainCandidate {
    toolchain: InstalledToolchain,
    bytes: u64,
    file_count: u64,
    complete: bool,
}

#[derive(Debug, Default, Deserialize)]
struct RustToolchainFile {
    toolchain: Option<RustToolchainSection>,
}

#[derive(Debug, Default, Deserialize)]
struct RustToolchainSection {
    channel: Option<String>,
}

#[derive(Debug, Default)]
struct ProjectToolchainLookup {
    direct_channels: HashMap<PathBuf, Option<String>>,
}

impl ProjectToolchainLookup {
    fn resolve(&mut self, root: &Path) -> Result<Option<String>, ToolchainError> {
        if !root.is_absolute() {
            return Err(ToolchainError::ProjectContext);
        }
        for (depth, directory) in root.ancestors().enumerate() {
            if depth >= MAX_PROJECT_TOOLCHAIN_ANCESTORS {
                log::warn!(
                    "rust_toolchain_project_ancestor_limit_reached max_ancestors={MAX_PROJECT_TOOLCHAIN_ANCESTORS}"
                );
                return Err(ToolchainError::ProjectContext);
            }
            if let Some(channel) = self.direct_channel(directory)? {
                return Ok(Some(channel));
            }
        }
        Ok(None)
    }

    fn direct_channel(&mut self, directory: &Path) -> Result<Option<String>, ToolchainError> {
        if let Some(channel) = self.direct_channels.get(directory) {
            return Ok(channel.clone());
        }
        let channel = direct_project_toolchain_channel(directory)?;
        self.direct_channels
            .insert(directory.to_path_buf(), channel.clone());
        Ok(channel)
    }

    fn inspected_directory_count(&self) -> usize {
        self.direct_channels.len()
    }
}

pub(super) fn preview(
    inventory: &ApplicationInventory,
    project_roots: &[String],
    is_cancelled: &(dyn Fn() -> bool + Sync),
    report_path: &(dyn Fn(&Path) + Sync),
    report_files: &(dyn Fn(&Path, u64, u64) + Sync),
) -> ScanRuleResult {
    let started = Instant::now();
    let Some(executable) = inventory.executable(EXECUTABLE_ALIASES) else {
        return unavailable_rule(
            if inventory.executable_inventory_complete() {
                ScanItemStatus::NotApplicable
            } else {
                ScanItemStatus::Limited
            },
            started.elapsed().as_millis() as u64,
        );
    };
    match discover_candidates(
        &executable,
        project_roots,
        is_cancelled,
        report_path,
        report_files,
    ) {
        Ok(candidates) => candidate_rule(candidates, started.elapsed().as_millis() as u64),
        Err(error) => {
            log::warn!("rust_toolchains_preview_limited reason={}", error.as_str());
            unavailable_rule(
                ScanItemStatus::Limited,
                started.elapsed().as_millis() as u64,
            )
        }
    }
}

pub(super) fn limited_rule() -> ScanRuleResult {
    unavailable_rule(ScanItemStatus::Limited, 0)
}

pub(super) fn execute(
    inventory: &ApplicationInventory,
    project_roots: &[String],
    scope: Option<&SourceScope>,
    dry_run: bool,
    operation: &OperationGuard,
) -> CleanupActionResult {
    let Some(executable) = inventory.executable(EXECUTABLE_ALIASES) else {
        return failed_action(0, CleanupActionReason::RequiredToolUnavailable);
    };
    let cancelled = || operation.cancelled().load(Ordering::Relaxed);
    let candidates = match discover_candidates(
        &executable,
        project_roots,
        &cancelled,
        &|_| {},
        &|_, _, _| {},
    ) {
        Ok(candidates) => candidates,
        Err(ToolchainError::Cancelled) => {
            return failed_action(0, CleanupActionReason::Cancelled);
        }
        Err(_) => return failed_action(0, CleanupActionReason::PreflightFailed),
    };
    let complete = candidates
        .iter()
        .filter(|candidate| candidate.complete)
        .collect::<Vec<_>>();
    if let Some(scope) = scope {
        if scope
            .validate_known_paths(
                candidates
                    .iter()
                    .map(|candidate| candidate.toolchain.path.as_path()),
            )
            .is_err()
        {
            return failed_action(0, CleanupActionReason::PreflightFailed);
        }
    }
    let selected = complete
        .into_iter()
        .filter(|candidate| scope.is_none_or(|scope| scope.selects(&candidate.toolchain.path)))
        .collect::<Vec<_>>();
    let expected_bytes = selected.iter().map(|candidate| candidate.bytes).sum();
    if dry_run {
        return completed_action(
            CleanupActionStatus::Previewed,
            false,
            expected_bytes,
            0,
            selected.len() as u64,
            0,
        );
    }

    let mut command_succeeded = Vec::new();
    let mut failed_item_count = 0_u64;
    let mut was_cancelled = false;
    for candidate in selected {
        if operation.ensure_not_cancelled().is_err() {
            was_cancelled = true;
            break;
        }
        let args = ["toolchain", "uninstall", candidate.toolchain.name.as_str()];
        let result = run_rustup(
            "rustup.toolchain-uninstall",
            &executable,
            &args,
            UNINSTALL_TIMEOUT,
            &cancelled,
        );
        match result {
            Ok(output) if output.status.success() => {
                command_succeeded.push(candidate);
            }
            Err(ToolchainError::Cancelled) => {
                was_cancelled = true;
                break;
            }
            _ => failed_item_count += 1,
        }
    }

    let mut released_bytes = 0_u64;
    let mut affected_item_count = 0_u64;
    /*
     * Cancellation stops new uninstall commands, but commands that already
     * succeeded still require a bounded read-only reconciliation. Otherwise a
     * cancelled operation could report no release after changing the system.
     */
    match list_installed(&executable, &|| false) {
        Ok(installed_after) => {
            let remaining = installed_after
                .into_iter()
                .map(|toolchain| toolchain.name)
                .collect::<HashSet<_>>();
            for candidate in command_succeeded {
                if remaining.contains(&candidate.toolchain.name) {
                    failed_item_count += 1;
                } else {
                    released_bytes = released_bytes.saturating_add(candidate.bytes);
                    affected_item_count += 1;
                }
            }
        }
        Err(_) => {
            // A successful command is not enough evidence for cleanup. If the
            // final rustup inventory cannot verify absence, fail closed and do
            // not report the toolchain bytes as released.
            failed_item_count = failed_item_count.saturating_add(command_succeeded.len() as u64);
        }
    }
    let status = if was_cancelled && affected_item_count == 0 {
        CleanupActionStatus::Blocked
    } else if was_cancelled {
        CleanupActionStatus::Partial
    } else if failed_item_count == 0 {
        CleanupActionStatus::Completed
    } else if affected_item_count > 0 {
        CleanupActionStatus::Partial
    } else {
        CleanupActionStatus::Failed
    };
    log::info!(
        "rust_toolchains_uninstall_finished expected_bytes={expected_bytes} released_bytes={released_bytes} affected_toolchains={affected_item_count} failed_toolchains={failed_item_count} cancelled={was_cancelled}"
    );
    completed_action(
        status,
        was_cancelled,
        expected_bytes,
        released_bytes,
        affected_item_count,
        failed_item_count,
    )
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ToolchainError {
    Cancelled,
    Command,
    InvalidOutput,
    ProjectContext,
}

impl ToolchainError {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Cancelled => "cancelled",
            Self::Command => "commandFailed",
            Self::InvalidOutput => "invalidOutput",
            Self::ProjectContext => "projectContextUnavailable",
        }
    }
}

fn discover_candidates(
    executable: &ControlledExecutable,
    project_roots: &[String],
    is_cancelled: &(dyn Fn() -> bool + Sync),
    report_path: &(dyn Fn(&Path) + Sync),
    report_files: &(dyn Fn(&Path, u64, u64) + Sync),
) -> Result<Vec<ToolchainCandidate>, ToolchainError> {
    let installed = list_installed(executable, is_cancelled)?;
    let mut protected_channels = installed
        .iter()
        .filter(|toolchain| toolchain.is_default || toolchain.is_active)
        .map(|toolchain| toolchain.name.clone())
        .collect::<HashSet<_>>();
    protected_channels.extend(list_override_channels(executable, is_cancelled)?);
    protected_channels.extend(project_toolchain_channels(project_roots)?);

    let mut candidates = Vec::new();
    for toolchain in installed {
        if is_cancelled() {
            return Err(ToolchainError::Cancelled);
        }
        if protected_channels
            .iter()
            .any(|channel| channel_matches_toolchain(channel, &toolchain.name))
        {
            continue;
        }
        report_path(&toolchain.path);
        let measured = measure_path_filtered(&toolchain.path, None, &|_, _| true);
        report_files(&toolchain.path, measured.file_count, measured.bytes);
        candidates.push(ToolchainCandidate {
            toolchain,
            bytes: measured.bytes,
            file_count: measured.file_count,
            complete: measured.skipped_count == 0,
        });
    }
    candidates.sort_by(|left, right| {
        right
            .bytes
            .cmp(&left.bytes)
            .then_with(|| left.toolchain.name.cmp(&right.toolchain.name))
    });
    Ok(candidates)
}

fn list_installed(
    executable: &ControlledExecutable,
    is_cancelled: &(dyn Fn() -> bool + Sync),
) -> Result<Vec<InstalledToolchain>, ToolchainError> {
    let output = run_rustup(
        "rustup.toolchain-list",
        executable,
        LIST_ARGS,
        COMMAND_TIMEOUT,
        is_cancelled,
    )?;
    if !output.status.success() {
        return Err(ToolchainError::Command);
    }
    parse_toolchain_list(
        &String::from_utf8(output.stdout).map_err(|_| ToolchainError::InvalidOutput)?,
    )
}

fn list_override_channels(
    executable: &ControlledExecutable,
    is_cancelled: &(dyn Fn() -> bool + Sync),
) -> Result<HashSet<String>, ToolchainError> {
    let output = run_rustup(
        "rustup.override-list",
        executable,
        OVERRIDE_ARGS,
        COMMAND_TIMEOUT,
        is_cancelled,
    )?;
    if !output.status.success() {
        return Err(ToolchainError::Command);
    }
    let text = String::from_utf8(output.stdout).map_err(|_| ToolchainError::InvalidOutput)?;
    Ok(parse_override_channels(&text))
}

fn run_rustup(
    command_id: &'static str,
    executable: &ControlledExecutable,
    args: &[&str],
    timeout: Duration,
    is_cancelled: &(dyn Fn() -> bool + Sync),
) -> Result<mangodisk_platform::ControlledCommandOutput, ToolchainError> {
    run_controlled_command(
        command_id,
        executable,
        args,
        ControlledEnvironmentPolicy::Isolated,
        ControlledCommandLimits {
            timeout,
            stdout_bytes: STDOUT_LIMIT,
            stderr_bytes: STDERR_LIMIT,
        },
        is_cancelled,
    )
    .map_err(|error| match error {
        ControlledCommandError::Cancelled => ToolchainError::Cancelled,
        _ => ToolchainError::Command,
    })
}

fn parse_toolchain_list(text: &str) -> Result<Vec<InstalledToolchain>, ToolchainError> {
    let mut toolchains = Vec::new();
    for line in text.lines().map(str::trim).filter(|line| !line.is_empty()) {
        let name_end = line
            .find(char::is_whitespace)
            .ok_or(ToolchainError::InvalidOutput)?;
        let name = line[..name_end].to_string();
        let remainder = line[name_end..].trim();
        let (markers, path_text) = if remainder.starts_with('(') {
            // rustup can combine markers, for example `(default, active)`.
            // The path always follows the final closing parenthesis.
            let marker_end = remainder.rfind(')').ok_or(ToolchainError::InvalidOutput)?;
            (
                remainder[1..marker_end]
                    .split(',')
                    .map(str::trim)
                    .collect::<HashSet<_>>(),
                remainder[marker_end + 1..].trim(),
            )
        } else {
            (HashSet::new(), remainder)
        };
        let is_default = markers.contains("default");
        let is_active = markers.contains("active");
        if path_text.is_empty() {
            return Err(ToolchainError::InvalidOutput);
        }
        toolchains.push(InstalledToolchain {
            name,
            path: PathBuf::from(path_text),
            is_default,
            is_active,
        });
    }
    Ok(toolchains)
}

fn parse_override_channels(text: &str) -> HashSet<String> {
    text.lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.contains("no overrides"))
        .filter_map(|line| line.split_whitespace().last())
        .map(ToOwned::to_owned)
        .collect()
}

fn project_toolchain_channels(
    configured_roots: &[String],
) -> Result<HashSet<String>, ToolchainError> {
    let started = Instant::now();
    let mut roots = project_root_index::load().map_err(|error| {
        log::warn!(
            "rust_toolchain_project_index_load_failed error_digest={}",
            blake3::hash(error.as_bytes()).to_hex()
        );
        ToolchainError::ProjectContext
    })?;
    roots.extend(configured_roots.iter().map(PathBuf::from));
    roots.sort();
    roots.dedup();
    let root_count = roots.len();
    let channels = project_toolchain_channels_from_roots(&roots)?;
    log::info!(
        "rust_toolchain_project_context_loaded root_count={} protected_channel_count={} elapsed_ms={}",
        root_count,
        channels.len(),
        started.elapsed().as_millis()
    );
    Ok(channels)
}

fn project_toolchain_channels_from_roots(
    project_roots: &[PathBuf],
) -> Result<HashSet<String>, ToolchainError> {
    let mut lookup = ProjectToolchainLookup::default();
    let mut channels = HashSet::new();
    for root in project_roots {
        if let Some(channel) = lookup.resolve(root)? {
            channels.insert(channel);
        }
    }
    log::debug!(
        "rust_toolchain_project_configs_resolved root_count={} inspected_directory_count={} protected_channel_count={}",
        project_roots.len(),
        lookup.inspected_directory_count(),
        channels.len()
    );
    Ok(channels)
}

fn direct_project_toolchain_channel(directory: &Path) -> Result<Option<String>, ToolchainError> {
    let toml_path = directory.join("rust-toolchain.toml");
    match std::fs::read_to_string(&toml_path) {
        Ok(content) => {
            let channel = toml::from_str::<RustToolchainFile>(&content)
                .map_err(|_| ToolchainError::ProjectContext)?
                .toolchain
                .and_then(|toolchain| toolchain.channel)
                .map(|channel| channel.trim().to_string())
                .filter(|channel| !channel.is_empty());
            return Ok(channel);
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(_) => return Err(ToolchainError::ProjectContext),
    }
    let path = directory.join("rust-toolchain");
    let content = match std::fs::read_to_string(path) {
        Ok(content) => content,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err(ToolchainError::ProjectContext),
    };
    Ok(content
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty() && !line.starts_with('#'))
        .map(ToOwned::to_owned))
}

fn channel_matches_toolchain(channel: &str, toolchain: &str) -> bool {
    channel == toolchain
        || toolchain
            .strip_prefix(channel)
            .is_some_and(|suffix| suffix.starts_with('-'))
}

fn candidate_rule(candidates: Vec<ToolchainCandidate>, elapsed_ms: u64) -> ScanRuleResult {
    let bytes = candidates.iter().map(|candidate| candidate.bytes).sum();
    let file_count = candidates
        .iter()
        .map(|candidate| candidate.file_count)
        .sum();
    let source_count = candidates.len() as u64;
    let selectable = candidates.iter().any(|candidate| candidate.complete);
    let sources = candidates
        .iter()
        .take(MAX_PREVIEW_SOURCES)
        .map(|candidate| CleanupSourceDetail {
            path: display_path(&candidate.toolchain.path),
            bytes: candidate.bytes,
            file_count: candidate.file_count,
            modified_at_ms: None,
            block_reason: (!candidate.complete)
                .then_some(CleanupSourceBlockReason::IncompleteMeasurement),
        })
        .collect();
    ScanRuleResult {
        rule_id: CLEANER_ID.to_string(),
        category: CleanupCategory::Development,
        group: CleanupGroup::Development,
        risk: RiskLevel::Recoverable,
        default_selected: false,
        recommended_selected: false,
        bytes,
        file_count,
        available: true,
        selectable,
        status: if candidates.is_empty() {
            ScanItemStatus::Clean
        } else if selectable {
            ScanItemStatus::Found
        } else {
            ScanItemStatus::ReviewOnly
        },
        running_processes: Vec::new(),
        requires_app_close: false,
        sources,
        source_count,
        sources_truncated: source_count > MAX_PREVIEW_SOURCES as u64,
        scan_elapsed_ms: elapsed_ms,
    }
}

fn unavailable_rule(status: ScanItemStatus, elapsed_ms: u64) -> ScanRuleResult {
    ScanRuleResult {
        rule_id: CLEANER_ID.to_string(),
        category: CleanupCategory::Development,
        group: CleanupGroup::Development,
        risk: RiskLevel::Recoverable,
        default_selected: false,
        recommended_selected: false,
        bytes: 0,
        file_count: 0,
        available: status != ScanItemStatus::NotApplicable,
        selectable: false,
        status,
        running_processes: Vec::new(),
        requires_app_close: false,
        sources: Vec::new(),
        source_count: 0,
        sources_truncated: false,
        scan_elapsed_ms: elapsed_ms,
    }
}

fn failed_action(bytes_expected: u64, reason: CleanupActionReason) -> CleanupActionResult {
    CleanupActionResult {
        rule_id: CLEANER_ID.to_string(),
        action_kind: CleanupActionKind::Command,
        status: if reason == CleanupActionReason::Cancelled {
            CleanupActionStatus::Blocked
        } else {
            CleanupActionStatus::Failed
        },
        reason_code: Some(reason),
        bytes_expected,
        released_bytes: 0,
        affected_item_count: 0,
        failed_item_count: u64::from(reason != CleanupActionReason::Cancelled),
        running_processes: Vec::new(),
    }
}

fn completed_action(
    status: CleanupActionStatus,
    was_cancelled: bool,
    bytes_expected: u64,
    released_bytes: u64,
    affected_item_count: u64,
    failed_item_count: u64,
) -> CleanupActionResult {
    CleanupActionResult {
        rule_id: CLEANER_ID.to_string(),
        action_kind: CleanupActionKind::Command,
        status,
        reason_code: if was_cancelled {
            Some(CleanupActionReason::Cancelled)
        } else {
            (failed_item_count > 0).then_some(CleanupActionReason::ItemsSkipped)
        },
        bytes_expected,
        released_bytes,
        affected_item_count,
        failed_item_count,
        running_processes: Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn project_toolchain_channel(root: &Path) -> Result<Option<String>, ToolchainError> {
        ProjectToolchainLookup::default().resolve(root)
    }

    fn test_directory(label: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "mangodisk-rust-toolchain-{label}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::SystemTime::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&path).unwrap();
        path
    }

    #[test]
    fn parses_verbose_toolchain_inventory_and_markers() {
        let parsed = parse_toolchain_list(
            "stable-aarch64-apple-darwin (default, active) /tmp/stable\n1.88.0-aarch64-apple-darwin (active) /tmp/1.88\n1.91.1-aarch64-apple-darwin /tmp/1.91\n",
        )
        .unwrap();

        assert_eq!(parsed.len(), 3);
        assert!(parsed[0].is_default);
        assert!(parsed[0].is_active);
        assert!(parsed[1].is_active);
        assert_eq!(parsed[2].path, PathBuf::from("/tmp/1.91"));
    }

    #[test]
    fn project_channel_protects_target_specific_toolchain_name() {
        assert!(channel_matches_toolchain(
            "1.88.0",
            "1.88.0-aarch64-apple-darwin"
        ));
        assert!(!channel_matches_toolchain(
            "1.88.0",
            "1.91.1-aarch64-apple-darwin"
        ));
    }

    #[test]
    fn parses_plain_and_toml_project_toolchain_files() {
        let plain = test_directory("plain");
        std::fs::write(plain.join("rust-toolchain"), "1.87.0\n").unwrap();
        assert_eq!(
            project_toolchain_channel(&plain).unwrap().as_deref(),
            Some("1.87.0")
        );

        let configured = test_directory("toml");
        std::fs::write(
            configured.join("rust-toolchain.toml"),
            "[toolchain]\nchannel = \"1.88.0\"\n",
        )
        .unwrap();
        assert_eq!(
            project_toolchain_channel(&configured).unwrap().as_deref(),
            Some("1.88.0")
        );
        std::fs::remove_dir_all(plain).unwrap();
        std::fs::remove_dir_all(configured).unwrap();
    }

    #[test]
    fn collects_channels_from_every_trusted_project_root() {
        let first = test_directory("multiple-first");
        let second = test_directory("multiple-second");
        std::fs::write(first.join("rust-toolchain"), "1.87.0\n").unwrap();
        std::fs::write(
            second.join("rust-toolchain.toml"),
            "[toolchain]\nchannel = \"1.88.0\"\n",
        )
        .unwrap();

        let channels =
            project_toolchain_channels_from_roots(&[first.clone(), second.clone()]).unwrap();

        assert_eq!(channels.len(), 2);
        assert!(channels.contains("1.87.0"));
        assert!(channels.contains("1.88.0"));
        std::fs::remove_dir_all(first).unwrap();
        std::fs::remove_dir_all(second).unwrap();
    }

    #[test]
    fn inherits_the_nearest_parent_toolchain_configuration() {
        let workspace = test_directory("inherited-parent");
        std::fs::write(workspace.join("rust-toolchain"), "1.87.0\n").unwrap();
        let project = workspace.join("packages").join("application");
        std::fs::create_dir_all(&project).unwrap();

        assert_eq!(
            project_toolchain_channel(&project).unwrap().as_deref(),
            Some("1.87.0")
        );
        std::fs::remove_dir_all(workspace).unwrap();
    }

    #[test]
    fn nearest_project_toolchain_configuration_overrides_its_parent() {
        let workspace = test_directory("nearest-parent");
        std::fs::write(workspace.join("rust-toolchain"), "1.87.0\n").unwrap();
        let project = workspace.join("application");
        std::fs::create_dir_all(&project).unwrap();
        std::fs::write(
            project.join("rust-toolchain.toml"),
            "[toolchain]\nchannel = \"1.88.0\"\n",
        )
        .unwrap();

        assert_eq!(
            project_toolchain_channel(&project).unwrap().as_deref(),
            Some("1.88.0")
        );
        std::fs::remove_dir_all(workspace).unwrap();
    }

    #[test]
    fn shared_ancestors_are_read_once_for_multiple_projects() {
        let workspace = test_directory("shared-parent-cache");
        std::fs::write(workspace.join("rust-toolchain"), "1.87.0\n").unwrap();
        let first = workspace.join("first");
        let second = workspace.join("second");
        std::fs::create_dir_all(&first).unwrap();
        std::fs::create_dir_all(&second).unwrap();
        let mut lookup = ProjectToolchainLookup::default();

        assert_eq!(lookup.resolve(&first).unwrap().as_deref(), Some("1.87.0"));
        assert_eq!(lookup.resolve(&second).unwrap().as_deref(), Some("1.87.0"));
        assert_eq!(lookup.inspected_directory_count(), 3);
        std::fs::remove_dir_all(workspace).unwrap();
    }

    #[test]
    fn excessive_ancestor_depth_fails_closed() {
        let workspace = test_directory("ancestor-limit");
        let mut project = workspace.clone();
        for depth in 0..MAX_PROJECT_TOOLCHAIN_ANCESTORS {
            project.push(format!("d{depth}"));
        }
        std::fs::create_dir_all(&project).unwrap();

        assert_eq!(
            project_toolchain_channel(&project),
            Err(ToolchainError::ProjectContext)
        );
        std::fs::remove_dir_all(workspace).unwrap();
    }

    #[test]
    fn malformed_project_toolchain_file_fails_closed() {
        let project = test_directory("malformed");
        std::fs::write(
            project.join("rust-toolchain.toml"),
            "[toolchain\nchannel = \"nightly\"\n",
        )
        .unwrap();

        assert_eq!(
            project_toolchain_channels_from_roots(std::slice::from_ref(&project)),
            Err(ToolchainError::ProjectContext)
        );
        std::fs::remove_dir_all(project).unwrap();
    }

    #[test]
    fn cancelled_action_does_not_report_a_failed_toolchain() {
        let result = failed_action(42, CleanupActionReason::Cancelled);

        assert_eq!(result.status, CleanupActionStatus::Blocked);
        assert_eq!(result.reason_code, Some(CleanupActionReason::Cancelled));
        assert_eq!(result.failed_item_count, 0);
    }

    #[test]
    fn partial_cancellation_preserves_verified_effects() {
        let result = completed_action(CleanupActionStatus::Partial, true, 100, 60, 1, 0);

        assert_eq!(result.status, CleanupActionStatus::Partial);
        assert_eq!(result.reason_code, Some(CleanupActionReason::Cancelled));
        assert_eq!(result.released_bytes, 60);
        assert_eq!(result.affected_item_count, 1);
        assert_eq!(result.failed_item_count, 0);
    }
}

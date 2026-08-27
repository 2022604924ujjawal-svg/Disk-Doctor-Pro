use std::{
    collections::{BTreeMap, HashMap},
    fs,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::Instant,
};

use serde::Serialize;

use crate::{
    cleanup::{
        cleaners, rules, CleanupScanEngineInfo, CleanupScanResult, CleanupScanService, RiskLevel,
        ScanItemStatus, ScanRuleResult,
    },
    filesystem::DiskInfo,
    reporting::benchmark::system::{
        environment_info, local_timestamp, now_ms, BenchmarkEnvironment, BenchmarkSourceInfo,
    },
    shared::TraversalProgress,
};

const BASELINE_SCHEMA_VERSION: &str = "1.7";
// Keep the persisted report kind, directory, and legacy `specialCleaner*`
// JSON fields stable so current reports remain comparable after internal
// cleanup-domain renames.
const BASELINE_REPORT_KIND: &str = "deep-clean";
const DEFAULT_OUTPUT_DIRECTORY: &str = ".local/reports/baselines/deep-clean";
const MAX_BASELINE_RUNS: usize = 10;
const MAX_NOTE_CHARACTERS: usize = 500;

#[derive(Debug, Clone)]
pub struct CleanupBaselineOptions {
    pub label: String,
    pub environment_id: Option<String>,
    pub note: Option<String>,
    pub runs: usize,
    pub output_directory: Option<PathBuf>,
    pub project_roots: Vec<String>,
    pub deep_project_discovery: bool,
    pub source: BenchmarkSourceInfo,
}

#[derive(Debug)]
pub struct BaselineArtifacts {
    pub json_path: PathBuf,
    pub markdown_path: PathBuf,
}

#[derive(Debug, Clone, Default)]
struct ProgressCapture {
    event_count: u64,
    first_progress_ms: Option<u64>,
    first_scan_observation_ms: Option<u64>,
    first_match_ms: Option<u64>,
    last_progress: Option<TraversalProgress>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct BaselineMethodology {
    scan_only: bool,
    cleanup_executed: bool,
    first_run_os_cache_state_known: bool,
    repeat_runs_may_use_os_cache: bool,
    project_root_count: usize,
    deep_project_discovery: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct DurationStatistics {
    first_run_ms: u64,
    minimum_ms: u64,
    median_ms: u64,
    maximum_ms: u64,
    mean_ms: u64,
    repeated_run_median_ms: Option<u64>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct BaselineRun {
    run_number: usize,
    scan_elapsed_ms: u64,
    caller_wall_elapsed_ms: u64,
    progress_event_count: u64,
    first_progress_ms: Option<u64>,
    first_scan_observation_ms: Option<u64>,
    first_match_ms: Option<u64>,
    items_scanned: u64,
    bytes_scanned: u64,
    completed_rules: u64,
    total_rules: u64,
    found_rules: u64,
    warning_count: u64,
    safe_bytes: u64,
    reclaimable_bytes: u64,
    applicability_elapsed_ms: u64,
    applicable_rule_count: u64,
    filtered_rule_count: u64,
    inventory_application_count: u64,
    inventory_process_count: u64,
    result_digest: String,
    filesystem_result_digest: String,
    special_result_digest: String,
    filesystem_reclaimable_bytes: u64,
    special_reclaimable_bytes: u64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct BaselineSummary {
    rule_count: u64,
    applicable_rule_count: u64,
    found_rule_count: u64,
    clean_rule_count: u64,
    not_applicable_rule_count: u64,
    requires_close_rule_count: u64,
    review_only_rule_count: u64,
    limited_rule_count: u64,
    default_selected_rule_count: u64,
    recommended_selected_rule_count: u64,
    recommended_selected_bytes: u64,
    blocked_recommended_rule_count: u64,
    blocked_recommended_bytes: u64,
    warning_count: u64,
    matched_file_count: u64,
    safe_bytes: u64,
    reclaimable_bytes: u64,
    filesystem_rule_count: u64,
    special_cleaner_count: u64,
    filesystem_reclaimable_bytes: u64,
    special_reclaimable_bytes: u64,
    inventory_application_count: u64,
    inventory_process_count: u64,
    result_consistent_across_runs: bool,
    filesystem_result_consistent_across_runs: bool,
    special_result_consistent_across_runs: bool,
    duration: DurationStatistics,
}

#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
struct CategoryBaseline {
    category: String,
    rule_count: u64,
    applicable_rule_count: u64,
    found_rule_count: u64,
    matched_file_count: u64,
    reclaimable_bytes: u64,
    sum_rule_elapsed_ms: u64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct RuleBaseline {
    rule_id: String,
    category: String,
    result_group: String,
    risk: String,
    status: String,
    default_selected: bool,
    recommended_selected: bool,
    available: bool,
    selectable: bool,
    requires_app_close: bool,
    running_process_count: usize,
    matched_file_count: u64,
    reclaimable_bytes: u64,
    scan_duration: DurationStatistics,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct CleanupBaselineReport {
    schema_version: &'static str,
    report_kind: &'static str,
    label: String,
    note: Option<String>,
    generated_at_ms: u64,
    generated_at_local: String,
    source: BenchmarkSourceInfo,
    environment: BenchmarkEnvironment,
    methodology: BaselineMethodology,
    engine: CleanupScanEngineInfo,
    rule_catalog_digest: String,
    special_cleaner_catalog_digest: String,
    cleanup_scan_schema_version: String,
    disk: DiskInfo,
    summary: BaselineSummary,
    runs: Vec<BaselineRun>,
    categories: Vec<CategoryBaseline>,
    rules: Vec<RuleBaseline>,
}

pub struct CleanupBaselineService;

impl CleanupBaselineService {
    /// Runs real cleanup scans and writes both JSON and Markdown reports. Repeated runs expose
    /// the difference between the first run and later runs in the same process. The tool does not
    /// purge operating-system file caches because doing so requires extra privileges and would
    /// disturb the real user environment.
    pub fn generate(options: CleanupBaselineOptions) -> Result<BaselineArtifacts, String> {
        validate_options(&options)?;
        let output_directory = options
            .output_directory
            .clone()
            .unwrap_or_else(|| PathBuf::from(DEFAULT_OUTPUT_DIRECTORY));
        fs::create_dir_all(&output_directory).map_err(|error| {
            format!(
                "failed to create the baseline report directory {}: {error}",
                output_directory.display()
            )
        })?;

        let generated_at_ms = now_ms();
        let generated_at_local = local_timestamp(generated_at_ms);
        let rule_catalog_digest = rules::compatibility_digest()?;
        let special_cleaner_catalog_digest = cleaners::catalog_digest();
        let mut snapshots = Vec::with_capacity(options.runs);
        let mut runs = Vec::with_capacity(options.runs);

        for run_index in 0..options.runs {
            let progress = Arc::new(Mutex::new(ProgressCapture::default()));
            let progress_callback = Arc::clone(&progress);
            let caller_started = Instant::now();
            let callback = move |event: TraversalProgress| {
                let Ok(mut capture) = progress_callback.lock() else {
                    return;
                };
                capture.event_count += 1;
                capture.first_progress_ms.get_or_insert(event.elapsed_ms);
                if event.items_scanned > 0 || event.bytes_scanned > 0 {
                    capture
                        .first_scan_observation_ms
                        .get_or_insert(event.elapsed_ms);
                }
                if event.found_bytes > 0 {
                    capture.first_match_ms.get_or_insert(event.elapsed_ms);
                }
                capture.last_progress = Some(event);
            };
            let snapshot = if options.project_roots.is_empty() {
                CleanupScanService::scan_with_deep_project_discovery(
                    options.deep_project_discovery,
                    callback,
                )
                .map_err(|error| error.to_string())?
            } else {
                CleanupScanService::scan_with_project_roots(options.project_roots.clone(), callback)
                    .map_err(|error| error.to_string())?
            };
            let caller_wall_elapsed_ms = caller_started.elapsed().as_millis() as u64;
            let capture = progress
                .lock()
                .map_err(|_| "failed to read cleanup baseline progress statistics".to_string())?
                .clone();
            let result_digest = result_digest(&snapshot);
            let filesystem_result_digest = filesystem_result_digest(&snapshot);
            let special_result_digest = special_result_digest(&snapshot);
            let filesystem_reclaimable_bytes = snapshot
                .rules
                .iter()
                .filter(|rule| rule.selectable && !cleaners::contains(&rule.rule_id))
                .map(|rule| rule.bytes)
                .sum();
            let special_reclaimable_bytes = snapshot
                .rules
                .iter()
                .filter(|rule| rule.selectable && cleaners::contains(&rule.rule_id))
                .map(|rule| rule.bytes)
                .sum();
            let last_progress = capture.last_progress.unwrap_or(TraversalProgress {
                operation_id: 0,
                current_stage: crate::shared::TraversalStage::Analyzing,
                current_path: String::new(),
                items_scanned: 0,
                bytes_scanned: 0,
                completed_steps: snapshot.rules.len() as u64,
                total_steps: snapshot.rules.len() as u64,
                found_items: snapshot.rules.iter().filter(|rule| rule.selectable).count() as u64,
                found_bytes: snapshot.reclaimable_bytes,
                elapsed_ms: snapshot.elapsed_ms,
            });
            runs.push(BaselineRun {
                run_number: run_index + 1,
                scan_elapsed_ms: snapshot.elapsed_ms,
                caller_wall_elapsed_ms,
                progress_event_count: capture.event_count,
                first_progress_ms: capture.first_progress_ms,
                first_scan_observation_ms: capture.first_scan_observation_ms,
                first_match_ms: capture.first_match_ms,
                items_scanned: last_progress.items_scanned,
                bytes_scanned: last_progress.bytes_scanned,
                // Progress events are throttled to protect downstream consumers, so the last
                // event is not guaranteed to coincide with scan completion. Final rule counts
                // must come from the completed snapshot; otherwise a fast scan can be recorded as
                // 30/31 and make later report-completeness checks unreliable.
                completed_rules: snapshot.rules.len() as u64,
                total_rules: snapshot.rules.len() as u64,
                found_rules: snapshot.rules.iter().filter(|rule| rule.selectable).count() as u64,
                warning_count: snapshot.warning_count,
                safe_bytes: snapshot.safe_bytes,
                reclaimable_bytes: snapshot.reclaimable_bytes,
                applicability_elapsed_ms: snapshot.applicability_elapsed_ms,
                applicable_rule_count: snapshot.applicable_rule_count,
                filtered_rule_count: snapshot.filtered_rule_count,
                inventory_application_count: snapshot.inventory_application_count,
                inventory_process_count: snapshot.inventory_process_count,
                result_digest,
                filesystem_result_digest,
                special_result_digest,
                filesystem_reclaimable_bytes,
                special_reclaimable_bytes,
            });
            snapshots.push(snapshot);
        }

        let representative = snapshots
            .last()
            .ok_or_else(|| "the baseline scan produced no results".to_string())?;
        let disk = representative.disk.clone();
        let cleanup_scan_schema_version = representative.schema_version.clone();
        let report = build_report(
            options,
            generated_at_ms,
            generated_at_local,
            snapshots,
            runs,
            disk,
            rule_catalog_digest,
            special_cleaner_catalog_digest,
            cleanup_scan_schema_version,
        );
        let file_stem = format!(
            "deep-clean-{}-{}-{}",
            std::env::consts::OS,
            sanitize_label(&report.label),
            generated_at_ms
        );
        let json_path = output_directory.join(format!("{file_stem}.json"));
        let markdown_path = output_directory.join(format!("{file_stem}.md"));
        let json = serde_json::to_string_pretty(&report)
            .map_err(|error| format!("failed to serialize baseline JSON: {error}"))?;
        let markdown = render_markdown(&report);
        write_atomic(&json_path, format!("{json}\n").as_bytes())?;
        write_atomic(&markdown_path, markdown.as_bytes())?;

        log::info!(
            "cleanup_baseline_generated label={} runs={} json_file={} markdown_file={}",
            report.label,
            report.runs.len(),
            file_name_for_log(&json_path),
            file_name_for_log(&markdown_path)
        );
        Ok(BaselineArtifacts {
            json_path,
            markdown_path,
        })
    }
}

fn validate_options(options: &CleanupBaselineOptions) -> Result<(), String> {
    if options.runs == 0 || options.runs > MAX_BASELINE_RUNS {
        return Err(format!(
            "baseline run count must be between 1 and {MAX_BASELINE_RUNS}"
        ));
    }
    if options.label.trim().is_empty() {
        return Err("baseline label must not be empty".to_string());
    }
    if !options
        .label
        .chars()
        .all(|character| character.is_ascii_alphanumeric() || matches!(character, '-' | '_' | '.'))
    {
        return Err(
            "baseline label may contain only ASCII letters, digits, dots, hyphens, and underscores"
                .to_string(),
        );
    }
    if options
        .environment_id
        .as_ref()
        .is_some_and(|environment_id| {
            environment_id.trim().is_empty()
                || !environment_id.chars().all(|character| {
                    character.is_ascii_alphanumeric() || matches!(character, '-' | '_' | '.')
                })
        })
    {
        return Err(
            "environment ID may contain only ASCII letters, digits, dots, hyphens, and underscores"
                .to_string(),
        );
    }
    if options
        .note
        .as_ref()
        .is_some_and(|note| note.chars().count() > MAX_NOTE_CHARACTERS)
    {
        return Err(format!(
            "baseline note must not exceed {MAX_NOTE_CHARACTERS} characters"
        ));
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn build_report(
    options: CleanupBaselineOptions,
    generated_at_ms: u64,
    generated_at_local: String,
    snapshots: Vec<CleanupScanResult>,
    runs: Vec<BaselineRun>,
    disk: DiskInfo,
    rule_catalog_digest: String,
    special_cleaner_catalog_digest: String,
    cleanup_scan_schema_version: String,
) -> CleanupBaselineReport {
    let representative = snapshots
        .last()
        .expect("the caller guarantees at least one scan result");
    let duration = duration_statistics(
        &runs
            .iter()
            .map(|run| run.scan_elapsed_ms)
            .collect::<Vec<_>>(),
    );
    let first_digest = runs
        .first()
        .map(|run| run.result_digest.as_str())
        .unwrap_or_default();
    let result_consistent_across_runs = runs
        .iter()
        .all(|run| run.result_digest.as_str() == first_digest);
    let first_filesystem_digest = runs
        .first()
        .map(|run| run.filesystem_result_digest.as_str())
        .unwrap_or_default();
    let filesystem_result_consistent_across_runs = runs
        .iter()
        .all(|run| run.filesystem_result_digest == first_filesystem_digest);
    let first_special_digest = runs
        .first()
        .map(|run| run.special_result_digest.as_str())
        .unwrap_or_default();
    let special_result_consistent_across_runs = runs
        .iter()
        .all(|run| run.special_result_digest == first_special_digest);
    let rules = build_rule_baselines(&snapshots);
    let categories = build_category_baselines(&rules);
    let summary = BaselineSummary {
        rule_count: representative.rules.len() as u64,
        applicable_rule_count: representative
            .rules
            .iter()
            .filter(|rule| rule.available)
            .count() as u64,
        found_rule_count: representative
            .rules
            .iter()
            .filter(|rule| rule.selectable)
            .count() as u64,
        clean_rule_count: count_status(&representative.rules, ScanItemStatus::Clean),
        not_applicable_rule_count: count_status(
            &representative.rules,
            ScanItemStatus::NotApplicable,
        ),
        requires_close_rule_count: count_status(
            &representative.rules,
            ScanItemStatus::RequiresClose,
        ),
        review_only_rule_count: count_status(&representative.rules, ScanItemStatus::ReviewOnly),
        limited_rule_count: count_status(&representative.rules, ScanItemStatus::Limited),
        default_selected_rule_count: representative
            .rules
            .iter()
            .filter(|rule| rule.default_selected && rule.selectable)
            .count() as u64,
        recommended_selected_rule_count: representative
            .rules
            .iter()
            .filter(|rule| rule.recommended_selected && rule.selectable)
            .count() as u64,
        recommended_selected_bytes: representative
            .rules
            .iter()
            .filter(|rule| rule.recommended_selected && rule.selectable)
            .map(|rule| rule.bytes)
            .sum(),
        blocked_recommended_rule_count: representative
            .rules
            .iter()
            .filter(|rule| {
                rule.recommended_selected
                    && rule.selectable
                    && rule.status == ScanItemStatus::RequiresClose
            })
            .count() as u64,
        blocked_recommended_bytes: representative
            .rules
            .iter()
            .filter(|rule| {
                rule.recommended_selected
                    && rule.selectable
                    && rule.status == ScanItemStatus::RequiresClose
            })
            .map(|rule| rule.bytes)
            .sum(),
        warning_count: representative.warning_count,
        matched_file_count: representative
            .rules
            .iter()
            .map(|rule| rule.file_count)
            .sum(),
        safe_bytes: representative.safe_bytes,
        reclaimable_bytes: representative.reclaimable_bytes,
        filesystem_rule_count: representative
            .rules
            .iter()
            .filter(|rule| !cleaners::contains(&rule.rule_id))
            .count() as u64,
        special_cleaner_count: representative
            .rules
            .iter()
            .filter(|rule| cleaners::contains(&rule.rule_id))
            .count() as u64,
        filesystem_reclaimable_bytes: runs
            .last()
            .map_or(0, |run| run.filesystem_reclaimable_bytes),
        special_reclaimable_bytes: runs.last().map_or(0, |run| run.special_reclaimable_bytes),
        inventory_application_count: representative.inventory_application_count,
        inventory_process_count: representative.inventory_process_count,
        result_consistent_across_runs,
        filesystem_result_consistent_across_runs,
        special_result_consistent_across_runs,
        duration,
    };

    let environment = environment_info(options.environment_id.as_deref(), &disk);
    CleanupBaselineReport {
        schema_version: BASELINE_SCHEMA_VERSION,
        report_kind: BASELINE_REPORT_KIND,
        label: options.label,
        note: options.note,
        generated_at_ms,
        generated_at_local,
        source: options.source,
        environment,
        methodology: BaselineMethodology {
            scan_only: true,
            cleanup_executed: false,
            first_run_os_cache_state_known: false,
            repeat_runs_may_use_os_cache: true,
            project_root_count: options.project_roots.len(),
            deep_project_discovery: options.deep_project_discovery,
        },
        engine: CleanupScanService::engine_info(),
        rule_catalog_digest,
        special_cleaner_catalog_digest,
        cleanup_scan_schema_version,
        disk,
        summary,
        runs,
        categories,
        rules,
    }
}

fn build_rule_baselines(snapshots: &[CleanupScanResult]) -> Vec<RuleBaseline> {
    let mut durations = HashMap::<&str, Vec<u64>>::new();
    for snapshot in snapshots {
        for rule in &snapshot.rules {
            durations
                .entry(rule.rule_id.as_str())
                .or_default()
                .push(rule.scan_elapsed_ms);
        }
    }
    let representative = snapshots
        .last()
        .expect("the caller guarantees at least one scan result");
    let mut rules = representative
        .rules
        .iter()
        .map(|rule| RuleBaseline {
            rule_id: rule.rule_id.clone(),
            category: rule.category.as_str().to_string(),
            result_group: rule.group.as_str().to_string(),
            risk: risk_name(rule.risk).to_string(),
            status: status_name(rule.status).to_string(),
            default_selected: rule.default_selected,
            recommended_selected: rule.recommended_selected,
            available: rule.available,
            selectable: rule.selectable,
            requires_app_close: rule.requires_app_close,
            running_process_count: rule.running_processes.len(),
            matched_file_count: rule.file_count,
            reclaimable_bytes: if rule.selectable { rule.bytes } else { 0 },
            scan_duration: duration_statistics(
                durations
                    .get(rule.rule_id.as_str())
                    .map(Vec::as_slice)
                    .unwrap_or(&[rule.scan_elapsed_ms]),
            ),
        })
        .collect::<Vec<_>>();
    rules.sort_by(|left, right| {
        left.category
            .cmp(&right.category)
            .then_with(|| right.reclaimable_bytes.cmp(&left.reclaimable_bytes))
            .then_with(|| left.rule_id.cmp(&right.rule_id))
    });
    rules
}

fn build_category_baselines(rules: &[RuleBaseline]) -> Vec<CategoryBaseline> {
    let mut categories = BTreeMap::<String, CategoryBaseline>::new();
    for rule in rules {
        let category = categories
            .entry(rule.category.as_str().to_string())
            .or_insert_with(|| CategoryBaseline {
                category: rule.category.as_str().to_string(),
                ..CategoryBaseline::default()
            });
        category.rule_count += 1;
        category.applicable_rule_count += u64::from(rule.available);
        category.found_rule_count += u64::from(rule.selectable);
        category.matched_file_count += rule.matched_file_count;
        category.reclaimable_bytes += rule.reclaimable_bytes;
        category.sum_rule_elapsed_ms += rule.scan_duration.median_ms;
    }
    categories.into_values().collect()
}

fn count_status(rules: &[ScanRuleResult], status: ScanItemStatus) -> u64 {
    rules.iter().filter(|rule| rule.status == status).count() as u64
}

fn duration_statistics(values: &[u64]) -> DurationStatistics {
    let mut sorted = values.to_vec();
    sorted.sort_unstable();
    let first_run_ms = values.first().copied().unwrap_or(0);
    let minimum_ms = sorted.first().copied().unwrap_or(0);
    let maximum_ms = sorted.last().copied().unwrap_or(0);
    let median_ms = median(&sorted);
    let mean_ms = if values.is_empty() {
        0
    } else {
        values.iter().sum::<u64>() / values.len() as u64
    };
    let repeated_run_median_ms = (values.len() > 1).then(|| {
        let mut repeated = values[1..].to_vec();
        repeated.sort_unstable();
        median(&repeated)
    });
    DurationStatistics {
        first_run_ms,
        minimum_ms,
        median_ms,
        maximum_ms,
        mean_ms,
        repeated_run_median_ms,
    }
}

fn median(sorted: &[u64]) -> u64 {
    if sorted.is_empty() {
        return 0;
    }
    let middle = sorted.len() / 2;
    if sorted.len().is_multiple_of(2) {
        sorted[middle - 1].saturating_add(sorted[middle]) / 2
    } else {
        sorted[middle]
    }
}

/// Excludes non-domain fields such as elapsed time and progress from the result digest. Stable
/// digests across repeated scans prove that rule concurrency and cache state did not change rule
/// matches during the baseline run.
fn result_digest(snapshot: &CleanupScanResult) -> String {
    result_digest_filtered(snapshot, |_| true)
}

fn filesystem_result_digest(snapshot: &CleanupScanResult) -> String {
    result_digest_filtered(snapshot, |rule| !cleaners::contains(&rule.rule_id))
}

fn special_result_digest(snapshot: &CleanupScanResult) -> String {
    result_digest_filtered(snapshot, |rule| cleaners::contains(&rule.rule_id))
}

fn result_digest_filtered(
    snapshot: &CleanupScanResult,
    include: impl Fn(&ScanRuleResult) -> bool,
) -> String {
    let mut rules = snapshot
        .rules
        .iter()
        .filter(|rule| include(rule))
        .collect::<Vec<_>>();
    rules.sort_by(|left, right| left.rule_id.cmp(&right.rule_id));
    let mut hasher = blake3::Hasher::new();
    hasher.update(snapshot.schema_version.as_bytes());
    for rule in rules {
        hasher.update(rule.rule_id.as_bytes());
        hasher.update(rule.category.as_str().as_bytes());
        hasher.update(risk_name(rule.risk).as_bytes());
        hasher.update(status_name(rule.status).as_bytes());
        hasher.update(&rule.bytes.to_le_bytes());
        hasher.update(&rule.file_count.to_le_bytes());
        hasher.update(&[u8::from(rule.available)]);
        hasher.update(&[u8::from(rule.selectable)]);
        for process in &rule.running_processes {
            hasher.update(process.as_bytes());
        }
    }
    hasher.finalize().to_hex().to_string()
}

fn render_markdown(report: &CleanupBaselineReport) -> String {
    let mut markdown = String::new();
    push_line(&mut markdown, "# MangoDisk Deep-Clean Baseline");
    push_line(&mut markdown, "");
    push_line(
        &mut markdown,
        "> Generated by the MangoDisk repository tooling from a real deep scan. No cleanup was executed.",
    );
    push_line(&mut markdown, "");
    push_line(&mut markdown, "## Report");
    push_line(&mut markdown, "");
    push_line(&mut markdown, "| Item | Value |");
    push_line(&mut markdown, "|------|------|");
    push_table_row(&mut markdown, "Label", &report.label);
    push_table_row(&mut markdown, "Generated", &report.generated_at_local);
    push_table_row(
        &mut markdown,
        "Application version",
        &report.source.application_version,
    );
    push_table_row(&mut markdown, "Source commit", &report.source.source_commit);
    push_table_row(
        &mut markdown,
        "Build worktree",
        if report.source.source_dirty_at_build {
            "dirty"
        } else {
            "clean"
        },
    );
    push_table_row(&mut markdown, "Build profile", &report.source.build_profile);
    push_table_row(
        &mut markdown,
        "Project scan roots",
        &if report.methodology.project_root_count == 0 {
            "auto-discovered".to_string()
        } else {
            report.methodology.project_root_count.to_string()
        },
    );
    push_table_row(
        &mut markdown,
        "Project discovery mode",
        if report.methodology.project_root_count > 0 {
            "explicit roots"
        } else if report.methodology.deep_project_discovery {
            "all local disks"
        } else {
            "standard scan"
        },
    );
    if let Some(note) = &report.note {
        push_table_row(&mut markdown, "Notes", note);
    }

    push_line(&mut markdown, "");
    push_line(&mut markdown, "## Environment");
    push_line(&mut markdown, "");
    push_line(&mut markdown, "| Item | Value |");
    push_line(&mut markdown, "|------|------|");
    push_table_row(
        &mut markdown,
        "Environment ID",
        &report.environment.environment_id,
    );
    push_table_row(
        &mut markdown,
        "Execution user",
        &report.environment.user_identity,
    );
    push_table_row(&mut markdown, "Operating system", &report.environment.os);
    push_table_row(&mut markdown, "OS version", &report.environment.os_version);
    push_table_row(
        &mut markdown,
        "Architecture",
        &report.environment.architecture,
    );
    push_table_row(&mut markdown, "CPU", &report.environment.cpu_model);
    push_table_row(
        &mut markdown,
        "Logical CPUs",
        &report.environment.logical_cpu_count.to_string(),
    );
    push_table_row(
        &mut markdown,
        "Physical memory",
        &report
            .environment
            .physical_memory_bytes
            .map(format_bytes)
            .unwrap_or_else(|| "unknown".to_string()),
    );
    push_table_row(&mut markdown, "Disk", &report.disk.name);
    push_table_row(&mut markdown, "Scanned volume", &report.disk.mount_point);
    push_table_row(
        &mut markdown,
        "Disk capacity",
        &format_bytes(report.disk.total_bytes),
    );
    push_table_row(
        &mut markdown,
        "Disk used",
        &format_bytes(report.disk.used_bytes),
    );
    push_table_row(
        &mut markdown,
        "Disk available",
        &format_bytes(report.disk.available_bytes),
    );

    push_line(&mut markdown, "");
    push_line(&mut markdown, "## Scan engine");
    push_line(&mut markdown, "");
    push_line(&mut markdown, "| Item | Value |");
    push_line(&mut markdown, "|------|------|");
    push_table_row(&mut markdown, "Strategy", report.engine.strategy);
    push_table_row(
        &mut markdown,
        "Rule catalog",
        report.engine.rule_catalog_mode,
    );
    push_table_row(
        &mut markdown,
        "Maximum workers",
        &report.engine.configured_worker_limit.to_string(),
    );
    push_table_row(
        &mut markdown,
        "Rule catalog digest",
        &report.rule_catalog_digest,
    );
    push_table_row(
        &mut markdown,
        "Specialized cleaner digest",
        &report.special_cleaner_catalog_digest,
    );
    push_table_row(
        &mut markdown,
        "Scan-result persistence",
        boolean_text(report.engine.scan_result_persistence_enabled),
    );
    push_table_row(
        &mut markdown,
        "Multi-rule traversal",
        boolean_text(report.engine.single_pass_rule_matching),
    );
    push_table_row(
        &mut markdown,
        "Incremental scan",
        boolean_text(report.engine.incremental_scan_enabled),
    );

    push_line(&mut markdown, "");
    push_line(&mut markdown, "## Baseline summary");
    push_line(&mut markdown, "");
    push_line(&mut markdown, "| Metric | Result |");
    push_line(&mut markdown, "|------|------|");
    push_table_row(&mut markdown, "Scan runs", &report.runs.len().to_string());
    push_table_row(
        &mut markdown,
        "First scan",
        &format_duration(report.summary.duration.first_run_ms),
    );
    push_table_row(
        &mut markdown,
        "Median duration",
        &format_duration(report.summary.duration.median_ms),
    );
    push_table_row(
        &mut markdown,
        "Repeated-run median",
        &report
            .summary
            .duration
            .repeated_run_median_ms
            .map(format_duration)
            .unwrap_or_else(|| "not run".to_string()),
    );
    push_table_row(
        &mut markdown,
        "Results consistent",
        boolean_text(report.summary.result_consistent_across_runs),
    );
    push_table_row(
        &mut markdown,
        "Filesystem-rule results consistent",
        boolean_text(report.summary.filesystem_result_consistent_across_runs),
    );
    push_table_row(
        &mut markdown,
        "Specialized-cleaner results consistent",
        boolean_text(report.summary.special_result_consistent_across_runs),
    );
    push_table_row(
        &mut markdown,
        "Total rules",
        &report.summary.rule_count.to_string(),
    );
    push_table_row(
        &mut markdown,
        "Applicable rules",
        &report.summary.applicable_rule_count.to_string(),
    );
    push_table_row(
        &mut markdown,
        "Application inventory",
        &report.summary.inventory_application_count.to_string(),
    );
    push_table_row(
        &mut markdown,
        "Process snapshot",
        &report.summary.inventory_process_count.to_string(),
    );
    push_table_row(
        &mut markdown,
        "Rules with cleanup candidates",
        &report.summary.found_rule_count.to_string(),
    );
    push_table_row(
        &mut markdown,
        "Default-selected rules",
        &report.summary.default_selected_rule_count.to_string(),
    );
    push_table_row(
        &mut markdown,
        "Initial interactive selection",
        &format!(
            "{} · {}",
            report.summary.recommended_selected_rule_count,
            format_bytes(report.summary.recommended_selected_bytes)
        ),
    );
    push_table_row(
        &mut markdown,
        "Requiring stopped applications",
        &format!(
            "{} · {}",
            report.summary.blocked_recommended_rule_count,
            format_bytes(report.summary.blocked_recommended_bytes)
        ),
    );
    push_table_row(
        &mut markdown,
        "Rules without candidates",
        &report.summary.clean_rule_count.to_string(),
    );
    push_table_row(
        &mut markdown,
        "Rules not applicable",
        &report.summary.not_applicable_rule_count.to_string(),
    );
    push_table_row(
        &mut markdown,
        "Rules requiring stopped applications",
        &report.summary.requires_close_rule_count.to_string(),
    );
    push_table_row(
        &mut markdown,
        "Read-only rules",
        &report.summary.review_only_rule_count.to_string(),
    );
    push_table_row(
        &mut markdown,
        "Permission-limited rules",
        &report.summary.limited_rule_count.to_string(),
    );
    push_table_row(
        &mut markdown,
        "Matched files",
        &report.summary.matched_file_count.to_string(),
    );
    push_table_row(
        &mut markdown,
        "Low-risk reclaimable",
        &format_bytes(report.summary.safe_bytes),
    );
    push_table_row(
        &mut markdown,
        "Total reclaimable",
        &format_bytes(report.summary.reclaimable_bytes),
    );
    push_table_row(
        &mut markdown,
        "Filesystem rules / reclaimable",
        &format!(
            "{} / {}",
            report.summary.filesystem_rule_count,
            format_bytes(report.summary.filesystem_reclaimable_bytes)
        ),
    );
    push_table_row(
        &mut markdown,
        "Specialized cleaners / reclaimable",
        &format!(
            "{} / {}",
            report.summary.special_cleaner_count,
            format_bytes(report.summary.special_reclaimable_bytes)
        ),
    );
    push_table_row(
        &mut markdown,
        "Skipped or warned",
        &report.summary.warning_count.to_string(),
    );

    push_line(&mut markdown, "");
    push_line(&mut markdown, "## Run performance");
    push_line(&mut markdown, "");
    push_line(
        &mut markdown,
        "| Run | Scan | Applicability | Applicable / filtered | Caller wall time | First progress | First scan data | First match | Events | Files checked | Bytes traversed | Digest |",
    );
    push_line(
        &mut markdown,
        "|------|----------|------------|-------------|------------|----------|----------|----------|----------|----------|----------|------|",
    );
    for run in &report.runs {
        push_line(
            &mut markdown,
            &format!(
                "| {} | {} | {} | {} / {} | {} | {} | {} | {} | {} | {} | {} | `{}` |",
                run.run_number,
                format_duration(run.scan_elapsed_ms),
                format_duration(run.applicability_elapsed_ms),
                run.applicable_rule_count,
                run.filtered_rule_count,
                format_duration(run.caller_wall_elapsed_ms),
                optional_duration(run.first_progress_ms),
                optional_duration(run.first_scan_observation_ms),
                optional_duration(run.first_match_ms),
                run.progress_event_count,
                run.items_scanned,
                format_bytes(run.bytes_scanned),
                &run.result_digest[..12],
            ),
        );
    }

    push_line(&mut markdown, "");
    push_line(&mut markdown, "### Partition result digests");
    push_line(&mut markdown, "");
    push_line(
        &mut markdown,
        "| Run | Filesystem reclaimable | Filesystem digest | Specialized reclaimable | Specialized digest |",
    );
    push_line(
        &mut markdown,
        "|------|----------------|--------------|------------|----------|",
    );
    for run in &report.runs {
        push_line(
            &mut markdown,
            &format!(
                "| {} | {} | `{}` | {} | `{}` |",
                run.run_number,
                format_bytes(run.filesystem_reclaimable_bytes),
                &run.filesystem_result_digest[..12],
                format_bytes(run.special_reclaimable_bytes),
                &run.special_result_digest[..12],
            ),
        );
    }

    push_line(&mut markdown, "");
    push_line(&mut markdown, "## Category results");
    push_line(&mut markdown, "");
    push_line(
        &mut markdown,
        "| Category | Rules | Applicable | Matched | Files | Reclaimable | Sum of rule medians |",
    );
    push_line(
        &mut markdown,
        "|------|------|------|------|------|--------|--------------------|",
    );
    for category in &report.categories {
        push_line(
            &mut markdown,
            &format!(
                "| {} | {} | {} | {} | {} | {} | {} |",
                escape_markdown(&category.category),
                category.rule_count,
                category.applicable_rule_count,
                category.found_rule_count,
                category.matched_file_count,
                format_bytes(category.reclaimable_bytes),
                format_duration(category.sum_rule_elapsed_ms),
            ),
        );
    }
    push_line(&mut markdown, "");
    push_line(
        &mut markdown,
        "> Rules run concurrently. The sum of rule medians compares work within a category and is not scan wall time.",
    );

    let mut top_rules = report.rules.iter().collect::<Vec<_>>();
    top_rules.sort_by(|left, right| {
        right
            .reclaimable_bytes
            .cmp(&left.reclaimable_bytes)
            .then_with(|| left.rule_id.cmp(&right.rule_id))
    });
    push_line(&mut markdown, "");
    push_line(&mut markdown, "## Highest-yield rules");
    push_line(&mut markdown, "");
    push_line(
        &mut markdown,
        "| Rule | Category | Risk | Files | Reclaimable | Median duration |",
    );
    push_line(
        &mut markdown,
        "|------|------|------|------|--------|----------|",
    );
    for rule in top_rules.into_iter().take(15) {
        push_line(
            &mut markdown,
            &format!(
                "| {} | {} | {} | {} | {} | {} |",
                escape_markdown(&rule.rule_id),
                escape_markdown(&rule.category),
                rule.risk,
                rule.matched_file_count,
                format_bytes(rule.reclaimable_bytes),
                format_duration(rule.scan_duration.median_ms),
            ),
        );
    }

    let mut slow_rules = report.rules.iter().collect::<Vec<_>>();
    slow_rules.sort_by(|left, right| {
        right
            .scan_duration
            .median_ms
            .cmp(&left.scan_duration.median_ms)
            .then_with(|| left.rule_id.cmp(&right.rule_id))
    });
    push_line(&mut markdown, "");
    push_line(&mut markdown, "## Slowest rules");
    push_line(&mut markdown, "");
    push_line(
        &mut markdown,
        "| Rule | Category | First run | Median | Matched files | Reclaimable |",
    );
    push_line(
        &mut markdown,
        "|------|------|----------|----------|----------|--------|",
    );
    for rule in slow_rules.into_iter().take(15) {
        push_line(
            &mut markdown,
            &format!(
                "| {} | {} | {} | {} | {} | {} |",
                escape_markdown(&rule.rule_id),
                escape_markdown(&rule.category),
                format_duration(rule.scan_duration.first_run_ms),
                format_duration(rule.scan_duration.median_ms),
                rule.matched_file_count,
                format_bytes(rule.reclaimable_bytes),
            ),
        );
    }

    push_line(&mut markdown, "");
    push_line(&mut markdown, "## Complete rule baseline");
    push_line(&mut markdown, "");
    push_line(
        &mut markdown,
        "| Rule ID | Category | Risk | Status | Auto-selected | Recommended | Files | Reclaimable | First run | Median |",
    );
    push_line(
        &mut markdown,
        "|---------|------|------|------|----------|----------|------|--------|----------|----------|",
    );
    for rule in &report.rules {
        push_line(
            &mut markdown,
            &format!(
                "| `{}` | {} | {} | {} | {} | {} | {} | {} | {} | {} |",
                escape_markdown(&rule.rule_id),
                escape_markdown(&rule.category),
                rule.risk,
                rule.status,
                boolean_text(rule.default_selected && rule.selectable),
                boolean_text(rule.recommended_selected && rule.selectable),
                rule.matched_file_count,
                format_bytes(rule.reclaimable_bytes),
                format_duration(rule.scan_duration.first_run_ms),
                format_duration(rule.scan_duration.median_ms),
            ),
        );
    }

    push_line(&mut markdown, "");
    push_line(&mut markdown, "## Measurement notes");
    push_line(&mut markdown, "");
    push_line(
        &mut markdown,
        "- This report scans and measures without cleaning. Reclaimable bytes are estimates, not measured released space.",
    );
    push_line(
        &mut markdown,
        "- The harness cannot control operating-system file caches. The first run is not a strict cold-cache run.",
    );
    push_line(
        &mut markdown,
        "- Files, processes, permissions, and free space may change between runs. Consistency checks expose those changes.",
    );
    push_line(
        &mut markdown,
        "- Compare revisions on the same machine, user, disk contents, build profile, run count, and label convention.",
    );
    push_line(
        &mut markdown,
        "- JSON is the stable source for automation. Markdown is for human review.",
    );
    push_line(&mut markdown, "");
    push_line(&mut markdown, "## Reproduction");
    push_line(&mut markdown, "");
    push_line(&mut markdown, "Development environment:");
    push_line(&mut markdown, "");
    push_line(&mut markdown, "```sh");
    push_line(
        &mut markdown,
        &format!(
            "pnpm baseline:cleanup -- --label {} --runs {}{}{}",
            report.label,
            report.runs.len(),
            if report.methodology.project_root_count > 0 {
                " --project-root <same-project-root>"
            } else {
                ""
            },
            if report.methodology.deep_project_discovery {
                " --deep-project-discovery"
            } else {
                ""
            }
        ),
    );
    push_line(&mut markdown, "```");
    push_line(&mut markdown, "");
    push_line(
        &mut markdown,
        "Repository maintainers generate reports through the standalone `xtask`; the Tauri GUI does not accept baseline arguments.",
    );
    markdown
}

fn push_line(target: &mut String, value: &str) {
    target.push_str(value);
    target.push('\n');
}

fn push_table_row(target: &mut String, key: &str, value: &str) {
    push_line(
        target,
        &format!("| {} | {} |", escape_markdown(key), escape_markdown(value)),
    );
}

fn escape_markdown(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace('|', "\\|")
        .replace(['\r', '\n'], " ")
}

fn boolean_text(value: bool) -> &'static str {
    if value {
        "yes"
    } else {
        "no"
    }
}

fn optional_duration(value: Option<u64>) -> String {
    value
        .map(format_duration)
        .unwrap_or_else(|| "no matches".to_string())
}

fn format_duration(milliseconds: u64) -> String {
    if milliseconds < 1_000 {
        format!("{milliseconds} ms")
    } else {
        format!("{:.2} s", milliseconds as f64 / 1_000.0)
    }
}

fn format_bytes(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{value:.2} {}", UNITS[unit])
    }
}

fn sanitize_label(label: &str) -> String {
    label
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || matches!(character, '-' | '_' | '.') {
                character
            } else {
                '-'
            }
        })
        .collect()
}

/// Keeps only the report file name in logs. The CLI explicitly returns the full output path to the
/// operator, but persisting that path in diagnostic logs would unnecessarily retain the user name
/// and local directory layout.
fn file_name_for_log(path: &Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| "unknown".to_string())
}

fn write_atomic(path: &Path, contents: &[u8]) -> Result<(), String> {
    let temporary = path.with_extension(format!(
        "{}.tmp",
        path.extension()
            .and_then(|extension| extension.to_str())
            .unwrap_or("report")
    ));
    fs::write(&temporary, contents).map_err(|error| {
        format!(
            "failed to write the temporary report {}: {error}",
            temporary.display()
        )
    })?;
    fs::rename(&temporary, path).map_err(|error| {
        let _ = fs::remove_file(&temporary);
        format!(
            "failed to save the baseline report {}: {error}",
            path.display()
        )
    })
}

fn risk_name(risk: RiskLevel) -> &'static str {
    match risk {
        RiskLevel::Safe => "safe",
        RiskLevel::Recoverable => "recoverable",
    }
}

fn status_name(status: ScanItemStatus) -> &'static str {
    match status {
        ScanItemStatus::Found => "found",
        ScanItemStatus::Clean => "clean",
        ScanItemStatus::NotApplicable => "notApplicable",
        ScanItemStatus::RequiresClose => "requiresClose",
        ScanItemStatus::ReviewOnly => "reviewOnly",
        ScanItemStatus::Limited => "limited",
    }
}

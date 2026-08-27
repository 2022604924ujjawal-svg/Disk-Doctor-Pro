use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
    process,
    time::{SystemTime, UNIX_EPOCH},
};

use serde::Deserialize;

const SUPPORTED_REPORT_KIND: &str = "deep-clean";
const MINIMUM_COMPARABLE_RUNS: usize = 3;
const PERFORMANCE_REGRESSION_PERCENT: f64 = 10.0;
const MAXIMUM_REPEATED_RUN_SPREAD_PERCENT: f64 = 25.0;

#[derive(Debug, Clone)]
pub struct BaselineComparisonOptions {
    pub baseline_path: PathBuf,
    pub candidate_path: PathBuf,
    pub output_path: PathBuf,
}

#[derive(Debug, Clone)]
pub struct BaselineComparisonArtifacts {
    pub markdown_path: PathBuf,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ComparableReport {
    schema_version: String,
    report_kind: String,
    label: String,
    source: ComparableSource,
    environment: ComparableEnvironment,
    engine: ComparableEngine,
    #[serde(default)]
    rule_catalog_digest: Option<String>,
    #[serde(default)]
    special_cleaner_catalog_digest: Option<String>,
    #[serde(alias = "scanSnapshotSchemaVersion")]
    cleanup_scan_schema_version: String,
    disk: ComparableDisk,
    summary: ComparableSummary,
    runs: Vec<ComparableRun>,
    rules: Vec<ComparableRule>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ComparableSource {
    application_version: String,
    source_commit: String,
    source_dirty_at_build: bool,
    build_profile: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ComparableEnvironment {
    #[serde(default)]
    environment_id: Option<String>,
    #[serde(default)]
    user_identity: Option<String>,
    os: String,
    architecture: String,
    os_version: String,
    cpu_model: String,
    logical_cpu_count: usize,
    physical_memory_bytes: Option<u64>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ComparableEngine {
    strategy: String,
    rule_catalog_mode: String,
    configured_worker_limit: usize,
    scan_result_persistence_enabled: bool,
    single_pass_rule_matching: bool,
    incremental_scan_enabled: bool,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ComparableDisk {
    mount_point: String,
    total_bytes: u64,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ComparableSummary {
    rule_count: u64,
    applicable_rule_count: u64,
    found_rule_count: u64,
    clean_rule_count: u64,
    not_applicable_rule_count: u64,
    requires_close_rule_count: u64,
    #[serde(default)]
    review_only_rule_count: u64,
    limited_rule_count: u64,
    default_selected_rule_count: u64,
    warning_count: u64,
    matched_file_count: u64,
    safe_bytes: u64,
    reclaimable_bytes: u64,
    #[serde(default)]
    filesystem_rule_count: Option<u64>,
    #[serde(default)]
    special_cleaner_count: Option<u64>,
    #[serde(default)]
    filesystem_reclaimable_bytes: Option<u64>,
    #[serde(default)]
    special_reclaimable_bytes: Option<u64>,
    result_consistent_across_runs: bool,
    duration: ComparableDuration,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ComparableDuration {
    first_run_ms: u64,
    median_ms: u64,
    repeated_run_median_ms: Option<u64>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ComparableRun {
    scan_elapsed_ms: u64,
    first_progress_ms: Option<u64>,
    #[serde(default)]
    first_scan_observation_ms: Option<u64>,
    first_match_ms: Option<u64>,
    progress_event_count: u64,
    items_scanned: u64,
    bytes_scanned: u64,
    result_digest: String,
    #[serde(default)]
    filesystem_result_digest: Option<String>,
    #[serde(default)]
    special_result_digest: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ComparableRule {
    rule_id: String,
    category: String,
    risk: String,
    status: String,
    default_selected: bool,
    requires_app_close: bool,
    matched_file_count: u64,
    reclaimable_bytes: u64,
    scan_duration: ComparableDuration,
}

#[derive(Debug)]
struct Compatibility {
    comparable: bool,
    reasons: Vec<String>,
}

#[derive(Debug)]
struct RuleChanges<'a> {
    added: Vec<&'a ComparableRule>,
    removed: Vec<&'a ComparableRule>,
    definition_changed: Vec<(&'a ComparableRule, &'a ComparableRule)>,
    result_changed: Vec<(&'a ComparableRule, &'a ComparableRule)>,
}

pub struct CleanupBaselineComparisonService;

impl CleanupBaselineComparisonService {
    /// Accepts only longitudinal data from the same environment. Raw values from different
    /// machines still appear in the report, but are explicitly marked as non-comparable so CPU,
    /// disk, or user-data differences are not attributed to scan-engine changes.
    pub fn compare(
        options: BaselineComparisonOptions,
    ) -> Result<BaselineComparisonArtifacts, String> {
        if paths_refer_to_same_file(&options.baseline_path, &options.candidate_path) {
            return Err("baseline and candidate reports must refer to different files".to_string());
        }
        if paths_refer_to_same_file(&options.output_path, &options.baseline_path)
            || paths_refer_to_same_file(&options.output_path, &options.candidate_path)
        {
            return Err("the comparison report must not overwrite an input report".to_string());
        }
        let baseline = read_report(&options.baseline_path)?;
        let candidate = read_report(&options.candidate_path)?;
        let compatibility = assess_compatibility(&baseline, &candidate);
        let markdown = render_comparison(&baseline, &candidate, &compatibility);
        write_atomic(&options.output_path, markdown.as_bytes())?;
        log::info!(
            "cleanup_baseline_compared baseline_label={} candidate_label={} comparable={} output_file={}",
            baseline.label,
            candidate.label,
            compatibility.comparable,
            file_name_for_log(&options.output_path)
        );
        Ok(BaselineComparisonArtifacts {
            markdown_path: options.output_path,
        })
    }
}

fn read_report(path: &Path) -> Result<ComparableReport, String> {
    let bytes = fs::read(path)
        .map_err(|error| format!("failed to read baseline report {}: {error}", path.display()))?;
    let report = serde_json::from_slice::<ComparableReport>(&bytes)
        .map_err(|error| format!("invalid baseline report {}: {error}", path.display()))?;
    if report.report_kind != SUPPORTED_REPORT_KIND {
        return Err(format!(
            "unsupported report kind in {}: {}",
            path.display(),
            report.report_kind
        ));
    }
    if report.runs.is_empty() {
        return Err(format!(
            "baseline report contains no runs: {}",
            path.display()
        ));
    }
    if report.source.application_version.trim().is_empty()
        || report.cleanup_scan_schema_version.trim().is_empty()
    {
        return Err(format!(
            "baseline report is missing version information: {}",
            path.display()
        ));
    }
    if schema_major(&report.schema_version).parse::<u32>().is_err() {
        return Err(format!(
            "invalid baseline report schema version in {}: {}",
            path.display(),
            report.schema_version
        ));
    }
    let unique_rule_count = report
        .rules
        .iter()
        .map(|rule| rule.rule_id.as_str())
        .collect::<BTreeSet<_>>()
        .len();
    if unique_rule_count != report.rules.len() {
        return Err(format!(
            "baseline report contains duplicate rule IDs: {}",
            path.display()
        ));
    }
    if report.summary.rule_count != report.rules.len() as u64 {
        return Err(format!(
            "baseline report rule count does not match rule details: {}",
            path.display()
        ));
    }
    let actually_consistent = report
        .runs
        .windows(2)
        .all(|runs| runs[0].result_digest == runs[1].result_digest);
    if report.summary.result_consistent_across_runs != actually_consistent {
        return Err(format!(
            "baseline report consistency flag does not match run digests: {}",
            path.display()
        ));
    }
    if report.rule_catalog_digest.as_ref().is_some_and(|digest| {
        digest.len() != 64 || !digest.chars().all(|value| value.is_ascii_hexdigit())
    }) {
        return Err(format!(
            "baseline report contains an invalid rule catalog digest: {}",
            path.display()
        ));
    }
    Ok(report)
}

fn assess_compatibility(
    baseline: &ComparableReport,
    candidate: &ComparableReport,
) -> Compatibility {
    let mut reasons = Vec::new();
    if schema_major(&baseline.schema_version) != schema_major(&candidate.schema_version) {
        reasons.push(format!(
            "Report schema major versions differ: {} / {}",
            baseline.schema_version, candidate.schema_version
        ));
    }
    if baseline.environment.os != candidate.environment.os {
        reasons.push(format!(
            "Operating systems differ: {} / {}",
            baseline.environment.os, candidate.environment.os
        ));
    }
    if baseline.environment.architecture != candidate.environment.architecture {
        reasons.push(format!(
            "Architectures differ: {} / {}",
            baseline.environment.architecture, candidate.environment.architecture
        ));
    }
    if baseline.environment.os_version != "unknown"
        && candidate.environment.os_version != "unknown"
        && baseline.environment.os_version != candidate.environment.os_version
    {
        reasons.push(format!(
            "Operating system versions differ: {} / {}",
            baseline.environment.os_version, candidate.environment.os_version
        ));
    }
    if candidate.environment.os_version == "unknown" {
        reasons.push("Candidate report lacks a verifiable operating system version".to_string());
    }

    match (
        baseline.environment.environment_id.as_deref(),
        candidate.environment.environment_id.as_deref(),
    ) {
        (Some(left), Some(right)) if left != right => {
            reasons.push(format!("Environment IDs differ: {left} / {right}"));
        }
        _ => {}
    }
    if candidate.environment.environment_id.is_none() {
        reasons.push("Candidate report lacks a stable environment ID".to_string());
    }
    if candidate.environment.user_identity.is_none() {
        reasons.push("Candidate report lacks an execution user".to_string());
    }
    if let (Some(left), Some(right)) = (
        baseline.environment.user_identity.as_deref(),
        candidate.environment.user_identity.as_deref(),
    ) {
        if left != right {
            reasons.push(format!("Execution users differ: {left} / {right}"));
        }
    }
    if candidate.environment.cpu_model.trim().is_empty()
        || candidate.environment.cpu_model == "unknown"
    {
        reasons.push("Candidate report lacks a verifiable CPU model".to_string());
    }
    if candidate.environment.physical_memory_bytes.is_none() {
        reasons.push("Candidate report lacks physical-memory information".to_string());
    }
    // An environment ID identifies a fixed test machine but cannot hide hardware or scanned-volume
    // changes. CPU, memory, and disk changes can materially affect performance even when the ID is
    // unchanged, so each fact must still be compared.
    compare_hardware_environment(baseline, candidate, &mut reasons);

    if baseline.source.source_dirty_at_build {
        reasons.push(
            "Baseline report was built from a dirty worktree and cannot be traced precisely"
                .to_string(),
        );
    }
    if candidate.source.source_dirty_at_build {
        reasons.push(
            "Candidate report was built from a dirty worktree and cannot be traced precisely"
                .to_string(),
        );
    }
    if baseline.source.source_commit.trim().is_empty() || baseline.source.source_commit == "unknown"
    {
        reasons.push("Baseline report lacks a traceable source commit".to_string());
    }
    if candidate.source.source_commit.trim().is_empty()
        || candidate.source.source_commit == "unknown"
    {
        reasons.push("Candidate report lacks a traceable source commit".to_string());
    }
    if baseline.source.build_profile != "release" || candidate.source.build_profile != "release" {
        reasons.push(format!(
            "Both reports must be release builds: {} / {}",
            baseline.source.build_profile, candidate.source.build_profile
        ));
    }
    if baseline.runs.len() < MINIMUM_COMPARABLE_RUNS
        || candidate.runs.len() < MINIMUM_COMPARABLE_RUNS
    {
        reasons.push(format!(
            "Performance comparison requires at least {MINIMUM_COMPARABLE_RUNS} runs: {} / {}",
            baseline.runs.len(),
            candidate.runs.len()
        ));
    }
    if !baseline.summary.result_consistent_across_runs {
        reasons.push("Baseline report has inconsistent results across runs".to_string());
    }
    if !candidate.summary.result_consistent_across_runs {
        reasons.push("Candidate report has inconsistent results across runs".to_string());
    }
    append_timing_stability_reason("Baseline", baseline, &mut reasons);
    append_timing_stability_reason("Candidate", candidate, &mut reasons);
    Compatibility {
        comparable: reasons.is_empty(),
        reasons,
    }
}

/// Checks stability only across repeated runs because the first run includes process
/// initialization and a different operating-system cache state. A three-run baseline has only two
/// repeated samples, so it uses range relative to the fastest sample as a conservative metric.
/// Continuing with a percentage comparison when spread is too wide would misclassify background
/// I/O noise as a code regression.
fn append_timing_stability_reason(
    label: &str,
    report: &ComparableReport,
    reasons: &mut Vec<String>,
) {
    let repeated = report
        .runs
        .iter()
        .skip(1)
        .map(|run| run.scan_elapsed_ms)
        .collect::<Vec<_>>();
    if let Some(spread) = spread_percent(&repeated) {
        if spread > MAXIMUM_REPEATED_RUN_SPREAD_PERCENT {
            reasons.push(format!(
                "{label} repeated-scan spread is {spread:.2}%, exceeding {MAXIMUM_REPEATED_RUN_SPREAD_PERCENT:.0}%"
            ));
        }
    }
}

fn compare_hardware_environment(
    baseline: &ComparableReport,
    candidate: &ComparableReport,
    reasons: &mut Vec<String>,
) {
    if baseline.environment.cpu_model != candidate.environment.cpu_model {
        reasons.push("CPU models differ".to_string());
    }
    if baseline.environment.logical_cpu_count != candidate.environment.logical_cpu_count {
        reasons.push("Logical CPU counts differ".to_string());
    }
    if baseline.environment.physical_memory_bytes != candidate.environment.physical_memory_bytes {
        reasons.push("Physical memory differs".to_string());
    }
    if baseline.disk.mount_point != candidate.disk.mount_point {
        reasons.push("Scan volumes differ".to_string());
    }
    if baseline.disk.total_bytes != candidate.disk.total_bytes {
        reasons.push("Disk capacities differ".to_string());
    }
}

fn schema_major(version: &str) -> &str {
    version.split('.').next().unwrap_or(version)
}

fn render_comparison(
    baseline: &ComparableReport,
    candidate: &ComparableReport,
    compatibility: &Compatibility,
) -> String {
    let mut markdown = String::new();
    line(&mut markdown, "# MangoDisk Deep-clean Baseline Comparison");
    line(&mut markdown, "");
    line(
        &mut markdown,
        "> This report compares scan results and performance without cleaning any files.",
    );
    line(&mut markdown, "");
    line(&mut markdown, "## Comparison Summary");
    line(&mut markdown, "");
    if compatibility.comparable {
        line(&mut markdown, "- Environment compatibility: **comparable**");
    } else {
        line(
            &mut markdown,
            "- Environment compatibility: **not suitable for performance conclusions**",
        );
        for reason in &compatibility.reasons {
            line(&mut markdown, &format!("  - {}", escape_markdown(reason)));
        }
    }

    let rule_changes = collect_rule_changes(baseline, candidate);
    let precise_catalog_changed = match (
        baseline.rule_catalog_digest.as_deref(),
        candidate.rule_catalog_digest.as_deref(),
    ) {
        (Some(left), Some(right)) => Some(left != right),
        _ => None,
    };
    let special_catalog_changed = match (
        baseline.special_cleaner_catalog_digest.as_deref(),
        candidate.special_cleaner_catalog_digest.as_deref(),
    ) {
        (Some(left), Some(right)) => Some(left != right),
        (None, None) => None,
        _ => Some(true),
    };
    let workload_comparable = precise_catalog_changed
        .map(|changed| !changed)
        .unwrap_or_else(|| {
            rule_changes.added.is_empty()
                && rule_changes.removed.is_empty()
                && rule_changes.definition_changed.is_empty()
        })
        && !special_catalog_changed.unwrap_or(false);
    let performance_comparable = compatibility.comparable && workload_comparable;
    let first_change = percentage_change(
        baseline.summary.duration.first_run_ms,
        candidate.summary.duration.first_run_ms,
    );
    let repeated_change = optional_percentage_change(
        baseline.summary.duration.repeated_run_median_ms,
        candidate.summary.duration.repeated_run_median_ms,
    );
    if performance_comparable {
        line(
            &mut markdown,
            &format!(
                "- First-scan change: {}",
                assessment_text(first_change, false)
            ),
        );
        line(
            &mut markdown,
            &format!(
                "- Repeated-scan change: {}",
                repeated_change
                    .map(|value| assessment_text(Some(value), false))
                    .unwrap_or_else(|| "No comparable data".to_string())
            ),
        );
        if first_change.is_some_and(|value| value > PERFORMANCE_REGRESSION_PERCENT)
            || repeated_change.is_some_and(|value| value > PERFORMANCE_REGRESSION_PERCENT)
        {
            line(
                &mut markdown,
                &format!(
                    "- First/repeated scan threshold: **failed**; regression exceeds {PERFORMANCE_REGRESSION_PERCENT:.0}%"
                ),
            );
        } else {
            line(&mut markdown, "- First/repeated scan threshold: **passed**");
        }
    } else if compatibility.comparable {
        line(
            &mut markdown,
            "- Performance threshold: **not applicable** because the rule catalog or definitions changed; raw timings are informational only",
        );
    }

    line(
        &mut markdown,
        &format!(
            "- Rule catalog changes: {} added, {} removed, {} definition changes",
            rule_changes.added.len(),
            rule_changes.removed.len(),
            rule_changes.definition_changed.len()
        ),
    );
    match precise_catalog_changed {
        Some(true) => line(&mut markdown, "- Exact rule catalog digest: **changed**"),
        Some(false) => line(&mut markdown, "- Exact rule catalog digest: unchanged"),
        None => line(
            &mut markdown,
            "- Exact rule catalog digest: unavailable in V1; visible rule fields provide an approximation",
        ),
    }
    line(
        &mut markdown,
        &format!(
            "- Rule result changes: {}",
            rule_changes.result_changed.len()
        ),
    );

    render_report_identity(&mut markdown, baseline, candidate);
    render_engine(&mut markdown, baseline, candidate);
    render_performance(&mut markdown, baseline, candidate, performance_comparable);
    render_coverage(&mut markdown, baseline, candidate);
    render_rule_changes(&mut markdown, rule_changes);
    render_result_digests(&mut markdown, baseline, candidate);
    markdown
}

fn render_report_identity(
    markdown: &mut String,
    baseline: &ComparableReport,
    candidate: &ComparableReport,
) {
    line(markdown, "");
    line(markdown, "## Reports and Environment");
    line(markdown, "");
    line(markdown, "| Field | Baseline | Candidate |");
    line(markdown, "|------|------|------|");
    row(markdown, "Label", &baseline.label, &candidate.label);
    row(
        markdown,
        "Source commit",
        &baseline.source.source_commit,
        &candidate.source.source_commit,
    );
    row(
        markdown,
        "Application version",
        &baseline.source.application_version,
        &candidate.source.application_version,
    );
    row(
        markdown,
        "Build profile",
        &baseline.source.build_profile,
        &candidate.source.build_profile,
    );
    row(
        markdown,
        "Dirty worktree at build time",
        boolean_text(baseline.source.source_dirty_at_build),
        boolean_text(candidate.source.source_dirty_at_build),
    );
    row(
        markdown,
        "Environment ID",
        baseline
            .environment
            .environment_id
            .as_deref()
            .unwrap_or("Not recorded in V1"),
        candidate
            .environment
            .environment_id
            .as_deref()
            .unwrap_or("Not recorded in V1"),
    );
    row(
        markdown,
        "Execution user",
        baseline
            .environment
            .user_identity
            .as_deref()
            .unwrap_or("Not recorded in V1"),
        candidate
            .environment
            .user_identity
            .as_deref()
            .unwrap_or("Not recorded in V1"),
    );
    row(
        markdown,
        "Operating system",
        &baseline.environment.os,
        &candidate.environment.os,
    );
    row(
        markdown,
        "Operating system version",
        &baseline.environment.os_version,
        &candidate.environment.os_version,
    );
    row(
        markdown,
        "Architecture",
        &baseline.environment.architecture,
        &candidate.environment.architecture,
    );
    row(
        markdown,
        "CPU",
        &baseline.environment.cpu_model,
        &candidate.environment.cpu_model,
    );
    row(
        markdown,
        "Logical CPUs",
        &baseline.environment.logical_cpu_count.to_string(),
        &candidate.environment.logical_cpu_count.to_string(),
    );
    row(
        markdown,
        "Physical memory",
        &baseline
            .environment
            .physical_memory_bytes
            .map(format_bytes)
            .unwrap_or_else(|| "Unknown".to_string()),
        &candidate
            .environment
            .physical_memory_bytes
            .map(format_bytes)
            .unwrap_or_else(|| "Unknown".to_string()),
    );
    row(
        markdown,
        "Scan volume",
        &baseline.disk.mount_point,
        &candidate.disk.mount_point,
    );
    row(
        markdown,
        "Disk capacity",
        &format_bytes(baseline.disk.total_bytes),
        &format_bytes(candidate.disk.total_bytes),
    );
}

fn render_engine(markdown: &mut String, baseline: &ComparableReport, candidate: &ComparableReport) {
    line(markdown, "");
    line(markdown, "## Engine Configuration");
    line(markdown, "");
    line(markdown, "| Field | Baseline | Candidate |");
    line(markdown, "|------|------|------|");
    row(
        markdown,
        "Policy",
        &baseline.engine.strategy,
        &candidate.engine.strategy,
    );
    row(
        markdown,
        "Rule catalog",
        &baseline.engine.rule_catalog_mode,
        &candidate.engine.rule_catalog_mode,
    );
    row(
        markdown,
        "Rule catalog digest",
        baseline
            .rule_catalog_digest
            .as_deref()
            .unwrap_or("Not recorded in V1"),
        candidate
            .rule_catalog_digest
            .as_deref()
            .unwrap_or("Not recorded in V1"),
    );
    row(
        markdown,
        "Specialized cleaner digest",
        baseline
            .special_cleaner_catalog_digest
            .as_deref()
            .unwrap_or("Not recorded in legacy report"),
        candidate
            .special_cleaner_catalog_digest
            .as_deref()
            .unwrap_or("Not recorded in legacy report"),
    );
    row(
        markdown,
        "Cleanup scan-result schema",
        &baseline.cleanup_scan_schema_version,
        &candidate.cleanup_scan_schema_version,
    );
    row(
        markdown,
        "Worker limit",
        &baseline.engine.configured_worker_limit.to_string(),
        &candidate.engine.configured_worker_limit.to_string(),
    );
    row(
        markdown,
        "Scan-result persistence",
        boolean_text(baseline.engine.scan_result_persistence_enabled),
        boolean_text(candidate.engine.scan_result_persistence_enabled),
    );
    row(
        markdown,
        "Multi-rule single traversal",
        boolean_text(baseline.engine.single_pass_rule_matching),
        boolean_text(candidate.engine.single_pass_rule_matching),
    );
    row(
        markdown,
        "Incremental scan",
        boolean_text(baseline.engine.incremental_scan_enabled),
        boolean_text(candidate.engine.incremental_scan_enabled),
    );
}

fn render_performance(
    markdown: &mut String,
    baseline: &ComparableReport,
    candidate: &ComparableReport,
    comparable: bool,
) {
    line(markdown, "");
    line(markdown, "## Performance");
    line(markdown, "");
    line(
        markdown,
        "| Metric | Baseline | Candidate | Change | Assessment |",
    );
    line(markdown, "|------|------|------|------|------|");
    performance_row(
        markdown,
        "First scan",
        baseline.summary.duration.first_run_ms,
        candidate.summary.duration.first_run_ms,
        comparable,
    );
    performance_row(
        markdown,
        "Scan median",
        baseline.summary.duration.median_ms,
        candidate.summary.duration.median_ms,
        comparable,
    );
    optional_performance_row(
        markdown,
        "Repeated-scan median",
        baseline.summary.duration.repeated_run_median_ms,
        candidate.summary.duration.repeated_run_median_ms,
        comparable,
    );
    optional_performance_row(
        markdown,
        "First progress",
        median_optional_run_value(&baseline.runs, |run| run.first_progress_ms),
        median_optional_run_value(&candidate.runs, |run| run.first_progress_ms),
        comparable,
    );
    optional_performance_row(
        markdown,
        "First scan data",
        median_optional_run_value(&baseline.runs, |run| run.first_scan_observation_ms),
        median_optional_run_value(&candidate.runs, |run| run.first_scan_observation_ms),
        comparable,
    );
    optional_performance_row(
        markdown,
        "First match",
        median_optional_run_value(&baseline.runs, |run| run.first_match_ms),
        median_optional_run_value(&candidate.runs, |run| run.first_match_ms),
        comparable,
    );

    line(markdown, "");
    line(markdown, "### Scan Workload");
    line(markdown, "");
    line(
        markdown,
        "| Metric | Baseline median | Candidate median | Change |",
    );
    line(markdown, "|------|------------|------------|------|");
    integer_metric_row(
        markdown,
        "Progress events",
        median_run_value(&baseline.runs, |run| run.progress_event_count),
        median_run_value(&candidate.runs, |run| run.progress_event_count),
        false,
    );
    integer_metric_row(
        markdown,
        "Files checked",
        median_run_value(&baseline.runs, |run| run.items_scanned),
        median_run_value(&candidate.runs, |run| run.items_scanned),
        false,
    );
    integer_metric_row(
        markdown,
        "Bytes traversed",
        median_run_value(&baseline.runs, |run| run.bytes_scanned),
        median_run_value(&candidate.runs, |run| run.bytes_scanned),
        true,
    );
}

fn render_coverage(
    markdown: &mut String,
    baseline: &ComparableReport,
    candidate: &ComparableReport,
) {
    line(markdown, "");
    line(markdown, "## Cleanup Coverage and Results");
    line(markdown, "");
    line(markdown, "| Metric | Baseline | Candidate | Change |");
    line(markdown, "|------|------|------|------|");
    integer_metric_row(
        markdown,
        "Total rules",
        baseline.summary.rule_count,
        candidate.summary.rule_count,
        false,
    );
    integer_metric_row(
        markdown,
        "Applicable rules",
        baseline.summary.applicable_rule_count,
        candidate.summary.applicable_rule_count,
        false,
    );
    integer_metric_row(
        markdown,
        "Matched rules",
        baseline.summary.found_rule_count,
        candidate.summary.found_rule_count,
        false,
    );
    integer_metric_row(
        markdown,
        "Clean rules",
        baseline.summary.clean_rule_count,
        candidate.summary.clean_rule_count,
        false,
    );
    integer_metric_row(
        markdown,
        "Inapplicable rules",
        baseline.summary.not_applicable_rule_count,
        candidate.summary.not_applicable_rule_count,
        false,
    );
    integer_metric_row(
        markdown,
        "Rules requiring closed applications",
        baseline.summary.requires_close_rule_count,
        candidate.summary.requires_close_rule_count,
        false,
    );
    integer_metric_row(
        markdown,
        "Inspect-only rules",
        baseline.summary.review_only_rule_count,
        candidate.summary.review_only_rule_count,
        false,
    );
    integer_metric_row(
        markdown,
        "Permission-restricted rules",
        baseline.summary.limited_rule_count,
        candidate.summary.limited_rule_count,
        false,
    );
    integer_metric_row(
        markdown,
        "Default-selected rules",
        baseline.summary.default_selected_rule_count,
        candidate.summary.default_selected_rule_count,
        false,
    );
    integer_metric_row(
        markdown,
        "Matched files",
        baseline.summary.matched_file_count,
        candidate.summary.matched_file_count,
        false,
    );
    integer_metric_row(
        markdown,
        "Low-risk reclaimable bytes",
        baseline.summary.safe_bytes,
        candidate.summary.safe_bytes,
        true,
    );
    integer_metric_row(
        markdown,
        "Total reclaimable bytes",
        baseline.summary.reclaimable_bytes,
        candidate.summary.reclaimable_bytes,
        true,
    );
    if let (Some(baseline_value), Some(candidate_value)) = (
        baseline.summary.filesystem_rule_count,
        candidate.summary.filesystem_rule_count,
    ) {
        integer_metric_row(
            markdown,
            "Filesystem rules",
            baseline_value,
            candidate_value,
            false,
        );
    }
    if let (Some(baseline_value), Some(candidate_value)) = (
        baseline.summary.special_cleaner_count,
        candidate.summary.special_cleaner_count,
    ) {
        integer_metric_row(
            markdown,
            "Specialized cleaners",
            baseline_value,
            candidate_value,
            false,
        );
    }
    if let (Some(baseline_value), Some(candidate_value)) = (
        baseline.summary.filesystem_reclaimable_bytes,
        candidate.summary.filesystem_reclaimable_bytes,
    ) {
        integer_metric_row(
            markdown,
            "Filesystem-rule reclaimable bytes",
            baseline_value,
            candidate_value,
            true,
        );
    }
    if let (Some(baseline_value), Some(candidate_value)) = (
        baseline.summary.special_reclaimable_bytes,
        candidate.summary.special_reclaimable_bytes,
    ) {
        integer_metric_row(
            markdown,
            "Specialized-cleaner estimated bytes",
            baseline_value,
            candidate_value,
            true,
        );
    }
    integer_metric_row(
        markdown,
        "Skipped items / warnings",
        baseline.summary.warning_count,
        candidate.summary.warning_count,
        false,
    );
}

fn collect_rule_changes<'a>(
    baseline: &'a ComparableReport,
    candidate: &'a ComparableReport,
) -> RuleChanges<'a> {
    let baseline_rules = baseline
        .rules
        .iter()
        .map(|rule| (rule.rule_id.as_str(), rule))
        .collect::<BTreeMap<_, _>>();
    let candidate_rules = candidate
        .rules
        .iter()
        .map(|rule| (rule.rule_id.as_str(), rule))
        .collect::<BTreeMap<_, _>>();
    let all_ids = baseline_rules
        .keys()
        .chain(candidate_rules.keys())
        .copied()
        .collect::<BTreeSet<_>>();
    let mut changes = RuleChanges {
        added: Vec::new(),
        removed: Vec::new(),
        definition_changed: Vec::new(),
        result_changed: Vec::new(),
    };
    for rule_id in all_ids {
        match (baseline_rules.get(rule_id), candidate_rules.get(rule_id)) {
            (None, Some(candidate_rule)) => changes.added.push(candidate_rule),
            (Some(baseline_rule), None) => changes.removed.push(baseline_rule),
            (Some(baseline_rule), Some(candidate_rule)) => {
                if rule_definition_signature(baseline_rule)
                    != rule_definition_signature(candidate_rule)
                {
                    changes
                        .definition_changed
                        .push((baseline_rule, candidate_rule));
                }
                if rule_result_signature(baseline_rule) != rule_result_signature(candidate_rule) {
                    changes.result_changed.push((baseline_rule, candidate_rule));
                }
            }
            (None, None) => {}
        }
    }
    changes
}

fn rule_definition_signature(rule: &ComparableRule) -> (&str, &str, bool, bool) {
    (
        &rule.category,
        &rule.risk,
        rule.default_selected,
        rule.requires_app_close,
    )
}

fn rule_result_signature(rule: &ComparableRule) -> (&str, u64, u64) {
    (
        &rule.status,
        rule.matched_file_count,
        rule.reclaimable_bytes,
    )
}

fn render_rule_changes(markdown: &mut String, changes: RuleChanges<'_>) {
    line(markdown, "");
    line(markdown, "## Rule Changes");
    line(markdown, "");
    render_rule_list(markdown, "Added rules", &changes.added);
    render_rule_list(markdown, "Removed rules", &changes.removed);

    line(markdown, "");
    line(markdown, "### Definition Changes");
    line(markdown, "");
    if changes.definition_changed.is_empty() {
        line(markdown, "None.");
    } else {
        line(markdown, "| Rule ID | Baseline | Candidate |");
        line(markdown, "|---------|------|------|");
        for (baseline, candidate) in changes.definition_changed {
            line(
                markdown,
                &format!(
                    "| `{}` | {} / {} / default {} / close application {} | {} / {} / default {} / close application {} |",
                    escape_markdown(&baseline.rule_id),
                    escape_markdown(&baseline.category),
                    baseline.risk,
                    boolean_text(baseline.default_selected),
                    boolean_text(baseline.requires_app_close),
                    escape_markdown(&candidate.category),
                    candidate.risk,
                    boolean_text(candidate.default_selected),
                    boolean_text(candidate.requires_app_close),
                ),
            );
        }
    }

    line(markdown, "");
    line(markdown, "### Result Changes");
    line(markdown, "");
    if changes.result_changed.is_empty() {
        line(markdown, "None.");
    } else {
        line(
            markdown,
            "| Rule ID | Status | File change | Space change | Median duration change |",
        );
        line(
            markdown,
            "|---------|------|----------|----------|----------------|",
        );
        for (baseline, candidate) in changes.result_changed {
            line(
                markdown,
                &format!(
                    "| `{}` | {} → {} | {} | {} | {} |",
                    escape_markdown(&baseline.rule_id),
                    baseline.status,
                    candidate.status,
                    signed_difference(
                        baseline.matched_file_count,
                        candidate.matched_file_count,
                        false
                    ),
                    signed_difference(
                        baseline.reclaimable_bytes,
                        candidate.reclaimable_bytes,
                        true
                    ),
                    signed_duration_difference(
                        baseline.scan_duration.median_ms,
                        candidate.scan_duration.median_ms
                    ),
                ),
            );
        }
    }
}

fn render_rule_list(markdown: &mut String, heading: &str, rules: &[&ComparableRule]) {
    line(markdown, &format!("### {heading}"));
    line(markdown, "");
    if rules.is_empty() {
        line(markdown, "None.");
    } else {
        for rule in rules {
            line(
                markdown,
                &format!(
                    "- `{}`：{} / {}",
                    escape_markdown(&rule.rule_id),
                    escape_markdown(&rule.category),
                    rule.risk
                ),
            );
        }
    }
    line(markdown, "");
}

fn render_result_digests(
    markdown: &mut String,
    baseline: &ComparableReport,
    candidate: &ComparableReport,
) {
    line(markdown, "");
    line(markdown, "## Result Digests");
    line(markdown, "");
    line(
        markdown,
        &format!(
            "- Baseline: `{}`",
            baseline
                .runs
                .first()
                .map(|run| run.result_digest.as_str())
                .unwrap_or("missing")
        ),
    );
    if let Some(digest) = baseline
        .runs
        .first()
        .and_then(|run| run.filesystem_result_digest.as_deref())
    {
        line(
            markdown,
            &format!("- Baseline filesystem rules: `{digest}`"),
        );
    }
    if let Some(digest) = candidate
        .runs
        .first()
        .and_then(|run| run.filesystem_result_digest.as_deref())
    {
        line(
            markdown,
            &format!("- Candidate filesystem rules: `{digest}`"),
        );
    }
    if let Some(digest) = baseline
        .runs
        .first()
        .and_then(|run| run.special_result_digest.as_deref())
    {
        line(
            markdown,
            &format!("- Baseline specialized cleaners: `{digest}`"),
        );
    }
    if let Some(digest) = candidate
        .runs
        .first()
        .and_then(|run| run.special_result_digest.as_deref())
    {
        line(
            markdown,
            &format!("- Candidate specialized cleaners: `{digest}`"),
        );
    }
    line(
        markdown,
        &format!(
            "- Candidate: `{}`",
            candidate
                .runs
                .first()
                .map(|run| run.result_digest.as_str())
                .unwrap_or("missing")
        ),
    );
    line(
        markdown,
        "- Different digests do not automatically indicate a regression. Cache contents change over time; interpret them with rule changes and per-rule results.",
    );
}

fn performance_row(
    markdown: &mut String,
    name: &str,
    baseline: u64,
    candidate: u64,
    comparable: bool,
) {
    optional_performance_row(markdown, name, Some(baseline), Some(candidate), comparable);
}

fn optional_performance_row(
    markdown: &mut String,
    name: &str,
    baseline: Option<u64>,
    candidate: Option<u64>,
    comparable: bool,
) {
    let change = optional_percentage_change(baseline, candidate);
    line(
        markdown,
        &format!(
            "| {} | {} | {} | {} | {} |",
            escape_markdown(name),
            baseline
                .map(format_duration)
                .unwrap_or_else(|| "Missing".to_string()),
            candidate
                .map(format_duration)
                .unwrap_or_else(|| "Missing".to_string()),
            change
                .map(format_percentage)
                .unwrap_or_else(|| "Unavailable".to_string()),
            if comparable {
                assessment_text(change, true)
            } else {
                "No performance conclusion".to_string()
            },
        ),
    );
}

fn integer_metric_row(
    markdown: &mut String,
    name: &str,
    baseline: u64,
    candidate: u64,
    bytes: bool,
) {
    line(
        markdown,
        &format!(
            "| {} | {} | {} | {} |",
            escape_markdown(name),
            format_metric(baseline, bytes),
            format_metric(candidate, bytes),
            signed_difference(baseline, candidate, bytes),
        ),
    );
}

fn median_run_value(runs: &[ComparableRun], selector: impl Fn(&ComparableRun) -> u64) -> u64 {
    let mut values = runs.iter().map(selector).collect::<Vec<_>>();
    median(&mut values)
}

fn median_optional_run_value(
    runs: &[ComparableRun],
    selector: impl Fn(&ComparableRun) -> Option<u64>,
) -> Option<u64> {
    let mut values = runs.iter().filter_map(selector).collect::<Vec<_>>();
    (!values.is_empty()).then(|| median(&mut values))
}

fn median(values: &mut [u64]) -> u64 {
    values.sort_unstable();
    let middle = values.len() / 2;
    if values.len().is_multiple_of(2) {
        values[middle - 1].saturating_add(values[middle]) / 2
    } else {
        values[middle]
    }
}

fn percentage_change(baseline: u64, candidate: u64) -> Option<f64> {
    if baseline == 0 {
        return None;
    }
    Some((candidate as f64 - baseline as f64) / baseline as f64 * 100.0)
}

fn spread_percent(values: &[u64]) -> Option<f64> {
    let minimum = values.iter().copied().min()?;
    let maximum = values.iter().copied().max()?;
    if minimum == 0 {
        return (maximum == 0).then_some(0.0);
    }
    Some((maximum - minimum) as f64 / minimum as f64 * 100.0)
}

fn optional_percentage_change(baseline: Option<u64>, candidate: Option<u64>) -> Option<f64> {
    percentage_change(baseline?, candidate?)
}

fn assessment_text(change: Option<f64>, include_threshold: bool) -> String {
    let Some(change) = change else {
        return "Unavailable".to_string();
    };
    let assessment = if change > PERFORMANCE_REGRESSION_PERCENT {
        "Regression"
    } else if change < -PERFORMANCE_REGRESSION_PERCENT {
        "Improvement"
    } else {
        "Within variance"
    };
    if include_threshold {
        format!("{assessment} (threshold ±{PERFORMANCE_REGRESSION_PERCENT:.0}%)")
    } else {
        format!("{}，{}", format_percentage(change), assessment)
    }
}

fn signed_difference(baseline: u64, candidate: u64, bytes: bool) -> String {
    if candidate >= baseline {
        format!("+{}", format_metric(candidate - baseline, bytes))
    } else {
        format!("-{}", format_metric(baseline - candidate, bytes))
    }
}

fn signed_duration_difference(baseline: u64, candidate: u64) -> String {
    if candidate >= baseline {
        format!("+{}", format_duration(candidate - baseline))
    } else {
        format!("-{}", format_duration(baseline - candidate))
    }
}

fn format_metric(value: u64, bytes: bool) -> String {
    if bytes {
        format_bytes(value)
    } else {
        value.to_string()
    }
}

fn format_percentage(value: f64) -> String {
    format!("{value:+.2}%")
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

fn boolean_text(value: bool) -> &'static str {
    if value {
        "Yes"
    } else {
        "No"
    }
}

fn line(target: &mut String, value: &str) {
    target.push_str(value);
    target.push('\n');
}

fn row(target: &mut String, name: &str, baseline: &str, candidate: &str) {
    line(
        target,
        &format!(
            "| {} | {} | {} |",
            escape_markdown(name),
            escape_markdown(baseline),
            escape_markdown(candidate)
        ),
    );
}

fn escape_markdown(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace('|', "\\|")
        .replace(['\r', '\n'], " ")
}

fn paths_refer_to_same_file(left: &Path, right: &Path) -> bool {
    if left == right {
        return true;
    }
    match (fs::canonicalize(left), fs::canonicalize(right)) {
        (Ok(left), Ok(right)) => left == right,
        _ => false,
    }
}

fn file_name_for_log(path: &Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| "unknown".to_string())
}

fn write_atomic(path: &Path, contents: &[u8]) -> Result<(), String> {
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent).map_err(|error| {
        format!(
            "failed to create the baseline comparison directory {}: {error}",
            parent.display()
        )
    })?;
    let unique_suffix = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let temporary = path.with_extension(format!(
        "{}.{}.{}.tmp",
        path.extension()
            .and_then(|extension| extension.to_str())
            .unwrap_or("md"),
        process::id(),
        unique_suffix
    ));
    fs::write(&temporary, contents).map_err(|error| {
        format!(
            "failed to write the temporary comparison report {}: {error}",
            temporary.display()
        )
    })?;
    match fs::rename(&temporary, path) {
        Ok(()) => Ok(()),
        Err(first_error) if path.exists() => replace_existing_file(path, &temporary, first_error),
        Err(error) => {
            let _ = fs::remove_file(&temporary);
            Err(format!(
                "failed to save the baseline comparison report {}: {error}",
                path.display()
            ))
        }
    }
}

/// Windows rename cannot replace an existing destination. The old report is first renamed to a
/// backup and deleted only after the new report is installed. If the second rename fails, the old
/// report is restored so one failed comparison cannot destroy archived iteration evidence.
fn replace_existing_file(
    path: &Path,
    temporary: &Path,
    first_error: std::io::Error,
) -> Result<(), String> {
    let backup = path.with_extension(format!(
        "{}.{}.bak",
        path.extension()
            .and_then(|extension| extension.to_str())
            .unwrap_or("md"),
        process::id()
    ));
    if backup.exists() {
        fs::remove_file(&backup).map_err(|error| {
            format!(
                "failed to remove the stale comparison report backup {}: {error}",
                backup.display()
            )
        })?;
    }
    fs::rename(path, &backup).map_err(|error| {
        format!(
            "failed to back up the existing comparison report {}: {error}",
            path.display()
        )
    })?;
    match fs::rename(temporary, path) {
        Ok(()) => {
            if let Err(error) = fs::remove_file(&backup) {
                log::warn!(
                    "baseline_comparison_backup_cleanup_failed backup_file={} error={}",
                    file_name_for_log(&backup),
                    error
                );
            }
            Ok(())
        }
        Err(error) => {
            let restore_error = fs::rename(&backup, path).err();
            let _ = fs::remove_file(temporary);
            Err(format!(
                "failed to save the baseline comparison report {}: {error}; initial error: \
                 {first_error}; previous report restore result: {}",
                path.display(),
                restore_error
                    .map(|value| value.to_string())
                    .unwrap_or_else(|| "succeeded".to_string())
            ))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        assess_compatibility, assessment_text, percentage_change, spread_percent, ComparableDisk,
        ComparableDuration, ComparableEngine, ComparableEnvironment, ComparableReport,
        ComparableRule, ComparableRun, ComparableSource, ComparableSummary,
    };

    fn comparable_report() -> ComparableReport {
        ComparableReport {
            schema_version: "1.1".to_string(),
            report_kind: "deep-clean".to_string(),
            label: "fixture".to_string(),
            source: ComparableSource {
                application_version: "0.1.0".to_string(),
                source_commit: "0123456789abcdef".to_string(),
                source_dirty_at_build: false,
                build_profile: "release".to_string(),
            },
            environment: ComparableEnvironment {
                environment_id: Some("fixture-machine".to_string()),
                user_identity: Some("fixture-user".to_string()),
                os: "macos".to_string(),
                architecture: "aarch64".to_string(),
                os_version: "26.5".to_string(),
                cpu_model: "fixture-cpu".to_string(),
                logical_cpu_count: 8,
                physical_memory_bytes: Some(16 * 1024 * 1024 * 1024),
            },
            engine: ComparableEngine {
                strategy: "fixture".to_string(),
                rule_catalog_mode: "legacy".to_string(),
                configured_worker_limit: 4,
                scan_result_persistence_enabled: false,
                single_pass_rule_matching: false,
                incremental_scan_enabled: false,
            },
            rule_catalog_digest: Some("a".repeat(64)),
            special_cleaner_catalog_digest: Some("b".repeat(64)),
            cleanup_scan_schema_version: "1.0".to_string(),
            disk: ComparableDisk {
                mount_point: "/".to_string(),
                total_bytes: 512 * 1024 * 1024 * 1024,
            },
            summary: ComparableSummary {
                rule_count: 1,
                applicable_rule_count: 1,
                found_rule_count: 1,
                clean_rule_count: 0,
                not_applicable_rule_count: 0,
                requires_close_rule_count: 0,
                review_only_rule_count: 0,
                limited_rule_count: 0,
                default_selected_rule_count: 1,
                warning_count: 0,
                matched_file_count: 1,
                safe_bytes: 1024,
                reclaimable_bytes: 1024,
                filesystem_rule_count: Some(1),
                special_cleaner_count: Some(0),
                filesystem_reclaimable_bytes: Some(1024),
                special_reclaimable_bytes: Some(0),
                result_consistent_across_runs: true,
                duration: ComparableDuration {
                    first_run_ms: 100,
                    median_ms: 90,
                    repeated_run_median_ms: Some(85),
                },
            },
            runs: (0..3)
                .map(|_| ComparableRun {
                    scan_elapsed_ms: 90,
                    first_progress_ms: Some(10),
                    first_scan_observation_ms: Some(15),
                    first_match_ms: Some(20),
                    progress_event_count: 3,
                    items_scanned: 10,
                    bytes_scanned: 1024,
                    result_digest: "stable-result".to_string(),
                    filesystem_result_digest: Some("stable-filesystem".to_string()),
                    special_result_digest: Some("stable-special".to_string()),
                })
                .collect(),
            rules: vec![ComparableRule {
                rule_id: "fixture.rule".to_string(),
                category: "fixture".to_string(),
                risk: "safe".to_string(),
                status: "found".to_string(),
                default_selected: true,
                requires_app_close: false,
                matched_file_count: 1,
                reclaimable_bytes: 1024,
                scan_duration: ComparableDuration {
                    first_run_ms: 50,
                    median_ms: 40,
                    repeated_run_median_ms: Some(35),
                },
            }],
        }
    }

    #[test]
    fn percentage_change_uses_baseline_as_denominator() {
        assert_eq!(percentage_change(100, 90), Some(-10.0));
        assert_eq!(percentage_change(100, 110), Some(10.0));
        assert_eq!(percentage_change(0, 10), None);
    }

    #[test]
    fn assessment_distinguishes_regression_and_improvement() {
        assert!(assessment_text(Some(11.0), true).starts_with("Regression"));
        assert!(assessment_text(Some(-11.0), true).starts_with("Improvement"));
        assert!(assessment_text(Some(5.0), true).starts_with("Within variance"));
    }

    #[test]
    fn repeated_run_spread_uses_the_fastest_run_as_baseline() {
        assert_eq!(spread_percent(&[100, 125]), Some(25.0));
        assert_eq!(spread_percent(&[0, 0]), Some(0.0));
        assert_eq!(spread_percent(&[0, 1]), None);
        assert_eq!(spread_percent(&[]), None);
    }

    #[test]
    fn noisy_repeated_runs_are_not_comparable() {
        let baseline = comparable_report();
        let mut candidate = comparable_report();
        candidate.runs[1].scan_elapsed_ms = 100;
        candidate.runs[2].scan_elapsed_ms = 150;

        let compatibility = assess_compatibility(&baseline, &candidate);

        assert!(!compatibility.comparable);
        assert!(compatibility
            .reasons
            .iter()
            .any(|reason| reason.contains("spread")));
    }

    #[test]
    fn identical_release_environments_are_comparable() {
        let compatibility = assess_compatibility(&comparable_report(), &comparable_report());
        assert!(compatibility.comparable);
        assert!(compatibility.reasons.is_empty());
    }

    #[test]
    fn candidate_without_os_version_is_not_comparable() {
        let baseline = comparable_report();
        let mut candidate = comparable_report();
        candidate.environment.os_version = "unknown".to_string();

        let compatibility = assess_compatibility(&baseline, &candidate);

        assert!(!compatibility.comparable);
        assert!(compatibility
            .reasons
            .iter()
            .any(|reason| reason.contains("verifiable operating system version")));
    }

    #[test]
    fn environment_id_cannot_hide_a_hardware_change() {
        let baseline = comparable_report();
        let mut candidate = comparable_report();
        candidate.environment.logical_cpu_count = 4;

        let compatibility = assess_compatibility(&baseline, &candidate);

        assert!(!compatibility.comparable);
        assert!(compatibility
            .reasons
            .iter()
            .any(|reason| reason.contains("Logical CPU counts")));
    }
}

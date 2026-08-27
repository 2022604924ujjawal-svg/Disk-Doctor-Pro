use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
};

use serde::Deserialize;

use super::system::now_ms;

const REQUIRED_RUNS: usize = 3;
const PERFORMANCE_THRESHOLD_PERCENT: f64 = 10.0;
const REPEATED_SPREAD_LIMIT_PERCENT: f64 = 25.0;
const REPEATED_SPREAD_ABSOLUTE_TOLERANCE_MS: u64 = 5;

#[derive(Debug)]
pub struct EngineBenchmarkComparisonOptions {
    pub baseline_path: PathBuf,
    pub candidate_path: PathBuf,
    pub output_path: PathBuf,
}

#[derive(Debug)]
pub struct EngineBenchmarkComparisonArtifacts {
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
    dataset: ComparableDataset,
    modules: Vec<ComparableModule>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ComparableSource {
    source_commit: String,
    source_dirty_at_build: bool,
    build_profile: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ComparableEnvironment {
    environment_id: String,
    user_identity: String,
    os: String,
    architecture: String,
    os_version: String,
    cpu_model: String,
    logical_cpu_count: usize,
    physical_memory_bytes: Option<u64>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ComparableDataset {
    dataset_version: String,
    dataset_id: String,
    logical_digest: String,
    logical_file_count: u64,
    logical_directory_count: u64,
    logical_bytes: u64,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ComparableModule {
    module: String,
    workload_kind: String,
    workload_digest: String,
    scan_mode: String,
    #[serde(default)]
    error_summary: Option<serde_json::Value>,
    summary: ComparableSummary,
    runs: Vec<ComparableRun>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ComparableSummary {
    result_consistent_across_runs: bool,
    expectation_met_across_runs: bool,
    first_run_ms: u64,
    repeated_run_median_ms: Option<u64>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ComparableRun {
    total_elapsed_ms: u64,
    result_digest: String,
    phase_elapsed_ms: BTreeMap<String, u64>,
    expectation_met: bool,
}

#[derive(Debug)]
struct ComparisonGate {
    global_reasons: Vec<String>,
    module_reasons: BTreeMap<String, Vec<String>>,
}

impl ComparisonGate {
    fn module_comparable(&self, name: &str) -> bool {
        self.global_reasons.is_empty() && !self.module_reasons.contains_key(name)
    }

    fn comparable_module_count(&self) -> usize {
        ["deepClean", "diskAnalysis", "largeFiles", "duplicateFiles"]
            .iter()
            .filter(|name| self.module_comparable(name))
            .count()
    }

    fn reason_count(&self) -> usize {
        self.global_reasons.len() + self.module_reasons.values().map(Vec::len).sum::<usize>()
    }
}

pub struct EngineBenchmarkComparisonService;

impl EngineBenchmarkComparisonService {
    pub fn compare(
        options: EngineBenchmarkComparisonOptions,
    ) -> Result<EngineBenchmarkComparisonArtifacts, String> {
        reject_input_overwrite(&options)?;
        let baseline = read_report(&options.baseline_path)?;
        let candidate = read_report(&options.candidate_path)?;
        let gate = comparison_gate(&baseline, &candidate);
        let markdown = render_comparison(&baseline, &candidate, &gate);
        write_atomic_replace(&options.output_path, markdown.as_bytes())?;
        log::info!(
            "engine_benchmark_comparison_generated comparable_module_count={} reason_count={} baseline_file={} candidate_file={} output_file={}",
            gate.comparable_module_count(),
            gate.reason_count(),
            file_name(&options.baseline_path),
            file_name(&options.candidate_path),
            file_name(&options.output_path)
        );
        Ok(EngineBenchmarkComparisonArtifacts {
            markdown_path: options.output_path,
        })
    }
}

fn read_report(path: &Path) -> Result<ComparableReport, String> {
    let bytes = fs::read(path).map_err(|error| {
        format!(
            "failed to read engine benchmark report {}: {error}",
            path.display()
        )
    })?;
    let report = serde_json::from_slice::<ComparableReport>(&bytes).map_err(|error| {
        format!(
            "failed to parse engine benchmark report {}: {error}",
            path.display()
        )
    })?;
    if report.schema_version != "1.0" || report.report_kind != "engine-suite" {
        return Err(format!(
            "{} is not a supported engine benchmark report",
            path.display()
        ));
    }
    if report.modules.is_empty() {
        return Err(format!(
            "{} has no benchmark module results",
            path.display()
        ));
    }
    let module_names = report
        .modules
        .iter()
        .map(|module| module.module.as_str())
        .collect::<BTreeSet<_>>();
    let expected_names =
        BTreeSet::from(["deepClean", "diskAnalysis", "largeFiles", "duplicateFiles"]);
    if module_names != expected_names || report.modules.len() != expected_names.len() {
        return Err(format!(
            "{} has an invalid or duplicate benchmark module set",
            path.display()
        ));
    }
    Ok(report)
}

fn comparison_gate(baseline: &ComparableReport, candidate: &ComparableReport) -> ComparisonGate {
    let mut global_reasons = Vec::new();
    check_source("Baseline", baseline, &mut global_reasons);
    check_source("Candidate", candidate, &mut global_reasons);
    compare_environment(baseline, candidate, &mut global_reasons);
    compare_dataset(baseline, candidate, &mut global_reasons);
    let module_reasons = compare_modules(baseline, candidate, &mut global_reasons);
    ComparisonGate {
        global_reasons,
        module_reasons,
    }
}

fn check_source(prefix: &str, report: &ComparableReport, reasons: &mut Vec<String>) {
    if report.source.source_commit == "unknown" || report.source.source_commit.trim().is_empty() {
        reasons.push(format!("{prefix} report has no traceable source commit"));
    }
    if report.source.source_dirty_at_build {
        reasons.push(format!("{prefix} report was built from a dirty worktree"));
    }
    if report.source.build_profile != "release" {
        reasons.push(format!("{prefix} report is not a release build"));
    }
}

fn compare_environment(
    baseline: &ComparableReport,
    candidate: &ComparableReport,
    reasons: &mut Vec<String>,
) {
    compare_value(
        "environment ID",
        &baseline.environment.environment_id,
        &candidate.environment.environment_id,
        reasons,
    );
    compare_value(
        "operating system",
        &baseline.environment.os,
        &candidate.environment.os,
        reasons,
    );
    compare_value(
        "architecture",
        &baseline.environment.architecture,
        &candidate.environment.architecture,
        reasons,
    );
    compare_value(
        "operating system version",
        &baseline.environment.os_version,
        &candidate.environment.os_version,
        reasons,
    );
    compare_value(
        "CPU",
        &baseline.environment.cpu_model,
        &candidate.environment.cpu_model,
        reasons,
    );
    if baseline.environment.logical_cpu_count != candidate.environment.logical_cpu_count {
        reasons.push("Logical CPU count differs".to_string());
    }
    if baseline.environment.physical_memory_bytes != candidate.environment.physical_memory_bytes {
        reasons.push("Physical memory differs".to_string());
    }
}

fn compare_dataset(
    baseline: &ComparableReport,
    candidate: &ComparableReport,
    reasons: &mut Vec<String>,
) {
    compare_value(
        "dataset version",
        &baseline.dataset.dataset_version,
        &candidate.dataset.dataset_version,
        reasons,
    );
    compare_value(
        "dataset ID",
        &baseline.dataset.dataset_id,
        &candidate.dataset.dataset_id,
        reasons,
    );
    compare_value(
        "dataset logical digest",
        &baseline.dataset.logical_digest,
        &candidate.dataset.logical_digest,
        reasons,
    );
    if baseline.dataset.logical_file_count != candidate.dataset.logical_file_count
        || baseline.dataset.logical_directory_count != candidate.dataset.logical_directory_count
        || baseline.dataset.logical_bytes != candidate.dataset.logical_bytes
    {
        reasons.push("Dataset file count, directory count, or logical bytes differ".to_string());
    }
}

fn compare_modules(
    baseline: &ComparableReport,
    candidate: &ComparableReport,
    global_reasons: &mut Vec<String>,
) -> BTreeMap<String, Vec<String>> {
    let baseline_modules = modules_by_name(baseline);
    let candidate_modules = modules_by_name(candidate);
    if baseline_modules.keys().collect::<Vec<_>>() != candidate_modules.keys().collect::<Vec<_>>() {
        global_reasons.push("Module sets differ".to_string());
        return BTreeMap::new();
    }
    let mut module_reasons = BTreeMap::new();
    for (name, baseline_module) in baseline_modules {
        let Some(candidate_module) = candidate_modules.get(name) else {
            continue;
        };
        let mut reasons = Vec::new();
        if baseline_module.workload_kind != candidate_module.workload_kind
            || baseline_module.workload_digest != candidate_module.workload_digest
        {
            reasons.push(format!(
                "Module {name} has a different workload type or digest"
            ));
        }
        if baseline_module.scan_mode != candidate_module.scan_mode {
            reasons.push(format!("Module {name} has a different scan mode"));
        }
        // fastPath identifies the optimization actually used, not workload compatibility. A
        // candidate that enables a cache or native platform path should differ from its baseline;
        // rejecting that difference would prevent the benchmark from measuring the improvement.
        // The dataset, scanMode, execution user, and result digest still establish comparability.
        if let Some(reason) = execution_user_difference(
            &baseline.environment.user_identity,
            &candidate.environment.user_identity,
            baseline_module,
            candidate_module,
        ) {
            reasons.push(format!("Module {name}: {reason}"));
        }
        let result_digest_matches = baseline_module
            .runs
            .first()
            .zip(candidate_module.runs.first())
            .is_some_and(|(baseline_run, candidate_run)| {
                baseline_run.result_digest == candidate_run.result_digest
            });
        if !result_digest_matches {
            reasons.push(format!(
                "Module {name} has different baseline and candidate result digests"
            ));
        }
        validate_module_runs("baseline", name, baseline_module, &mut reasons);
        validate_module_runs("candidate", name, candidate_module, &mut reasons);
        if !reasons.is_empty() {
            module_reasons.insert(name.to_string(), reasons);
        }
    }
    module_reasons
}

fn execution_user_difference(
    baseline_user: &str,
    candidate_user: &str,
    baseline_module: &ComparableModule,
    candidate_module: &ComparableModule,
) -> Option<&'static str> {
    if baseline_user == candidate_user {
        return None;
    }
    // Cleanup reads the execution user's real caches, application configuration, and
    // temporary directories, so changing users changes both workload and permission boundaries.
    // Fixed-dataset modules are constrained by their digest, file counts, and expected results;
    // the CI or SSH account that starts them must not invalidate otherwise reproducible output.
    let uses_user_environment = baseline_module.workload_kind == "environment"
        || candidate_module.workload_kind == "environment";
    uses_user_environment.then_some("execution user differs")
}

fn validate_module_runs(
    prefix: &str,
    name: &str,
    module: &ComparableModule,
    reasons: &mut Vec<String>,
) {
    if module.error_summary.is_some() {
        reasons.push(format!("Module {name} {prefix} execution failed"));
    }
    if module.runs.len() < REQUIRED_RUNS {
        reasons.push(format!(
            "Module {name} has fewer than {REQUIRED_RUNS} {prefix} runs"
        ));
    }
    let digest_consistent = module.runs.first().is_some_and(|first| {
        module
            .runs
            .iter()
            .all(|run| run.result_digest == first.result_digest)
    });
    if !module.summary.result_consistent_across_runs || !digest_consistent {
        reasons.push(format!(
            "Module {name} has inconsistent {prefix} result digests"
        ));
    }
    let expectation_met = module.runs.iter().all(|run| run.expectation_met);
    if !module.summary.expectation_met_across_runs || !expectation_met {
        reasons.push(format!(
            "Module {name} did not satisfy fixed {prefix} expectations"
        ));
    }
    if let Some(spread) = repeated_spread_percent(&module.runs) {
        if spread > REPEATED_SPREAD_LIMIT_PERCENT {
            reasons.push(format!(
                "Module {name} {prefix} sample spread {spread:.2}% exceeds {REPEATED_SPREAD_LIMIT_PERCENT:.0}%"
            ));
        }
    }
}

fn modules_by_name(report: &ComparableReport) -> BTreeMap<&str, &ComparableModule> {
    report
        .modules
        .iter()
        .map(|module| (module.module.as_str(), module))
        .collect()
}

fn compare_value(name: &str, baseline: &str, candidate: &str, reasons: &mut Vec<String>) {
    if baseline != candidate {
        reasons.push(format!("{name} differs"));
    }
}

fn repeated_spread_percent(runs: &[ComparableRun]) -> Option<f64> {
    let repeated = runs.get(1..)?;
    if repeated.len() < 2 {
        return None;
    }
    let minimum = repeated
        .iter()
        .map(|run| run.total_elapsed_ms)
        .min()
        .unwrap_or_default();
    let maximum = repeated
        .iter()
        .map(|run| run.total_elapsed_ms)
        .max()
        .unwrap_or_default();
    // Timer quantization exaggerates relative differences for millisecond-scale modules: 3 ms and
    // 4 ms differ by 33%, but the absolute difference is not actionable performance noise. Apply
    // the relative spread threshold only after exceeding the absolute tolerance.
    if maximum.saturating_sub(minimum) <= REPEATED_SPREAD_ABSOLUTE_TOLERANCE_MS {
        return Some(0.0);
    }
    let median = median(
        &repeated
            .iter()
            .map(|run| run.total_elapsed_ms)
            .collect::<Vec<_>>(),
    );
    (median > 0).then(|| (maximum.saturating_sub(minimum)) as f64 / median as f64 * 100.0)
}

fn render_comparison(
    baseline: &ComparableReport,
    candidate: &ComparableReport,
    gate: &ComparisonGate,
) -> String {
    let mut markdown = String::new();
    push(
        &mut markdown,
        "# MangoDisk Unified Scan Engine Benchmark Comparison",
    );
    push(&mut markdown, "");
    push(
        &mut markdown,
        "> This report compares scan results and performance without cleaning any files.",
    );
    push(&mut markdown, "");
    push(&mut markdown, "## Comparison Summary");
    push(&mut markdown, "");
    push(
        &mut markdown,
        &format!(
            "- Performance comparability: **{}**",
            match gate.comparable_module_count() {
                4 => "Comparable",
                0 => "Not comparable",
                _ => "Partially comparable",
            }
        ),
    );
    if gate.reason_count() == 0 {
        push(&mut markdown, "- Rejection reasons: none");
    } else {
        for reason in &gate.global_reasons {
            push(&mut markdown, &format!("- Rejection reason: {reason}"));
        }
        for (module, reasons) in &gate.module_reasons {
            for reason in reasons {
                push(
                    &mut markdown,
                    &format!("- {}：{reason}", module_name(module)),
                );
            }
        }
    }
    push(&mut markdown, "");
    push(&mut markdown, "## Report Metadata");
    push(&mut markdown, "");
    push(&mut markdown, "| Field | Baseline | Candidate |");
    push(&mut markdown, "|------|------|------|");
    comparison_row(&mut markdown, "Label", &baseline.label, &candidate.label);
    comparison_row(
        &mut markdown,
        "Source commit",
        &baseline.source.source_commit,
        &candidate.source.source_commit,
    );
    comparison_row(
        &mut markdown,
        "Environment ID",
        &baseline.environment.environment_id,
        &candidate.environment.environment_id,
    );
    comparison_row(
        &mut markdown,
        "Execution user",
        &baseline.environment.user_identity,
        &candidate.environment.user_identity,
    );
    comparison_row(
        &mut markdown,
        "Dataset",
        &baseline.dataset.dataset_id,
        &candidate.dataset.dataset_id,
    );
    comparison_row(
        &mut markdown,
        "Dataset digest",
        &baseline.dataset.logical_digest,
        &candidate.dataset.logical_digest,
    );

    push(&mut markdown, "");
    push(&mut markdown, "## Module Performance");
    push(&mut markdown, "");
    push(
        &mut markdown,
        "| Module | Baseline first run | Candidate first run | Change | Baseline repeated | Candidate repeated | Change | Result digest |",
    );
    push(
        &mut markdown,
        "|------|----------|----------|------|----------|----------|------|----------|",
    );
    let baseline_modules = modules_by_name(baseline);
    let candidate_modules = modules_by_name(candidate);
    for (name, baseline_module) in baseline_modules {
        let Some(candidate_module) = candidate_modules.get(name) else {
            continue;
        };
        let first_change = percent_change(
            baseline_module.summary.first_run_ms,
            candidate_module.summary.first_run_ms,
        );
        let repeated_change = match (
            baseline_module.summary.repeated_run_median_ms,
            candidate_module.summary.repeated_run_median_ms,
        ) {
            (Some(left), Some(right)) => percent_change(left, right),
            _ => None,
        };
        let result_same = baseline_module
            .runs
            .first()
            .zip(candidate_module.runs.first())
            .is_some_and(|(left, right)| left.result_digest == right.result_digest);
        let module_comparable = gate.module_comparable(name);
        push(
            &mut markdown,
            &format!(
                "| {} | {} | {} | {} | {} | {} | {} | {} |",
                module_name(name),
                duration(baseline_module.summary.first_run_ms),
                duration(candidate_module.summary.first_run_ms),
                performance_change(first_change, module_comparable),
                optional_duration(baseline_module.summary.repeated_run_median_ms),
                optional_duration(candidate_module.summary.repeated_run_median_ms),
                performance_change(repeated_change, module_comparable),
                if result_same { "Same" } else { "Changed" }
            ),
        );
    }
    for (name, baseline_module) in modules_by_name(baseline) {
        let Some(candidate_module) = candidate_modules.get(name) else {
            continue;
        };
        render_phase_comparison(
            &mut markdown,
            name,
            baseline_module,
            candidate_module,
            gate.module_comparable(name),
        );
    }
    markdown
}

fn render_phase_comparison(
    markdown: &mut String,
    module_name_value: &str,
    baseline: &ComparableModule,
    candidate: &ComparableModule,
    comparable: bool,
) {
    let phases = baseline
        .runs
        .iter()
        .flat_map(|run| run.phase_elapsed_ms.keys())
        .chain(
            candidate
                .runs
                .iter()
                .flat_map(|run| run.phase_elapsed_ms.keys()),
        )
        .cloned()
        .collect::<BTreeSet<_>>();
    if phases.is_empty() {
        return;
    }
    push(markdown, "");
    push(
        markdown,
        &format!("### {} Stage Timings", module_name(module_name_value)),
    );
    push(markdown, "");
    push(
        markdown,
        "| Stage | Baseline median | Candidate median | Change |",
    );
    push(markdown, "|------|------------|------------|------|");
    for phase in phases {
        let baseline_value = phase_median(&baseline.runs, &phase);
        let candidate_value = phase_median(&candidate.runs, &phase);
        push(
            markdown,
            &format!(
                "| `{phase}` | {} | {} | {} |",
                optional_duration(baseline_value),
                optional_duration(candidate_value),
                performance_change(
                    baseline_value
                        .zip(candidate_value)
                        .and_then(|(left, right)| { percent_change(left, right) }),
                    comparable
                )
            ),
        );
    }
}

fn phase_median(runs: &[ComparableRun], phase: &str) -> Option<u64> {
    let values = runs
        .iter()
        .filter_map(|run| run.phase_elapsed_ms.get(phase).copied())
        .collect::<Vec<_>>();
    (!values.is_empty()).then(|| median(&values))
}

fn percent_change(baseline: u64, candidate: u64) -> Option<f64> {
    (baseline > 0).then(|| (candidate as f64 - baseline as f64) / baseline as f64 * 100.0)
}

fn performance_change(change: Option<f64>, comparable: bool) -> String {
    if !comparable {
        return "No performance conclusion".to_string();
    }
    let Some(change) = change else {
        return "Unavailable".to_string();
    };
    let judgment = if change <= -PERFORMANCE_THRESHOLD_PERCENT {
        "Improvement"
    } else if change >= PERFORMANCE_THRESHOLD_PERCENT {
        "Regression"
    } else {
        "Within variance"
    };
    format!("{change:+.2}%（{judgment}）")
}

fn median(values: &[u64]) -> u64 {
    let mut sorted = values.to_vec();
    sorted.sort_unstable();
    let middle = sorted.len() / 2;
    if sorted.len().is_multiple_of(2) {
        sorted[middle - 1].saturating_add(sorted[middle]) / 2
    } else {
        sorted[middle]
    }
}

fn reject_input_overwrite(options: &EngineBenchmarkComparisonOptions) -> Result<(), String> {
    let output = absolute_path(&options.output_path)?;
    for input in [&options.baseline_path, &options.candidate_path] {
        if absolute_path(input)? == output {
            return Err(
                "engine benchmark comparison output cannot overwrite an input JSON report"
                    .to_string(),
            );
        }
    }
    Ok(())
}

fn absolute_path(path: &Path) -> Result<PathBuf, String> {
    if path.exists() {
        return fs::canonicalize(path)
            .map_err(|error| format!("failed to canonicalize {}: {error}", path.display()));
    }
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let parent = fs::canonicalize(parent)
        .map_err(|error| format!("failed to canonicalize {}: {error}", parent.display()))?;
    Ok(parent.join(
        path.file_name()
            .ok_or_else(|| format!("output path {} has no file name", path.display()))?,
    ))
}

fn write_atomic_replace(path: &Path, content: &[u8]) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .map_err(|error| format!("failed to create {}: {error}", parent.display()))?;
    }
    let temporary = path.with_extension(format!("tmp-{}-{}", std::process::id(), now_ms()));
    fs::write(&temporary, content).map_err(|error| {
        format!(
            "failed to write temporary benchmark comparison {}: {error}",
            temporary.display()
        )
    })?;
    if path.exists() {
        let backup = path.with_extension(format!("backup-{}-{}", std::process::id(), now_ms()));
        fs::rename(path, &backup).map_err(|error| {
            format!(
                "failed to back up existing benchmark comparison {}: {error}",
                path.display()
            )
        })?;
        match fs::rename(&temporary, path) {
            Ok(()) => {
                let _ = fs::remove_file(backup);
                return Ok(());
            }
            Err(error) => {
                let _ = fs::rename(&backup, path);
                let _ = fs::remove_file(&temporary);
                return Err(format!(
                    "failed to replace benchmark comparison {}: {error}",
                    path.display()
                ));
            }
        }
    }
    fs::rename(&temporary, path).map_err(|error| {
        format!(
            "failed to save benchmark comparison {}: {error}",
            path.display()
        )
    })
}

fn module_name(value: &str) -> &'static str {
    match value {
        "deepClean" => "Deep Clean",
        "diskAnalysis" => "Disk Analysis",
        "largeFiles" => "Large Files",
        "duplicateFiles" => "Duplicate Files",
        _ => "Unknown Module",
    }
}

fn duration(milliseconds: u64) -> String {
    if milliseconds < 1_000 {
        format!("{milliseconds} ms")
    } else {
        format!("{:.2} s", milliseconds as f64 / 1_000.0)
    }
}

fn optional_duration(milliseconds: Option<u64>) -> String {
    milliseconds
        .map(duration)
        .unwrap_or_else(|| "None".to_string())
}

fn comparison_row(markdown: &mut String, name: &str, baseline: &str, candidate: &str) {
    push(
        markdown,
        &format!(
            "| {name} | {} | {} |",
            table_value(baseline),
            table_value(candidate)
        ),
    );
}

fn table_value(value: &str) -> String {
    value.replace('|', "\\|").replace('\n', " ")
}

fn push(markdown: &mut String, value: &str) {
    markdown.push_str(value);
    markdown.push('\n');
}

fn file_name(path: &Path) -> String {
    path.file_name()
        .map(|value| value.to_string_lossy().into_owned())
        .unwrap_or_else(|| "unknown".to_string())
}

#[cfg(test)]
mod tests {
    use super::{
        execution_user_difference, validate_module_runs, ComparableModule, ComparableRun,
        ComparableSummary, ComparisonGate, REQUIRED_RUNS,
    };
    use std::collections::BTreeMap;

    fn valid_module() -> ComparableModule {
        let runs = (0..REQUIRED_RUNS)
            .map(|_| ComparableRun {
                total_elapsed_ms: 100,
                result_digest: "stable-result".to_string(),
                phase_elapsed_ms: BTreeMap::new(),
                expectation_met: true,
            })
            .collect();
        ComparableModule {
            module: "diskAnalysis".to_string(),
            workload_kind: "fixedDataset".to_string(),
            workload_digest: "fixed-workload".to_string(),
            scan_mode: "recursiveTraversalAndAggregate".to_string(),
            error_summary: None,
            summary: ComparableSummary {
                result_consistent_across_runs: true,
                expectation_met_across_runs: true,
                first_run_ms: 100,
                repeated_run_median_ms: Some(100),
            },
            runs,
        }
    }

    #[test]
    fn module_error_summary_always_blocks_performance_comparison() {
        let mut module = valid_module();
        module.error_summary = Some(serde_json::json!({
            "code": "scanFailed",
            "digest": "redacted"
        }));
        let mut reasons = Vec::new();
        validate_module_runs("candidate", &module.module, &module, &mut reasons);
        assert!(
            reasons
                .iter()
                .any(|reason| reason.contains("execution failed")),
            "a module error summary must block performance comparison"
        );
    }

    #[test]
    fn stable_repeated_module_passes_run_validation() {
        let module = valid_module();
        let mut reasons = Vec::new();
        validate_module_runs("candidate", &module.module, &module, &mut reasons);
        assert!(reasons.is_empty());
    }

    #[test]
    fn millisecond_scale_spread_uses_absolute_tolerance() {
        let mut module = valid_module();
        module.runs[1].total_elapsed_ms = 3;
        module.runs[2].total_elapsed_ms = 4;
        let mut reasons = Vec::new();
        validate_module_runs("candidate", &module.module, &module, &mut reasons);
        assert!(
            reasons.is_empty(),
            "a 1 ms timing difference must not become incomparable through relative percentages"
        );
    }

    #[test]
    fn environment_result_change_only_blocks_affected_module() {
        let gate = ComparisonGate {
            global_reasons: Vec::new(),
            module_reasons: BTreeMap::from([(
                "deepClean".to_string(),
                vec!["User-environment results changed".to_string()],
            )]),
        };
        assert!(!gate.module_comparable("deepClean"));
        assert!(gate.module_comparable("diskAnalysis"));
        assert_eq!(gate.comparable_module_count(), 3);
    }

    #[test]
    fn execution_user_change_only_blocks_environment_workload() {
        let mut environment_module = valid_module();
        environment_module.workload_kind = "environment".to_string();
        assert_eq!(
            execution_user_difference(
                "desktop-user",
                "automation-user",
                &environment_module,
                &environment_module
            ),
            Some("execution user differs")
        );

        let fixed_module = valid_module();
        assert_eq!(
            execution_user_difference(
                "desktop-user",
                "automation-user",
                &fixed_module,
                &fixed_module
            ),
            None,
            "fixed datasets are constrained independently of the account that starts the process"
        );
    }
}

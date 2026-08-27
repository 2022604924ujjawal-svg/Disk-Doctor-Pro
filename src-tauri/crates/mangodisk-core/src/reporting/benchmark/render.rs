use std::{collections::BTreeSet, fmt::Write};

use super::report::{EngineBenchmarkReport, ModuleBenchmarkReport};

pub(crate) fn render_markdown(report: &EngineBenchmarkReport) -> String {
    let mut markdown = String::new();
    line(&mut markdown, "# MangoDisk Unified Scan Engine Benchmark");
    line(&mut markdown, "");
    line(
        &mut markdown,
        "> This benchmark scans and hashes files but never cleans them. Markdown output excludes full user paths.",
    );
    line(&mut markdown, "");
    line(&mut markdown, "## Report Metadata");
    line(&mut markdown, "");
    line(&mut markdown, "| Field | Value |");
    line(&mut markdown, "|------|------|");
    row(&mut markdown, "Label", &report.label);
    row(&mut markdown, "Generated at", &report.generated_at_local);
    row(&mut markdown, "Source commit", &report.source.source_commit);
    row(&mut markdown, "Build profile", &report.source.build_profile);
    row(
        &mut markdown,
        "Dirty worktree at build time",
        yes_no(report.source.source_dirty_at_build),
    );
    row(
        &mut markdown,
        "Environment ID",
        &report.environment.environment_id,
    );
    row(
        &mut markdown,
        "Execution user",
        &report.environment.user_identity,
    );
    row(
        &mut markdown,
        "Platform / architecture",
        &format!(
            "{} / {}",
            report.environment.os, report.environment.architecture
        ),
    );
    row(
        &mut markdown,
        "Operating system version",
        &report.environment.os_version,
    );
    row(&mut markdown, "CPU", &report.environment.cpu_model);
    row(
        &mut markdown,
        "Logical CPUs",
        &report.environment.logical_cpu_count.to_string(),
    );
    row(&mut markdown, "Dataset", &report.dataset.dataset_id);
    row(
        &mut markdown,
        "Dataset digest",
        &report.dataset.logical_digest,
    );
    row(
        &mut markdown,
        "Dataset files / directories",
        &format!(
            "{} / {}",
            report.dataset.logical_file_count, report.dataset.logical_directory_count
        ),
    );
    row(
        &mut markdown,
        "Dataset logical bytes",
        &format_bytes(report.dataset.logical_bytes),
    );
    row(
        &mut markdown,
        "Dataset allocated bytes",
        &report
            .dataset
            .allocated_bytes
            .map(format_bytes)
            .unwrap_or_else(|| "Unavailable".to_string()),
    );
    row(
        &mut markdown,
        "Boundary features",
        &format!(
            "sparse file {}, hard link {}, symbolic link {}, restricted directory {}",
            report.dataset.sparse_files_created,
            report.dataset.hard_links_created,
            report.dataset.symbolic_links_created,
            report.dataset.permission_restricted_directories
        ),
    );
    if !report.dataset.unsupported_features.is_empty() {
        row(
            &mut markdown,
            "Unsupported boundary features",
            &report.dataset.unsupported_features.join("、"),
        );
    }
    if let Some(note) = &report.note {
        row(&mut markdown, "Notes", note);
    }

    line(&mut markdown, "");
    line(&mut markdown, "## Module Results");
    line(&mut markdown, "");
    line(
        &mut markdown,
        "| Module | Status | Workload | First run | Repeated median | First result | Stable result | Expected result |",
    );
    line(
        &mut markdown,
        "|------|------|--------|------|------------|----------|----------|----------|",
    );
    for module in &report.modules {
        let first_result = module
            .runs
            .first()
            .and_then(|run| run.first_result_ms)
            .map(format_duration)
            .unwrap_or_else(|| "None".to_string());
        let _ = writeln!(
            markdown,
            "| {} | {} | {} | {} | {} | {} | {} | {} |",
            module_name(module.module),
            if module.error_summary.is_some() {
                "Failed"
            } else {
                "Completed"
            },
            workload_name(module.workload_kind),
            format_duration(module.summary.first_run_ms),
            module
                .summary
                .repeated_run_median_ms
                .map(format_duration)
                .unwrap_or_else(|| "None".to_string()),
            first_result,
            yes_no(module.summary.result_consistent_across_runs),
            expectation_status(module)
        );
    }
    for module in &report.modules {
        render_module(&mut markdown, module);
    }
    markdown
}

fn render_module(markdown: &mut String, module: &ModuleBenchmarkReport) {
    line(markdown, "");
    line(markdown, &format!("## {}", module_name(module.module)));
    line(markdown, "");
    line(
        markdown,
        &format!(
            "- Workload: {} (digest `{}`)",
            workload_name(module.workload_kind),
            module.workload_digest
        ),
    );
    line(markdown, &format!("- Scan mode: `{}`", module.scan_mode));
    line(markdown, &format!("- Fast path: `{}`", module.fast_path));
    if let Some(error) = &module.error_summary {
        line(
            markdown,
            &format!(
                "- Execution error: `{}` (digest `{}`)",
                error.code, error.digest
            ),
        );
    }
    line(
        markdown,
        &format!(
            "- Median throughput: {} files/s, {} logical MB/s",
            optional_number(module.summary.files_per_second_median),
            optional_number(module.summary.logical_megabytes_per_second_median)
        ),
    );
    line(markdown, "");
    line(
        markdown,
        "| Run | First progress | First valid result | Total | Files traversed | Bytes observed | Result count | Result bytes | Skipped | Expected |",
    );
    line(
        markdown,
        "|------|----------|--------------|--------|----------|----------|--------|----------|------|------|",
    );
    for run in &module.runs {
        let _ = writeln!(
            markdown,
            "| {} | {} | {} | {} | {} | {} | {} | {} | {} | {} |",
            run.run_number,
            optional_duration(run.first_progress_ms),
            optional_duration(run.first_result_ms),
            format_duration(run.total_elapsed_ms),
            run.files_visited,
            format_bytes(run.bytes_observed),
            run.result_count,
            format_bytes(run.result_bytes),
            run.skipped_count,
            yes_no(run.expectation_met)
        );
    }
    let phase_names = module
        .runs
        .iter()
        .flat_map(|run| run.phase_elapsed_ms.keys().cloned())
        .collect::<BTreeSet<_>>();
    if !phase_names.is_empty() {
        line(markdown, "");
        line(markdown, "### Stage Timings");
        line(markdown, "");
        let mut header = String::from("| Run |");
        let mut separator = String::from("|------|");
        for phase in &phase_names {
            let _ = write!(header, " `{phase}` |");
            separator.push_str("------|");
        }
        line(markdown, &header);
        line(markdown, &separator);
        for run in &module.runs {
            let mut row = format!("| {} |", run.run_number);
            for phase in &phase_names {
                let value = run
                    .phase_elapsed_ms
                    .get(phase)
                    .copied()
                    .map(format_duration)
                    .unwrap_or_else(|| "None".to_string());
                let _ = write!(row, " {value} |");
            }
            line(markdown, &row);
        }
    }
    let work_metric_names = module
        .runs
        .iter()
        .flat_map(|run| run.work_metrics.keys().cloned())
        .collect::<BTreeSet<_>>();
    if !work_metric_names.is_empty() {
        line(markdown, "");
        line(markdown, "### Workload Metrics");
        line(markdown, "");
        let mut header = String::from("| Run |");
        let mut separator = String::from("|------|");
        for metric in &work_metric_names {
            let _ = write!(header, " `{metric}` |");
            separator.push_str("------|");
        }
        line(markdown, &header);
        line(markdown, &separator);
        for run in &module.runs {
            let mut row = format!("| {} |", run.run_number);
            for metric in &work_metric_names {
                let value = run
                    .work_metrics
                    .get(metric)
                    .copied()
                    .map(|value| value.to_string())
                    .unwrap_or_else(|| "None".to_string());
                let _ = write!(row, " {value} |");
            }
            line(markdown, &row);
        }
    }
    if !module.detail_metrics.is_empty() {
        line(markdown, "");
        let (title, id_label) = if module.module == "deepClean" {
            ("### Slowest Rules (Top 10)", "Rule ID")
        } else {
            ("### Detailed Metrics", "Metric ID")
        };
        line(markdown, title);
        line(markdown, "");
        line(
            markdown,
            &format!("| {id_label} | Median duration | Files | Space | Stable across runs |"),
        );
        line(
            markdown,
            "|---------|------------|------|------|----------|",
        );
        for detail in module.detail_metrics.iter().take(10) {
            let _ = writeln!(
                markdown,
                "| `{}` | {} | {} | {} | {} |",
                detail.id,
                format_duration(detail.median_elapsed_ms),
                detail.result_count,
                format_bytes(detail.result_bytes),
                yes_no(detail.result_consistent_across_runs)
            );
        }
    }
    if !module.phase_notes.is_empty() {
        line(markdown, "");
        for note in &module.phase_notes {
            line(markdown, &format!("- {note}"));
        }
    }
}

fn module_name(value: &str) -> &'static str {
    match value {
        "deepClean" => "Deep clean",
        "diskAnalysis" => "Disk analysis",
        "largeFiles" => "Large files",
        "duplicateFiles" => "Duplicate files",
        _ => "Unknown module",
    }
}

fn workload_name(value: &str) -> &'static str {
    match value {
        "environment" => "User environment",
        "fixedDataset" => "Fixed dataset",
        _ => "Unknown workload",
    }
}

fn optional_duration(value: Option<u64>) -> String {
    value
        .map(format_duration)
        .unwrap_or_else(|| "None".to_string())
}

fn optional_number(value: Option<u64>) -> String {
    value.map_or_else(|| "None".to_string(), |number| number.to_string())
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
    let mut index = 0;
    while value >= 1024.0 && index < UNITS.len() - 1 {
        value /= 1024.0;
        index += 1;
    }
    if index == 0 {
        format!("{bytes} B")
    } else {
        format!("{value:.2} {}", UNITS[index])
    }
}

fn expectation_status(module: &ModuleBenchmarkReport) -> &'static str {
    if module.expected_result.result_count.is_none()
        && module.expected_result.result_bytes.is_none()
    {
        "N/A"
    } else {
        yes_no(module.summary.expectation_met_across_runs)
    }
}

fn yes_no(value: bool) -> &'static str {
    if value {
        "Yes"
    } else {
        "No"
    }
}

fn row(markdown: &mut String, name: &str, value: &str) {
    let safe = value.replace('|', "\\|").replace('\n', " ");
    let _ = writeln!(markdown, "| {name} | {safe} |");
}

fn line(markdown: &mut String, value: &str) {
    markdown.push_str(value);
    markdown.push('\n');
}

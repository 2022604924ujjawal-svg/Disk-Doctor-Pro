use std::collections::BTreeMap;

use serde::Serialize;

use super::{dataset::BenchmarkDatasetManifest, system::BenchmarkEnvironment};
use crate::{filesystem::DiskInfo, reporting::benchmark::system::BenchmarkSourceInfo};

pub(crate) const BENCHMARK_SCHEMA_VERSION: &str = "1.0";
pub(crate) const BENCHMARK_REPORT_KIND: &str = "engine-suite";

#[derive(Debug, Clone)]
pub struct EngineBenchmarkOptions {
    pub label: String,
    pub environment_id: Option<String>,
    pub note: Option<String>,
    pub runs: usize,
    pub dataset_manifest_path: std::path::PathBuf,
    pub output_directory: std::path::PathBuf,
    pub source: BenchmarkSourceInfo,
}

#[derive(Debug)]
pub struct EngineBenchmarkArtifacts {
    pub json_path: std::path::PathBuf,
    pub markdown_path: std::path::PathBuf,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct EngineBenchmarkReport {
    pub(crate) schema_version: &'static str,
    pub(crate) report_kind: &'static str,
    pub(crate) label: String,
    pub(crate) note: Option<String>,
    pub(crate) generated_at_ms: u64,
    pub(crate) generated_at_local: String,
    pub(crate) source: BenchmarkSourceInfo,
    pub(crate) environment: BenchmarkEnvironment,
    pub(crate) disk: DiskInfo,
    pub(crate) dataset: BenchmarkDatasetSummary,
    pub(crate) modules: Vec<ModuleBenchmarkReport>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct BenchmarkDatasetSummary {
    pub(crate) dataset_version: String,
    pub(crate) dataset_id: String,
    pub(crate) seed: u64,
    pub(crate) logical_digest: String,
    pub(crate) logical_file_count: u64,
    pub(crate) logical_directory_count: u64,
    pub(crate) logical_bytes: u64,
    pub(crate) allocated_bytes: Option<u64>,
    pub(crate) expected_large_file_count: u64,
    pub(crate) expected_large_file_bytes: u64,
    pub(crate) expected_duplicate_group_count: u64,
    pub(crate) expected_duplicate_file_count: u64,
    pub(crate) expected_reclaimable_bytes: u64,
    pub(crate) sparse_files_created: u64,
    pub(crate) hard_links_created: u64,
    pub(crate) symbolic_links_created: u64,
    pub(crate) permission_restricted_directories: u64,
    pub(crate) unsupported_features: Vec<String>,
}

impl From<&BenchmarkDatasetManifest> for BenchmarkDatasetSummary {
    fn from(value: &BenchmarkDatasetManifest) -> Self {
        Self {
            dataset_version: value.dataset_version.clone(),
            dataset_id: value.dataset_id.clone(),
            seed: value.seed,
            logical_digest: value.logical_digest.clone(),
            logical_file_count: value.logical_file_count,
            logical_directory_count: value.logical_directory_count,
            logical_bytes: value.logical_bytes,
            allocated_bytes: value.allocated_bytes,
            expected_large_file_count: value.expected_large_file_count,
            expected_large_file_bytes: value.expected_large_file_bytes,
            expected_duplicate_group_count: value.expected_duplicate_group_count,
            expected_duplicate_file_count: value.expected_duplicate_file_count,
            expected_reclaimable_bytes: value.expected_reclaimable_bytes,
            sparse_files_created: value.features.sparse_files_created,
            hard_links_created: value.features.hard_links_created,
            symbolic_links_created: value.features.symbolic_links_created,
            permission_restricted_directories: value.features.permission_restricted_directories,
            unsupported_features: value.features.unsupported_features.clone(),
        }
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ModuleBenchmarkReport {
    pub(crate) module: &'static str,
    pub(crate) workload_kind: &'static str,
    pub(crate) workload_digest: String,
    pub(crate) scan_mode: &'static str,
    pub(crate) fast_path: &'static str,
    pub(crate) expected_result: BenchmarkExpectation,
    pub(crate) error_summary: Option<BenchmarkErrorSummary>,
    pub(crate) summary: ModuleBenchmarkSummary,
    pub(crate) runs: Vec<ModuleBenchmarkRun>,
    pub(crate) detail_metrics: Vec<BenchmarkDetailMetric>,
    pub(crate) phase_notes: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct BenchmarkErrorSummary {
    pub(crate) code: &'static str,
    pub(crate) digest: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct BenchmarkDetailMetric {
    pub(crate) id: String,
    pub(crate) median_elapsed_ms: u64,
    pub(crate) result_count: u64,
    pub(crate) result_bytes: u64,
    pub(crate) result_consistent_across_runs: bool,
}

#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct BenchmarkExpectation {
    pub(crate) result_count: Option<u64>,
    pub(crate) result_bytes: Option<u64>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ModuleBenchmarkSummary {
    pub(crate) result_consistent_across_runs: bool,
    pub(crate) expectation_met_across_runs: bool,
    pub(crate) first_run_ms: u64,
    pub(crate) repeated_run_median_ms: Option<u64>,
    pub(crate) median_ms: u64,
    pub(crate) minimum_ms: u64,
    pub(crate) maximum_ms: u64,
    pub(crate) mean_ms: u64,
    pub(crate) files_per_second_median: Option<u64>,
    pub(crate) logical_megabytes_per_second_median: Option<u64>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ModuleBenchmarkRun {
    pub(crate) run_number: usize,
    pub(crate) first_progress_ms: Option<u64>,
    pub(crate) first_result_ms: Option<u64>,
    pub(crate) total_elapsed_ms: u64,
    pub(crate) files_visited: u64,
    pub(crate) bytes_observed: u64,
    pub(crate) result_count: u64,
    pub(crate) result_bytes: u64,
    pub(crate) skipped_count: u64,
    pub(crate) result_digest: String,
    pub(crate) phase_elapsed_ms: BTreeMap<String, u64>,
    /// Phase timings alone cannot show whether I/O work actually decreased. This extension stores
    /// path-free counts and byte totals only. The comparator treats the field as an empty map when
    /// reading older 1.0 reports, which preserves archived baseline compatibility.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub(crate) work_metrics: BTreeMap<String, u64>,
    pub(crate) expectation_met: bool,
}

pub(crate) fn summarize_runs(
    runs: &[ModuleBenchmarkRun],
    input_files: u64,
    input_bytes: u64,
) -> ModuleBenchmarkSummary {
    let durations = runs
        .iter()
        .map(|run| run.total_elapsed_ms)
        .collect::<Vec<_>>();
    let repeated = durations.get(1..).filter(|values| !values.is_empty());
    let median_ms = median(&durations);
    ModuleBenchmarkSummary {
        result_consistent_across_runs: runs.first().is_some_and(|first| {
            runs.iter()
                .all(|run| run.result_digest == first.result_digest)
        }),
        expectation_met_across_runs: runs.iter().all(|run| run.expectation_met),
        first_run_ms: durations.first().copied().unwrap_or_default(),
        repeated_run_median_ms: repeated.map(median),
        median_ms,
        minimum_ms: durations.iter().copied().min().unwrap_or_default(),
        maximum_ms: durations.iter().copied().max().unwrap_or_default(),
        mean_ms: if durations.is_empty() {
            0
        } else {
            durations.iter().sum::<u64>() / durations.len() as u64
        },
        files_per_second_median: rate_per_second(input_files, median_ms),
        logical_megabytes_per_second_median: rate_per_second(
            input_bytes / (1024 * 1024),
            median_ms,
        ),
    }
}

fn rate_per_second(value: u64, elapsed_ms: u64) -> Option<u64> {
    (elapsed_ms > 0).then(|| value.saturating_mul(1_000) / elapsed_ms)
}

fn median(values: &[u64]) -> u64 {
    if values.is_empty() {
        return 0;
    }
    let mut sorted = values.to_vec();
    sorted.sort_unstable();
    let middle = sorted.len() / 2;
    if sorted.len().is_multiple_of(2) {
        sorted[middle - 1].saturating_add(sorted[middle]) / 2
    } else {
        sorted[middle]
    }
}

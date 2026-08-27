use std::{
    io::{self, IsTerminal, Write},
    path::Path,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
};

use mangodisk_core::{
    diagnostic_path, CleanupExecutionProgress, CleanupExecutionStage, OperationCancellationToken,
    ProgressSink, TraversalProgress,
};
use serde_json::json;

use crate::{arguments::OutputFormat, output::OUTPUT_SCHEMA_VERSION};

#[derive(Clone, Default)]
pub struct CancellationController {
    active: Arc<Mutex<Option<OperationCancellationToken>>>,
    cancelled: Arc<AtomicBool>,
}

impl CancellationController {
    pub fn install() -> Result<Self, String> {
        let controller = Self::default();
        let signal_controller = controller.clone();
        ctrlc::set_handler(move || signal_controller.cancel())
            .map_err(|error| format!("failed to install the cancellation handler: {error}"))?;
        Ok(controller)
    }

    pub fn activate(&self, token: OperationCancellationToken) -> ActiveCancellation<'_> {
        if let Ok(mut active) = self.active.lock() {
            *active = Some(token);
        }
        // Ctrl+C can arrive after argument parsing but before Core creates its
        // operation guard. Preserve that intent and cancel the newly activated
        // operation instead of resetting the signal during adapter hand-off.
        if self.cancelled.load(Ordering::Relaxed) {
            token.cancel();
        }
        ActiveCancellation { controller: self }
    }

    pub fn was_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Relaxed)
    }

    fn cancel(&self) {
        self.cancelled.store(true, Ordering::Relaxed);
        if let Ok(active) = self.active.lock() {
            if let Some(token) = *active {
                token.cancel();
            }
        }
    }
}

pub struct ActiveCancellation<'a> {
    controller: &'a CancellationController,
}

impl Drop for ActiveCancellation<'_> {
    fn drop(&mut self) {
        if let Ok(mut active) = self.controller.active.lock() {
            *active = None;
        }
    }
}

pub struct CliProgressSink {
    format: OutputFormat,
    enabled: bool,
    include_full_paths: bool,
    interactive: bool,
    rendered_line: Mutex<bool>,
}

impl CliProgressSink {
    pub fn new(format: OutputFormat, enabled: bool, include_full_paths: bool) -> Self {
        Self {
            format,
            enabled,
            include_full_paths,
            interactive: format == OutputFormat::Human && io::stderr().is_terminal(),
            rendered_line: Mutex::new(false),
        }
    }

    pub fn begin_scan(&self) {
        if !self.enabled || self.format != OutputFormat::Human {
            return;
        }
        if self.interactive {
            eprint!("\r\x1b[2KPreparing cleanup scan...");
            let _ = io::stderr().flush();
            if let Ok(mut rendered) = self.rendered_line.lock() {
                *rendered = true;
            }
        } else {
            eprintln!("Preparing cleanup scan...");
        }
    }
}

impl ProgressSink for CliProgressSink {
    fn report(&self, progress: TraversalProgress) {
        if !self.enabled {
            return;
        }
        let path = if self.include_full_paths {
            progress.current_path.clone()
        } else {
            diagnostic_path(Path::new(&progress.current_path))
        };
        match self.format {
            OutputFormat::Human if self.interactive => {
                eprint!(
                    "\r\x1b[2KScanning {} items ({}) · {}",
                    progress.items_scanned,
                    format_bytes(progress.bytes_scanned),
                    path
                );
                let _ = io::stderr().flush();
                if let Ok(mut rendered) = self.rendered_line.lock() {
                    *rendered = true;
                }
            }
            OutputFormat::Human => eprintln!(
                "Scanning {} items ({}), found {} · {}",
                progress.items_scanned,
                format_bytes(progress.bytes_scanned),
                progress.found_items,
                path
            ),
            OutputFormat::Json | OutputFormat::Jsonl => eprintln!(
                "{}",
                json!({
                    "schemaVersion": OUTPUT_SCHEMA_VERSION,
                    "type": "progress",
                    "data": {
                        "operationId": progress.operation_id,
                        "currentPath": path,
                        "itemsScanned": progress.items_scanned,
                        "bytesScanned": progress.bytes_scanned,
                        "completedSteps": progress.completed_steps,
                        "totalSteps": progress.total_steps,
                        "foundItems": progress.found_items,
                        "foundBytes": progress.found_bytes,
                        "elapsedMs": progress.elapsed_ms,
                    }
                })
            ),
        }
    }
}

pub struct CliCleanupProgress {
    format: OutputFormat,
    enabled: bool,
    include_full_paths: bool,
    interactive: bool,
    rendered_line: bool,
    last_non_interactive_state: Option<(CleanupExecutionStage, Option<String>, u64)>,
}

impl CliCleanupProgress {
    pub fn new(format: OutputFormat, enabled: bool, include_full_paths: bool) -> Self {
        Self {
            format,
            enabled,
            include_full_paths,
            interactive: format == OutputFormat::Human && io::stderr().is_terminal(),
            rendered_line: false,
            last_non_interactive_state: None,
        }
    }

    pub fn begin(&mut self) {
        if !self.enabled || self.format != OutputFormat::Human {
            return;
        }
        self.write_human("Preparing cleanup...");
    }

    pub fn report(&mut self, mut progress: CleanupExecutionProgress, rule_label: Option<&str>) {
        if !self.enabled {
            return;
        }
        if !self.include_full_paths {
            progress.current_item_path = progress
                .current_item_path
                .as_deref()
                .map(Path::new)
                .map(diagnostic_path);
        }
        match self.format {
            OutputFormat::Human if self.interactive => {
                self.write_human(&cleanup_progress_message(&progress, rule_label));
            }
            OutputFormat::Human => {
                let state = (
                    progress.stage,
                    progress.current_rule_id.clone(),
                    progress.completed_rule_count,
                );
                if self.last_non_interactive_state.as_ref() != Some(&state) {
                    eprintln!("{}", cleanup_progress_message(&progress, rule_label));
                    self.last_non_interactive_state = Some(state);
                }
            }
            OutputFormat::Json | OutputFormat::Jsonl => eprintln!(
                "{}",
                json!({
                    "schemaVersion": OUTPUT_SCHEMA_VERSION,
                    "type": "progress",
                    "data": progress,
                })
            ),
        }
    }

    fn write_human(&mut self, message: &str) {
        if self.interactive {
            eprint!("\r\x1b[2K{message}");
            let _ = io::stderr().flush();
            self.rendered_line = true;
        } else {
            eprintln!("{message}");
        }
    }
}

impl Drop for CliCleanupProgress {
    fn drop(&mut self) {
        if self.interactive && self.rendered_line {
            eprint!("\r\x1b[2K");
            let _ = io::stderr().flush();
        }
    }
}

fn cleanup_progress_message(
    progress: &CleanupExecutionProgress,
    rule_label: Option<&str>,
) -> String {
    let elapsed = format_duration(progress.elapsed_ms);
    match progress.stage {
        CleanupExecutionStage::Validating => format!(
            "Validating cleanup {}/{} rules · {} items checked ({}) · {elapsed}",
            progress.validated_rule_count,
            progress.total_rule_count,
            progress.checked_item_count,
            format_bytes(progress.checked_bytes)
        ),
        CleanupExecutionStage::Cleaning => {
            let rule = rule_label.unwrap_or("cleanup item");
            format!(
                "Cleaning · {rule} · {}/{} rules complete · {} items affected · {} reclaimed · {elapsed}",
                progress.completed_rule_count,
                progress.total_rule_count,
                progress.affected_item_count,
                format_bytes(progress.released_bytes)
            )
        }
        CleanupExecutionStage::Finalizing => format!(
            "Finalizing cleanup · {}/{} rules complete · {} items affected · {} reclaimed · {elapsed}",
            progress.completed_rule_count,
            progress.total_rule_count,
            progress.affected_item_count,
            format_bytes(progress.released_bytes)
        ),
    }
}

impl Drop for CliProgressSink {
    fn drop(&mut self) {
        if self.interactive && self.rendered_line.get_mut().is_ok_and(|rendered| *rendered) {
            eprint!("\r\x1b[2K");
            let _ = io::stderr().flush();
        }
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
        format!("{value:.1} {}", UNITS[unit])
    }
}

fn format_duration(elapsed_ms: u64) -> String {
    if elapsed_ms < 1_000 {
        format!("{elapsed_ms} ms")
    } else {
        format!("{:.1} s", elapsed_ms as f64 / 1_000.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn activation_preserves_a_signal_received_before_core_starts() {
        let controller = CancellationController::default();
        controller.cancel();

        let _active = controller.activate(OperationCancellationToken::cleanup_scan());

        assert!(controller.was_cancelled());
    }

    #[test]
    fn progress_bytes_use_compact_units() {
        assert_eq!(format_bytes(512), "512 B");
        assert_eq!(format_bytes(1_572_864), "1.5 MB");
    }

    #[test]
    fn cleanup_progress_reports_stage_counts_and_reclaimed_space() {
        let message = cleanup_progress_message(
            &CleanupExecutionProgress {
                stage: CleanupExecutionStage::Cleaning,
                planned_rule_ids: vec!["development.npm-cache".to_string()],
                current_rule_id: Some("development.npm-cache".to_string()),
                current_item_path: None,
                current_rule_affected_item_count: 42,
                current_rule_released_bytes: 1_572_864,
                completed_rule_results: Vec::new(),
                validated_rule_count: 1,
                completed_rule_count: 0,
                total_rule_count: 1,
                checked_item_count: 42,
                checked_bytes: 1_572_864,
                affected_item_count: 42,
                released_bytes: 1_572_864,
                elapsed_ms: 1_250,
            },
            Some("npm Cache"),
        );

        assert_eq!(
            message,
            "Cleaning · npm Cache · 0/1 rules complete · 42 items affected · 1.5 MB reclaimed · 1.2 s"
        );
    }
}

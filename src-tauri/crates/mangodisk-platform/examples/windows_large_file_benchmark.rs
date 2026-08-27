#[cfg(not(windows))]
fn main() {
    println!("windows_large_file_benchmark status=unsupported platform=non_windows");
}

#[cfg(windows)]
fn main() {
    if let Err(error) = windows_benchmark::run() {
        eprintln!("windows_large_file_benchmark status=failed error={error}");
        std::process::exit(1);
    }
}

#[cfg(windows)]
mod windows_benchmark {
    use std::{
        env, fs,
        os::windows::ffi::OsStrExt,
        path::{Path, PathBuf},
        time::Instant,
    };

    use mangodisk_platform::{current_platform, LargeFileCandidateScanError, Platform};
    use windows_sys::Win32::System::{
        ProcessStatus::{K32GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS},
        Threading::GetCurrentProcess,
    };

    const DEFAULT_MINIMUM_MIB: u64 = 50;
    static LOGGER: BenchmarkLogger = BenchmarkLogger;

    #[derive(Clone, Copy, PartialEq, Eq)]
    enum BenchmarkMode {
        Scan,
        CancelImmediately,
        FailConsumer,
    }

    struct BenchmarkLogger;

    impl log::Log for BenchmarkLogger {
        fn enabled(&self, metadata: &log::Metadata<'_>) -> bool {
            metadata.level() <= log::Level::Info
        }

        fn log(&self, record: &log::Record<'_>) {
            if self.enabled(record.metadata()) {
                eprintln!("{} {}", record.level(), record.args());
            }
        }

        fn flush(&self) {}
    }

    pub(super) fn run() -> Result<(), String> {
        // The benchmark is not a Tauri application and does not initialize
        // tauri-plugin-log. Install a minimal logger so native fallbacks remain
        // observable without modifying production logging initialization.
        log::set_logger(&LOGGER)
            .map(|()| log::set_max_level(log::LevelFilter::Info))
            .map_err(|error| format!("benchmark_logger_initialization_failed error={error}"))?;
        let root = env::args()
            .nth(1)
            .map(PathBuf::from)
            .unwrap_or_else(|| current_platform().system_volume_path());
        let minimum_mib = env::args()
            .nth(2)
            .map(|value| {
                value
                    .parse::<u64>()
                    .map_err(|_| format!("invalid_minimum_file_size_mib value={value}"))
            })
            .transpose()?
            .unwrap_or(DEFAULT_MINIMUM_MIB);
        let minimum_bytes = minimum_mib
            .checked_mul(1024 * 1024)
            .ok_or_else(|| "minimum_file_size_out_of_range".to_string())?;
        let mode = match env::args().nth(3).as_deref() {
            None | Some("scan") => BenchmarkMode::Scan,
            Some("cancel") => BenchmarkMode::CancelImmediately,
            Some("consumer-fail") => BenchmarkMode::FailConsumer,
            Some(value) => return Err(format!("unsupported_benchmark_mode value={value}")),
        };

        let mut results = Vec::new();
        let mut metadata_skipped = 0u64;
        let started = Instant::now();
        let scan_result = current_platform().fast_large_file_candidates(
            &root,
            minimum_bytes,
            &|| mode == BenchmarkMode::CancelImmediately,
            &mut |path| {
                if mode == BenchmarkMode::FailConsumer {
                    return Err("benchmark_consumer_failure".to_string());
                }
                let Ok(metadata) = fs::symlink_metadata(&path) else {
                    metadata_skipped = metadata_skipped.saturating_add(1);
                    return Ok(());
                };
                if !metadata.is_file()
                    || current_platform().is_link_like(&metadata)
                    || metadata.len() < minimum_bytes
                {
                    metadata_skipped = metadata_skipped.saturating_add(1);
                    return Ok(());
                }
                results.push((path_digest(&path), metadata.len()));
                Ok(())
            },
        );
        match (mode, &scan_result) {
            (BenchmarkMode::CancelImmediately, Err(LargeFileCandidateScanError::Cancelled)) => {
                println!(
                    "windows_large_file_benchmark status=cancelled elapsed_ms={}",
                    started.elapsed().as_millis()
                );
                return Ok(());
            }
            (BenchmarkMode::FailConsumer, Err(LargeFileCandidateScanError::Consumer(error)))
                if error == "benchmark_consumer_failure" =>
            {
                println!(
                    "windows_large_file_benchmark status=consumer_failed elapsed_ms={}",
                    started.elapsed().as_millis()
                );
                return Ok(());
            }
            (BenchmarkMode::CancelImmediately, _) | (BenchmarkMode::FailConsumer, _) => {
                return Err("benchmark_fault_injection_returned_unexpected_result".to_string());
            }
            (BenchmarkMode::Scan, _) => {}
        }
        let summary = scan_result
            .map_err(format_scan_error)?
            .ok_or_else(|| "large_file_candidate_scan_unavailable".to_string())?;
        let elapsed_ms = started.elapsed().as_millis();

        // Scan strategies enumerate in different orders. Sort path digests
        // before hashing so A/B equality depends only on the candidate set and
        // sizes, never on traversal order.
        results.sort_unstable();
        let mut result_hasher = blake3::Hasher::new();
        let mut total_bytes = 0u64;
        for (path_hash, bytes) in &results {
            result_hasher.update(path_hash);
            result_hasher.update(&bytes.to_le_bytes());
            total_bytes = total_bytes.saturating_add(*bytes);
        }
        let (peak_working_set_bytes, working_set_bytes) = process_memory_bytes()?;
        println!(
            "windows_large_file_benchmark status=ok strategy={} root_digest={} threshold_bytes={} summary_candidates={} valid_candidates={} metadata_skipped={} total_bytes={} result_digest={} skipped={} peak_in_flight={} consumer_ms={} peak_working_set_bytes={} working_set_bytes={} elapsed_ms={}",
            summary.strategy,
            path_digest_hex(&root),
            minimum_bytes,
            summary.candidate_count,
            results.len(),
            metadata_skipped,
            total_bytes,
            result_hasher.finalize().to_hex(),
            summary.skipped_count,
            summary.peak_in_flight_candidates,
            summary.consumer_elapsed_ms,
            peak_working_set_bytes,
            working_set_bytes,
            elapsed_ms,
        );
        Ok(())
    }

    fn format_scan_error(error: LargeFileCandidateScanError) -> String {
        match error {
            LargeFileCandidateScanError::Cancelled => "scan_cancelled_unexpectedly".to_string(),
            LargeFileCandidateScanError::Platform(error) => {
                format!("platform_scan_failed error={error}")
            }
            LargeFileCandidateScanError::Consumer(error) => {
                format!("candidate_consumer_failed error={error}")
            }
        }
    }

    fn path_digest(path: &Path) -> [u8; 32] {
        let mut hasher = blake3::Hasher::new();
        for unit in path.as_os_str().encode_wide() {
            // Windows path identity is ASCII-case-insensitive. Normalize drive
            // letters and common directory names so MFT and Win32 paths that
            // differ only by case produce the same digest.
            let normalized = if (u16::from(b'A')..=u16::from(b'Z')).contains(&unit) {
                unit + u16::from(b'a' - b'A')
            } else {
                unit
            };
            hasher.update(&normalized.to_le_bytes());
        }
        *hasher.finalize().as_bytes()
    }

    fn path_digest_hex(path: &Path) -> String {
        blake3::Hash::from_bytes(path_digest(path))
            .to_hex()
            .to_string()
    }

    fn process_memory_bytes() -> Result<(usize, usize), String> {
        let mut counters = PROCESS_MEMORY_COUNTERS {
            cb: u32::try_from(std::mem::size_of::<PROCESS_MEMORY_COUNTERS>())
                .map_err(|_| "process_memory_structure_size_out_of_range".to_string())?,
            ..Default::default()
        };
        let succeeded =
            unsafe { K32GetProcessMemoryInfo(GetCurrentProcess(), &mut counters, counters.cb) };
        if succeeded == 0 {
            return Err(format!(
                "benchmark_process_memory_query_failed error={}",
                std::io::Error::last_os_error()
            ));
        }
        Ok((counters.PeakWorkingSetSize, counters.WorkingSetSize))
    }
}

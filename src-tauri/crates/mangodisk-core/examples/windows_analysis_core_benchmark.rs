#[cfg(not(windows))]
fn main() {
    println!("windows_analysis_core_benchmark status=unsupported platform=non_windows");
}

#[cfg(windows)]
fn main() {
    if let Err(error) = windows_benchmark::run() {
        eprintln!("windows_analysis_core_benchmark status=failed error={error}");
        std::process::exit(1);
    }
}

#[cfg(windows)]
mod windows_benchmark {
    use std::{
        env,
        sync::{Arc, Mutex},
        time::Instant,
    };

    use mangodisk_core::{
        configure_application_paths, AnalysisService, ApplicationPaths, TraversalProgress,
        APPLICATION_IDENTIFIER,
    };
    use mangodisk_platform::application_directories;

    pub(super) fn run() -> Result<(), String> {
        // A benchmark that scans the system drive must keep its generated index on another
        // volume. Otherwise each sample scans the previous sample's database and invalidates the
        // comparison. Production runs continue to use the platform application directories.
        let paths = if let Some(root) = env::var_os("MANGODISK_BENCHMARK_STATE_ROOT") {
            let root = std::path::PathBuf::from(root);
            ApplicationPaths::new(root.join("data"), root.join("cache"), root.join("runtime"))
                .map_err(|error| error.to_string())?
        } else {
            let directories = application_directories(APPLICATION_IDENTIFIER)
                .map_err(|error| error.to_string())?;
            ApplicationPaths::from_base_directories(
                directories.local_data_directory,
                directories.cache_directory,
            )
            .map_err(|error| error.to_string())?
        };
        configure_application_paths(paths).map_err(|error| error.to_string())?;

        let root = env::args().nth(1).unwrap_or_else(|| r"C:\".to_string());
        let final_progress = Arc::new(Mutex::new(None::<TraversalProgress>));
        let callback_progress = Arc::clone(&final_progress);
        let started = Instant::now();
        let result = AnalysisService::analyze_with_progress(Some(root), true, move |progress| {
            if let Ok(mut latest) = callback_progress.lock() {
                *latest = Some(progress);
            }
        })
        .map_err(|error| error.to_string())?;
        let elapsed_ms = started.elapsed().as_millis();

        // Top-level result order is a presentation detail and must not affect the content digest.
        // Paths enter only the local BLAKE3 state and are never logged, allowing the same result
        // set to be compared between native and Win32 A/B runs.
        let mut entries = result
            .entries
            .iter()
            .map(|entry| {
                (
                    *blake3::hash(entry.path.as_bytes()).as_bytes(),
                    entry.bytes,
                    entry.file_count,
                    entry.is_directory,
                )
            })
            .collect::<Vec<_>>();
        entries.sort_unstable();
        let mut hasher = blake3::Hasher::new();
        for (path, bytes, file_count, is_directory) in &entries {
            hasher.update(path);
            hasher.update(&bytes.to_le_bytes());
            hasher.update(&file_count.to_le_bytes());
            hasher.update(&[u8::from(*is_directory)]);
        }
        let progress = final_progress
            .lock()
            .map_err(|_| "failed to read final analysis progress".to_string())?
            .clone()
            .ok_or_else(|| "analysis did not emit final progress".to_string())?;
        println!(
            "windows_analysis_core_benchmark status=ok total_bytes={} skipped={} entries={} files_observed={} bytes_observed={} result_digest={} elapsed_ms={}",
            result.total_bytes,
            result.skipped_count,
            result.entries.len(),
            progress.items_scanned,
            progress.bytes_scanned,
            hasher.finalize().to_hex(),
            elapsed_ms,
        );
        Ok(())
    }
}

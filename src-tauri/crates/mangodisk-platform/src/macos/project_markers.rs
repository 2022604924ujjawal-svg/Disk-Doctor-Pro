use std::{
    collections::HashSet,
    ffi::OsString,
    io,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc::{self, RecvTimeoutError},
        Arc, Condvar, Mutex,
    },
    thread,
    time::Duration,
};

use crate::{
    ProjectMarkerCandidateProgress, ProjectMarkerCandidateScanError, ProjectMarkerCandidateSummary,
};

use super::bulk_directory::{
    worker_count, AlignedBuffer, BulkDirectory, VNODE_TYPE_DIRECTORY, VNODE_TYPE_REGULAR_FILE,
};

const RESULT_POLL_INTERVAL: Duration = Duration::from_millis(40);
// Core may scan several independent roots concurrently. Two readers overlap directory I/O inside
// one large source tree without multiplying metadata pressure across every cleanup root.
const MAX_DIRECTORY_WORKERS: usize = 2;

/// Keeps project-discovery policy at one boundary instead of extending an already long function
/// signature whenever the declarative rule contract gains another matcher.
pub(super) struct ProjectMarkerScanRequest<'a> {
    pub(super) root: &'a Path,
    pub(super) file_names: &'a [String],
    pub(super) file_suffixes: &'a [String],
    pub(super) pruned_directory_names: &'a [String],
    pub(super) maximum_depth: usize,
    pub(super) is_cancelled: &'a (dyn Fn() -> bool + Sync),
    pub(super) report_progress: &'a (dyn Fn(ProjectMarkerCandidateProgress) + Sync),
}

#[derive(Debug, Clone)]
struct DirectoryTask {
    path: PathBuf,
    depth: usize,
    is_root: bool,
}

#[derive(Debug)]
struct DirectoryReadResult {
    task: DirectoryTask,
    child_directories: Vec<PathBuf>,
    candidates: Vec<PathBuf>,
    file_count: u64,
    directory_count: u64,
}

#[derive(Debug)]
struct DirectoryReadFailure {
    task: DirectoryTask,
    error: io::Error,
}

#[derive(Default)]
struct DirectoryTaskQueue {
    state: Mutex<DirectoryTaskQueueState>,
    ready: Condvar,
}

#[derive(Default)]
struct DirectoryTaskQueueState {
    tasks: Vec<DirectoryTask>,
    stopped: bool,
}

impl DirectoryTaskQueue {
    fn push_many(&self, tasks: impl IntoIterator<Item = DirectoryTask>) {
        let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        state.tasks.extend(tasks);
        self.ready.notify_all();
    }

    fn pop(&self) -> Option<DirectoryTask> {
        let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        loop {
            if let Some(task) = state.tasks.pop() {
                return Some(task);
            }
            if state.stopped {
                return None;
            }
            state = self
                .ready
                .wait(state)
                .unwrap_or_else(|error| error.into_inner());
        }
    }

    fn stop(&self) {
        let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        state.stopped = true;
        state.tasks.clear();
        self.ready.notify_all();
    }
}

/// Finds project marker files with Darwin's bulk directory API.
///
/// Spotlight cannot prove completeness for every user-selected subtree. This scanner therefore
/// performs the same bounded-depth walk as Core's fallback while retrieving names and object types
/// in kernel-filled batches. A bounded worker pool overlaps independent directory reads, while the
/// coordinator alone emits candidates and progress so consumer ordering and failure handling stay
/// deterministic. Generated artifact directories are pruned before descent. Core still validates
/// every emitted marker, its scope, link safety, and the complete declarative project rule.
pub(super) fn scan(
    request: ProjectMarkerScanRequest<'_>,
    consumer: &mut dyn FnMut(PathBuf) -> Result<(), String>,
) -> Result<ProjectMarkerCandidateSummary, ProjectMarkerCandidateScanError> {
    let ProjectMarkerScanRequest {
        root,
        file_names,
        file_suffixes,
        pruned_directory_names,
        maximum_depth,
        is_cancelled,
        report_progress,
    } = request;
    if is_cancelled() {
        return Err(ProjectMarkerCandidateScanError::Cancelled);
    }
    let file_names = Arc::new(file_names.iter().cloned().collect::<HashSet<_>>());
    let file_suffixes = Arc::new(file_suffixes.to_vec());
    let pruned_directory_names = Arc::new(
        pruned_directory_names
            .iter()
            .cloned()
            .collect::<HashSet<_>>(),
    );
    let task_queue = Arc::new(DirectoryTaskQueue::default());
    let abort = Arc::new(AtomicBool::new(false));
    let (result_sender, result_receiver) = mpsc::channel();
    let worker_count = worker_count(MAX_DIRECTORY_WORKERS);
    let mut workers = Vec::with_capacity(worker_count);

    for worker_index in 0..worker_count {
        let worker_queue = Arc::clone(&task_queue);
        let worker_abort = Arc::clone(&abort);
        let worker_file_names = Arc::clone(&file_names);
        let worker_file_suffixes = Arc::clone(&file_suffixes);
        let worker_prune_names = Arc::clone(&pruned_directory_names);
        let result_sender = result_sender.clone();
        let worker = thread::Builder::new()
            .name(format!("mangodisk-project-marker-{worker_index}"))
            .spawn(move || {
                let mut buffer = AlignedBuffer::new();
                while let Some(task) = worker_queue.pop() {
                    if worker_abort.load(Ordering::Relaxed) {
                        break;
                    }
                    let result = read_directory(
                        task,
                        maximum_depth,
                        &worker_file_names,
                        &worker_file_suffixes,
                        &worker_prune_names,
                        &worker_abort,
                        &mut buffer,
                    );
                    if result_sender.send(result).is_err() {
                        break;
                    }
                }
            })
            .map_err(|error| {
                ProjectMarkerCandidateScanError::Platform(format!(
                    "unable to spawn project marker worker: {error}"
                ))
            })?;
        workers.push(worker);
    }
    drop(result_sender);

    let mut outstanding_tasks = 1_usize;
    let mut candidate_count = 0_u64;
    let mut file_count = 0_u64;
    let mut directory_count = 0_u64;
    let mut scan_result = Ok(());
    task_queue.push_many([DirectoryTask {
        path: root.to_path_buf(),
        depth: 0,
        is_root: true,
    }]);

    while outstanding_tasks > 0 {
        if is_cancelled() {
            scan_result = Err(ProjectMarkerCandidateScanError::Cancelled);
            break;
        }
        let result = match result_receiver.recv_timeout(RESULT_POLL_INTERVAL) {
            Ok(result) => result,
            Err(RecvTimeoutError::Timeout) => continue,
            Err(RecvTimeoutError::Disconnected) => {
                scan_result = Err(ProjectMarkerCandidateScanError::Platform(
                    "project marker workers disconnected".to_string(),
                ));
                break;
            }
        };
        outstanding_tasks -= 1;
        let result = match result {
            Ok(result) => result,
            Err(failure) if !failure.task.is_root => {
                log::debug!(
                    "project_marker_directory_skipped platform=macos error_kind={:?}",
                    failure.error.kind()
                );
                continue;
            }
            Err(failure) => {
                scan_result = Err(ProjectMarkerCandidateScanError::Platform(format!(
                    "unable to enumerate project scan root: {}",
                    failure.error
                )));
                break;
            }
        };

        file_count = file_count.saturating_add(result.file_count);
        directory_count = directory_count.saturating_add(result.directory_count);
        report_progress(ProjectMarkerCandidateProgress {
            current_directory: result.task.path.clone(),
            file_count: result.file_count,
            directory_count: result.directory_count,
        });
        for candidate in result.candidates {
            if let Err(error) = consumer(candidate) {
                scan_result = Err(ProjectMarkerCandidateScanError::Consumer(error));
                break;
            }
            candidate_count = candidate_count.saturating_add(1);
        }
        if scan_result.is_err() {
            break;
        }
        let child_tasks = result
            .child_directories
            .into_iter()
            .map(|path| DirectoryTask {
                path,
                depth: result.task.depth + 1,
                is_root: false,
            })
            .collect::<Vec<_>>();
        outstanding_tasks = outstanding_tasks.saturating_add(child_tasks.len());
        task_queue.push_many(child_tasks);
    }

    abort.store(true, Ordering::Relaxed);
    task_queue.stop();
    for worker in workers {
        if worker.join().is_err() && scan_result.is_ok() {
            scan_result = Err(ProjectMarkerCandidateScanError::Platform(
                "project marker worker panicked".to_string(),
            ));
        }
    }
    scan_result?;

    Ok(ProjectMarkerCandidateSummary {
        candidate_count,
        file_count,
        directory_count,
        strategy: "darwin-parallel-getattrlistbulk-project-markers-v2",
    })
}

fn read_directory(
    task: DirectoryTask,
    maximum_depth: usize,
    file_names: &HashSet<String>,
    file_suffixes: &[String],
    pruned_directory_names: &HashSet<String>,
    abort: &AtomicBool,
    buffer: &mut AlignedBuffer,
) -> Result<DirectoryReadResult, DirectoryReadFailure> {
    let directory = BulkDirectory::open(&task.path).map_err(|error| DirectoryReadFailure {
        task: task.clone(),
        error,
    })?;
    let mut child_directories = Vec::new();
    let mut candidates = Vec::new();
    let mut file_count = 0_u64;
    let mut directory_count = 0_u64;
    loop {
        if abort.load(Ordering::Relaxed) {
            return Ok(DirectoryReadResult {
                task,
                child_directories,
                candidates,
                file_count,
                directory_count,
            });
        }
        let entries = directory
            .read_name_page(buffer)
            .map_err(|error| DirectoryReadFailure {
                task: task.clone(),
                error,
            })?;
        if entries.is_empty() {
            break;
        }
        for entry in entries {
            if entry.attribute_error != 0 || entry.name.as_encoded_bytes().is_empty() {
                continue;
            }
            if entry.object_type == VNODE_TYPE_DIRECTORY {
                directory_count = directory_count.saturating_add(1);
                if task.depth < maximum_depth
                    && !directory_is_pruned(&entry.name, pruned_directory_names)
                {
                    child_directories.push(task.path.join(entry.name));
                }
            } else if entry.object_type == VNODE_TYPE_REGULAR_FILE {
                file_count = file_count.saturating_add(1);
                if marker_name_matches(&entry.name, file_names, file_suffixes) {
                    candidates.push(task.path.join(entry.name));
                }
            }
        }
    }
    Ok(DirectoryReadResult {
        task,
        child_directories,
        candidates,
        file_count,
        directory_count,
    })
}

fn directory_is_pruned(name: &OsString, pruned_directory_names: &HashSet<String>) -> bool {
    let name = name.to_string_lossy();
    name.starts_with('.') || pruned_directory_names.contains(name.as_ref())
}

fn marker_name_matches(
    name: &OsString,
    file_names: &HashSet<String>,
    file_suffixes: &[String],
) -> bool {
    let name = name.to_string_lossy();
    file_names.contains(name.as_ref()) || file_suffixes.iter().any(|suffix| name.ends_with(suffix))
}

#[cfg(test)]
mod tests {
    use std::{
        fs,
        path::PathBuf,
        sync::{
            atomic::{AtomicU64, Ordering},
            Mutex,
        },
    };

    use super::{scan, ProjectMarkerScanRequest};

    #[test]
    fn scan_finds_markers_prunes_generated_trees_and_reports_work() {
        let fixture = TestDirectory::new();
        let visible = fixture.path.join("workspace/app");
        let pruned = fixture.path.join("workspace/app/node_modules/dependency");
        let hidden = fixture.path.join("workspace/.hidden/project");
        fs::create_dir_all(&visible).unwrap();
        fs::create_dir_all(&pruned).unwrap();
        fs::create_dir_all(&hidden).unwrap();
        fs::write(visible.join("package.json"), b"{}").unwrap();
        fs::write(visible.join("source.rs"), b"").unwrap();
        fs::write(pruned.join("package.json"), b"{}").unwrap();
        fs::write(hidden.join("project.xcodeproj"), b"").unwrap();

        let mut candidates = Vec::<PathBuf>::new();
        let progress = Mutex::new((0_u64, 0_u64));
        let summary = scan(
            ProjectMarkerScanRequest {
                root: &fixture.path,
                file_names: &["package.json".to_string()],
                file_suffixes: &[".xcodeproj".to_string()],
                pruned_directory_names: &["node_modules".to_string()],
                maximum_depth: 64,
                is_cancelled: &|| false,
                report_progress: &|batch| {
                    let mut totals = progress.lock().unwrap();
                    totals.0 += batch.file_count;
                    totals.1 += batch.directory_count;
                },
            },
            &mut |path| {
                candidates.push(path);
                Ok(())
            },
        )
        .unwrap();

        assert_eq!(summary.candidate_count, 1);
        assert_eq!(summary.file_count, 2);
        assert_eq!(progress.into_inner().unwrap().0, summary.file_count);
        assert_eq!(candidates, vec![visible.join("package.json")]);
    }

    #[test]
    fn scan_respects_the_discovery_depth_limit() {
        let fixture = TestDirectory::new();
        let shallow = fixture.path.join("one");
        let deep = shallow.join("two");
        fs::create_dir_all(&deep).unwrap();
        fs::write(shallow.join("package.json"), b"{}").unwrap();
        fs::write(deep.join("package.json"), b"{}").unwrap();

        let mut candidates = Vec::<PathBuf>::new();
        let summary = scan(
            ProjectMarkerScanRequest {
                root: &fixture.path,
                file_names: &["package.json".to_string()],
                file_suffixes: &[],
                pruned_directory_names: &[],
                maximum_depth: 1,
                is_cancelled: &|| false,
                report_progress: &|_| {},
            },
            &mut |path| {
                candidates.push(path);
                Ok(())
            },
        )
        .unwrap();

        assert_eq!(summary.candidate_count, 1);
        assert_eq!(candidates, vec![shallow.join("package.json")]);
    }

    static TEST_DIRECTORY_SEQUENCE: AtomicU64 = AtomicU64::new(0);

    struct TestDirectory {
        path: PathBuf,
    }

    impl TestDirectory {
        fn new() -> Self {
            let sequence = TEST_DIRECTORY_SEQUENCE.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "mangodisk-project-markers-{}-{sequence}",
                std::process::id()
            ));
            let _ = fs::remove_dir_all(&path);
            fs::create_dir_all(&path).unwrap();
            Self { path }
        }
    }

    impl Drop for TestDirectory {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.path);
        }
    }
}

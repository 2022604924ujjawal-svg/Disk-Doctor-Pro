use std::{
    collections::{HashMap, HashSet},
    ffi::OsString,
    mem::{size_of, zeroed},
    os::windows::ffi::OsStringExt,
    path::{Path, PathBuf},
    thread,
    time::{Duration, Instant},
};

use windows_sys::Win32::{
    Foundation::{CloseHandle, HANDLE, HWND, INVALID_HANDLE_VALUE, LPARAM},
    System::{
        Diagnostics::ToolHelp::{
            CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W,
            TH32CS_SNAPPROCESS,
        },
        Threading::{
            GetCurrentProcessId, OpenProcess, QueryFullProcessImageNameW, TerminateProcess,
            PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_TERMINATE,
        },
    },
    UI::WindowsAndMessaging::{EnumWindows, GetWindowThreadProcessId, PostMessageW, WM_CLOSE},
};

use crate::{
    ApplicationProcessCloseMode, ApplicationProcessCloseResult, ApplicationProcessTarget,
    PlatformCancellation, PlatformError, PlatformResult, RunningProcessIdentity,
};

use super::path_identity;

const GRACEFUL_CLOSE_TIMEOUT: Duration = Duration::from_secs(5);
const FORCE_CLOSE_TIMEOUT: Duration = Duration::from_secs(2);
const CLOSE_POLL_INTERVAL: Duration = Duration::from_millis(100);
const MAX_EXECUTABLE_PATH_UNITS: usize = 32_768;

#[derive(Debug, Clone)]
struct ProcessInstance {
    pid: u32,
    executable_name: String,
    executable_path: Option<PathBuf>,
}

struct OwnedHandle(HANDLE);

impl Drop for OwnedHandle {
    fn drop(&mut self) {
        if !self.0.is_null() && self.0 != INVALID_HANDLE_VALUE {
            unsafe {
                CloseHandle(self.0);
            }
        }
    }
}

pub(super) fn close(
    target: &ApplicationProcessTarget,
    mode: ApplicationProcessCloseMode,
) -> PlatformResult<ApplicationProcessCloseResult> {
    close_many(std::slice::from_ref(target), mode)
        .into_iter()
        .next()
        .expect("a single process-close target must produce one result")
}

/// Uses one ToolHelp snapshot per polling interval and one deadline for the
/// complete selection. This avoids serial five-second waits and thousands of
/// redundant image-path queries when many applications show save prompts.
pub(super) fn close_many(
    targets: &[ApplicationProcessTarget],
    mode: ApplicationProcessCloseMode,
) -> Vec<PlatformResult<ApplicationProcessCloseResult>> {
    if targets.is_empty() {
        return Vec::new();
    }
    let validation_errors = targets
        .iter()
        .map(|target| validate_target(target).err())
        .collect::<Vec<_>>();
    if validation_errors.iter().all(Option::is_some) {
        return validation_errors
            .into_iter()
            .map(|error| Err(error.expect("every target was invalid")))
            .collect();
    }

    let initial_snapshot = match process_snapshot() {
        Ok(snapshot) => snapshot,
        Err(error) => return snapshot_error_results(&validation_errors, error),
    };
    let matched = targets
        .iter()
        .zip(&validation_errors)
        .map(|(target, error)| {
            if error.is_none() {
                matching_processes_in_snapshot(target, &initial_snapshot)
            } else {
                Vec::new()
            }
        })
        .collect::<Vec<_>>();

    let mut requests = HashMap::new();
    for (target, processes) in targets.iter().zip(&matched) {
        for process in processes {
            // A missing image path is sufficient to keep a path-owned target
            // blocked, but it must never authorize terminating a same-name
            // process from an unknown installation.
            let authorized = process_is_authorized_for_target(target, process);
            if authorized && !requests.get(&process.pid).copied().unwrap_or(false) {
                let requested = match mode {
                    ApplicationProcessCloseMode::Graceful => request_graceful_close(process),
                    ApplicationProcessCloseMode::Force => request_force_close(process),
                };
                requests.insert(process.pid, requested);
            } else {
                requests.entry(process.pid).or_insert(false);
            }
        }
    }

    let timeout = match mode {
        ApplicationProcessCloseMode::Graceful => GRACEFUL_CLOSE_TIMEOUT,
        ApplicationProcessCloseMode::Force => FORCE_CLOSE_TIMEOUT,
    };
    let deadline = Instant::now() + timeout;
    let final_snapshot = loop {
        let snapshot = match process_snapshot() {
            Ok(snapshot) => snapshot,
            Err(error) => return snapshot_error_results(&validation_errors, error),
        };
        let has_remaining = targets
            .iter()
            .zip(&validation_errors)
            .any(|(target, error)| {
                error.is_none() && !matching_processes_in_snapshot(target, &snapshot).is_empty()
            });
        if !has_remaining || Instant::now() >= deadline {
            break snapshot;
        }
        thread::sleep(CLOSE_POLL_INTERVAL);
    };

    targets
        .iter()
        .zip(validation_errors)
        .zip(matched)
        .map(|((target, validation_error), matched)| {
            if let Some(error) = validation_error {
                return Err(error);
            }
            let remaining = matching_processes_in_snapshot(target, &final_snapshot);
            Ok(ApplicationProcessCloseResult {
                matched_process_count: matched.len() as u64,
                requested_process_count: matched
                    .iter()
                    .filter(|process| requests.get(&process.pid).copied().unwrap_or(false))
                    .count() as u64,
                remaining_processes: unique_executable_names(&remaining),
            })
        })
        .collect()
}

fn validate_target(target: &ApplicationProcessTarget) -> PlatformResult<()> {
    if target.executable_names.is_empty() && target.executable_paths.is_empty() {
        return Err(PlatformError::operation_failed(
            "application process target contains no identity",
        ));
    }
    Ok(())
}

fn request_graceful_close(process: &ProcessInstance) -> bool {
    let Some(_process_handle) = open_verified_process(process, 0) else {
        return false;
    };
    let mut context = WindowCloseContext {
        pid: process.pid,
        posted: false,
    };
    unsafe {
        EnumWindows(
            Some(post_close_to_process_window),
            (&mut context as *mut WindowCloseContext) as LPARAM,
        );
    }
    context.posted
}

struct WindowCloseContext {
    pid: u32,
    posted: bool,
}

unsafe extern "system" fn post_close_to_process_window(window: HWND, context: LPARAM) -> i32 {
    let context = unsafe { &mut *(context as *mut WindowCloseContext) };
    let mut window_pid = 0_u32;
    unsafe {
        GetWindowThreadProcessId(window, &mut window_pid);
    }
    if window_pid == context.pid && unsafe { PostMessageW(window, WM_CLOSE, 0, 0) } != 0 {
        context.posted = true;
    }
    1
}

fn request_force_close(process: &ProcessInstance) -> bool {
    let Some(handle) = open_verified_process(process, PROCESS_TERMINATE) else {
        return false;
    };
    unsafe { TerminateProcess(handle.0, 1) != 0 }
}

#[cfg(test)]
fn matching_processes(target: &ApplicationProcessTarget) -> PlatformResult<Vec<ProcessInstance>> {
    process_snapshot().map(|snapshot| matching_processes_in_snapshot(target, &snapshot))
}

fn matching_processes_in_snapshot(
    target: &ApplicationProcessTarget,
    snapshot: &[ProcessInstance],
) -> Vec<ProcessInstance> {
    let names = target
        .executable_names
        .iter()
        .flat_map(|name| normalized_name_aliases(name))
        .collect::<HashSet<_>>();
    let paths = target
        .executable_paths
        .iter()
        .map(|path| normalize_path(path))
        .collect::<HashSet<_>>();
    let path_names = target
        .executable_paths
        .iter()
        .filter_map(|path| path.file_name())
        .flat_map(|name| normalized_name_aliases(&name.to_string_lossy()))
        .collect::<HashSet<_>>();
    let current_pid = unsafe { GetCurrentProcessId() };
    snapshot
        .iter()
        .filter(|process| {
            process.pid != current_pid
                && if paths.is_empty() {
                    normalized_name_aliases(&process.executable_name)
                        .iter()
                        .any(|name| names.contains(name))
                } else {
                    process.executable_path.as_deref().map_or_else(
                        || {
                            normalized_name_aliases(&process.executable_name)
                                .iter()
                                .any(|name| path_names.contains(name))
                        },
                        |path| paths.contains(&normalize_path(path)),
                    )
                }
        })
        .cloned()
        .collect()
}

fn process_is_authorized_for_target(
    target: &ApplicationProcessTarget,
    process: &ProcessInstance,
) -> bool {
    if target.executable_paths.is_empty() {
        return true;
    }
    let Some(process_path) = process.executable_path.as_deref() else {
        return false;
    };
    target
        .executable_paths
        .iter()
        .any(|target_path| normalize_path(target_path) == normalize_path(process_path))
}

fn snapshot_error_results(
    validation_errors: &[Option<PlatformError>],
    snapshot_error: PlatformError,
) -> Vec<PlatformResult<ApplicationProcessCloseResult>> {
    validation_errors
        .iter()
        .map(|validation_error| {
            Err(validation_error
                .clone()
                .unwrap_or_else(|| snapshot_error.clone()))
        })
        .collect()
}

/// Keeps a handle to the original process object while posting `WM_CLOSE` or
/// terminating it. Windows cannot reuse that PID while this handle remains
/// alive, and the queried image path must still match the captured identity.
fn open_verified_process(process: &ProcessInstance, additional_access: u32) -> Option<OwnedHandle> {
    if process.pid == 0 || process.pid == unsafe { GetCurrentProcessId() } {
        return None;
    }
    let handle = unsafe {
        OpenProcess(
            PROCESS_QUERY_LIMITED_INFORMATION | additional_access,
            0,
            process.pid,
        )
    };
    if handle.is_null() {
        return None;
    }
    let handle = OwnedHandle(handle);
    let current_path = query_executable_path(handle.0)?;
    let identity_matches = process
        .executable_path
        .as_deref()
        .map(|path| normalize_path(path) == normalize_path(&current_path))
        .unwrap_or_else(|| {
            current_path.file_name().is_some_and(|name| {
                normalize_name(&name.to_string_lossy()) == normalize_name(&process.executable_name)
            })
        });
    identity_matches.then_some(handle)
}

fn process_snapshot() -> PlatformResult<Vec<ProcessInstance>> {
    process_snapshot_with_cancellation(None)
}

fn process_snapshot_with_cancellation(
    cancellation: Option<&PlatformCancellation>,
) -> PlatformResult<Vec<ProcessInstance>> {
    if cancellation.is_some_and(PlatformCancellation::is_cancelled) {
        return Err(PlatformError::operation_failed(
            "windows process snapshot capture was cancelled",
        ));
    }
    let snapshot = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) };
    if snapshot == INVALID_HANDLE_VALUE {
        return Err(PlatformError::operation_failed(
            "windows process control snapshot creation failed",
        ));
    }
    let snapshot = OwnedHandle(snapshot);
    let mut entry: PROCESSENTRY32W = unsafe { zeroed() };
    entry.dwSize = size_of::<PROCESSENTRY32W>() as u32;
    if unsafe { Process32FirstW(snapshot.0, &mut entry) } == 0 {
        return Err(PlatformError::operation_failed(
            "windows process control snapshot enumeration failed",
        ));
    }

    let mut processes = Vec::new();
    loop {
        if cancellation.is_some_and(PlatformCancellation::is_cancelled) {
            return Err(PlatformError::operation_failed(
                "windows process snapshot capture was cancelled",
            ));
        }
        let executable_name = wide_c_string(&entry.szExeFile);
        if entry.th32ProcessID != 0 && !executable_name.is_empty() {
            processes.push(ProcessInstance {
                pid: entry.th32ProcessID,
                executable_path: executable_path(entry.th32ProcessID),
                executable_name,
            });
        }
        if unsafe { Process32NextW(snapshot.0, &mut entry) } == 0 {
            break;
        }
    }
    Ok(processes)
}

pub(super) fn running_process_identities(
    cancellation: &PlatformCancellation,
) -> PlatformResult<Vec<RunningProcessIdentity>> {
    process_snapshot_with_cancellation(Some(cancellation)).map(|processes| {
        processes
            .into_iter()
            .map(|process| RunningProcessIdentity {
                executable_name: process.executable_name,
                executable_path: process.executable_path,
            })
            .collect()
    })
}

fn executable_path(pid: u32) -> Option<PathBuf> {
    let handle = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
    if handle.is_null() {
        return None;
    }
    let handle = OwnedHandle(handle);
    query_executable_path(handle.0)
}

fn query_executable_path(handle: HANDLE) -> Option<PathBuf> {
    let mut buffer = vec![0_u16; MAX_EXECUTABLE_PATH_UNITS];
    let mut length = buffer.len() as u32;
    if unsafe { QueryFullProcessImageNameW(handle, 0, buffer.as_mut_ptr(), &mut length) } == 0 {
        return None;
    }
    buffer.truncate(length as usize);
    Some(PathBuf::from(OsString::from_wide(&buffer)))
}

fn wide_c_string(units: &[u16]) -> String {
    let length = units
        .iter()
        .position(|unit| *unit == 0)
        .unwrap_or(units.len());
    OsString::from_wide(&units[..length])
        .to_string_lossy()
        .into_owned()
}

fn unique_executable_names(processes: &[ProcessInstance]) -> Vec<String> {
    let mut names = processes
        .iter()
        .map(|process| process.executable_name.clone())
        .collect::<Vec<_>>();
    names.sort_by_key(|name| name.to_ascii_lowercase());
    names.dedup_by(|left, right| left.eq_ignore_ascii_case(right));
    names
}

fn normalize_name(name: &str) -> String {
    Path::new(name.trim())
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .to_lowercase()
}

fn normalized_name_aliases(name: &str) -> Vec<String> {
    let normalized = normalize_name(name);
    let mut aliases = vec![normalized.clone()];
    if let Some(stem) = normalized.strip_suffix(".exe") {
        aliases.push(stem.to_string());
    }
    aliases
}

fn normalize_path(path: &Path) -> String {
    path_identity::comparison_key(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::{Command, Stdio};

    #[test]
    fn path_normalization_is_case_and_separator_insensitive() {
        assert_eq!(
            normalize_path(Path::new("C:/Program Files/Example/App.exe")),
            normalize_path(Path::new(r"c:\Program Files\Example\APP.EXE"))
        );
    }

    #[test]
    fn executable_extension_is_an_optional_process_alias() {
        let target = normalized_name_aliases("MangoDiskCloseUninstallFixture");
        let process = normalized_name_aliases("MangoDiskCloseUninstallFixture.exe");
        assert!(process.iter().any(|name| target.contains(name)));
    }

    #[test]
    fn exact_paths_disable_same_name_fallback() {
        let snapshot = vec![ProcessInstance {
            pid: 42,
            executable_name: "SharedHelper.exe".to_string(),
            executable_path: Some(PathBuf::from(r"C:\Other\SharedHelper.exe")),
        }];
        let target = ApplicationProcessTarget {
            executable_names: vec!["SharedHelper.exe".to_string()],
            executable_paths: vec![PathBuf::from(r"C:\Expected\SharedHelper.exe")],
        };

        assert!(matching_processes_in_snapshot(&target, &snapshot).is_empty());
    }

    #[test]
    fn unresolved_path_blocks_without_authorizing_same_name_process() {
        let process = ProcessInstance {
            pid: 42,
            executable_name: "SharedHelper.exe".to_string(),
            executable_path: None,
        };
        let target = ApplicationProcessTarget {
            executable_names: Vec::new(),
            executable_paths: vec![PathBuf::from(r"C:\Expected\SharedHelper.exe")],
        };

        assert_eq!(
            matching_processes_in_snapshot(&target, std::slice::from_ref(&process)).len(),
            1
        );
        assert!(!process_is_authorized_for_target(&target, &process));
    }

    #[test]
    #[ignore = "launches and closes a real native window"]
    fn closes_spawned_window_gracefully() {
        let mut child = Command::new("powershell.exe")
            .args([
                "-NoProfile",
                "-STA",
                "-Command",
                "Add-Type -AssemblyName PresentationFramework; $window = New-Object System.Windows.Window; $window.Title = 'MangoDisk Process Close Test'; $window.Width = 420; $window.Height = 140; [void]$window.ShowDialog()",
            ])
            .spawn()
            .expect("the test window should start");
        let process_path = wait_for_spawned_process(&mut child);
        // Give WPF time to create its top-level HWND after the process itself
        // becomes visible in the ToolHelp snapshot.
        thread::sleep(Duration::from_millis(500));
        let result = close(
            &ApplicationProcessTarget {
                executable_names: Vec::new(),
                executable_paths: vec![process_path],
            },
            ApplicationProcessCloseMode::Graceful,
        );

        // Reap the isolated process even when an assertion below fails. A
        // successful WM_CLOSE usually makes `kill` a harmless no-op.
        let _ = child.kill();
        let _ = child.wait();

        let result = result.expect("the close request should succeed");
        assert_eq!(result.matched_process_count, 1);
        assert_eq!(result.requested_process_count, 1);
        assert!(result.remaining_processes.is_empty());
    }

    #[test]
    #[ignore = "launches and force-terminates a real child process"]
    fn force_closes_spawned_command() {
        let mut child = Command::new("ping.exe")
            .args(["-t", "127.0.0.1"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("the test process should start");
        let process_path = wait_for_spawned_process(&mut child);
        let result = close(
            &ApplicationProcessTarget {
                executable_names: Vec::new(),
                executable_paths: vec![process_path],
            },
            ApplicationProcessCloseMode::Force,
        );

        let _ = child.kill();
        let _ = child.wait();

        let result = result.expect("the force-close request should succeed");
        assert_eq!(result.matched_process_count, 1);
        assert_eq!(result.requested_process_count, 1);
        assert!(result.remaining_processes.is_empty());
    }

    fn wait_for_spawned_process(child: &mut std::process::Child) -> PathBuf {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if let Some(path) = executable_path(child.id()) {
                if matching_processes(&ApplicationProcessTarget {
                    executable_names: Vec::new(),
                    executable_paths: vec![path.clone()],
                })
                .is_ok_and(|processes| processes.iter().any(|process| process.pid == child.id()))
                {
                    return path;
                }
            }
            if Instant::now() >= deadline {
                let _ = child.kill();
                let _ = child.wait();
                panic!("the test process did not become discoverable");
            }
            thread::sleep(CLOSE_POLL_INTERVAL);
        }
    }
}

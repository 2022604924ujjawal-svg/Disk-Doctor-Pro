use std::path::PathBuf;

/// One process identity captured from the operating system.
///
/// Windows can enumerate a process name even when querying its image path is
/// denied. Keeping the optional path separate prevents Core from mistaking a
/// bare executable name for an absolute path while still allowing it to block
/// a destructive operation conservatively.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunningProcessIdentity {
    pub executable_name: String,
    pub executable_path: Option<PathBuf>,
}

/// Trusted process identity facts resolved by Core from a cleanup rule or an
/// installed-application catalog entry. Platform implementations may match by
/// executable path when available and fall back to an exact executable name
/// for declarative cleanup rules that intentionally own only process aliases.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ApplicationProcessTarget {
    pub executable_names: Vec<String>,
    pub executable_paths: Vec<PathBuf>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApplicationProcessCloseMode {
    Graceful,
    Force,
}

/// Bounded result from one platform close attempt.
///
/// A request can be accepted while a process remains alive because an
/// application may show a save prompt, reject termination, or immediately
/// restart a helper. Callers must treat `remaining_processes` as the final
/// authority before entering a destructive operation.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ApplicationProcessCloseResult {
    pub matched_process_count: u64,
    pub requested_process_count: u64,
    pub remaining_processes: Vec<String>,
}

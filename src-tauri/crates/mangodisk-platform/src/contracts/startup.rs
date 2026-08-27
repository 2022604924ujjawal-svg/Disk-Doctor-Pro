use std::path::PathBuf;

use super::{PlatformCancellation, PlatformResult};

/// Operating-system facts required by the startup domain.
///
/// The platform contract intentionally excludes product grouping, localized text, and change
/// recommendations. Core owns those decisions while each platform preserves the native source
/// semantics needed to explain and later revalidate an item.
pub trait StartupPlatform: Send + Sync {
    fn scan_startup_sources(
        &self,
        cancellation: &PlatformCancellation,
    ) -> PlatformResult<Vec<PlatformStartupSourceResult>>;

    /// Changes one item resolved from a server-owned catalog session.
    ///
    /// Implementations must re-read the native item, reject state drift, perform the smallest
    /// source-specific mutation, and verify the resulting configured state before returning.
    /// The optional authorization prompt is ephemeral localized UI context for native dialogs.
    fn change_startup_item(
        &self,
        request: &PlatformStartupChangeRequest,
        authorization_prompt: Option<&str>,
    ) -> PlatformResult<PlatformStartupChangeResult>;

    /// Changes a prepared batch while preserving an individual result for every request.
    ///
    /// Platforms may override this method to share one native authorization boundary across
    /// privileged items. The default keeps unsupported platforms correct without elevating
    /// ordinary user-scoped mutations.
    fn change_startup_items(
        &self,
        requests: &[PlatformStartupChangeRequest],
        authorization_prompt: Option<&str>,
    ) -> PlatformResult<Vec<PlatformResult<PlatformStartupChangeResult>>> {
        Ok(requests
            .iter()
            .map(|request| self.change_startup_item(request, authorization_prompt))
            .collect())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlatformStartupDesiredState {
    Enabled,
    Disabled,
    Removed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlatformStartupChangeRequest {
    /// Stable provider identifier retained only in the Core catalog session.
    pub provider_item_id: String,
    pub source_id: String,
    /// Complete native fact snapshot captured during preflight.
    ///
    /// Providers compare its mutation-relevant facts with a fresh read immediately before the
    /// change. Volatile observations such as the current process state must not invalidate an
    /// otherwise safe request, while target, trigger, scope, capability, and configured-state
    /// drift must still fail closed.
    pub expected_artifact: PlatformStartupArtifact,
    pub desired_state: PlatformStartupDesiredState,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlatformStartupChangeResult {
    pub previous_state: PlatformStartupConfiguredState,
    pub configured_state: PlatformStartupConfiguredState,
    pub verified: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlatformStartupSourceKind {
    RegistryRun,
    StartupFolder,
    ScheduledTask,
    Service,
    PackagedStartupTask,
    LaunchAgent,
    LaunchDaemon,
    LoginItem,
    BackgroundTask,
    EmbeddedItem,
    AdvancedAutoRun,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlatformStartupScope {
    CurrentUser,
    User,
    AllUsers,
    Machine,
    System,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum PlatformStartupTrigger {
    Boot,
    UserLogon,
    Scheduled,
    Event,
    KeepAlive,
    ShellLoad,
    ApplicationLaunch,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlatformStartupConfiguredState {
    Enabled,
    Disabled,
    Unknown,
    NotApplicable,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlatformStartupRuntimeState {
    Running,
    Stopped,
    Loaded,
    Unloaded,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlatformStartupControlCapability {
    Toggleable,
    ElevationRequired,
    RemoveOnly,
    SystemManaged,
    PolicyManaged,
    ViewOnly,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlatformStartupTrustState {
    System,
    Verified,
    Invalid,
    Unsigned,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlatformStartupIdentityConfidence {
    Exact,
    Strong,
    Probable,
    Unresolved,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlatformStartupSummarySource {
    ServiceDescription,
    TaskDescription,
    PackageManifest,
    VersionInfo,
    BundleMetadata,
    SourceLabel,
    Unavailable,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlatformStartupTargetKind {
    Executable,
    Application,
    Script,
    Service,
    Task,
    Other,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlatformStartupDiagnosticCode {
    AccessDenied,
    InvalidData,
    MissingIdentity,
    MissingTarget,
    StateUnavailable,
    UnsupportedFormat,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlatformStartupTarget {
    pub kind: PlatformStartupTargetKind,
    /// Provider-normalized identity used only as grouping evidence inside Core.
    ///
    /// This may be a normalized path, service name, or task identity. Core hashes the value before
    /// it crosses an adapter boundary, so UI code can never treat it as mutation authority.
    pub identity_key: String,
    pub path: Option<PathBuf>,
    pub executable_name: Option<String>,
    pub arguments: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlatformStartupOwner {
    /// Strong platform identity such as a bundle identifier or package family name.
    pub identity_key: Option<String>,
    pub name: Option<String>,
    pub publisher: Option<String>,
    pub summary: Option<String>,
    pub summary_source: PlatformStartupSummarySource,
    pub version: Option<String>,
    pub icon_path: Option<PathBuf>,
    pub confidence: PlatformStartupIdentityConfidence,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlatformStartupArtifact {
    /// Provider-owned stable identity. The value remains inside Core and is hashed for UI output.
    pub provider_item_id: String,
    pub source_kind: PlatformStartupSourceKind,
    pub scope: PlatformStartupScope,
    pub triggers: Vec<PlatformStartupTrigger>,
    pub display_name: String,
    /// Path to the file that defines the startup behavior, when the provider has one.
    ///
    /// This is display evidence only. Providers must continue to use their typed identifiers and
    /// recovery data as mutation authority.
    pub configuration_path: Option<PathBuf>,
    pub target: PlatformStartupTarget,
    pub owner: PlatformStartupOwner,
    pub configured_state: PlatformStartupConfiguredState,
    pub runtime_state: PlatformStartupRuntimeState,
    pub control_capability: PlatformStartupControlCapability,
    pub trust: PlatformStartupTrustState,
    pub modified_at_ms: Option<u64>,
    pub diagnostics: Vec<PlatformStartupDiagnosticCode>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlatformStartupCoverageStatus {
    Complete,
    Partial,
    Unavailable,
    Failed,
    Cancelled,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlatformStartupCoverageReason {
    AccessDenied,
    ApiUnavailable,
    Cancelled,
    InvalidData,
    NotImplemented,
    StateUnavailable,
    UnsupportedOperatingSystem,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlatformStartupSourceResult {
    pub source_id: String,
    pub required: bool,
    pub status: PlatformStartupCoverageStatus,
    pub reason: Option<PlatformStartupCoverageReason>,
    pub items: Vec<PlatformStartupArtifact>,
    pub elapsed_ms: u64,
}

impl PlatformStartupSourceResult {
    pub fn unavailable(
        source_id: impl Into<String>,
        required: bool,
        reason: PlatformStartupCoverageReason,
    ) -> Self {
        Self {
            source_id: source_id.into(),
            required,
            status: PlatformStartupCoverageStatus::Unavailable,
            reason: Some(reason),
            items: Vec::new(),
            elapsed_ms: 0,
        }
    }
}

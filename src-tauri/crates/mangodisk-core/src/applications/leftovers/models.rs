use serde::{Deserialize, Serialize};

pub const APPLICATION_LEFTOVER_SCAN_SCHEMA_VERSION: u32 = 2;
pub const APPLICATION_LEFTOVER_PLAN_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ApplicationLeftoverSource {
    SandboxContainer,
    ApplicationSupport,
    Preferences,
    Logs,
    SavedState,
    WebData,
    ApplicationScripts,
}

#[cfg(target_os = "macos")]
impl ApplicationLeftoverSource {
    pub(super) const fn stable_code(self) -> &'static str {
        match self {
            Self::SandboxContainer => "sandboxContainer",
            Self::ApplicationSupport => "applicationSupport",
            Self::Preferences => "preferences",
            Self::Logs => "logs",
            Self::SavedState => "savedState",
            Self::WebData => "webData",
            Self::ApplicationScripts => "applicationScripts",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ApplicationLeftoverConfidence {
    High,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ApplicationLeftoverEvidence {
    ContainerMetadataVerified,
    FormerBundleMissing,
    InstalledOwnerAbsent,
    ExactIdentifierAssociation,
    FilesystemSnapshotComplete,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ApplicationLeftoverCandidate {
    pub candidate_id: String,
    pub application_identifier: String,
    pub application_name: String,
    pub source: ApplicationLeftoverSource,
    pub path: String,
    pub bytes: u64,
    pub file_count: u64,
    pub modified_at_ms: Option<u64>,
    pub confidence: ApplicationLeftoverConfidence,
    pub default_selected: bool,
    pub evidence: Vec<ApplicationLeftoverEvidence>,
    /// The complete metadata snapshot is returned so a reviewed GUI result can
    /// explain why a later execution rejects an item after it changes.
    pub snapshot_fingerprint: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ApplicationLeftoverScanResult {
    pub schema_version: u32,
    pub scanned_at_ms: u64,
    pub supported: bool,
    pub inventory_complete: bool,
    pub access_limited: bool,
    pub candidates: Vec<ApplicationLeftoverCandidate>,
    pub total_bytes: u64,
    pub total_file_count: u64,
    pub skipped_count: u64,
    pub elapsed_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ApplicationLeftoverPlanItem {
    pub candidate_id: String,
    pub expected_bytes: u64,
    pub expected_file_count: u64,
    pub expected_snapshot_fingerprint: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ApplicationLeftoverPlan {
    pub schema_version: u32,
    pub plan_id: String,
    pub plan_hash: String,
    pub created_at_ms: u64,
    pub items: Vec<ApplicationLeftoverPlanItem>,
    pub expected_bytes: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ApplicationLeftoverActionStatus {
    Previewed,
    Completed,
    Cancelled,
    Failed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ApplicationLeftoverActionReason {
    CandidateChanged,
    OwnerReappeared,
    ApplicationRunning,
    #[serde(alias = "moveToTrashFailed")]
    PermanentDeleteFailed,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ApplicationLeftoverActionResult {
    pub candidate_id: String,
    pub application_identifier: String,
    pub application_name: String,
    pub status: ApplicationLeftoverActionStatus,
    pub reason: Option<ApplicationLeftoverActionReason>,
    pub expected_bytes: u64,
    pub released_bytes: u64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ApplicationLeftoverResult {
    pub plan_id: String,
    pub expected_bytes: u64,
    pub released_bytes: u64,
    pub affected_item_count: u64,
    pub failed_item_count: u64,
    pub dry_run: bool,
    pub actions: Vec<ApplicationLeftoverActionResult>,
    pub history_saved: bool,
}

#[cfg(test)]
mod tests {
    use super::ApplicationLeftoverActionReason;

    #[test]
    fn legacy_trash_failure_reason_migrates_to_permanent_delete_failure() {
        let reason =
            serde_json::from_str::<ApplicationLeftoverActionReason>("\"moveToTrashFailed\"")
                .expect("legacy persisted action reason must remain readable");

        assert_eq!(
            reason,
            ApplicationLeftoverActionReason::PermanentDeleteFailed
        );
        assert_eq!(
            serde_json::to_string(&reason).expect("current action reason must serialize"),
            "\"permanentDeleteFailed\""
        );
    }
}

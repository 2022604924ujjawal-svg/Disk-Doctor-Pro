use serde::Serialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum DuplicateGroupKind {
    File,
    Directory,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DuplicateFileEntry {
    pub name: String,
    pub path: String,
    pub parent_path: String,
    pub bytes: u64,
    pub modified_at_ms: Option<u64>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DuplicateGroup {
    pub id: String,
    pub hash: String,
    pub kind: DuplicateGroupKind,
    pub bytes_per_file: u64,
    /// Number of regular files represented by one entry. File groups always use one.
    pub file_count_per_entry: u64,
    pub reclaimable_bytes: u64,
    pub entries: Vec<DuplicateFileEntry>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DuplicateFilesResult {
    pub scan_id: u64,
    pub roots: Vec<String>,
    pub scanned_at_ms: u64,
    pub scanned_file_count: u64,
    pub skipped_count: u64,
    pub duplicate_file_count: u64,
    pub total_duplicate_bytes: u64,
    pub reclaimable_bytes: u64,
    pub total_group_count: u64,
    pub returned_group_count: u64,
    pub truncated: bool,
    pub groups: Vec<DuplicateGroup>,
}

/// Streams only groups that passed full-content hashing.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DuplicateGroupBatch {
    pub operation_id: u64,
    pub sequence: u64,
    pub groups: Vec<DuplicateGroup>,
    pub found_group_count: u64,
    pub found_file_count: u64,
    pub found_total_bytes: u64,
    pub found_reclaimable_bytes: u64,
    pub elapsed_ms: u64,
}

/// Pages duplicate groups from the in-memory session created by the current scan.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DuplicateGroupPage {
    pub scan_id: u64,
    pub offset: u64,
    pub next_offset: Option<u64>,
    pub total_count: u64,
    pub groups: Vec<DuplicateGroup>,
}

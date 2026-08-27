use serde::Serialize;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LargeFileEntry {
    pub name: String,
    pub path: String,
    pub parent_path: String,
    pub bytes: u64,
    pub modified_at_ms: Option<u64>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LargeFilesResult {
    pub scan_id: u64,
    pub root: String,
    pub scanned_at_ms: u64,
    pub minimum_bytes: u64,
    pub total_bytes: u64,
    pub total_count: u64,
    pub returned_count: u64,
    pub truncated: bool,
    pub skipped_count: u64,
    pub cache_reused: bool,
    pub entries: Vec<LargeFileEntry>,
}

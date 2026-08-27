use std::{
    collections::{HashMap, HashSet},
    sync::{Mutex, MutexGuard, OnceLock},
};

use mangodisk_platform::{current_platform, Platform};

use super::{DuplicateFilesResult, DuplicateGroupKind, DuplicateGroupPage};
use crate::filesystem::PermanentDeleteCandidate;

pub(super) const DUPLICATE_RESULT_PAGE_SIZE: usize = 40;
const DUPLICATE_RESULT_PAGE_LIMIT: usize = 100;

static DUPLICATE_RESULT_SESSION: OnceLock<Mutex<Option<DuplicateResultSession>>> = OnceLock::new();

struct DuplicateResultSession {
    result: DuplicateFilesResult,
}

#[derive(Debug)]
pub(super) struct ValidatedDuplicateDeleteCandidate {
    pub(super) candidate: PermanentDeleteCandidate,
    pub(super) kind: DuplicateGroupKind,
    pub(super) expected_hash: String,
    pub(super) expected_file_count: u64,
    pub(super) scan_root: String,
}

fn result_session() -> &'static Mutex<Option<DuplicateResultSession>> {
    DUPLICATE_RESULT_SESSION.get_or_init(|| Mutex::new(None))
}

fn lock_result_session() -> Result<MutexGuard<'static, Option<DuplicateResultSession>>, String> {
    result_session()
        .lock()
        .map_err(|_| "the duplicate-file result session is unavailable".to_string())
}

pub(super) fn clear_result_session() -> Result<(), String> {
    *lock_result_session()? = None;
    Ok(())
}

pub(super) fn publish_result_session(
    mut result: DuplicateFilesResult,
) -> Result<DuplicateFilesResult, String> {
    let full_groups = std::mem::take(&mut result.groups);
    result.groups = full_groups
        .iter()
        .take(DUPLICATE_RESULT_PAGE_SIZE)
        .cloned()
        .collect();
    let mut session_result = result.clone();
    session_result.groups = full_groups;
    *lock_result_session()? = Some(DuplicateResultSession {
        result: session_result,
    });
    Ok(result)
}

pub(super) fn result_page(
    scan_id: u64,
    offset: u64,
    limit: u64,
) -> Result<DuplicateGroupPage, String> {
    if limit == 0 {
        return Err("the duplicate-file page size must be greater than zero".to_string());
    }
    let offset = usize::try_from(offset)
        .map_err(|_| "the duplicate-file page offset is out of range".to_string())?;
    let limit = usize::try_from(limit)
        .unwrap_or(DUPLICATE_RESULT_PAGE_LIMIT)
        .min(DUPLICATE_RESULT_PAGE_LIMIT);
    let session = lock_result_session()?;
    let result = &session
        .as_ref()
        .filter(|session| session.result.scan_id == scan_id)
        .ok_or_else(|| "the duplicate-file result session expired; scan again".to_string())?
        .result;
    if offset > result.groups.len() {
        return Err("the duplicate-file page offset exceeds the current result".to_string());
    }
    let end = offset.saturating_add(limit).min(result.groups.len());
    let groups = result.groups[offset..end].to_vec();
    let next_offset = (end < result.groups.len()).then(|| u64::try_from(end).unwrap_or(u64::MAX));
    Ok(DuplicateGroupPage {
        scan_id,
        offset: u64::try_from(offset).unwrap_or(u64::MAX),
        next_offset,
        total_count: u64::try_from(result.groups.len()).unwrap_or(u64::MAX),
        groups,
    })
}

pub(super) fn resolve_open_target(scan_id: u64, selected_path: &str) -> Result<String, String> {
    let session = lock_result_session()?;
    let result = &session
        .as_ref()
        .filter(|session| session.result.scan_id == scan_id)
        .ok_or_else(|| "the duplicate-file result session expired; scan again".to_string())?
        .result;
    result
        .groups
        .iter()
        .flat_map(|group| &group.entries)
        .find(|entry| entry.path == selected_path)
        .map(|entry| entry.path.clone())
        .ok_or_else(|| {
            "the selected item is not part of the current duplicate-file scan".to_string()
        })
}

/// Validates permanent-delete input against the authoritative duplicate scan session.
///
/// The WebView owns selection state, but it is not a safety boundary. Permanent deletion must
/// therefore reject stale or fabricated entries and must preserve at least one file in every
/// affected duplicate group.
pub(super) fn validate_permanent_delete_candidates(
    scan_id: u64,
    candidates: Vec<PermanentDeleteCandidate>,
) -> Result<Vec<ValidatedDuplicateDeleteCandidate>, String> {
    if candidates.is_empty() {
        return Err("at least one duplicate file must be selected".to_string());
    }

    let session = lock_result_session()?;
    let result = &session
        .as_ref()
        .filter(|session| session.result.scan_id == scan_id)
        .ok_or_else(|| "the duplicate-file result session expired; scan again".to_string())?
        .result;
    let mut candidates_by_path = HashMap::with_capacity(candidates.len());
    for candidate in &candidates {
        if candidates_by_path
            .insert(candidate.path.as_str(), candidate)
            .is_some()
        {
            return Err("a duplicate file was selected more than once".to_string());
        }
    }

    let mut matched_paths = HashSet::with_capacity(candidates.len());
    let mut validated = Vec::with_capacity(candidates.len());
    for group in &result.groups {
        let selected_count = group
            .entries
            .iter()
            .filter(|entry| candidates_by_path.contains_key(entry.path.as_str()))
            .count();
        if selected_count == group.entries.len() && selected_count > 0 {
            return Err("at least one file must remain in every duplicate group".to_string());
        }
        for entry in &group.entries {
            let Some(candidate) = candidates_by_path.get(entry.path.as_str()).copied() else {
                continue;
            };
            if candidate.expected_bytes != entry.bytes
                || candidate.expected_modified_at_ms != entry.modified_at_ms
            {
                return Err("a duplicate file no longer matches the scan result".to_string());
            }
            matched_paths.insert(entry.path.as_str());
            let scan_root = result
                .roots
                .iter()
                .filter(|root| {
                    current_platform().path_is_same_or_child(
                        std::path::Path::new(&entry.path),
                        std::path::Path::new(root),
                    )
                })
                .max_by_key(|root| std::path::Path::new(root).components().count())
                .cloned()
                .ok_or_else(|| "a duplicate item is outside the current scan roots".to_string())?;
            validated.push(ValidatedDuplicateDeleteCandidate {
                candidate: candidate.clone(),
                kind: group.kind,
                expected_hash: group.hash.clone(),
                expected_file_count: group.file_count_per_entry,
                scan_root,
            });
        }
    }
    if matched_paths.len() != candidates.len() {
        return Err("a selected file is not part of the current duplicate scan".to_string());
    }

    Ok(validated)
}

pub(super) fn synchronize_result_session(
    scan_id: u64,
    removed_paths: Vec<String>,
) -> Result<DuplicateFilesResult, String> {
    let removed_paths = removed_paths.into_iter().collect::<HashSet<_>>();
    let mut session = lock_result_session()?;
    let result = &mut session
        .as_mut()
        .filter(|session| session.result.scan_id == scan_id)
        .ok_or_else(|| "the duplicate-file result session expired; scan again".to_string())?
        .result;
    let mut removed_duplicate_files = 0_u64;
    let mut removed_duplicate_bytes = 0_u64;
    let mut removed_reclaimable_bytes = 0_u64;
    let mut removed_groups = 0_u64;

    result.groups.retain_mut(|group| {
        let previous_entry_count = u64::try_from(group.entries.len()).unwrap_or(u64::MAX);
        let previous_count = previous_entry_count.saturating_mul(group.file_count_per_entry);
        let previous_reclaimable = group.reclaimable_bytes;
        group
            .entries
            .retain(|entry| !removed_paths.contains(&entry.path));
        let remains_duplicate = group.entries.len() > 1;
        let remaining_entry_count = if remains_duplicate {
            u64::try_from(group.entries.len()).unwrap_or(u64::MAX)
        } else {
            0
        };
        let remaining_count = remaining_entry_count.saturating_mul(group.file_count_per_entry);
        removed_duplicate_files =
            removed_duplicate_files.saturating_add(previous_count.saturating_sub(remaining_count));
        removed_duplicate_bytes = removed_duplicate_bytes.saturating_add(
            group
                .bytes_per_file
                .saturating_mul(previous_entry_count.saturating_sub(remaining_entry_count)),
        );
        let remaining_reclaimable = if remains_duplicate {
            group
                .bytes_per_file
                .saturating_mul(remaining_entry_count.saturating_sub(1))
        } else {
            0
        };
        group.reclaimable_bytes = remaining_reclaimable;
        removed_reclaimable_bytes = removed_reclaimable_bytes
            .saturating_add(previous_reclaimable.saturating_sub(remaining_reclaimable));
        if !remains_duplicate {
            removed_groups = removed_groups.saturating_add(1);
        }
        remains_duplicate
    });
    result.groups.sort_by(|left, right| {
        right
            .reclaimable_bytes
            .cmp(&left.reclaimable_bytes)
            .then_with(|| left.hash.cmp(&right.hash))
    });
    result.duplicate_file_count = result
        .duplicate_file_count
        .saturating_sub(removed_duplicate_files);
    result.total_duplicate_bytes = result
        .total_duplicate_bytes
        .saturating_sub(removed_duplicate_bytes);
    result.reclaimable_bytes = result
        .reclaimable_bytes
        .saturating_sub(removed_reclaimable_bytes);
    result.total_group_count = result.total_group_count.saturating_sub(removed_groups);
    result.returned_group_count = u64::try_from(result.groups.len()).unwrap_or(u64::MAX);
    result.truncated = result.returned_group_count < result.total_group_count;

    let mut response = result.clone();
    response.groups.truncate(DUPLICATE_RESULT_PAGE_SIZE);
    Ok(response)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shared::operation::test_operation_lock;
    use crate::storage::duplicates::{DuplicateFileEntry, DuplicateGroup};

    fn result() -> DuplicateFilesResult {
        let entries = ["a", "b", "c"]
            .into_iter()
            .map(|name| DuplicateFileEntry {
                name: format!("{name}.bin"),
                path: format!("/fixture/{name}.bin"),
                parent_path: "/fixture".to_string(),
                bytes: 10,
                modified_at_ms: Some(1),
            })
            .collect::<Vec<_>>();
        DuplicateFilesResult {
            scan_id: 42,
            roots: vec!["/fixture".to_string()],
            scanned_at_ms: 1,
            scanned_file_count: 3,
            skipped_count: 0,
            duplicate_file_count: 3,
            total_duplicate_bytes: 30,
            reclaimable_bytes: 20,
            total_group_count: 1,
            returned_group_count: 1,
            truncated: false,
            groups: vec![DuplicateGroup {
                id: "group".to_string(),
                hash: "hash".to_string(),
                kind: DuplicateGroupKind::File,
                bytes_per_file: 10,
                file_count_per_entry: 1,
                reclaimable_bytes: 20,
                entries,
            }],
        }
    }

    fn candidate(path: &str) -> PermanentDeleteCandidate {
        PermanentDeleteCandidate {
            path: path.to_string(),
            expected_bytes: 10,
            expected_modified_at_ms: Some(1),
        }
    }

    #[test]
    fn permanent_delete_validation_preserves_one_file_per_group() {
        let _operation_lock = test_operation_lock();
        publish_result_session(result()).expect("publish the duplicate fixture");

        let allowed = validate_permanent_delete_candidates(
            42,
            vec![candidate("/fixture/a.bin"), candidate("/fixture/b.bin")],
        )
        .expect("deleting duplicate copies while keeping one should be allowed");
        assert_eq!(allowed.len(), 2);
        assert_eq!(allowed[0].kind, DuplicateGroupKind::File);

        let error = validate_permanent_delete_candidates(
            42,
            vec![
                candidate("/fixture/a.bin"),
                candidate("/fixture/b.bin"),
                candidate("/fixture/c.bin"),
            ],
        )
        .expect_err("deleting every file in a duplicate group must be rejected");
        assert!(error.contains("at least one file must remain"));
        clear_result_session().expect("clear the duplicate fixture");
    }

    #[test]
    fn permanent_delete_validation_rejects_entries_outside_the_scan() {
        let _operation_lock = test_operation_lock();
        publish_result_session(result()).expect("publish the duplicate fixture");

        let error =
            validate_permanent_delete_candidates(42, vec![candidate("/fixture/not-scanned.bin")])
                .expect_err("an entry outside the scan must be rejected");
        assert!(error.contains("not part of the current duplicate scan"));
        clear_result_session().expect("clear the duplicate fixture");
    }

    #[test]
    fn permanent_delete_validation_rejects_authoritative_entries_outside_the_scan_root() {
        let _operation_lock = test_operation_lock();
        let mut fixture = result();
        fixture.groups[0].entries[0].path = "/outside/a.bin".to_string();
        fixture.groups[0].entries[0].parent_path = "/outside".to_string();
        publish_result_session(fixture).expect("publish the out-of-scope duplicate fixture");

        let error = validate_permanent_delete_candidates(42, vec![candidate("/outside/a.bin")])
            .expect_err("an authoritative entry outside every scan root must be rejected");
        assert!(error.contains("outside the current scan roots"));
        clear_result_session().expect("clear the out-of-scope duplicate fixture");
    }

    #[test]
    fn duplicate_open_target_is_bound_to_the_scan_result() {
        let _operation_lock = test_operation_lock();
        publish_result_session(result()).expect("publish the duplicate fixture");

        let target = resolve_open_target(42, "/fixture/a.bin")
            .expect("resolve the published duplicate file");
        assert_eq!(target, "/fixture/a.bin");
        assert!(resolve_open_target(42, "/fixture/not-scanned.bin").is_err());
        assert!(resolve_open_target(41, "/fixture/a.bin").is_err());
        clear_result_session().expect("clear the duplicate fixture");
    }
}

use std::{fs, path::Path};

use crate::{
    cleanup::rules::{matches_rule, MatcherSpec},
    filesystem::metadata::is_link_like,
};

type EntryFilter<'a> = dyn Fn(&Path, &fs::Metadata) -> bool + 'a;

#[derive(Default)]
pub(crate) struct MeasureResult {
    pub(crate) bytes: u64,
    pub(crate) file_count: u64,
    pub(crate) skipped_count: u64,
}

/// Re-measures a cleanup root using the same matcher and ownership filter as
/// the scan plan. The filter receives metadata already read by this traversal,
/// so execution validation does not add another filesystem lookup per entry.
pub(crate) fn measure_path_filtered(
    path: &Path,
    matcher: Option<&MatcherSpec>,
    filter: &EntryFilter<'_>,
) -> MeasureResult {
    measure_path_inner(path, path, matcher, filter)
}

fn measure_path_inner(
    root: &Path,
    path: &Path,
    matcher: Option<&MatcherSpec>,
    filter: &EntryFilter<'_>,
) -> MeasureResult {
    let Ok(metadata) = fs::symlink_metadata(path) else {
        return MeasureResult::default();
    };
    if is_link_like(&metadata) {
        return MeasureResult {
            skipped_count: 1,
            ..MeasureResult::default()
        };
    }
    if metadata.is_file() {
        if matches_rule(root, path, &metadata, matcher) && filter(path, &metadata) {
            return MeasureResult {
                bytes: metadata.len(),
                file_count: 1,
                skipped_count: 0,
            };
        }
        return MeasureResult::default();
    }
    let Ok(entries) = fs::read_dir(path) else {
        return MeasureResult {
            skipped_count: 1,
            ..MeasureResult::default()
        };
    };
    let mut total = MeasureResult::default();
    for entry in entries {
        let Ok(entry) = entry else {
            total.skipped_count += 1;
            continue;
        };
        let child = measure_path_inner(root, &entry.path(), matcher, filter);
        total.bytes += child.bytes;
        total.file_count += child.file_count;
        total.skipped_count += child.skipped_count;
    }
    total
}

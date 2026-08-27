use std::{
    fs,
    path::Path,
    time::{Duration, SystemTime},
};

use crate::cleanup::rules::MatcherSpec;

pub(crate) fn matches_rule(
    root: &Path,
    path: &Path,
    metadata: &fs::Metadata,
    matcher: Option<&MatcherSpec>,
) -> bool {
    matcher.is_none_or(|matcher| matches_spec(root, path, metadata, matcher))
}

/// Returns a safe upper bound for descendant traversal when a matcher contains
/// an explicit depth gate. `None` means traversal cannot be pruned without
/// changing the set of possible matches.
pub(crate) fn maximum_match_depth(matcher: &MatcherSpec) -> Option<usize> {
    match matcher {
        MatcherSpec::MaxDepth(depth) => Some(*depth),
        MatcherSpec::AllOf(items) => items.iter().filter_map(maximum_match_depth).min(),
        MatcherSpec::AnyOf(items) => {
            let depths = items
                .iter()
                .map(maximum_match_depth)
                .collect::<Option<Vec<_>>>()?;
            depths.into_iter().max()
        }
        MatcherSpec::Not(_) => None,
        _ => None,
    }
}

fn matches_spec(root: &Path, path: &Path, metadata: &fs::Metadata, matcher: &MatcherSpec) -> bool {
    match matcher {
        MatcherSpec::All => true,
        MatcherSpec::NameEquals(values) => path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| values.iter().any(|value| platform_equal(name, value))),
        MatcherSpec::NameGlob(values) => path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| values.iter().any(|pattern| wildcard_match(pattern, name))),
        MatcherSpec::ExtensionIn(values) => path
            .extension()
            .and_then(|extension| extension.to_str())
            .is_some_and(|extension| {
                values
                    .iter()
                    .any(|value| platform_equal(extension, value.trim_start_matches('.')))
            }),
        MatcherSpec::PathSegmentIn(values) => path
            .strip_prefix(root)
            .unwrap_or(path)
            .components()
            .filter_map(|component| component.as_os_str().to_str())
            .any(|segment| values.iter().any(|value| platform_equal(segment, value))),
        MatcherSpec::OlderThanDays(days) => is_older_than(metadata, *days),
        MatcherSpec::LargerThanBytes(bytes) => metadata.len() > *bytes,
        MatcherSpec::SmallerThanBytes(bytes) => metadata.len() < *bytes,
        MatcherSpec::MaxDepth(depth) => path
            .strip_prefix(root)
            .map(|relative| relative.components().count() <= *depth)
            .unwrap_or(false),
        MatcherSpec::AllOf(items) => items
            .iter()
            .all(|item| matches_spec(root, path, metadata, item)),
        MatcherSpec::AnyOf(items) => items
            .iter()
            .any(|item| matches_spec(root, path, metadata, item)),
        MatcherSpec::Not(item) => !matches_spec(root, path, metadata, item),
    }
}

fn platform_equal(left: &str, right: &str) -> bool {
    #[cfg(windows)]
    {
        left.eq_ignore_ascii_case(right)
    }
    #[cfg(not(windows))]
    {
        left == right
    }
}

/// Name globs support only `*` and `?`; paths and recursive `**` are rejected.
/// Build validation limits pattern length and syntax. This linear backtracking
/// implementation avoids regex compilation and ReDoS exposure.
fn wildcard_match(pattern: &str, value: &str) -> bool {
    // Globbing runs on the per-file hot path. Backtracking with UTF-8 boundary
    // indexes avoids allocating two Vec<char> values for every file, which
    // materially reduces allocator pressure during million-file scans.
    let mut pattern_index = 0usize;
    let mut value_index = 0usize;
    let mut star_pattern_index = None;
    let mut star_value_index = 0usize;
    while value_index < value.len() {
        let pattern_character = next_character(pattern, pattern_index);
        let value_character =
            next_character(value, value_index).expect("value index must be on a UTF-8 boundary");
        if pattern_character.is_some_and(|(character, _)| character == '*') {
            let (_, next_pattern_index) = pattern_character.expect("star character must exist");
            star_pattern_index = Some(next_pattern_index);
            pattern_index = next_pattern_index;
            star_value_index = value_index;
        } else if pattern_character.is_some_and(|(character, _)| {
            character == '?' || platform_character_equal(character, value_character.0)
        }) {
            pattern_index = pattern_character.expect("matching character must exist").1;
            value_index = value_character.1;
        } else if let Some(after_star) = star_pattern_index {
            pattern_index = after_star;
            star_value_index = next_character(value, star_value_index)
                .map(|(_, next_index)| next_index)
                .unwrap_or(value.len());
            value_index = star_value_index;
        } else {
            return false;
        }
    }
    while next_character(pattern, pattern_index).is_some_and(|(character, _)| character == '*') {
        pattern_index = next_character(pattern, pattern_index)
            .expect("star character must exist")
            .1;
    }
    pattern_index == pattern.len()
}

fn next_character(value: &str, index: usize) -> Option<(char, usize)> {
    value
        .get(index..)?
        .chars()
        .next()
        .map(|character| (character, index + character.len_utf8()))
}

fn platform_character_equal(left: char, right: char) -> bool {
    #[cfg(windows)]
    {
        left.eq_ignore_ascii_case(&right)
    }
    #[cfg(not(windows))]
    {
        left == right
    }
}

fn is_older_than(metadata: &fs::Metadata, days: u64) -> bool {
    metadata.modified().ok().is_some_and(|modified| {
        SystemTime::now()
            .duration_since(modified)
            .map(|age| age >= Duration::from_secs(days.saturating_mul(86_400)))
            .unwrap_or(false)
    })
}

#[cfg(test)]
mod tests {
    use std::{fs, time::SystemTime};

    use super::{matches_rule, maximum_match_depth, wildcard_match};
    use crate::cleanup::rules::MatcherSpec;

    #[test]
    fn name_glob_matches_one_file_name_only() {
        assert!(wildcard_match("cache-?.bin", "cache-a.bin"));
        assert!(wildcard_match("thumb*", "thumbnail.db"));
        assert!(!wildcard_match("cache-?.bin", "cache-long.bin"));
        assert!(!wildcard_match("*.log", "report.txt"));
        assert!(wildcard_match("cache-?.bin", "cache-a.bin"));
    }

    #[test]
    fn declarative_matchers_combine_root_and_metadata_conditions() {
        let root = std::env::temp_dir().join(format!(
            "mangodisk-matcher-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .expect("system time must be valid")
                .as_nanos()
        ));
        let nested = root.join("Code Cache");
        fs::create_dir_all(&nested).expect("matcher fixture directory must be created");
        let file = nested.join("cache-a.bin");
        fs::write(&file, b"1234").expect("matcher fixture file must be written");
        let metadata = fs::metadata(&file).expect("matcher fixture metadata must be readable");

        let matcher = MatcherSpec::AllOf(vec![
            MatcherSpec::NameGlob(vec!["cache-?.bin".to_string()]),
            MatcherSpec::ExtensionIn(vec!["bin".to_string()]),
            MatcherSpec::PathSegmentIn(vec!["Code Cache".to_string()]),
            MatcherSpec::LargerThanBytes(3),
            MatcherSpec::SmallerThanBytes(5),
            MatcherSpec::MaxDepth(2),
            MatcherSpec::Not(Box::new(MatcherSpec::NameEquals(vec![
                "protected.bin".to_string()
            ]))),
        ]);
        assert!(matches_rule(&root, &file, &metadata, Some(&matcher)));
        assert!(!matches_rule(
            &root,
            &file,
            &metadata,
            Some(&MatcherSpec::MaxDepth(1))
        ));
        assert!(matches_rule(
            &root,
            &file,
            &metadata,
            Some(&MatcherSpec::AnyOf(vec![
                MatcherSpec::NameEquals(vec!["missing.bin".to_string()]),
                MatcherSpec::NameEquals(vec!["cache-a.bin".to_string()]),
            ]))
        ));

        fs::remove_dir_all(root).expect("matcher fixture directory must be removed");
    }

    #[test]
    fn traversal_depth_is_derived_only_when_every_possible_branch_is_bounded() {
        assert_eq!(
            maximum_match_depth(&MatcherSpec::AllOf(vec![
                MatcherSpec::ExtensionIn(vec!["part".to_string()]),
                MatcherSpec::MaxDepth(3),
                MatcherSpec::MaxDepth(5),
            ])),
            Some(3)
        );
        assert_eq!(
            maximum_match_depth(&MatcherSpec::AnyOf(vec![
                MatcherSpec::MaxDepth(2),
                MatcherSpec::MaxDepth(4),
            ])),
            Some(4)
        );
        assert_eq!(
            maximum_match_depth(&MatcherSpec::AnyOf(vec![
                MatcherSpec::MaxDepth(2),
                MatcherSpec::NameEquals(vec!["cache".to_string()]),
            ])),
            None
        );
        assert_eq!(
            maximum_match_depth(&MatcherSpec::Not(Box::new(MatcherSpec::MaxDepth(2)))),
            None
        );
    }
}

use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
};

use crate::cleanup::{CleanupSourceSelection, CleanupSourceSelectionMode};

const MAX_RULE_SOURCE_SELECTIONS: usize = 256;
const MAX_SELECTED_SOURCE_PATHS: usize = 4_096;

/// Maps a matched file to the stable source row shown by the cleanup scan.
///
/// Files directly under a rule root share the root row; deeper files are
/// grouped by the first child. Scan and execution must use the same mapping so
/// a source checkbox cannot expand or silently change its deletion scope.
pub(crate) fn cleanup_source_path(rule_root: &Path, matched_file: &Path) -> PathBuf {
    let Ok(relative) = matched_file.strip_prefix(rule_root) else {
        return rule_root.to_path_buf();
    };
    let mut components = relative.components();
    let Some(first) = components.next() else {
        return rule_root.to_path_buf();
    };
    if components.next().is_none() {
        return rule_root.to_path_buf();
    }
    rule_root.join(first.as_os_str())
}

#[derive(Debug)]
pub(crate) struct SourceSelectionPolicy {
    scopes: HashMap<String, SourceScope>,
}

#[derive(Debug)]
pub(crate) struct SourceScope {
    mode: CleanupSourceSelectionMode,
    paths: HashSet<PathBuf>,
}

impl SourceSelectionPolicy {
    #[cfg(test)]
    pub(crate) fn empty() -> Self {
        Self {
            scopes: HashMap::new(),
        }
    }

    pub(crate) fn from_request(
        selected_rule_ids: &HashSet<String>,
        selections: &[CleanupSourceSelection],
    ) -> Result<Self, String> {
        if selections.len() > MAX_RULE_SOURCE_SELECTIONS {
            return Err("the cleanup request contains too many source selections".to_string());
        }
        let mut scopes = HashMap::with_capacity(selections.len());
        let mut total_path_count = 0_usize;
        for selection in selections {
            if !selected_rule_ids.contains(&selection.rule_id) {
                return Err("a cleanup source selection references an unselected rule".to_string());
            }
            if selection.paths.is_empty() {
                return Err("a cleanup source selection must contain at least one path".to_string());
            }
            total_path_count = total_path_count.saturating_add(selection.paths.len());
            if total_path_count > MAX_SELECTED_SOURCE_PATHS {
                return Err("the cleanup request contains too many source paths".to_string());
            }
            let paths = selection
                .paths
                .iter()
                .map(PathBuf::from)
                .collect::<HashSet<_>>();
            if paths.len() != selection.paths.len() || paths.iter().any(|path| !path.is_absolute())
            {
                return Err("cleanup source paths must be unique absolute paths".to_string());
            }
            if scopes
                .insert(
                    selection.rule_id.clone(),
                    SourceScope {
                        mode: selection.mode,
                        paths,
                    },
                )
                .is_some()
            {
                return Err("the cleanup request contains duplicate source selections".to_string());
            }
        }
        Ok(Self { scopes })
    }

    pub(crate) fn scope(&self, rule_id: &str) -> Option<&SourceScope> {
        self.scopes.get(rule_id)
    }
}

impl SourceScope {
    pub(crate) fn selects(&self, source_path: &Path) -> bool {
        let contains = self.paths.contains(source_path);
        match self.mode {
            CleanupSourceSelectionMode::Include => contains,
            CleanupSourceSelectionMode::Exclude => !contains,
        }
    }

    /// Rejects stale or fabricated source paths before a cleaner uses the
    /// selection. The scan summary is only presentation data; live discovery
    /// remains the authority for every destructive operation.
    pub(crate) fn validate_known_paths<'a>(
        &self,
        known_paths: impl IntoIterator<Item = &'a Path>,
    ) -> Result<(), String> {
        let known = known_paths.into_iter().collect::<HashSet<_>>();
        if self.paths.iter().all(|path| known.contains(path.as_path())) {
            Ok(())
        } else {
            Err("a selected cleanup source is no longer available".to_string())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn selected_rules() -> HashSet<String> {
        HashSet::from(["app.cache".to_string()])
    }

    #[test]
    fn request_rejects_relative_duplicate_and_unselected_sources() {
        let relative = CleanupSourceSelection {
            rule_id: "app.cache".to_string(),
            mode: CleanupSourceSelectionMode::Include,
            paths: vec!["relative".to_string()],
        };
        assert!(SourceSelectionPolicy::from_request(&selected_rules(), &[relative]).is_err());

        let duplicate = CleanupSourceSelection {
            rule_id: "app.cache".to_string(),
            mode: CleanupSourceSelectionMode::Include,
            paths: vec!["/cache/a".to_string(), "/cache/a".to_string()],
        };
        assert!(SourceSelectionPolicy::from_request(&selected_rules(), &[duplicate]).is_err());

        let unselected = CleanupSourceSelection {
            rule_id: "app.other".to_string(),
            mode: CleanupSourceSelectionMode::Include,
            paths: vec!["/cache/a".to_string()],
        };
        assert!(SourceSelectionPolicy::from_request(&selected_rules(), &[unselected]).is_err());
    }

    #[test]
    fn include_and_exclude_scopes_validate_live_sources() {
        let include = SourceScope {
            mode: CleanupSourceSelectionMode::Include,
            paths: HashSet::from([PathBuf::from("/cache/a")]),
        };
        assert!(include.selects(Path::new("/cache/a")));
        assert!(!include.selects(Path::new("/cache/b")));
        assert!(include
            .validate_known_paths([Path::new("/cache/a"), Path::new("/cache/b")])
            .is_ok());

        let exclude = SourceScope {
            mode: CleanupSourceSelectionMode::Exclude,
            paths: HashSet::from([PathBuf::from("/cache/a")]),
        };
        assert!(!exclude.selects(Path::new("/cache/a")));
        assert!(exclude.selects(Path::new("/cache/b")));
        assert!(exclude
            .validate_known_paths([Path::new("/cache/b")])
            .is_err());
    }
}

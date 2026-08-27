use std::path::Path;

const VERBATIM_PREFIX: &str = r"\\?\";
const VERBATIM_UNC_PREFIX: &str = r"\\?\UNC\";

/// Converts an internal Windows path to the stable representation exposed to adapters.
/// Canonicalization commonly adds a verbatim prefix, but the prefix is an I/O detail and must
/// not make otherwise identical result paths compare differently across a workflow.
pub(super) fn display(path: &Path) -> String {
    let value = path.to_string_lossy();
    if let Some(path) = strip_ascii_case_prefix(&value, VERBATIM_UNC_PREFIX) {
        return format!(r"\\{path}");
    }
    if let Some(path) = strip_ascii_case_prefix(&value, VERBATIM_PREFIX) {
        return path.to_string();
    }
    value.into_owned()
}

/// Produces a key for Windows path identity without reading the filesystem.
/// Callers that need physical identity must canonicalize first and then compare the resulting
/// paths through this function.
pub(super) fn comparison_key(path: &Path) -> String {
    let normalized = display(path).replace('/', "\\");
    let trimmed = normalized.trim_end_matches('\\');
    let identity = if trimmed.len() == 2
        && trimmed.as_bytes().get(1) == Some(&b':')
        && normalized.len() > trimmed.len()
    {
        format!(r"{trimmed}\")
    } else {
        trimmed.to_string()
    };
    identity.to_lowercase()
}

pub(super) fn equal(left: &Path, right: &Path) -> bool {
    comparison_key(left) == comparison_key(right)
}

pub(super) fn is_same_or_child(path: &Path, root: &Path) -> bool {
    let path = comparison_key(path);
    let root = comparison_key(root);
    path == root
        || root.ends_with('\\') && path.starts_with(&root)
        || path
            .strip_prefix(&root)
            .is_some_and(|suffix| suffix.starts_with('\\'))
}

/// Returns the normalized relative key only when `path` is below `root`.
/// This preserves the same component boundary rules used by containment checks.
pub(super) fn relative_child_key(path: &Path, root: &Path) -> Option<String> {
    let path = comparison_key(path);
    let root = comparison_key(root);
    let suffix = if root.ends_with('\\') {
        path.strip_prefix(&root)?
    } else {
        path.strip_prefix(&root)?.strip_prefix('\\')?
    };
    (!suffix.is_empty()).then(|| suffix.to_string())
}

fn strip_ascii_case_prefix<'a>(value: &'a str, prefix: &str) -> Option<&'a str> {
    let candidate = value.get(..prefix.len())?;
    candidate
        .eq_ignore_ascii_case(prefix)
        .then(|| &value[prefix.len()..])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_removes_verbatim_prefixes_without_changing_path_text() {
        assert_eq!(
            display(Path::new(r"\\?\C:\Users\Developer\Sample.bin")),
            r"C:\Users\Developer\Sample.bin"
        );
        assert_eq!(
            display(Path::new(r"\\?\unc\Server\Share\Sample.bin")),
            r"\\Server\Share\Sample.bin"
        );
    }

    #[test]
    fn identity_ignores_verbatim_prefix_casing_and_separators() {
        assert!(equal(
            Path::new(r"\\?\C:\Users\Developer\Sample.bin"),
            Path::new(r"c:/users/developer/sample.bin")
        ));
        assert!(is_same_or_child(
            Path::new(r"\\?\C:\Users\Developer\Projects\MangoDisk"),
            Path::new(r"c:/USERS/developer")
        ));
        assert!(!is_same_or_child(
            Path::new(r"C:\Users\Developer-Archive"),
            Path::new(r"C:\Users\Developer")
        ));
        assert_eq!(comparison_key(Path::new(r"C:\")), r"c:\");
        assert_ne!(
            comparison_key(Path::new(r"C:\")),
            comparison_key(Path::new("C:"))
        );
        assert!(is_same_or_child(
            Path::new(r"C:\fixture"),
            Path::new(r"c:\")
        ));
        assert_eq!(
            relative_child_key(
                Path::new(r"\\?\C:\Program Files\Example\app.exe"),
                Path::new(r"c:/program files")
            )
            .as_deref(),
            Some(r"example\app.exe")
        );
        assert!(relative_child_key(
            Path::new(r"C:\Program Files-Archive\app.exe"),
            Path::new(r"C:\Program Files")
        )
        .is_none());
    }
}

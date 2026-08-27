/// Automatic cleanup must never own these top-level home directories. The
/// list is intentionally narrower than a user-configurable keep list: it
/// contains only identity, source, backup, synchronization, and virtual
/// machine locations whose contents cannot be inferred from a cache-like
/// descendant name.
const PROTECTED_HOME_ROOTS: &[&str] = &[
    ".ansible",
    ".aws",
    ".azure",
    ".config",
    ".docker",
    ".env",
    ".gcloud",
    ".git-credentials",
    ".gnupg",
    ".idea",
    ".kube",
    ".netrc",
    ".password-store",
    ".ssh",
    ".vscode",
    "backup",
    "backups",
    "box",
    "code",
    "creative cloud files",
    "dev",
    "dropbox",
    "google drive",
    "icloud drive",
    "my virtual machines",
    "nextcloud",
    "onedrive",
    "owncloud",
    "projects",
    "repos",
    "repositories",
    "saved games",
    "source",
    "src",
    "sync",
    "virtualbox vms",
    "vms",
    "workspace",
    "workspaces",
];

/// Repository metadata is protected at any depth. Build-output cleanup is
/// allowed inside a project, but repository state is never a build artifact.
const PROTECTED_REPOSITORY_COMPONENTS: &[&str] = &[".bzr", ".git", ".hg", ".svn"];

/// These Library subtrees contain user state even though they live below the
/// same Library directory as caches. The runtime check prevents a future
/// matcher from turning a narrowly authored rule into an unsafe root.
const PROTECTED_LIBRARY_ROOTS: &[&str] = &[
    "accounts",
    "addressbook",
    "calendars",
    "cloudstorage",
    "daemon containers",
    "keychains",
    "mail",
    "messages",
    "mobile documents",
    "photos",
    "safari",
];

/// File Provider and CloudKit keep synchronization databases, working-set
/// metadata, and provider coordination state in these otherwise cache-looking
/// locations. Automatic cleanup must preserve the complete subtree because a
/// partial removal can force a resync or detach locally materialized content.
const PROTECTED_LIBRARY_SUBTREES: &[&[&str]] = &[
    &["application support", "clouddocs"],
    &["application support", "fileprovider"],
    &["caches", "cloudkit"],
    &["caches", "com.apple.bird"],
    &["caches", "com.apple.cloudd"],
    &["caches", "com.apple.clouddocs"],
    &["caches", "com.apple.fileprovider"],
];

pub(crate) fn is_protected_home_root(value: &str) -> bool {
    let normalized = value.to_ascii_lowercase();
    if normalized == "onedrive" || normalized.starts_with("onedrive - ") {
        return true;
    }
    PROTECTED_HOME_ROOTS
        .iter()
        .any(|protected| value.eq_ignore_ascii_case(protected))
}

pub(crate) fn is_protected_repository_component(value: &str) -> bool {
    PROTECTED_REPOSITORY_COMPONENTS
        .iter()
        .any(|protected| value.eq_ignore_ascii_case(protected))
}

pub(crate) fn is_protected_library_root(value: &str) -> bool {
    PROTECTED_LIBRARY_ROOTS
        .iter()
        .any(|protected| value.eq_ignore_ascii_case(protected))
}

fn is_protected_library_subtree(components: &[String]) -> bool {
    PROTECTED_LIBRARY_SUBTREES.iter().any(|protected| {
        components.len() >= protected.len()
            && components
                .iter()
                .zip(*protected)
                .all(|(actual, expected)| actual.eq_ignore_ascii_case(expected))
    })
}

pub(crate) fn is_protected_home_relative_path(components: &[String]) -> bool {
    components
        .first()
        .is_some_and(|first| is_protected_home_root(first))
        || components
            .iter()
            .any(|component| is_protected_repository_component(component))
        || components.len() >= 2
            && ((components[0].eq_ignore_ascii_case("Library")
                && (is_protected_library_root(&components[1])
                    || is_protected_library_subtree(&components[1..])))
                || (components[0].eq_ignore_ascii_case(".local")
                    && components[1].eq_ignore_ascii_case("share")
                    && !is_verified_local_share_cache_root(components)))
}

/// Allows only platform-standard cache roots inside `~/.local/share`, which is
/// otherwise protected because applications commonly persist user state
/// there. Exact component matching prevents a cache-looking descendant from
/// weakening the boundary for an unrelated application directory.
fn is_verified_local_share_cache_root(components: &[String]) -> bool {
    components.len() == 4
        && components[0].eq_ignore_ascii_case(".local")
        && components[1].eq_ignore_ascii_case("share")
        && components[2].eq_ignore_ascii_case("NuGet")
        && matches!(
            components[3].to_ascii_lowercase().as_str(),
            "http-cache" | "v3-cache"
        )
}

use std::path::{Path, PathBuf};

use crate::{ApplicationDirectories, PlatformError, PlatformResult, UserDirectories};

const SYSTEM_DIRECTORY: &str = "/System";
const SHARED_LIBRARY_DIRECTORY: &str = "/Library";
const APPLICATIONS_DIRECTORY: &str = "/Applications";
const SYSTEM_APPLICATIONS_DIRECTORY: &str = "/System/Applications";
const DUPLICATE_PROTECTED_DIRECTORIES: [&str; 17] = [
    "/bin",
    "/sbin",
    "/etc",
    "/usr",
    "/private/etc",
    "/private/var/db",
    "/var/db",
    "/private/var/log",
    "/var/log",
    "/private/var/run",
    "/var/run",
    "/private/var/root",
    "/var/root",
    "/private/var/vm",
    "/var/vm",
    "/cores",
    "/.vol",
];
const DUPLICATE_TRANSIENT_DIRECTORIES: [&str; 5] = [
    "/private/var/folders",
    "/var/folders",
    "/private/var/tmp",
    "/private/tmp",
    "/tmp",
];
const VOLUME_METADATA_DIRECTORY_NAMES: [&str; 4] = [
    ".DocumentRevisions-V100",
    ".Spotlight-V100",
    ".Trashes",
    ".fseventsd",
];
const MANAGED_BUNDLE_EXTENSIONS: [&str; 10] = [
    "app",
    "appex",
    "bundle",
    "framework",
    "kext",
    "mdimporter",
    "plugin",
    "prefpane",
    "qlgenerator",
    "xpc",
];

pub(super) fn application_directories(identifier: &str) -> PlatformResult<ApplicationDirectories> {
    let local_data_directory = local_data_directory()?;
    Ok(ApplicationDirectories {
        local_data_directory: local_data_directory.join(identifier),
        cache_directory: cache_directory()?.join(identifier),
    })
}

pub(super) fn user_directories() -> PlatformResult<UserDirectories> {
    let home_directory = home_directory()?;
    let cache_directory = cache_directory()?;
    let local_data_directory = local_data_directory()?;
    let data_directory = dirs::data_dir().ok_or_else(|| {
        PlatformError::invalid_path("macOS application data directory is unavailable")
    })?;
    Ok(UserDirectories::new(
        home_directory,
        std::env::temp_dir(),
        cache_directory,
        [local_data_directory, data_directory],
    ))
}

pub(super) fn home_directory() -> PlatformResult<PathBuf> {
    dirs::home_dir().ok_or_else(|| PlatformError::invalid_path("macOS user home is unavailable"))
}

fn local_data_directory() -> PlatformResult<PathBuf> {
    dirs::data_local_dir().ok_or_else(|| {
        PlatformError::invalid_path("macOS local application data directory is unavailable")
    })
}

fn cache_directory() -> PlatformResult<PathBuf> {
    dirs::cache_dir()
        .ok_or_else(|| PlatformError::invalid_path("macOS cache directory is unavailable"))
}

pub(super) fn application_installation_directories(home: &Path) -> [PathBuf; 3] {
    [
        PathBuf::from(APPLICATIONS_DIRECTORY),
        PathBuf::from(SYSTEM_APPLICATIONS_DIRECTORY),
        home.join("Applications"),
    ]
}

pub(super) fn is_system_critical(path: &Path) -> bool {
    [
        SYSTEM_DIRECTORY,
        "/Library/Apple",
        "/private/var/vm",
        "/dev",
        "/Volumes/.timemachine",
    ]
    .into_iter()
    .any(|root| path == Path::new(root) || path.starts_with(root))
}

pub(super) fn is_shared_library_or_application(path: &Path) -> bool {
    [SHARED_LIBRARY_DIRECTORY, APPLICATIONS_DIRECTORY]
        .into_iter()
        .any(|root| path == Path::new(root) || path.starts_with(root))
}

/// Returns whether duplicate discovery reached an operating-system-owned subtree.
///
/// These locations contain system databases, runtime state, logs, and volume metadata. Byte-equal
/// files in them are not independently removable copies, so duplicate cleanup must never expose
/// them even when the user starts a scan at the volume root.
pub(super) fn is_protected_duplicate_scope(path: &Path) -> bool {
    DUPLICATE_PROTECTED_DIRECTORIES
        .into_iter()
        .any(|root| path == Path::new(root) || path.starts_with(root))
        || path.components().any(|component| {
            let value = component.as_os_str();
            VOLUME_METADATA_DIRECTORY_NAMES
                .iter()
                .any(|name| value == *name)
        })
        || contains_managed_bundle(path)
}

/// Returns whether a path belongs to a code-signed or application-managed bundle.
///
/// These bundles are independently managed units even though the filesystem exposes their
/// resources as ordinary directories and files. Duplicate cleanup must not offer bundle internals
/// as removable copies because deleting one can invalidate code signing or break its owner.
/// Personal document packages are deliberately absent so they remain eligible for exact matching.
fn contains_managed_bundle(path: &Path) -> bool {
    path.components().any(|component| {
        component
            .as_os_str()
            .to_str()
            .and_then(|value| value.rsplit_once('.').map(|(_, extension)| extension))
            .is_some_and(|extension| {
                MANAGED_BUNDLE_EXTENSIONS
                    .iter()
                    .any(|candidate| extension.eq_ignore_ascii_case(candidate))
            })
    })
}

/// Returns whether a broad scan reached macOS-managed temporary or application data.
///
/// Code-signing clones and application extraction trees under `/private/var/folders` can contain
/// many byte-identical copies that disappear without user action. `~/Library` also contains app
/// containers, caches, support bundles, and managed databases whose internal copies are not
/// independent user documents. These files belong in Deep Cleanup or App Uninstaller rather than
/// Duplicate File Cleanup. An explicit scan rooted inside one of these locations remains allowed
/// so the platform policy does not silently override a narrowly stated user request.
pub(super) fn is_transient_duplicate_scope(path: &Path, scan_root: &Path) -> bool {
    let path_is_transient = DUPLICATE_TRANSIENT_DIRECTORIES
        .into_iter()
        .any(|root| path.starts_with(root))
        || is_user_library(path);
    let root_is_transient = DUPLICATE_TRANSIENT_DIRECTORIES
        .into_iter()
        .any(|root| scan_root.starts_with(root))
        || is_user_library(scan_root);
    path_is_transient && !root_is_transient
}

fn is_user_library(path: &Path) -> bool {
    let mut components = path.components();
    matches!(components.next(), Some(std::path::Component::RootDir))
        && components
            .next()
            .is_some_and(|value| value.as_os_str() == "Users")
        && components.next().is_some()
        && components
            .next()
            .is_some_and(|value| value.as_os_str() == "Library")
}

pub(super) fn is_protected_cleanup_path(path: &Path) -> bool {
    [
        SYSTEM_DIRECTORY,
        "/Library/Apple",
        APPLICATIONS_DIRECTORY,
        "/private/var/vm",
    ]
    .into_iter()
    .any(|root| path.starts_with(root))
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::{
        application_installation_directories, is_protected_cleanup_path,
        is_protected_duplicate_scope, is_shared_library_or_application, is_system_critical,
        is_transient_duplicate_scope,
    };

    #[test]
    fn application_installation_directories_cover_system_and_user_domains() {
        let roots = application_installation_directories(Path::new("/Users/example"));
        assert_eq!(roots[0].as_path(), Path::new("/Applications"));
        assert_eq!(roots[1].as_path(), Path::new("/System/Applications"));
        assert_eq!(roots[2].as_path(), Path::new("/Users/example/Applications"));
    }

    #[test]
    fn directory_boundaries_do_not_match_similar_prefixes() {
        assert!(is_system_critical(Path::new("/System/Library")));
        assert!(!is_system_critical(Path::new("/Systematic")));
        assert!(is_shared_library_or_application(Path::new(
            "/Applications/Example.app"
        )));
        assert!(!is_shared_library_or_application(Path::new(
            "/Applications-Backup"
        )));
        assert!(is_protected_cleanup_path(Path::new(
            "/private/var/vm/swapfile0"
        )));
        assert!(!is_protected_cleanup_path(Path::new("/private/var/vms")));
    }

    #[test]
    fn broad_duplicate_scans_skip_transient_user_data_but_explicit_scans_allow_it() {
        let transient_file =
            Path::new("/private/var/folders/example/X/code_sign_clone/Google Chrome Framework");

        assert!(is_transient_duplicate_scope(transient_file, Path::new("/")));
        assert!(is_transient_duplicate_scope(
            Path::new("/var/folders/example/X/archive"),
            Path::new("/Users/example")
        ));
        assert!(!is_transient_duplicate_scope(
            transient_file,
            Path::new("/private/var/folders/example")
        ));
        assert!(!is_transient_duplicate_scope(
            Path::new("/private/var/folders-backup/archive"),
            Path::new("/")
        ));
        assert!(is_transient_duplicate_scope(
            Path::new("/Users/example/Library/Application Support/browser/cache.bin"),
            Path::new("/Users/example")
        ));
        assert!(!is_transient_duplicate_scope(
            Path::new("/Users/example/Library/Caches/browser/cache.bin"),
            Path::new("/Users/example/Library")
        ));
        assert!(!is_transient_duplicate_scope(
            Path::new("/Users/example/Documents/Library/report.pdf"),
            Path::new("/Users/example")
        ));
    }

    #[test]
    fn duplicate_scans_always_skip_system_and_volume_metadata() {
        for path in [
            "/usr/lib/example.dylib",
            "/etc/hosts",
            "/private/var/db/example.db",
            "/var/db/example.db",
            "/Users/example/.Trash/.Spotlight-V100/index",
            "/Volumes/External/.fseventsd/0000000000000001",
            "/Users/example/Applications/Example.app/Contents/Resources/copy.dat",
            "/Users/example/Downloads/Example.APP/Contents/MacOS/example",
            "/Users/example/Projects/Engine.framework/Versions/A/Engine",
            "/Users/example/Projects/Extension.appex/Contents/MacOS/Extension",
        ] {
            assert!(is_protected_duplicate_scope(Path::new(path)), "{path}");
        }
        assert!(!is_protected_duplicate_scope(Path::new(
            "/Users/example/Documents/.Spotlight-V100-backup/report.pdf"
        )));
        assert!(!is_protected_duplicate_scope(Path::new(
            "/Users/example/Documents/Example.app-backup/report.pdf"
        )));
        assert!(!is_protected_duplicate_scope(Path::new(
            "/Users/example/Documents/Report.pages/Index.zip"
        )));
    }
}

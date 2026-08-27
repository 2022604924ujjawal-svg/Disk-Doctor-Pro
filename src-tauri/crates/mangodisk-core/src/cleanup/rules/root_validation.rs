use std::path::{Component, Path};

use mangodisk_platform::{current_platform, Platform};

use super::protected_paths::is_protected_home_relative_path;

/// Revalidates the resolved rule root immediately before automatic cleanup.
///
/// Manual large-file and duplicate-file deletion use their own explicit
/// candidate authorization and do not call this policy. Keeping the policies
/// separate prevents a global safety net from blocking intentional user file
/// management while making declarative cleanup fail closed.
pub(crate) fn validate_automatic_cleanup_root(root: &Path, home: &Path) -> Result<(), String> {
    let Some(relative) = relative_components(root, home)? else {
        return Ok(());
    };
    if relative.is_empty() {
        return Err("automatic cleanup cannot own the user home directory".to_string());
    }

    if is_protected_home_relative_path(&relative) {
        return Err("automatic cleanup cannot own protected user content".to_string());
    }
    Ok(())
}

fn relative_components(path: &Path, base: &Path) -> Result<Option<Vec<String>>, String> {
    // Canonicalization adds a verbatim prefix on Windows. Remove only that
    // platform-owned representation detail before comparing components so a
    // protected directory cannot escape the home boundary check.
    let path = current_platform().display_path(path);
    let base = current_platform().display_path(base);
    let path = normalized_components(Path::new(&path))?;
    let base = normalized_components(Path::new(&base))?;
    if path.len() < base.len()
        || !path
            .iter()
            .zip(&base)
            .all(|(left, right)| left.eq_ignore_ascii_case(right))
    {
        return Ok(None);
    }
    Ok(Some(path.into_iter().skip(base.len()).collect()))
}

fn normalized_components(path: &Path) -> Result<Vec<String>, String> {
    let mut output = Vec::new();
    for component in path.components() {
        match component {
            Component::Prefix(prefix) => output.push(
                prefix
                    .as_os_str()
                    .to_str()
                    .ok_or_else(|| "automatic cleanup requires Unicode path roots".to_string())?
                    .to_string(),
            ),
            Component::RootDir => output.push(String::new()),
            Component::Normal(value) => output.push(
                value
                    .to_str()
                    .ok_or_else(|| "automatic cleanup requires Unicode path roots".to_string())?
                    .to_string(),
            ),
            Component::CurDir | Component::ParentDir => {
                return Err("automatic cleanup requires normalized path roots".to_string());
            }
        }
    }
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn automatic_cleanup_rejects_identity_sync_source_and_repository_roots() {
        let home = Path::new("/Users/example");
        for protected in [
            "/Users/example/.ssh/cache",
            "/Users/example/.aws/cache",
            "/Users/example/.config/tool/cache",
            "/Users/example/.local/share/tool/cache",
            "/Users/example/OneDrive/cache",
            "/Users/example/OneDrive - Example Organization/cache",
            "/Users/example/projects/generated",
            "/Users/example/workspace/app/.git/objects",
            "/Users/example/Library/Mobile Documents/com~apple~CloudDocs/cache",
            "/Users/example/Library/Application Support/CloudDocs/session/db",
            "/Users/example/Library/Application Support/FileProvider/state.db",
            "/Users/example/Library/Caches/CloudKit/CloudKitMetadata.db",
            "/Users/example/Library/Caches/com.apple.bird/session",
            "/Users/example/Library/Caches/com.apple.cloudd/session",
            "/Users/example/Library/Caches/com.apple.CloudDocs/session",
            "/Users/example/Library/Caches/com.apple.FileProvider/session",
            "/Users/example/Library/Daemon Containers/provider/state",
        ] {
            assert!(
                validate_automatic_cleanup_root(Path::new(protected), home).is_err(),
                "{protected} must remain outside automatic cleanup"
            );
        }
    }

    #[test]
    fn automatic_cleanup_allows_explicit_cache_and_package_roots() {
        let home = Path::new("/Users/example");
        for allowed in [
            "/Users/example/Library/Caches/com.example.app",
            "/Users/example/.cache/compiler",
            "/Users/example/.cargo/git/db",
            "/Users/example/.local/share/NuGet/http-cache",
            "/Users/example/.local/share/NuGet/v3-cache",
            "/Users/example/Library/Application Support/App/Code Cache",
        ] {
            assert!(
                validate_automatic_cleanup_root(Path::new(allowed), home).is_ok(),
                "{allowed} must remain eligible for a verified rule"
            );
        }
    }

    #[test]
    fn automatic_cleanup_keeps_other_local_share_state_protected() {
        let home = Path::new("/Users/example");
        for protected in [
            "/Users/example/.local/share/NuGet/config",
            "/Users/example/.local/share/Example/cache",
            "/Users/example/.local/share/NuGet/http-cache/nested",
        ] {
            assert!(
                validate_automatic_cleanup_root(Path::new(protected), home).is_err(),
                "{protected} must remain outside automatic cleanup"
            );
        }
    }

    #[test]
    fn paths_outside_the_home_are_deferred_to_platform_validation() {
        assert!(validate_automatic_cleanup_root(
            Path::new("/private/var/tmp/example"),
            Path::new("/Users/example")
        )
        .is_ok());
    }

    #[test]
    fn non_normalized_paths_fail_closed() {
        assert!(validate_automatic_cleanup_root(
            Path::new("/Users/example/projects/../Library/Caches"),
            Path::new("/Users/example")
        )
        .is_err());
    }

    #[cfg(windows)]
    #[test]
    fn canonical_protected_path_remains_inside_display_home() {
        let home = std::env::temp_dir().join(format!(
            "mangodisk-protected-path-{}-{}",
            std::process::id(),
            crate::filesystem::metadata::now_ms()
        ));
        let protected = home.join(".ssh").join("cache");
        std::fs::create_dir_all(&protected).expect("the protected path fixture should be created");
        let canonical = std::fs::canonicalize(&protected)
            .expect("the protected path fixture should canonicalize");

        assert!(validate_automatic_cleanup_root(&canonical, &home).is_err());

        std::fs::remove_dir_all(home).expect("the protected path fixture should be removed");
    }
}

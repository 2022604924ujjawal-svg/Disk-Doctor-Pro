use std::{env, path::PathBuf};

use winreg::{enums::HKEY_LOCAL_MACHINE, RegKey};

use crate::ApplicationInstallScope;

use super::{directories, path_identity};

const MACHINE_ENVIRONMENT_KEY: &str =
    r"SYSTEM\CurrentControlSet\Control\Session Manager\Environment";

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct ScoopRoot {
    pub path: PathBuf,
    pub scope: ApplicationInstallScope,
}

pub(super) fn scoop_roots() -> Vec<ScoopRoot> {
    let mut roots = Vec::new();
    if let Some(root) = env::var_os("SCOOP")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
    {
        roots.push(ScoopRoot {
            path: root,
            scope: ApplicationInstallScope::CurrentUser,
        });
    } else if let Some(profile) = env::var_os("USERPROFILE") {
        let root = PathBuf::from(profile).join("scoop");
        if root.is_absolute() {
            roots.push(ScoopRoot {
                path: root,
                scope: ApplicationInstallScope::CurrentUser,
            });
        }
    }
    if let Some(root) = env::var_os("SCOOP_GLOBAL")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
    {
        roots.push(ScoopRoot {
            path: root,
            scope: ApplicationInstallScope::Machine,
        });
    } else if let Ok(program_data) = directories::program_data_directory() {
        roots.push(ScoopRoot {
            path: program_data.join("scoop"),
            scope: ApplicationInstallScope::Machine,
        });
    }
    normalize_scoop_roots(roots)
}

fn normalize_scoop_roots(mut roots: Vec<ScoopRoot>) -> Vec<ScoopRoot> {
    roots.sort_by(|left, right| {
        normalized_path(&left.path)
            .cmp(&normalized_path(&right.path))
            // Machine scope sorts first so an ambiguously shared root loses
            // current-user uninstall authority rather than gaining it.
            .then_with(|| right.scope.cmp(&left.scope))
    });
    roots.dedup_by(|right, left| normalized_path(&right.path) == normalized_path(&left.path));
    roots
}

fn normalized_path(path: &std::path::Path) -> String {
    path_identity::comparison_key(path)
}

/// Resolves Chocolatey's machine installation root without trusting mutable
/// process environment variables. The returned path may later authorize an
/// elevated executable, so only a machine-level registry value or the native
/// ProgramData Known Folder is accepted.
pub(super) fn chocolatey_root() -> Option<PathBuf> {
    machine_environment_value("ChocolateyInstall")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .or_else(|| {
            directories::program_data_directory()
                .ok()
                .map(|path| path.join("chocolatey"))
        })
}

fn machine_environment_value(name: &str) -> Option<String> {
    let environment = RegKey::predef(HKEY_LOCAL_MACHINE)
        .open_subkey(MACHINE_ENVIRONMENT_KEY)
        .ok()?;
    environment
        .get_value::<String, _>(name)
        .ok()
        .map(|value| value.trim().trim_matches('"').to_string())
        .filter(|value| !value.is_empty())
}

#[cfg(test)]
mod tests {
    use super::{chocolatey_root, normalize_scoop_roots, scoop_roots, ScoopRoot};
    use crate::ApplicationInstallScope;

    #[test]
    fn package_roots_are_absolute_and_deduplicated() {
        let scoop = scoop_roots();
        assert!(scoop.iter().all(|root| root.path.is_absolute()));
        for (index, root) in scoop.iter().enumerate() {
            assert!(!scoop[..index].iter().any(|candidate| candidate == root));
        }
        assert!(chocolatey_root().is_none_or(|path| path.is_absolute()));
    }

    #[test]
    fn ambiguous_scoop_root_keeps_the_safer_machine_scope() {
        let roots = normalize_scoop_roots(vec![
            ScoopRoot {
                path: r"C:\Tools\Scoop".into(),
                scope: ApplicationInstallScope::CurrentUser,
            },
            ScoopRoot {
                path: r"c:\tools\scoop\".into(),
                scope: ApplicationInstallScope::Machine,
            },
        ]);

        assert_eq!(roots.len(), 1);
        assert_eq!(roots[0].scope, ApplicationInstallScope::Machine);
    }
}

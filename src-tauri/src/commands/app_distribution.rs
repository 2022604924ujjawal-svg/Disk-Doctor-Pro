use serde::Serialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum AppDistribution {
    Installed,
    Portable,
}

impl AppDistribution {
    pub(crate) const fn diagnostic_name(self) -> &'static str {
        match self {
            Self::Installed => "installed",
            Self::Portable => "portable",
        }
    }
}

pub(crate) const fn current() -> AppDistribution {
    if cfg!(all(target_os = "windows", feature = "portable")) {
        AppDistribution::Portable
    } else {
        AppDistribution::Installed
    }
}

/// Returns the distribution selected when this binary was compiled. A build
/// marker is deterministic after users move the executable and avoids
/// mistaking stale registry entries or custom installation paths for a
/// portable launch.
#[tauri::command]
pub fn get_app_distribution() -> AppDistribution {
    current()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn distribution_uses_stable_transport_values() {
        assert_eq!(
            serde_json::to_value(AppDistribution::Installed).unwrap(),
            "installed"
        );
        assert_eq!(
            serde_json::to_value(AppDistribution::Portable).unwrap(),
            "portable"
        );
    }

    #[test]
    fn portable_feature_only_changes_windows_builds() {
        let expected = if cfg!(all(target_os = "windows", feature = "portable")) {
            AppDistribution::Portable
        } else {
            AppDistribution::Installed
        };
        assert_eq!(current(), expected);
    }
}

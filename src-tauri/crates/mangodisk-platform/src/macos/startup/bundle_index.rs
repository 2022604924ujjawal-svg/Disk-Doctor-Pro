use std::{fs, path::PathBuf};

use plist::Dictionary;

use crate::{
    PlatformStartupIdentityConfidence, PlatformStartupOwner, PlatformStartupSummarySource,
};

use super::embedded::{application_roots, bundle_name, read_bundle_metadata, string_value};

#[derive(Debug)]
struct InstalledApplication {
    path: PathBuf,
    bundle_identifier: String,
    name: String,
    version: Option<String>,
}

#[derive(Default)]
pub(super) struct BundleIndex {
    applications: Vec<InstalledApplication>,
}

impl BundleIndex {
    pub(super) fn discover() -> Self {
        let mut applications = Vec::new();
        for (root, system_owned) in application_roots() {
            if system_owned {
                continue;
            }
            let Ok(entries) = fs::read_dir(root) else {
                continue;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                if entry.file_type().is_ok_and(|kind| kind.is_symlink())
                    || !path
                        .extension()
                        .and_then(|extension| extension.to_str())
                        .is_some_and(|extension| extension.eq_ignore_ascii_case("app"))
                {
                    continue;
                }
                let Some(metadata) = read_bundle_metadata(&path) else {
                    continue;
                };
                let Some(application) = application_from_metadata(path, &metadata) else {
                    continue;
                };
                applications.push(application);
            }
        }
        log::debug!(
            "startup_bundle_index_ready application_count={}",
            applications.len()
        );
        Self { applications }
    }

    pub(super) fn resolve_owner(
        &self,
        label: Option<&str>,
        _target_team_id: Option<&str>,
    ) -> Option<PlatformStartupOwner> {
        let label = label?;
        // Discovery already creates a scan-scoped snapshot from live directory entries. Avoid
        // re-reading every bundle for every launchd job; the next scan will reflect later changes.
        let candidates: Vec<_> = self.applications.iter().collect();
        let (application, score) = unique_best_match(label, &candidates)?;
        let confidence = if score >= 100 {
            PlatformStartupIdentityConfidence::Exact
        } else {
            PlatformStartupIdentityConfidence::Strong
        };
        Some(owner(application, confidence))
    }
}

fn application_from_metadata(path: PathBuf, metadata: &Dictionary) -> Option<InstalledApplication> {
    let bundle_identifier = string_value(metadata, "CFBundleIdentifier")?;
    let name = bundle_name(metadata).or_else(|| {
        path.file_stem()
            .map(|value| value.to_string_lossy().into_owned())
    })?;
    Some(InstalledApplication {
        path,
        bundle_identifier,
        name,
        version: string_value(metadata, "CFBundleShortVersionString"),
    })
}

fn unique_best_match<'a>(
    label: &str,
    candidates: &[&'a InstalledApplication],
) -> Option<(&'a InstalledApplication, u16)> {
    let mut scored: Vec<_> = candidates
        .iter()
        .filter_map(|application| {
            let score = match_score(label, application);
            (score >= 60).then_some((*application, score))
        })
        .collect();
    scored.sort_by_key(|(_, score)| std::cmp::Reverse(*score));
    let best = *scored.first()?;
    if scored.get(1).is_some_and(|(_, score)| *score == best.1) {
        return None;
    }
    Some(best)
}

fn match_score(label: &str, application: &InstalledApplication) -> u16 {
    let normalized_label = normalize(label);
    let normalized_bundle = normalize(&application.bundle_identifier);
    if normalized_bundle.len() >= 8 && normalized_label.contains(&normalized_bundle) {
        return 100;
    }
    application
        .bundle_identifier
        .split(|character: char| !character.is_ascii_alphanumeric())
        .map(normalize)
        .filter(|token| token.len() >= 5 && normalized_label.contains(token))
        .map(|_| 70)
        .max()
        .unwrap_or(0)
}

fn normalize(value: &str) -> String {
    value
        .chars()
        .filter(|character| character.is_ascii_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

fn owner(
    application: &InstalledApplication,
    confidence: PlatformStartupIdentityConfidence,
) -> PlatformStartupOwner {
    PlatformStartupOwner {
        identity_key: Some(format!("bundle:{}", application.bundle_identifier)),
        name: Some(application.name.clone()),
        publisher: None,
        summary: None,
        summary_source: PlatformStartupSummarySource::BundleMetadata,
        version: application.version.clone(),
        icon_path: Some(application.path.clone()),
        confidence,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn application(bundle_identifier: &str, name: &str) -> InstalledApplication {
        InstalledApplication {
            path: PathBuf::from("/Applications/Fixture.app"),
            bundle_identifier: bundle_identifier.to_owned(),
            name: name.to_owned(),
            version: None,
        }
    }

    #[test]
    fn exact_bundle_identity_has_the_highest_score() {
        let application = application("com.macpaw.CleanMyMac5", "CleanMyMac");

        assert_eq!(
            match_score("com.macpaw.CleanMyMac5.Updater", &application),
            100
        );
    }

    #[test]
    fn distinctive_product_tokens_match_helper_labels() {
        let application = application("org.wireshark.Wireshark", "Wireshark");

        assert_eq!(match_score("org.wireshark.ChmodBPF", &application), 70);
    }

    #[test]
    fn short_generic_tokens_do_not_create_owner_matches() {
        let application = application("com.example.Open", "Open");

        assert_eq!(match_score("homebrew.mxcl.openresty", &application), 0);
    }

    #[test]
    fn display_name_words_do_not_associate_unrelated_applications() {
        let application = application("com.tencent.cleanwechat", "Clean My WeChat");

        assert_eq!(
            match_score("com.macpaw.CleanMyMac5.Updater", &application),
            0
        );
    }
}

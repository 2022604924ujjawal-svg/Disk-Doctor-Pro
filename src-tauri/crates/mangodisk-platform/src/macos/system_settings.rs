use std::{
    path::Path,
    process::{Command, Output, Stdio},
};

use crate::{
    preflight_system_setting_change, PlatformCancellation, PlatformError, PlatformErrorCode,
    PlatformResult, PlatformSystemSettingChangeRequest, PlatformSystemSettingChangeResult,
    PlatformSystemSettingDiagnosticCode, PlatformSystemSettingState, PlatformSystemSettingValue,
};

#[derive(Clone, Copy)]
enum ValueKind {
    Boolean,
    Integer,
    Text,
}

#[derive(Clone, Copy)]
struct SettingDefinition {
    id: &'static str,
    domain: &'static str,
    key: &'static str,
    kind: ValueKind,
}

const SETTINGS: &[SettingDefinition] = &[
    setting(
        "macos.finder.show-file-extensions",
        "NSGlobalDomain",
        "AppleShowAllExtensions",
        ValueKind::Boolean,
    ),
    setting(
        "macos.finder.show-path-bar",
        "com.apple.finder",
        "ShowPathbar",
        ValueKind::Boolean,
    ),
    setting(
        "macos.finder.show-status-bar",
        "com.apple.finder",
        "ShowStatusBar",
        ValueKind::Boolean,
    ),
    setting(
        "macos.finder.show-hidden-files",
        "NSGlobalDomain",
        "AppleShowAllFiles",
        ValueKind::Boolean,
    ),
    setting(
        "macos.finder.show-posix-path",
        "com.apple.finder",
        "_FXShowPosixPathInTitle",
        ValueKind::Boolean,
    ),
    setting(
        "macos.finder.disable-animations",
        "com.apple.finder",
        "DisableAllAnimations",
        ValueKind::Boolean,
    ),
    setting(
        "macos.finder.keep-folders-on-top",
        "com.apple.finder",
        "_FXSortFoldersFirst",
        ValueKind::Boolean,
    ),
    setting(
        "macos.finder.search-current-folder",
        "com.apple.finder",
        "FXDefaultSearchScope",
        ValueKind::Text,
    ),
    setting(
        "macos.finder.disable-extension-warning",
        "com.apple.finder",
        "FXEnableExtensionChangeWarning",
        ValueKind::Boolean,
    ),
    setting(
        "macos.finder.show-hard-drives-on-desktop",
        "com.apple.finder",
        "ShowHardDrivesOnDesktop",
        ValueKind::Boolean,
    ),
    setting(
        "macos.finder.show-external-drives-on-desktop",
        "com.apple.finder",
        "ShowExternalHardDrivesOnDesktop",
        ValueKind::Boolean,
    ),
    setting(
        "macos.finder.show-removable-media-on-desktop",
        "com.apple.finder",
        "ShowRemovableMediaOnDesktop",
        ValueKind::Boolean,
    ),
    setting(
        "macos.finder.default-list-view",
        "com.apple.finder",
        "FXPreferredViewStyle",
        ValueKind::Text,
    ),
    setting(
        "macos.finder.folders-first-on-desktop",
        "com.apple.finder",
        "_FXSortFoldersFirstOnDesktop",
        ValueKind::Boolean,
    ),
    setting(
        "macos.finder.enable-quit-menu",
        "com.apple.finder",
        "QuitMenuItem",
        ValueKind::Boolean,
    ),
    setting(
        "macos.finder.remove-old-trash-items",
        "com.apple.finder",
        "FXRemoveOldTrashItems",
        ValueKind::Boolean,
    ),
    setting(
        "macos.panels.expand-save",
        "NSGlobalDomain",
        "NSNavPanelExpandedStateForSaveMode",
        ValueKind::Boolean,
    ),
    setting(
        "macos.panels.expand-print",
        "NSGlobalDomain",
        "PMPrintingExpandedStateForPrint",
        ValueKind::Boolean,
    ),
    setting(
        "macos.desktop.prevent-network-ds-store",
        "com.apple.desktopservices",
        "DSDontWriteNetworkStores",
        ValueKind::Boolean,
    ),
    setting(
        "macos.desktop.prevent-usb-ds-store",
        "com.apple.desktopservices",
        "DSDontWriteUSBStores",
        ValueKind::Boolean,
    ),
    setting(
        "macos.dock.auto-hide",
        "com.apple.dock",
        "autohide",
        ValueKind::Boolean,
    ),
    setting(
        "macos.dock.minimize-to-application",
        "com.apple.dock",
        "minimize-to-application",
        ValueKind::Boolean,
    ),
    setting(
        "macos.dock.use-scale-effect",
        "com.apple.dock",
        "mineffect",
        ValueKind::Text,
    ),
    setting(
        "macos.mission-control.keep-space-order",
        "com.apple.dock",
        "mru-spaces",
        ValueKind::Boolean,
    ),
    setting(
        "macos.dock.hide-recent-apps",
        "com.apple.dock",
        "show-recents",
        ValueKind::Boolean,
    ),
    setting(
        "macos.dock.disable-launch-animation",
        "com.apple.dock",
        "launchanim",
        ValueKind::Boolean,
    ),
    setting(
        "macos.dock.show-only-open-apps",
        "com.apple.dock",
        "static-only",
        ValueKind::Boolean,
    ),
    setting(
        "macos.dock.dim-hidden-apps",
        "com.apple.dock",
        "showhidden",
        ValueKind::Boolean,
    ),
    setting(
        "macos.dock.remove-auto-hide-delay",
        "com.apple.dock",
        "autohide-delay",
        ValueKind::Integer,
    ),
    setting(
        "macos.dock.enable-magnification",
        "com.apple.dock",
        "magnification",
        ValueKind::Boolean,
    ),
    setting(
        "macos.mission-control.group-windows-by-app",
        "com.apple.dock",
        "expose-group-apps",
        ValueKind::Boolean,
    ),
    setting(
        "macos.window.disable-animations",
        "NSGlobalDomain",
        "NSAutomaticWindowAnimationsEnabled",
        ValueKind::Boolean,
    ),
    setting(
        "macos.window.close-app-windows",
        "NSGlobalDomain",
        "NSQuitAlwaysKeepsWindows",
        ValueKind::Boolean,
    ),
    setting(
        "macos.window.double-click-titlebar-minimize",
        "NSGlobalDomain",
        "AppleActionOnDoubleClick",
        ValueKind::Text,
    ),
    setting(
        "macos.screenshots.disable-shadow",
        "com.apple.screencapture",
        "disable-shadow",
        ValueKind::Boolean,
    ),
    setting(
        "macos.screenshots.use-png",
        "com.apple.screencapture",
        "type",
        ValueKind::Text,
    ),
    setting(
        "macos.screenshots.disable-thumbnail",
        "com.apple.screencapture",
        "show-thumbnail",
        ValueKind::Boolean,
    ),
    setting(
        "macos.keyboard.full-navigation",
        "NSGlobalDomain",
        "AppleKeyboardUIMode",
        ValueKind::Integer,
    ),
    setting(
        "macos.keyboard.fast-key-repeat",
        "NSGlobalDomain",
        "KeyRepeat",
        ValueKind::Integer,
    ),
    setting(
        "macos.keyboard.short-repeat-delay",
        "NSGlobalDomain",
        "InitialKeyRepeat",
        ValueKind::Integer,
    ),
    setting(
        "macos.keyboard.disable-press-and-hold",
        "NSGlobalDomain",
        "ApplePressAndHoldEnabled",
        ValueKind::Boolean,
    ),
    setting(
        "macos.keyboard.use-standard-function-keys",
        "NSGlobalDomain",
        "com.apple.keyboard.fnState",
        ValueKind::Boolean,
    ),
    setting(
        "macos.documents.save-locally",
        "NSGlobalDomain",
        "NSDocumentSaveNewDocumentsToCloud",
        ValueKind::Boolean,
    ),
    setting(
        "macos.printing.quit-after-finish",
        "com.apple.print.PrintingPrefs",
        "Quit When Finished",
        ValueKind::Boolean,
    ),
    setting(
        "macos.safari.show-full-url",
        "com.apple.Safari",
        "ShowFullURLInSmartSearchField",
        ValueKind::Boolean,
    ),
    setting(
        "macos.safari.disable-safe-download-auto-open",
        "com.apple.Safari",
        "AutoOpenSafeDownloads",
        ValueKind::Boolean,
    ),
    setting(
        "macos.safari.show-status-bar",
        "com.apple.Safari",
        "ShowOverlayStatusBar",
        ValueKind::Boolean,
    ),
    setting(
        "macos.safari.enable-develop-menu",
        "com.apple.Safari",
        "IncludeDevelopMenu",
        ValueKind::Boolean,
    ),
    setting(
        "macos.textedit.plain-text-default",
        "com.apple.TextEdit",
        "RichText",
        ValueKind::Boolean,
    ),
    setting(
        "macos.photos.disable-auto-open",
        "com.apple.ImageCapture",
        "disableHotPlug",
        ValueKind::Boolean,
    ),
    setting(
        "macos.privacy.disable-personalized-ads",
        "com.apple.AdLib",
        "allowApplePersonalizedAdvertising",
        ValueKind::Boolean,
    ),
    setting(
        "macos.sharing.disable-airdrop",
        "com.apple.NetworkBrowser",
        "DisableAirDrop",
        ValueKind::Boolean,
    ),
    setting(
        "macos.activity-monitor.show-all-processes",
        "com.apple.ActivityMonitor",
        "ShowCategory",
        ValueKind::Integer,
    ),
    setting(
        "macos.app-store.enable-auto-updates",
        "com.apple.commerce",
        "AutoUpdate",
        ValueKind::Boolean,
    ),
    setting(
        "macos.text.disable-auto-correct",
        "NSGlobalDomain",
        "NSAutomaticSpellingCorrectionEnabled",
        ValueKind::Boolean,
    ),
    setting(
        "macos.text.disable-smart-quotes",
        "NSGlobalDomain",
        "NSAutomaticQuoteSubstitutionEnabled",
        ValueKind::Boolean,
    ),
    setting(
        "macos.text.disable-smart-dashes",
        "NSGlobalDomain",
        "NSAutomaticDashSubstitutionEnabled",
        ValueKind::Boolean,
    ),
    setting(
        "macos.text.disable-auto-capitalization",
        "NSGlobalDomain",
        "NSAutomaticCapitalizationEnabled",
        ValueKind::Boolean,
    ),
    setting(
        "macos.text.disable-period-substitution",
        "NSGlobalDomain",
        "NSAutomaticPeriodSubstitutionEnabled",
        ValueKind::Boolean,
    ),
    setting(
        "macos.finder.warn-before-empty-trash",
        "com.apple.finder",
        "WarnOnEmptyTrash",
        ValueKind::Boolean,
    ),
    setting(
        "macos.dock.enable-spring-loading",
        "com.apple.dock",
        "enable-spring-load-actions-on-all-items",
        ValueKind::Boolean,
    ),
    setting(
        "macos.dock.scroll-to-expose",
        "com.apple.dock",
        "scroll-to-open",
        ValueKind::Boolean,
    ),
    setting(
        "macos.dock.fast-auto-hide-animation",
        "com.apple.dock",
        "autohide-time-modifier",
        ValueKind::Integer,
    ),
    setting(
        "macos.safari.disable-search-suggestions",
        "com.apple.Safari",
        "SuppressSearchSuggestions",
        ValueKind::Boolean,
    ),
    setting(
        "macos.safari.disable-top-hit-preload",
        "com.apple.Safari",
        "PreloadTopHit",
        ValueKind::Boolean,
    ),
    setting(
        "macos.text.disable-text-completion",
        "NSGlobalDomain",
        "NSAutomaticTextCompletionEnabled",
        ValueKind::Boolean,
    ),
    setting(
        "macos.text.disable-inline-predictions",
        "NSGlobalDomain",
        "NSAutomaticInlinePredictionEnabled",
        ValueKind::Boolean,
    ),
    setting(
        "macos.sound.disable-volume-feedback",
        "NSGlobalDomain",
        "com.apple.sound.beep.feedback",
        ValueKind::Boolean,
    ),
    setting(
        "macos.time-machine.hide-new-disk-prompts",
        "com.apple.TimeMachine",
        "DoNotOfferNewDisksForBackup",
        ValueKind::Boolean,
    ),
    setting(
        "macos.security.require-password-after-sleep",
        "com.apple.screensaver",
        "askForPassword",
        ValueKind::Boolean,
    ),
    setting(
        "macos.security.lock-immediately-after-sleep",
        "com.apple.screensaver",
        "askForPasswordDelay",
        ValueKind::Integer,
    ),
];

const fn setting(
    id: &'static str,
    domain: &'static str,
    key: &'static str,
    kind: ValueKind,
) -> SettingDefinition {
    SettingDefinition {
        id,
        domain,
        key,
        kind,
    }
}

pub(crate) fn scan(
    setting_ids: &[&str],
    cancellation: &PlatformCancellation,
) -> PlatformResult<Vec<PlatformSystemSettingState>> {
    let mut states = Vec::with_capacity(setting_ids.len());
    for setting_id in setting_ids {
        if cancellation.is_cancelled() {
            // Core owns the public cancellation protocol. Returning the partial read allows it to
            // convert the shared cancellation flag into the stable operation-cancelled error.
            break;
        }
        let Some(definition) = definition(setting_id) else {
            states.push(unsupported_state(setting_id));
            continue;
        };
        states.push(read_state(definition));
    }
    Ok(states)
}

pub(crate) fn change(
    request: &PlatformSystemSettingChangeRequest,
) -> PlatformResult<PlatformSystemSettingChangeResult> {
    let definition = definition(&request.setting_id).ok_or_else(|| {
        PlatformError::new(
            PlatformErrorCode::Unsupported,
            "system setting identifier is unsupported",
        )
    })?;
    validate_value(definition.kind, &request.desired_value)?;
    let before = read_value(definition)?;
    if let Some(result) = preflight_system_setting_change(&before, request)? {
        return Ok(result);
    }

    write_value(definition, &request.desired_value)
        .map_err(PlatformError::with_possible_side_effects)?;
    let after = read_value(definition).map_err(PlatformError::with_possible_side_effects)?;
    Ok(PlatformSystemSettingChangeResult {
        changed: after != before,
        verified: after == request.desired_value,
        value: after,
    })
}

fn definition(setting_id: &str) -> Option<SettingDefinition> {
    SETTINGS.iter().copied().find(|item| item.id == setting_id)
}

fn unsupported_state(setting_id: &str) -> PlatformSystemSettingState {
    PlatformSystemSettingState {
        setting_id: setting_id.to_string(),
        value: PlatformSystemSettingValue::Missing,
        effective_value: PlatformSystemSettingValue::Missing,
        requires_elevation: false,
        diagnostic: Some(PlatformSystemSettingDiagnosticCode::Unsupported),
    }
}

fn read_state(definition: SettingDefinition) -> PlatformSystemSettingState {
    match read_value(definition) {
        Ok(value) => PlatformSystemSettingState {
            setting_id: definition.id.to_string(),
            effective_value: value.clone(),
            value,
            requires_elevation: false,
            diagnostic: None,
        },
        Err(error) => {
            log::warn!(
                "macos_system_setting_read_failed setting_id={} code={:?}",
                definition.id,
                error.code()
            );
            PlatformSystemSettingState {
                setting_id: definition.id.to_string(),
                value: PlatformSystemSettingValue::Missing,
                effective_value: PlatformSystemSettingValue::Missing,
                requires_elevation: false,
                diagnostic: Some(diagnostic_for_error(error.code())),
            }
        }
    }
}

fn diagnostic_for_error(code: PlatformErrorCode) -> PlatformSystemSettingDiagnosticCode {
    match code {
        PlatformErrorCode::AccessDenied => PlatformSystemSettingDiagnosticCode::AccessDenied,
        PlatformErrorCode::Unsupported => PlatformSystemSettingDiagnosticCode::Unsupported,
        _ => PlatformSystemSettingDiagnosticCode::StateUnavailable,
    }
}

fn read_value(definition: SettingDefinition) -> PlatformResult<PlatformSystemSettingValue> {
    let output = run_defaults(&["read", definition.domain, definition.key])?;
    if !output.status.success() {
        // A missing key delegates to the operating-system default and must be
        // preserved as a distinct value so an optimization can be reverted.
        return Ok(PlatformSystemSettingValue::Missing);
    }
    let text = String::from_utf8(output.stdout)
        .map_err(|_| PlatformError::operation_failed("system setting returned invalid UTF-8"))?;
    let text = text.trim();
    match definition.kind {
        ValueKind::Boolean => match text.to_ascii_lowercase().as_str() {
            "1" | "true" | "yes" => Ok(PlatformSystemSettingValue::Boolean(true)),
            "0" | "false" | "no" => Ok(PlatformSystemSettingValue::Boolean(false)),
            _ => Err(PlatformError::operation_failed(
                "system setting returned an invalid boolean",
            )),
        },
        ValueKind::Integer => text
            .parse::<i64>()
            .map(PlatformSystemSettingValue::Integer)
            .map_err(|_| {
                PlatformError::operation_failed("system setting returned an invalid integer")
            }),
        ValueKind::Text => Ok(PlatformSystemSettingValue::Text(text.to_string())),
    }
}

fn write_value(
    definition: SettingDefinition,
    value: &PlatformSystemSettingValue,
) -> PlatformResult<()> {
    let output = match value {
        PlatformSystemSettingValue::Missing => {
            run_defaults(&["delete", definition.domain, definition.key])?
        }
        PlatformSystemSettingValue::Boolean(value) => run_defaults(&[
            "write",
            definition.domain,
            definition.key,
            "-bool",
            if *value { "true" } else { "false" },
        ])?,
        PlatformSystemSettingValue::Integer(value) => {
            let value = value.to_string();
            run_defaults(&[
                "write",
                definition.domain,
                definition.key,
                "-int",
                value.as_str(),
            ])?
        }
        PlatformSystemSettingValue::Text(value) => {
            run_defaults(&["write", definition.domain, definition.key, "-string", value])?
        }
        PlatformSystemSettingValue::Snapshot(_) => {
            return Err(PlatformError::new(
                PlatformErrorCode::InvalidData,
                "native snapshots are unsupported for macOS settings",
            ))
        }
    };
    let deleted = matches!(value, PlatformSystemSettingValue::Missing)
        && read_value(definition)? == PlatformSystemSettingValue::Missing;
    if output.status.success() || deleted {
        Ok(())
    } else {
        Err(PlatformError::operation_failed(
            "failed to write the system setting",
        ))
    }
}

fn validate_value(kind: ValueKind, value: &PlatformSystemSettingValue) -> PlatformResult<()> {
    let valid = matches!(value, PlatformSystemSettingValue::Missing)
        || matches!(
            (kind, value),
            (ValueKind::Boolean, PlatformSystemSettingValue::Boolean(_))
                | (ValueKind::Integer, PlatformSystemSettingValue::Integer(_))
                | (ValueKind::Text, PlatformSystemSettingValue::Text(_))
        );
    if valid {
        Ok(())
    } else {
        Err(PlatformError::operation_failed(
            "system setting value has an invalid type",
        ))
    }
}

fn run_defaults(args: &[&str]) -> PlatformResult<Output> {
    Command::new(Path::new("/usr/bin/defaults"))
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .output()
        .map_err(|error| PlatformError::io("run defaults", &error))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn setting_identifiers_are_unique_and_namespaced() {
        let mut ids = SETTINGS.iter().map(|item| item.id).collect::<Vec<_>>();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), SETTINGS.len());
        assert!(ids.iter().all(|id| id.starts_with("macos.")));
    }

    #[test]
    fn expanded_catalog_uses_expected_preference_types() {
        let cases = [
            (
                "macos.finder.default-list-view",
                "com.apple.finder",
                "FXPreferredViewStyle",
                ValueKind::Text,
            ),
            (
                "macos.dock.remove-auto-hide-delay",
                "com.apple.dock",
                "autohide-delay",
                ValueKind::Integer,
            ),
            (
                "macos.privacy.disable-personalized-ads",
                "com.apple.AdLib",
                "allowApplePersonalizedAdvertising",
                ValueKind::Boolean,
            ),
            (
                "macos.finder.warn-before-empty-trash",
                "com.apple.finder",
                "WarnOnEmptyTrash",
                ValueKind::Boolean,
            ),
            (
                "macos.dock.fast-auto-hide-animation",
                "com.apple.dock",
                "autohide-time-modifier",
                ValueKind::Integer,
            ),
            (
                "macos.security.lock-immediately-after-sleep",
                "com.apple.screensaver",
                "askForPasswordDelay",
                ValueKind::Integer,
            ),
        ];

        for (id, domain, key, kind) in cases {
            let definition = definition(id).expect("the expanded setting should exist");
            assert_eq!(definition.domain, domain);
            assert_eq!(definition.key, key);
            assert!(matches!(
                (definition.kind, kind),
                (ValueKind::Boolean, ValueKind::Boolean)
                    | (ValueKind::Integer, ValueKind::Integer)
                    | (ValueKind::Text, ValueKind::Text)
            ));
        }
    }

    #[test]
    #[ignore = "reads the current macOS preferences database"]
    fn actual_catalog_reads_every_known_setting() {
        let ids = SETTINGS.iter().map(|item| item.id).collect::<Vec<_>>();
        let states = scan(&ids, &PlatformCancellation::new(|| false))
            .expect("the macOS preferences catalog should be readable");

        assert_eq!(states.len(), SETTINGS.len());
        assert!(states.iter().all(|state| state.diagnostic.is_none()));
    }

    #[test]
    #[ignore = "temporarily changes and restores the current Activity Monitor preference"]
    fn actual_setting_round_trip_restores_original_value() {
        let definition = definition("macos.activity-monitor.show-all-processes")
            .expect("the Activity Monitor setting should exist");
        let before = read_value(definition).expect("the original preference should be readable");
        let temporary = if before == PlatformSystemSettingValue::Integer(100) {
            PlatformSystemSettingValue::Integer(102)
        } else {
            PlatformSystemSettingValue::Integer(100)
        };

        let round_trip = (|| -> PlatformResult<()> {
            write_value(definition, &temporary)?;
            let after = read_value(definition)?;
            if after != temporary {
                return Err(PlatformError::operation_failed(
                    "temporary system setting value was not persisted",
                ));
            }
            Ok(())
        })();
        let restore = write_value(definition, &before);

        restore.expect("the original preference should always be restored");
        round_trip.expect("the expanded setting should support a verified round trip");
        assert_eq!(
            read_value(definition).expect("the restored preference should be readable"),
            before
        );
    }

    #[test]
    #[ignore = "temporarily changes and restores newly added macOS preferences"]
    fn actual_expanded_settings_round_trip_and_restore() {
        let cases = [
            (
                "macos.finder.warn-before-empty-trash",
                PlatformSystemSettingValue::Boolean(false),
            ),
            (
                "macos.dock.fast-auto-hide-animation",
                PlatformSystemSettingValue::Integer(1),
            ),
            (
                "macos.sound.disable-volume-feedback",
                PlatformSystemSettingValue::Boolean(false),
            ),
        ];

        for (setting_id, candidate) in cases {
            let definition = definition(setting_id).expect("the expanded setting should exist");
            let before =
                read_value(definition).expect("the original preference should be readable");
            let temporary = if before == candidate {
                match candidate {
                    PlatformSystemSettingValue::Boolean(value) => {
                        PlatformSystemSettingValue::Boolean(!value)
                    }
                    PlatformSystemSettingValue::Integer(value) => {
                        PlatformSystemSettingValue::Integer(value + 1)
                    }
                    _ => panic!("the test case must use a scalar preference value"),
                }
            } else {
                candidate
            };

            let round_trip = (|| -> PlatformResult<()> {
                write_value(definition, &temporary)?;
                if read_value(definition)? != temporary {
                    return Err(PlatformError::operation_failed(
                        "temporary expanded preference value was not persisted",
                    ));
                }
                Ok(())
            })();
            let restore = write_value(definition, &before);

            restore.expect("the original expanded preference should always be restored");
            round_trip.expect("the expanded preference should support a verified round trip");
            assert_eq!(
                read_value(definition).expect("the restored preference should be readable"),
                before
            );
        }
    }
}

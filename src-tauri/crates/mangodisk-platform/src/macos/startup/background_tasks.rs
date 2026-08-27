use std::{
    collections::BTreeSet,
    fs,
    io::Cursor,
    path::{Path, PathBuf},
    time::{Instant, SystemTime, UNIX_EPOCH},
};

use plist::{Dictionary, Value};

use crate::{
    PlatformCancellation, PlatformError, PlatformResult, PlatformStartupArtifact,
    PlatformStartupChangeRequest, PlatformStartupChangeResult, PlatformStartupConfiguredState,
    PlatformStartupControlCapability, PlatformStartupCoverageReason, PlatformStartupCoverageStatus,
    PlatformStartupDesiredState, PlatformStartupDiagnosticCode, PlatformStartupIdentityConfidence,
    PlatformStartupOwner, PlatformStartupRuntimeState, PlatformStartupScope,
    PlatformStartupSourceKind, PlatformStartupSourceResult, PlatformStartupSummarySource,
    PlatformStartupTarget, PlatformStartupTargetKind, PlatformStartupTrigger,
    PlatformStartupTrustState,
};

use super::{
    embedded::{bundle_name, read_bundle_metadata, string_value},
    login_items,
};

const SOURCE_ID: &str = "macos.background_tasks";
const DATABASE_DIRECTORY: &str = "/var/db/com.apple.backgroundtaskmanagement";
const SUPPORTED_ARCHIVE_VERSIONS: &[u64] = &[13];
const MAX_DATABASE_BYTES: u64 = 8 * 1024 * 1024;
const APP_RECORD_TYPE: u64 = 2;
const DISPOSITION_ENABLED: u64 = 1;
const COCOA_REFERENCE_DATE_OFFSET_SECONDS: u64 = 978_307_200;

pub(super) fn scan(cancellation: &PlatformCancellation) -> PlatformStartupSourceResult {
    let started = Instant::now();
    if cancellation.is_cancelled() {
        return result(
            Vec::new(),
            PlatformStartupCoverageStatus::Cancelled,
            Some(PlatformStartupCoverageReason::Cancelled),
            started,
        );
    }
    let (records, modified_at_ms) = match read_database_records(cancellation) {
        Ok(value) => value,
        Err(ParseError::Cancelled) => {
            return result(
                Vec::new(),
                PlatformStartupCoverageStatus::Cancelled,
                Some(PlatformStartupCoverageReason::Cancelled),
                started,
            );
        }
        Err(ParseError::AccessDenied) => {
            return unavailable(started, PlatformStartupCoverageReason::AccessDenied);
        }
        Err(ParseError::Unsupported) => {
            return unavailable(
                started,
                PlatformStartupCoverageReason::UnsupportedOperatingSystem,
            );
        }
        Err(ParseError::InvalidData) => {
            return unavailable(started, PlatformStartupCoverageReason::InvalidData);
        }
    };
    let enabled_paths = login_items::enabled_paths().ok();
    let items = records
        .into_iter()
        .map(|record| artifact_from_record(record, modified_at_ms, enabled_paths.as_ref()))
        .collect();
    result(
        items,
        PlatformStartupCoverageStatus::Complete,
        None,
        started,
    )
}

#[derive(Debug, PartialEq, Eq)]
enum ParseError {
    AccessDenied,
    Cancelled,
    InvalidData,
    Unsupported,
}

#[derive(Debug, PartialEq)]
struct BackgroundAppRecord {
    identifier: String,
    bundle_identifier: String,
    name: String,
    developer_name: Option<String>,
    path: PathBuf,
    disposition: u64,
    modified_at: Option<f64>,
}

fn database_path() -> Option<(u64, PathBuf)> {
    SUPPORTED_ARCHIVE_VERSIONS.first().map(|version| {
        (
            *version,
            Path::new(DATABASE_DIRECTORY).join(format!("BackgroundItems-v{version}.btm")),
        )
    })
}

fn read_database_records(
    cancellation: &PlatformCancellation,
) -> Result<(Vec<BackgroundAppRecord>, Option<u64>), ParseError> {
    let (version, path) = database_path().ok_or(ParseError::Unsupported)?;
    let metadata = fs::metadata(&path).map_err(io_parse_error)?;
    if !metadata.is_file() || metadata.len() > MAX_DATABASE_BYTES {
        return Err(ParseError::InvalidData);
    }
    let bytes = fs::read(&path).map_err(io_parse_error)?;
    let archive = Value::from_reader(Cursor::new(bytes)).map_err(|_| ParseError::InvalidData)?;
    let records = parse_archive(&archive, version, cancellation)?;
    Ok((records, metadata.modified().ok().and_then(system_time_ms)))
}

fn io_parse_error(error: std::io::Error) -> ParseError {
    if error.kind() == std::io::ErrorKind::PermissionDenied {
        ParseError::AccessDenied
    } else {
        ParseError::InvalidData
    }
}

fn parse_archive(
    archive: &Value,
    expected_version: u64,
    cancellation: &PlatformCancellation,
) -> Result<Vec<BackgroundAppRecord>, ParseError> {
    let root = archive.as_dictionary().ok_or(ParseError::InvalidData)?;
    if root.get("$archiver").and_then(Value::as_string) != Some("NSKeyedArchiver") {
        return Err(ParseError::InvalidData);
    }
    let top = root
        .get("$top")
        .and_then(Value::as_dictionary)
        .ok_or(ParseError::InvalidData)?;
    if top.get("version").and_then(Value::as_unsigned_integer) != Some(expected_version) {
        return Err(ParseError::Unsupported);
    }
    let objects = root
        .get("$objects")
        .and_then(Value::as_array)
        .ok_or(ParseError::InvalidData)?;
    let item_record_classes: BTreeSet<u64> = objects
        .iter()
        .enumerate()
        .filter_map(|(index, value)| {
            (value.as_dictionary()?.get("$classname")?.as_string()? == "ItemRecord")
                .then_some(index as u64)
        })
        .collect();
    if item_record_classes.is_empty() {
        return Err(ParseError::InvalidData);
    }

    let mut seen = BTreeSet::new();
    let mut records = Vec::new();
    for value in objects {
        if cancellation.is_cancelled() {
            return Err(ParseError::Cancelled);
        }
        let Some(dictionary) = value.as_dictionary() else {
            continue;
        };
        let Some(class) = dictionary.get("$class").and_then(Value::as_uid) else {
            continue;
        };
        if !item_record_classes.contains(&class.get())
            || dictionary.get("type").and_then(Value::as_unsigned_integer) != Some(APP_RECORD_TYPE)
        {
            continue;
        }
        let Some(record) = app_record(objects, dictionary) else {
            continue;
        };
        if seen.insert(record.identifier.clone()) {
            records.push(record);
        }
    }
    Ok(records)
}

fn app_record(objects: &[Value], dictionary: &Dictionary) -> Option<BackgroundAppRecord> {
    let identifier = referenced_string(objects, dictionary, "identifier")?;
    let bundle_identifier = referenced_string(objects, dictionary, "bundleIdentifier")?;
    let archived_name = referenced_string(objects, dictionary, "name")?;
    let path = referenced_url(objects, dictionary, "url")?;
    if !path
        .extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| extension.eq_ignore_ascii_case("app"))
    {
        return None;
    }
    Some(BackgroundAppRecord {
        identifier,
        bundle_identifier,
        name: archived_name,
        developer_name: referenced_string(objects, dictionary, "developerName"),
        path,
        disposition: dictionary
            .get("disposition")
            .and_then(Value::as_unsigned_integer)?,
        modified_at: dictionary.get("modificationDate").and_then(number_value),
    })
}

fn referenced_string(objects: &[Value], dictionary: &Dictionary, key: &str) -> Option<String> {
    referenced_value(objects, dictionary, key)?
        .as_string()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
}

fn referenced_url(objects: &[Value], dictionary: &Dictionary, key: &str) -> Option<PathBuf> {
    let value = referenced_value(objects, dictionary, key)?.as_dictionary()?;
    let relative = referenced_value(objects, value, "NS.relative")?.as_string()?;
    file_url_path(relative)
}

fn referenced_value<'a>(
    objects: &'a [Value],
    dictionary: &Dictionary,
    key: &str,
) -> Option<&'a Value> {
    let index = dictionary.get(key)?.as_uid()?.get() as usize;
    (index != 0).then(|| objects.get(index)).flatten()
}

fn file_url_path(value: &str) -> Option<PathBuf> {
    let encoded = value.strip_prefix("file://")?;
    if !encoded.starts_with('/') {
        return None;
    }
    let bytes = encoded.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' {
            let high = hex_value(*bytes.get(index + 1)?)?;
            let low = hex_value(*bytes.get(index + 2)?)?;
            decoded.push((high << 4) | low);
            index += 3;
        } else {
            decoded.push(bytes[index]);
            index += 1;
        }
    }
    String::from_utf8(decoded).ok().map(PathBuf::from)
}

fn hex_value(value: u8) -> Option<u8> {
    match value {
        b'0'..=b'9' => Some(value - b'0'),
        b'a'..=b'f' => Some(value - b'a' + 10),
        b'A'..=b'F' => Some(value - b'A' + 10),
        _ => None,
    }
}

fn number_value(value: &Value) -> Option<f64> {
    value
        .as_real()
        .or_else(|| value.as_signed_integer().map(|number| number as f64))
        .or_else(|| value.as_unsigned_integer().map(|number| number as f64))
}

fn artifact_from_record(
    record: BackgroundAppRecord,
    database_modified_at_ms: Option<u64>,
    enabled_paths: Option<&BTreeSet<PathBuf>>,
) -> PlatformStartupArtifact {
    let metadata = read_bundle_metadata(&record.path);
    let name = metadata
        .as_ref()
        .and_then(bundle_name)
        .unwrap_or(record.name);
    let executable_name = metadata
        .as_ref()
        .and_then(|metadata| string_value(metadata, "CFBundleExecutable"));
    // BTM already provides an exact bundle identity and a developer label for these
    // system-managed records. Synchronous signature validation can take seconds per
    // application on a cold macOS trust cache, so leave trust enrichment unknown
    // instead of blocking the complete startup scan for non-actionable metadata.
    let mut diagnostics = Vec::new();
    if !record.path.exists() {
        diagnostics.push(PlatformStartupDiagnosticCode::MissingTarget);
    }
    let enabled = enabled_paths.is_some_and(|paths| paths.contains(&record.path))
        || (enabled_paths.is_none() && record.disposition & DISPOSITION_ENABLED != 0);
    PlatformStartupArtifact {
        provider_item_id: format!("background-task:{}", record.identifier),
        source_kind: PlatformStartupSourceKind::BackgroundTask,
        scope: PlatformStartupScope::CurrentUser,
        triggers: vec![PlatformStartupTrigger::UserLogon],
        display_name: name.clone(),
        configuration_path: None,
        target: PlatformStartupTarget {
            kind: PlatformStartupTargetKind::Application,
            identity_key: format!("bundle:{}", record.bundle_identifier),
            path: Some(record.path.clone()),
            executable_name,
            arguments: Vec::new(),
        },
        owner: PlatformStartupOwner {
            identity_key: Some(format!("bundle:{}", record.bundle_identifier)),
            name: Some(name),
            publisher: record.developer_name,
            summary: None,
            summary_source: PlatformStartupSummarySource::BundleMetadata,
            version: metadata
                .as_ref()
                .and_then(|metadata| string_value(metadata, "CFBundleShortVersionString")),
            icon_path: Some(record.path),
            confidence: PlatformStartupIdentityConfidence::Exact,
        },
        configured_state: if enabled {
            PlatformStartupConfiguredState::Enabled
        } else {
            PlatformStartupConfiguredState::Disabled
        },
        runtime_state: PlatformStartupRuntimeState::Unknown,
        control_capability: if enabled_paths.is_some() {
            PlatformStartupControlCapability::Toggleable
        } else {
            PlatformStartupControlCapability::SystemManaged
        },
        trust: PlatformStartupTrustState::Unknown,
        modified_at_ms: record
            .modified_at
            .and_then(cocoa_time_ms)
            .or(database_modified_at_ms),
        diagnostics,
    }
}

pub(super) fn change(
    request: &PlatformStartupChangeRequest,
) -> PlatformResult<PlatformStartupChangeResult> {
    let cancellation = PlatformCancellation::new(|| false);
    let current = scan(&cancellation)
        .items
        .into_iter()
        .find(|artifact| artifact.provider_item_id == request.provider_item_id)
        .ok_or_else(|| PlatformError::item_changed("background login item no longer exists"))?;
    if current != request.expected_artifact {
        return Err(PlatformError::item_changed(
            "background login item changed after preflight",
        ));
    }
    if current.control_capability != PlatformStartupControlCapability::Toggleable {
        return Err(PlatformError::new(
            crate::PlatformErrorCode::Unsupported,
            "background login item is not toggleable",
        ));
    }
    if request.desired_state == PlatformStartupDesiredState::Removed {
        return Err(PlatformError::new(
            crate::PlatformErrorCode::Unsupported,
            "background login items cannot be removed by startup management",
        ));
    }
    let path =
        current.target.path.as_deref().ok_or_else(|| {
            PlatformError::invalid_path("background login item path is unavailable")
        })?;
    let enabled = request.desired_state == PlatformStartupDesiredState::Enabled;
    login_items::set_enabled(path, enabled)?;
    let verified_enabled = login_items::enabled_paths()?.contains(path);
    let configured_state = if verified_enabled {
        PlatformStartupConfiguredState::Enabled
    } else {
        PlatformStartupConfiguredState::Disabled
    };
    let desired_state = if enabled {
        PlatformStartupConfiguredState::Enabled
    } else {
        PlatformStartupConfiguredState::Disabled
    };
    Ok(PlatformStartupChangeResult {
        previous_state: current.configured_state,
        configured_state,
        verified: configured_state == desired_state,
    })
}

fn cocoa_time_ms(seconds: f64) -> Option<u64> {
    (seconds.is_finite() && seconds >= 0.0)
        .then(|| ((seconds + COCOA_REFERENCE_DATE_OFFSET_SECONDS as f64) * 1000.0).round() as u64)
}

fn system_time_ms(value: SystemTime) -> Option<u64> {
    value
        .duration_since(UNIX_EPOCH)
        .ok()
        .map(|duration| duration.as_millis() as u64)
}

fn unavailable(
    started: Instant,
    reason: PlatformStartupCoverageReason,
) -> PlatformStartupSourceResult {
    result(
        Vec::new(),
        PlatformStartupCoverageStatus::Unavailable,
        Some(reason),
        started,
    )
}

fn result(
    items: Vec<PlatformStartupArtifact>,
    status: PlatformStartupCoverageStatus,
    reason: Option<PlatformStartupCoverageReason>,
    started: Instant,
) -> PlatformStartupSourceResult {
    PlatformStartupSourceResult {
        source_id: SOURCE_ID.to_owned(),
        required: false,
        status,
        reason,
        items,
        elapsed_ms: started.elapsed().as_millis() as u64,
    }
}

#[cfg(test)]
mod tests {
    use plist::{Uid, Value};

    use super::*;

    fn dictionary(values: impl IntoIterator<Item = (&'static str, Value)>) -> Value {
        Value::Dictionary(
            values
                .into_iter()
                .map(|(key, value)| (key.to_owned(), value))
                .collect(),
        )
    }

    fn uid(value: u64) -> Value {
        Value::Uid(Uid::new(value))
    }

    fn fixture_archive(disposition: u64) -> Value {
        let objects = vec![
            Value::String("$null".to_owned()),
            dictionary([("$classname", Value::String("ItemRecord".to_owned()))]),
            dictionary([
                ("$class", uid(1)),
                ("type", Value::Integer(APP_RECORD_TYPE.into())),
                ("identifier", uid(3)),
                ("bundleIdentifier", uid(4)),
                ("name", uid(5)),
                ("developerName", uid(6)),
                ("url", uid(7)),
                ("disposition", Value::Integer(disposition.into())),
                ("modificationDate", Value::Real(1.0)),
            ]),
            Value::String("2.com.example.Example".to_owned()),
            Value::String("com.example.Example".to_owned()),
            Value::String("Example".to_owned()),
            Value::String("Example Developer".to_owned()),
            dictionary([("NS.relative", uid(8))]),
            Value::String("file:///Applications/Example%20App.app/".to_owned()),
        ];
        dictionary([
            ("$archiver", Value::String("NSKeyedArchiver".to_owned())),
            ("$top", dictionary([("version", Value::Integer(13.into()))])),
            ("$objects", Value::Array(objects)),
        ])
    }

    #[test]
    fn parses_supported_application_records() {
        let cancellation = PlatformCancellation::new(|| false);
        let records = parse_archive(&fixture_archive(11), 13, &cancellation).unwrap();

        assert_eq!(records.len(), 1);
        assert_eq!(records[0].name, "Example");
        assert_eq!(
            records[0].path,
            PathBuf::from("/Applications/Example App.app/")
        );
        assert_eq!(records[0].disposition, 11);

        let artifact = artifact_from_record(records.into_iter().next().unwrap(), None, None);
        assert_eq!(
            artifact.owner.publisher.as_deref(),
            Some("Example Developer")
        );
        assert_eq!(artifact.trust, PlatformStartupTrustState::Unknown);
    }

    #[test]
    fn rejects_an_unexpected_archive_version() {
        let cancellation = PlatformCancellation::new(|| false);

        assert_eq!(
            parse_archive(&fixture_archive(11), 12, &cancellation),
            Err(ParseError::Unsupported)
        );
    }

    #[test]
    fn disposition_low_bit_controls_configured_state() {
        let enabled = artifact_from_record(
            parse_archive(
                &fixture_archive(11),
                13,
                &PlatformCancellation::new(|| false),
            )
            .unwrap()
            .remove(0),
            None,
            None,
        );
        let disabled = artifact_from_record(
            parse_archive(
                &fixture_archive(10),
                13,
                &PlatformCancellation::new(|| false),
            )
            .unwrap()
            .remove(0),
            None,
            None,
        );

        assert_eq!(
            enabled.configured_state,
            PlatformStartupConfiguredState::Enabled
        );
        assert_eq!(
            disabled.configured_state,
            PlatformStartupConfiguredState::Disabled
        );
    }

    #[test]
    fn shared_login_item_membership_enables_direct_management() {
        let record = parse_archive(
            &fixture_archive(10),
            13,
            &PlatformCancellation::new(|| false),
        )
        .unwrap()
        .remove(0);
        let path = record.path.clone();
        let enabled_paths = BTreeSet::from([path]);

        let artifact = artifact_from_record(record, None, Some(&enabled_paths));

        assert_eq!(
            artifact.configured_state,
            PlatformStartupConfiguredState::Enabled
        );
        assert_eq!(
            artifact.control_capability,
            PlatformStartupControlCapability::Toggleable
        );
    }

    #[test]
    #[ignore = "requires a supported macOS background item database"]
    fn parses_the_installed_background_item_database() {
        let (version, path) = database_path().expect("a supported background item database");
        let archive = Value::from_file(path).expect("a readable background item database");
        let records = parse_archive(&archive, version, &PlatformCancellation::new(|| false))
            .expect("a supported background item schema");

        assert!(!records.is_empty());
    }
}

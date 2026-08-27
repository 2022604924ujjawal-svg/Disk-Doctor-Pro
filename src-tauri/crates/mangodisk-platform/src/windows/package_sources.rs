use std::{
    collections::HashSet,
    env, fs,
    io::Read,
    path::{Path, PathBuf},
    process,
    time::{Duration, Instant, UNIX_EPOCH},
};

use serde::Deserialize;
use serde_json::Value;

use crate::{
    command::{
        run_controlled_command, ControlledCommandLimits, ControlledEnvironmentPolicy,
        ControlledExecutable,
    },
    ApplicationInstallScope, ApplicationInventorySource, ApplicationUninstallRegistration,
    PlatformCancellation,
};

use super::{native_uninstall, package_evidence, package_locations, path_identity};

const COMMAND_TIMEOUT: Duration = Duration::from_secs(20);
const COMMAND_OUTPUT_LIMIT: usize = 2 * 1024 * 1024;
const MAX_PACKAGE_ENTRIES_PER_SOURCE: usize = 10_000;
const MAX_PACKAGE_METADATA_BYTES: u64 = 2 * 1024 * 1024;
const MAX_WINGET_EXPORT_BYTES: u64 = 8 * 1024 * 1024;

#[derive(Debug)]
pub(super) struct PackageSourceFact {
    pub source: ApplicationInventorySource,
    pub identifier: String,
    pub name: String,
    pub version: Option<String>,
    pub install_path: Option<PathBuf>,
    pub estimated_bytes: u64,
    pub installed_at_ms: Option<u64>,
    pub uninstall_registration: Option<ApplicationUninstallRegistration>,
    /// Whether this source has enough evidence to represent a standalone app
    /// when no registry or AppX row matches it.
    pub surface_when_unmatched: bool,
}

#[derive(Debug, Default)]
pub(super) struct PackageSourceInventory {
    pub facts: Vec<PackageSourceFact>,
    pub detected_sources: Vec<&'static str>,
    pub complete: bool,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "PascalCase")]
struct WingetExport {
    sources: Vec<WingetExportSource>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "PascalCase")]
struct WingetExportSource {
    packages: Vec<WingetExportPackage>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "PascalCase")]
struct WingetExportPackage {
    package_identifier: String,
    version: Option<String>,
}

pub(super) fn inventory(cancellation: PlatformCancellation) -> PackageSourceInventory {
    let started = Instant::now();
    let mut result = PackageSourceInventory {
        complete: true,
        ..PackageSourceInventory::default()
    };
    let source_started = Instant::now();
    discover_scoop(&mut result, &cancellation);
    let scoop_elapsed_ms = source_started.elapsed().as_millis();
    let source_started = Instant::now();
    if cancellation.is_cancelled() {
        result.complete = false;
        return result;
    }
    discover_steam(&mut result, &cancellation);
    let steam_elapsed_ms = source_started.elapsed().as_millis();
    let source_started = Instant::now();
    if cancellation.is_cancelled() {
        result.complete = false;
        return result;
    }
    discover_chocolatey(&mut result, &cancellation);
    let chocolatey_elapsed_ms = source_started.elapsed().as_millis();
    let source_started = Instant::now();
    if cancellation.is_cancelled() {
        result.complete = false;
        return result;
    }
    discover_winget(&mut result, &cancellation);
    let winget_elapsed_ms = source_started.elapsed().as_millis();
    log::info!(
        "windows_package_source_inventory_ready fact_count={} detected_source_count={} complete={} scoop_elapsed_ms={} steam_elapsed_ms={} chocolatey_elapsed_ms={} winget_elapsed_ms={} elapsed_ms={}",
        result.facts.len(),
        result.detected_sources.len(),
        result.complete,
        scoop_elapsed_ms,
        steam_elapsed_ms,
        chocolatey_elapsed_ms,
        winget_elapsed_ms,
        started.elapsed().as_millis(),
    );
    result
}

pub(super) fn revision_fingerprint() -> String {
    let mut paths = package_locations::scoop_roots()
        .into_iter()
        .map(|root| root.path.join("apps"))
        .collect::<Vec<_>>();
    if let Some(root) = steam_root() {
        append_steam_revision_paths(&mut paths, &root);
    }
    if let Some(root) = package_locations::chocolatey_root() {
        paths.push(root.join("lib"));
    }
    revision_fingerprint_for_paths(paths)
}

fn revision_fingerprint_for_paths(mut paths: Vec<PathBuf>) -> String {
    paths.sort_by_key(|path| path_identity::comparison_key(path));
    paths.dedup_by(|left, right| path_identity::equal(left, right));
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"mangodisk-windows-package-source-revision-v1");
    for path in paths {
        hasher.update(path_identity::comparison_key(&path).as_bytes());
        append_path_revision(&mut hasher, &path);
    }
    hasher.finalize().to_hex().to_string()
}

fn append_steam_revision_paths(paths: &mut Vec<PathBuf>, steam_root: &Path) {
    let library_index = steam_root.join("steamapps").join("libraryfolders.vdf");
    paths.push(library_index);
    let (libraries, _) = steam_library_paths(steam_root);
    paths.extend(
        libraries
            .into_iter()
            .map(|library| library.join("steamapps")),
    );
}

fn discover_scoop(result: &mut PackageSourceInventory, cancellation: &PlatformCancellation) {
    let mut detected = false;
    let mut remaining_entries = MAX_PACKAGE_ENTRIES_PER_SOURCE;
    for root in package_locations::scoop_roots() {
        if cancellation.is_cancelled() || remaining_entries == 0 {
            result.complete = false;
            break;
        }
        let apps = root.path.join("apps");
        if !apps.is_dir() {
            continue;
        }
        detected = true;
        let Ok(entries) = fs::read_dir(&apps) else {
            result.complete = false;
            continue;
        };
        let scoop_script = root
            .path
            .join("apps")
            .join("scoop")
            .join("current")
            .join("bin")
            .join("scoop.ps1");
        let scoop_script_digest = (root.scope == ApplicationInstallScope::CurrentUser)
            .then(|| file_digest(&scoop_script))
            .flatten();
        for entry in entries {
            if cancellation.is_cancelled() || remaining_entries == 0 {
                result.complete = false;
                break;
            }
            remaining_entries -= 1;
            let Ok(entry) = entry else {
                result.complete = false;
                continue;
            };
            let package_name = entry.file_name().to_string_lossy().trim().to_string();
            if package_name.is_empty() || !entry.path().is_dir() {
                continue;
            }
            let current = entry.path().join("current");
            let install_file = current.join("install.json");
            let manifest_file = current.join("manifest.json");
            let install = read_json(&install_file);
            let manifest = read_json(&manifest_file);
            let version = install
                .as_ref()
                .and_then(|value| json_string(value, "version"))
                .or_else(|| {
                    manifest
                        .as_ref()
                        .and_then(|value| json_string(value, "version"))
                })
                .or_else(|| scoop_current_version(&current));
            // Scoop manifests expose a free-form description rather than a
            // dedicated display name. Using that sentence as the catalog name
            // makes rows unstable across manifest revisions and produces poor
            // search results. The package identifier is the canonical,
            // user-recognizable name used by Scoop itself.
            let name = package_name.clone();
            let uninstall_registration = (package_name != "scoop"
                && root.scope == ApplicationInstallScope::CurrentUser)
                .then(|| {
                    Some(ApplicationUninstallRegistration::WindowsScoop {
                        package_name: package_name.clone(),
                        scope: root.scope,
                        install_root: root.path.clone(),
                        package_marker_digest: package_evidence::file_set_digest(&[
                            &install_file,
                            &manifest_file,
                        ])?,
                        scoop_script_digest: scoop_script_digest.clone()?,
                        estimated_bytes: 0,
                    })
                })
                .flatten();
            result.facts.push(PackageSourceFact {
                source: ApplicationInventorySource::Scoop,
                identifier: scoop_source_identifier(root.scope, &package_name),
                name,
                version,
                install_path: current.is_dir().then_some(current.clone()),
                estimated_bytes: 0,
                installed_at_ms: path_timestamp_millis(&current),
                uninstall_registration,
                surface_when_unmatched: true,
            });
        }
    }
    if detected {
        result.detected_sources.push("scoop");
    }
}

fn scoop_source_identifier(scope: ApplicationInstallScope, package_name: &str) -> String {
    format!("{}:{package_name}", install_scope_code(scope))
}

const fn install_scope_code(scope: ApplicationInstallScope) -> &'static str {
    match scope {
        ApplicationInstallScope::CurrentUser => "current-user",
        ApplicationInstallScope::Machine => "machine",
    }
}

fn discover_winget(result: &mut PackageSourceInventory, cancellation: &PlatformCancellation) {
    let Some(executable) = native_uninstall::trusted_winget_path() else {
        return;
    };
    result.detected_sources.push("winget");
    let Ok(executable) = ControlledExecutable::capture(&executable) else {
        result.complete = false;
        return;
    };
    let output_path = env::temp_dir().join(format!(
        "mangodisk-winget-export-{}-{}.json",
        process::id(),
        current_time_nanos()
    ));
    let output_path_text = output_path.to_string_lossy().into_owned();
    let arguments = [
        "export",
        "--output",
        output_path_text.as_str(),
        "--include-versions",
        "--accept-source-agreements",
        "--disable-interactivity",
    ];
    let limits = ControlledCommandLimits {
        timeout: COMMAND_TIMEOUT,
        stdout_bytes: 256 * 1024,
        stderr_bytes: 256 * 1024,
    };
    let command_result = run_controlled_command(
        "windows-winget-inventory",
        &executable,
        &arguments,
        ControlledEnvironmentPolicy::Inherit,
        limits,
        &|| cancellation.is_cancelled(),
    );
    let export = command_result
        .ok()
        .filter(|output| output.status.success())
        .and_then(|_| read_bounded_bytes(&output_path, MAX_WINGET_EXPORT_BYTES))
        .and_then(|contents| serde_json::from_slice::<WingetExport>(&contents).ok());
    let _ = fs::remove_file(&output_path);

    let Some(export) = export else {
        result.complete = false;
        return;
    };
    for (index, package) in export
        .sources
        .into_iter()
        .flat_map(|source| source.packages)
        .enumerate()
    {
        if index >= MAX_PACKAGE_ENTRIES_PER_SOURCE {
            result.complete = false;
            break;
        }
        let identifier = package.package_identifier.trim().to_string();
        if identifier.is_empty() {
            result.complete = false;
            continue;
        }
        result.facts.push(PackageSourceFact {
            source: ApplicationInventorySource::Winget,
            // WinGet export intentionally carries a stable package identifier
            // rather than a localized display name. Inventory reconciliation
            // may use strict version/name-segment evidence to attach it to an
            // existing Windows registration, but never surfaces an unmatched
            // export row as an independently uninstallable application.
            name: identifier.clone(),
            identifier,
            version: package.version.filter(|value| !value.trim().is_empty()),
            install_path: None,
            estimated_bytes: 0,
            installed_at_ms: None,
            uninstall_registration: None,
            surface_when_unmatched: false,
        });
    }
}

fn discover_steam(result: &mut PackageSourceInventory, cancellation: &PlatformCancellation) {
    let Some(steam_root) = steam_root() else {
        return;
    };
    result.detected_sources.push("steam");
    let (libraries, library_index_complete) = steam_library_paths(&steam_root);
    if !library_index_complete {
        result.complete = false;
    }
    let mut remaining_entries = MAX_PACKAGE_ENTRIES_PER_SOURCE;
    for library in libraries {
        if cancellation.is_cancelled() || remaining_entries == 0 {
            result.complete = false;
            break;
        }
        if !discover_steam_library(
            &library,
            &mut result.facts,
            &mut remaining_entries,
            cancellation,
        ) {
            result.complete = false;
        }
    }
}

fn steam_library_paths(steam_root: &Path) -> (Vec<PathBuf>, bool) {
    let mut libraries = vec![steam_root.to_path_buf()];
    let library_file = steam_root.join("steamapps").join("libraryfolders.vdf");
    let mut complete = true;
    if library_file.is_file() {
        let Some(contents) = read_bounded_text(&library_file) else {
            return (libraries, false);
        };
        for line in contents.lines() {
            if let Some(path) = vdf_value(line, "path") {
                libraries.push(PathBuf::from(path.replace("\\\\", "\\")));
            }
        }
    }
    libraries.sort();
    libraries.dedup();
    if libraries.len() > MAX_PACKAGE_ENTRIES_PER_SOURCE {
        libraries.truncate(MAX_PACKAGE_ENTRIES_PER_SOURCE);
        complete = false;
    }
    (libraries, complete)
}

/// Reads one Steam library without launching Steam or trusting registry
/// display names. Returning `false` marks the package-source inventory
/// incomplete while retaining every independently valid manifest.
fn discover_steam_library(
    library: &Path,
    facts: &mut Vec<PackageSourceFact>,
    remaining_entries: &mut usize,
    cancellation: &PlatformCancellation,
) -> bool {
    let steamapps = library.join("steamapps");
    let Ok(entries) = fs::read_dir(&steamapps) else {
        return false;
    };
    let mut complete = true;
    for entry in entries {
        if cancellation.is_cancelled() || *remaining_entries == 0 {
            complete = false;
            break;
        }
        *remaining_entries -= 1;
        let Ok(entry) = entry else {
            complete = false;
            continue;
        };
        let file_name = entry.file_name().to_string_lossy().to_string();
        if !file_name.starts_with("appmanifest_") || !file_name.ends_with(".acf") {
            continue;
        }
        let Some(contents) = read_bounded_text(&entry.path()) else {
            complete = false;
            continue;
        };
        let app_id = vdf_document_value(&contents, "appid");
        let name = vdf_document_value(&contents, "name");
        let install_dir = vdf_document_value(&contents, "installdir");
        let (Some(app_id), Some(name), Some(install_dir)) = (app_id, name, install_dir) else {
            complete = false;
            continue;
        };
        if !app_id.chars().all(|character| character.is_ascii_digit())
            || install_dir
                .chars()
                .any(|character| matches!(character, '/' | '\\'))
        {
            complete = false;
            continue;
        }
        let estimated_bytes = vdf_document_value(&contents, "SizeOnDisk")
            .and_then(|value| value.parse::<u64>().ok())
            .unwrap_or_default();
        let installed_at_ms = vdf_document_value(&contents, "LastUpdated")
            .and_then(|value| value.parse::<u64>().ok())
            .map(|seconds| seconds.saturating_mul(1_000));
        facts.push(PackageSourceFact {
            source: ApplicationInventorySource::Steam,
            identifier: app_id,
            name,
            version: None,
            install_path: Some(steamapps.join("common").join(install_dir)),
            estimated_bytes,
            installed_at_ms,
            uninstall_registration: None,
            surface_when_unmatched: true,
        });
    }
    complete
}

fn discover_chocolatey(result: &mut PackageSourceInventory, cancellation: &PlatformCancellation) {
    let Some(executable_path) = chocolatey_executable() else {
        return;
    };
    result.detected_sources.push("chocolatey");
    let Ok(executable) = ControlledExecutable::capture(&executable_path) else {
        result.complete = false;
        return;
    };
    let limits = ControlledCommandLimits {
        timeout: COMMAND_TIMEOUT,
        stdout_bytes: COMMAND_OUTPUT_LIMIT,
        stderr_bytes: 64 * 1024,
    };
    let output = run_controlled_command(
        "windows-chocolatey-inventory",
        &executable,
        &["list", "--limit-output", "--no-color"],
        ControlledEnvironmentPolicy::Inherit,
        limits,
        &|| cancellation.is_cancelled(),
    );
    let Ok(output) = output else {
        result.complete = false;
        return;
    };
    if !output.status.success() {
        result.complete = false;
        return;
    }
    let install_root = package_locations::chocolatey_root();
    let mut accepted_packages = 0_usize;
    let mut packages = Vec::new();
    for line in String::from_utf8_lossy(&output.stdout).lines() {
        if cancellation.is_cancelled() {
            result.complete = false;
            break;
        }
        let Some((name, version)) = line.split_once('|') else {
            continue;
        };
        let name = name.trim();
        let version = version.trim();
        if name.is_empty()
            || version.is_empty()
            || name.eq_ignore_ascii_case("chocolatey")
            || name.to_ascii_lowercase().ends_with(".extension")
        {
            continue;
        }
        if accepted_packages >= MAX_PACKAGE_ENTRIES_PER_SOURCE {
            result.complete = false;
            break;
        }
        accepted_packages = accepted_packages.saturating_add(1);
        let install_path = install_root
            .as_ref()
            .map(|root| root.join("lib").join(name))
            .filter(|path| path.is_dir());
        let uninstall_registration = install_root
            .as_ref()
            .and_then(|root| chocolatey_registration_from_evidence(name, root, &executable, 0));
        packages.push(PackageSourceFact {
            source: ApplicationInventorySource::Chocolatey,
            identifier: name.to_string(),
            name: name.to_string(),
            version: Some(version.to_string()),
            installed_at_ms: install_path.as_deref().and_then(path_timestamp_millis),
            install_path,
            estimated_bytes: 0,
            uninstall_registration,
            surface_when_unmatched: false,
        });
    }
    let dependency_ids = install_root
        .as_deref()
        .map(|root| chocolatey_dependency_ids(root, &packages))
        .unwrap_or_default();
    for mut package in packages {
        // Chocolatey does not persist whether a package was explicitly
        // requested. The installed dependency graph provides the narrowest
        // durable approximation: graph roots are user-facing packages, while
        // referenced packages remain hidden unless they match a Windows app.
        package.surface_when_unmatched = package.uninstall_registration.is_some()
            && !dependency_ids.contains(&package.identifier.to_ascii_lowercase());
        result.facts.push(package);
    }
}

fn chocolatey_dependency_ids(
    install_root: &Path,
    packages: &[PackageSourceFact],
) -> HashSet<String> {
    packages
        .iter()
        .filter_map(|package| {
            let path = install_root
                .join("lib")
                .join(&package.identifier)
                .join(format!("{}.nuspec", package.identifier));
            read_bounded_text(&path)
        })
        .flat_map(|contents| nuspec_dependency_ids(&contents))
        .collect()
}

fn nuspec_dependency_ids(contents: &str) -> Vec<String> {
    let lowercase = contents.to_ascii_lowercase();
    let mut remaining = lowercase.as_str();
    let mut dependencies = Vec::new();
    while let Some(offset) = remaining.find("<dependency") {
        remaining = &remaining[offset + "<dependency".len()..];
        let Some(end) = remaining.find('>') else {
            break;
        };
        let tag = &remaining[..end];
        if let Some(identifier) = xml_attribute(tag, "id").filter(|value| valid_package_name(value))
        {
            dependencies.push(identifier.to_string());
        }
        remaining = &remaining[end + 1..];
    }
    dependencies
}

fn xml_attribute<'a>(tag: &'a str, expected_name: &str) -> Option<&'a str> {
    let mut cursor = 0;
    while let Some(relative_offset) = tag[cursor..].find(expected_name) {
        let offset = cursor + relative_offset;
        let prefix_boundary = offset == 0
            || tag[..offset]
                .chars()
                .next_back()
                .is_some_and(char::is_whitespace);
        let remaining = &tag[offset + expected_name.len()..];
        let suffix_boundary = remaining
            .chars()
            .next()
            .is_some_and(|character| character.is_whitespace() || character == '=');
        if prefix_boundary && suffix_boundary {
            let value = remaining.trim_start().strip_prefix('=')?.trim_start();
            let quote = value.chars().next()?;
            if !matches!(quote, '\'' | '"') {
                return None;
            }
            let value = &value[quote.len_utf8()..];
            let end = value.find(quote)?;
            return Some(&value[..end]);
        }
        cursor = offset + expected_name.len();
    }
    None
}

fn steam_root() -> Option<PathBuf> {
    use winreg::{enums::HKEY_CURRENT_USER, RegKey};

    let key = RegKey::predef(HKEY_CURRENT_USER)
        .open_subkey(r"SOFTWARE\Valve\Steam")
        .ok()?;
    ["SteamPath", "InstallPath"]
        .into_iter()
        .find_map(|name| key.get_value::<String, _>(name).ok())
        .map(PathBuf::from)
        .filter(|path| path.is_dir())
}

fn chocolatey_executable() -> Option<PathBuf> {
    let root = package_locations::chocolatey_root()?;
    [root.join("bin").join("choco.exe"), root.join("choco.exe")]
        .into_iter()
        .find(|path| path.is_file())
}

/// Derives a Chocolatey package identity from a registered install source.
///
/// Chocolatey-backed MSI packages commonly record
/// `<ChocolateyInstall>\lib\<package>\tools` as `InstallSource`, while their
/// Windows display name can differ substantially from the package ID. The
/// package marker check prevents an arbitrary similarly shaped path from
/// becoming cross-source identity evidence. This identity is metadata only;
/// the Windows registration remains the sole uninstall authority.
pub(super) fn chocolatey_package_from_install_source(value: &str) -> Option<String> {
    let source = PathBuf::from(value.trim().trim_matches('"'));
    if !source.is_absolute() {
        return None;
    }
    let root = package_locations::chocolatey_root()?.join("lib");
    let relative = strip_windows_path_prefix(&source, &root)?;
    let mut components = relative
        .split('\\')
        .filter(|component| !component.is_empty());
    let package = components.next()?.trim();
    if package.is_empty()
        || !package.chars().all(|character| {
            character.is_ascii_alphanumeric() || matches!(character, '.' | '-' | '_')
        })
    {
        return None;
    }
    let package_root = root.join(package);
    let has_marker = [
        package_root.join(format!("{package}.nuspec")),
        package_root.join(format!("{package}.nupkg")),
    ]
    .into_iter()
    .any(|path| path.is_file());
    has_marker.then(|| package.to_string())
}

/// Captures typed Chocolatey uninstall authority from package-manager-owned
/// evidence. Callers may use this only after an exact package identity was
/// obtained from a registry value or Chocolatey's own inventory.
pub(super) fn chocolatey_uninstall_registration(
    package_name: &str,
    estimated_bytes: u64,
) -> Option<ApplicationUninstallRegistration> {
    let install_root = package_locations::chocolatey_root()?;
    let executable = ControlledExecutable::capture(&chocolatey_executable()?).ok()?;
    chocolatey_registration_from_evidence(package_name, &install_root, &executable, estimated_bytes)
}

fn chocolatey_registration_from_evidence(
    package_name: &str,
    install_root: &Path,
    executable: &ControlledExecutable,
    estimated_bytes: u64,
) -> Option<ApplicationUninstallRegistration> {
    if !valid_package_name(package_name) || package_name.eq_ignore_ascii_case("chocolatey") {
        return None;
    }
    let package_marker_digest = chocolatey_package_marker_digest(install_root, package_name)?;
    Some(ApplicationUninstallRegistration::WindowsChocolatey {
        package_name: package_name.to_string(),
        install_root: install_root.to_path_buf(),
        package_marker_digest,
        chocolatey_executable: executable.clone(),
        estimated_bytes,
    })
}

fn chocolatey_package_marker_digest(install_root: &Path, package_name: &str) -> Option<String> {
    if !valid_package_name(package_name) {
        return None;
    }
    let marker = install_root
        .join("lib")
        .join(package_name)
        .join(format!("{package_name}.nuspec"));
    package_evidence::file_set_digest(&[&marker])
}

fn valid_package_name(package_name: &str) -> bool {
    !package_name.is_empty()
        && package_name.len() <= 128
        && package_name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
}

fn strip_windows_path_prefix(path: &Path, root: &Path) -> Option<String> {
    path_identity::relative_child_key(path, root)
}

fn append_path_revision(hasher: &mut blake3::Hasher, path: &Path) {
    let Ok(metadata) = fs::metadata(path) else {
        hasher.update(b"missing");
        return;
    };
    hasher.update(&metadata.len().to_le_bytes());
    if let Ok(modified) = metadata.modified() {
        if let Ok(duration) = modified.duration_since(UNIX_EPOCH) {
            hasher.update(&duration.as_nanos().to_le_bytes());
        }
    }
    if !metadata.is_dir() {
        return;
    }
    let Ok(entries) = fs::read_dir(path) else {
        hasher.update(b"unreadable");
        return;
    };
    let mut children = entries
        .take(10_000)
        .filter_map(Result::ok)
        .map(|entry| {
            let metadata = entry.metadata().ok();
            (
                entry.file_name().to_string_lossy().to_ascii_lowercase(),
                metadata.as_ref().map(fs::Metadata::len).unwrap_or_default(),
                metadata
                    .and_then(|metadata| metadata.modified().ok())
                    .and_then(|modified| modified.duration_since(UNIX_EPOCH).ok())
                    .map(|duration| duration.as_nanos())
                    .unwrap_or_default(),
            )
        })
        .collect::<Vec<_>>();
    children.sort();
    for (name, bytes, modified_nanos) in children {
        hasher.update(name.as_bytes());
        hasher.update(&bytes.to_le_bytes());
        hasher.update(&modified_nanos.to_le_bytes());
    }
}

fn read_json(path: &Path) -> Option<Value> {
    let contents = read_bounded_text(path)?;
    serde_json::from_str(&contents).ok()
}

/// Reads package-manager metadata with an explicit memory ceiling.
///
/// Package directories are writable by their owning user and therefore
/// cannot be trusted to contain reasonably sized JSON or VDF files. Refusing
/// oversized metadata keeps application inventory responsive while marking
/// the optional source incomplete instead of risking an unbounded allocation.
fn read_bounded_text(path: &Path) -> Option<String> {
    String::from_utf8(read_bounded_bytes(path, MAX_PACKAGE_METADATA_BYTES)?).ok()
}

fn read_bounded_bytes(path: &Path, maximum_bytes: u64) -> Option<Vec<u8>> {
    let file = fs::File::open(path).ok()?;
    if file.metadata().ok()?.len() > maximum_bytes {
        return None;
    }
    let capacity = usize::try_from(maximum_bytes.min(64 * 1024)).ok()?;
    let mut contents = Vec::with_capacity(capacity);
    file.take(maximum_bytes.saturating_add(1))
        .read_to_end(&mut contents)
        .ok()?;
    (u64::try_from(contents.len()).ok()? <= maximum_bytes).then_some(contents)
}

fn scoop_current_version(current: &Path) -> Option<String> {
    let canonical = fs::canonicalize(current).ok()?;
    let version = canonical.file_name()?.to_string_lossy().trim().to_string();
    (!version.is_empty() && !version.eq_ignore_ascii_case("current")).then_some(version)
}

fn file_digest(path: &Path) -> Option<String> {
    package_evidence::file_set_digest(&[path])
}

fn json_string(value: &Value, key: &str) -> Option<String> {
    value.get(key)?.as_str().map(str::to_string)
}

fn vdf_document_value(contents: &str, key: &str) -> Option<String> {
    contents.lines().find_map(|line| vdf_value(line, key))
}

fn vdf_value(line: &str, expected_key: &str) -> Option<String> {
    let mut values = line
        .split('"')
        .enumerate()
        .filter_map(|(index, value)| (index % 2 == 1).then_some(value));
    let key = values.next()?;
    let value = values.next()?;
    key.eq_ignore_ascii_case(expected_key)
        .then(|| value.to_string())
}

fn path_timestamp_millis(path: &Path) -> Option<u64> {
    path.metadata()
        .ok()?
        .modified()
        .ok()?
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|duration| u64::try_from(duration.as_millis()).ok())
}

fn current_time_nanos() -> u128 {
    std::time::SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nuspec_dependencies_form_case_insensitive_package_identities() {
        let contents = r#"
            <package xmlns="http://schemas.microsoft.com/packaging/2011/08/nuspec.xsd">
              <metadata>
                <dependencies>
                  <dependency id="Chocolatey-Core.Extension" version="1.4.0" />
                  <dependency id = 'KB2919355' />
                  <dependency id="invalid package" />
                  <dependency packageid="must-not-match" />
                </dependencies>
              </metadata>
            </package>
        "#;

        assert_eq!(
            nuspec_dependency_ids(contents),
            vec![
                "chocolatey-core.extension".to_string(),
                "kb2919355".to_string()
            ]
        );
    }

    #[test]
    fn vdf_values_preserve_paths_and_names() {
        assert_eq!(
            vdf_value(r#"        "path"      "D:\\SteamLibrary""#, "path"),
            Some(r"D:\\SteamLibrary".to_string())
        );
        assert_eq!(
            vdf_document_value(
                "\"AppState\"\n{\n  \"appid\" \"12345\"\n  \"name\" \"Example Game\"\n}",
                "name"
            ),
            Some("Example Game".to_string())
        );
    }

    #[test]
    fn winget_export_decodes_stable_package_identity() {
        let export = serde_json::from_str::<WingetExport>(
            r#"{
                "Sources": [{
                    "Packages": [{
                        "PackageIdentifier": "Microsoft.VisualStudioCode",
                        "Version": "1.99.0"
                    }]
                }]
            }"#,
        )
        .expect("the WinGet export fixture should decode");

        assert_eq!(
            export.sources[0].packages[0].package_identifier,
            "Microsoft.VisualStudioCode"
        );
        assert_eq!(
            export.sources[0].packages[0].version.as_deref(),
            Some("1.99.0")
        );
    }

    #[test]
    fn package_metadata_reader_rejects_oversized_files() {
        let root = std::env::temp_dir().join(format!(
            "mangodisk-package-metadata-limit-{}-{}",
            process::id(),
            current_time_nanos()
        ));
        fs::create_dir_all(&root).expect("the metadata fixture directory should be created");
        let acceptable = root.join("acceptable.json");
        let oversized = root.join("oversized.json");
        fs::write(&acceptable, b"{\"version\":\"1.0\"}")
            .expect("the acceptable metadata fixture should be written");
        fs::write(
            &oversized,
            vec![b'x'; usize::try_from(MAX_PACKAGE_METADATA_BYTES).unwrap() + 1],
        )
        .expect("the oversized metadata fixture should be written");

        assert!(read_bounded_text(&acceptable).is_some());
        assert!(read_bounded_text(&oversized).is_none());

        fs::remove_dir_all(root).expect("the metadata fixture directory should be removed");
    }

    #[test]
    fn windows_path_prefix_requires_a_real_child_boundary() {
        let root = Path::new(r"C:\ProgramData\chocolatey\lib");
        assert_eq!(
            strip_windows_path_prefix(
                Path::new(r"C:\ProgramData\Chocolatey\lib\putty.install\tools"),
                root,
            )
            .as_deref(),
            Some(r"putty.install\tools")
        );
        assert!(strip_windows_path_prefix(
            Path::new(r"C:\ProgramData\chocolatey\library\putty.install"),
            root,
        )
        .is_none());
    }

    #[test]
    fn steam_library_manifests_produce_stable_catalog_facts() {
        let root = std::env::temp_dir().join(format!(
            "mangodisk-steam-library-{}-{}",
            process::id(),
            current_time_nanos()
        ));
        let steamapps = root.join("steamapps");
        fs::create_dir_all(steamapps.join("common").join("FixtureGame"))
            .expect("the Steam fixture should be created");
        fs::write(
            steamapps.join("appmanifest_12345.acf"),
            r#""AppState"
{
    "appid" "12345"
    "name" "Fixture Game"
    "installdir" "FixtureGame"
    "LastUpdated" "1700000000"
    "SizeOnDisk" "987654321"
}"#,
        )
        .expect("the Steam manifest should be written");

        let mut facts = Vec::new();
        let mut remaining_entries = MAX_PACKAGE_ENTRIES_PER_SOURCE;
        let cancellation = PlatformCancellation::new(|| false);
        assert!(discover_steam_library(
            &root,
            &mut facts,
            &mut remaining_entries,
            &cancellation,
        ));
        assert_eq!(facts.len(), 1);
        assert_eq!(facts[0].source, ApplicationInventorySource::Steam);
        assert_eq!(facts[0].identifier, "12345");
        assert_eq!(facts[0].name, "Fixture Game");
        assert_eq!(facts[0].estimated_bytes, 987_654_321);
        assert_eq!(facts[0].installed_at_ms, Some(1_700_000_000_000));
        let expected_install_path = steamapps.join("common").join("FixtureGame");
        assert_eq!(
            facts[0].install_path.as_deref(),
            Some(expected_install_path.as_path())
        );

        fs::remove_dir_all(root).expect("the Steam fixture should be removed");
    }

    #[test]
    fn steam_library_rejects_manifest_install_directory_escape() {
        let root = std::env::temp_dir().join(format!(
            "mangodisk-steam-library-invalid-{}-{}",
            process::id(),
            current_time_nanos()
        ));
        let steamapps = root.join("steamapps");
        fs::create_dir_all(&steamapps).expect("the Steam fixture should be created");
        fs::write(
            steamapps.join("appmanifest_12345.acf"),
            r#""AppState"
{
    "appid" "12345"
    "name" "Fixture Game"
    "installdir" "..\\Outside"
}"#,
        )
        .expect("the invalid Steam manifest should be written");

        let mut facts = Vec::new();
        let mut remaining_entries = MAX_PACKAGE_ENTRIES_PER_SOURCE;
        let cancellation = PlatformCancellation::new(|| false);
        assert!(!discover_steam_library(
            &root,
            &mut facts,
            &mut remaining_entries,
            &cancellation,
        ));
        assert!(facts.is_empty());

        fs::remove_dir_all(root).expect("the Steam fixture should be removed");
    }

    #[test]
    fn steam_secondary_library_manifest_changes_revision() {
        let root = std::env::temp_dir().join(format!(
            "mangodisk-steam-revision-{}-{}",
            process::id(),
            current_time_nanos()
        ));
        let primary = root.join("primary");
        let secondary = root.join("secondary");
        fs::create_dir_all(primary.join("steamapps"))
            .expect("the primary Steam library should be created");
        fs::create_dir_all(secondary.join("steamapps"))
            .expect("the secondary Steam library should be created");
        fs::write(
            primary.join("steamapps").join("libraryfolders.vdf"),
            format!(
                "\"libraryfolders\"\n{{\n  \"1\"\n  {{\n    \"path\" \"{}\"\n  }}\n}}",
                secondary.to_string_lossy().replace('\\', "\\\\")
            ),
        )
        .expect("the Steam library index should be written");
        let manifest = secondary.join("steamapps").join("appmanifest_42.acf");
        fs::write(&manifest, b"first")
            .expect("the initial secondary library manifest should be written");

        let fingerprint = || {
            let mut paths = Vec::new();
            append_steam_revision_paths(&mut paths, &primary);
            revision_fingerprint_for_paths(paths)
        };
        let before = fingerprint();
        fs::write(&manifest, b"second-version")
            .expect("the secondary library manifest should be updated");
        let after = fingerprint();

        assert_ne!(before, after);
        fs::remove_dir_all(root).expect("the Steam revision fixture should be removed");
    }
}

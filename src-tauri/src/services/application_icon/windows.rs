use std::{
    cmp::Reverse,
    collections::HashMap,
    fs,
    io::Write,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

use base64::{engine::general_purpose::STANDARD, Engine as _};
use serde::{Deserialize, Serialize};

use mangodisk_platform::configure_background_process;

use super::{cache::ApplicationIconCache, ApplicationIcon, ApplicationIconLoadResult};

const MAX_ICON_SOURCE_ENTRIES: usize = 512;
const MAX_ICON_BYTES: u64 = 2 * 1024 * 1024;

const EXTRACT_ICON_SCRIPT: &str = r#"
[Console]::InputEncoding = [System.Text.UTF8Encoding]::new($false)
[Console]::OutputEncoding = [System.Text.UTF8Encoding]::new($false)
Add-Type -AssemblyName System.Drawing
Add-Type @'
using System;
using System.Runtime.InteropServices;

public static class MangoDiskShellIcon {
  [DllImport("shell32.dll", CharSet = CharSet.Unicode)]
  public static extern int SHDefExtractIcon(
    string iconFile,
    int iconIndex,
    uint flags,
    out IntPtr largeIcon,
    out IntPtr smallIcon,
    uint iconSize
  );

  [DllImport("user32.dll")]
  [return: MarshalAs(UnmanagedType.Bool)]
  public static extern bool DestroyIcon(IntPtr icon);
}
'@
$items = [Console]::In.ReadToEnd() | ConvertFrom-Json
$results = @()
foreach ($item in $items) {
  try {
    $source = [string]$item.source
    $background = [Drawing.Color]::Transparent
    if ($null -ne $item.background) {
      $value = [string]$item.background
      if ($value -and $value -ne 'transparent') {
        $background = [Drawing.ColorTranslator]::FromHtml($value)
      }
    }
    $extension = [IO.Path]::GetExtension($source).ToLowerInvariant()
    $image = $null
    $icon = $null
    if ($extension -eq '.png' -or $extension -eq '.jpg' -or $extension -eq '.jpeg') {
      $image = [Drawing.Image]::FromFile($source)
    } else {
      $largeIcon = [IntPtr]::Zero
      $smallIcon = [IntPtr]::Zero
      $iconSize = [uint32]((32 -shl 16) -bor 256)
      $result = [MangoDiskShellIcon]::SHDefExtractIcon(
        $source,
        0,
        0,
        [ref]$largeIcon,
        [ref]$smallIcon,
        $iconSize
      )
      if ($result -ge 0 -and $largeIcon -ne [IntPtr]::Zero) {
        $icon = [Drawing.Icon]::FromHandle($largeIcon).Clone()
        $image = $icon.ToBitmap()
      }
      if ($largeIcon -ne [IntPtr]::Zero) {
        [void][MangoDiskShellIcon]::DestroyIcon($largeIcon)
      }
      if ($smallIcon -ne [IntPtr]::Zero) {
        [void][MangoDiskShellIcon]::DestroyIcon($smallIcon)
      }
    }
    if ($null -eq $image) { continue }
    $bitmap = [Drawing.Bitmap]::new(128, 128)
    $graphics = [Drawing.Graphics]::FromImage($bitmap)
    $graphics.Clear($background)
    $graphics.InterpolationMode = [Drawing.Drawing2D.InterpolationMode]::HighQualityBicubic
    $graphics.PixelOffsetMode = [Drawing.Drawing2D.PixelOffsetMode]::HighQuality
    $graphics.SmoothingMode = [Drawing.Drawing2D.SmoothingMode]::HighQuality
    $graphics.DrawImage($image, 0, 0, 128, 128)
    $stream = [IO.MemoryStream]::new()
    $bitmap.Save($stream, [Drawing.Imaging.ImageFormat]::Png)
    $results += [pscustomobject]@{ source = $source; data = [Convert]::ToBase64String($stream.ToArray()) }
    $stream.Dispose(); $graphics.Dispose(); $bitmap.Dispose(); $image.Dispose()
    if ($null -ne $icon) { $icon.Dispose() }
  } catch { }
}
ConvertTo-Json -InputObject $results -Compress
"#;

#[derive(Serialize)]
struct IconDecodeRequest {
    source: String,
    background: Option<String>,
}

#[derive(Deserialize)]
struct IconDecodeResult {
    source: String,
    data: String,
}

struct PendingIcon {
    request_path: String,
    source: PathBuf,
    background: Option<String>,
    cache_key: String,
}

pub(super) fn load(paths: Vec<String>, cache_root: Option<PathBuf>) -> ApplicationIconLoadResult {
    let cache = ApplicationIconCache::new(cache_root);
    let mut result = ApplicationIconLoadResult::default();
    let mut pending = Vec::new();

    for request_path in paths {
        let Some(resolved) = resolve_icon_source(Path::new(&request_path)) else {
            continue;
        };
        let variant = resolved
            .background
            .as_deref()
            .unwrap_or_default()
            .as_bytes();
        let Some(lookup) = cache.lookup(&resolved.source, variant) else {
            continue;
        };
        if let Some(png) = lookup.png {
            result.icons.push(application_icon(request_path, png));
            result.cache_hits += 1;
        } else {
            pending.push(PendingIcon {
                request_path,
                source: resolved.source,
                background: resolved.background,
                cache_key: lookup.key,
            });
        }
    }

    let decoded = decode_icons(&pending);
    for pending in pending {
        let source = pending.source.to_string_lossy();
        let Some(png) = decoded.get(source.as_ref()) else {
            continue;
        };
        cache.store(&pending.cache_key, png);
        result
            .icons
            .push(application_icon(pending.request_path, png.clone()));
        result.decoded_icons += 1;
    }
    result
}

fn application_icon(path: String, png: Vec<u8>) -> ApplicationIcon {
    ApplicationIcon::new(
        path,
        format!("data:image/png;base64,{}", STANDARD.encode(png)),
    )
}

struct ResolvedIconSource {
    source: PathBuf,
    background: Option<String>,
}

fn resolve_icon_source(path: &Path) -> Option<ResolvedIconSource> {
    if let Some(source) = declared_icon_variant(path) {
        return Some(ResolvedIconSource {
            background: icon_background(&source),
            source,
        });
    }
    if path.is_file() {
        return Some(ResolvedIconSource {
            source: path.to_path_buf(),
            background: icon_background(path),
        });
    }
    if !path.is_dir() {
        return None;
    }

    let mut candidates = Vec::new();
    let mut directories = vec![(path.to_path_buf(), 0_u8)];
    let mut visited = 0;
    while let Some((directory, depth)) = directories.pop() {
        let Ok(entries) = fs::read_dir(directory) else {
            continue;
        };
        for entry in entries.filter_map(Result::ok) {
            visited += 1;
            if visited > MAX_ICON_SOURCE_ENTRIES {
                break;
            }
            let candidate = entry.path();
            if candidate.is_dir() && depth < 2 {
                directories.push((candidate, depth + 1));
                continue;
            }
            if !candidate.is_file() {
                continue;
            }
            let extension = candidate
                .extension()
                .and_then(|value| value.to_str())
                .unwrap_or_default();
            if matches!(
                extension.to_ascii_lowercase().as_str(),
                "png" | "ico" | "exe"
            ) {
                candidates.push(candidate);
            }
        }
        if visited > MAX_ICON_SOURCE_ENTRIES {
            break;
        }
    }
    candidates.sort_by_key(|candidate| icon_candidate_score(candidate));
    let source = candidates.into_iter().next()?;
    Some(ResolvedIconSource {
        background: icon_background(&source),
        source,
    })
}

fn icon_background(source: &Path) -> Option<String> {
    let name = source
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    // Windows applies the manifest background to regular target-size assets. Only explicitly
    // unplated variants are designed to remain transparent on the host surface.
    if name.contains("altform-unplated") || name.contains("altform-lightunplated") {
        return None;
    }
    appx_manifest_background(source)
}

fn declared_icon_variant(path: &Path) -> Option<PathBuf> {
    let parent = path.parent()?;
    let declared_stem = path.file_stem()?.to_string_lossy().to_ascii_lowercase();
    let declared_extension = path.extension()?.to_string_lossy().to_ascii_lowercase();
    if !matches!(declared_extension.as_str(), "png" | "jpg" | "jpeg") {
        return None;
    }
    let prefix = format!("{declared_stem}.");
    let mut candidates = fs::read_dir(parent)
        .ok()?
        .take(MAX_ICON_SOURCE_ENTRIES)
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|candidate| candidate.is_file())
        .filter(|candidate| {
            candidate
                .extension()
                .is_some_and(|extension| extension.eq_ignore_ascii_case(&declared_extension))
                && candidate.file_stem().is_some_and(|stem| {
                    stem.to_string_lossy()
                        .to_ascii_lowercase()
                        .starts_with(&prefix)
                })
        })
        .collect::<Vec<_>>();
    candidates.sort_by_key(|candidate| icon_candidate_score(candidate));
    candidates.into_iter().next()
}

fn appx_manifest_background(source: &Path) -> Option<String> {
    const MAX_MANIFEST_BYTES: u64 = 1024 * 1024;
    let mut directory = source.parent();
    for _ in 0..4 {
        let current = directory?;
        let manifest_path = current.join("AppxManifest.xml");
        if manifest_path.is_file() {
            let metadata = fs::metadata(&manifest_path).ok()?;
            if metadata.len() > MAX_MANIFEST_BYTES {
                return None;
            }
            let manifest = fs::read_to_string(manifest_path).ok()?;
            return xml_attribute(&manifest, "BackgroundColor")
                .filter(|value| valid_manifest_background(value));
        }
        directory = current.parent();
    }
    None
}

fn xml_attribute(document: &str, attribute: &str) -> Option<String> {
    let lowercase = document.to_ascii_lowercase();
    let attribute = attribute.to_ascii_lowercase();
    let mut offset = 0;
    while let Some(relative) = lowercase[offset..].find(&attribute) {
        let start = offset + relative + attribute.len();
        let remainder = document[start..].trim_start();
        let remainder = remainder.strip_prefix('=')?.trim_start();
        let quote = remainder.chars().next()?;
        if quote != '"' && quote != '\'' {
            offset = start;
            continue;
        }
        let value = &remainder[quote.len_utf8()..];
        let end = value.find(quote)?;
        return Some(value[..end].trim().to_string());
    }
    None
}

fn valid_manifest_background(value: &str) -> bool {
    if value.eq_ignore_ascii_case("transparent") {
        return true;
    }
    value.strip_prefix('#').is_some_and(|hex| {
        matches!(hex.len(), 6 | 8) && hex.bytes().all(|byte| byte.is_ascii_hexdigit())
    })
}

fn icon_candidate_score(path: &Path) -> (u8, Reverse<u64>, usize, String) {
    let name = path
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    let extension = path
        .extension()
        .and_then(|value| value.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    let target_size_256 = name.contains("targetsize-256") || name.contains("targetsize_256");
    let rank = if target_size_256 && !name.contains("altform-") {
        0
    } else if target_size_256 && name.contains("altform-lightunplated") {
        1
    } else if target_size_256 {
        2
    } else if name.contains("scale-400") || name.contains("scale-200") {
        3
    } else if extension == "ico" {
        4
    } else if name.contains("square44x44logo") {
        8
    } else if name.contains("logo") || name.contains("icon") {
        5
    } else if name.ends_with(".exe") {
        6
    } else {
        7
    };
    let bytes = fs::metadata(path)
        .map(|metadata| metadata.len())
        .unwrap_or_default();
    (rank, Reverse(bytes), name.len(), name)
}

fn decode_icons(pending: &[PendingIcon]) -> HashMap<String, Vec<u8>> {
    if pending.is_empty() {
        return HashMap::new();
    }
    let requests = pending
        .iter()
        .map(|pending| IconDecodeRequest {
            source: pending.source.to_string_lossy().into_owned(),
            background: pending.background.clone(),
        })
        .collect::<Vec<_>>();
    let Ok(input) = serde_json::to_vec(&requests) else {
        return HashMap::new();
    };
    let mut command = Command::new("powershell.exe");
    configure_background_process(&mut command);
    let Ok(mut child) = command
        .args([
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            EXTRACT_ICON_SCRIPT,
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
    else {
        return HashMap::new();
    };
    let Some(mut stdin) = child.stdin.take() else {
        return HashMap::new();
    };
    if stdin.write_all(&input).is_err() {
        return HashMap::new();
    }
    drop(stdin);
    let Ok(output) = child.wait_with_output() else {
        return HashMap::new();
    };
    if !output.status.success() {
        log::warn!(
            "application_icon_decode_failed reason=process_exit requested={} status={:?} stdout_bytes={} stderr_bytes={}",
            pending.len(),
            output.status.code(),
            output.stdout.len(),
            output.stderr.len()
        );
        return HashMap::new();
    }
    let decoded = match serde_json::from_slice::<Vec<IconDecodeResult>>(&output.stdout) {
        Ok(decoded) => decoded,
        Err(error) => {
            log::warn!(
                "application_icon_decode_failed reason=invalid_output requested={} stdout_bytes={} stderr_bytes={} error={error}",
                pending.len(),
                output.stdout.len(),
                output.stderr.len()
            );
            return HashMap::new();
        }
    };
    let valid = decoded
        .into_iter()
        .filter_map(|decoded| {
            let png = STANDARD.decode(decoded.data).ok()?;
            (png.len() as u64 <= MAX_ICON_BYTES && png.starts_with(b"\x89PNG\r\n\x1a\n"))
                .then_some((decoded.source, png))
        })
        .collect::<HashMap<_, _>>();
    if !output.stderr.is_empty() {
        log::warn!(
            "application_icon_decode_limited reason=script_stderr requested={} resolved={} stderr_bytes={}",
            pending.len(),
            valid.len(),
            output.stderr.len()
        );
    }
    valid
}

#[cfg(test)]
mod tests {
    use std::{env, fs};

    use base64::{engine::general_purpose::STANDARD, Engine as _};

    #[test]
    fn appx_manifest_background_is_applied_only_to_tile_style_assets() {
        let root = env::temp_dir().join(format!(
            "mangodisk-appx-icon-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("system time should follow the Unix epoch")
                .as_nanos()
        ));
        let declared_asset = root.join("Assets").join("Square150x150Logo.png");
        let variant_asset = root.join("Assets").join("Square150x150Logo.scale-200.png");
        fs::create_dir_all(declared_asset.parent().expect("asset must have a parent"))
            .expect("asset directory should be created");
        fs::write(&variant_asset, b"png fixture").expect("asset fixture should be written");
        fs::write(
            root.join("AppxManifest.xml"),
            r##"<Application><VisualElements BackgroundColor="#3143FF" /></Application>"##,
        )
        .expect("manifest fixture should be written");

        assert_eq!(
            super::appx_manifest_background(&variant_asset).as_deref(),
            Some("#3143FF")
        );
        let resolved = super::resolve_icon_source(&declared_asset)
            .expect("the declared package icon should resolve to its scale variant");
        assert_eq!(resolved.source, variant_asset);
        assert_eq!(resolved.background.as_deref(), Some("#3143FF"));

        fs::write(
            root.join("AppxManifest.xml"),
            r#"<Application><VisualElements BackgroundColor="not a color" /></Application>"#,
        )
        .expect("manifest fixture should be updated");
        assert_eq!(super::appx_manifest_background(&resolved.source), None);
        fs::remove_dir_all(root).expect("fixture should be removed");
    }

    #[test]
    fn appx_app_list_variant_uses_manifest_background_only_for_plated_assets() {
        let root = env::temp_dir().join(format!(
            "mangodisk-appx-app-list-icon-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("system time should follow the Unix epoch")
                .as_nanos()
        ));
        let declared_asset = root.join("Assets").join("AppList.png");
        let plated = root.join("Assets").join("AppList.targetsize-256.png");
        let light_unplated = root
            .join("Assets")
            .join("AppList.targetsize-256_altform-lightunplated.png");
        let unplated = root
            .join("Assets")
            .join("AppList.targetsize-256_altform-unplated.png");
        fs::create_dir_all(declared_asset.parent().expect("asset must have a parent"))
            .expect("asset directory should be created");
        fs::write(&declared_asset, b"base").expect("base asset fixture should be written");
        fs::write(&plated, b"plated").expect("plated asset fixture should be written");
        fs::write(&light_unplated, b"light").expect("light asset fixture should be written");
        fs::write(&unplated, b"unplated").expect("unplated asset fixture should be written");
        fs::write(
            root.join("AppxManifest.xml"),
            r##"<Application><VisualElements BackgroundColor="#0078D4" /></Application>"##,
        )
        .expect("manifest fixture should be written");

        let resolved = super::resolve_icon_source(&declared_asset)
            .expect("the app-list icon should resolve to a target-size variant");
        assert_eq!(resolved.source, plated);
        assert_eq!(resolved.background.as_deref(), Some("#0078D4"));

        fs::remove_file(&resolved.source).expect("plated fixture should be removed");
        let resolved = super::resolve_icon_source(&declared_asset)
            .expect("the app-list icon should fall back to its light unplated variant");
        assert_eq!(resolved.source, light_unplated);
        assert_eq!(resolved.background, None);

        fs::remove_dir_all(root).expect("fixture should be removed");
    }

    #[test]
    #[ignore = "extracts a native icon through Windows PowerShell"]
    fn native_icons_are_decoded_and_reused_from_disk_cache() {
        let source = env::var("SystemRoot")
            .map(|root| format!(r"{root}\System32\notepad.exe"))
            .expect("SystemRoot should be available");
        let cache_root =
            env::temp_dir().join(format!("mangodisk-icon-validation-{}", std::process::id()));
        let first = super::load(vec![source.clone(); 32], Some(cache_root.clone()));
        assert_eq!(first.icons.len(), 32);
        assert_eq!(first.decoded_icons, 32);

        let second = super::load(vec![source], Some(cache_root.clone()));
        assert_eq!(second.icons.len(), 1);
        assert_eq!(second.cache_hits, 1);
        let _ = fs::remove_dir_all(cache_root);
    }

    #[test]
    fn unicode_icon_paths_are_decoded_in_the_default_test_suite() {
        let root = env::temp_dir().join(format!(
            "mangodisk-unicode-icon-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("system time should follow the Unix epoch")
                .as_nanos()
        ));
        let ascii_icon = root.join("ascii-icon.png");
        let unicode_icon = root
            .join("\u{56fe}\u{6807}\u{9a8c}\u{8bc1}")
            .join("\u{5e94}\u{7528}\u{56fe}\u{6807}.png");
        fs::create_dir_all(
            unicode_icon
                .parent()
                .expect("Unicode icon should have a parent"),
        )
        .expect("Unicode fixture directory should be created");
        // A real PNG keeps the test independent from installed applications
        // while exercising the same PowerShell JSON transport and image path.
        let png = STANDARD
            .decode("iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mNk+A8AAQUBAScY42YAAAAASUVORK5CYII=")
            .expect("embedded PNG fixture should be valid base64");
        fs::write(&ascii_icon, &png).expect("ASCII icon fixture should be written");
        fs::write(&unicode_icon, &png).expect("Unicode icon fixture should be written");

        let result = super::load(
            vec![
                ascii_icon.to_string_lossy().into_owned(),
                unicode_icon.to_string_lossy().into_owned(),
            ],
            None,
        );

        assert_eq!(result.icons.len(), 2);
        assert_eq!(result.decoded_icons, 2);
        fs::remove_dir_all(root).expect("Unicode icon fixture should be removed");
    }
}

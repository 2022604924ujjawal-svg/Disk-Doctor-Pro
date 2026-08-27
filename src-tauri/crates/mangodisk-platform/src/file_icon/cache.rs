use std::{
    fs,
    path::{Path, PathBuf},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use super::IconQuery;

const CACHE_SCHEMA: &[u8] = b"mangodisk-native-file-icon-v1";
const PNG_SIGNATURE: &[u8] = b"\x89PNG\r\n\x1a\n";
// Maintenance is intentionally throttled, so this target can be exceeded by
// icons written between maintenance passes and is restored on the next pass.
const MAX_CACHE_FILES: usize = 512;
const MAX_PNG_BYTES: u64 = 1024 * 1024;
const CACHE_MAX_AGE: Duration = Duration::from_secs(7 * 24 * 60 * 60);
const MAINTENANCE_MARKER_FILE_NAME: &str = ".maintenance-v1";
const MAINTENANCE_INTERVAL: Duration = Duration::from_secs(60);

pub(super) struct CacheLookup {
    pub key: String,
    pub png: Option<Vec<u8>>,
}

pub(super) struct FileIconCache {
    root: Option<PathBuf>,
}

impl FileIconCache {
    pub fn new(root: Option<PathBuf>) -> Self {
        let root = root.filter(|path| match fs::create_dir_all(path) {
            Ok(()) => true,
            Err(error) => {
                log::warn!("file_icon_cache_unavailable error={error}");
                false
            }
        });
        let cache = Self { root };
        cache.prune_if_due();
        cache
    }

    pub fn lookup(&self, query: &IconQuery, provider_variant: &[u8]) -> CacheLookup {
        let key = cache_key(query, provider_variant);
        let png = self.root.as_ref().and_then(|root| {
            let path = root.join(format!("{key}.png"));
            let metadata = fs::metadata(&path).ok()?;
            let modified = metadata.modified().ok()?;
            let fresh = SystemTime::now()
                .duration_since(modified)
                .map(|age| age <= CACHE_MAX_AGE)
                .unwrap_or(false);
            if !metadata.is_file() || metadata.len() > MAX_PNG_BYTES || !fresh {
                let _ = fs::remove_file(&path);
                return None;
            }
            let bytes = fs::read(&path).ok()?;
            if !valid_png(&bytes) {
                let _ = fs::remove_file(path);
                return None;
            }
            Some(bytes)
        });
        CacheLookup { key, png }
    }

    pub fn store(&self, key: &str, png: &[u8]) {
        let Some(root) = &self.root else {
            return;
        };
        if !valid_png(png) {
            return;
        }

        let destination = root.join(format!("{key}.png"));
        if destination.is_file() {
            return;
        }
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|duration| duration.as_nanos())
            .unwrap_or_default();
        let temporary = root.join(format!(".{key}-{}-{nonce}.tmp", std::process::id()));
        if let Err(error) =
            fs::write(&temporary, png).and_then(|_| fs::rename(&temporary, destination))
        {
            let _ = fs::remove_file(temporary);
            log::debug!("file_icon_cache_write_failed error={error}");
        }
    }

    fn prune_if_due(&self) {
        let Some(root) = &self.root else {
            return;
        };
        let marker = root.join(MAINTENANCE_MARKER_FILE_NAME);
        let maintenance_is_fresh = fs::metadata(&marker)
            .and_then(|metadata| metadata.modified())
            .ok()
            .and_then(|modified| SystemTime::now().duration_since(modified).ok())
            .is_some_and(|age| age < MAINTENANCE_INTERVAL);
        if maintenance_is_fresh {
            return;
        }

        self.prune();
        // The marker avoids a full directory walk for every icon IPC batch.
        // If writing it fails, later requests safely fall back to pruning more
        // often instead of allowing maintenance to stop indefinitely.
        if let Err(error) = fs::write(marker, []) {
            log::debug!("file_icon_cache_maintenance_marker_write_failed error={error}");
        }
    }

    fn prune(&self) {
        let Some(root) = &self.root else {
            return;
        };
        let Ok(entries) = fs::read_dir(root) else {
            return;
        };
        let now = SystemTime::now();
        let mut cached_files = entries
            .filter_map(Result::ok)
            .filter_map(|entry| {
                let path = entry.path();
                if path.extension().is_none_or(|extension| extension != "png") {
                    return None;
                }
                let metadata = entry.metadata().ok()?;
                let modified = metadata.modified().ok()?;
                let expired = now
                    .duration_since(modified)
                    .map(|age| age > CACHE_MAX_AGE)
                    .unwrap_or(true);
                if expired || metadata.len() > MAX_PNG_BYTES {
                    let _ = fs::remove_file(path);
                    return None;
                }
                Some((modified, path))
            })
            .collect::<Vec<_>>();
        if cached_files.len() <= MAX_CACHE_FILES {
            return;
        }
        cached_files.sort_by_key(|(modified, _)| *modified);
        let remove_count = cached_files.len() - MAX_CACHE_FILES;
        for (_, path) in cached_files.into_iter().take(remove_count) {
            if let Err(error) = fs::remove_file(path) {
                log::debug!("file_icon_cache_prune_failed error={error}");
            }
        }
    }
}

fn cache_key(query: &IconQuery, provider_variant: &[u8]) -> String {
    let mut hasher = blake3::Hasher::new();
    hasher.update(CACHE_SCHEMA);
    hasher.update(query.key().as_bytes());
    hasher.update(provider_variant);
    if let IconQuery::Path { path, .. } = query {
        add_path_metadata(&mut hasher, path);
    }
    hasher.finalize().to_hex().to_string()
}

fn add_path_metadata(hasher: &mut blake3::Hasher, path: &Path) {
    let Ok(metadata) = fs::metadata(path) else {
        return;
    };
    hasher.update(&metadata.len().to_le_bytes());
    if let Ok(modified) = metadata.modified().and_then(|value| {
        value
            .duration_since(UNIX_EPOCH)
            .map_err(std::io::Error::other)
    }) {
        hasher.update(&modified.as_secs().to_le_bytes());
        hasher.update(&modified.subsec_nanos().to_le_bytes());
    }
}

pub(super) fn valid_png(bytes: &[u8]) -> bool {
    if !bytes.starts_with(PNG_SIGNATURE) || bytes.len() as u64 > MAX_PNG_BYTES {
        return false;
    }

    let mut offset = PNG_SIGNATURE.len();
    let mut first_chunk = true;
    while offset.checked_add(12).is_some_and(|end| end <= bytes.len()) {
        let length = u32::from_be_bytes([
            bytes[offset],
            bytes[offset + 1],
            bytes[offset + 2],
            bytes[offset + 3],
        ]) as usize;
        let chunk_end = match offset
            .checked_add(12)
            .and_then(|base| base.checked_add(length))
        {
            Some(end) if end <= bytes.len() => end,
            _ => return false,
        };
        let kind = &bytes[offset + 4..offset + 8];
        if first_chunk && (kind != b"IHDR" || length != 13) {
            return false;
        }
        if kind == b"IEND" {
            return length == 0 && chunk_end == bytes.len();
        }
        first_chunk = false;
        offset = chunk_end;
    }
    false
}

#[cfg(test)]
mod tests {
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::*;

    #[test]
    fn type_cache_keys_share_provider_identity() {
        let first = IconQuery::Type {
            key: "ext:pdf".to_string(),
            extension: Some("pdf".to_string()),
        };
        let second = IconQuery::Type {
            key: "ext:pdf".to_string(),
            extension: Some("pdf".to_string()),
        };
        assert_eq!(
            cache_key(&first, b"preview"),
            cache_key(&second, b"preview")
        );
        assert_ne!(
            cache_key(&first, b"preview"),
            cache_key(&second, b"acrobat")
        );
    }

    #[test]
    fn truncated_png_is_not_accepted_as_a_cache_hit() {
        let mut truncated = PNG_SIGNATURE.to_vec();
        truncated.extend_from_slice(b"not-a-complete-png");
        assert!(!valid_png(&truncated));
    }

    #[test]
    fn invalid_cache_entry_is_removed_before_refresh() {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock should follow Unix epoch")
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "mangodisk-file-icon-cache-test-{}-{nonce}",
            std::process::id()
        ));
        let query = IconQuery::Type {
            key: "ext:test".to_string(),
            extension: Some("test".to_string()),
        };
        let cache = FileIconCache::new(Some(root.clone()));
        let lookup = cache.lookup(&query, b"provider");
        let invalid_path = root.join(format!("{}.png", lookup.key));
        fs::write(&invalid_path, PNG_SIGNATURE).expect("invalid fixture should be writable");

        assert!(cache.lookup(&query, b"provider").png.is_none());
        assert!(!invalid_path.exists());
        if let Err(error) = fs::remove_dir_all(root) {
            log::debug!("file_icon_cache_test_cleanup_failed error={error}");
        }
    }

    #[test]
    fn skips_repeated_pruning_during_the_maintenance_interval() {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock should follow the Unix epoch")
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "mangodisk-file-icon-maintenance-test-{}-{nonce}",
            std::process::id()
        ));
        fs::create_dir_all(&root).expect("test cache root must be created");
        for index in 0..=MAX_CACHE_FILES {
            fs::write(root.join(format!("{index:04}.png")), minimal_png())
                .expect("cache fixture must be written");
        }

        let _first = FileIconCache::new(Some(root.clone()));
        assert_eq!(cached_png_count(&root), MAX_CACHE_FILES);
        fs::write(root.join("new.png"), minimal_png())
            .expect("additional cache fixture must be written");

        let _second = FileIconCache::new(Some(root.clone()));
        assert_eq!(cached_png_count(&root), MAX_CACHE_FILES + 1);
        assert!(root.join(MAINTENANCE_MARKER_FILE_NAME).is_file());
        fs::remove_dir_all(root).expect("test cache root must be removed");
    }

    fn cached_png_count(root: &Path) -> usize {
        fs::read_dir(root)
            .expect("cache root must be readable")
            .filter_map(Result::ok)
            .filter(|entry| entry.path().extension().is_some_and(|value| value == "png"))
            .count()
    }

    fn minimal_png() -> &'static [u8] {
        b"\x89PNG\r\n\x1a\n\x00\x00\x00\rIHDR\
          \x00\x00\x00\x01\x00\x00\x00\x01\x08\x06\x00\x00\x00\
          \x1f\x15\xc4\x89\x00\x00\x00\x00IEND\xaeB`\x82"
    }
}

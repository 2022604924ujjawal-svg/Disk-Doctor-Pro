use std::{
    fs,
    path::{Path, PathBuf},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

// Version 5 restores manifest backgrounds for plated AppX target-size assets while keeping
// explicitly unplated variants transparent. Decoder semantics remain in the identity so stale
// low-contrast images are replaced automatically.
const CACHE_SCHEMA: &[u8] = b"mangodisk-application-icon-v5";
const PNG_SIGNATURE: &[u8] = b"\x89PNG\r\n\x1a\n";
// Maintenance is intentionally throttled, so this target can be exceeded by
// icons written between maintenance passes and is restored on the next pass.
const MAX_CACHE_FILES: usize = 512;
const MAX_CACHED_PNG_BYTES: u64 = 2 * 1024 * 1024;
const MAINTENANCE_MARKER_FILE_NAME: &str = ".maintenance-v1";
const MAINTENANCE_INTERVAL: Duration = Duration::from_secs(60);

pub(super) struct CacheLookup {
    pub key: String,
    pub png: Option<Vec<u8>>,
}

/// Stores decoded PNG icons outside operational data. Cache keys combine the
/// canonical source identity and metadata, so an updated application icon is
/// decoded again without requiring a separate invalidation database.
pub(super) struct ApplicationIconCache {
    root: Option<PathBuf>,
}

impl ApplicationIconCache {
    pub fn new(root: Option<PathBuf>) -> Self {
        let root = root.filter(|path| match fs::create_dir_all(path) {
            Ok(()) => true,
            Err(error) => {
                log::warn!("application_icon_cache_unavailable error={error}");
                false
            }
        });
        let cache = Self { root };
        cache.prune_if_due();
        cache
    }

    pub fn lookup(&self, source: &Path, variant: &[u8]) -> Option<CacheLookup> {
        let key = cache_key(source, variant)?;
        let Some(root) = &self.root else {
            return Some(CacheLookup { key, png: None });
        };
        let path = root.join(format!("{key}.png"));
        let png = fs::metadata(&path)
            .ok()
            .filter(|metadata| metadata.is_file() && metadata.len() <= MAX_CACHED_PNG_BYTES)
            .and_then(|_| fs::read(path).ok())
            .filter(|bytes| bytes.starts_with(PNG_SIGNATURE));
        Some(CacheLookup { key, png })
    }

    pub fn store(&self, key: &str, png: &[u8]) {
        let Some(root) = &self.root else {
            return;
        };
        if !png.starts_with(PNG_SIGNATURE) || png.len() as u64 > MAX_CACHED_PNG_BYTES {
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
            log::debug!("application_icon_cache_write_failed error={error}");
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
        // The marker makes maintenance persistent across independent Tauri
        // command batches and short app restarts. A failed marker write merely
        // falls back to the previous, more conservative pruning frequency.
        if let Err(error) = fs::write(marker, []) {
            log::debug!("application_icon_cache_maintenance_marker_write_failed error={error}");
        }
    }

    fn prune(&self) {
        let Some(root) = &self.root else {
            return;
        };
        let Ok(entries) = fs::read_dir(root) else {
            return;
        };
        let mut cached_files = entries
            .filter_map(Result::ok)
            .filter_map(|entry| {
                let path = entry.path();
                if path.extension().is_none_or(|extension| extension != "png") {
                    return None;
                }
                let modified = entry.metadata().ok()?.modified().ok()?;
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
                log::debug!("application_icon_cache_prune_failed error={error}");
            }
        }
    }
}

fn cache_key(source: &Path, variant: &[u8]) -> Option<String> {
    let metadata = fs::metadata(source).ok()?;
    let modified = metadata.modified().ok()?.duration_since(UNIX_EPOCH).ok()?;
    let mut hasher = blake3::Hasher::new();
    hasher.update(CACHE_SCHEMA);
    hasher.update(source.as_os_str().as_encoded_bytes());
    hasher.update(&metadata.len().to_le_bytes());
    hasher.update(&modified.as_secs().to_le_bytes());
    hasher.update(&modified.subsec_nanos().to_le_bytes());
    hasher.update(variant);
    Some(hasher.finalize().to_hex().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_root(name: &str) -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock must follow the Unix epoch")
            .as_nanos();
        std::env::temp_dir().join(format!(
            "mangodisk-icon-cache-{name}-{}-{nonce}",
            std::process::id()
        ))
    }

    #[test]
    fn reuses_cached_png_for_unchanged_source_metadata() {
        let root = test_root("reuse");
        let source = root.join("source.icns");
        fs::create_dir_all(&root).expect("test cache root must be created");
        fs::write(&source, b"source").expect("test source must be written");
        let cache = ApplicationIconCache::new(Some(root.join("cache")));

        let first = cache
            .lookup(&source, &[])
            .expect("source metadata must produce a cache key");
        assert!(first.png.is_none());
        let png = b"\x89PNG\r\n\x1a\ntest";
        cache.store(&first.key, png);
        let second = cache
            .lookup(&source, &[])
            .expect("source metadata must still be readable");

        assert_eq!(second.key, first.key);
        assert_eq!(second.png.as_deref(), Some(png.as_slice()));
        fs::remove_dir_all(root).expect("test cache root must be removed");
    }

    #[test]
    fn invalidates_cached_png_when_source_metadata_changes() {
        let root = test_root("invalidate");
        let source = root.join("source.icns");
        fs::create_dir_all(&root).expect("test cache root must be created");
        fs::write(&source, b"first").expect("test source must be written");
        let cache = ApplicationIconCache::new(Some(root.join("cache")));
        let first = cache
            .lookup(&source, &[])
            .expect("source metadata must produce a cache key");
        cache.store(&first.key, b"\x89PNG\r\n\x1a\ntest");

        fs::write(&source, b"different-size").expect("test source must be updated");
        let second = cache
            .lookup(&source, &[])
            .expect("updated source metadata must be readable");

        assert_ne!(second.key, first.key);
        assert!(second.png.is_none());
        fs::remove_dir_all(root).expect("test cache root must be removed");
    }

    #[test]
    fn decoder_variant_participates_in_the_cache_identity() {
        let root = test_root("variant");
        let source = root.join("source.png");
        fs::create_dir_all(&root).expect("test cache root must be created");
        fs::write(&source, b"source").expect("test source must be written");
        let cache = ApplicationIconCache::new(Some(root.join("cache")));

        let transparent = cache
            .lookup(&source, b"transparent")
            .expect("source metadata must produce a cache key");
        let branded = cache
            .lookup(&source, b"#3143FF")
            .expect("source metadata must produce a cache key");

        assert_ne!(transparent.key, branded.key);
        fs::remove_dir_all(root).expect("test cache root must be removed");
    }

    #[test]
    fn rejects_corrupt_cached_png_data() {
        let root = test_root("corrupt");
        let source = root.join("source.icns");
        let cache_root = root.join("cache");
        fs::create_dir_all(&root).expect("test cache root must be created");
        fs::write(&source, b"source").expect("test source must be written");
        let cache = ApplicationIconCache::new(Some(cache_root.clone()));
        let lookup = cache
            .lookup(&source, &[])
            .expect("source metadata must produce a cache key");
        fs::write(cache_root.join(format!("{}.png", lookup.key)), b"not-a-png")
            .expect("corrupt cache fixture must be written");

        let corrupt = cache
            .lookup(&source, &[])
            .expect("source metadata must remain readable");

        assert!(corrupt.png.is_none());
        fs::remove_dir_all(root).expect("test cache root must be removed");
    }

    #[test]
    fn skips_repeated_pruning_during_the_maintenance_interval() {
        let root = test_root("maintenance-interval");
        let cache_root = root.join("cache");
        fs::create_dir_all(&cache_root).expect("test cache root must be created");
        for index in 0..=MAX_CACHE_FILES {
            fs::write(cache_root.join(format!("{index:04}.png")), PNG_SIGNATURE)
                .expect("cache fixture must be written");
        }

        let _first = ApplicationIconCache::new(Some(cache_root.clone()));
        assert_eq!(cached_png_count(&cache_root), MAX_CACHE_FILES);
        fs::write(cache_root.join("new.png"), PNG_SIGNATURE)
            .expect("additional cache fixture must be written");

        let _second = ApplicationIconCache::new(Some(cache_root.clone()));
        assert_eq!(cached_png_count(&cache_root), MAX_CACHE_FILES + 1);
        assert!(cache_root.join(MAINTENANCE_MARKER_FILE_NAME).is_file());
        fs::remove_dir_all(root).expect("test cache root must be removed");
    }

    fn cached_png_count(root: &Path) -> usize {
        fs::read_dir(root)
            .expect("cache root must be readable")
            .filter_map(Result::ok)
            .filter(|entry| entry.path().extension().is_some_and(|value| value == "png"))
            .count()
    }
}

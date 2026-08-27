use std::{fs::File, io::Read, path::Path};

const MAX_PACKAGE_MARKER_BYTES: u64 = 1024 * 1024;

/// Hashes package-manager markers without trusting their on-disk size.
///
/// Scoop and Chocolatey metadata can live in directories writable by the
/// package owner. Reading through a bounded stream prevents a malformed marker
/// from forcing an unbounded allocation during inventory or uninstall
/// revalidation. The extra byte distinguishes an exact-limit file from an
/// oversized file without changing the established digest format.
pub(super) fn file_set_digest(paths: &[&Path]) -> Option<String> {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"mangodisk-windows-package-marker-v1");
    for path in paths {
        let contents = read_bounded_marker(path)?;
        hasher.update(path.file_name()?.to_string_lossy().as_bytes());
        hasher.update(&u64::try_from(contents.len()).ok()?.to_le_bytes());
        hasher.update(&contents);
    }
    Some(hasher.finalize().to_hex().to_string())
}

fn read_bounded_marker(path: &Path) -> Option<Vec<u8>> {
    let file = File::open(path).ok()?;
    if file.metadata().ok()?.len() > MAX_PACKAGE_MARKER_BYTES {
        return None;
    }
    let mut contents = Vec::with_capacity(64 * 1024);
    file.take(MAX_PACKAGE_MARKER_BYTES + 1)
        .read_to_end(&mut contents)
        .ok()?;
    (u64::try_from(contents.len()).ok()? <= MAX_PACKAGE_MARKER_BYTES).then_some(contents)
}

#[cfg(test)]
mod tests {
    use std::{fs, process};

    use super::*;

    #[test]
    fn package_marker_digest_rejects_oversized_files() {
        let root = std::env::temp_dir().join(format!(
            "mangodisk-package-marker-limit-{}-{}",
            process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("the system clock should follow the Unix epoch")
                .as_nanos()
        ));
        fs::create_dir_all(&root).expect("the package marker fixture should be created");
        let acceptable = root.join("acceptable.json");
        let oversized = root.join("oversized.json");
        fs::write(&acceptable, b"fixture")
            .expect("the acceptable package marker should be written");
        fs::write(
            &oversized,
            vec![b'x'; usize::try_from(MAX_PACKAGE_MARKER_BYTES).unwrap() + 1],
        )
        .expect("the oversized package marker should be written");

        assert!(file_set_digest(&[&acceptable]).is_some());
        assert!(file_set_digest(&[&oversized]).is_none());

        fs::remove_dir_all(root).expect("the package marker fixture should be removed");
    }
}

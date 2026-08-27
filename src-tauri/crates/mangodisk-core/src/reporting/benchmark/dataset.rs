use std::{
    collections::BTreeMap,
    fs::{self, File, OpenOptions},
    io::{Read, Seek, SeekFrom, Write},
    path::{Component, Path, PathBuf},
};

use serde::{Deserialize, Serialize};

const DATASET_SCHEMA_VERSION: &str = "1.0";
const DATASET_VERSION: &str = "fixed-v1";
const DEFAULT_SEED: u64 = 20_260_717;
const MAX_DATASET_PARENT_LENGTH: usize = 4_096;
const DENSE_BUFFER_BYTES: usize = 64 * 1024;
const SPARSE_MARKER_BYTES: usize = 4 * 1024;

#[derive(Debug)]
pub struct BenchmarkDatasetOptions {
    pub parent_directory: PathBuf,
    pub seed: Option<u64>,
    pub recreate: bool,
}

#[derive(Debug)]
pub struct BenchmarkDatasetArtifacts {
    pub dataset_directory: PathBuf,
    pub manifest_path: PathBuf,
    pub markdown_path: PathBuf,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BenchmarkDatasetManifest {
    pub schema_version: String,
    pub dataset_version: String,
    pub dataset_id: String,
    pub seed: u64,
    pub root_path: String,
    pub logical_digest: String,
    pub logical_file_count: u64,
    pub logical_directory_count: u64,
    pub logical_bytes: u64,
    #[serde(default)]
    pub allocated_bytes: Option<u64>,
    pub expected_large_file_count: u64,
    #[serde(default)]
    pub expected_large_file_bytes: u64,
    pub expected_duplicate_group_count: u64,
    pub expected_duplicate_file_count: u64,
    pub expected_reclaimable_bytes: u64,
    pub features: DatasetFeatureReport,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DatasetFeatureReport {
    pub sparse_files_created: u64,
    pub hard_links_created: u64,
    pub symbolic_links_created: u64,
    pub permission_restricted_directories: u64,
    pub unsupported_features: Vec<String>,
}

#[derive(Clone)]
enum FileRecipe {
    Dense {
        bytes: u64,
        stream: u64,
    },
    Sparse {
        bytes: u64,
        stream: u64,
    },
    SampleCollision {
        bytes: u64,
        shared_stream: u64,
        unique_stream: u64,
        patch_offset: u64,
    },
}

#[derive(Clone)]
struct PlannedFile {
    relative_path: String,
    recipe: FileRecipe,
    duplicate_group: Option<&'static str>,
}

impl PlannedFile {
    fn bytes(&self) -> u64 {
        match self.recipe {
            FileRecipe::Dense { bytes, .. }
            | FileRecipe::Sparse { bytes, .. }
            | FileRecipe::SampleCollision { bytes, .. } => bytes,
        }
    }
}

pub struct BenchmarkDatasetService;

impl BenchmarkDatasetService {
    /// Generates a deterministic dataset without depending on a source checkout. The generator
    /// always owns a fixed child directory under the requested parent and removes it only after
    /// validating the expected ownership manifest. Unsupported link or permission capabilities
    /// are recorded in the manifest without changing the cross-platform logical digest.
    pub fn generate(options: BenchmarkDatasetOptions) -> Result<BenchmarkDatasetArtifacts, String> {
        validate_options(&options)?;
        let seed = options.seed.unwrap_or(DEFAULT_SEED);
        let dataset_id = format!("mangodisk-{DATASET_VERSION}-seed-{seed}");
        let parent = absolute_directory(&options.parent_directory)?;
        let dataset_directory = parent.join(&dataset_id);
        let benchmark_root = dataset_directory.join("core");
        let manifest_path = dataset_directory.join("dataset-manifest.json");
        let markdown_path = dataset_directory.join("dataset-manifest.md");

        if dataset_directory.exists() {
            if !options.recreate {
                let manifest = read_manifest(&manifest_path)?;
                validate_existing_manifest(&manifest, &dataset_id, seed, &dataset_directory)?;
                return Ok(BenchmarkDatasetArtifacts {
                    dataset_directory,
                    manifest_path,
                    markdown_path,
                });
            }
            remove_owned_dataset(&dataset_directory, &manifest_path, &dataset_id)?;
        }

        fs::create_dir_all(&dataset_directory).map_err(|error| {
            format!(
                "failed to create benchmark dataset directory {}: {error}",
                dataset_directory.display()
            )
        })?;
        fs::create_dir_all(&benchmark_root).map_err(|error| {
            format!(
                "failed to create benchmark core directory {}: {error}",
                benchmark_root.display()
            )
        })?;

        let planned_files = planned_files(seed);
        let mut features = DatasetFeatureReport::default();
        let generation_result = generate_files(&benchmark_root, &planned_files, &mut features)
            .and_then(|_| {
                create_platform_features(&benchmark_root, &dataset_directory, &mut features)
            })
            .and_then(|_| {
                build_manifest(&dataset_id, seed, &benchmark_root, &planned_files, features)
            });
        let manifest = match generation_result {
            Ok(manifest) => manifest,
            Err(error) => {
                restore_restricted_directory(&dataset_directory);
                let _ = fs::remove_dir_all(&dataset_directory);
                return Err(error);
            }
        };

        if let Err(error) = write_json(&manifest_path, &manifest)
            .and_then(|_| write_text(&markdown_path, &render_markdown(&manifest)))
        {
            restore_restricted_directory(&dataset_directory);
            let _ = fs::remove_dir_all(&dataset_directory);
            return Err(error);
        }
        log::info!(
            "benchmark_dataset_generated dataset_id={} files={} directories={} logical_bytes={} unsupported_features={}",
            manifest.dataset_id,
            manifest.logical_file_count,
            manifest.logical_directory_count,
            manifest.logical_bytes,
            manifest.features.unsupported_features.len()
        );
        Ok(BenchmarkDatasetArtifacts {
            dataset_directory,
            manifest_path,
            markdown_path,
        })
    }

    pub fn read_manifest(path: &Path) -> Result<BenchmarkDatasetManifest, String> {
        let mut manifest = read_manifest(path)?;
        let directory = path
            .parent()
            .ok_or_else(|| "benchmark dataset manifest has no parent directory".to_string())?;
        let expected_id = format!(
            "mangodisk-{}-seed-{}",
            manifest.dataset_version, manifest.seed
        );
        let observed_allocated_bytes =
            validate_existing_manifest(&manifest, &expected_id, manifest.seed, directory)?;
        // Delayed allocation, compression, and security software can change the physical size of
        // sparse files. Record the value observed immediately before a run instead of retaining a
        // potentially stale generation-time snapshot.
        manifest.allocated_bytes = observed_allocated_bytes;
        Ok(manifest)
    }
}

fn validate_options(options: &BenchmarkDatasetOptions) -> Result<(), String> {
    let value = options.parent_directory.to_string_lossy();
    if value.trim().is_empty() {
        return Err("benchmark dataset parent directory cannot be empty".to_string());
    }
    if value.len() > MAX_DATASET_PARENT_LENGTH {
        return Err("benchmark dataset parent directory is too long".to_string());
    }
    if options
        .parent_directory
        .components()
        .any(|component| matches!(component, Component::ParentDir))
    {
        return Err("benchmark dataset parent directory cannot contain `..`".to_string());
    }
    Ok(())
}

fn absolute_directory(path: &Path) -> Result<PathBuf, String> {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .map_err(|error| format!("failed to read the current directory: {error}"))?
            .join(path)
    };
    fs::create_dir_all(&absolute)
        .map_err(|error| format!("failed to create {}: {error}", absolute.display()))?;
    fs::canonicalize(&absolute)
        .map_err(|error| format!("failed to canonicalize {}: {error}", absolute.display()))
}

fn planned_files(seed: u64) -> Vec<PlannedFile> {
    let mut files = Vec::new();
    for index in 0..512_u64 {
        files.push(PlannedFile {
            relative_path: format!("small-files/bucket-{:02}/item-{index:04}.bin", index % 16),
            recipe: FileRecipe::Dense {
                bytes: 1_024 + (index * 97 % 15_360),
                stream: seed ^ index.rotate_left(17),
            },
            duplicate_group: None,
        });
    }

    let mut deep_path = String::from("deep-tree");
    for depth in 0..24_u64 {
        deep_path.push_str(&format!("/level-{depth:02}"));
        files.push(PlannedFile {
            relative_path: format!("{deep_path}/depth-{depth:02}.dat"),
            recipe: FileRecipe::Dense {
                bytes: 2_048 + depth * 31,
                stream: seed ^ 0xD33F_0000 ^ depth,
            },
            duplicate_group: None,
        });
    }

    for index in 0..1_024_u64 {
        files.push(PlannedFile {
            relative_path: format!("flat-directory/flat-{index:05}.txt"),
            recipe: FileRecipe::Dense {
                bytes: 512 + index % 128,
                stream: seed ^ 0xF1A7_0000 ^ index,
            },
            duplicate_group: None,
        });
    }

    let large_specs = [
        ("large-files/archive-050.bin", 50 * 1024 * 1024, 0x50),
        ("large-files/archive-064.bin", 64 * 1024 * 1024, 0x64),
        ("large-files/archive-096.bin", 96 * 1024 * 1024, 0x96),
    ];
    for (path, bytes, stream) in large_specs {
        files.push(PlannedFile {
            relative_path: path.to_string(),
            recipe: FileRecipe::Sparse {
                bytes,
                stream: seed ^ stream,
            },
            duplicate_group: None,
        });
    }

    for path in [
        "duplicate-mix/group-a/original.bin",
        "duplicate-mix/group-a/copy-one.bin",
        "duplicate-mix/group-a/copy-two.bin",
    ]
    .into_iter()
    {
        files.push(PlannedFile {
            relative_path: path.to_string(),
            recipe: FileRecipe::Dense {
                bytes: 1024 * 1024,
                stream: seed ^ 0xA11C_E001,
            },
            duplicate_group: Some("group-a"),
        });
    }

    for path in [
        "duplicate-mix/group-large/original.bin",
        "duplicate-mix/group-large/copy.bin",
    ] {
        files.push(PlannedFile {
            relative_path: path.to_string(),
            recipe: FileRecipe::Sparse {
                bytes: 64 * 1024 * 1024,
                stream: seed ^ 0x1A26_E001,
            },
            duplicate_group: Some("group-large"),
        });
    }

    // These files have equal sizes and matching head and tail samples but different middle
    // content. They ensure sampled hashes are never treated as final evidence: all three must
    // reach the full-hash stage without forming a duplicate group.
    for index in 0..3_u64 {
        files.push(PlannedFile {
            relative_path: format!("duplicate-mix/sample-collision/item-{index}.bin"),
            recipe: FileRecipe::SampleCollision {
                bytes: 2 * 1024 * 1024,
                shared_stream: seed ^ 0xC011_1000,
                unique_stream: seed ^ 0xC011_2000 ^ index,
                patch_offset: 512 * 1024,
            },
            duplicate_group: None,
        });
    }

    for index in 0..4_u64 {
        files.push(PlannedFile {
            relative_path: format!("duplicate-mix/same-size-unique/item-{index}.bin"),
            recipe: FileRecipe::Dense {
                bytes: 768 * 1024,
                stream: seed ^ 0x51AE_0000 ^ index,
            },
            duplicate_group: None,
        });
    }

    for (index, name) in ["cache-sample.dat", "résumé-data.bin", "δεδομένα.bin"]
        .into_iter()
        .enumerate()
    {
        files.push(PlannedFile {
            relative_path: format!("unicode-names/{name}"),
            recipe: FileRecipe::Dense {
                bytes: 4_096 + index as u64,
                stream: seed ^ 0xC0DE_0000 ^ index as u64,
            },
            duplicate_group: None,
        });
    }
    files
}

fn generate_files(
    root: &Path,
    planned_files: &[PlannedFile],
    features: &mut DatasetFeatureReport,
) -> Result<(), String> {
    for planned in planned_files {
        let path = safe_join(root, &planned.relative_path)?;
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .map_err(|error| format!("failed to create {}: {error}", parent.display()))?;
        }
        match planned.recipe {
            FileRecipe::Dense { bytes, stream } => write_dense_file(&path, bytes, stream)?,
            FileRecipe::Sparse { bytes, stream } => {
                if write_sparse_file(&path, bytes, stream)? {
                    features.sparse_files_created += 1;
                } else if !features
                    .unsupported_features
                    .iter()
                    .any(|feature| feature == "sparse-file:platform-not-supported")
                {
                    features
                        .unsupported_features
                        .push("sparse-file:platform-not-supported".to_string());
                }
            }
            FileRecipe::SampleCollision {
                bytes,
                shared_stream,
                unique_stream,
                patch_offset,
            } => write_sample_collision_file(
                &path,
                bytes,
                shared_stream,
                unique_stream,
                patch_offset,
            )?,
        }
    }
    Ok(())
}

fn safe_join(root: &Path, relative: &str) -> Result<PathBuf, String> {
    let relative_path = Path::new(relative);
    if relative_path.is_absolute()
        || relative_path
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        return Err(format!(
            "unsafe benchmark dataset relative path: {relative}"
        ));
    }
    Ok(root.join(relative_path))
}

fn write_dense_file(path: &Path, bytes: u64, stream: u64) -> Result<(), String> {
    let mut file = File::create(path)
        .map_err(|error| format!("failed to create {}: {error}", path.display()))?;
    let mut remaining = bytes;
    let mut state = stream;
    let mut buffer = vec![0_u8; DENSE_BUFFER_BYTES];
    while remaining > 0 {
        let write_bytes = remaining.min(buffer.len() as u64) as usize;
        fill_deterministic(&mut buffer[..write_bytes], &mut state);
        file.write_all(&buffer[..write_bytes])
            .map_err(|error| format!("failed to write {}: {error}", path.display()))?;
        remaining -= write_bytes as u64;
    }
    Ok(())
}

fn write_sparse_file(path: &Path, bytes: u64, stream: u64) -> Result<bool, String> {
    let mut file = OpenOptions::new()
        .create_new(true)
        .read(true)
        .write(true)
        .open(path)
        .map_err(|error| format!("failed to create {}: {error}", path.display()))?;
    let sparse_enabled = enable_sparse_file(&file);
    file.set_len(bytes)
        .map_err(|error| format!("failed to resize {}: {error}", path.display()))?;
    let marker_bytes = (bytes.min(SPARSE_MARKER_BYTES as u64)) as usize;
    let offsets = [
        0,
        bytes.saturating_sub(marker_bytes as u64) / 2,
        bytes.saturating_sub(marker_bytes as u64),
    ];
    for (index, offset) in offsets.into_iter().enumerate() {
        let mut state = stream ^ (index as u64).rotate_left(23);
        let mut marker = vec![0_u8; marker_bytes];
        fill_deterministic(&mut marker, &mut state);
        file.seek(SeekFrom::Start(offset))
            .map_err(|error| format!("failed to seek in {}: {error}", path.display()))?;
        file.write_all(&marker)
            .map_err(|error| format!("failed to write {}: {error}", path.display()))?;
    }
    Ok(sparse_enabled)
}

#[cfg(unix)]
fn enable_sparse_file(_file: &File) -> bool {
    // Unix filesystems retain holes between positioned writes without an additional ioctl.
    true
}

#[cfg(windows)]
fn enable_sparse_file(file: &File) -> bool {
    use std::{os::windows::io::AsRawHandle, ptr};
    use windows_sys::Win32::System::{Ioctl::FSCTL_SET_SPARSE, IO::DeviceIoControl};

    let mut returned = 0_u32;
    // SAFETY: The file handle remains valid for the synchronous call. This control code needs no
    // input or output buffer, and `returned` is a valid writable pointer.
    let succeeded = unsafe {
        DeviceIoControl(
            file.as_raw_handle(),
            FSCTL_SET_SPARSE,
            ptr::null(),
            0,
            ptr::null_mut(),
            0,
            &mut returned,
            ptr::null_mut(),
        )
    } != 0;
    if !succeeded {
        // A dense fallback preserves logical content and correctness expectations. The manifest
        // records the missing sparse capability so platform differences are not mistaken for an
        // engine regression.
        log::warn!("benchmark_sparse_file_not_supported");
    }
    succeeded
}

#[cfg(not(any(unix, windows)))]
fn enable_sparse_file(_file: &File) -> bool {
    false
}

fn write_sample_collision_file(
    path: &Path,
    bytes: u64,
    shared_stream: u64,
    unique_stream: u64,
    patch_offset: u64,
) -> Result<(), String> {
    write_dense_file(path, bytes, shared_stream)?;
    let mut file = OpenOptions::new()
        .write(true)
        .open(path)
        .map_err(|error| format!("failed to open {}: {error}", path.display()))?;
    let patch_bytes = 4 * 1024;
    if patch_offset.saturating_add(patch_bytes as u64) > bytes {
        return Err(format!(
            "sample-collision patch is outside the file: {}",
            path.display()
        ));
    }
    let mut state = unique_stream;
    let mut patch = vec![0_u8; patch_bytes];
    fill_deterministic(&mut patch, &mut state);
    file.seek(SeekFrom::Start(patch_offset))
        .map_err(|error| format!("failed to seek in {}: {error}", path.display()))?;
    file.write_all(&patch)
        .map_err(|error| format!("failed to write {}: {error}", path.display()))
}

fn fill_deterministic(buffer: &mut [u8], state: &mut u64) {
    for byte in buffer {
        *state ^= *state << 13;
        *state ^= *state >> 7;
        *state ^= *state << 17;
        *byte = (*state >> 24) as u8;
    }
}

fn create_platform_features(
    benchmark_root: &Path,
    dataset_directory: &Path,
    features: &mut DatasetFeatureReport,
) -> Result<(), String> {
    // Boundary capabilities stay outside the performance scan root. Windows symbolic-link and ACL
    // support depends on system policy; including these entries in the core directory would make
    // file and skip counts for the same seed incomparable across machines.
    let boundary_root = dataset_directory.join("boundary-features");
    let hard_link_source = benchmark_root.join("duplicate-mix/group-a/original.bin");
    let hard_link = boundary_root.join("hard-link-to-group-a.bin");
    fs::create_dir_all(
        hard_link
            .parent()
            .ok_or_else(|| "hard-link fixture path has no parent directory".to_string())?,
    )
    .map_err(|error| format!("failed to create link fixture directory: {error}"))?;
    match fs::hard_link(&hard_link_source, &hard_link) {
        Ok(()) => features.hard_links_created = 1,
        Err(error) => features
            .unsupported_features
            .push(format!("hard-link:{error}")),
    }

    match create_symbolic_link(
        &benchmark_root.join("small-files/bucket-00/item-0000.bin"),
        &boundary_root.join("symbolic-link-to-small-file.bin"),
    ) {
        Ok(()) => features.symbolic_links_created = 1,
        Err(error) => features
            .unsupported_features
            .push(format!("symbolic-link:{error}")),
    }

    let restricted = boundary_root.join("restricted");
    fs::create_dir_all(&restricted)
        .map_err(|error| format!("failed to create permission fixture directory: {error}"))?;
    write_dense_file(
        &restricted.join("unreadable-content.bin"),
        8_192,
        0xA11C_7001,
    )?;
    match restrict_directory(&restricted) {
        Ok(()) => features.permission_restricted_directories = 1,
        Err(error) => features
            .unsupported_features
            .push(format!("restricted-directory:{error}")),
    }
    Ok(())
}

#[cfg(unix)]
fn create_symbolic_link(source: &Path, link: &Path) -> Result<(), String> {
    std::os::unix::fs::symlink(source, link).map_err(|error| error.to_string())
}

#[cfg(windows)]
fn create_symbolic_link(source: &Path, link: &Path) -> Result<(), String> {
    std::os::windows::fs::symlink_file(source, link).map_err(|error| error.to_string())
}

#[cfg(unix)]
fn restrict_directory(path: &Path) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o000)).map_err(|error| error.to_string())
}

#[cfg(windows)]
fn restrict_directory(_path: &Path) -> Result<(), String> {
    // Changing a Windows ACL requires explicit security-descriptor and inheritance handling. An
    // incorrect descriptor could prevent a standard user from removing the generated dataset, so
    // the manifest reports this capability as unsupported until a safe implementation exists.
    Err("windows-acl-not-configured".to_string())
}

fn build_manifest(
    dataset_id: &str,
    seed: u64,
    root: &Path,
    files: &[PlannedFile],
    features: DatasetFeatureReport,
) -> Result<BenchmarkDatasetManifest, String> {
    let logical_digest = actual_logical_digest(root, files)?;
    let logical_bytes = files.iter().map(PlannedFile::bytes).sum();
    let allocated_bytes = actual_allocated_bytes(root, files)?;
    let expected_large_file_count = files
        .iter()
        .filter(|file| file.bytes() >= 50 * 1024 * 1024)
        .count() as u64;
    let expected_large_file_bytes = files
        .iter()
        .filter(|file| file.bytes() >= 50 * 1024 * 1024)
        .map(PlannedFile::bytes)
        .sum();
    let mut duplicate_groups = BTreeMap::<&str, (u64, u64)>::new();
    for file in files {
        if let Some(group) = file.duplicate_group {
            let entry = duplicate_groups.entry(group).or_insert((0, file.bytes()));
            entry.0 += 1;
            if entry.1 != file.bytes() {
                return Err(format!(
                    "duplicate fixture group {group} contains files with different sizes"
                ));
            }
        }
    }
    let expected_duplicate_file_count = duplicate_groups.values().map(|(count, _)| *count).sum();
    let expected_reclaimable_bytes = duplicate_groups
        .values()
        .map(|(count, bytes)| bytes.saturating_mul(count.saturating_sub(1)))
        .sum();
    let logical_directory_count = logical_directories(files).len() as u64;
    Ok(BenchmarkDatasetManifest {
        schema_version: DATASET_SCHEMA_VERSION.to_string(),
        dataset_version: DATASET_VERSION.to_string(),
        dataset_id: dataset_id.to_string(),
        seed,
        root_path: root
            .to_str()
            .ok_or_else(|| "benchmark dataset path is not valid Unicode".to_string())?
            .to_string(),
        logical_digest,
        logical_file_count: files.len() as u64,
        logical_directory_count,
        logical_bytes,
        allocated_bytes,
        expected_large_file_count,
        expected_large_file_bytes,
        expected_duplicate_group_count: duplicate_groups.len() as u64,
        expected_duplicate_file_count,
        expected_reclaimable_bytes,
        features,
    })
}

fn actual_logical_digest(root: &Path, files: &[PlannedFile]) -> Result<String, String> {
    let mut ordered = files.to_vec();
    ordered.sort_by(|left, right| left.relative_path.cmp(&right.relative_path));
    let mut hasher = blake3::Hasher::new();
    hasher.update(DATASET_VERSION.as_bytes());
    for file in ordered {
        let path = safe_join(root, &file.relative_path)?;
        let metadata = fs::symlink_metadata(&path)
            .map_err(|error| format!("failed to validate {}: {error}", path.display()))?;
        if metadata.file_type().is_symlink()
            || !metadata.is_file()
            || metadata.len() != file.bytes()
        {
            return Err(format!(
                "benchmark dataset file size mismatch: {}, expected {}, observed {}",
                path.display(),
                file.bytes(),
                metadata.len()
            ));
        }
        let content_digest = file_digest(&path)?;
        hasher.update(file.relative_path.as_bytes());
        hasher.update(&[0]);
        hasher.update(&file.bytes().to_le_bytes());
        hasher.update(content_digest.as_bytes());
        hasher.update(&[0xff]);
    }
    Ok(hasher.finalize().to_hex().to_string())
}

fn file_digest(path: &Path) -> Result<String, String> {
    let mut file =
        File::open(path).map_err(|error| format!("failed to read {}: {error}", path.display()))?;
    let mut hasher = blake3::Hasher::new();
    let mut buffer = vec![0_u8; 1024 * 1024];
    loop {
        let read = file
            .read(&mut buffer)
            .map_err(|error| format!("failed to read {}: {error}", path.display()))?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(hasher.finalize().to_hex().to_string())
}

fn actual_allocated_bytes(root: &Path, files: &[PlannedFile]) -> Result<Option<u64>, String> {
    let mut total = 0_u64;
    for file in files {
        let path = safe_join(root, &file.relative_path)?;
        let Some(bytes) = file_allocated_bytes(&path)? else {
            return Ok(None);
        };
        total = total.saturating_add(bytes);
    }
    Ok(Some(total))
}

#[cfg(unix)]
fn file_allocated_bytes(path: &Path) -> Result<Option<u64>, String> {
    use std::os::unix::fs::MetadataExt;

    let metadata = fs::metadata(path).map_err(|error| {
        format!(
            "failed to read allocated size for {}: {error}",
            path.display()
        )
    })?;
    Ok(Some(metadata.blocks().saturating_mul(512)))
}

#[cfg(windows)]
fn file_allocated_bytes(path: &Path) -> Result<Option<u64>, String> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::{
        Foundation::{GetLastError, SetLastError, ERROR_SUCCESS},
        Storage::FileSystem::GetCompressedFileSizeW,
    };

    let wide = path
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect::<Vec<_>>();
    let mut high = 0_u32;
    // `INVALID_FILE_SIZE` can also be a valid low 32-bit value. A successful Windows call does not
    // guarantee that it clears the thread error, so reset the value first to distinguish valid
    // output from an API failure.
    // SAFETY: SetLastError modifies only the current thread error and has no extra preconditions.
    unsafe { SetLastError(ERROR_SUCCESS) };
    // SAFETY: `wide` remains valid and NUL-terminated for the call, and `high` points to writable
    // u32 storage.
    let low = unsafe { GetCompressedFileSizeW(wide.as_ptr(), &mut high) };
    if low == u32::MAX {
        // `INVALID_FILE_SIZE` can be a valid low value, so GetLastError determines whether the call
        // failed.
        // SAFETY: GetLastError only reads the current thread error and has no preconditions.
        let error = unsafe { GetLastError() };
        if error != ERROR_SUCCESS {
            return Err(format!(
                "failed to read allocated size for {}: Windows error {error}",
                path.display()
            ));
        }
    }
    Ok(Some(((high as u64) << 32) | low as u64))
}

#[cfg(not(any(unix, windows)))]
fn file_allocated_bytes(_path: &Path) -> Result<Option<u64>, String> {
    Ok(None)
}

fn logical_directories(files: &[PlannedFile]) -> Vec<String> {
    let mut directories = Vec::new();
    for file in files {
        let mut current = Path::new(&file.relative_path).parent();
        while let Some(path) = current {
            if path.as_os_str().is_empty() {
                break;
            }
            let value = path.to_string_lossy().replace('\\', "/");
            if !directories.contains(&value) {
                directories.push(value);
            }
            current = path.parent();
        }
    }
    directories.sort();
    directories
}

fn read_manifest(path: &Path) -> Result<BenchmarkDatasetManifest, String> {
    let bytes =
        fs::read(path).map_err(|error| format!("failed to read {}: {error}", path.display()))?;
    serde_json::from_slice(&bytes).map_err(|error| {
        format!(
            "failed to parse benchmark dataset manifest {}: {error}",
            path.display()
        )
    })
}

fn validate_existing_manifest(
    manifest: &BenchmarkDatasetManifest,
    dataset_id: &str,
    seed: u64,
    directory: &Path,
) -> Result<Option<u64>, String> {
    let expected = planned_files(seed);
    let expected_logical_bytes = expected.iter().map(PlannedFile::bytes).sum::<u64>();
    let expected_large_files = expected
        .iter()
        .filter(|file| file.bytes() >= 50 * 1024 * 1024)
        .collect::<Vec<_>>();
    let mut expected_duplicate_groups = BTreeMap::<&str, (u64, u64)>::new();
    for file in &expected {
        if let Some(group) = file.duplicate_group {
            let entry = expected_duplicate_groups
                .entry(group)
                .or_insert((0, file.bytes()));
            entry.0 += 1;
        }
    }
    let expected_duplicate_file_count = expected_duplicate_groups
        .values()
        .map(|(count, _)| *count)
        .sum::<u64>();
    let expected_reclaimable_bytes = expected_duplicate_groups
        .values()
        .map(|(count, bytes)| bytes.saturating_mul(count.saturating_sub(1)))
        .sum::<u64>();
    if manifest.schema_version != DATASET_SCHEMA_VERSION
        || manifest.dataset_version != DATASET_VERSION
        || manifest.dataset_id != dataset_id
        || manifest.seed != seed
        || manifest.logical_file_count != expected.len() as u64
        || manifest.logical_directory_count != logical_directories(&expected).len() as u64
        || manifest.logical_bytes != expected_logical_bytes
        || manifest.allocated_bytes.is_none()
        || manifest.expected_large_file_count != expected_large_files.len() as u64
        || manifest.expected_large_file_bytes
            != expected_large_files
                .iter()
                .map(|file| file.bytes())
                .sum::<u64>()
        || manifest.expected_duplicate_group_count != expected_duplicate_groups.len() as u64
        || manifest.expected_duplicate_file_count != expected_duplicate_file_count
        || manifest.expected_reclaimable_bytes != expected_reclaimable_bytes
    {
        return Err(format!(
            "existing dataset {} is incompatible with this generator; use --recreate to rebuild it explicitly",
            directory.display()
        ));
    }
    let root = PathBuf::from(&manifest.root_path);
    let root_metadata = fs::symlink_metadata(&root)
        .map_err(|error| format!("failed to validate the benchmark dataset root: {error}"))?;
    let canonical_directory = fs::canonicalize(directory).map_err(|error| {
        format!("failed to canonicalize the benchmark dataset directory: {error}")
    })?;
    let canonical_root = fs::canonicalize(&root)
        .map_err(|error| format!("failed to canonicalize the benchmark dataset root: {error}"))?;
    if root_metadata.file_type().is_symlink()
        || !root_metadata.is_dir()
        || canonical_root.parent() != Some(canonical_directory.as_path())
    {
        return Err(format!(
            "benchmark dataset {} has an invalid root boundary",
            directory.display()
        ));
    }
    let actual_digest = actual_logical_digest(&canonical_root, &expected)?;
    if manifest.logical_digest != actual_digest {
        return Err(format!(
            "benchmark dataset {} has changed; use --recreate to rebuild it explicitly",
            directory.display()
        ));
    }
    let observed_allocated_bytes = actual_allocated_bytes(&canonical_root, &expected)?;
    if manifest.allocated_bytes != observed_allocated_bytes {
        log::info!(
            "benchmark_dataset_allocation_changed dataset_id={} generated_bytes={:?} observed_bytes={:?}",
            manifest.dataset_id,
            manifest.allocated_bytes,
            observed_allocated_bytes
        );
    }
    Ok(observed_allocated_bytes)
}

fn remove_owned_dataset(
    directory: &Path,
    manifest_path: &Path,
    expected_id: &str,
) -> Result<(), String> {
    let manifest = read_manifest(manifest_path).map_err(|error| {
        format!(
            "refusing to remove dataset directory {} without a valid ownership manifest: {error}",
            directory.display()
        )
    })?;
    if manifest.dataset_id != expected_id || manifest.dataset_version != DATASET_VERSION {
        return Err(format!(
            "refusing to remove dataset directory {} because it is not owned by this generator",
            directory.display()
        ));
    }
    restore_restricted_directory(directory);
    fs::remove_dir_all(directory).map_err(|error| {
        format!(
            "failed to remove dataset {} before rebuilding: {error}",
            directory.display()
        )
    })
}

#[cfg(unix)]
fn restore_restricted_directory(root: &Path) {
    use std::os::unix::fs::PermissionsExt;

    let restricted = root.join("boundary-features/restricted");
    if !restricted.exists() {
        return;
    }
    let _ = fs::set_permissions(&restricted, fs::Permissions::from_mode(0o700));
}

#[cfg(not(unix))]
fn restore_restricted_directory(_root: &Path) {}

fn write_json(path: &Path, manifest: &BenchmarkDatasetManifest) -> Result<(), String> {
    let content = serde_json::to_vec_pretty(manifest)
        .map_err(|error| format!("failed to serialize benchmark dataset manifest: {error}"))?;
    fs::write(path, content).map_err(|error| format!("failed to write {}: {error}", path.display()))
}

fn write_text(path: &Path, content: &str) -> Result<(), String> {
    fs::write(path, content).map_err(|error| format!("failed to write {}: {error}", path.display()))
}

fn render_markdown(manifest: &BenchmarkDatasetManifest) -> String {
    let unsupported = if manifest.features.unsupported_features.is_empty() {
        "None".to_string()
    } else {
        manifest.features.unsupported_features.join("；")
    };
    format!(
        "# MangoDisk Fixed Benchmark Dataset\n\n\
         - Dataset ID: `{}`\n\
         - schema：`{}`\n\
         - Dataset version: `{}`\n\
         - seed：`{}`\n\
         - Logical digest: `{}`\n\
         - Logical files: {}\n\
         - Logical directories: {}\n\
         - Logical bytes: {}\n\
         - Allocated bytes: {}\n\
         - Expected large files: {}\n\
         - Expected large-file bytes: {}\n\
         - Expected duplicate groups: {}\n\
         - Expected duplicate files: {}\n\
         - Expected reclaimable bytes: {}\n\n\
         ## Platform Features\n\n\
         - Sparse file: {}\n\
         - Hard link: {}\n\
         - Symbolic link: {}\n\
         - Restricted directory: {}\n\
         - Unsupported or restricted: {}\n\n\
         > The root path is stored only in the local JSON report. Markdown never records the full user path.\n",
        manifest.dataset_id,
        manifest.schema_version,
        manifest.dataset_version,
        manifest.seed,
        manifest.logical_digest,
        manifest.logical_file_count,
        manifest.logical_directory_count,
        manifest.logical_bytes,
        manifest
            .allocated_bytes
            .map(|value| value.to_string())
            .unwrap_or_else(|| "Unavailable".to_string()),
        manifest.expected_large_file_count,
        manifest.expected_large_file_bytes,
        manifest.expected_duplicate_group_count,
        manifest.expected_duplicate_file_count,
        manifest.expected_reclaimable_bytes,
        manifest.features.sparse_files_created,
        manifest.features.hard_links_created,
        manifest.features.symbolic_links_created,
        manifest.features.permission_restricted_directories,
        unsupported
    )
}

#[cfg(test)]
mod tests {
    use super::safe_join;
    use std::path::Path;

    #[test]
    fn fixed_dataset_rejects_paths_outside_owned_root() {
        let root = Path::new("/owned-dataset");
        assert!(safe_join(root, "../outside.bin").is_err());
        assert!(safe_join(root, "/absolute.bin").is_err());
        assert_eq!(
            safe_join(root, "nested/file.bin").expect("a normal relative path must be accepted"),
            root.join("nested/file.bin")
        );
    }
}

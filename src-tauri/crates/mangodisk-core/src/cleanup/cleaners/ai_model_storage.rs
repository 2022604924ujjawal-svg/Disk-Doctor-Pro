use std::{
    collections::{BTreeMap, HashMap, HashSet},
    env, fs,
    path::{Path, PathBuf},
    sync::{Mutex, OnceLock},
    time::{Instant, UNIX_EPOCH},
};

use mangodisk_platform::{current_platform, Platform};

use crate::{
    applications::catalog::ProcessSnapshot,
    cleanup::{
        source_selection::SourceScope, CleanupActionKind, CleanupActionReason, CleanupActionResult,
        CleanupActionStatus, CleanupCategory, CleanupGroup, CleanupSourceDetail, RiskLevel,
        ScanItemStatus, ScanRuleResult,
    },
    filesystem::{
        metadata::{diagnostic_path, display_path, is_link_like, modified_ms},
        permanent_delete::{delete_path_permanently, prepare_path_for_permanent_delete},
    },
    shared::operation::OperationGuard,
};

pub(super) const CLEANER_REVISION: &str = "ai-model-storage-v5-keras-formats";

const HUGGING_FACE_ID: &str = "special.ai-model-hugging-face";
const WHISPER_ID: &str = "special.ai-model-whisper";
const PYTORCH_ID: &str = "special.ai-model-pytorch";
const PYTORCH_HUB_REPOSITORIES_ID: &str = "special.ai-cache-pytorch-hub-repositories";
const MODELSCOPE_ID: &str = "special.ai-model-modelscope";
const KERAS_ID: &str = "special.ai-model-keras";
const OPENAI_CLIP_ID: &str = "special.ai-model-openai-clip";
const TENSORFLOW_HUB_ID: &str = "special.ai-model-tensorflow-hub";
const LM_STUDIO_ID: &str = "special.ai-model-lm-studio";
const OLLAMA_ID: &str = "special.ai-model-ollama";
const COQUI_TTS_ID: &str = "special.ai-model-coqui-tts";
const GPT4ALL_ID: &str = "special.ai-model-gpt4all";
const JAN_ID: &str = "special.ai-model-jan";

const LM_STUDIO_PROCESSES: &[&str] = &["LM Studio", "lms"];
const OLLAMA_PROCESSES: &[&str] = &["Ollama", "ollama"];
const GPT4ALL_PROCESSES: &[&str] = &["GPT4All", "gpt4all"];
const JAN_PROCESSES: &[&str] = &["Jan", "jan"];

static LAST_PREVIEWS: OnceLock<Mutex<HashMap<String, Vec<ModelCandidate>>>> = OnceLock::new();

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ModelLayout {
    DirectoriesWithPrefix(&'static [&'static str]),
    DirectoriesWithMarker(&'static str),
    DirectDirectories,
    DirectHexDirectories(usize),
    DirectFilesWithExtensions(&'static [&'static str]),
    DirectFiles,
    TwoLevelDirectories,
    WholeRoot,
}

#[derive(Debug, Clone, Copy)]
enum ModelRoot {
    Home(&'static [&'static str]),
    HuggingFace,
    PyTorch,
    PyTorchHub,
    ModelScope,
    Ollama,
    TensorFlowHub,
    Gpt4All,
    Jan,
}

#[derive(Debug, Clone, Copy)]
struct ProviderSpec {
    id: &'static str,
    root: ModelRoot,
    layout: ModelLayout,
    required_stopped_processes: &'static [&'static str],
}

const PROVIDERS: &[ProviderSpec] = &[
    ProviderSpec {
        id: HUGGING_FACE_ID,
        root: ModelRoot::HuggingFace,
        layout: ModelLayout::DirectoriesWithPrefix(&["models--"]),
        required_stopped_processes: &[],
    },
    ProviderSpec {
        id: WHISPER_ID,
        root: ModelRoot::Home(&[".cache", "whisper"]),
        layout: ModelLayout::DirectFilesWithExtensions(&["pt"]),
        required_stopped_processes: &[],
    },
    ProviderSpec {
        id: PYTORCH_ID,
        root: ModelRoot::PyTorch,
        layout: ModelLayout::DirectFiles,
        required_stopped_processes: &[],
    },
    ProviderSpec {
        id: PYTORCH_HUB_REPOSITORIES_ID,
        root: ModelRoot::PyTorchHub,
        layout: ModelLayout::DirectoriesWithMarker("hubconf.py"),
        required_stopped_processes: &[],
    },
    ProviderSpec {
        id: MODELSCOPE_ID,
        root: ModelRoot::ModelScope,
        layout: ModelLayout::TwoLevelDirectories,
        required_stopped_processes: &[],
    },
    ProviderSpec {
        id: KERAS_ID,
        root: ModelRoot::Home(&[".keras", "models"]),
        // HDF5 is too generic for whole-disk classification, but files inside
        // Keras' dedicated model directory are model weights by ownership.
        layout: ModelLayout::DirectFilesWithExtensions(&["h5", "hdf5", "keras"]),
        required_stopped_processes: &[],
    },
    ProviderSpec {
        id: OPENAI_CLIP_ID,
        root: ModelRoot::Home(&[".cache", "clip"]),
        layout: ModelLayout::DirectFilesWithExtensions(&["pt"]),
        required_stopped_processes: &[],
    },
    ProviderSpec {
        id: TENSORFLOW_HUB_ID,
        root: ModelRoot::TensorFlowHub,
        layout: ModelLayout::DirectHexDirectories(40),
        required_stopped_processes: &[],
    },
    ProviderSpec {
        id: LM_STUDIO_ID,
        root: ModelRoot::Home(&[".lmstudio", "models"]),
        layout: ModelLayout::TwoLevelDirectories,
        required_stopped_processes: LM_STUDIO_PROCESSES,
    },
    ProviderSpec {
        id: OLLAMA_ID,
        root: ModelRoot::Ollama,
        layout: ModelLayout::WholeRoot,
        required_stopped_processes: OLLAMA_PROCESSES,
    },
    ProviderSpec {
        id: COQUI_TTS_ID,
        root: ModelRoot::Home(&[".local", "share", "tts"]),
        layout: ModelLayout::DirectoriesWithPrefix(&["tts_models--", "vocoder_models--"]),
        required_stopped_processes: &[],
    },
    ProviderSpec {
        id: GPT4ALL_ID,
        root: ModelRoot::Gpt4All,
        layout: ModelLayout::DirectFilesWithExtensions(&["gguf"]),
        required_stopped_processes: GPT4ALL_PROCESSES,
    },
    ProviderSpec {
        id: JAN_ID,
        root: ModelRoot::Jan,
        layout: ModelLayout::DirectDirectories,
        required_stopped_processes: JAN_PROCESSES,
    },
];

#[derive(Debug, Clone, PartialEq, Eq)]
struct ModelCandidate {
    path: PathBuf,
    bytes: u64,
    file_count: u64,
    modified_at_ms: Option<u64>,
    fingerprint: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct ModelSnapshot {
    bytes: u64,
    file_count: u64,
    modified_at_ms: Option<u64>,
    fingerprint: String,
}

pub(super) fn preview_all(
    is_cancelled: &(dyn Fn() -> bool + Sync),
    report_path: &(dyn Fn(&Path) + Sync),
    report_files: &(dyn Fn(&Path, u64, u64) + Sync),
) -> Vec<ScanRuleResult> {
    let Some(home) = home_directory() else {
        return preview_limited_all();
    };
    let process_snapshot = ProcessSnapshot::capture();

    PROVIDERS
        .iter()
        .map(|provider| {
            let started = Instant::now();
            let Some(root) = provider_root(&home, provider) else {
                replace_preview(provider.id, Some(Vec::new()));
                return unavailable_rule(provider, ScanItemStatus::NotApplicable, 0);
            };
            if !root.exists() {
                replace_preview(provider.id, Some(Vec::new()));
                return unavailable_rule(provider, ScanItemStatus::NotApplicable, 0);
            }
            let running_processes = match &process_snapshot {
                Ok(snapshot) => snapshot.matching_processes(
                    &provider
                        .required_stopped_processes
                        .iter()
                        .map(|name| (*name).to_string())
                        .collect::<Vec<_>>(),
                ),
                Err(_) if !provider.required_stopped_processes.is_empty() => {
                    replace_preview(provider.id, None);
                    return unavailable_rule(
                        provider,
                        ScanItemStatus::Limited,
                        started.elapsed().as_millis() as u64,
                    );
                }
                Err(_) => Vec::new(),
            };
            let candidates =
                discover_provider(provider, &root, is_cancelled, report_path, report_files);
            let candidates = match candidates {
                Ok(candidates) => candidates,
                Err(error) => {
                    log::warn!(
                        "ai_model_preview_failed provider_id={} reason={} root={}",
                        provider.id,
                        error,
                        diagnostic_path(&root)
                    );
                    replace_preview(provider.id, None);
                    return unavailable_rule(
                        provider,
                        ScanItemStatus::Limited,
                        started.elapsed().as_millis() as u64,
                    );
                }
            };
            replace_preview(provider.id, Some(candidates.clone()));
            model_rule(
                provider,
                candidates,
                running_processes,
                started.elapsed().as_millis() as u64,
            )
        })
        .collect()
}

pub(super) fn preview_limited_all() -> Vec<ScanRuleResult> {
    PROVIDERS
        .iter()
        .map(|provider| {
            // A failed scan must revoke the previous source snapshot. Keeping
            // it would let a later request execute against evidence that the
            // current scan could not reproduce.
            replace_preview(provider.id, None);
            unavailable_rule(provider, ScanItemStatus::Limited, 0)
        })
        .collect()
}

pub(super) fn contains(id: &str) -> bool {
    PROVIDERS.iter().any(|provider| provider.id == id)
}

pub(super) fn count() -> usize {
    PROVIDERS.len()
}

pub(super) fn catalog_digest() -> String {
    let mut hasher = blake3::Hasher::new();
    hasher.update(CLEANER_REVISION.as_bytes());
    for provider in PROVIDERS {
        update_digest_field(&mut hasher, provider.id.as_bytes());
        update_root_digest(&mut hasher, provider.root);
        update_layout_digest(&mut hasher, provider.layout);
        for process in provider.required_stopped_processes {
            update_digest_field(&mut hasher, process.as_bytes());
        }
    }
    hasher.finalize().to_hex().to_string()
}

fn update_root_digest(hasher: &mut blake3::Hasher, root: ModelRoot) {
    let (kind, components): (&[u8], &[&str]) = match root {
        ModelRoot::Home(components) => (b"home", components),
        ModelRoot::HuggingFace => (b"hugging-face", &[]),
        ModelRoot::PyTorch => (b"pytorch", &[]),
        ModelRoot::PyTorchHub => (b"pytorch-hub", &[]),
        ModelRoot::ModelScope => (b"modelscope", &[]),
        ModelRoot::Ollama => (b"ollama", &[]),
        ModelRoot::TensorFlowHub => (b"tensorflow-hub", &[]),
        ModelRoot::Gpt4All => (b"gpt4all", &[]),
        ModelRoot::Jan => (b"jan", &[]),
    };
    update_digest_field(hasher, kind);
    for component in components {
        update_digest_field(hasher, component.as_bytes());
    }
}

fn update_digest_field(hasher: &mut blake3::Hasher, value: &[u8]) {
    hasher.update(&(value.len() as u64).to_le_bytes());
    hasher.update(value);
}

fn update_layout_digest(hasher: &mut blake3::Hasher, layout: ModelLayout) {
    let (kind, values): (&[u8], &[&str]) = match layout {
        ModelLayout::DirectoriesWithPrefix(prefixes) => (b"directories-with-prefix", prefixes),
        ModelLayout::DirectoriesWithMarker(marker) => {
            update_digest_field(hasher, b"directories-with-marker");
            update_digest_field(hasher, marker.as_bytes());
            return;
        }
        ModelLayout::DirectDirectories => (b"direct-directories", &[]),
        ModelLayout::DirectHexDirectories(length) => {
            update_digest_field(hasher, b"direct-hex-directories");
            update_digest_field(hasher, &length.to_le_bytes());
            return;
        }
        ModelLayout::DirectFilesWithExtensions(extensions) => {
            (b"direct-files-with-extensions", extensions)
        }
        ModelLayout::DirectFiles => (b"direct-files", &[]),
        ModelLayout::TwoLevelDirectories => (b"two-level-directories", &[]),
        ModelLayout::WholeRoot => (b"whole-root", &[]),
    };
    update_digest_field(hasher, kind);
    for value in values {
        update_digest_field(hasher, value.as_bytes());
    }
}

pub(super) fn execute(
    id: &str,
    source_scope: Option<&SourceScope>,
    dry_run: bool,
    operation: &OperationGuard,
) -> CleanupActionResult {
    let Some(provider) = PROVIDERS.iter().find(|provider| provider.id == id) else {
        return failed_action(id, 0, CleanupActionReason::CleanerUnavailable);
    };
    let expected = LAST_PREVIEWS
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .ok()
        .and_then(|previews| previews.get(id).cloned());
    let Some(expected_all) = expected else {
        return failed_action(id, 0, CleanupActionReason::PreflightFailed);
    };
    if let Some(scope) = source_scope {
        if scope
            .validate_known_paths(
                expected_all
                    .iter()
                    .map(|candidate| candidate.path.as_path()),
            )
            .is_err()
        {
            return failed_action(id, 0, CleanupActionReason::PreflightFailed);
        }
    }
    let expected = expected_all
        .iter()
        .filter(|candidate| source_scope.is_none_or(|scope| scope.selects(&candidate.path)))
        .cloned()
        .collect::<Vec<_>>();
    let expected_bytes = expected.iter().map(|candidate| candidate.bytes).sum();

    if !provider.required_stopped_processes.is_empty() {
        let running = match ProcessSnapshot::capture() {
            Ok(snapshot) => snapshot.matching_processes(
                &provider
                    .required_stopped_processes
                    .iter()
                    .map(|name| (*name).to_string())
                    .collect::<Vec<_>>(),
            ),
            Err(_) => {
                return failed_action(id, expected_bytes, CleanupActionReason::PreflightFailed)
            }
        };
        if !running.is_empty() {
            return CleanupActionResult {
                rule_id: id.to_string(),
                action_kind: CleanupActionKind::Delete,
                status: CleanupActionStatus::Blocked,
                reason_code: Some(CleanupActionReason::RunningProcesses),
                bytes_expected: expected_bytes,
                released_bytes: 0,
                affected_item_count: 0,
                failed_item_count: expected.len() as u64,
                running_processes: running,
            };
        }
    }

    let mut released_bytes = 0_u64;
    let mut affected_items = 0_u64;
    let mut failed_items = 0_u64;
    let mut deleted_paths = HashSet::new();
    for candidate in &expected {
        if operation.ensure_not_cancelled().is_err() {
            failed_items = failed_items.saturating_add(1);
            break;
        }
        let Ok(prepared) = prepare_path_for_permanent_delete(&candidate.path) else {
            failed_items = failed_items.saturating_add(1);
            continue;
        };
        if revalidate_candidate(candidate).is_err() {
            failed_items = failed_items.saturating_add(1);
            continue;
        }
        if dry_run {
            affected_items = affected_items.saturating_add(candidate.file_count);
            continue;
        }
        match delete_path_permanently(prepared, candidate.bytes, candidate.file_count) {
            Ok(()) => {
                released_bytes = released_bytes.saturating_add(candidate.bytes);
                affected_items = affected_items.saturating_add(candidate.file_count);
                deleted_paths.insert(candidate.path.clone());
            }
            Err(error) => {
                released_bytes = released_bytes.saturating_add(error.released_bytes());
                affected_items = affected_items.saturating_add(error.affected_item_count());
                log::warn!(
                    "ai_model_permanent_delete_failed provider_id={} path={} partial={} released_bytes={} affected_item_count={} error_digest={}",
                    id,
                    diagnostic_path(&candidate.path),
                    error.is_partial(),
                    error.released_bytes(),
                    error.affected_item_count(),
                    blake3::hash(error.to_string().as_bytes()).to_hex()
                );
                failed_items = failed_items.saturating_add(1);
            }
        }
    }
    if !dry_run && !deleted_paths.is_empty() {
        replace_preview(id, Some(remaining_preview(expected_all, &deleted_paths)));
    }
    CleanupActionResult {
        rule_id: id.to_string(),
        action_kind: CleanupActionKind::Delete,
        status: if failed_items == 0 {
            if dry_run {
                CleanupActionStatus::Previewed
            } else {
                CleanupActionStatus::Completed
            }
        } else if released_bytes > 0 {
            CleanupActionStatus::Partial
        } else {
            CleanupActionStatus::Failed
        },
        reason_code: (failed_items > 0).then_some(CleanupActionReason::ItemsSkipped),
        bytes_expected: expected_bytes,
        released_bytes,
        affected_item_count: affected_items,
        failed_item_count: failed_items,
        running_processes: Vec::new(),
    }
}

fn remaining_preview(
    candidates: Vec<ModelCandidate>,
    moved_paths: &HashSet<PathBuf>,
) -> Vec<ModelCandidate> {
    candidates
        .into_iter()
        .filter(|candidate| !moved_paths.contains(&candidate.path))
        .collect()
}

fn model_rule(
    provider: &ProviderSpec,
    mut candidates: Vec<ModelCandidate>,
    running_processes: Vec<String>,
    elapsed_ms: u64,
) -> ScanRuleResult {
    candidates.sort_by(|left, right| {
        right
            .bytes
            .cmp(&left.bytes)
            .then_with(|| left.path.cmp(&right.path))
    });
    let bytes = candidates.iter().map(|candidate| candidate.bytes).sum();
    let file_count = candidates
        .iter()
        .map(|candidate| candidate.file_count)
        .sum();
    let source_count = candidates.len() as u64;
    let sources = candidates
        .iter()
        .map(|candidate| CleanupSourceDetail {
            path: display_path(&candidate.path),
            bytes: candidate.bytes,
            file_count: candidate.file_count,
            modified_at_ms: candidate.modified_at_ms,
            block_reason: None,
        })
        .collect();
    let status = if candidates.is_empty() {
        ScanItemStatus::Clean
    } else if running_processes.is_empty() {
        ScanItemStatus::Found
    } else {
        ScanItemStatus::RequiresClose
    };
    ScanRuleResult {
        rule_id: provider.id.to_string(),
        category: CleanupCategory::Ai,
        group: CleanupGroup::Ai,
        risk: RiskLevel::Recoverable,
        default_selected: false,
        recommended_selected: false,
        bytes,
        file_count,
        available: true,
        selectable: !candidates.is_empty(),
        status,
        running_processes,
        requires_app_close: !provider.required_stopped_processes.is_empty(),
        sources,
        source_count,
        sources_truncated: false,
        scan_elapsed_ms: elapsed_ms,
    }
}

fn unavailable_rule(
    provider: &ProviderSpec,
    status: ScanItemStatus,
    elapsed_ms: u64,
) -> ScanRuleResult {
    ScanRuleResult {
        rule_id: provider.id.to_string(),
        category: CleanupCategory::Ai,
        group: CleanupGroup::Ai,
        risk: RiskLevel::Recoverable,
        default_selected: false,
        recommended_selected: false,
        bytes: 0,
        file_count: 0,
        available: status != ScanItemStatus::NotApplicable,
        selectable: false,
        status,
        running_processes: Vec::new(),
        requires_app_close: !provider.required_stopped_processes.is_empty(),
        sources: Vec::new(),
        source_count: 0,
        sources_truncated: false,
        scan_elapsed_ms: elapsed_ms,
    }
}

fn discover_provider(
    provider: &ProviderSpec,
    root: &Path,
    is_cancelled: &(dyn Fn() -> bool + Sync),
    report_path: &(dyn Fn(&Path) + Sync),
    report_files: &(dyn Fn(&Path, u64, u64) + Sync),
) -> Result<Vec<ModelCandidate>, String> {
    current_platform()
        .validate_path_no_links(root)
        .map_err(|error| error.to_string())?;
    let paths = candidate_paths(root, provider.layout)?;
    let mut candidates = Vec::new();
    for path in paths {
        if is_cancelled() {
            return Err("cancelled".to_string());
        }
        report_path(&path);
        let snapshot = snapshot_model_path(&path)?;
        report_files(&path, snapshot.file_count, snapshot.bytes);
        if snapshot.bytes > 0 || snapshot.file_count > 0 {
            candidates.push(ModelCandidate {
                path,
                bytes: snapshot.bytes,
                file_count: snapshot.file_count,
                modified_at_ms: snapshot.modified_at_ms,
                fingerprint: snapshot.fingerprint,
            });
        }
    }
    Ok(candidates)
}

fn candidate_paths(root: &Path, layout: ModelLayout) -> Result<Vec<PathBuf>, String> {
    if layout == ModelLayout::WholeRoot {
        let entries = direct_entries(root)?;
        if entries.is_empty() {
            return Ok(Vec::new());
        }
        // OLLAMA_MODELS is user-configurable. Never allow an arbitrary
        // absolute directory to become one selectable cleanup source merely
        // because the environment variable points to it.
        if !is_directory(&root.join("blobs")) || !is_directory(&root.join("manifests")) {
            return Err("ollamaModelRootStructureMismatch".to_string());
        }
        return Ok(vec![root.to_path_buf()]);
    }
    let first_level = direct_entries(root)?;
    let mut paths = Vec::new();
    match layout {
        ModelLayout::DirectoriesWithPrefix(prefixes) => {
            for path in first_level {
                let name = path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .unwrap_or("");
                if prefixes.iter().any(|prefix| name.starts_with(prefix)) && is_directory(&path) {
                    paths.push(path);
                }
            }
        }
        ModelLayout::DirectoriesWithMarker(marker) => {
            paths.extend(
                first_level.into_iter().filter(|path| {
                    is_visible_directory(path) && is_regular_file(&path.join(marker))
                }),
            );
        }
        ModelLayout::DirectDirectories => {
            paths.extend(
                first_level
                    .into_iter()
                    .filter(|path| is_visible_directory(path)),
            );
        }
        ModelLayout::DirectHexDirectories(length) => {
            paths.extend(first_level.into_iter().filter(|path| {
                is_visible_directory(path)
                    && path.file_name().is_some_and(|name| {
                        let name = name.to_string_lossy();
                        name.len() == length && name.bytes().all(|byte| byte.is_ascii_hexdigit())
                    })
            }));
        }
        ModelLayout::DirectFilesWithExtensions(extensions) => {
            for path in first_level {
                let extension = path
                    .extension()
                    .and_then(|value| value.to_str())
                    .unwrap_or("");
                if extensions
                    .iter()
                    .any(|candidate| extension.eq_ignore_ascii_case(candidate))
                    && is_regular_file(&path)
                {
                    paths.push(path);
                }
            }
        }
        ModelLayout::DirectFiles => {
            paths.extend(first_level.into_iter().filter(|path| is_regular_file(path)));
        }
        ModelLayout::TwoLevelDirectories => {
            for publisher in first_level
                .into_iter()
                .filter(|path| is_visible_directory(path))
            {
                paths.extend(
                    direct_entries(&publisher)?
                        .into_iter()
                        .filter(|path| is_visible_directory(path)),
                );
            }
        }
        ModelLayout::WholeRoot => unreachable!(),
    }
    paths.sort();
    Ok(paths)
}

fn direct_entries(root: &Path) -> Result<Vec<PathBuf>, String> {
    let mut paths = fs::read_dir(root)
        .map_err(|error| error.to_string())?
        .map(|entry| {
            entry
                .map(|entry| entry.path())
                .map_err(|error| error.to_string())
        })
        .collect::<Result<Vec<_>, _>>()?;
    paths.sort();
    Ok(paths)
}

fn is_directory(path: &Path) -> bool {
    fs::symlink_metadata(path).is_ok_and(|metadata| metadata.is_dir() && !is_link_like(&metadata))
}

fn is_visible_directory(path: &Path) -> bool {
    is_directory(path)
        && path
            .file_name()
            .is_some_and(|name| !name.to_string_lossy().starts_with('.'))
}

fn is_regular_file(path: &Path) -> bool {
    fs::symlink_metadata(path).is_ok_and(|metadata| metadata.is_file() && !is_link_like(&metadata))
}

fn snapshot_model_path(path: &Path) -> Result<ModelSnapshot, String> {
    let metadata = fs::symlink_metadata(path).map_err(|error| error.to_string())?;
    if is_link_like(&metadata) {
        return Err("candidateRootIsLink".to_string());
    }
    if metadata.is_file() {
        let fingerprint = metadata_entry_fingerprint(path, &metadata, None)?;
        return Ok(ModelSnapshot {
            bytes: metadata.len(),
            file_count: 1,
            modified_at_ms: modified_ms(&metadata),
            fingerprint,
        });
    }
    if !metadata.is_dir() {
        return Err("candidateRootIsNotRegular".to_string());
    }
    snapshot_model_directory(path)
}

fn snapshot_model_directory(path: &Path) -> Result<ModelSnapshot, String> {
    let mut child_fingerprints = BTreeMap::new();
    let mut snapshot = ModelSnapshot::default();
    for child in direct_entries(path)? {
        let metadata = fs::symlink_metadata(&child).map_err(|error| error.to_string())?;
        let name = child
            .file_name()
            .map(|value| value.to_string_lossy().into_owned())
            .unwrap_or_default();
        if is_link_like(&metadata) {
            // Hugging Face snapshots intentionally contain relative symlinks to
            // content-addressed blobs. They are fingerprinted as leaf entries
            // and never followed, so moving the selected repository cannot
            // traverse outside its approved model-cache boundary.
            let target = fs::read_link(&child).map_err(|error| error.to_string())?;
            let fingerprint = metadata_entry_fingerprint(
                &child,
                &metadata,
                Some(target.to_string_lossy().as_bytes()),
            )?;
            child_fingerprints.insert(name, fingerprint);
            snapshot.modified_at_ms = latest(snapshot.modified_at_ms, modified_ms(&metadata));
            continue;
        }
        if metadata.is_dir() {
            let child_snapshot = snapshot_model_directory(&child)?;
            snapshot.bytes = snapshot.bytes.saturating_add(child_snapshot.bytes);
            snapshot.file_count = snapshot
                .file_count
                .saturating_add(child_snapshot.file_count);
            snapshot.modified_at_ms =
                latest(snapshot.modified_at_ms, child_snapshot.modified_at_ms);
            child_fingerprints.insert(name, child_snapshot.fingerprint);
        } else if metadata.is_file() {
            snapshot.bytes = snapshot.bytes.saturating_add(metadata.len());
            snapshot.file_count = snapshot.file_count.saturating_add(1);
            snapshot.modified_at_ms = latest(snapshot.modified_at_ms, modified_ms(&metadata));
            child_fingerprints.insert(name, metadata_entry_fingerprint(&child, &metadata, None)?);
        } else {
            return Err("candidateContainsSpecialFile".to_string());
        }
    }
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"mangodisk-ai-model-directory-v1");
    for (name, fingerprint) in child_fingerprints {
        hasher.update(&(name.len() as u64).to_le_bytes());
        hasher.update(name.as_bytes());
        hasher.update(fingerprint.as_bytes());
    }
    snapshot.fingerprint = hasher.finalize().to_hex().to_string();
    Ok(snapshot)
}

fn metadata_entry_fingerprint(
    path: &Path,
    metadata: &fs::Metadata,
    link_target: Option<&[u8]>,
) -> Result<String, String> {
    let modified = metadata
        .modified()
        .map_err(|error| error.to_string())?
        .duration_since(UNIX_EPOCH)
        .map_err(|error| error.to_string())?
        .as_nanos();
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"mangodisk-ai-model-entry-v1");
    hasher.update(
        path.file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .as_bytes(),
    );
    hasher.update(&metadata.len().to_le_bytes());
    hasher.update(&modified.to_le_bytes());
    if let Some(target) = link_target {
        hasher.update(target);
    }
    Ok(hasher.finalize().to_hex().to_string())
}

fn revalidate_candidate(candidate: &ModelCandidate) -> Result<(), String> {
    current_platform()
        .validate_path_no_links(&candidate.path)
        .map_err(|error| error.to_string())?;
    let current = snapshot_model_path(&candidate.path)?;
    if current.bytes != candidate.bytes
        || current.file_count != candidate.file_count
        || current.modified_at_ms != candidate.modified_at_ms
        || current.fingerprint != candidate.fingerprint
    {
        return Err("candidateChangedAfterPreview".to_string());
    }
    Ok(())
}

fn replace_preview(id: &str, candidates: Option<Vec<ModelCandidate>>) {
    let Ok(mut previews) = LAST_PREVIEWS
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
    else {
        return;
    };
    match candidates {
        Some(candidates) => {
            previews.insert(id.to_string(), candidates);
        }
        None => {
            previews.remove(id);
        }
    }
}

fn provider_root(home: &Path, provider: &ProviderSpec) -> Option<PathBuf> {
    match provider.root {
        ModelRoot::Home(components) => Some(join_components(home, components)),
        ModelRoot::HuggingFace => absolute_environment_path("HF_HUB_CACHE")
            .or_else(|| absolute_environment_path("HUGGINGFACE_HUB_CACHE"))
            .or_else(|| absolute_environment_path("HF_HOME").map(|path| path.join("hub")))
            .or_else(|| Some(home.join(".cache").join("huggingface").join("hub"))),
        ModelRoot::PyTorch => absolute_environment_path("TORCH_HOME")
            .map(|path| path.join("hub").join("checkpoints"))
            .or_else(|| {
                Some(
                    home.join(".cache")
                        .join("torch")
                        .join("hub")
                        .join("checkpoints"),
                )
            }),
        ModelRoot::PyTorchHub => absolute_environment_path("TORCH_HOME")
            .map(|path| path.join("hub"))
            .or_else(|| Some(home.join(".cache").join("torch").join("hub"))),
        ModelRoot::ModelScope => {
            let cache = absolute_environment_path("MODELSCOPE_CACHE")
                .unwrap_or_else(|| home.join(".cache").join("modelscope").join("hub"));
            Some(append_unless_named(cache, "models"))
        }
        ModelRoot::Ollama => absolute_environment_path("OLLAMA_MODELS")
            .or_else(|| Some(home.join(".ollama").join("models"))),
        ModelRoot::TensorFlowHub => absolute_environment_path("TFHUB_CACHE_DIR")
            .or_else(|| Some(env::temp_dir().join("tfhub_modules"))),
        ModelRoot::Gpt4All => gpt4all_model_root(home),
        ModelRoot::Jan => jan_model_root(home),
    }
}

fn join_components(root: &Path, components: &[&str]) -> PathBuf {
    components
        .iter()
        .fold(root.to_path_buf(), |path, component| path.join(component))
}

fn append_unless_named(path: PathBuf, component: &str) -> PathBuf {
    if path
        .file_name()
        .is_some_and(|name| name.to_string_lossy().eq_ignore_ascii_case(component))
    {
        path
    } else {
        path.join(component)
    }
}

fn gpt4all_model_root(_home: &Path) -> Option<PathBuf> {
    #[cfg(target_os = "macos")]
    {
        Some(
            _home
                .join("Library")
                .join("Application Support")
                .join("nomic.ai")
                .join("GPT4All"),
        )
    }
    #[cfg(windows)]
    {
        absolute_environment_path("LOCALAPPDATA").map(|path| path.join("nomic.ai").join("GPT4All"))
    }
    #[cfg(not(any(target_os = "macos", windows)))]
    {
        Some(
            _home
                .join(".local")
                .join("share")
                .join("nomic.ai")
                .join("GPT4All"),
        )
    }
}

fn jan_model_root(home: &Path) -> Option<PathBuf> {
    let (settings, default_data) = jan_settings_and_default_data(home)?;
    if !settings.exists() {
        return Some(default_data.join("models"));
    }
    let content = match fs::read_to_string(&settings) {
        Ok(content) => content,
        Err(error) => {
            log::warn!(
                "ai_model_jan_settings_unreadable error_digest={}",
                blake3::hash(error.to_string().as_bytes()).to_hex()
            );
            return None;
        }
    };
    let configured = match parse_jan_data_folder(&content) {
        Ok(path) => path,
        Err(error) => {
            log::warn!(
                "ai_model_jan_settings_invalid error_digest={}",
                blake3::hash(error.as_bytes()).to_hex()
            );
            return None;
        }
    };
    match configured {
        Some(path) if path.is_absolute() => Some(path.join("models")),
        Some(_) => {
            log::warn!("ai_model_jan_settings_ignored reason=notAbsolute");
            None
        }
        None => Some(default_data.join("models")),
    }
}

fn parse_jan_data_folder(content: &str) -> Result<Option<PathBuf>, String> {
    serde_json::from_str::<serde_json::Value>(content)
        .map_err(|error| error.to_string())
        .map(|value| {
            value
                .get("data_folder")
                .and_then(serde_json::Value::as_str)
                .map(PathBuf::from)
        })
}

fn jan_settings_and_default_data(_home: &Path) -> Option<(PathBuf, PathBuf)> {
    #[cfg(target_os = "macos")]
    {
        let root = _home
            .join("Library")
            .join("Application Support")
            .join("Jan");
        Some((root.join("settings.json"), root.join("data")))
    }
    #[cfg(windows)]
    {
        let root = absolute_environment_path("APPDATA")?.join("Jan");
        Some((root.join("settings.json"), root.join("data")))
    }
    #[cfg(not(any(target_os = "macos", windows)))]
    {
        let root = absolute_environment_path("XDG_CONFIG_HOME")
            .unwrap_or_else(|| _home.join(".config"))
            .join("Jan");
        Some((root.join("settings.json"), root.join("data")))
    }
}

fn absolute_environment_path(name: &str) -> Option<PathBuf> {
    let path = env::var_os(name).map(PathBuf::from)?;
    if path.is_absolute() {
        Some(path)
    } else {
        log::warn!("ai_model_environment_path_ignored variable={name} reason=notAbsolute");
        None
    }
}

fn home_directory() -> Option<PathBuf> {
    current_platform()
        .user_directories()
        .ok()
        .map(|directories| directories.home_directory().to_path_buf())
}

fn latest(current: Option<u64>, candidate: Option<u64>) -> Option<u64> {
    match (current, candidate) {
        (Some(current), Some(candidate)) => Some(current.max(candidate)),
        (current, candidate) => current.or(candidate),
    }
}

fn failed_action(
    id: &str,
    expected_bytes: u64,
    reason: CleanupActionReason,
) -> CleanupActionResult {
    CleanupActionResult {
        rule_id: id.to_string(),
        action_kind: CleanupActionKind::Delete,
        status: CleanupActionStatus::Failed,
        reason_code: Some(reason),
        bytes_expected: expected_bytes,
        released_bytes: 0,
        affected_item_count: 0,
        failed_item_count: 1,
        running_processes: Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::SystemTime;

    fn fixture_root(name: &str) -> PathBuf {
        env::temp_dir().join(format!(
            "mangodisk-ai-model-{name}-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        ))
    }

    #[test]
    fn hugging_face_discovery_keeps_models_separate_from_datasets() {
        let root = fixture_root("hugging-face");
        let model = root.join("models--openai--whisper-small");
        let dataset = root.join("datasets--sample");
        fs::create_dir_all(&model).expect("the model fixture should be created");
        fs::create_dir_all(&dataset).expect("the dataset fixture should be created");
        fs::write(model.join("model.bin"), b"model").expect("the model fixture should be written");
        fs::write(dataset.join("data.bin"), b"dataset")
            .expect("the dataset fixture should be written");

        let paths = candidate_paths(&root, ModelLayout::DirectoriesWithPrefix(&["models--"]))
            .expect("model discovery should succeed");
        assert_eq!(paths, vec![model]);

        fs::remove_dir_all(root).expect("the fixture should be removed");
    }

    #[test]
    fn modelscope_discovery_keeps_models_separate_by_repository() {
        let root = fixture_root("modelscope");
        let first = root.join("Qwen").join("Qwen3-0___6B");
        let second = root.join("iic").join("speech_paraformer-large");
        fs::create_dir_all(&first).expect("the first model fixture should be created");
        fs::create_dir_all(&second).expect("the second model fixture should be created");
        fs::write(first.join("model.safetensors"), b"model")
            .expect("the first model fixture should be written");
        fs::write(second.join("model.pt"), b"model")
            .expect("the second model fixture should be written");
        fs::write(root.join(".DS_Store"), b"metadata")
            .expect("the metadata fixture should be written");
        fs::create_dir_all(root.join("._____temp").join("partial"))
            .expect("the temporary fixture should be created");

        let paths = candidate_paths(&root, ModelLayout::TwoLevelDirectories)
            .expect("ModelScope discovery should succeed");
        assert_eq!(paths, vec![first, second]);

        fs::remove_dir_all(root).expect("the fixture should be removed");
    }

    #[test]
    fn pytorch_hub_repository_layout_requires_hubconf_marker() {
        let root = fixture_root("pytorch-hub-repositories");
        let repository = root.join("pytorch_vision_main");
        fs::create_dir_all(&repository).expect("the repository fixture should be created");
        fs::write(repository.join("hubconf.py"), b"dependencies = []")
            .expect("the repository marker should be written");
        fs::create_dir_all(root.join("checkpoints"))
            .expect("the checkpoint directory fixture should be created");
        fs::write(root.join("trusted_list"), b"pytorch_vision")
            .expect("the trust metadata fixture should be written");

        let paths = candidate_paths(&root, ModelLayout::DirectoriesWithMarker("hubconf.py"))
            .expect("PyTorch Hub repository discovery should succeed");
        assert_eq!(paths, vec![repository]);

        fs::remove_dir_all(root).expect("the fixture should be removed");
    }

    #[test]
    fn direct_directory_layout_ignores_app_metadata_files() {
        let root = fixture_root("direct-directories");
        let model = root.join("tiny-llama");
        fs::create_dir_all(&model).expect("the model fixture should be created");
        fs::write(root.join("models.json"), b"metadata")
            .expect("the metadata fixture should be written");

        let paths = candidate_paths(&root, ModelLayout::DirectDirectories)
            .expect("direct-directory discovery should succeed");
        assert_eq!(paths, vec![model]);

        fs::remove_dir_all(root).expect("the fixture should be removed");
    }

    #[test]
    fn tensorflow_hub_layout_only_accepts_sha1_module_directories() {
        let root = fixture_root("tensorflow-hub");
        let model = root.join("9616fd04ec2360621642ef9455b84f4b668e219e");
        fs::create_dir_all(&model).expect("the model fixture should be created");
        fs::create_dir_all(root.join("unrelated-user-directory"))
            .expect("the unrelated fixture should be created");
        fs::write(root.join("metadata.json"), b"metadata")
            .expect("the metadata fixture should be written");

        let paths = candidate_paths(&root, ModelLayout::DirectHexDirectories(40))
            .expect("TensorFlow Hub discovery should succeed");
        assert_eq!(paths, vec![model]);

        fs::remove_dir_all(root).expect("the fixture should be removed");
    }

    #[test]
    fn direct_file_layout_only_accepts_documented_model_extensions() {
        let root = fixture_root("direct-files");
        fs::create_dir_all(&root).expect("the model fixture should be created");
        let model = root.join("model.gguf");
        fs::write(&model, b"model").expect("the model fixture should be written");
        fs::write(root.join("chats.db"), b"private data")
            .expect("the unrelated fixture should be written");

        let paths = candidate_paths(&root, ModelLayout::DirectFilesWithExtensions(&["gguf"]))
            .expect("direct-file discovery should succeed");
        assert_eq!(paths, vec![model]);

        fs::remove_dir_all(root).expect("the fixture should be removed");
    }

    #[test]
    fn keras_layout_accepts_current_and_legacy_model_formats_only() {
        let root = fixture_root("keras-formats");
        fs::create_dir_all(&root).expect("the Keras model fixture should be created");
        let legacy = root.join("legacy.h5");
        let hdf5 = root.join("portable.hdf5");
        let current = root.join("whole-model.keras");
        for model in [&legacy, &hdf5, &current] {
            fs::write(model, b"model").expect("the Keras model fixture should be written");
        }
        fs::write(root.join("dataset.csv"), b"personal data")
            .expect("the unrelated fixture should be written");

        let provider = PROVIDERS
            .iter()
            .find(|provider| provider.id == KERAS_ID)
            .expect("the Keras provider must exist");
        let paths =
            candidate_paths(&root, provider.layout).expect("Keras model discovery should succeed");

        assert_eq!(paths, vec![legacy, hdf5, current]);
        fs::remove_dir_all(root).expect("the fixture should be removed");
    }

    #[test]
    fn model_candidates_always_require_explicit_selection() {
        let rule = model_rule(
            &PROVIDERS[0],
            vec![ModelCandidate {
                path: PathBuf::from("model"),
                bytes: 1024,
                file_count: 1,
                modified_at_ms: Some(1),
                fingerprint: "fixture".to_string(),
            }],
            Vec::new(),
            1,
        );

        assert!(rule.selectable);
        assert!(!rule.default_selected);
        assert!(!rule.recommended_selected);
        assert_eq!(rule.risk, RiskLevel::Recoverable);
    }

    #[test]
    fn every_model_provider_requires_explicit_selection() {
        for provider in PROVIDERS {
            let rule = model_rule(
                provider,
                vec![ModelCandidate {
                    path: PathBuf::from(provider.id),
                    bytes: 1024,
                    file_count: 1,
                    modified_at_ms: Some(1),
                    fingerprint: "fixture".to_string(),
                }],
                Vec::new(),
                1,
            );
            assert!(
                !rule.default_selected,
                "{} must not be selected",
                provider.id
            );
            assert!(
                !rule.recommended_selected,
                "{} must not be recommended",
                provider.id
            );
        }
    }

    #[test]
    fn provider_root_suffix_is_not_duplicated() {
        assert_eq!(
            append_unless_named(PathBuf::from("/cache/hub"), "models"),
            PathBuf::from("/cache/hub/models")
        );
        assert_eq!(
            append_unless_named(PathBuf::from("/cache/hub/models"), "models"),
            PathBuf::from("/cache/hub/models")
        );
    }

    #[test]
    fn jan_settings_only_accept_an_explicit_data_folder_field() {
        assert_eq!(
            parse_jan_data_folder(r#"{"data_folder":"/models/jan"}"#)
                .expect("valid Jan settings should parse"),
            Some(PathBuf::from("/models/jan"))
        );
        assert_eq!(
            parse_jan_data_folder(r#"{"threads":"private"}"#)
                .expect("unrelated Jan settings should parse"),
            None
        );
        assert!(parse_jan_data_folder("not-json").is_err());
    }

    #[test]
    fn completed_source_selection_preserves_unselected_models() {
        let selected = ModelCandidate {
            path: PathBuf::from("selected"),
            bytes: 1024,
            file_count: 1,
            modified_at_ms: Some(1),
            fingerprint: "selected".to_string(),
        };
        let retained = ModelCandidate {
            path: PathBuf::from("retained"),
            bytes: 2048,
            file_count: 1,
            modified_at_ms: Some(2),
            fingerprint: "retained".to_string(),
        };

        let remaining = remaining_preview(
            vec![selected.clone(), retained.clone()],
            &HashSet::from([selected.path]),
        );

        assert_eq!(remaining, vec![retained]);
    }

    #[test]
    fn whole_root_requires_an_ollama_repository_structure() {
        let root = fixture_root("ollama-root");
        fs::create_dir_all(&root).expect("the model fixture should be created");
        fs::write(root.join("personal-file.txt"), b"keep")
            .expect("the unrelated fixture should be written");
        assert!(candidate_paths(&root, ModelLayout::WholeRoot).is_err());

        fs::create_dir_all(root.join("blobs")).expect("the blob directory should be created");
        fs::create_dir_all(root.join("manifests"))
            .expect("the manifest directory should be created");
        assert_eq!(
            candidate_paths(&root, ModelLayout::WholeRoot)
                .expect("the Ollama repository should be accepted"),
            vec![root.clone()]
        );
        fs::remove_dir_all(root).expect("the fixture should be removed");
    }

    #[cfg(unix)]
    #[test]
    fn model_snapshot_fingerprints_symlinks_without_following_them() {
        use std::os::unix::fs::symlink;

        let root = fixture_root("symlink");
        let model = root.join("model");
        let outside = root.join("outside.bin");
        fs::create_dir_all(&model).expect("the model fixture should be created");
        fs::write(&outside, vec![0_u8; 32]).expect("the outside fixture should be written");
        symlink(&outside, model.join("weight.bin")).expect("the model link should be created");

        let snapshot = snapshot_model_path(&model).expect("the model snapshot should succeed");
        assert_eq!(snapshot.bytes, 0, "linked target bytes must not be counted");
        assert_eq!(
            snapshot.file_count, 0,
            "linked targets must not be traversed"
        );
        assert!(!snapshot.fingerprint.is_empty());

        fs::remove_dir_all(root).expect("the fixture should be removed");
    }

    #[test]
    fn snapshot_detects_equal_sized_model_replacement() {
        let root = fixture_root("replacement");
        fs::create_dir_all(&root).expect("the model fixture should be created");
        let model = root.join("model.pt");
        fs::write(&model, b"before").expect("the model fixture should be written");
        let before = snapshot_model_path(&model).expect("the first snapshot should succeed");
        std::thread::sleep(std::time::Duration::from_millis(20));
        fs::write(&model, b"after!").expect("the replacement should be written");
        let after = snapshot_model_path(&model).expect("the second snapshot should succeed");
        assert_eq!(before.bytes, after.bytes);
        assert_ne!(before.fingerprint, after.fingerprint);
        fs::remove_dir_all(root).expect("the fixture should be removed");
    }
}

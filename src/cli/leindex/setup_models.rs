use super::*;
use std::sync::atomic::{AtomicU64, Ordering};

static MODEL_INSTALL_SEQUENCE: AtomicU64 = AtomicU64::new(0);

fn next_model_install_id() -> u64 {
    MODEL_INSTALL_SEQUENCE.fetch_add(1, Ordering::Relaxed)
}

pub(super) const QWEN3_ONNX_REPOSITORY: &str = "zhiqing/Qwen3-Embedding-0.6B-ONNX";
pub(super) const QWEN3_ONNX_REVISION: &str = "c96cc9c82d08ee7869600e2191078fc939957026";
pub(super) const QWEN3_REMOTE_MODEL: &str = "model.onnx";
pub(super) const QWEN3_LOCAL_MODEL: &str =
    crate::cli::leindex::model_download::DYNAMIC_MODEL_ONNX_FILENAME;
pub(super) const QWEN3_MODEL_FILES: &[&str] =
    &[QWEN3_REMOTE_MODEL, "tokenizer.json", "config.json"];

#[derive(Debug, Clone, Copy)]
pub(super) struct ModelDownloadProfile {
    pub(super) repository: &'static str,
    pub(super) revision: &'static str,
    pub(super) remote_model: &'static str,
    pub(super) local_model: &'static str,
    pub(super) files: &'static [&'static str],
}

pub(super) fn model_download_profile(_provider: Option<ExecutionProvider>) -> ModelDownloadProfile {
    ModelDownloadProfile {
        repository: QWEN3_ONNX_REPOSITORY,
        revision: QWEN3_ONNX_REVISION,
        remote_model: QWEN3_REMOTE_MODEL,
        local_model: QWEN3_LOCAL_MODEL,
        files: QWEN3_MODEL_FILES,
    }
}

pub(super) fn model_name_for_provider(provider: Option<ExecutionProvider>) -> &'static str {
    model_download_profile(provider)
        .local_model
        .strip_suffix(".onnx")
        .expect("model profile local filename must end in .onnx")
}

/// Check if model files are present in the model directory.
pub(super) fn check_model_present_for_name(model_name: &str) -> bool {
    let model_filename = format!("{}.onnx", model_name);

    // Check via config module's model_dir_path
    if let Some(model_dir) = crate::cli::neural_config::model_dir_path() {
        if model_assets_present(&model_dir, &model_filename) {
            return true;
        }
    }

    // Also check bundled models relative to the binary
    if let Ok(exe) = std::env::current_exe() {
        if let Some(parent) = exe.parent() {
            if model_assets_present(&parent.join("models"), &model_filename) {
                return true;
            }
            if let Some(gp) = parent.parent() {
                if model_assets_present(&gp.join("models"), &model_filename) {
                    return true;
                }
            }
        }
    }

    false
}

pub(super) fn model_assets_present(model_dir: &Path, model_filename: &str) -> bool {
    if !model_dir.join(model_filename).exists() {
        return false;
    }

    if model_filename != crate::cli::leindex::model_download::DYNAMIC_MODEL_ONNX_FILENAME {
        return true;
    }

    dynamic_model_assets_present(model_dir)
}

pub(super) fn dynamic_model_assets_present(model_dir: &Path) -> bool {
    const MIN_MODEL_BYTES: u64 = 100 * 1024 * 1024;

    let model = model_dir.join(crate::cli::leindex::model_download::DYNAMIC_MODEL_ONNX_FILENAME);
    if std::fs::metadata(model)
        .map(|metadata| metadata.len() < MIN_MODEL_BYTES)
        .unwrap_or(true)
    {
        return false;
    }
    ["tokenizer.json", "config.json"].iter().all(|file| {
        std::fs::metadata(model_dir.join(file))
            .map(|metadata| metadata.len() > 0)
            .unwrap_or(false)
    })
}

/// Outcome of comparing the on-disk model file against the checksum manifest.
///
/// VAL-SETUP-014: `--check` mode reports `Mismatch` so the user knows the
/// model file is corrupted before re-running setup. VAL-SETUP-017/018 share
/// the same primitive.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ModelChecksumStatus {
    /// File is missing entirely.
    Missing,
    /// File exists and the manifest's checksum matches.
    Ok,
    /// File exists but no checksum entry is available.
    Unknown,
    /// File exists but its computed SHA256 differs from the manifest.
    Mismatch { expected: String, actual: String },
}

pub(super) fn model_checksum_status_for_name(model_name: &str) -> ModelChecksumStatus {
    use crate::cli::leindex::model_download::{
        CheckResult, DYNAMIC_MODEL_ONNX_FILENAME, check_file_against_manifest, parse_checksums,
    };

    let model_dir = match crate::cli::neural_config::model_dir_path() {
        Some(d) => d,
        None => return ModelChecksumStatus::Missing,
    };

    let model_filename = format!("{}.onnx", model_name);
    let onnx_path = model_dir.join(&model_filename);
    if !onnx_path.exists() {
        return ModelChecksumStatus::Missing;
    }

    let manifest_path = model_dir.join("checksums.sha256");
    let manifest_str = match std::fs::read_to_string(&manifest_path) {
        Ok(s) => s,
        Err(_) => return ModelChecksumStatus::Unknown,
    };
    let manifest = parse_checksums(&manifest_str);

    let status = match check_file_against_manifest(&onnx_path, &manifest) {
        Ok(CheckResult::Verified) => ModelChecksumStatus::Ok,
        Ok(CheckResult::NoEntry) => ModelChecksumStatus::Unknown,
        Ok(CheckResult::Mismatch { expected, actual }) => {
            ModelChecksumStatus::Mismatch { expected, actual }
        }
        Ok(CheckResult::Missing) => ModelChecksumStatus::Missing,
        Err(_) => ModelChecksumStatus::Unknown,
    };

    if model_filename == DYNAMIC_MODEL_ONNX_FILENAME && status == ModelChecksumStatus::Ok {
        let metadata_verified = ["tokenizer.json", "config.json"].iter().all(|file| {
            matches!(
                check_file_against_manifest(&model_dir.join(file), &manifest),
                Ok(CheckResult::Verified)
            )
        });
        if !metadata_verified {
            return ModelChecksumStatus::Unknown;
        }
    }
    status
}

/// Locate a Hugging Face CLI executable, honoring `HF_BIN` first.
pub(super) fn find_hugging_face_cli() -> Option<String> {
    let mut candidates = Vec::new();
    if let Ok(path) = std::env::var("HF_BIN") {
        if !path.trim().is_empty() {
            candidates.push(path);
        }
    }
    candidates.extend(["hf".to_string(), "huggingface-cli".to_string()]);

    candidates.into_iter().find(|program| {
        Command::new(program)
            .arg("download")
            .arg("--help")
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .map(|status| status.success())
            .unwrap_or(false)
    })
}

pub(super) fn ensure_hugging_face_cli() -> Result<String, SetupError> {
    if let Some(cli) = find_hugging_face_cli() {
        return Ok(cli);
    }

    let package = "huggingface_hub";
    let pip_cmd = find_pip().ok_or(SetupError::PipNotFound)?;
    println!("Installing {} for model downloads...", package);
    let output = Command::new(&pip_cmd.0)
        .args(&pip_cmd.1)
        .arg("install")
        .arg(package)
        .arg("--upgrade")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .output()
        .map_err(|_| SetupError::PipInstallFailed {
            package: package.to_string(),
            exit_code: -1,
        })?;

    if !output.status.success() {
        let combined = format!(
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        if is_network_error(&combined) {
            return Err(SetupError::PipNetworkFailed {
                package: package.to_string(),
                exit_code: output.status.code().unwrap_or(-1),
                output: truncate_for_error(&combined),
            });
        }
        return Err(SetupError::PipInstallFailed {
            package: package.to_string(),
            exit_code: output.status.code().unwrap_or(-1),
        });
    }

    find_hugging_face_cli().ok_or(SetupError::HuggingFaceCliNotFound)
}

pub(super) fn profile_assets_verified(model_dir: &Path, profile: ModelDownloadProfile) -> bool {
    use crate::cli::leindex::model_download::{
        CheckResult, check_file_against_manifest, parse_checksums,
    };

    if !dynamic_model_assets_present(model_dir) {
        return false;
    }
    let Ok(contents) = std::fs::read_to_string(model_dir.join("checksums.sha256")) else {
        return false;
    };
    let source_marker = format!("# source: {}@{}", profile.repository, profile.revision);
    if !contents
        .lines()
        .any(|line| line.trim() == source_marker.as_str())
    {
        return false;
    }
    let manifest = parse_checksums(&contents);
    [profile.local_model, "tokenizer.json", "config.json"]
        .iter()
        .all(|file| {
            matches!(
                check_file_against_manifest(&model_dir.join(file), &manifest),
                Ok(CheckResult::Verified)
            )
        })
}

pub(super) fn install_downloaded_model_file(src: &Path, dst: &Path) -> Result<(), SetupError> {
    if !src.exists() {
        return Err(SetupError::Io(format!(
            "Hugging Face download did not produce {}",
            src.display()
        )));
    }
    let file_name = dst
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| SetupError::Io(format!("Invalid model destination {}", dst.display())))?;
    let install_id = next_model_install_id();
    let staged = dst.with_file_name(format!(
        ".{file_name}.leindex-install-{}-{install_id}",
        std::process::id()
    ));
    let _ = std::fs::remove_file(&staged);

    // Stage beside the destination so the final rename stays on one
    // filesystem. The existing destination is untouched until staging succeeds.
    std::fs::copy(src, &staged).map_err(|e| {
        SetupError::Io(format!(
            "Cannot stage downloaded model file {}: {}",
            dst.display(),
            e
        ))
    })?;

    let install = match std::fs::rename(&staged, dst) {
        Ok(()) => Ok(()),
        Err(rename_error) if dst.exists() => {
            // Windows cannot rename over an existing file. Move the old
            // destination to a sibling backup, install the staged file, and
            // restore the backup if the replacement fails.
            let backup = dst.with_file_name(format!(
                ".{file_name}.leindex-backup-{}-{install_id}",
                std::process::id()
            ));
            let _ = std::fs::remove_file(&backup);
            if let Err(error) = std::fs::rename(dst, &backup) {
                let _ = std::fs::remove_file(&staged);
                return Err(SetupError::Io(format!(
                    "Cannot prepare replacement for {}: {}",
                    dst.display(),
                    error
                )));
            }
            match std::fs::rename(&staged, dst) {
                Ok(()) => {
                    let _ = std::fs::remove_file(&backup);
                    Ok(())
                }
                Err(error) => match std::fs::rename(&backup, dst) {
                    Ok(()) => Err(SetupError::Io(format!(
                        "Cannot install downloaded model file {} after staging: {} (initial rename: {})",
                        dst.display(),
                        error,
                        rename_error
                    ))),
                    Err(restore_error) => Err(SetupError::Io(format!(
                        "Cannot install downloaded model file {} after staging: {}; restoring the previous file also failed: {} (initial rename: {})",
                        dst.display(),
                        error,
                        restore_error,
                        rename_error
                    ))),
                },
            }
        }
        Err(error) => Err(SetupError::Io(format!(
            "Cannot install downloaded model file {} after staging: {}",
            dst.display(),
            error
        ))),
    };
    if install.is_err() {
        let _ = std::fs::remove_file(&staged);
    }
    install
}

pub(super) fn generate_profile_checksum_manifest(
    model_dir: &Path,
    profile: ModelDownloadProfile,
) -> Result<(), SetupError> {
    use crate::cli::leindex::model_download::sha256_of_file;

    let mut manifest = format!("# source: {}@{}\n", profile.repository, profile.revision);
    for file in [profile.local_model, "tokenizer.json", "config.json"] {
        let path = model_dir.join(file);
        let hash = sha256_of_file(&path)
            .map_err(|e| SetupError::Io(format!("Cannot checksum {}: {}", path.display(), e)))?;
        manifest.push_str(&format!("{}  {}\n", hash, file));
    }
    std::fs::write(model_dir.join("checksums.sha256"), manifest)
        .map_err(|e| SetupError::Io(format!("Cannot write model checksums: {}", e)))
}

pub(super) fn ensure_hugging_face_model_present(
    profile: ModelDownloadProfile,
    model_dir: &Path,
) -> Result<bool, SetupError> {
    if profile_assets_verified(model_dir, profile) {
        println!(
            "  -> {} already present; all model checksums verified.",
            profile.local_model
        );
        return Ok(true);
    }

    let hf = ensure_hugging_face_cli()?;
    let staging = model_dir.join(format!(
        ".hf-download-{}-{}",
        std::process::id(),
        next_model_install_id()
    ));
    let _ = std::fs::remove_dir_all(&staging);
    std::fs::create_dir_all(&staging).map_err(|e| {
        SetupError::Io(format!(
            "Cannot create Hugging Face staging directory {}: {}",
            staging.display(),
            e
        ))
    })?;

    println!(
        "Downloading {} with Hugging Face CLI...",
        profile.repository
    );
    let output = Command::new(&hf)
        .arg("download")
        .arg(profile.repository)
        .args(profile.files)
        .arg("--revision")
        .arg(profile.revision)
        .arg("--local-dir")
        .arg(&staging)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .output()
        .map_err(|e| SetupError::Io(format!("Failed to run {}: {}", hf, e)))?;

    if !output.status.success() {
        let details = truncate_for_error(&format!(
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        ));
        let _ = std::fs::remove_dir_all(&staging);
        return Err(SetupError::HuggingFaceDownloadFailed {
            repository: profile.repository.to_string(),
            exit_code: output.status.code().unwrap_or(-1),
            output: details,
        });
    }

    // Install within a closure so the staging dir is cleaned on every path
    // (success or install failure) before the error, if any, propagates.
    let install_result = (|| -> Result<(), SetupError> {
        install_downloaded_model_file(
            &staging.join(profile.remote_model),
            &model_dir.join(profile.local_model),
        )?;
        for file in ["tokenizer.json", "config.json"] {
            install_downloaded_model_file(&staging.join(file), &model_dir.join(file))?;
        }
        Ok(())
    })();
    let _ = std::fs::remove_dir_all(&staging);
    install_result?;

    if !dynamic_model_assets_present(model_dir) {
        return Err(SetupError::ModelUnavailable {
            model_name: profile.local_model.trim_end_matches(".onnx").to_string(),
            model_dir: model_dir.to_path_buf(),
        });
    }
    generate_profile_checksum_manifest(model_dir, profile)?;
    println!("  -> Model files ready at {}", model_dir.display());
    Ok(true)
}

pub(super) fn ensure_model_file_present(
    file: &crate::cli::leindex::model_download::ModelFile,
    model_dir: &Path,
    manifest_path: &Path,
    manifest_str: &str,
    manifest: &std::collections::HashMap<String, String>,
    model_onnx_name: &str,
) -> Result<(bool, bool), SetupError> {
    use crate::cli::leindex::model_download::{
        CheckResult, DEFAULT_DOWNLOAD_RETRIES, DownloadOutcome, check_file_against_manifest,
        download_file_with_retry, parse_checksums,
    };

    let dest = model_dir.join(file.local);
    let required = file.local != "checksums.sha256" && file.local != "LICENSE";
    let is_model = file.local == model_onnx_name;

    match check_file_against_manifest(&dest, manifest)
        .map_err(|e| SetupError::Io(format!("Cannot stat {}: {}", dest.display(), e)))?
    {
        CheckResult::Verified => {
            println!("  -> {} already present, checksum verified.", file.local);
            return Ok((false, is_model));
        }
        CheckResult::NoEntry if dest.exists() => {
            println!(
                "  -> {} present (no checksum entry; cannot verify).",
                file.local
            );
            return Ok((false, is_model));
        }
        CheckResult::Mismatch { expected, actual } => {
            println!(
                "  -> WARNING: {} checksum mismatch (expected {}..., got {}...).",
                file.local,
                short_hash(&expected),
                short_hash(&actual)
            );
            println!("     Removing corrupt file and re-downloading...");
            let _ = std::fs::remove_file(&dest);
        }
        CheckResult::NoEntry | CheckResult::Missing => {}
    }

    let outcome: DownloadOutcome = match download_file_with_retry(
        file,
        model_dir,
        Some(manifest_path),
        DEFAULT_DOWNLOAD_RETRIES,
    ) {
        Ok(outcome) => outcome,
        Err(_) if !required => {
            println!(
                "  -> {} not available on the CDN; skipping (non-fatal).",
                file.local
            );
            return Ok((false, false));
        }
        Err(error) => return Err(map_model_download_error(error)),
    };

    let fresh_manifest_str =
        std::fs::read_to_string(manifest_path).unwrap_or_else(|_| manifest_str.to_string());
    let fresh_manifest = parse_checksums(&fresh_manifest_str);
    match check_file_against_manifest(&outcome.path, &fresh_manifest)
        .unwrap_or(CheckResult::Missing)
    {
        CheckResult::Verified => {
            println!("  -> {} downloaded, checksum verified.", file.local);
        }
        CheckResult::NoEntry => {
            println!(
                "  -> {} downloaded (no checksum entry in manifest; cannot verify).",
                file.local
            );
        }
        CheckResult::Mismatch { expected, actual } if required => {
            return Err(SetupError::ModelChecksumPostDownload {
                file: file.local.to_string(),
                expected,
                actual,
            });
        }
        CheckResult::Mismatch { .. } => {
            println!(
                "  -> WARNING: {} downloaded but checksum mismatch; \
                 keeping anyway (non-required file).",
                file.local
            );
        }
        CheckResult::Missing => {
            return Err(SetupError::Io(format!(
                "Download reported success but file is missing: {}",
                outcome.path.display()
            )));
        }
    }

    Ok((true, is_model))
}

/// Provision the configured embedding model.
///
/// Current provider profiles use the validated single-file dynamic Qwen3
/// export and download it with the Hugging Face CLI. A locally generated
/// checksum manifest protects subsequent setup and check runs. The legacy
/// fixed-model path remains below only for existing pre-1.8.4 configurations;
/// release artifacts never contain model files.
pub(super) fn ensure_models_present(
    provider: Option<ExecutionProvider>,
    model_name: &str,
) -> Result<bool, SetupError> {
    use crate::cli::leindex::model_download::{
        self, DYNAMIC_MODEL_ONNX_FILENAME, iter_model_files, parse_checksums,
    };

    let legacy_model_name = model_download::MODEL_ONNX_FILENAME
        .strip_suffix(".onnx")
        .unwrap_or(model_download::MODEL_ONNX_FILENAME);
    let dynamic_model_name = DYNAMIC_MODEL_ONNX_FILENAME
        .strip_suffix(".onnx")
        .unwrap_or(DYNAMIC_MODEL_ONNX_FILENAME);
    if model_name != legacy_model_name && model_name != dynamic_model_name {
        return Err(SetupError::InvalidModelName {
            model_name: model_name.to_string(),
            accepted_names: format!("'{}' or '{}'", legacy_model_name, dynamic_model_name),
        });
    }

    let model_dir = crate::cli::neural_config::model_dir_path()
        .ok_or_else(|| SetupError::Io("Cannot resolve model directory".to_string()))?;

    // Create model directory up front so subsequent file operations can rely
    // on it existing.
    std::fs::create_dir_all(&model_dir)
        .map_err(|e| SetupError::Io(format!("Cannot create model dir: {}", e)))?;

    let model_filename = format!("{}.onnx", model_name);
    if model_filename == DYNAMIC_MODEL_ONNX_FILENAME {
        return ensure_hugging_face_model_present(model_download_profile(provider), &model_dir);
    }

    let manifest_path = model_dir.join("checksums.sha256");
    let model_onnx_name = model_download::MODEL_ONNX_FILENAME;

    // ── Step 1: copy from bundled location if present ────────────────────
    // Bundled files (GitHub Release bundle layout) are a no-network fast path.
    // We link (symlink > hardlink > copy) each missing file from the bundle,
    // then fall through to the network path only for anything still missing
    // or corrupted.
    //
    // LEINDEX_SKIP_MODEL_COPY: when set, copy_bundled_models only creates
    // symlinks (no copy fallback). This is used by tests/CI to avoid
    // duplicating the 569 MB model into temp directories.
    if let Some(bundled_dir) = model_download::find_bundled_models() {
        copy_bundled_models(&bundled_dir, &model_dir);
    }

    // ── Step 2: download / verify every file ────────────────────────────
    // The model triplet is split into "required" (onnx, tokenizer, config)
    // and "optional" (checksums.sha256, LICENSE) files. The onnx-community
    // HuggingFace repo does NOT ship `checksums.sha256` or `LICENSE`, so those
    // downloads tolerate 404 / network failure. When the manifest is missing
    // we generate it locally after the required downloads succeed, so
    // second-run verification (VAL-SETUP-017) still works.
    //
    // Per-file strategy:
    //   - Verified (checksum matches)   -> skip (VAL-SETUP-017)
    //   - NoEntry (no checksum to cmp)  -> keep if present, else download
    //   - Mismatch (checksum differs)   -> delete + re-download (VAL-SETUP-018)
    //   - Missing                       -> download (VAL-SETUP-016)
    let manifest_str = std::fs::read_to_string(&manifest_path).unwrap_or_default();
    let manifest = parse_checksums(&manifest_str);

    let mut downloaded_any = false;
    let mut model_present_after = false;

    for file in iter_model_files() {
        let (downloaded, model_present) = ensure_model_file_present(
            file,
            &model_dir,
            &manifest_path,
            &manifest_str,
            &manifest,
            model_onnx_name,
        )?;
        downloaded_any |= downloaded;
        model_present_after |= model_present;
    }

    // ── Step 3: ensure a checksum manifest exists for future runs ────────
    // If we never obtained checksums.sha256 from the CDN (the onnx-community
    // repo does not host one), generate one locally from the files we just
    // downloaded. This makes VAL-SETUP-017 work on the second run: the
    // locally-generated manifest becomes the source of truth, and any future
    // corruption (VAL-SETUP-018) is detected against it.
    if !manifest_path.exists() {
        if let Err(e) = generate_local_checksum_manifest(&model_dir) {
            eprintln!(
                "warning: could not generate local checksum manifest ({}); \
                 future runs cannot verify file integrity until checksums.sha256 \
                 is present in {}.",
                e,
                model_dir.display()
            );
        } else {
            println!("  -> Generated local checksums.sha256 for future verification.");
        }
    }

    if downloaded_any {
        println!("\nModel files ready at {}", model_dir.display());
    }

    Ok(model_present_after)
}

/// Write a `checksums.sha256` file into `model_dir` by computing SHA256 of
/// each model file present. Used when the onnx-community CDN does not host a
/// manifest (it does not), so that subsequent setup runs can verify file
/// integrity (VAL-SETUP-017) and detect corruption (VAL-SETUP-018).
pub(super) fn generate_local_checksum_manifest(model_dir: &std::path::Path) -> std::io::Result<()> {
    use crate::cli::leindex::model_download::{iter_model_files, sha256_of_file};
    let manifest_path = model_dir.join("checksums.sha256");
    let mut out = String::new();
    for file in iter_model_files() {
        if file.local == "checksums.sha256" {
            continue;
        }
        let path = model_dir.join(file.local);
        if path.exists() {
            let hash = sha256_of_file(&path)?;
            out.push_str(&hash);
            out.push_str("  ");
            out.push_str(file.local);
            out.push('\n');
        }
    }
    if out.is_empty() {
        return Ok(());
    }
    std::fs::write(&manifest_path, out)
}

/// Link or copy every model file present in `bundled_dir` into `dest_dir`,
/// skipping any that already exist in `dest_dir`.
///
/// **Resource-duplication fix (Bug 3):** Previously this function unconditionally
/// called `std::fs::copy`, which duplicated the 569 MB `qwen3-embed-0.6b.onnx`
/// into every `LEINDEX_HOME/models/` temp directory. During test runs, 47 temp
/// dirs accumulated in `/tmp` (tmpfs), consuming 18.6 GB of RAM-backed storage.
///
/// The new strategy avoids copying heavyweight model files whenever possible:
///
/// 1. **Symlink** (preferred): zero memory, zero disk overhead. Used when source
///    and destination are on the same filesystem (the common case for bundled
///    installs). `symlink()` is tried first because it works across all
///    same-filesystem and even cross-filesystem scenarios on Linux.
/// 2. **Hardlink** (fallback): zero memory overhead, shares inodes. Used when
///    `symlink()` fails (e.g., cross-filesystem) but `link()` succeeds (same
///    filesystem). Each hardlink shares the same inode = zero memory overhead.
/// 3. **Copy** (last resort): full byte copy. Used only when both `symlink()`
///    and `link()` fail (genuinely cross-filesystem scenario, e.g., copying
///    from a USB-mounted bundle to `/tmp`).
///
/// Small metadata files (`config.json`, `checksums.sha256`, `LICENSE`) are
/// cheap to copy and symlink/hardlink is preferred for them too for consistency.
///
/// When `LEINDEX_SKIP_MODEL_COPY` environment variable is set (any non-empty
/// value), the function only creates symlinks (no hardlink or copy fallback).
/// This is intended for test/CI environments where the bundled models directory
/// is the repo `models/` directory and should be referenced in-place.
pub(super) fn copy_bundled_models(bundled_dir: &std::path::Path, dest_dir: &std::path::Path) {
    let skip_copy = std::env::var("LEINDEX_SKIP_MODEL_COPY")
        .map(|v| !v.trim().is_empty())
        .unwrap_or(false);

    let mut linked_any = false;
    for file in crate::cli::leindex::model_download::iter_model_files() {
        let src = bundled_dir.join(file.local);
        let dst = dest_dir.join(file.local);
        if src.exists() && !dst.exists() {
            if !linked_any {
                println!(
                    "  -> Linking bundled model files from {}...",
                    bundled_dir.display()
                );
                linked_any = true;
            }

            // Resolve symlinks on the source so we link to the real file.
            // This matters when the bundled dir itself contains symlinks
            // (e.g., release-bundle layout where models/ has symlinks to
            // a shared storage location).
            let src_resolved = std::fs::canonicalize(&src).unwrap_or_else(|_| src.clone());

            let linked = try_link_model_file(&src_resolved, &dst, skip_copy);
            if let Err(e) = linked {
                if skip_copy {
                    // LEINDEX_SKIP_MODEL_COPY is set: do not fall back to copy.
                    // Log a warning so the user knows the file was skipped.
                    eprintln!(
                        "warning: LEINDEX_SKIP_MODEL_COPY is set and symlink/hardlink failed for {} ({}); \
                         skipping file (will be resolved at download stage if missing).",
                        file.local, e
                    );
                } else {
                    eprintln!(
                        "warning: failed to link {} from bundle ({}); will download instead.",
                        file.local, e
                    );
                }
            }
        }
    }
}

/// Create a symlink at `dst` pointing to `src`, using the platform-appropriate
/// API. Cfg-gated so the function (and `try_link_model_file`) compiles on
/// Windows, where `std::os::unix` does not exist.
#[cfg(unix)]
pub(super) fn try_symlink_model_file(src: &Path, dst: &Path) -> std::io::Result<()> {
    std::os::unix::fs::symlink(src, dst)
}

#[cfg(windows)]
pub(super) fn try_symlink_model_file(src: &Path, dst: &Path) -> std::io::Result<()> {
    std::os::windows::fs::symlink_file(src, dst)
}

#[cfg(not(any(unix, windows)))]
pub(super) fn try_symlink_model_file(_src: &Path, _dst: &Path) -> std::io::Result<()> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "symlinks are not supported on this platform",
    ))
}

/// Try to link a model file using symlink > hardlink > copy strategy.
///
/// Returns `Ok(())` on success, or an `Err` describing why all strategies
/// failed (used for logging by the caller).
pub(super) fn try_link_model_file(
    src: &std::path::Path,
    dst: &std::path::Path,
    skip_copy: bool,
) -> std::io::Result<()> {
    // Ensure the parent directory exists.
    if let Some(parent) = dst.parent() {
        std::fs::create_dir_all(parent)?;
    }

    // Strategy 1: Symlink (preferred, zero overhead, works cross-filesystem on Linux).
    // Symlinks are the best option because they reference the source file by path
    // without duplicating any data. They work even across filesystem boundaries.
    // The platform-appropriate API is selected by `try_symlink_model_file` so
    // this compiles on Windows (no `std::os::unix`).
    if try_symlink_model_file(src, dst).is_ok() {
        return Ok(());
    }

    // Strategy 2: Hardlink (zero memory overhead, shares inodes).
    // Only works on the same filesystem. `link()` on Unix, `fs::hard_link()`
    // cross-platform wrapper in std.
    match std::fs::hard_link(src, dst) {
        Ok(()) => return Ok(()),
        Err(e) => {
            // Fall through to copy if allowed.
            if skip_copy {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::Unsupported,
                    format!(
                        "LEINDEX_SKIP_MODEL_COPY is set but symlink and hardlink both failed: {}",
                        e
                    ),
                ));
            }
        }
    }

    // Strategy 3: Copy (last resort, full byte duplication).
    // Only reached when symlink and hardlink both fail AND skip_copy is false.
    std::fs::copy(src, dst).map(|_| ())
}

/// Strip a SHA256 hex string down to its first 12 chars for compact logging.
pub(super) fn short_hash(hash: &str) -> String {
    if hash.len() <= 12 {
        hash.to_string()
    } else {
        format!("{}...", &hash[..12])
    }
}

/// Convert a [`model_download::ModelDownloadError`] into the equivalent
/// [`SetupError`] so the caller-facing Display impl stays uniform.
pub(super) fn map_model_download_error(
    e: crate::cli::leindex::model_download::ModelDownloadError,
) -> SetupError {
    use crate::cli::leindex::model_download::ModelDownloadError as Mde;
    match e {
        Mde::CurlNotFound => SetupError::CurlNotFound,
        Mde::Io(path, msg) => SetupError::Io(format!("{}: {}", path.display(), msg)),
        Mde::ChecksumMismatch {
            file,
            expected,
            actual,
        } => SetupError::ModelDownloadFailed {
            file,
            url: format!("checksum expected {}, got {}", expected, actual),
            exit_code: -1,
            network: false,
        },
        Mde::DownloadFailed {
            file,
            url,
            exit_code,
            network,
        } => SetupError::ModelDownloadFailed {
            file,
            url,
            exit_code,
            network,
        },
    }
}

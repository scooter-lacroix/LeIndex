// Model file download and SHA256 checksum verification for `leindex setup`.
//
// This module owns the model-fetching logic invoked by `ensure_models_present()`
// in `setup.rs`. It downloads the Qwen3-Embedding-0.6B-ONNX model triplet
// (onnx weights + tokenizer + config + LICENSE + checksums.sha256) from the
// HuggingFace CDN to `~/.leindex/models/` (or `$LEINDEX_HOME/models/`).
//
// VAL-SETUP-016: First run downloads the model with progress shown.
// VAL-SETUP-017: Second run skips when the checksum matches.
// VAL-SETUP-018: Checksum failure triggers a re-download.
// VAL-SETUP-019: Network error surfaces an actionable error (no hang/panic).
//
// Design notes
// ------------
// * Downloads use `curl` as a subprocess, mirroring the existing
//   `scripts/download-models.sh` and `.github/workflows/release.yml` pattern.
//   curl ships with macOS, modern Windows, and almost every Linux distro, and
//   its `--progress-bar` / `--retry` flags give us solid behavior for free.
// * SHA256 verification uses the pure-Rust `sha2` crate (no shell-out) so the
//   checksum-matching logic is unit-testable on every platform.
// * Each file is downloaded to `<dest>.tmp` and renamed only after the curl
//   call returns success, so a failed download never leaves a partial,
//   zero-byte, or half-finished file at the canonical path (the contract
//   requires "no partial model file left at the target path").

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use sha2::{Digest, Sha256};

/// HuggingFace repository that hosts the ONNX model triplet.
///
/// Matches `HF_REPO` in `.github/workflows/release.yml` and
/// `scripts/download-models.sh` so the runtime setup path and the CI/release
/// path pull from exactly the same source.
pub const MODEL_HF_REPO: &str = "onnx-community/Qwen3-Embedding-0.6B-ONNX";

/// The on-disk filename of the embedding model file.
pub const MODEL_ONNX_FILENAME: &str = "qwen3-embed-0.6b.onnx";

/// File listing within the bundled checksum manifest. The trailing
/// `(local_filename, remote_subpath)` pairs let the local layout (flat
/// `~/.leindex/models/qwen3-embed-0.6b.onnx`) diverge from the HuggingFace
/// repo layout (ONNX weights live under `onnx/model.onnx`).
const MODEL_FILES: &[ModelFile; 5] = &[
    ModelFile {
        local: "qwen3-embed-0.6b.onnx",
        remote: "onnx/model.onnx",
        // 596 MiB. Used only for progress-bar sizing heuristics; the model is
        // verified by checksum, not by exact byte count.
        approx_bytes: 596_314_328,
    },
    ModelFile {
        local: "tokenizer.json",
        remote: "tokenizer.json",
        approx_bytes: 11_423_705,
    },
    ModelFile {
        local: "config.json",
        remote: "config.json",
        approx_bytes: 727,
    },
    ModelFile {
        local: "checksums.sha256",
        remote: "checksums.sha256",
        approx_bytes: 256,
    },
    ModelFile {
        local: "LICENSE",
        remote: "LICENSE",
        approx_bytes: 11_000,
    },
];

/// A single file in the model triplet, with its local name and remote path.
#[derive(Debug, Clone, Copy)]
pub struct ModelFile {
    /// Filename on disk under `~/.leindex/models/`.
    pub local: &'static str,
    /// Path *relative to the HuggingFace repo root*. The URL is built as
    /// `https://huggingface.co/{MODEL_HF_REPO}/resolve/main/{remote}`.
    pub remote: &'static str,
    /// Approximate expected size in bytes. Only used for progress messaging.
    pub approx_bytes: u64,
}

/// Default per-file retry count for downloads.
///
/// VAL-SETUP-019: transient network failures must produce a clear error after
/// exhausting retries, not hang silently.
pub const DEFAULT_DOWNLOAD_RETRIES: u32 = 3;

/// Delay between download retries. curl also performs internal retries for
/// transient HTTP errors via `--retry`; this delay applies between our own
/// outer-level attempts.
pub const RETRY_DELAY: Duration = Duration::from_secs(2);

/// Build the HuggingFace CDN URL for a remote sub-path under
/// `MODEL_HF_REPO`. Exposed so unit tests can pin the exact URL scheme.
///
/// ```ignore
/// assert_eq!(
///     hf_url("tokenizer.json").as_str(),
///     "https://huggingface.co/onnx-community/Qwen3-Embedding-0.6B-ONNX/resolve/main/tokenizer.json",
/// );
/// ```
pub fn hf_url(remote_subpath: &str) -> String {
    format!(
        "https://huggingface.co/{}/resolve/main/{}",
        MODEL_HF_REPO,
        remote_subpath.trim_start_matches('/')
    )
}

/// Parsed `checksums.sha256` manifest: maps `filename -> lowercase hex sha256`.
///
/// Lines have the shape `<64-hex>  <filename>` (two-space separator, as
/// produced by GNU `sha256sum`). Single-space and `*<filename>` (binary-mode)
/// forms are also accepted.
pub fn parse_checksums(contents: &str) -> std::collections::HashMap<String, String> {
    let mut out = std::collections::HashMap::new();
    for raw in contents.lines() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        // Each entry is `<hash><whitespace><flag?><filename>`.
        let mut iter = line.split_whitespace();
        let Some(hash) = iter.next() else {
            continue;
        };
        let Some(rest) = iter.next() else {
            continue;
        };
        // GNU sha256sum emits `./<name>` or `<name>`; in binary mode `*<name>`.
        let name = rest.trim_start_matches('*').trim_start_matches("./");
        // Reject malformed hashes; they would cause false mismatches downstream.
        let is_hex_64 = hash.len() == 64 && hash.as_bytes().iter().all(|b| b.is_ascii_hexdigit());
        if is_hex_64 {
            out.insert(name.to_string(), hash.to_ascii_lowercase());
        }
    }
    out
}

/// Compute the SHA256 of a file, returning the lowercase hex digest.
///
/// Used both for first-run verification (after a fresh download) and for the
/// second-run "is the existing file still valid?" check.
pub fn sha256_of_file(path: &Path) -> std::io::Result<String> {
    use std::io::Read;
    let mut file = std::fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buf = [0u8; 64 * 1024];
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hex_lower(&hasher.finalize()))
}

/// Encode bytes as lowercase hex.
pub fn hex_lower(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    const HEX: &[u8; 16] = b"0123456789abcdef";
    for b in bytes {
        s.push(HEX[(b >> 4) as usize] as char);
        s.push(HEX[(b & 0x0f) as usize] as char);
    }
    s
}

/// Outcome of `check_file_against_manifest`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CheckResult {
    /// File exists and its checksum matches the manifest entry.
    Verified,
    /// File exists but the manifest has no entry for it. Nothing to compare
    /// against; we keep the file as-is.
    NoEntry,
    /// File exists but its computed checksum differs from the manifest.
    /// VAL-SETUP-018: caller must re-download.
    Mismatch { expected: String, actual: String },
    /// File is absent.
    Missing,
}

/// Compare a file on disk against the parsed checksum manifest.
pub fn check_file_against_manifest(
    path: &Path,
    manifest: &std::collections::HashMap<String, String>,
) -> std::io::Result<CheckResult> {
    let filename = match path.file_name().and_then(|n| n.to_str()) {
        Some(n) => n,
        None => return Ok(CheckResult::Missing),
    };

    if !path.exists() {
        return Ok(CheckResult::Missing);
    }

    let Some(expected) = manifest.get(filename) else {
        return Ok(CheckResult::NoEntry);
    };

    let actual = sha256_of_file(path)?;
    if &actual == expected {
        Ok(CheckResult::Verified)
    } else {
        Ok(CheckResult::Mismatch {
            expected: expected.clone(),
            actual,
        })
    }
}

/// Find the curl executable. Returns the program path if `curl --version`
/// exits 0.
pub fn find_curl() -> Option<&'static str> {
    let candidate = "curl";
    let ok = Command::new(candidate)
        .arg("--version")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    if ok {
        Some(candidate)
    } else {
        None
    }
}

/// Heuristic to classify a curl failure as a network/connectivity problem.
///
/// VAL-SETUP-019: network failures must surface an actionable, connectivity-
/// themed message rather than a raw exit code. curl's documented exit codes
/// below cover DNS resolution, TCP connect, TLS handshake, timeouts, and
/// "received nothing" / "open socket failed" failures.
pub fn is_curl_network_error(exit_code: i32, stderr: &str) -> bool {
    matches!(
        exit_code,
        4 |   // --fail with HTTP code >= 400 (treat as upstream/network)
        5 |   // could not resolve proxy
        6 |   // could not resolve host
        7 |   // failed to connect to host
        28 |  // operation timeout
        35 |  // SSL connect error
        52 |  // got nothing (server returned no data)
        56 |  // failure receiving network data
        92 // stream error in HTTP/2 framing layer (CDN-side)
    ) || {
        let lower = stderr.to_ascii_lowercase();
        const HINTS: &[&str] = &[
            "could not resolve host",
            "connection refused",
            "connection timed out",
            "connection reset",
            "network is unreachable",
            "no route to host",
            "ssl connect error",
            "failed to connect",
            "timeout",
            "name resolution",
            "name or service not known",
            "temporary failure in name resolution",
        ];
        HINTS.iter().any(|h| lower.contains(h))
    }
}

/// Outcome of a single file download.
#[derive(Debug, Clone)]
#[allow(dead_code)] // Fields are surfaced via the public API for callers/tests.
pub struct DownloadOutcome {
    /// Final path of the downloaded file.
    pub path: PathBuf,
    /// Computed SHA256 of the downloaded bytes (lowercase hex), if available.
    pub sha256: Option<String>,
    /// Number of attempts used (1 = first try succeeded).
    pub attempts: u32,
    /// True if the manifest (already on disk before this download) verifies
    /// the file. The caller can use this to surface "checksum OK" messaging.
    pub verified: bool,
}

/// Download one model file to `dest_dir`, retrying on transient failures.
///
/// `manifest` (if present) is consulted so the caller can report checksum
/// status immediately after the download succeeds. The manifest itself is
/// re-read from disk after every download so a freshly-fetched
/// `checksums.sha256` is honoured.
///
/// VAL-SETUP-016: prints progress (`curl --progress-bar` writes to stderr).
/// VAL-SETUP-019: returns a `ModelDownloadError::Network` on connectivity
/// failures after `retries`, with the offending URL included for the user.
pub fn download_file_with_retry(
    file: &ModelFile,
    dest_dir: &Path,
    manifest_path: Option<&Path>,
    retries: u32,
) -> Result<DownloadOutcome, ModelDownloadError> {
    let curl = find_curl().ok_or(ModelDownloadError::CurlNotFound)?;

    let dest = dest_dir.join(file.local);
    let tmp_dest = dest_dir.join(format!("{}.tmp", file.local));

    std::fs::create_dir_all(dest_dir)
        .map_err(|e| ModelDownloadError::Io(dest_dir.to_path_buf(), e.to_string()))?;

    let url = hf_url(file.remote);

    let mut last_err: Option<ModelDownloadError> = None;
    for attempt in 1..=retries.max(1) {
        // Remove any stale temp file from a previous attempt so a partial
        // download cannot be mistaken for a complete one.
        let _ = std::fs::remove_file(&tmp_dest);

        if attempt > 1 {
            println!(
                "  -> Retry {}/{} for {} ...",
                attempt,
                retries.max(1),
                file.local
            );
            std::thread::sleep(RETRY_DELAY);
        }

        println!(
            "  -> Downloading {} ({}) ...",
            file.local,
            human_bytes(file.approx_bytes)
        );

        let status = Command::new(curl)
            .args([
                "--fail",
                "--location",
                "--progress-bar",
                "--connect-timeout",
                "30",
                "--max-time",
                "600",
                "--retry",
                "3",
                "--retry-delay",
                "5",
                "--retry-connrefused",
                "--retry-all-errors",
                "-o",
            ])
            .arg(&tmp_dest)
            .arg(&url)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::inherit())
            .stderr(std::process::Stdio::inherit())
            .status();

        let exit = match status {
            Ok(s) => s,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                // curl disappeared between the find_curl() probe and now.
                last_err = Some(ModelDownloadError::CurlNotFound);
                continue;
            }
            Err(e) => {
                last_err = Some(ModelDownloadError::Io(dest.clone(), e.to_string()));
                continue;
            }
        };

        if !exit.success() {
            let code = exit.code().unwrap_or(-1);
            let networkish = is_curl_network_error(code, "");
            last_err = Some(ModelDownloadError::DownloadFailed {
                file: file.local.to_string(),
                url: url.clone(),
                exit_code: code,
                network: networkish,
            });
            // Network errors are retried implicitly by curl's own --retry,
            // but we surface our outer retry for the user to see progress.
            continue;
        }

        // curl succeeded: move the temp file into place.
        if let Err(e) = std::fs::rename(&tmp_dest, &dest) {
            // rename can fail across filesystems; fall back to copy+remove.
            if let Err(copy_err) = std::fs::copy(&tmp_dest, &dest) {
                let _ = std::fs::remove_file(&tmp_dest);
                last_err = Some(ModelDownloadError::Io(dest.clone(), copy_err.to_string()));
                continue;
            }
            let _ = std::fs::remove_file(&tmp_dest);
            // copy succeeded; fall through with the rename-error discarded.
            let _ = e;
        }

        // Compute the post-download checksum. We do this even before the
        // manifest arrives so a caller asking for "did the network round-trip
        // produce a deterministic file?" gets an answer.
        let sha = sha256_of_file(&dest).ok();

        // Re-read the manifest in case `checksums.sha256` was a newly
        // downloaded sibling.
        let manifest_now = manifest_path
            .and_then(|p| std::fs::read_to_string(p).ok())
            .map(|c| parse_checksums(&c))
            .unwrap_or_default();

        let verified = sha
            .as_ref()
            .and_then(|h| manifest_now.get(file.local).map(|exp| exp == h))
            .unwrap_or(false);

        return Ok(DownloadOutcome {
            path: dest,
            sha256: sha,
            attempts: attempt,
            verified,
        });
    }

    // All retries exhausted.
    let _ = std::fs::remove_file(&tmp_dest);
    // VAL-SETUP-019: leave no partial file at the canonical path.
    let _ = std::fs::remove_file(&dest);

    Err(last_err.unwrap_or(ModelDownloadError::DownloadFailed {
        file: file.local.to_string(),
        url: url.clone(),
        exit_code: -1,
        network: false,
    }))
}

/// Human-readable byte size for progress logging ("596.3 MiB").
fn human_bytes(n: u64) -> String {
    const KIB: u64 = 1024;
    const MIB: u64 = KIB * 1024;
    const GIB: u64 = MIB * 1024;
    if n >= GIB {
        format!("{:.2} GiB", n as f64 / GIB as f64)
    } else if n >= MIB {
        format!("{:.1} MiB", n as f64 / MIB as f64)
    } else if n >= KIB {
        format!("{:.1} KiB", n as f64 / KIB as f64)
    } else {
        format!("{} B", n)
    }
}

/// Errors that can occur while fetching model files.
#[derive(Debug)]
pub enum ModelDownloadError {
    /// curl is not available on PATH.
    CurlNotFound,
    /// curl returned a non-zero status.
    DownloadFailed {
        file: String,
        url: String,
        exit_code: i32,
        network: bool,
    },
    /// Filesystem I/O error.
    Io(PathBuf, String),
}

impl std::fmt::Display for ModelDownloadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ModelDownloadError::CurlNotFound => {
                write!(
                    f,
                    "curl not found on PATH. curl is required to download model \
                     files. Install curl:\n  \
                     - Debian/Ubuntu: sudo apt install curl\n  \
                     - macOS: curl ships with macOS (verify /usr/bin/curl)\n  \
                     - Windows 10+: curl.exe is preinstalled\n  \
                     Alternatively copy model files manually to ~/.leindex/models/ \
                     and re-run `leindex setup --check`."
                )
            }
            ModelDownloadError::DownloadFailed {
                file,
                url,
                exit_code,
                network,
            } => {
                if *network {
                    write!(
                        f,
                        "Network failure downloading '{}' from {} (curl exit code {}). \
                         Check your internet connection, DNS, proxy settings, or the \
                         HuggingFace CDN status (https://status.huggingface.co). \
                         You can retry with `leindex setup` or download the file \
                         manually and place it under ~/.leindex/models/{}. \
                         Tip: set LEINDEX_MODEL_PATH to use an offline model directory.",
                        file, url, exit_code, file
                    )
                } else {
                    write!(
                        f,
                        "Failed to download '{}' from {} (curl exit code {}). \
                         The file may be temporarily unavailable on the CDN, or the \
                         repo layout changed. Re-run `leindex setup` to retry, or \
                         copy the model manually. If you have an offline copy, set \
                         LEINDEX_MODEL_PATH.",
                        file, url, exit_code
                    )
                }
            }
            ModelDownloadError::Io(path, msg) => {
                write!(
                    f,
                    "I/O error on {}: {}. Check disk space and directory permissions.",
                    path.display(),
                    msg
                )
            }
        }
    }
}

impl std::error::Error for ModelDownloadError {}

/// Iterator over the bundled model file triplet.
pub fn iter_model_files() -> impl Iterator<Item = &'static ModelFile> {
    MODEL_FILES.iter()
}

/// Find the bundled-models directory relative to the running binary, if any.
///
/// This mirrors `find_bundled_models` in `setup.rs`; kept here so the download
/// module can prefer a bundled copy before reaching for the network.
pub fn find_bundled_models() -> Option<PathBuf> {
    if let Ok(exe) = std::env::current_exe() {
        if let Some(parent) = exe.parent() {
            for dir in [
                parent.join("models"),
                parent.join("..").join("models"),
                parent.join("..").join("..").join("models"),
            ] {
                if dir.join(MODEL_ONNX_FILENAME).exists() {
                    return Some(dir);
                }
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_hf_url_format() {
        let u = hf_url("tokenizer.json");
        assert_eq!(
            u,
            "https://huggingface.co/onnx-community/Qwen3-Embedding-0.6B-ONNX/resolve/main/tokenizer.json"
        );

        // Leading slash on the subpath is stripped.
        let u2 = hf_url("/onnx/model.onnx");
        assert_eq!(
            u2,
            "https://huggingface.co/onnx-community/Qwen3-Embedding-0.6B-ONNX/resolve/main/onnx/model.onnx"
        );
    }

    #[test]
    fn test_model_files_include_required_set() {
        // VAL-SETUP-016: the download list must cover the full triplet named
        // in the feature description.
        let names: Vec<&str> = iter_model_files().map(|f| f.local).collect();
        assert!(names.contains(&"qwen3-embed-0.6b.onnx"));
        assert!(names.contains(&"tokenizer.json"));
        assert!(names.contains(&"config.json"));
        assert!(names.contains(&"checksums.sha256"));
        assert!(names.contains(&"LICENSE"));
        assert_eq!(names.len(), 5, "exactly five files expected");
    }

    #[test]
    fn test_onnx_remote_path_is_under_onnx_subdir() {
        // The HuggingFace `onnx-community/*` repos host weights under
        // `onnx/model.onnx`, not at the repo root. CI's release.yml confirms
        // this layout; mirror it exactly so VAL-SETUP-016 downloads succeed.
        let onnx = iter_model_files()
            .find(|f| f.local == MODEL_ONNX_FILENAME)
            .expect("onnx file entry present");
        assert_eq!(onnx.remote, "onnx/model.onnx");
    }

    #[test]
    fn test_parse_checksums_two_space_format() {
        // GNU sha256sum default format (`<hash>  <file>`).
        let contents = "c41936b2ddcb7395d9220d9a805017772d63fb4e286f10bc7423635d200263b1  qwen3-embed-0.6b.onnx\ndef76fb086971c7867b829c23a26261e38d9d74e02139253b38aeb9df8b4b50a  tokenizer.json\n";
        let map = parse_checksums(contents);
        assert_eq!(map.len(), 2);
        assert_eq!(
            map.get("qwen3-embed-0.6b.onnx").unwrap(),
            "c41936b2ddcb7395d9220d9a805017772d63fb4e286f10bc7423635d200263b1"
        );
        assert_eq!(
            map.get("tokenizer.json").unwrap(),
            "def76fb086971c7867b829c23a26261e38d9d74e02139253b38aeb9df8b4b50a"
        );
    }

    #[test]
    fn test_parse_checksums_single_space_and_binary_mode() {
        // Single-space separator and binary `*name` form. The hash must be 64
        // lowercase-hex chars to pass the format check.
        let hash64 = "0f1e2d3c4b5a69788796a5b4c3d2e1f00f1e2d3c4b5a69788796a5b4c3d2e1f0";
        let contents = format!("{} *LICENSE\n", hash64);
        let map = parse_checksums(&contents);
        assert_eq!(map.len(), 1);
        // The leading `*` is stripped from the filename.
        assert!(map.contains_key("LICENSE"));
        assert_eq!(map.get("LICENSE").unwrap(), hash64);
    }

    #[test]
    fn test_parse_checksums_strips_dot_slash_prefix() {
        // `./foo` form is normalised to `foo`.
        let hash64 = "0f1e2d3c4b5a69788796a5b4c3d2e1f00f1e2d3c4b5a69788796a5b4c3d2e1f0";
        let contents = format!("{}  ./config.json\n", hash64);
        let map = parse_checksums(&contents);
        assert_eq!(map.len(), 1);
        assert!(map.contains_key("config.json"));
        assert_eq!(map.get("config.json").unwrap(), hash64);
    }

    #[test]
    fn test_parse_checksums_ignores_blank_and_comment_lines() {
        let contents = "# generated by leindex\n\n  \nbadhash short  foo\n";
        let map = parse_checksums(contents);
        assert!(map.is_empty(), "blank/comment/malformed lines ignored");
    }

    #[test]
    fn test_parse_checksums_rejects_non_hex_hash() {
        // 64 chars but with a non-hex glyph -> dropped.
        let contents =
            "zzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzz  tokenizer.json\n";
        let map = parse_checksums(contents);
        assert!(map.is_empty());
    }

    #[test]
    fn test_parse_checksums_lowercases_hash() {
        // 64 lowercase-hex chars (every other char uppercased for the test).
        // AAAA + 1234567890 + ABCDEF + 1234567890 + ABCDEF + 1234567890 + ABCD
        let hash64 = "AAAA1234567890ABCDEF1234567890AAAA1234567890ABCDEF1234567890AAAA";
        assert_eq!(hash64.len(), 64, "hash is exactly 64 chars");
        let contents = format!("{}  foo\n", hash64);
        let map = parse_checksums(&contents);
        // Map is keyed on the filename; value is lowercased.
        assert_eq!(map.get("foo").unwrap(), &hash64.to_ascii_lowercase());
    }

    #[test]
    fn test_sha256_of_known_bytes() {
        // sha256(b"hello\n") == 5891b5b522d5df086d0ff0b110fbd9d21bb4fc7163af34d08286a2e846f6be03
        let dir = std::env::temp_dir().join(format!("leindex-mdl-test-sha-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("known.txt");
        std::fs::write(&path, b"hello\n").unwrap();
        let hash = sha256_of_file(&path).unwrap();
        assert_eq!(
            hash,
            "5891b5b522d5df086d0ff0b110fbd9d21bb4fc7163af34d08286a2e846f6be03"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_hex_lower_basic() {
        assert_eq!(hex_lower(&[0x00, 0xff, 0xab]), "00ffab");
        assert_eq!(hex_lower(&[]), "");
    }

    #[test]
    fn test_check_result_missing_for_absent_file() {
        let manifest = std::collections::HashMap::new();
        let res =
            check_file_against_manifest(Path::new("/nonexistent/does-not-exist-XYZ"), &manifest)
                .unwrap();
        assert_eq!(res, CheckResult::Missing);
    }

    #[test]
    fn test_check_result_no_entry_when_manifest_lacks_filename() {
        // File is present but the manifest only mentions `tokenizer.json`,
        // not the file's actual name on disk.
        let dir =
            std::env::temp_dir().join(format!("leindex-mdl-test-noentry-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.json");
        std::fs::write(&path, b"contents").unwrap();
        let mut manifest = std::collections::HashMap::new();
        // manifest intentionally lacks "config.json"
        manifest.insert("tokenizer.json".to_string(), "deadbeef".to_string());
        let res = check_file_against_manifest(&path, &manifest).unwrap();
        assert_eq!(res, CheckResult::NoEntry);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_check_result_verified_on_match() {
        // Use a manually-chosen filename written into the temp dir so the
        // manifest lookup by file_name matches.
        let dir =
            std::env::temp_dir().join(format!("leindex-mdl-test-verify-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("tokenizer.json");
        std::fs::write(&path, b"hello\n").unwrap();
        let mut manifest = std::collections::HashMap::new();
        manifest.insert(
            "tokenizer.json".to_string(),
            "5891b5b522d5df086d0ff0b110fbd9d21bb4fc7163af34d08286a2e846f6be03".to_string(),
        );
        let res = check_file_against_manifest(&path, &manifest).unwrap();
        assert_eq!(res, CheckResult::Verified);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_check_result_mismatch_on_wrong_checksum() {
        // VAL-SETUP-018: caller must re-download when this is returned.
        let dir =
            std::env::temp_dir().join(format!("leindex-mdl-test-mismatch-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.json");
        std::fs::write(&path, b"corrupted").unwrap();
        let mut manifest = std::collections::HashMap::new();
        manifest.insert(
            "config.json".to_string(),
            "0000000000000000000000000000000000000000000000000000000000000000".to_string(),
        );
        let res = check_file_against_manifest(&path, &manifest).unwrap();
        match res {
            CheckResult::Mismatch { expected, actual } => {
                assert_eq!(
                    expected,
                    "0000000000000000000000000000000000000000000000000000000000000000"
                );
                // Actual is the real hash of "corrupted": 64 lowercase hex chars,
                // and it must NOT equal the all-zero expected.
                assert_eq!(actual.len(), 64, "sha256 hex digest is 64 chars");
                assert!(
                    actual.chars().all(|c| c.is_ascii_hexdigit()),
                    "actual is lowercase hex"
                );
                assert_ne!(
                    actual,
                    "0000000000000000000000000000000000000000000000000000000000000000"
                );
            }
            other => panic!("expected Mismatch, got {:?}", other),
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_is_curl_network_error_exit_codes() {
        // VAL-SETUP-019: network-coded exit codes are classified as network errs.
        assert!(is_curl_network_error(6, "")); // could not resolve host
        assert!(is_curl_network_error(7, "")); // failed to connect
        assert!(is_curl_network_error(28, "")); // timeout
        assert!(is_curl_network_error(35, "")); // SSL connect error
        assert!(is_curl_network_error(52, "")); // got nothing
        assert!(is_curl_network_error(56, "")); // recv network data
    }

    #[test]
    fn test_is_curl_network_error_stderr_heuristic() {
        assert!(is_curl_network_error(
            1,
            "curl: (6) Could not resolve host: huggingface.co"
        ));
        assert!(is_curl_network_error(1, "Connection timed out"));
        assert!(is_curl_network_error(1, "SSL connect error"));
    }

    #[test]
    fn test_is_curl_network_error_ignores_unrelated_failures() {
        // 22 = --fail with HTTP 4xx/5xx; we don't treat that as a network error
        // because it usually means the URL is genuinely wrong (repo moved).
        assert!(!is_curl_network_error(22, ""));
        assert!(!is_curl_network_error(0, ""));
    }

    #[test]
    fn test_model_download_error_network_message_mentions_connectivity() {
        // VAL-SETUP-019: the Display impl names connectivity AND the
        // LEINDEX_MODEL_PATH remediation hint.
        let err = ModelDownloadError::DownloadFailed {
            file: "qwen3-embed-0.6b.onnx".to_string(),
            url: hf_url("onnx/model.onnx"),
            exit_code: 28,
            network: true,
        };
        let msg = err.to_string();
        assert!(msg.contains("Network failure"), "{}", msg);
        assert!(msg.contains("internet connection"), "{}", msg);
        assert!(msg.contains("LEINDEX_MODEL_PATH"), "{}", msg);
        assert!(msg.contains("huggingface.co"), "{}", msg);
    }

    #[test]
    fn test_model_download_error_curl_not_found_message_mentions_curl() {
        let err = ModelDownloadError::CurlNotFound;
        let msg = err.to_string();
        assert!(msg.contains("curl not found"), "{}", msg);
        assert!(msg.contains("curl"), "{}", msg);
    }

    #[test]
    fn test_model_download_error_io_message_names_path() {
        let err = ModelDownloadError::Io(
            PathBuf::from("/home/user/.leindex/models/tokenizer.json"),
            "permission denied".to_string(),
        );
        let msg = err.to_string();
        assert!(msg.contains("permission denied"));
        assert!(msg.contains("tokenizer.json"));
    }

    #[test]
    fn test_human_bytes_thresholds() {
        assert_eq!(human_bytes(0), "0 B");
        assert_eq!(human_bytes(1023), "1023 B");
        assert_eq!(human_bytes(1024), "1.0 KiB");
        assert_eq!(human_bytes(1024 * 1024), "1.0 MiB");
        assert_eq!(human_bytes(1024 * 1024 * 1024), "1.00 GiB");
        // 569 MiB-ish, the model size.
        assert!(human_bytes(596_314_328).contains("MiB"));
    }

    #[test]
    fn test_default_retries_constant_is_at_least_one() {
        assert!(DEFAULT_DOWNLOAD_RETRIES >= 1);
        // RETRY_DELAY must be nonzero so backoff actually waits.
        assert!(RETRY_DELAY > Duration::from_secs(0));
    }
}

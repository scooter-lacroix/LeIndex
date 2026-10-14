//! Validated, package-owned llama.cpp runtime provisioning for managed services.
//! GitHub's HTTPS release API authenticates the fixed tag's current asset digest;
//! this is not a claim that GitHub release assets are immutable.

use super::SetupError;
use crate::cli::leindex::model_download::sha256_of_file;
use dialoguer::{Input, Select};
use flate2::read::GzDecoder;
use serde::Deserialize;
use std::collections::HashSet;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

const TAG: &str = "b11574";
const API: &str = "https://api.github.com/repos/ggml-org/llama.cpp/releases/tags/b11574";
const MAX_ARCHIVE_BYTES: u64 = 3 * 1024 * 1024 * 1024;
const MAX_EXTRACTED_BYTES: u64 = 6 * 1024 * 1024 * 1024;
const MAX_MEMBERS: usize = 4096;

fn failure(message: impl Into<String>) -> SetupError {
    SetupError::Io(message.into())
}

fn io_context(context: &str, error: impl std::fmt::Display) -> SetupError {
    failure(format!("{context}: {error}"))
}

fn managed_platform() -> Result<(), SetupError> {
    if std::env::consts::OS != "linux" {
        return Err(failure(
            "Managed llama.cpp services currently require Linux; configure an external HTTP service on this platform",
        ));
    }
    Ok(())
}

/// Ask for a user-owned binary/directory or a package-owned official runtime.
pub(super) fn configure_binary(existing: &str) -> Result<String, SetupError> {
    managed_platform()?;
    let choice = Select::new()
        .with_prompt("llama-server runtime")
        .items(&[
            "Use an explicit binary or directory",
            "Download official CPU runtime",
            "Download official Vulkan runtime",
            "Download official CUDA runtime",
        ])
        .default(0)
        .interact()
        .map_err(|e| io_context("Cannot select llama.cpp runtime", e))?;
    if choice == 0 {
        let mut prompt =
            Input::<String>::new().with_prompt("Absolute llama-server binary or runtime directory");
        if Path::new(existing).is_absolute() {
            prompt = prompt.default(existing.to_owned());
        }
        let path = prompt
            .interact_text()
            .map_err(|e| io_context("Cannot read llama-server path", e))?;
        ensure_binary(Path::new(&path))
    } else {
        install_official(["cpu", "vulkan", "cuda"][choice - 1])
    }
}

/// Validate an explicit binary or directory without searching PATH or changing it.
pub(super) fn ensure_binary(path: &Path) -> Result<String, SetupError> {
    managed_platform()?;
    if !path.is_absolute() {
        return Err(failure(
            "llama-server must be an absolute binary or directory path; PATH lookup is not supported",
        ));
    }
    let candidates = if path.is_dir() {
        [path.join("llama-server"), path.join("bin/llama-server")]
            .into_iter()
            .filter(|p| p.is_file())
            .collect::<Vec<_>>()
    } else {
        vec![path.to_path_buf()]
    };
    if candidates.len() != 1 {
        return Err(failure(format!(
            "{} must contain exactly one llama-server at its root or bin/",
            path.display()
        )));
    }
    validate_binary(&candidates[0])
}

fn validate_binary(path: &Path) -> Result<String, SetupError> {
    let canonical = fs::canonicalize(path)
        .map_err(|e| io_context(&format!("Cannot resolve {}", path.display()), e))?;
    let metadata =
        fs::metadata(&canonical).map_err(|e| io_context("Cannot inspect llama-server", e))?;
    if !metadata.is_file() {
        return Err(failure("llama-server is not a regular file"));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if metadata.permissions().mode() & 0o111 == 0 {
            return Err(failure(format!(
                "{} is not executable",
                canonical.display()
            )));
        }
    }
    let binary = canonical
        .to_str()
        .ok_or_else(|| failure("llama-server path is not valid UTF-8"))?
        .to_owned();
    let capture =
        tempfile::tempdir().map_err(|e| io_context("Cannot stage runtime validation", e))?;
    let output = capture.path().join("version");
    let mut command = Command::new(&canonical);
    command.arg("--version").current_dir("/");
    run_bounded(&mut command, &output, 1024 * 1024, Duration::from_secs(15))?;
    if fs::metadata(output)
        .map_err(|e| io_context("Cannot inspect version response", e))?
        .len()
        == 0
    {
        return Err(failure(
            "llama-server --version returned no output; model/API compatibility still requires a service smoke test",
        ));
    }
    Ok(binary)
}

/// Run with bounded time and combined stdout/stderr, avoiding pipe deadlocks.
fn run_bounded(
    command: &mut Command,
    output: &Path,
    max_bytes: u64,
    timeout: Duration,
) -> Result<(), SetupError> {
    let file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(output)
        .map_err(|e| io_context("Cannot create command output", e))?;
    let stderr = file
        .try_clone()
        .map_err(|e| io_context("Cannot capture command errors", e))?;
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::from(file))
        .stderr(Stdio::from(stderr))
        .spawn()
        .map_err(|e| io_context("Cannot execute runtime provisioning command", e))?;
    let deadline = Instant::now() + timeout;
    loop {
        let length = match fs::metadata(output) {
            Ok(metadata) => metadata.len(),
            Err(error) => {
                let _ = terminate_probe(&mut child);
                return Err(io_context("Cannot inspect command output", error));
            }
        };
        if length > max_bytes || Instant::now() >= deadline {
            terminate_probe(&mut child)
                .map_err(|e| io_context("Cannot terminate provisioning command", e))?;
            return Err(failure(
                "Runtime provisioning command exceeded its time or output limit",
            ));
        }
        match child.try_wait() {
            Ok(Some(status)) if status.success() => {
                if fs::metadata(output)
                    .map_err(|e| io_context("Cannot inspect final command output", e))?
                    .len()
                    > max_bytes
                {
                    return Err(failure(
                        "Runtime provisioning command exceeded its output limit",
                    ));
                }
                return Ok(());
            }
            Ok(Some(status)) => {
                return Err(failure(format!(
                    "Runtime provisioning command failed ({status}); check binary libraries, drivers, network access, and selected backend"
                )));
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(20)),
            Err(error) => {
                let _ = terminate_probe(&mut child);
                return Err(io_context("Cannot wait for provisioning command", error));
            }
        }
    }
}

fn terminate_probe(child: &mut std::process::Child) -> io::Result<()> {
    #[cfg(unix)]
    {
        let result = unsafe { libc::kill(-(child.id() as i32), libc::SIGKILL) };
        if result != 0 {
            let error = io::Error::last_os_error();
            if error.raw_os_error() != Some(libc::ESRCH) {
                return Err(error);
            }
        }
    }
    #[cfg(not(unix))]
    child.kill()?;
    child.wait()?;
    Ok(())
}

fn asset_name(os: &str, arch: &str, backend: &str) -> Result<String, SetupError> {
    let suffix = match (os, arch, backend) {
        ("linux", "x86_64", "cpu") => "ubuntu-x64",
        ("linux", "aarch64", "cpu") => "ubuntu-arm64",
        ("linux", "x86_64", "vulkan") => "ubuntu-vulkan-x64",
        ("linux", "aarch64", "vulkan") => "ubuntu-vulkan-arm64",
        ("linux", "x86_64", "cuda") => "ubuntu-cuda-12.8-x64",
        _ => {
            return Err(failure(format!(
                "No supported official managed llama.cpp runtime for {os}/{arch}/{backend}; provide an explicit compatible runtime or external HTTP endpoint"
            )));
        }
    };
    Ok(format!("llama-{TAG}-bin-{suffix}.tar.gz"))
}

#[derive(Deserialize)]
struct Release {
    tag_name: String,
    assets: Vec<Asset>,
}

#[derive(Deserialize)]
struct Asset {
    name: String,
    browser_download_url: String,
    digest: Option<String>,
    size: u64,
}

fn select_asset(json: &[u8], name: &str) -> Result<Asset, SetupError> {
    let release: Release = serde_json::from_slice(json)
        .map_err(|e| io_context("Invalid official release metadata", e))?;
    if release.tag_name != TAG {
        return Err(failure(
            "Official release API returned the wrong llama.cpp tag",
        ));
    }
    let mut matches = release
        .assets
        .into_iter()
        .filter(|asset| asset.name == name);
    let asset = matches
        .next()
        .ok_or_else(|| failure(format!("Official release {TAG} does not contain {name}")))?;
    if matches.next().is_some()
        || asset.browser_download_url
            != format!("https://github.com/ggml-org/llama.cpp/releases/download/{TAG}/{name}")
    {
        return Err(failure(
            "Official runtime asset is ambiguous or has an unexpected download URL",
        ));
    }
    asset_digest(&asset)?;
    if asset.size == 0 || asset.size > MAX_ARCHIVE_BYTES {
        return Err(failure(
            "Official runtime archive exceeds the permitted size",
        ));
    }
    Ok(asset)
}

fn asset_digest(asset: &Asset) -> Result<&str, SetupError> {
    let digest = asset
        .digest
        .as_deref()
        .and_then(|value| value.strip_prefix("sha256:"))
        .ok_or_else(|| {
            failure("Official runtime asset has no SHA256 digest; refusing an unverified download")
        })?;
    if digest.len() != 64 || !digest.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(failure(
            "Official runtime asset has a malformed SHA256 digest",
        ));
    }
    Ok(digest)
}

fn curl_command() -> Command {
    let mut command = Command::new("curl");
    command.args([
        "--disable",
        "--fail",
        "--silent",
        "--show-error",
        "--location",
        "--proto",
        "=https",
        "--proto-redir",
        "=https",
        "--connect-timeout",
        "30",
        "--max-time",
        "600",
        "--retry",
        "2",
        "--retry-delay",
        "2",
    ]);
    command
}

/// Install a digest-verified official runtime, preserving its complete library layout.
pub(super) fn install_official(backend: &str) -> Result<String, SetupError> {
    managed_platform()?;
    let name = asset_name(std::env::consts::OS, std::env::consts::ARCH, backend)?;
    let home = crate::config::resolve_leindex_home()
        .ok_or_else(|| failure("Cannot resolve LeIndex home"))?;
    let root = home.join("runtimes/llama.cpp").join(TAG).join(backend);
    let stage = staging_dir(&root)?;
    let asset = fetch_release_asset(stage.path(), &name)?;
    let digest = asset_digest(&asset)?.to_ascii_lowercase();
    let destination = root.join(format!("{}-{digest}", std::env::consts::ARCH));
    let archive = stage_archive(stage.path(), &destination, &asset, backend)?;
    verify_archive(&archive, &asset, &digest)?;
    publish_verified(stage, &archive, &destination)
}

/// Create the private staging directory under the runtime root. CCN-gate extraction from install_official.
fn staging_dir(root: &Path) -> Result<tempfile::TempDir, SetupError> {
    fs::create_dir_all(root).map_err(|e| io_context("Cannot create runtime directory", e))?;
    tempfile::Builder::new()
        .prefix(".llama-stage-")
        .tempdir_in(root)
        .map_err(|e| io_context("Cannot create private runtime staging directory", e))
}

/// Fetch official release metadata and select the digest-pinned asset named `name`. CCN-gate extraction from install_official.
fn fetch_release_asset(stage: &Path, name: &str) -> Result<Asset, SetupError> {
    let metadata = stage.join("release.json");
    let mut command = curl_command();
    command.args([
        "--header",
        "Accept: application/vnd.github+json",
        "--header",
        "User-Agent: LeIndex-setup",
        API,
    ]);
    run_bounded(
        &mut command,
        &metadata,
        8 * 1024 * 1024,
        Duration::from_secs(120),
    )?;
    select_asset(
        &fs::read(metadata).map_err(|e| io_context("Cannot read release metadata", e))?,
        name,
    )
}

/// Reuse a size-checked cached archive or download the official one into staging. CCN-gate extraction from install_official.
fn stage_archive(
    stage: &Path,
    destination: &Path,
    asset: &Asset,
    backend: &str,
) -> Result<PathBuf, SetupError> {
    let archive = stage.join("runtime.tar.gz");
    if destination.exists() {
        let cached = destination.join("runtime.tar.gz");
        let metadata = fs::symlink_metadata(&cached)
            .map_err(|e| io_context("Cannot inspect cached runtime archive", e))?;
        if !metadata.file_type().is_file() || metadata.len() != asset.size {
            return Err(failure(
                "Cached runtime archive is not a regular file of the verified size",
            ));
        }
        fs::copy(&cached, &archive).map_err(|e| io_context("Cannot read cached runtime archive; supply an explicit runtime or repair the cache", e))?;
    } else {
        println!("Downloading official llama.cpp {TAG} {backend} runtime...");
        let output = stage.join("download.log");
        let mut command = curl_command();
        command
            .arg("--max-filesize")
            .arg(asset.size.to_string())
            .arg("--output")
            .arg(&archive)
            .arg(&asset.browser_download_url);
        run_bounded(&mut command, &output, 1024 * 1024, Duration::from_secs(660))?;
    }
    Ok(archive)
}

/// Check the staged archive against its declared size and SHA256 before extraction. CCN-gate extraction from install_official.
fn verify_archive(archive: &Path, asset: &Asset, digest: &str) -> Result<(), SetupError> {
    if fs::metadata(archive)
        .map_err(|e| io_context("Cannot inspect runtime archive", e))?
        .len()
        != asset.size
        || sha256_of_file(archive).map_err(|e| io_context("Cannot hash runtime archive", e))?
            != digest
    {
        return Err(failure(
            "Official runtime archive SHA256 or size mismatch; refusing to extract or execute",
        ));
    }
    Ok(())
}

/// Unpack a verified archive into staging, then adopt an existing bundle or publish this one. CCN-gate extraction from install_official.
fn publish_verified(
    stage: tempfile::TempDir,
    archive: &Path,
    destination: &Path,
) -> Result<String, SetupError> {
    let bundle = stage.path().join("bundle");
    fs::create_dir(&bundle).map_err(|e| io_context("Cannot create runtime bundle", e))?;
    extract_archive(archive, &bundle)?;
    let server = locate_server(&bundle)?;
    let relative = server
        .strip_prefix(&bundle)
        .map_err(|e| io_context("Invalid runtime server layout", e))?
        .to_path_buf();
    // Validate the replacement before touching an existing installation.
    validate_binary(&server)?;
    let parent = destination
        .parent()
        .ok_or_else(|| failure("Runtime destination has no parent"))?;
    let _lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(parent.join(".publish.lock"))
        .map_err(|e| io_context("Cannot open runtime publication lock", e))?;
    #[cfg(unix)]
    {
        use std::os::fd::AsRawFd;
        // SAFETY: the file descriptor is owned and live for this entire scope.
        if unsafe { libc::flock(_lock.as_raw_fd(), libc::LOCK_EX) } != 0 {
            return Err(io_context(
                "Cannot lock runtime publication",
                io::Error::last_os_error(),
            ));
        }
    }
    if destination.exists() {
        return repair_or_reuse_bundle(stage, &bundle, destination, parent, &relative);
    }
    publish_bundle(stage, destination, &relative)
}

/// Reuse an identical installation or replace a corrupt bundle with rollback protection.
fn repair_or_reuse_bundle(
    stage: tempfile::TempDir,
    bundle: &Path,
    destination: &Path,
    parent: &Path,
    relative: &Path,
) -> Result<String, SetupError> {
    let installed_bundle = destination.join("bundle");
    if compare_bundle(bundle, &installed_bundle).is_ok() {
        return validate_binary(&installed_bundle.join(relative));
    }

    // A directory rename cannot replace a nonempty bundle. Retire it under
    // the publication lock, keep it for rollback, then publish verified data.
    let backup = tempfile::Builder::new()
        .prefix(".llama-repair-")
        .tempdir_in(parent)
        .map_err(|e| io_context("Cannot stage runtime repair", e))?;
    let retired = backup.path().join("previous");
    fs::rename(destination, &retired)
        .map_err(|e| io_context("Cannot retire corrupt runtime", e))?;
    let result = publish_bundle(stage, destination, relative);
    if result.is_err() {
        let restored = (|| -> io::Result<()> {
            if destination.exists() {
                fs::rename(destination, backup.path().join("failed"))?;
            }
            fs::rename(&retired, destination)
        })();
        if let Err(error) = restored {
            let retained = backup.keep();
            return Err(failure(format!(
                "Cannot restore runtime after repair failed: {error}; original retained at {}",
                retained.display()
            )));
        }
    }
    result
}

/// Atomically rename the staged bundle into place, deferring to a concurrent winner. CCN-gate extraction from install_official.
fn publish_bundle(
    stage: tempfile::TempDir,
    destination: &Path,
    relative: &Path,
) -> Result<String, SetupError> {
    let staged = stage.keep();
    match fs::rename(&staged, destination) {
        Ok(()) => validate_binary(&destination.join("bundle").join(relative)),
        Err(error) => concurrent_publish(&staged, destination, relative, error),
    }
}

/// Adopt a bundle a concurrent publisher installed first, then clean up our staging copy. CCN-gate extraction from install_official.
fn concurrent_publish(
    staged: &Path,
    destination: &Path,
    relative: &Path,
    error: io::Error,
) -> Result<String, SetupError> {
    // A concurrent publisher may have won. Never overwrite its bundle.
    let result = if destination.exists() {
        compare_bundle(&staged.join("bundle"), &destination.join("bundle"))
            .and_then(|()| validate_binary(&destination.join("bundle").join(relative)))
    } else {
        Err(io_context(
            "Cannot publish runtime bundle atomically",
            error,
        ))
    };
    fs::remove_dir_all(staged)
        .map_err(|e| io_context("Cannot clean runtime staging directory", e))?;
    result
}

fn safe_relative(path: &Path) -> Result<PathBuf, SetupError> {
    let text = path
        .to_str()
        .ok_or_else(|| failure("Archive path is not UTF-8"))?;
    if text.is_empty()
        || text.contains('\\')
        || text.contains(':')
        || text.chars().any(char::is_control)
    {
        return Err(failure("Archive contains an unsafe member path"));
    }
    let mut result = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Normal(part) => result.push(part),
            Component::CurDir => {}
            _ => return Err(failure("Archive member escapes the runtime bundle")),
        }
    }
    Ok(result)
}

fn link_target(member: &Path, target: &Path) -> Result<PathBuf, SetupError> {
    let text = target
        .to_str()
        .ok_or_else(|| failure("Archive link target is not UTF-8"))?;
    if text.is_empty()
        || text.contains('\\')
        || text.contains(':')
        || text.chars().any(char::is_control)
    {
        return Err(failure("Archive contains an unsafe link target"));
    }
    let mut result = member.parent().unwrap_or(Path::new("")).to_path_buf();
    for component in target.components() {
        match component {
            Component::Normal(part) => result.push(part),
            Component::CurDir => {}
            Component::ParentDir if result.pop() => {}
            _ => return Err(failure("Archive link escapes the runtime bundle")),
        }
    }
    Ok(result)
}

/// Unpack every member of a verified archive into `root`, deferring symlinks to materialize as copies. CCN-gate extraction from extract_archive.
fn extract_archive(archive: &Path, root: &Path) -> Result<(), SetupError> {
    let input = File::open(archive).map_err(|e| io_context("Cannot open runtime archive", e))?;
    let mut archive =
        tar::Archive::new(GzDecoder::new(input).take(MAX_EXTRACTED_BYTES + 16 * 1024 * 1024));
    let entries = archive
        .entries()
        .map_err(|e| io_context("Cannot read runtime archive", e))?;
    let mut names = HashSet::new();
    let mut links = Vec::new();
    let mut total = 0u64;
    for (index, entry) in entries.enumerate() {
        if index >= MAX_MEMBERS {
            return Err(failure("Runtime archive contains too many members"));
        }
        let entry = entry.map_err(|e| io_context("Invalid archive entry", e))?;
        if let Some(link) = extract_entry(entry, root, &mut names, &mut total)? {
            links.push(link);
        }
    }
    materialize_links(root, links, &mut total)
}

/// Extract one archive member and return a symlink `(member, target)` to materialize later. CCN-gate extraction from extract_archive.
fn extract_entry(
    entry: tar::Entry<'_, impl Read>,
    root: &Path,
    names: &mut HashSet<PathBuf>,
    total: &mut u64,
) -> Result<Option<(PathBuf, PathBuf)>, SetupError> {
    let member = safe_relative(
        &entry
            .path()
            .map_err(|e| io_context("Invalid archive member path", e))?,
    )?;
    let kind = entry.header().entry_type();
    if member.as_os_str().is_empty() && kind.is_dir() {
        return Ok(None);
    }
    claim_name(names, &member)?;
    place_member(entry, kind, &member, root, total)
}

/// Reject empty or duplicate member names before anything is written. CCN-gate extraction from extract_archive.
fn claim_name(names: &mut HashSet<PathBuf>, member: &Path) -> Result<(), SetupError> {
    if member.as_os_str().is_empty() || !names.insert(member.to_path_buf()) {
        return Err(failure(
            "Runtime archive contains an empty or duplicate member",
        ));
    }
    Ok(())
}

/// Create a directory, write a regular file, or defer a symlink for `member`. CCN-gate extraction from extract_archive.
fn place_member(
    mut entry: tar::Entry<'_, impl Read>,
    kind: tar::EntryType,
    member: &Path,
    root: &Path,
    total: &mut u64,
) -> Result<Option<(PathBuf, PathBuf)>, SetupError> {
    let destination = root.join(member);
    if kind.is_dir() {
        fs::create_dir_all(&destination)
            .map_err(|e| io_context("Cannot create archive directory", e))?;
    } else if kind.is_file() {
        extract_file(&mut entry, &destination, total)?;
    } else if kind.is_symlink() {
        let target = entry
            .link_name()
            .map_err(|e| io_context("Invalid archive link", e))?
            .ok_or_else(|| failure("Archive link has no target"))?;
        return Ok(Some((member.to_path_buf(), link_target(member, &target)?)));
    } else {
        return Err(failure(
            "Runtime archive contains unsupported hardlinks, devices, or special entries",
        ));
    }
    Ok(None)
}

/// Write one regular archive member within the size budget, then normalize its mode. CCN-gate extraction from extract_archive.
fn extract_file(
    entry: &mut tar::Entry<'_, impl Read>,
    destination: &Path,
    total: &mut u64,
) -> Result<(), SetupError> {
    let size = entry
        .header()
        .size()
        .map_err(|e| io_context("Invalid archive member size", e))?;
    *total = total
        .checked_add(size)
        .ok_or_else(|| failure("Archive size overflow"))?;
    if *total > MAX_EXTRACTED_BYTES {
        return Err(failure("Runtime archive exceeds extracted size limit"));
    }
    if let Some(parent) = destination.parent() {
        fs::create_dir_all(parent).map_err(|e| io_context("Cannot create member directory", e))?;
    }
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(destination)
        .map_err(|e| io_context("Cannot create runtime file", e))?;
    let copied = io::copy(&mut entry.by_ref().take(size + 1), &mut file)
        .map_err(|e| io_context("Cannot extract runtime file", e))?;
    if copied != size {
        return Err(failure("Archive member size does not match its header"));
    }
    file.flush()
        .map_err(|e| io_context("Cannot flush runtime file", e))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = entry
            .header()
            .mode()
            .map_err(|e| io_context("Invalid archive permissions", e))?;
        fs::set_permissions(
            destination,
            fs::Permissions::from_mode(if mode & 0o111 != 0 { 0o755 } else { 0o644 }),
        )
        .map_err(|e| io_context("Cannot set runtime permissions", e))?;
    }
    Ok(())
}

/// Materialize contained library aliases as regular copies, retrying until targets exist. CCN-gate extraction from extract_archive.
fn materialize_links(
    root: &Path,
    mut links: Vec<(PathBuf, PathBuf)>,
    total: &mut u64,
) -> Result<(), SetupError> {
    while !links.is_empty() {
        let before = links.len();
        let mut pending = Vec::new();
        for (member, target) in links {
            let source = root.join(target);
            if source.is_file() {
                copy_alias(root, &member, &source, total)?;
            } else {
                pending.push((
                    member,
                    source
                        .strip_prefix(root)
                        .map_err(|e| io_context("Invalid link layout", e))?
                        .to_path_buf(),
                ));
            }
        }
        if pending.len() == before {
            return Err(failure(
                "Runtime archive contains missing, cyclic, or directory link targets",
            ));
        }
        links = pending;
    }
    Ok(())
}

/// Copy one alias target to `member` under the alias size budget, preserving its permissions. CCN-gate extraction from extract_archive.
fn copy_alias(
    root: &Path,
    member: &Path,
    source: &Path,
    total: &mut u64,
) -> Result<(), SetupError> {
    let size = fs::metadata(source)
        .map_err(|e| io_context("Cannot inspect alias target", e))?
        .len();
    *total = total
        .checked_add(size)
        .ok_or_else(|| failure("Archive alias size overflow"))?;
    if *total > MAX_EXTRACTED_BYTES {
        return Err(failure("Runtime aliases exceed extracted size limit"));
    }
    let destination = root.join(member);
    if let Some(parent) = destination.parent() {
        fs::create_dir_all(parent).map_err(|e| io_context("Cannot create alias directory", e))?;
    }
    let mut output = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&destination)
        .map_err(|e| io_context("Cannot create runtime alias", e))?;
    io::copy(
        &mut File::open(source).map_err(|e| io_context("Cannot read alias target", e))?,
        &mut output,
    )
    .map_err(|e| io_context("Cannot materialize runtime alias", e))?;
    fs::set_permissions(
        &destination,
        fs::metadata(source)
            .map_err(|e| io_context("Cannot inspect alias permissions", e))?
            .permissions(),
    )
    .map_err(|e| io_context("Cannot set alias permissions", e))
}

fn bundle_files(root: &Path) -> Result<Vec<PathBuf>, SetupError> {
    if !fs::symlink_metadata(root)
        .map_err(|e| io_context("Cannot inspect runtime root", e))?
        .file_type()
        .is_dir()
    {
        return Err(failure(
            "Runtime bundle root must be a real directory, not a symlink",
        ));
    }
    let mut files = Vec::new();
    for (index, entry) in walkdir::WalkDir::new(root)
        .follow_links(false)
        .into_iter()
        .enumerate()
    {
        if index > MAX_MEMBERS {
            return Err(failure("Installed runtime contains too many members"));
        }
        let entry = entry.map_err(|e| io_context("Cannot inspect installed runtime", e))?;
        if entry.file_type().is_symlink()
            || (!entry.file_type().is_file() && !entry.file_type().is_dir())
        {
            return Err(failure(
                "Installed runtime contains unexpected links or special files",
            ));
        }
        if entry.file_type().is_file() {
            files.push(
                entry
                    .path()
                    .strip_prefix(root)
                    .map_err(|e| io_context("Invalid installed runtime path", e))?
                    .to_path_buf(),
            );
        }
        if files.len() > MAX_MEMBERS {
            return Err(failure("Installed runtime contains too many files"));
        }
    }
    files.sort();
    Ok(files)
}

fn locate_server(root: &Path) -> Result<PathBuf, SetupError> {
    let candidates: Vec<_> = bundle_files(root)?
        .into_iter()
        .filter(|path| path.file_name().is_some_and(|name| name == "llama-server"))
        .collect();
    if candidates.len() != 1 {
        return Err(failure(
            "Official runtime must contain exactly one llama-server executable",
        ));
    }
    Ok(root.join(&candidates[0]))
}

fn compare_bundle(expected: &Path, installed: &Path) -> Result<(), SetupError> {
    let files = bundle_files(expected)?;
    if files != bundle_files(installed)? {
        return Err(failure(
            "Installed runtime layout differs from the verified archive; refusing to execute",
        ));
    }
    for file in files {
        let expected_meta = fs::metadata(expected.join(&file))
            .map_err(|e| io_context("Cannot inspect verified runtime file", e))?;
        let installed_meta = fs::metadata(installed.join(&file))
            .map_err(|e| io_context("Cannot inspect installed runtime file", e))?;
        if expected_meta.len() != installed_meta.len() {
            return Err(failure(
                "Installed runtime file size differs from the verified archive",
            ));
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if expected_meta.permissions().mode() & 0o7777
                != installed_meta.permissions().mode() & 0o7777
            {
                return Err(failure(
                    "Installed runtime permissions differ from the verified archive",
                ));
            }
        }
        if sha256_of_file(&expected.join(&file))
            .map_err(|e| io_context("Cannot hash verified runtime file", e))?
            != sha256_of_file(&installed.join(&file))
                .map_err(|e| io_context("Cannot hash installed runtime file", e))?
        {
            return Err(failure(format!(
                "Installed runtime file {} failed integrity verification; refusing to execute",
                file.display()
            )));
        }
    }
    Ok(())
}

#[cfg(test)]
#[path = "setup_llama_test.rs"]
mod test;

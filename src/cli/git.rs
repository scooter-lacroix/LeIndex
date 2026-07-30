//! Live Git status parsing without line-oriented or whitespace-sensitive paths.
#![allow(missing_docs)]

use std::path::{Path, PathBuf};
use std::process::Command;

/// Error returned when Git cannot provide a source inventory.
#[derive(Debug)]
pub enum GitInventoryError {
    /// The path is not inside a Git worktree.
    NotRepository,
    /// Git returned a non-zero status.
    Failed(String),
    /// The subprocess could not be started.
    Io(std::io::Error),
}

impl std::fmt::Display for GitInventoryError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotRepository => f.write_str("not a git repository"),
            Self::Failed(message) => f.write_str(message),
            Self::Io(error) => error.fmt(f),
        }
    }
}

impl std::error::Error for GitInventoryError {}

/// Return whether `root` is inside a Git worktree without scanning it.
pub fn is_worktree(root: &Path) -> bool {
    Command::new("git")
        .args(["rev-parse", "--is-inside-work-tree"])
        .current_dir(root)
        .output()
        .map(|output| output.status.success() && output.stdout.starts_with(b"true"))
        .unwrap_or(false)
}

/// Return the committed tree identity used to validate a generation snapshot.
pub fn tree_oid(root: &Path) -> Result<Option<String>, GitInventoryError> {
    let output = Command::new("git")
        .args(["rev-parse", "HEAD^{tree}"])
        .current_dir(root)
        .output()
        .map_err(GitInventoryError::Io)?;
    if !output.status.success() {
        if String::from_utf8_lossy(&output.stderr).contains("not a git repository") {
            return Ok(None);
        }
        return Err(GitInventoryError::Failed(
            String::from_utf8_lossy(&output.stderr).trim().to_owned(),
        ));
    }
    Ok(Some(
        String::from_utf8_lossy(&output.stdout).trim().to_owned(),
    ))
}

/// Enumerate tracked and non-ignored untracked files at a Git boundary.
///
/// Git remains the source of truth for ignore rules and gitlinks. The returned
/// paths are absolute, sorted, and never include descendants of nested
/// repositories or submodules.
pub fn source_inventory(root: &Path) -> Result<Vec<PathBuf>, GitInventoryError> {
    let root = root.canonicalize().map_err(GitInventoryError::Io)?;
    if !git_output(&root, &["rev-parse", "--show-toplevel"])?
        .status
        .success()
    {
        return Err(GitInventoryError::NotRepository);
    }

    let list_output = successful_git_output(
        &root,
        &[
            "ls-files",
            "-z",
            "--cached",
            "--others",
            "--exclude-standard",
        ],
    )?;
    let stage_output = successful_git_output(&root, &["ls-files", "-z", "--stage"])?;
    let gitlinks = gitlink_paths(&root, &stage_output.stdout);
    let mut paths = inventory_paths(&root, &list_output.stdout, &gitlinks);
    paths.sort();
    paths.dedup();
    Ok(paths)
}

fn git_output(root: &Path, arguments: &[&str]) -> Result<std::process::Output, GitInventoryError> {
    Command::new("git")
        .args(arguments)
        .current_dir(root)
        .output()
        .map_err(GitInventoryError::Io)
}

fn successful_git_output(
    root: &Path,
    arguments: &[&str],
) -> Result<std::process::Output, GitInventoryError> {
    let output = git_output(root, arguments)?;
    if output.status.success() {
        return Ok(output);
    }
    Err(GitInventoryError::Failed(
        String::from_utf8_lossy(&output.stderr).trim().to_owned(),
    ))
}

fn gitlink_paths(root: &Path, stage_output: &[u8]) -> Vec<PathBuf> {
    stage_output
        .split(|byte| *byte == b'\0')
        .filter_map(|record| {
            let tab = record.iter().position(|byte| *byte == b'\t')?;
            let (header, raw_path) = record.split_at(tab);
            let raw_path = raw_path.get(1..)?;
            (header.split(|byte| *byte == b' ').next() == Some(b"160000".as_slice()))
                .then(|| root.join(path_bytes(raw_path)))
        })
        .collect()
}

fn inventory_paths(root: &Path, list_output: &[u8], gitlinks: &[PathBuf]) -> Vec<PathBuf> {
    list_output
        .split(|byte| *byte == b'\0')
        .filter(|raw| !raw.is_empty())
        .map(|raw| root.join(path_bytes(raw)))
        .filter_map(|candidate| accepted_source_path(candidate, root, gitlinks))
        .collect()
}

fn accepted_source_path(candidate: PathBuf, root: &Path, gitlinks: &[PathBuf]) -> Option<PathBuf> {
    if !candidate.starts_with(root)
        || gitlinks
            .iter()
            .any(|gitlink| candidate.starts_with(gitlink))
        || is_skipped_source_path(&candidate, root)
        || has_nested_git_boundary(&candidate, root)
        || !candidate.is_file()
    {
        return None;
    }

    candidate
        .canonicalize()
        .ok()
        .filter(|resolved| resolved.starts_with(root))
        .map(|_| candidate)
}

/// Return worktree files whose contents contain a fixed string.
///
/// Git performs the content prefilter against tracked files without forcing
/// callers to enumerate and read the entire worktree. Non-ignored untracked
/// files are included as candidates so live symbol fallback still sees edits
/// that have not been staged. The caller remains responsible for parsing and
/// applying language/scope filters.
pub fn source_candidates(root: &Path, needle: &str) -> Result<Vec<PathBuf>, GitInventoryError> {
    let root = root.canonicalize().map_err(GitInventoryError::Io)?;
    let top_output = Command::new("git")
        .args(["rev-parse", "--show-toplevel"])
        .current_dir(&root)
        .output()
        .map_err(GitInventoryError::Io)?;
    if !top_output.status.success() {
        return Err(GitInventoryError::NotRepository);
    }
    let tracked = Command::new("git")
        .args(["grep", "--no-color", "-I", "-l", "-F", "-z", "--", needle])
        .current_dir(&root)
        .output()
        .map_err(GitInventoryError::Io)?;
    // git grep exits 1 for a valid search with no matches. Any other failure
    // is an actual Git error and should not silently broaden the scan.
    if !tracked.status.success() && tracked.status.code() != Some(1) {
        return Err(GitInventoryError::Failed(
            String::from_utf8_lossy(&tracked.stderr).trim().to_owned(),
        ));
    }

    let untracked = Command::new("git")
        .args(["ls-files", "-z", "--others", "--exclude-standard"])
        .current_dir(&root)
        .output()
        .map_err(GitInventoryError::Io)?;
    if !untracked.status.success() {
        return Err(GitInventoryError::Failed(
            String::from_utf8_lossy(&untracked.stderr).trim().to_owned(),
        ));
    }

    let mut paths = Vec::new();
    for raw in tracked
        .stdout
        .split(|byte| *byte == b'\0')
        .chain(untracked.stdout.split(|byte| *byte == b'\0'))
    {
        if raw.is_empty() {
            continue;
        }
        if let Some(candidate) = accepted_source_path(root.join(path_bytes(raw)), &root, &[]) {
            paths.push(candidate);
        }
    }
    paths.sort();
    paths.dedup();
    Ok(paths)
}

/// Return live source candidates changed relative to `HEAD`.
///
/// This is the bounded fallback for a catalog miss: an indexed generation
/// already covers the unchanged tree, so only modified/staged/untracked paths
/// need live parsing. Callers that cannot prove the catalog was built from the
/// current tree must use [`source_candidates`] instead.
pub fn changed_source_candidates(root: &Path) -> Result<Vec<PathBuf>, GitInventoryError> {
    let root = root.canonicalize().map_err(GitInventoryError::Io)?;
    let top_output = Command::new("git")
        .args(["rev-parse", "--show-toplevel"])
        .current_dir(&root)
        .output()
        .map_err(GitInventoryError::Io)?;
    if !top_output.status.success() {
        return Err(GitInventoryError::NotRepository);
    }
    let changed = Command::new("git")
        .args(["diff", "--name-only", "-z", "HEAD", "--"])
        .current_dir(&root)
        .output()
        .map_err(GitInventoryError::Io)?;
    if !changed.status.success() {
        return Err(GitInventoryError::Failed(
            String::from_utf8_lossy(&changed.stderr).trim().to_owned(),
        ));
    }
    let untracked = Command::new("git")
        .args(["ls-files", "-z", "--others", "--exclude-standard"])
        .current_dir(&root)
        .output()
        .map_err(GitInventoryError::Io)?;
    if !untracked.status.success() {
        return Err(GitInventoryError::Failed(
            String::from_utf8_lossy(&untracked.stderr).trim().to_owned(),
        ));
    }

    let mut paths = Vec::new();
    for raw in changed
        .stdout
        .split(|byte| *byte == b'\0')
        .chain(untracked.stdout.split(|byte| *byte == b'\0'))
    {
        if raw.is_empty() {
            continue;
        }
        if let Some(candidate) = accepted_source_path(root.join(path_bytes(raw)), &root, &[]) {
            paths.push(candidate);
        }
    }
    paths.sort();
    paths.dedup();
    Ok(paths)
}

fn has_nested_git_boundary(path: &Path, root: &Path) -> bool {
    let mut current = path.parent();
    while let Some(directory) = current {
        if directory == root {
            break;
        }
        // A project may itself be a subdirectory of a larger worktree. Only
        // repositories below the requested root are boundaries; an ancestor
        // `.git` belongs to the containing workspace and must not hide every
        // source file in the project.
        if !directory.starts_with(root) {
            break;
        }
        if directory.join(".git").exists() {
            return true;
        }
        current = directory.parent();
    }
    false
}

fn is_skipped_source_path(path: &Path, root: &Path) -> bool {
    path.strip_prefix(root).ok().is_some_and(|relative| {
        relative.components().any(|component| {
            component
                .as_os_str()
                .to_str()
                .is_some_and(|name| crate::cli::skip_dirs::SKIP_DIRS.contains(&name))
        })
    })
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct GitStatus {
    pub modified: Vec<PathBuf>,
    pub staged: Vec<PathBuf>,
    pub untracked: Vec<PathBuf>,
    pub conflicted: Vec<PathBuf>,
    pub ignored: Vec<PathBuf>,
    pub deleted: Vec<PathBuf>,
    pub renames: Vec<GitRename>,
    pub submodules: Vec<GitSubmodule>,
    pub branch: Option<String>,
    pub head_oid: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitRename {
    pub from: PathBuf,
    pub to: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitSubmodule {
    pub path: PathBuf,
    pub state: String,
}

/// Committed gitlink identity used for a bounded PDG summary node.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitSubmoduleSummary {
    pub path: PathBuf,
    pub commit_oid: String,
}

/// Read gitlink paths and commit OIDs without entering submodule worktrees.
pub fn submodule_summaries(root: &Path) -> Result<Vec<GitSubmoduleSummary>, GitInventoryError> {
    let top_output = Command::new("git")
        .args(["rev-parse", "--show-toplevel"])
        .current_dir(root)
        .output()
        .map_err(GitInventoryError::Io)?;
    if !top_output.status.success() {
        return Err(GitInventoryError::NotRepository);
    }
    let top = PathBuf::from(String::from_utf8_lossy(&top_output.stdout).trim());
    let output = Command::new("git")
        .args(["ls-files", "-z", "--stage"])
        .current_dir(&top)
        .output()
        .map_err(GitInventoryError::Io)?;
    if !output.status.success() {
        return Err(GitInventoryError::Failed(
            String::from_utf8_lossy(&output.stderr).trim().to_owned(),
        ));
    }
    let mut summaries = Vec::new();
    for record in output.stdout.split(|byte| *byte == b'\0') {
        let Some(tab) = record.iter().position(|byte| *byte == b'\t') else {
            continue;
        };
        let header = &record[..tab];
        let raw_path = &record[tab + 1..];
        let fields = header.split(|byte| *byte == b' ').collect::<Vec<_>>();
        if fields.len() < 3 || fields[0] != b"160000" {
            continue;
        }
        summaries.push(GitSubmoduleSummary {
            path: top.join(path_bytes(raw_path)),
            commit_oid: String::from_utf8_lossy(fields[1]).into_owned(),
        });
    }
    summaries.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(summaries)
}

#[derive(Debug)]
pub enum GitStatusError {
    NotRepository,
    Failed(String),
    Io(std::io::Error),
}

impl std::fmt::Display for GitStatusError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotRepository => write!(f, "not a git repository"),
            Self::Failed(message) => f.write_str(message),
            Self::Io(error) => error.fmt(f),
        }
    }
}

impl std::error::Error for GitStatusError {}

/// Run the one status command used by live status consumers.
pub fn status(root: &Path) -> Result<GitStatus, GitStatusError> {
    let output = Command::new("git")
        .args([
            "status",
            "--porcelain=v2",
            "-z",
            "--branch",
            "--untracked-files=all",
        ])
        .current_dir(root)
        .output()
        .map_err(GitStatusError::Io)?;
    if output.status.success() {
        return Ok(parse_status(&output.stdout));
    }

    let message = String::from_utf8_lossy(&output.stderr).trim().to_owned();
    if message.contains("not a git repository") {
        Err(GitStatusError::NotRepository)
    } else {
        Err(GitStatusError::Failed(message))
    }
}

/// Parse `git status --porcelain=v2 -z --branch` output.
///
/// Records are split only on NUL delimiters; path fields are otherwise opaque
/// byte strings, so spaces, tabs, and rename arrows remain part of their path.
pub fn parse_status(bytes: &[u8]) -> GitStatus {
    let mut status = GitStatus::default();
    let mut fields = bytes.split(|byte| *byte == b'\0');

    while let Some(record) = fields.next() {
        if record.is_empty() {
            continue;
        }
        match record[0] {
            b'#' => parse_header(record, &mut status),
            b'1' => parse_ordinary(record, &mut status),
            b'2' => parse_rename(record, fields.next(), &mut status),
            b'u' => parse_unmerged(record, &mut status),
            b'?' => status.untracked.push(path_field(record, 2)),
            b'!' => status.ignored.push(path_field(record, 2)),
            _ => {}
        }
    }
    status
}

fn parse_header(record: &[u8], status: &mut GitStatus) {
    let mut fields = record.splitn(3, |byte| *byte == b' ');
    let _ = fields.next();
    let Some(key) = fields.next() else { return };
    let Some(value) = fields.next() else { return };
    match key {
        b"branch.head" if value != b"(detached)" => {
            status.branch = Some(String::from_utf8_lossy(value).into_owned())
        }
        b"branch.oid" if value != b"(initial)" => {
            status.head_oid = Some(String::from_utf8_lossy(value).into_owned())
        }
        _ => {}
    }
}

fn parse_ordinary(record: &[u8], status: &mut GitStatus) {
    let fields = record.splitn(9, |byte| *byte == b' ').collect::<Vec<_>>();
    if fields.len() != 9 {
        return;
    }
    let xy = fields[1];
    let submodule = fields[2];
    let path = path_bytes(fields[8]);
    classify_xy(xy, &path, status);
    if submodule.first() == Some(&b'S') {
        status.submodules.push(GitSubmodule {
            path,
            state: String::from_utf8_lossy(submodule).into_owned(),
        });
    }
}

fn parse_rename(record: &[u8], original: Option<&[u8]>, status: &mut GitStatus) {
    let fields = record.splitn(10, |byte| *byte == b' ').collect::<Vec<_>>();
    if fields.len() != 10 {
        return;
    }
    let xy = fields[1];
    let submodule = fields[2];
    let to = path_bytes(fields[9]);
    classify_xy(xy, &to, status);
    if let Some(from) = original {
        status.renames.push(GitRename {
            from: path_bytes(from),
            to: to.clone(),
        });
    }
    if submodule.first() == Some(&b'S') {
        status.submodules.push(GitSubmodule {
            path: to,
            state: String::from_utf8_lossy(submodule).into_owned(),
        });
    }
}

fn parse_unmerged(record: &[u8], status: &mut GitStatus) {
    let fields = record.splitn(11, |byte| *byte == b' ').collect::<Vec<_>>();
    if fields.len() == 11 {
        status.conflicted.push(path_bytes(fields[10]));
    }
}

fn classify_xy(xy: &[u8], path: &Path, status: &mut GitStatus) {
    let x = xy.first().copied().unwrap_or(b'.');
    let y = xy.get(1).copied().unwrap_or(b'.');
    if x != b'.' && x != b' ' {
        status.staged.push(path.to_path_buf());
    }
    if y != b'.' && y != b' ' {
        status.modified.push(path.to_path_buf());
    }
    if x == b'D' || y == b'D' {
        status.deleted.push(path.to_path_buf());
    }
}

fn path_field(record: &[u8], start: usize) -> PathBuf {
    path_bytes(record.get(start..).unwrap_or_default())
}

fn path_bytes(bytes: &[u8]) -> PathBuf {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStringExt;
        PathBuf::from(std::ffi::OsString::from_vec(bytes.to_vec()))
    }
    #[cfg(not(unix))]
    {
        PathBuf::from(String::from_utf8_lossy(bytes).into_owned())
    }
}

//! Live Git status parsing without line-oriented or whitespace-sensitive paths.
#![allow(missing_docs)]

use std::path::{Path, PathBuf};
use std::process::Command;

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

fn classify_xy(xy: &[u8], path: &PathBuf, status: &mut GitStatus) {
    let x = xy.first().copied().unwrap_or(b'.');
    let y = xy.get(1).copied().unwrap_or(b'.');
    if x != b'.' && x != b' ' {
        status.staged.push(path.clone());
    }
    if y != b'.' && y != b' ' {
        status.modified.push(path.clone());
    }
    if x == b'D' || y == b'D' {
        status.deleted.push(path.clone());
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

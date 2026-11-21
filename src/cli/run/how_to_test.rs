//! `how_to_test(path)` (§6.5): the gate command for a path, discovered from
//! Cargo workspaces, fenced commands in AGENTS.md / CONTRIBUTING.md,
//! package.json scripts, Makefile / justfile targets and `script/` wrappers.
//!
//! Discovery is cached per project root. The cache records every manifest it
//! read (and the fixed-name files it looked for and did not find) with their
//! mtimes, and is rebuilt as soon as any of them changes.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::SystemTime;

use anyhow::{Result, bail};

/// Docs scanned for fenced commands.
const DOC_FILES: [&str; 2] = ["AGENTS.md", "CONTRIBUTING.md"];

/// Fenced doc lines kept per project, and alternatives returned per answer.
const MAX_DOC_COMMANDS: usize = 24;
const MAX_ALSO: usize = 5;

/// The answer for one path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Guidance {
    /// Command to run (argv joined by spaces; none of it needs a shell).
    pub command: String,
    /// Directory to run it from (relative to the project, `.` for the root).
    pub cwd: PathBuf,
    /// Where the command came from.
    pub source: String,
    /// Other documented gate commands (fmt, clippy, test) worth knowing.
    pub also: Vec<String>,
}

#[derive(Debug, Clone)]
struct Member {
    name: String,
    dir: PathBuf,
}

#[derive(Debug, Clone, Default)]
struct Facts {
    members: Vec<Member>,
    doc_commands: Vec<String>,
    make_targets: Vec<String>,
    just_targets: Vec<String>,
    scripts: Vec<String>,
    npm_scripts: Vec<String>,
}

/// `(path, mtime)` of everything discovery depended on; `None` = absent.
type Stamp = Vec<(PathBuf, Option<SystemTime>)>;

struct Entry {
    stamp: Stamp,
    facts: Facts,
}

fn cache() -> &'static Mutex<HashMap<PathBuf, Entry>> {
    static CACHE: OnceLock<Mutex<HashMap<PathBuf, Entry>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

fn mtime(path: &Path) -> Option<SystemTime> {
    std::fs::metadata(path).and_then(|m| m.modified()).ok()
}

fn restamp(stamp: &Stamp) -> Stamp {
    stamp.iter().map(|(p, _)| (p.clone(), mtime(p))).collect()
}

/// Gate command for `path` (absolute or project-relative).
pub(crate) fn how_to_test(project_root: &Path, path: &Path) -> Result<Guidance> {
    let root = std::fs::canonicalize(project_root)?;
    let facts = facts_for(&root);
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        root.join(path)
    };
    if !absolute.starts_with(&root) {
        bail!("{} is outside the project", absolute.display());
    }
    guidance(&root, &absolute, &facts)
}

fn facts_for(root: &Path) -> Facts {
    let mut cache = cache().lock().unwrap_or_else(|p| p.into_inner());
    if let Some(entry) = cache.get(root)
        && restamp(&entry.stamp) == entry.stamp
    {
        return entry.facts.clone();
    }
    let mut stamp: Stamp = Vec::new();
    let facts = discover(root, &mut stamp);
    cache.insert(
        root.to_path_buf(),
        Entry {
            stamp,
            facts: facts.clone(),
        },
    );
    facts
}

/// Read a file and record it (present or not) in the stamp.
fn read_stamped(path: PathBuf, stamp: &mut Stamp) -> Option<String> {
    stamp.push((path.clone(), mtime(&path)));
    std::fs::read_to_string(path).ok()
}

fn discover(root: &Path, stamp: &mut Stamp) -> Facts {
    let mut facts = Facts::default();

    if let Some(text) = read_stamped(root.join("Cargo.toml"), stamp)
        && let Ok(table) = text.parse::<toml::Table>()
    {
        let mut dirs: Vec<PathBuf> = Vec::new();
        if table.contains_key("package") {
            dirs.push(root.to_path_buf());
        }
        let patterns = table
            .get("workspace")
            .and_then(|w| w.get("members"))
            .and_then(toml::Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(toml::Value::as_str);
        for pattern in patterns {
            match pattern.strip_suffix("/*") {
                Some(parent) => {
                    if let Ok(children) = std::fs::read_dir(root.join(parent)) {
                        let mut found: Vec<PathBuf> =
                            children.filter_map(Result::ok).map(|c| c.path()).collect();
                        found.sort();
                        dirs.extend(found.into_iter().filter(|d| d.is_dir()));
                    }
                }
                None => dirs.push(root.join(pattern)),
            }
        }
        for dir in dirs {
            if let Some(manifest) = read_stamped(dir.join("Cargo.toml"), stamp)
                && let Some(name) = package_name(&manifest)
            {
                facts.members.push(Member { name, dir });
            }
        }
    }

    for doc in DOC_FILES {
        if let Some(text) = read_stamped(root.join(doc), stamp) {
            facts.doc_commands.extend(fenced_commands(&text));
        }
    }
    facts.doc_commands.dedup();
    facts.doc_commands.truncate(MAX_DOC_COMMANDS);

    if let Some(text) = read_stamped(root.join("package.json"), stamp) {
        facts.npm_scripts = package_scripts(&text);
    }
    if let Some(text) = read_stamped(root.join("Makefile"), stamp) {
        facts.make_targets = make_targets(&text, false);
    }
    // `justfile` is case-insensitive in practice; check both spellings.
    for name in ["justfile", "Justfile"] {
        if let Some(text) = read_stamped(root.join(name), stamp) {
            facts.just_targets = make_targets(&text, true);
        }
    }

    let script_dir = root.join("script");
    stamp.push((script_dir.clone(), mtime(&script_dir)));
    if let Ok(entries) = std::fs::read_dir(&script_dir) {
        facts.scripts = entries
            .filter_map(Result::ok)
            .filter(|e| e.path().is_file())
            .filter_map(|e| e.file_name().into_string().ok())
            .collect();
        facts.scripts.sort();
    }
    facts
}

fn package_name(manifest: &str) -> Option<String> {
    manifest
        .parse::<toml::Table>()
        .ok()?
        .get("package")?
        .get("name")?
        .as_str()
        .map(str::to_string)
}

fn package_scripts(text: &str) -> Vec<String> {
    serde_json::from_str::<serde_json::Value>(text)
        .ok()
        .and_then(|v| v.get("scripts").and_then(|s| s.as_object().cloned()))
        .map(|scripts| scripts.keys().cloned().collect())
        .unwrap_or_default()
}

/// Command lines inside ``` fences that start with a known runner and mention
/// a gate (`test`, `clippy`, `fmt`, `check`). `$ ` prompts are stripped.
fn fenced_commands(text: &str) -> Vec<String> {
    const RUNNERS: [&str; 9] = [
        "cargo",
        "npm",
        "pnpm",
        "yarn",
        "npx",
        "pytest",
        "make",
        "just",
        "./script/",
    ];
    let mut inside = false;
    let mut found = Vec::new();
    for line in text.lines() {
        if line.trim_start().starts_with("```") {
            inside = !inside;
            continue;
        }
        if !inside {
            continue;
        }
        let command = line.trim().trim_start_matches("$ ").trim();
        let is_gate = ["test", "clippy", "fmt", "check"]
            .iter()
            .any(|word| command.contains(word));
        if is_gate && RUNNERS.iter().any(|runner| command.starts_with(runner)) {
            found.push(command.to_string());
        }
    }
    found
}

/// Target names from a Makefile or justfile (column-0 `name:` lines; just
/// recipes may take arguments before the colon).
fn make_targets(text: &str, just: bool) -> Vec<String> {
    let mut targets = Vec::new();
    for line in text.lines() {
        let Some(first) = line.chars().next() else {
            continue;
        };
        if !(first.is_ascii_alphanumeric() || first == '_') {
            continue;
        }
        let Some((head, tail)) = line.split_once(':') else {
            continue;
        };
        // `X := y` / `X = y` are assignments, not targets. A just recipe may
        // carry `=` in a parameter default (`test arg='x':`), so only the
        // `:=` form is an assignment there.
        if tail.starts_with('=') || (!just && head.contains('=')) {
            continue;
        }
        let name = if just {
            head.split_whitespace().next().unwrap_or("")
        } else {
            head.trim()
        };
        if !name.is_empty() && !name.contains(char::is_whitespace) {
            targets.push(name.to_string());
        }
    }
    targets
}

fn relative(root: &Path, dir: &Path) -> PathBuf {
    dir.strip_prefix(root)
        .map(|p| {
            if p.as_os_str().is_empty() {
                PathBuf::from(".")
            } else {
                p.to_path_buf()
            }
        })
        .unwrap_or_else(|_| PathBuf::from("."))
}

fn guidance(root: &Path, path: &Path, facts: &Facts) -> Result<Guidance> {
    let also: Vec<String> = facts.doc_commands.iter().take(MAX_ALSO).cloned().collect();
    // Deepest owning workspace member wins.
    let owner = facts
        .members
        .iter()
        .filter(|m| path.starts_with(&m.dir))
        .max_by_key(|m| m.dir.components().count());
    let make = |command: String, dir: &Path, source: String| Guidance {
        command,
        cwd: relative(root, dir),
        source,
        also: also.clone(),
    };

    if let Some(member) = owner {
        // A project wrapper (e.g. `script/cargo-heavy`) replaces bare `cargo`.
        let runner = if facts.scripts.iter().any(|s| s == "cargo-heavy") {
            "./script/cargo-heavy"
        } else {
            "cargo"
        };
        return Ok(make(
            format!("{runner} test -p {} --lib", member.name),
            root,
            format!(
                "Cargo member `{}` ({})",
                member.name,
                if runner == "cargo" {
                    "no wrapper"
                } else {
                    "script/cargo-heavy wrapper"
                }
            ),
        ));
    }

    // Nearest package.json with a test script, walking up to the root.
    let mut dir = path.parent().map(Path::to_path_buf);
    while let Some(current) = dir {
        if !current.starts_with(root) {
            break;
        }
        if let Ok(text) = std::fs::read_to_string(current.join("package.json"))
            && package_scripts(&text).iter().any(|s| s == "test")
        {
            return Ok(make(
                "npm test".to_string(),
                &current,
                format!(
                    "package.json script `test` in {}",
                    relative(root, &current).display()
                ),
            ));
        }
        if current == root {
            break;
        }
        dir = current.parent().map(Path::to_path_buf);
    }

    if let Some(command) = facts.doc_commands.iter().find(|c| c.contains("test")) {
        return Ok(make(
            command.clone(),
            root,
            "fenced command in AGENTS.md/CONTRIBUTING.md".to_string(),
        ));
    }
    if facts.just_targets.iter().any(|t| t == "test") {
        return Ok(make(
            "just test".to_string(),
            root,
            "justfile target `test`".to_string(),
        ));
    }
    if facts.make_targets.iter().any(|t| t == "test") {
        return Ok(make(
            "make test".to_string(),
            root,
            "Makefile target `test`".to_string(),
        ));
    }
    if let Some(script) = facts.scripts.iter().find(|s| s.contains("test")) {
        return Ok(make(
            format!("./script/{script}"),
            root,
            format!("script/{script} wrapper"),
        ));
    }
    if facts.npm_scripts.iter().any(|s| s == "test") {
        return Ok(make(
            "npm test".to_string(),
            root,
            "package.json script `test`".to_string(),
        ));
    }
    bail!(
        "no test command found for {}: no Cargo member owns it and the project has no package.json \
         test script, documented test command, justfile/Makefile `test` target or script/*test*",
        path.display()
    )
}

#[cfg(test)]
#[path = "how_to_test_test.rs"]
mod how_to_test_tests;

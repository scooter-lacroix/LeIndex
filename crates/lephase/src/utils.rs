use crate::options::{DocsMode, PhaseOptions};
use anyhow::{Context, Result};
use std::path::{Path, PathBuf};

/// Files selected for a phase run.
#[derive(Debug, Clone, Default)]
pub struct CollectedFiles {
    /// Source-code files supported by leparse.
    pub code_files: Vec<PathBuf>,
    /// Optional docs files (markdown/text) when explicitly enabled.
    pub docs_files: Vec<PathBuf>,
}

/// Collect source files with optional docs based on options.
pub fn collect_files(root: &Path, options: &PhaseOptions) -> Result<CollectedFiles> {
    let mut collected = CollectedFiles::default();

    let code_exts = [
        "rs", "py", "js", "jsx", "ts", "tsx", "go", "java", "c", "h", "hpp", "cc", "cxx",
    ];

    // Optional focused-file mode (used by MCP when path points to a single file).
    if !options.focus_files.is_empty() {
        for raw_path in &options.focus_files {
            let candidate = if raw_path.is_absolute() {
                raw_path.clone()
            } else {
                root.join(raw_path)
            };

            if !candidate.is_file() {
                continue;
            }

            if let Some(ext) = candidate.extension().and_then(|e| e.to_str()) {
                if code_exts.contains(&ext) {
                    collected.code_files.push(candidate.clone());
                } else if options.include_docs && include_docs_extension(ext, options.docs_mode) {
                    collected.docs_files.push(candidate.clone());
                }
            }

            if collected.code_files.len() >= options.max_files {
                break;
            }
        }

        collected.code_files.sort();
        collected.code_files.dedup();
        collected.docs_files.sort();
        collected.docs_files.dedup();

        if !collected.code_files.is_empty() || !collected.docs_files.is_empty() {
            return Ok(collected);
        }
    }

    let mut walker = walkdir::WalkDir::new(root).into_iter();
    while let Some(entry) = walker.next() {
        let entry = match entry {
            Ok(e) => e,
            Err(_) => continue,
        };

        let path = entry.path();
        let file_name = entry.file_name().to_string_lossy();

        if entry.file_type().is_dir() {
            if should_skip_dir(&file_name) {
                walker.skip_current_dir();
                continue;
            }
            continue;
        }

        if !entry.file_type().is_file() {
            continue;
        }

        if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
            if code_exts.contains(&ext) {
                collected.code_files.push(path.to_path_buf());
                if collected.code_files.len() >= options.max_files {
                    break;
                }
                continue;
            }

            if options.include_docs && include_docs_extension(ext, options.docs_mode) {
                collected.docs_files.push(path.to_path_buf());
            }
        }

        if collected.code_files.len() >= options.max_files {
            break;
        }
    }

    Ok(collected)
}

/// Build (path, hash) inventory for freshness checks.
pub fn hash_inventory(paths: &[PathBuf]) -> Result<Vec<(PathBuf, String)>> {
    let mut out = Vec::with_capacity(paths.len());
    for path in paths {
        let bytes = std::fs::read(path)
            .with_context(|| format!("failed reading file for hashing: {}", path.display()))?;
        let hash = blake3::hash(&bytes).to_hex().to_string();
        out.push((path.clone(), hash));
    }
    out.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(out)
}

/// Render a path relative to root when possible.
pub fn display_path(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .map(|p| p.display().to_string())
        .unwrap_or_else(|_| path.display().to_string())
}

fn should_skip_dir(file_name: &str) -> bool {
    matches!(
        file_name,
        ".git"
            | ".hg"
            | ".svn"
            | "target"
            | "node_modules"
            | "vendor"
            | "dist"
            | "build"
            | "__pycache__"
    )
}

fn include_docs_extension(ext: &str, mode: DocsMode) -> bool {
    (mode.include_markdown() && matches!(ext, "md" | "markdown"))
        || (mode.include_text() && matches!(ext, "txt" | "text"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn collect_files_respects_docs_gating_and_skips_dirs() {
        let dir = tempdir().expect("tempdir");
        std::fs::create_dir_all(dir.path().join("src")).expect("mkdir src");
        std::fs::create_dir_all(dir.path().join("target")).expect("mkdir target");

        std::fs::write(dir.path().join("src/lib.rs"), "pub fn f(){}\n").expect("write code");
        std::fs::write(dir.path().join("README.md"), "# Doc\n").expect("write readme");
        std::fs::write(dir.path().join("notes.txt"), "hello\n").expect("write text");
        std::fs::write(dir.path().join("target/ignored.rs"), "pub fn g(){}\n")
            .expect("write ignored");

        let no_docs = collect_files(
            dir.path(),
            &PhaseOptions {
                root: dir.path().to_path_buf(),
                include_docs: false,
                docs_mode: DocsMode::All,
                ..PhaseOptions::default()
            },
        )
        .expect("collect no docs");

        assert_eq!(no_docs.code_files.len(), 1);
        assert!(no_docs.docs_files.is_empty());

        let with_docs = collect_files(
            dir.path(),
            &PhaseOptions {
                root: dir.path().to_path_buf(),
                include_docs: true,
                docs_mode: DocsMode::All,
                ..PhaseOptions::default()
            },
        )
        .expect("collect docs");

        assert_eq!(with_docs.code_files.len(), 1);
        assert_eq!(with_docs.docs_files.len(), 2);
    }

    #[test]
    fn hash_inventory_and_display_path_work() {
        let dir = tempdir().expect("tempdir");
        let file = dir.path().join("src/lib.rs");
        std::fs::create_dir_all(file.parent().expect("parent")).expect("mkdir");
        std::fs::write(&file, "pub fn f(){}\n").expect("write");

        let inventory = hash_inventory(&[file.clone()]).expect("inventory");
        assert_eq!(inventory.len(), 1);
        assert_eq!(inventory[0].0, file);
        assert!(!inventory[0].1.is_empty());

        let rendered = display_path(dir.path(), &inventory[0].0);
        assert_eq!(rendered, "src/lib.rs");
    }

    #[test]
    fn collect_files_respects_max_files_limit() {
        let dir = tempdir().expect("tempdir");
        std::fs::create_dir_all(dir.path().join("src")).expect("mkdir src");
        std::fs::write(dir.path().join("src/a.rs"), "pub fn a(){}\n").expect("write a");
        std::fs::write(dir.path().join("src/b.rs"), "pub fn b(){}\n").expect("write b");

        let collected = collect_files(
            dir.path(),
            &PhaseOptions {
                root: dir.path().to_path_buf(),
                max_files: 1,
                ..PhaseOptions::default()
            },
        )
        .expect("collect limited");

        assert_eq!(collected.code_files.len(), 1);
    }

    #[test]
    fn docs_mode_filters_markdown_vs_text() {
        let dir = tempdir().expect("tempdir");
        std::fs::create_dir_all(dir.path().join("src")).expect("mkdir src");
        std::fs::write(dir.path().join("src/lib.rs"), "pub fn x(){}\n").expect("write code");
        std::fs::write(dir.path().join("README.md"), "# Doc\n").expect("write md");
        std::fs::write(dir.path().join("notes.txt"), "text\n").expect("write txt");

        let markdown_only = collect_files(
            dir.path(),
            &PhaseOptions {
                root: dir.path().to_path_buf(),
                include_docs: true,
                docs_mode: DocsMode::Markdown,
                ..PhaseOptions::default()
            },
        )
        .expect("collect markdown");
        assert_eq!(markdown_only.docs_files.len(), 1);

        let text_only = collect_files(
            dir.path(),
            &PhaseOptions {
                root: dir.path().to_path_buf(),
                include_docs: true,
                docs_mode: DocsMode::Text,
                ..PhaseOptions::default()
            },
        )
        .expect("collect text");
        assert_eq!(text_only.docs_files.len(), 1);
    }

    #[test]
    fn collect_files_uses_focus_files_when_provided() {
        let dir = tempdir().expect("tempdir");
        std::fs::create_dir_all(dir.path().join("src")).expect("mkdir src");
        let a = dir.path().join("src/a.rs");
        let b = dir.path().join("src/b.rs");
        std::fs::write(&a, "pub fn a(){}\n").expect("write a");
        std::fs::write(&b, "pub fn b(){}\n").expect("write b");

        let collected = collect_files(
            dir.path(),
            &PhaseOptions {
                root: dir.path().to_path_buf(),
                focus_files: vec![b.clone()],
                max_files: 1,
                ..PhaseOptions::default()
            },
        )
        .expect("collect focused");

        assert_eq!(collected.code_files, vec![b]);
    }
}

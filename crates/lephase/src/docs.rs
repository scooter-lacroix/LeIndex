use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Summary for optional markdown/text processing.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct DocsSummary {
    /// Number of docs files scanned.
    pub files_scanned: usize,
    /// Total heading-like lines seen.
    pub heading_count: usize,
    /// Total TODO-like lines seen.
    pub todo_count: usize,
}

/// Analyze markdown/text files for lightweight actionable signals.
pub fn analyze_docs(files: &[PathBuf]) -> Result<DocsSummary> {
    let mut summary = DocsSummary::default();

    for file in files {
        let content = std::fs::read_to_string(file)
            .with_context(|| format!("failed reading docs file {}", file.display()))?;

        summary.files_scanned += 1;
        for line in content.lines() {
            let line_trimmed = line.trim_start();
            if line_trimmed.starts_with('#') {
                summary.heading_count += 1;
            }
            let upper = line_trimmed.to_ascii_uppercase();
            if upper.contains("TODO") || upper.contains("FIXME") || upper.contains("ACTION") {
                summary.todo_count += 1;
            }
        }
    }

    Ok(summary)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn analyze_docs_counts_headings_and_action_markers() {
        let dir = tempfile::tempdir().expect("tempdir");
        let a = dir.path().join("README.md");
        let b = dir.path().join("notes.txt");

        std::fs::write(&a, "# Title\nTODO: wire this\ntext\n").expect("write a");
        std::fs::write(&b, "## Sub\nfixme now\naction required\n").expect("write b");

        let summary = analyze_docs(&[a, b]).expect("analyze");
        assert_eq!(summary.files_scanned, 2);
        assert_eq!(summary.heading_count, 2);
        assert_eq!(summary.todo_count, 3);
    }

    #[test]
    fn analyze_docs_propagates_read_error() {
        let missing = PathBuf::from("/definitely/missing/docs-file.md");
        let err = analyze_docs(&[missing]).err().expect("must fail");
        assert!(err.to_string().contains("failed reading docs file"));
    }
}

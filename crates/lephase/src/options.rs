use crate::format::FormatMode;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Controls optional markdown/text analysis.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum DocsMode {
    /// Disable docs processing.
    Off,
    /// Analyze markdown files only.
    Markdown,
    /// Analyze plain-text files only.
    Text,
    /// Analyze markdown and plain-text files.
    All,
}

impl Default for DocsMode {
    fn default() -> Self {
        Self::Off
    }
}

impl DocsMode {
    /// Parse docs mode from string.
    pub fn parse(value: &str) -> Option<Self> {
        match value.to_ascii_lowercase().as_str() {
            "off" => Some(Self::Off),
            "markdown" => Some(Self::Markdown),
            "text" => Some(Self::Text),
            "all" => Some(Self::All),
            _ => None,
        }
    }

    /// True if markdown files should be considered.
    pub fn include_markdown(self) -> bool {
        matches!(self, Self::Markdown | Self::All)
    }

    /// True if plain text files should be considered.
    pub fn include_text(self) -> bool {
        matches!(self, Self::Text | Self::All)
    }
}

/// Execution options for phase analysis.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PhaseOptions {
    /// Project root path.
    pub root: PathBuf,
    /// Output mode.
    pub mode: FormatMode,
    /// Max number of source files to consider.
    pub max_files: usize,
    /// Max number of focus files in phase 3.
    pub max_focus_files: usize,
    /// Generic top-N used by ranking phases.
    pub top_n: usize,
    /// Max output characters.
    pub max_output_chars: usize,
    /// Enable incremental freshness-aware updates.
    pub use_incremental_refresh: bool,
    /// Explicit opt-in for markdown/text analysis.
    pub include_docs: bool,
    /// Docs processing mode.
    pub docs_mode: DocsMode,
    /// Keyword list used by phase-4 text signal hotspot heuristic.
    #[serde(default = "default_hotspot_keywords")]
    pub hotspot_keywords: Vec<String>,
}

impl Default for PhaseOptions {
    fn default() -> Self {
        Self {
            root: PathBuf::new(),
            mode: FormatMode::Balanced,
            max_files: 2_000,
            max_focus_files: 20,
            top_n: 10,
            max_output_chars: FormatMode::Balanced.default_max_chars(),
            use_incremental_refresh: true,
            include_docs: false,
            docs_mode: DocsMode::Off,
            hotspot_keywords: default_hotspot_keywords(),
        }
    }
}

fn default_hotspot_keywords() -> Vec<String> {
    vec![
        "auth".to_string(),
        "critical".to_string(),
        "error".to_string(),
    ]
}

impl PhaseOptions {
    /// Ensure docs mode is disabled unless explicitly opted in.
    pub fn normalized(mut self) -> Self {
        if !self.include_docs {
            self.docs_mode = DocsMode::Off;
        }

        if self.max_output_chars == 0 {
            self.max_output_chars = self.mode.default_max_chars();
        }

        if self.hotspot_keywords.is_empty() {
            self.hotspot_keywords = PhaseOptions::default().hotspot_keywords;
        }

        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn docs_mode_parse_and_flags_work() {
        assert_eq!(DocsMode::parse("markdown"), Some(DocsMode::Markdown));
        assert_eq!(DocsMode::parse("text"), Some(DocsMode::Text));
        assert_eq!(DocsMode::parse("all"), Some(DocsMode::All));
        assert_eq!(DocsMode::parse("invalid"), None);

        assert!(DocsMode::Markdown.include_markdown());
        assert!(!DocsMode::Markdown.include_text());
        assert!(DocsMode::All.include_markdown());
        assert!(DocsMode::All.include_text());
    }

    #[test]
    fn normalized_disables_docs_when_not_opted_in() {
        let normalized = PhaseOptions {
            include_docs: false,
            docs_mode: DocsMode::All,
            max_output_chars: 0,
            hotspot_keywords: Vec::new(),
            ..PhaseOptions::default()
        }
        .normalized();

        assert_eq!(normalized.docs_mode, DocsMode::Off);
        assert_eq!(
            normalized.max_output_chars,
            normalized.mode.default_max_chars()
        );
        assert!(!normalized.hotspot_keywords.is_empty());
    }

    #[test]
    fn default_requires_explicit_root_assignment() {
        assert!(PhaseOptions::default().root.as_os_str().is_empty());
    }
}

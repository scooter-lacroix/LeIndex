//! Token accounting for tokbench.
//!
//! The roadmap brief asks for a real tokenizer when one is already a
//! dependency. The `tokenizers` crate is already in the workspace lockfile
//! (LeIndex's `onnx` feature uses it), and the runtime model directory ships
//! `tokenizer.json` (the Qwen3 embedder tokenizer LeIndex itself uses), so the
//! default counter counts REAL BPE tokens from that file. When the file is
//! absent the counter falls back to the house chars/4 estimate — the same
//! "W6 model" `tools/headtohead/headtohead.py` uses — and reports label it
//! `estimate` so no number is ever presented as tokenizer-derived when it is
//! not. Both benchmark arms always use the SAME counter, so the comparison
//! stays fair either way.

use std::path::PathBuf;

/// Which counter produced the numbers in a report.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum TokenCounter {
    /// Real BPE token counts via the LeIndex embedder tokenizer file.
    Tokenizer,
    /// chars/4 house estimate (`headtohead.py` W6 model).
    Estimate,
}

/// Load the tokenizer from the runtime model directory when present, else
/// count chars/4. `LEINDEX_TOKBENCH_TOKENIZER` overrides the file path
/// (used by tests to exercise both branches deterministically).
pub struct Counter {
    kind: TokenCounter,
    tokenizer: Option<tokenizers::Tokenizer>,
}

impl Counter {
    /// Build the counter from the default search paths.
    pub fn load() -> Self {
        match candidate_tokenizer_paths().into_iter().find_map(|path| {
            tokenizers::Tokenizer::from_file(&path).ok().map(|t| (path, t))
        }) {
            Some((_path, tokenizer)) => Self {
                kind: TokenCounter::Tokenizer,
                tokenizer: Some(tokenizer),
            },
            None => Self {
                kind: TokenCounter::Estimate,
                tokenizer: None,
            },
        }
    }

    /// Which counter is active.
    pub fn kind(&self) -> TokenCounter {
        self.kind
    }

    /// Human-readable provenance for report headers.
    pub fn provenance(&self) -> &'static str {
        match self.kind {
            TokenCounter::Tokenizer => {
                "real tokens (LeIndex embedder tokenizer.json)"
            }
            TokenCounter::Estimate => "ESTIMATE chars/4 (tokenizer.json not found)",
        }
    }

    /// Count tokens in `text` with the active counter.
    pub fn count(&self, text: &str) -> usize {
        match &self.tokenizer {
            Some(tokenizer) => tokenizer
                .encode(text, false)
                .map(|encoding| encoding.get_ids().len())
                .unwrap_or_else(|_| estimate_tokens(text)),
            None => estimate_tokens(text),
        }
    }
}

/// `$HOME/.leindex/models/tokenizer.json`, overridable for tests.
fn candidate_tokenizer_paths() -> Vec<PathBuf> {
    if let Some(path) = std::env::var_os("LEINDEX_TOKBENCH_TOKENIZER") {
        return vec![PathBuf::from(path)];
    }
    let Some(home) = std::env::var_os("HOME") else {
        return Vec::new();
    };
    vec![PathBuf::from(home)
        .join(".leindex")
        .join("models")
        .join("tokenizer.json")]
}

/// The house estimate: ceil(chars / 4), matching `headtohead.py`'s W6 model.
pub fn estimate_tokens(text: &str) -> usize {
    text.chars().count().div_ceil(4)
}

#[cfg(test)]
#[path = "tokens_test.rs"]
mod tests;

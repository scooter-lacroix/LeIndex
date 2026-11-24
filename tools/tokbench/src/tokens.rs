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
//!
//! Two guarantees make the `Tokenizer` label trustworthy:
//!
//! - truncation and padding configured in the loaded file are cleared before
//!   anything is measured, so a capped or padded encoding is never reported
//!   as the size of a tool response;
//! - an encoding error from the tokenizer is never silently replaced by an
//!   estimate. [`Counter::count`] returns the error, and the run restarts
//!   with the estimator under its own label (see `tasks::run_all`).

use std::path::PathBuf;
use std::sync::Arc;

/// Which counter produced the numbers in a report.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum TokenCounter {
    /// Real BPE token counts via the LeIndex embedder tokenizer file.
    Tokenizer,
    /// chars/4 house estimate (`headtohead.py` W6 model).
    Estimate,
}

/// Why a loaded tokenizer could not count a text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TokenizerError {
    /// What the tokenizer reported.
    pub message: String,
}

impl std::fmt::Display for TokenizerError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "tokenizer encode failed: {}", self.message)
    }
}

impl std::error::Error for TokenizerError {}

/// Load the tokenizer from the runtime model directory when present, else
/// count chars/4. `LEINDEX_TOKBENCH_TOKENIZER` overrides the file path
/// (used by tests to exercise both branches deterministically).
///
/// `Clone` hands back the SAME loaded tokenizer (shared through an `Arc`),
/// never a re-read, so a run's report is always labelled with the counter
/// that measured it.
#[derive(Clone)]
pub struct Counter {
    kind: TokenCounter,
    tokenizer: Option<Arc<tokenizers::Tokenizer>>,
}

impl Counter {
    /// Build the counter from the default search paths.
    pub fn load() -> Self {
        match Self::load_tokenizer(candidate_tokenizer_paths()) {
            Some(tokenizer) => Self {
                kind: TokenCounter::Tokenizer,
                tokenizer: Some(tokenizer),
            },
            None => Self::estimate(),
        }
    }

    /// The chars/4 estimator, explicitly.
    pub fn estimate() -> Self {
        Self {
            kind: TokenCounter::Estimate,
            tokenizer: None,
        }
    }

    /// A counter built from a tokenizer that has already been loaded
    /// (tests construct one from a fixed vocabulary).
    #[cfg(test)]
    pub fn from_tokenizer(tokenizer: tokenizers::Tokenizer) -> Self {
        match Self::for_measurement(tokenizer) {
            Ok(tokenizer) => Self {
                kind: TokenCounter::Tokenizer,
                tokenizer: Some(Arc::new(tokenizer)),
            },
            // Clearing truncation/padding only fails on a malformed file; a
            // tokenizer that cannot be made fit for measurement is not used.
            Err(_) => Self::estimate(),
        }
    }

    /// First loadable candidate, with truncation and padding cleared.
    fn load_tokenizer(paths: Vec<PathBuf>) -> Option<Arc<tokenizers::Tokenizer>> {
        paths.into_iter().find_map(|path| {
            tokenizers::Tokenizer::from_file(&path)
                .ok()
                .and_then(|tokenizer| Self::for_measurement(tokenizer).ok())
                .map(Arc::new)
        })
    }

    /// Measurement must see the whole text: a tokenizer file may configure
    /// truncation (capping the token count) or padding (inflating it), and
    /// `encode(text, false)` applies both. Both are cleared here so the
    /// count reflects the text, not the file's training-time settings.
    fn for_measurement(
        mut tokenizer: tokenizers::Tokenizer,
    ) -> Result<tokenizers::Tokenizer, TokenizerError> {
        // On the loaded tokenizer these are `&mut self` setters; clearing
        // cannot fail (`None` carries no parameters to validate), but the
        // truncation setter is still fallible, so the error is surfaced
        // rather than dropped.
        tokenizer
            .with_truncation(None)
            .map_err(|error| TokenizerError {
                message: error.to_string(),
            })?;
        tokenizer.with_padding(None);
        Ok(tokenizer)
    }

    /// Which counter is active.
    pub fn kind(&self) -> TokenCounter {
        self.kind
    }

    /// Human-readable provenance for report headers.
    pub fn provenance(&self) -> &'static str {
        match self.kind {
            TokenCounter::Tokenizer => "real tokens (LeIndex embedder tokenizer.json)",
            TokenCounter::Estimate => "ESTIMATE chars/4 (tokenizer.json not found)",
        }
    }

    /// Count tokens in `text` with the active counter.
    ///
    /// With a tokenizer this can fail (for example a BPE model that needs a
    /// token missing from its vocabulary). The error is returned rather than
    /// replaced by an estimate: the caller decides how to label a fallback.
    pub fn count(&self, text: &str) -> Result<usize, TokenizerError> {
        match &self.tokenizer {
            Some(tokenizer) => tokenizer
                .encode(text, false)
                .map(|encoding| encoding.get_ids().len())
                .map_err(|error| TokenizerError {
                    message: error.to_string(),
                }),
            None => Ok(estimate_tokens(text)),
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
    vec![
        PathBuf::from(home)
            .join(".leindex")
            .join("models")
            .join("tokenizer.json"),
    ]
}

/// The house estimate: ceil(chars / 4), matching `headtohead.py`'s W6 model.
/// Used when the tokenizer file is absent (Counter falls back) or a run is
/// restarted under the estimate label after a tokenizer failure.
#[cfg_attr(not(test), allow(dead_code))]
pub fn estimate_tokens(text: &str) -> usize {
    text.chars().count().div_ceil(4)
}

#[cfg(test)]
#[path = "tokens_test.rs"]
mod tests;

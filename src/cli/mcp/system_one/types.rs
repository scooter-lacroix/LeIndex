//! Provider-neutral request/answer types for System-One (typed-decision) models.
//!
//! Only the two primitives LeIndex needs are modelled: a yes/no question and a
//! pick-one question. The ordinal `score` primitive is deliberately absent: the
//! Laya model card rates it the weakest primitive.

use serde_json::Value;
use std::collections::HashMap;
use std::future::Future;

/// A typed question.
#[derive(Debug, Clone, PartialEq)]
pub enum Question {
    /// Probability that a condition is true.
    YesNo {
        /// The exact condition to evaluate.
        instructions: String,
    },
    /// Choose exactly one of `options` (`(value, description)` pairs).
    Pick {
        /// The question.
        instructions: String,
        /// Allowed values with a description of when each applies.
        options: Vec<(String, String)>,
    },
}

/// A typed answer. `p_yes` and every probability are finite and within `0..=1`.
#[derive(Debug, Clone, PartialEq)]
pub enum Answer {
    /// Answer to [`Question::YesNo`].
    YesNo {
        /// Probability the condition holds.
        p_yes: f32,
    },
    /// Answer to [`Question::Pick`]; `choice` is always one of the offered values.
    Pick {
        /// The selected option value.
        choice: String,
        /// Probability per option.
        probabilities: Vec<(String, f32)>,
        /// Model confidence, or the top probability when the provider omits it.
        confidence: f32,
    },
}

/// One evaluation: shared evidence plus named questions.
#[derive(Debug, Clone)]
pub struct Request {
    /// Evidence every question is evaluated against.
    pub state: Value,
    /// `(name, question)` pairs; names are unique within a request.
    pub questions: Vec<(String, Question)>,
}

/// Decoded answers keyed by question name. A question the provider refused or
/// answered invalidly is simply absent.
#[derive(Debug, Default, Clone)]
pub struct Response {
    /// Answers by question name.
    pub answers: HashMap<String, Answer>,
    /// Billed input tokens when the provider reports them.
    pub input_tokens: Option<u64>,
}

impl Response {
    /// Probability of "yes" for a yes/no question.
    pub fn p_yes(&self, name: &str) -> Option<f32> {
        match self.answers.get(name) {
            Some(Answer::YesNo { p_yes }) => Some(*p_yes),
            _ => None,
        }
    }

    /// `(choice, confidence)` for a pick question.
    pub fn pick(&self, name: &str) -> Option<(&str, f32)> {
        match self.answers.get(name) {
            Some(Answer::Pick {
                choice, confidence, ..
            }) => Some((choice.as_str(), *confidence)),
            _ => None,
        }
    }
}

/// Which wire format and defaults to use.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Provider {
    /// Self-hosted Laya (`laya-serve`): Jev-compatible `/v1/systemone`.
    Laya,
    /// TypeSafe Jev: `POST https://api.typesafe.ai/v1/systemone`.
    Jev,
    /// OpenAI Decisions: `POST https://api.openai.com/v1/decisions`.
    OpenAi,
}

impl Provider {
    /// Parse the `[system_one] provider` config value.
    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "laya" => Some(Self::Laya),
            "jev" | "typesafe" => Some(Self::Jev),
            "openai" => Some(Self::OpenAi),
            _ => None,
        }
    }

    /// True for hosted providers that require an API key. This is not a
    /// privacy boundary: any provider's configured endpoint may be remote.
    pub fn is_remote(self) -> bool {
        !matches!(self, Self::Laya)
    }
}

/// Why a System-One call produced no usable answer. Every variant means the
/// gate must fail open.
#[derive(Debug, thiserror::Error)]
pub enum SystemOneError {
    /// Connection, timeout, or client construction failure.
    #[error("transport: {0}")]
    Transport(String),
    /// Non-2xx HTTP status (401/422 are not retryable; 429/529 are rate limits).
    #[error("HTTP {0}")]
    Status(u16),
    /// The body was not the documented shape.
    #[error("bad response: {0}")]
    Decode(String),
}

/// Anything that can answer a [`Request`].
pub trait Asker: Send + Sync {
    /// Evaluate `request`. Implementations must not retry: the gate is
    /// latency-bounded and fails open.
    fn ask(
        &self,
        request: &Request,
    ) -> impl Future<Output = Result<Response, SystemOneError>> + Send;
}

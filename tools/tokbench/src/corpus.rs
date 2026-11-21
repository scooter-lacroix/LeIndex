//! The pinned task corpus: a generated fixture workspace + the 7 seed tasks.
//!
//! The fixture is a small, realistic multi-file Rust workspace (an LLM
//! gateway with per-provider request assembly). It is GENERATED from the
//! embedded [`SOURCE_FILES`] constant — never vendored as loose files — so
//! the corpus is pinned by the hash of that constant (reported in every run)
//! and cannot drift from the code that exercises it. Ground truths encoded
//! in the task assertions:
//!
//! - `crates/api/src/lib.rs` defines `ChatCompletionRequestBody` with its
//!   builder (`ChatCompletionRequestBody::new`).
//! - Four construction/call sites thread through `crates/server/src/lib.rs`
//!   (`build_chat_request`), `crates/server/src/routes.rs`,
//!   `crates/server/src/outbound.rs`, and the two provider crates.
//! - `crates/provider-openai/src/lib.rs` carries a deliberately failing
//!   `#[test]` (`test_openai_request_carries_provider_tag`), asserting a
//!   `provider_tag` marker the builder does not produce — task 3 reads its
//!   failure output.
//! - `Option<String>?` appears NOWHERE, so task 7's regex-looking literal is
//!   a genuine zero-result search.

use anyhow::{Context, Result};
use sha2::{Digest, Sha256};
use std::path::Path;

/// The pinned fixture, as `(relative path, file content)` pairs.
pub const SOURCE_FILES: &[(&str, &str)] = &[
    (
        "Cargo.toml",
        r####"[workspace]
resolver = "2"
members = [
    "crates/api",
    "crates/server",
    "crates/provider-openai",
    "crates/provider-anthropic",
]

[workspace.package]
edition = "2021"
version = "0.3.0"
license = "MIT"

[profile.dev]
debug = 0
"####,
    ),
    (
        ".gitignore",
        "/target\n/.leindex\n",
    ),
    (
        "README.md",
        "# tokbench fixture gateway\n\nA small LLM gateway workspace used by the tokbench task corpus. Provider\nrequest bodies are assembled in `crates/api`, mapped from internal requests\nby `crates/server`, and dispatched per provider.\n",
    ),
    (
        "crates/api/Cargo.toml",
        r####"[package]
name = "api"
version.workspace = true
edition.workspace = true
license.workspace = true

[dependencies]
"####,
    ),
    (
        "crates/api/src/lib.rs",
        r####"//! Shared request/response types for the gateway workspace.
//!
//! `ChatCompletionRequestBody` is assembled by each provider crate; the
//! server crate maps internal chat requests onto it.

pub mod testutil;

/// Provider-agnostic chat request as assembled by each provider crate.
#[derive(Debug, Clone)]
pub struct ChatCompletionRequestBody {
    /// Model identifier as the upstream API expects it.
    pub model: String,
    /// Conversation so far, oldest first.
    pub messages: Vec<Message>,
    /// Sampling temperature; `None` lets the upstream default apply.
    pub temperature: Option<f32>,
}

/// A single conversation message.
#[derive(Debug, Clone)]
pub struct Message {
    /// `system`, `user`, `assistant` or `tool`.
    pub role: String,
    /// Message body.
    pub content: String,
}

impl ChatCompletionRequestBody {
    /// Assemble a request body for `model` from the conversation.
    ///
    /// Provider crates call this builder then set provider-specific fields
    /// on the returned value.
    pub fn new(model: impl Into<String>, messages: Vec<Message>) -> Self {
        Self {
            model: model.into(),
            messages,
            temperature: None,
        }
    }
}
"####,
    ),
    (
        "crates/api/src/testutil.rs",
        r####"//! Shared test helpers for provider request assembly.
//!
//! A `ChatCompletionRequestBody` here is one assertion away from a failing
//! provider build, so keep this in sync with `api::ChatCompletionRequestBody`.

use crate::{ChatCompletionRequestBody, Message};

/// Build the request body the provider tests assert against.
pub fn sample_chat_request(model: &str) -> ChatCompletionRequestBody {
    ChatCompletionRequestBody::new(
        model,
        vec![Message {
            role: "user".to_string(),
            content: "ping".to_string(),
        }],
    )
}
"####,
    ),
    (
        "crates/server/Cargo.toml",
        r####"[package]
name = "server"
version.workspace = true
edition.workspace = true
license.workspace = true

[dependencies]
api = { path = "../api" }
serde = { version = "1", features = ["derive"] }

[dev-dependencies]
"####,
    ),
    (
        "crates/server/src/lib.rs",
        r####"//! Request assembly helpers and the chat-completion pipeline for the gateway.

pub mod routes;
pub mod outbound;

use api::{ChatCompletionRequestBody, Message};

/// Assemble the upstream request body for a completed internal chat request.
pub fn build_chat_request(model: &str, prompt: &str) -> ChatCompletionRequestBody {
    ChatCompletionRequestBody::new(
        model,
        vec![Message {
            role: "user".to_string(),
            content: prompt.to_string(),
        }],
    )
}
"####,
    ),
    (
        "crates/server/src/routes.rs",
        r####"//! HTTP surface of the gateway server.
//!
//! The chat endpoint is the only place that assembles provider requests.

use crate::build_chat_request;
use api::Message;

/// Body of `POST /v1/chat`.
#[derive(Debug, serde::Deserialize)]
pub struct ChatRequestBody {
    /// Model the client asked for.
    pub model: String,
    /// Single-shot prompt from the client.
    pub prompt: String,
}

/// Handle `POST /v1/chat`.
///
/// Route handlers are thin: assemble the provider request body and hand it to
/// the outbound dispatcher.
pub fn handle_chat(body: ChatRequestBody) -> String {
    let outbound = build_chat_request(&body.model, &body.prompt);
    let replies = crate::outbound::dispatch(&outbound);
    format!(
        "routed to {} provider(s); first reply: {}",
        replies.len(),
        replies
            .first()
            .map(|r| r.body.as_str())
            .unwrap_or("<none>")
    )
}

/// Convenience constructor used by the health-check route tests.
pub fn healthcheck_message() -> Message {
    Message {
        role: "system".to_string(),
        content: "ok".to_string(),
    }
}
"####,
    ),
    (
        "crates/server/src/outbound.rs",
        r####"//! Outbound provider dispatch for assembled chat requests.

use api::ChatCompletionRequestBody;

/// Where a dispatched request went and what came back.
#[derive(Debug)]
pub struct ProviderReply {
    /// Provider that handled the request, e.g. `openai`.
    pub provider: &'static str,
    /// Reply text returned by the provider.
    pub body: String,
}

/// Send the assembled request body to every configured provider.
///
/// Construction sites of `ChatCompletionRequestBody` outside `api` and
/// `server::routes` belong to the per-provider modules below.
pub fn dispatch(request: &ChatCompletionRequestBody) -> Vec<ProviderReply> {
    vec![
        ProviderReply {
            provider: "openai",
            body: crate::build_chat_request(&request.model, "openai copy of the prompt")
                .messages
                .len()
                .to_string(),
        },
        ProviderReply {
            provider: "anthropic",
            body: crate::build_chat_request(&request.model, "anthropic copy of the prompt")
                .messages
                .len()
                .to_string(),
        },
    ]
}
"####,
    ),
    (
        "crates/provider-openai/Cargo.toml",
        r####"[package]
name = "provider-openai"
version.workspace = true
edition.workspace = true
license.workspace = true

[dependencies]
api = { path = "../api" }
server = { path = "../server" }
"####,
    ),
    (
        "crates/provider-openai/src/lib.rs",
        r####"//! OpenAI provider: request assembly + a failing audit test.
//!
//! The test exists to be broken: tokbench task 3 runs it, reads the
//! compiler/test failure, and locates the failing line.

/// Extra completion options layered onto the assembled request body.
#[derive(Debug, Default)]
pub struct OpenAiOptions {
    /// Cap on upstream completion tokens.
    pub max_tokens: usize,
    /// Injected into the system slot of every request.
    pub system_prompt: Option<String>,
}

/// The provider's default system prompt.
pub const DEFAULT_SYSTEM_PROMPT: &str = "You are a terse assistant.";

/// Build the outbound request body for the OpenAI provider.
pub fn build_openai_request(model: &str, prompt: &str, options: &OpenAiOptions) -> String {
    let mut body = server::build_chat_request(model, prompt);
    body.temperature = Some(0.2);
    if let Some(system) = &options.system_prompt {
        body.messages.insert(
            0,
            api::Message {
                role: "system".to_string(),
                content: system.clone(),
            },
        );
    }
    format!("openai:{}:{}:{}", body.model, body.messages.len(), options.max_tokens)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Deliberately broken for the tokbench task-3 corpus: asserts a field
    /// that no longer exists after the provider refactor.
    #[test]
    fn test_openai_request_carries_provider_tag() {
        let options = OpenAiOptions::default();
        let request = build_openai_request("gpt-test", "hello", &options);
        assert!(request.contains("provider_tag"), "missing provider_tag in {request}");
    }
}
"####,
    ),
    (
        "crates/provider-anthropic/Cargo.toml",
        r####"[package]
name = "provider-anthropic"
version.workspace = true
edition.workspace = true
license.workspace = true

[dependencies]
api = { path = "../api" }
server = { path = "../server" }
"####,
    ),
    (
        "crates/provider-anthropic/src/lib.rs",
        r####"//! Anthropic provider: request assembly.

use api::testutil::sample_chat_request;

/// Extra completion options layered onto the assembled request body.
#[derive(Debug)]
pub struct AnthropicOptions {
    /// Injected into the system slot of every request.
    pub system_prompt: Option<String>,
    /// Anthropic API version string sent as a header.
    pub api_version: String,
}

impl Default for AnthropicOptions {
    fn default() -> Self {
        Self {
            system_prompt: None,
            api_version: "2023-06-01".to_string(),
        }
    }
}

/// Build the outbound request body for the Anthropic provider.
pub fn build_anthropic_request(model: &str, prompt: &str, options: &AnthropicOptions) -> String {
    let mut body = server::build_chat_request(model, prompt);
    body.temperature = Some(0.7);
    if let Some(system) = &options.system_prompt {
        body.messages.insert(
            0,
            api::Message {
                role: "system".to_string(),
                content: system.clone(),
            },
        );
    }
    format!(
        "anthropic:{}:{}:{}",
        body.model, body.messages.len(), options.api_version
    )
}

/// Sanity check used by the gateway smoke path (not `#[test]`-wired).
pub fn anthropic_smoke(model: &str) -> String {
    let options = AnthropicOptions::default();
    let _ = sample_chat_request(model);
    build_anthropic_request(model, "smoke", &options)
}
"####,
    ),
];

/// sha256 over every file's path + content, in table order. Recorded in the
/// report so a corpus change is visible in every baseline comparison.
pub fn corpus_hash() -> String {
    let mut hasher = Sha256::new();
    for (path, content) in SOURCE_FILES {
        hasher.update(path.as_bytes());
        hasher.update(b"\0");
        hasher.update(content.as_bytes());
        hasher.update(b"\0");
    }
    format!("{:x}", hasher.finalize())
}

/// Materialize the fixture into `dest` (must exist) and initialize a git
/// repo with one commit — the state every task starts from.
pub fn materialize(dest: &Path) -> Result<()> {
    for (relative, content) in SOURCE_FILES {
        let path = dest.join(relative);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("create dir for {relative}"))?;
        }
        std::fs::write(&path, content).with_context(|| format!("write {relative}"))?;
    }
    for args in [
        ["init"].as_slice(),
        ["config", "user.email", "tokbench@example.test"].as_slice(),
        ["config", "user.name", "tokbench fixture"].as_slice(),
        ["add", "."].as_slice(),
        ["commit", "-m", "fixture: pinned tokbench corpus"].as_slice(),
    ] {
        let output = std::process::Command::new("git")
            .args(args)
            .current_dir(dest)
            .output()
            .context("git init fixture")?;
        anyhow::ensure!(
            output.status.success(),
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr)
        );
    }
    Ok(())
}

#[cfg(test)]
#[path = "corpus_test.rs"]
mod tests;

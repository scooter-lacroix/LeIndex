//! Interactive model picker for an external embeddings route.
//!
//! Split out of `setup_routes.rs` (already over the module line budget) because
//! choosing a served model is self-contained presentation: it owns no route
//! resolution, no validation and no config mutation, and it is the only place in
//! setup that renders more than one prompt for a single question.
//!
//! Discovery lives in `search::onnx::remote_models`, which owns the listing wire
//! format. This module only decides how to *present* what discovery returned —
//! in particular, that a listing mixing chat, vision and embedding models opens
//! on the embedding-looking subset while keeping the full catalogue reachable.

use super::SetupError;
// Only the picker prompts use it, so it is gated: the free-text fallback still
// compiles in builds without `remote-embeddings`, where this would be unused.
#[cfg(feature = "remote-embeddings")]
use super::setup_routes::prompt;
#[cfg(feature = "remote-embeddings")]
use dialoguer::Select;

/// Choose one served model, defaulting to the embedding-looking subset.
///
/// A listing mixes chat, vision, and embedding models, and the route being
/// configured can only use an embedding model, so the picker opens on the
/// models that look like embedding models. The full listing stays one entry away
/// because the heuristic is a name match over an API that reports no modality:
/// a real embedding model with an unusual name must never be unreachable.
#[cfg(feature = "remote-embeddings")]
pub(super) fn pick_model(models: &[String], current: &str) -> Result<String, SetupError> {
    use crate::search::onnx::remote::looks_like_embedding_model;

    let embedding: Vec<&str> = models
        .iter()
        .map(String::as_str)
        .filter(|model| looks_like_embedding_model(model))
        .collect();
    // Nothing recognizable, or nothing filtered out: a second prompt would
    // offer the same rows twice, so the full listing IS the picker.
    if embedding.is_empty() || embedding.len() == models.len() {
        return select_model(models, current, "Model");
    }
    // The show-all row is built LAST and is the only row not backed by a model,
    // so its position is derived here rather than remembered in a parallel
    // `Option<usize>` sentinel that a caller could compare against the wrong
    // index. Every row before it maps 1:1 onto `embedding`, so a chosen index is
    // usable directly.
    let mut rows: Vec<&str> = embedding.clone();
    rows.push("[show all served models]");
    let show_all = rows.len() - 1;
    let chosen = select_row(&rows, current, "Model (embedding first)")?;
    if chosen == show_all {
        return select_model(models, current, "Model (all served)");
    }
    Ok(embedding[chosen].to_string())
}

/// Prompt over `models` and return the chosen identifier.
#[cfg(feature = "remote-embeddings")]
fn select_model(models: &[String], current: &str, prompt: &str) -> Result<String, SetupError> {
    let rows: Vec<&str> = models.iter().map(String::as_str).collect();
    Ok(models[select_row(&rows, current, prompt)?].to_string())
}

/// Prompt over `rows` and return the chosen index, defaulting to `current`.
///
/// Returns an INDEX rather than an enum: each caller interprets its own row
/// list, so no caller has to handle an outcome its rows cannot produce. That is
/// what removes an `unreachable!` arm — a panic in an interactive wizard is a
/// crash for the user, not a compile error.
///
/// The default matters on a re-run: a model the user did not touch must stay
/// selected, including one the heuristic filtered out (it is absent from the
/// filtered rows, which is exactly what the show-all row exists for).
#[cfg(feature = "remote-embeddings")]
fn select_row(rows: &[&str], current: &str, prompt: &str) -> Result<usize, SetupError> {
    let default = rows.iter().position(|row| *row == current).unwrap_or(0);
    Select::new()
        .with_prompt(prompt)
        .items(rows)
        .default(default)
        .interact()
        .map_err(|e| SetupError::Interactive(e.to_string()))
}

/// The free-text fallback, kept beside the picker so both paths live together.
///
/// Used when discovery produced no usable list — an endpoint that hides its
/// catalogue, or a build without `remote-embeddings`. The prompt is the whole
/// contract on that path, so it must stay reachable.
pub(super) fn prompt_free_text(current: &str) -> Result<String, SetupError> {
    prompt("Server model identifier", current.to_string())
}

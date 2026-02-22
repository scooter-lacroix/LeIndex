//! SSE (Server-Sent Events) streaming support for indexing progress
//!
//! This module provides streaming progress updates during project indexing.

use super::protocol::{JsonRpcError, ProgressEvent};
use crate::leindex::LeIndex;
use axum::{
    response::{sse::{Event, Sse}, Json},
};
use futures_util::stream::{Stream, StreamExt};
use serde_json::Value;
use std::convert::Infallible;
use std::sync::Arc;
use tokio::sync::{mpsc, Mutex};
use tokio_stream::wrappers::ReceiverStream;
use tracing::info;

/// Global state access from parent module
use super::server::SERVER_STATE;

/// SSE handler for streaming indexing progress
///
/// This endpoint accepts POST requests with indexing parameters
/// and returns an SSE stream of progress events.
///
/// # Arguments
///
/// * `body` - JSON request body containing:
///   - `project_path` - Absolute path to project directory to index
///   - `force_reindex` - Optional boolean to force re-indexing
///
/// # Returns
///
/// Sse stream that sends progress events as indexing progresses
pub async fn index_stream_handler(
    Json(body): Json<Value>,
) -> Sse<impl Stream<Item = Result<Event, Infallible>> + Send> {
    // Create a channel for sending events
    let (tx, rx) = mpsc::channel::<ProgressEvent>(100);

    // Spawn background task for indexing
    tokio::spawn(async move {
        let state = match SERVER_STATE.get() {
            Some(s) => s,
            None => {
                let _ = tx.send(ProgressEvent::error("Server not initialized")).await;
                return;
            }
        };

        // Extract parameters from body
        let project_path = match body.get("project_path").and_then(|v: &Value| v.as_str()) {
            Some(p) => p.to_string(),
            None => {
                let _ = tx.send(ProgressEvent::error("Missing project_path")).await;
                return;
            }
        };

        let force_reindex = body
            .get("force_reindex")
            .and_then(|v: &Value| v.as_bool())
            .unwrap_or(false);

        // Send starting event
        let _ = tx.send(ProgressEvent::progress(
                "starting",
                0,
                0,
                format!("Starting indexing for: {}", project_path),
            ))
            .await;

        // Perform indexing with progress callbacks
        match index_with_progress(state, &project_path, force_reindex, tx.clone()).await {
            Ok(stats) => {
                let _ = tx.send(ProgressEvent::complete(
                        "indexing",
                        format!("Done: {} files", stats.files_parsed),
                    ))
                    .await;
            }
            Err(e) => {
                let _ = tx.send(ProgressEvent::error(format!(
                        "Error: {}", e)))
                    .await;
            }
        }
    });

    // Create SSE stream from receiver
    let stream = ReceiverStream::new(rx)
        .map(|event| -> Result<Event, Infallible> {
            let event_data = Event::default()
                .json_data(event)
                .unwrap_or_else(|_| Event::default().data("error".to_string()));
            Ok(event_data)
        });

    Sse::new(stream)
        .keep_alive(
            axum::response::sse::KeepAlive::new()
                .interval(std::time::Duration::from_secs(15))
                .text("keep-alive")
        )
}

/// Perform indexing with progress reporting via channel
///
/// This helper function runs the indexing operation while sending progress
/// events through the provided channel.
///
/// # Arguments
///
/// * `state` - Reference to global LeIndex state
/// * `project_path` - Path to project to index
/// * `force_reindex` - Whether to re-index even if already indexed
/// * `tx` - Channel sender for progress events
///
/// # Returns
///
/// * `Result<IndexStats, JsonRpcError>` - Index statistics or error
pub async fn index_with_progress(
    state: &Arc<Mutex<LeIndex>>,
    project_path: &str,
    force_reindex: bool,
    tx: mpsc::Sender<ProgressEvent>,
) -> Result<crate::leindex::IndexStats, JsonRpcError> {
    let index = state.lock().await;

    // Check if already indexed and we're not forcing reindex
    if index.is_indexed() && !force_reindex {
        let _ = tx.send(ProgressEvent::progress(
                    "skipping",
                    1,
                    1,
                    "Already indexed",
                ))
                .await;
        return Ok(index.get_stats().clone());
    }

    // Send collecting files event
    let _ = tx.send(ProgressEvent::progress(
                "collecting",
                0,
                0,
                "Collecting source files...",
            ))
            .await;

    // Perform indexing in blocking task
    let project_path = project_path.to_string();
    let project_path_for_blocking = project_path.clone();
    let stats = tokio::task::spawn_blocking(move || {
        let mut temp_leindex = LeIndex::new(&project_path_for_blocking).map_err(|e| {
            JsonRpcError::indexing_failed(format!("Failed to create LeIndex: {}", e))
        })?;

        temp_leindex
            .index_project(force_reindex)
            .map_err(|e| {
                JsonRpcError::indexing_failed(format!("Indexing failed: {}", e))
            })
    })
    .await
        .map_err(|e| JsonRpcError::internal_error(format!("Task join error: {}", e)))??;

    // Update shared state by loading newly indexed project from storage
    let mut index = state.lock().await;

    let path = std::path::Path::new(&project_path)
        .canonicalize()
        .map_err(|e| JsonRpcError::internal_error(format!("Failed to canonicalize path: {}", e)))
        ?;

    if index.project_path() != path {
        info!("Switching projects: {:?} -> {:?}", index.project_path(), path);
        let _ = tx.send(ProgressEvent::progress(
                    "switching_projects",
                    0,
                    0,
                    format!("{:?}", index.project_path()),
                ))
                .await;

        let _ = index.close();
        *index = LeIndex::new(&path).map_err(|e| {
            JsonRpcError::indexing_failed(format!("Failed to re-initialize LeIndex: {}", e))
        })?;
    }

    let _ = tx.send(ProgressEvent::progress(
                "loading_storage",
                0,
                0,
                "Loading indexed data from storage...",
            ))
            .await;

    index
        .load_from_storage()
        .map_err(|e| {
            JsonRpcError::indexing_failed(format!("Failed to load indexed data: {}", e))
        })?;

    Ok(stats)
}

//! leserve - HTTP/WebSocket Server
//!
//! *Le Serve* (The Server) - Axum-based HTTP/WebSocket server for LeIndex dashboard

#![warn(missing_docs)]
#![warn(unused_extern_crates)]

/// API error types
pub mod error;

/// HTTP handlers for REST endpoints
pub mod handlers;

/// Server configuration from TOML
pub mod config;

/// WebSocket event broadcasting
pub mod websocket;

/// API response types matching frontend contract
pub mod responses;

/// Server instance management
pub mod server;

pub use config::ServerConfig;
pub use error::{ApiError, ApiResult};
pub use server::LeServeServer;

/// leserve library initialization
pub fn init() {
    let _ = tracing::subscriber::set_default(tracing::subscriber::NoSubscriber::default());
}

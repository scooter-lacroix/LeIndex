//! Server instance management

use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use tokio::signal;
use tracing::{error, info};

use crate::config::ServerConfig;
use crate::error::ApiError;
use crate::handlers::{create_router, AppState};
use lestockage::Storage;

/// LeServe HTTP/WebSocket server
///
/// Manages Axum server lifecycle including startup,
/// graceful shutdown, and connection management.
pub struct LeServeServer {
    /// Server configuration
    config: ServerConfig,

    /// Storage layer wrapped in Arc<Mutex> for thread safety
    storage: Arc<Mutex<Storage>>,
}

impl LeServeServer {
    /// Create new server instance
    ///
    /// # Arguments
    ///
    /// * `config` - Server configuration
    ///
    /// # Returns
    ///
    /// `Result<LeServeServer, ApiError>` - Server or error
    pub fn new(config: ServerConfig) -> Result<Self, ApiError> {
        // Validate config
        if let Err(e) = config.validate() {
            return Err(ApiError::internal(format!("Invalid config: {}", e)));
        }

        // Open storage
        let storage = Storage::open(&config.db_path)
            .map_err(|e| {
                error!("Failed to open storage: {}", e);
                ApiError::internal(format!("Failed to open storage: {}", e))
            })?;

        Ok(Self {
            config,
            storage: Arc::new(Mutex::new(storage)),
        })
    }

    /// Get socket address for binding
    ///
    /// # Returns
    ///
    /// `Result<SocketAddr, ApiError>` - Parsed address or error
    pub fn socket_addr(&self) -> Result<SocketAddr, ApiError> {
        format!("{}:{}", self.config.host, self.config.port)
            .parse::<SocketAddr>()
            .map_err(|e| ApiError::internal(format!("Failed to parse address: {}", e)))
    }

    /// Start server
    ///
    /// # Returns
    ///
    /// `Result<(), ApiError>` - Success or error
    pub async fn start(&self) -> Result<(), ApiError> {
        let addr = self.socket_addr()?;

        // Build application state (clone Arc<Mutex<Storage>>)
        let state = AppState::new_from_arc(Arc::clone(&self.storage), self.config.clone());

        // Build router
        let app = create_router().with_state(state);

        // Create server
        let listener = tokio::net::TcpListener::bind(addr)
            .await
            .map_err(|e| {
                error!("Failed to bind to {}: {:?}", addr, e);
                ApiError::internal(format!("Failed to bind to {}: {}", addr, e))
            })?;

        info!("Server listening on: http://{}:{}", self.config.host, self.config.port);

        axum::serve(listener, app).await
            .map_err(|e| ApiError::internal(format!("Server error: {}", e)))
    }

    /// Wait for shutdown signal
    ///
    /// Blocks until Ctrl+C is received
    pub async fn wait_for_shutdown(&self) {
        let ctrl_c = async {
            signal::ctrl_c()
                .await
                .expect("Failed to install Ctrl+C handler");
            info!("Received shutdown signal");
        };

        #[cfg(unix)]
        let terminate = async {
            use tokio::signal::unix;
            unix::signal(unix::SignalKind::terminate())
                .expect("Failed to install TERM handler")
                .recv()
                .await;
            info!("Received TERM signal");
        };

        #[cfg(not(unix))]
        let terminate = std::future::pending::<()>();

        tokio::select! {
            _ = ctrl_c => {},
            _ = terminate => {},
        }
    }

    /// Get storage reference
    ///
    /// # Returns
    ///
    /// Reference to Arc<Mutex<Storage>>
    #[must_use]
    pub fn storage(&self) -> Arc<Mutex<Storage>> {
        Arc::clone(&self.storage)
    }

    /// Get server URL
    ///
    /// # Returns
    ///
    /// Formatted server URL
    #[must_use]
    pub fn server_url(&self) -> String {
        format!("http://{}:{}", self.config.host, self.config.port)
    }

    /// Get WebSocket URL
    ///
    /// # Returns
    ///
    /// Formatted WebSocket URL
    #[must_use]
    pub fn websocket_url(&self) -> String {
        format!("ws://{}:{}/ws/events", self.config.host, self.config.port)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_server_default_config() {
        let config = ServerConfig::default();
        let server = LeServeServer::new(config);
        assert!(server.is_ok());
    }
}

//! WebSocket event broadcasting

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::broadcast;
use tracing::info;

/// Maximum WebSocket message size (1MB) to prevent DoS attacks
pub const MAX_WS_MESSAGE_SIZE: usize = 1_000_000;

/// Maximum WebSocket frame size (16KB) to prevent memory exhaustion
pub const MAX_WS_FRAME_SIZE: usize = 16_384;

/// WebSocket event types
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum WsEvent {
    /// Project added to registry
    ProjectAdded {
        /// Unique codebase identifier
        codebase_id: String,
        
        /// Display name of the project
        display_name: String,
        
        /// Base name of the project
        base_name: String,
    },

    /// Project metadata updated
    ProjectUpdated {
        /// Unique codebase identifier
        codebase_id: String,
        
        /// New display name of the project
        display_name: String,
    },

    /// Project removed from registry
    ProjectRemoved {
        /// Unique codebase identifier
        codebase_id: String,
    },

    /// Indexing progress update
    #[serde(rename = "indexing.progress")]
    IndexingProgress {
        /// Unique codebase identifier
        codebase_id: String,
        
        /// Current indexing phase
        phase: u32,
        
        /// Progress percentage (0-100)
        percent: u8,
        
        /// Currently processed file
        current_file: String,
    },

    /// Heartbeat/ping
    Heartbeat {
        /// Unix timestamp in milliseconds
        timestamp: u64,
    },
}

impl WsEvent {
    /// Get event type string for serialization
    pub fn event_type(&self) -> &'static str {
        match self {
            Self::ProjectAdded { .. } => "project_added",
            Self::ProjectUpdated { .. } => "project_updated",
            Self::ProjectRemoved { .. } => "project_removed",
            Self::IndexingProgress { .. } => "indexing.progress",
            Self::Heartbeat { .. } => "heartbeat",
        }
    }

    /// Convert to JSON string
    pub fn to_json(&self) -> String {
        serde_json::to_string(self).expect("Failed to serialize WebSocket event to JSON")
    }
}

impl ToString for WsEvent {
    fn to_string(&self) -> String {
        self.to_json()
    }
}

/// Client connection state
#[derive(Debug, Clone)]
pub struct ConnectionState {
    /// Unique connection ID
    pub id: String,

    /// Subscribed project IDs (empty = all projects)
    pub subscriptions: Vec<String>,

    /// Client IP address
    pub ip_addr: Option<String>,
}

impl ConnectionState {
    /// Create new connection state
    pub fn new(id: String, ip_addr: Option<String>) -> Self {
        Self {
            id,
            subscriptions: Vec::new(),
            ip_addr,
        }
    }

    /// Check if connection is subscribed to a project
    pub fn is_subscribed_to(&self, project_id: &str) -> bool {
        // Empty subscriptions = all projects
        self.subscriptions.is_empty() || self.subscriptions.iter().any(|s| s == project_id)
    }

    /// Add subscription
    pub fn subscribe(&mut self, project_id: String) {
        if !self.subscriptions.contains(&project_id) {
            self.subscriptions.push(project_id);
        }
    }

    /// Remove subscription
    pub fn unsubscribe(&mut self, project_id: &str) {
        self.subscriptions.retain(|s| s != project_id);
    }
}

/// WebSocket connection manager
///
/// Tracks all active connections and broadcasts events
#[derive(Clone)]
pub struct WsManager {
    /// Active connections: connection_id -> state
    pub connections: Arc<tokio::sync::RwLock<HashMap<String, ConnectionState>>>,

    /// Broadcast channel for events
    pub broadcaster: broadcast::Sender<WsEvent>,
}

impl WsManager {
    /// Create new WebSocket manager
    pub fn new() -> Self {
        let (broadcaster, _) = broadcast::channel(1000);
        Self {
            connections: Arc::new(tokio::sync::RwLock::new(HashMap::new())),
            broadcaster,
        }
    }

    /// Register a new connection
    pub async fn register_connection(&self, conn_id: String, ip_addr: Option<String>) {
        let state = ConnectionState::new(conn_id.clone(), ip_addr);
        let mut connections = self.connections.write().await;
        connections.insert(conn_id.clone(), state);
        info!("WebSocket connected: {} (active: {})", conn_id, connections.len());
    }

    /// Unregister a connection
    pub async fn unregister_connection(&self, conn_id: &str) {
        let mut connections = self.connections.write().await;
        connections.remove(conn_id);
        info!("WebSocket disconnected: {} (active: {})", conn_id, connections.len());
    }

    /// Broadcast event to all connected clients
    pub async fn broadcast(&self, event: WsEvent) {
        let _ = self.broadcaster.send(event);
    }

    /// Broadcast event to specific project subscribers
    pub async fn broadcast_to_project(&self, _project_id: &str, event: WsEvent) {
        // For now, broadcast to all (filtering done per connection)
        self.broadcast(event).await;
    }

    /// Get number of active connections
    pub async fn connection_count(&self) -> usize {
        self.connections.read().await.len()
    }

    /// Get connection info by ID
    pub async fn get_connection(&self, id: &str) -> Option<ConnectionState> {
        self.connections.read().await.get(id).cloned()
    }
}

impl Default for WsManager {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_ws_event_project_added() {
        let event = WsEvent::ProjectAdded {
            codebase_id: "test_a1b2c3d4_0".to_string(),
            display_name: "Test".to_string(),
            base_name: "test".to_string(),
        };
        assert_eq!(event.event_type(), "project_added");

        let json = event.to_json();
        assert!(json.contains(r#""type":"project_added""#));
        assert!(json.contains("test_a1b2c3d4_0"));
    }

    #[test]
    fn test_ws_event_indexing_progress() {
        let event = WsEvent::IndexingProgress {
            codebase_id: "test_a1b2c3d4_0".to_string(),
            phase: 2,
            percent: 45,
            current_file: "src/lib.rs".to_string(),
        };
        assert_eq!(event.event_type(), "indexing.progress");

        let json = event.to_json();
        assert!(json.contains(r#""type":"indexing.progress""#));
        assert!(json.contains("45"));
    }

    #[test]
    fn test_ws_event_heartbeat() {
        let event = WsEvent::Heartbeat {
            timestamp: 1234567890,
        };
        assert_eq!(event.event_type(), "heartbeat");
    }

    #[test]
    fn test_connection_state_new() {
        let state = ConnectionState::new(
            "conn_1".to_string(),
            Some("127.0.0.1:12345".to_string()),
        );
        assert_eq!(state.id, "conn_1");
        assert_eq!(state.ip_addr, Some("127.0.0.1:12345".to_string()));
        assert!(state.subscriptions.is_empty());
    }

    #[test]
    fn test_connection_state_subscribe() {
        let mut state = ConnectionState::new("conn_1".to_string(), None);
        state.subscribe("proj_1".to_string());
        state.subscribe("proj_2".to_string());

        assert_eq!(state.subscriptions.len(), 2);
        assert!(state.is_subscribed_to("proj_1"));
        assert!(state.is_subscribed_to("proj_2"));
        assert!(!state.is_subscribed_to("proj_3"));
    }

    #[test]
    fn test_connection_state_unsubscribe() {
        let mut state = ConnectionState::new("conn_1".to_string(), None);
        state.subscribe("proj_1".to_string());
        state.subscribe("proj_2".to_string());

        state.unsubscribe("proj_1");
        assert_eq!(state.subscriptions.len(), 1);
        assert!(!state.is_subscribed_to("proj_1"));
        assert!(state.is_subscribed_to("proj_2"));
    }

    #[test]
    fn test_connection_state_empty_subscribes_to_all() {
        let state = ConnectionState::new("conn_1".to_string(), None);
        // Empty subscriptions means all projects
        assert!(state.is_subscribed_to("any_project"));
    }

    #[tokio::test]
    async fn test_ws_manager_new() {
        let manager = WsManager::new();
        let count = manager.connection_count().await;
        assert_eq!(count, 0);
    }

    #[tokio::test]
    async fn test_ws_manager_broadcast() {
        let manager = WsManager::new();
        let event = WsEvent::Heartbeat {
            timestamp: 123,
        };

        // Should not panic
        manager.broadcast(event).await;
    }

    #[tokio::test]
    async fn test_ws_manager_register_connection() {
        let manager = WsManager::new();
        manager.register_connection("conn_1".to_string(), Some("127.0.0.1".to_string())).await;

        assert_eq!(manager.connection_count().await, 1);

        let conn = manager.get_connection("conn_1").await;
        assert!(conn.is_some());
        let conn = conn.expect("Connection should exist after registration");
        assert_eq!(conn.id, "conn_1");
    }

    #[tokio::test]
    async fn test_ws_manager_unregister_connection() {
        let manager = WsManager::new();
        manager.register_connection("conn_1".to_string(), None).await;
        manager.register_connection("conn_2".to_string(), None).await;

        assert_eq!(manager.connection_count().await, 2);

        manager.unregister_connection("conn_1").await;
        assert_eq!(manager.connection_count().await, 1);
    }
}

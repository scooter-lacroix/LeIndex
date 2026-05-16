//! Session model for authentication.

use serde::{Deserialize, Serialize};

/// User session data.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Session {
    pub id: String,
    pub user_id: u64,
    pub token: String,
    pub expires_at: String,
    pub created_at: String,
    pub ip_address: String,
    pub user_agent: String,
}

impl Session {
    pub fn new(user_id: u64, token: String) -> Self {
        Self {
            id: format!("sess-{}", user_id),
            user_id,
            token,
            expires_at: String::from("2024-12-31T23:59:59Z"),
            created_at: String::from("2024-01-01T00:00:00Z"),
            ip_address: String::from("127.0.0.1"),
            user_agent: String::from("test-agent"),
        }
    }

    pub fn is_expired(&self) -> bool {
        // Simplified check for fixture purposes
        false
    }
}

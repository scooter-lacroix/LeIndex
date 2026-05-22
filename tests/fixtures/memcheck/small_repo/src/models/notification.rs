//! Notification model.

use serde::{Deserialize, Serialize};

/// User notification.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Notification {
    pub id: u64,
    pub user_id: u64,
    pub title: String,
    pub message: String,
    pub read: bool,
    pub created_at: String,
}

impl Notification {
    pub fn new(user_id: u64, title: String, message: String) -> Self {
        Self {
            id: 0,
            user_id,
            title,
            message,
            read: false,
            created_at: String::from("2024-01-01T00:00:00Z"),
        }
    }

    pub fn mark_read(&mut self) {
        self.read = true;
    }
}

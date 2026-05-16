//! Comment model.

use serde::{Deserialize, Serialize};

/// Comment on a document.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Comment {
    pub id: u64,
    pub document_id: u64,
    pub author_id: u64,
    pub content: String,
    pub parent_id: Option<u64>,
    pub created_at: String,
    pub updated_at: String,
}

impl Comment {
    pub fn new(document_id: u64, author_id: u64, content: String) -> Self {
        Self {
            id: 0,
            document_id,
            author_id,
            content,
            parent_id: None,
            created_at: String::from("2024-01-01T00:00:00Z"),
            updated_at: String::from("2024-01-01T00:00:00Z"),
        }
    }

    pub fn is_reply(&self) -> bool {
        self.parent_id.is_some()
    }
}

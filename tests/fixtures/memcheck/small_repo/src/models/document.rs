//! Document model for the small repo fixture.

use serde::{Deserialize, Serialize};

/// Represents a document in the system.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Document {
    pub id: u64,
    pub title: String,
    pub content: String,
    pub project_id: u64,
    pub author_id: u64,
    pub version: u32,
    pub status: DocumentStatus,
}

/// Document lifecycle status.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum DocumentStatus {
    Draft,
    Review,
    Published,
    Archived,
}

impl Document {
    pub fn new(id: u64, title: String, project_id: u64, author_id: u64) -> Self {
        Self {
            id,
            title,
            content: String::new(),
            project_id,
            author_id,
            version: 1,
            status: DocumentStatus::Draft,
        }
    }

    pub fn publish(&mut self) -> Result<(), String> {
        if self.title.is_empty() {
            return Err("cannot publish document without title".into());
        }
        self.status = DocumentStatus::Published;
        Ok(())
    }

    pub fn archive(&mut self) {
        self.status = DocumentStatus::Archived;
    }

    pub fn is_published(&self) -> bool {
        self.status == DocumentStatus::Published
    }

    pub fn word_count(&self) -> usize {
        self.content.split_whitespace().count()
    }
}

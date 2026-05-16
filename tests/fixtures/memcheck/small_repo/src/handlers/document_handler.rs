//! Document request handlers.

use crate::models::{Document, DocumentStatus};

/// Handler for document-related operations.
pub struct DocumentHandler {
    documents: Vec<Document>,
}

impl DocumentHandler {
    pub fn new() -> Self {
        Self { documents: Vec::new() }
    }

    pub fn create_document(&mut self, id: u64, title: String, project_id: u64, author_id: u64) -> Document {
        let doc = Document::new(id, title, project_id, author_id);
        self.documents.push(doc.clone());
        doc
    }

    pub fn publish_document(&mut self, id: u64) -> Result<(), String> {
        if let Some(doc) = self.documents.iter_mut().find(|d| d.id == id) {
            doc.publish()
        } else {
            Err("document not found".into())
        }
    }

    pub fn list_published(&self) -> Vec<&Document> {
        self.documents.iter().filter(|d| d.is_published()).collect()
    }

    pub fn get_document(&self, id: u64) -> Option<&Document> {
        self.documents.iter().find(|d| d.id == id)
    }
}

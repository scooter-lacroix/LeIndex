//! Tag model.

use serde::{Deserialize, Serialize};

/// Tag for categorizing resources.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Tag {
    pub id: u64,
    pub name: String,
    pub color: String,
    pub count: u32,
}

impl Tag {
    pub fn new(name: String) -> Self {
        Self {
            id: 0,
            name,
            color: String::from("#000000"),
            count: 0,
        }
    }

    pub fn increment(&mut self) {
        self.count += 1;
    }

    pub fn decrement(&mut self) {
        self.count = self.count.saturating_sub(1);
    }
}

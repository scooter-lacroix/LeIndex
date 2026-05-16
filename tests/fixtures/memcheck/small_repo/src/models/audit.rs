//! Audit log model.

use serde::{Deserialize, Serialize};

/// Audit log entry.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuditEntry {
    pub id: u64,
    pub action: String,
    pub resource_type: String,
    pub resource_id: u64,
    pub actor_id: u64,
    pub timestamp: String,
    pub metadata: serde_json::Value,
}

impl AuditEntry {
    pub fn new(action: String, resource_type: String, resource_id: u64, actor_id: u64) -> Self {
        Self {
            id: 0,
            action,
            resource_type,
            resource_id,
            actor_id,
            timestamp: String::from("2024-01-01T00:00:00Z"),
            metadata: serde_json::Value::Null,
        }
    }
}

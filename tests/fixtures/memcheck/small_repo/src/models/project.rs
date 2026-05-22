//! Project model for the small repo fixture.

use serde::{Deserialize, Serialize};

/// Represents a project in the system.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Project {
    pub id: u64,
    pub name: String,
    pub description: String,
    pub owner_id: u64,
    pub visibility: Visibility,
    pub tags: Vec<String>,
    pub created_at: String,
}

/// Project visibility level.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum Visibility {
    Private,
    Internal,
    Public,
}

impl Project {
    pub fn new(id: u64, name: String, owner_id: u64) -> Self {
        Self {
            id,
            name,
            description: String::new(),
            owner_id,
            visibility: Visibility::Private,
            tags: Vec::new(),
            created_at: String::from("2024-01-01T00:00:00Z"),
        }
    }

    pub fn is_public(&self) -> bool {
        self.visibility == Visibility::Public
    }

    pub fn add_tag(&mut self, tag: String) {
        if !self.tags.contains(&tag) {
            self.tags.push(tag);
        }
    }

    pub fn remove_tag(&mut self, tag: &str) {
        self.tags.retain(|t| t != tag);
    }
}

/// Repository for project persistence.
pub struct ProjectRepository {
    projects: Vec<Project>,
}

impl ProjectRepository {
    pub fn new() -> Self {
        Self { projects: Vec::new() }
    }

    pub fn add(&mut self, project: Project) {
        self.projects.push(project);
    }

    pub fn find_by_id(&self, id: u64) -> Option<&Project> {
        self.projects.iter().find(|p| p.id == id)
    }

    pub fn list_public(&self) -> Vec<&Project> {
        self.projects.iter().filter(|p| p.is_public()).collect()
    }

    pub fn find_by_owner(&self, owner_id: u64) -> Vec<&Project> {
        self.projects.iter().filter(|p| p.owner_id == owner_id).collect()
    }
}

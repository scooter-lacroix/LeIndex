//! Project request handlers.

use crate::models::{Project, ProjectRepository, Visibility};

/// Handler for project-related operations.
pub struct ProjectHandler {
    repo: ProjectRepository,
}

impl ProjectHandler {
    pub fn new(repo: ProjectRepository) -> Self {
        Self { repo }
    }

    pub fn create_project(&mut self, id: u64, name: String, owner_id: u64) -> Project {
        let project = Project::new(id, name, owner_id);
        self.repo.add(project.clone());
        project
    }

    pub fn get_project(&self, id: u64) -> Option<&Project> {
        self.repo.find_by_id(id)
    }

    pub fn list_public_projects(&self) -> Vec<&Project> {
        self.repo.list_public()
    }

    pub fn set_visibility(&mut self, id: u64, visibility: Visibility) -> Result<(), String> {
        if let Some(project) = self.repo.find_by_id(id) {
            let mut project = project.clone();
            project.visibility = visibility;
            Ok(())
        } else {
            Err("project not found".into())
        }
    }
}

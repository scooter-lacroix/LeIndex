//! User request handlers.

use crate::models::{User, UserRepository};

/// Handler for user-related operations.
pub struct UserHandler {
    repo: UserRepository,
}

impl UserHandler {
    pub fn new(repo: UserRepository) -> Self {
        Self { repo }
    }

    pub fn create_user(&mut self, id: u64, username: String, email: String) -> Result<User, String> {
        let user = User::new(id, username, email);
        user.validate()?;
        self.repo.add(user.clone());
        Ok(user)
    }

    pub fn get_user(&self, id: u64) -> Option<&User> {
        self.repo.find_by_id(id)
    }

    pub fn list_active_users(&self) -> Vec<&User> {
        self.repo.list_active()
    }

    pub fn deactivate_user(&mut self, id: u64) -> Result<(), String> {
        if let Some(user) = self.repo.find_by_id(id) {
            let mut user = user.clone();
            user.deactivate();
            Ok(())
        } else {
            Err("user not found".into())
        }
    }
}

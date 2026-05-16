//! User model for the small repo fixture.

use serde::{Deserialize, Serialize};

/// Represents a user in the system.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct User {
    pub id: u64,
    pub username: String,
    pub email: String,
    pub display_name: String,
    pub active: bool,
    pub created_at: String,
    pub updated_at: String,
}

impl User {
    /// Create a new user with default values.
    pub fn new(id: u64, username: String, email: String) -> Self {
        Self {
            id,
            username,
            email,
            display_name: username.clone(),
            active: true,
            created_at: String::from("2024-01-01T00:00:00Z"),
            updated_at: String::from("2024-01-01T00:00:00Z"),
        }
    }

    /// Validate the user fields.
    pub fn validate(&self) -> Result<(), String> {
        if self.username.is_empty() {
            return Err("username cannot be empty".into());
        }
        if !self.email.contains('@') {
            return Err("invalid email format".into());
        }
        Ok(())
    }

    /// Deactivate the user.
    pub fn deactivate(&mut self) {
        self.active = false;
    }
}

/// Repository for user persistence.
pub struct UserRepository {
    users: Vec<User>,
}

impl UserRepository {
    pub fn new() -> Self {
        Self { users: Vec::new() }
    }

    pub fn add(&mut self, user: User) {
        self.users.push(user);
    }

    pub fn find_by_id(&self, id: u64) -> Option<&User> {
        self.users.iter().find(|u| u.id == id)
    }

    pub fn find_by_username(&self, username: &str) -> Option<&User> {
        self.users.iter().find(|u| u.username == username)
    }

    pub fn list_active(&self) -> Vec<&User> {
        self.users.iter().filter(|u| u.active).collect()
    }

    pub fn count(&self) -> usize {
        self.users.len()
    }
}

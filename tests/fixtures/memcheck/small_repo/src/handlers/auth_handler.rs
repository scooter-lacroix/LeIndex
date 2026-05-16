//! Authentication handler.

use crate::models::Session;

/// Authentication handler.
pub struct AuthHandler {
    sessions: Vec<Session>,
}

impl AuthHandler {
    pub fn new() -> Self {
        Self { sessions: Vec::new() }
    }

    pub fn login(&mut self, user_id: u64, token: String) -> Session {
        let session = Session::new(user_id, token);
        self.sessions.push(session.clone());
        session
    }

    pub fn validate_token(&self, token: &str) -> Option<&Session> {
        self.sessions.iter().find(|s| s.token == token && !s.is_expired())
    }

    pub fn logout(&mut self, token: &str) -> bool {
        let before = self.sessions.len();
        self.sessions.retain(|s| s.token != token);
        self.sessions.len() < before
    }
}

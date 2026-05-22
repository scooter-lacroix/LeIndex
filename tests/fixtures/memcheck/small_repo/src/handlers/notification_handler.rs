//! Notification handler.

use crate::models::Notification;

/// Handler for notification operations.
pub struct NotificationHandler {
    notifications: Vec<Notification>,
}

impl NotificationHandler {
    pub fn new() -> Self {
        Self { notifications: Vec::new() }
    }

    pub fn send(&mut self, user_id: u64, title: String, message: String) -> Notification {
        let n = Notification::new(user_id, title, message);
        self.notifications.push(n.clone());
        n
    }

    pub fn get_unread(&self, user_id: u64) -> Vec<&Notification> {
        self.notifications.iter().filter(|n| n.user_id == user_id && !n.read).collect()
    }

    pub fn mark_read(&mut self, id: u64) -> bool {
        if let Some(n) = self.notifications.iter_mut().find(|n| n.id == id) {
            n.mark_read();
            true
        } else {
            false
        }
    }
}

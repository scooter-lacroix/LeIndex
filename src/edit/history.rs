//! Edit history with command pattern.
//!
//! Provides [`EditHistory`] for tracking edit operations with
//! undo, redo, and rollback point support.

use std::collections::HashMap;

use super::command::EditCommand;

/// Edit history with command pattern
#[derive(Debug)]
pub struct EditHistory {
    /// List of recorded edit commands
    pub commands: Vec<EditCommand>,

    /// Current position in the command history
    pub current_index: usize,

    /// Named rollback points mapping to command indices
    pub rollback_points: HashMap<String, usize>,
}

impl EditHistory {
    /// Create a new empty history
    pub fn new() -> Self {
        Self {
            commands: Vec::new(),
            current_index: 0,
            rollback_points: HashMap::new(),
        }
    }

    /// Record a command
    pub fn record_command(&mut self, command: EditCommand) {
        // Remove any commands after current index (redo stack)
        self.commands.truncate(self.current_index);
        self.commands.push(command);
        self.current_index += 1;
    }

    /// Undo last command
    pub fn undo(&mut self) -> Option<&EditCommand> {
        if self.current_index == 0 {
            return None;
        }
        self.current_index -= 1;
        self.commands.get(self.current_index)
    }

    /// Redo last undone command
    pub fn redo(&mut self) -> Option<&EditCommand> {
        if self.current_index >= self.commands.len() {
            return None;
        }
        let command = self.commands.get(self.current_index)?;
        self.current_index += 1;
        Some(command)
    }

    /// Create a rollback point
    pub fn create_rollback_point(&mut self, name: String) {
        self.rollback_points.insert(name, self.current_index);
    }

    /// Rollback to a named point
    pub fn rollback(&mut self, name: &str) -> Option<&EditCommand> {
        let index = self.rollback_points.get(name)?;
        self.current_index = *index;
        self.commands.get(self.current_index)
    }

    /// Get current index
    pub fn current_index(&self) -> usize {
        self.current_index
    }

    /// Get history length
    pub fn len(&self) -> usize {
        self.commands.len()
    }

    /// Check if history is empty
    pub fn is_empty(&self) -> bool {
        self.commands.is_empty()
    }

    /// Get all commands
    pub fn commands(&self) -> &[EditCommand] {
        &self.commands
    }
}

impl Default for EditHistory {
    fn default() -> Self {
        Self::new()
    }
}

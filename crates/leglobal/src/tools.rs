//! Tool detection and execution for project discovery
//!
//! Provides detection and execution of fast filesystem tools:
//! - fd (fastest, preferred)
//! - ripgrep (alternative)
//! - walkdir (fallback)

use std::process::Command;
use thiserror::Error;

/// Types of discovery tools available
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum ToolType {
    /// fd - fast file/directory search (preferred)
    Fd,
    /// ripgrep - grep alternative with good directory search
    Ripgrep,
    /// walkdir - Rust-based fallback (slowest but always available)
    Walkdir,
}

impl ToolType {
    /// Get the command name for this tool
    #[must_use]
    pub const fn command_name(&self) -> &'static str {
        match self {
            Self::Fd => "fd",
            Self::Ripgrep => "rg",
            Self::Walkdir => "walkdir",
        }
    }

    /// Check if this tool is available on the system
    #[must_use]
    pub fn is_available(&self) -> bool {
        match self {
            Self::Walkdir => true, // Always available (it's a Rust crate)
            Self::Fd | Self::Ripgrep => {
                Command::new(self.command_name())
                    .arg("--version")
                    .output()
                    .map(|o| o.status.success())
                    .unwrap_or(false)
            }
        }
    }
}

impl std::fmt::Display for ToolType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Fd => write!(f, "fd"),
            Self::Ripgrep => write!(f, "ripgrep"),
            Self::Walkdir => write!(f, "walkdir"),
        }
    }
}

/// Errors that can occur during tool detection
#[derive(Debug, Error)]
pub enum ToolError {
    /// No discovery tools available
    #[error("No discovery tools available")]
    NoToolsAvailable,

    /// Tool execution failed
    #[error("Tool execution failed for {tool}: {message}")]
    ExecutionFailed {
        /// The name of the tool that failed
        tool: String,
        /// The error message describing the failure
        message: String,
    },
}

/// Detect the best available discovery tool
///
/// Priority: fd (fastest) -> ripgrep -> walkdir (fallback)
///
/// # Returns
///
/// `Option<ToolType>` - The best available tool, or None if walkdir is unavailable
#[must_use]
pub fn detect_tool() -> ToolType {
    // Check in priority order
    let tools = [ToolType::Fd, ToolType::Ripgrep, ToolType::Walkdir];

    for tool in tools {
        if tool.is_available() {
            return tool;
        }
    }

    // Walkdir should always be available as it's a Rust crate
    ToolType::Walkdir
}

/// Get all available discovery tools
///
/// # Returns
///
/// `Vec<ToolType>` - All tools currently available on the system
#[must_use]
pub fn available_tools() -> Vec<ToolType> {
    [ToolType::Fd, ToolType::Ripgrep, ToolType::Walkdir]
        .iter()
        .filter(|t| t.is_available())
        .copied()
        .collect()
}

/// Check for tool aliases (e.g., 'find' might be aliased to 'fd')
///
/// # Arguments
///
/// * `alias` - The alias name to check
///
/// # Returns
///
/// `Option<ToolType>` - The tool this alias maps to, if known
#[must_use]
pub fn resolve_alias(alias: &str) -> Option<ToolType> {
    match alias {
        "fd" | "fdfind" | "find" => Some(ToolType::Fd),
        "rg" | "ripgrep" => Some(ToolType::Ripgrep),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_tooltype_command_names() {
        assert_eq!(ToolType::Fd.command_name(), "fd");
        assert_eq!(ToolType::Ripgrep.command_name(), "rg");
        assert_eq!(ToolType::Walkdir.command_name(), "walkdir");
    }

    #[test]
    fn test_walkdir_always_available() {
        assert!(ToolType::Walkdir.is_available());
    }

    #[test]
    fn test_detect_tool_returns_something() {
        let tool = detect_tool();
        // Should always return at least Walkdir
        assert!(tool == ToolType::Fd || tool == ToolType::Ripgrep || tool == ToolType::Walkdir);
    }

    #[test]
    fn test_available_tools_not_empty() {
        let tools = available_tools();
        assert!(!tools.is_empty());
        assert!(tools.contains(&ToolType::Walkdir));
    }

    #[test]
    fn test_resolve_alias_fd() {
        assert_eq!(resolve_alias("fd"), Some(ToolType::Fd));
        assert_eq!(resolve_alias("fdfind"), Some(ToolType::Fd));
        assert_eq!(resolve_alias("find"), Some(ToolType::Fd));
    }

    #[test]
    fn test_resolve_alias_ripgrep() {
        assert_eq!(resolve_alias("rg"), Some(ToolType::Ripgrep));
        assert_eq!(resolve_alias("ripgrep"), Some(ToolType::Ripgrep));
    }

    #[test]
    fn test_resolve_alias_unknown() {
        assert_eq!(resolve_alias("unknown"), None);
        assert_eq!(resolve_alias(""), None);
    }

    #[test]
    fn test_tooltype_display() {
        assert_eq!(format!("{}", ToolType::Fd), "fd");
        assert_eq!(format!("{}", ToolType::Ripgrep), "ripgrep");
        assert_eq!(format!("{}", ToolType::Walkdir), "walkdir");
    }

    #[test]
    fn test_tooltype_ord() {
        // Tools should be orderable
        assert!(ToolType::Fd < ToolType::Ripgrep);
        assert!(ToolType::Ripgrep < ToolType::Walkdir);
        assert!(ToolType::Fd < ToolType::Walkdir);
    }

    #[test]
    fn test_tooltype_partial_eq() {
        assert_eq!(ToolType::Fd, ToolType::Fd);
        assert_ne!(ToolType::Fd, ToolType::Ripgrep);
    }
}

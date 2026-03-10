use serde::{Deserialize, Serialize};

/// Output formatting mode for phase reports.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum FormatMode {
    /// Shortest output for tight token budgets.
    Ultra,
    /// Balanced detail and compactness.
    Balanced,
    /// Most detailed output.
    Verbose,
}

impl Default for FormatMode {
    fn default() -> Self {
        Self::Balanced
    }
}

impl FormatMode {
    /// Parse mode from CLI/MCP string.
    pub fn parse(value: &str) -> Option<Self> {
        match value.to_ascii_lowercase().as_str() {
            "ultra" => Some(Self::Ultra),
            "balanced" => Some(Self::Balanced),
            "verbose" => Some(Self::Verbose),
            _ => None,
        }
    }

    /// Suggested max chars for each mode.
    pub fn default_max_chars(self) -> usize {
        match self {
            Self::Ultra => 4_000,
            Self::Balanced => 12_000,
            Self::Verbose => 24_000,
        }
    }
}

/// Token-aware formatter utilities.
pub struct TokenFormatter;

impl TokenFormatter {
    /// Truncate a string to a max character count while preserving UTF-8 boundaries.
    pub fn truncate(input: &str, max_chars: usize) -> String {
        if input.chars().count() <= max_chars {
            return input.to_string();
        }

        let mut out = String::new();
        for (i, ch) in input.chars().enumerate() {
            if i >= max_chars {
                break;
            }
            out.push(ch);
        }
        out.push_str("\n\n…[truncated]");
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncate_keeps_short_strings() {
        assert_eq!(TokenFormatter::truncate("abc", 10), "abc");
    }

    #[test]
    fn truncate_limits_output() {
        let value = TokenFormatter::truncate("1234567890", 4);
        assert!(value.starts_with("1234"));
        assert!(value.contains("truncated"));
    }

    #[test]
    fn format_mode_parse_and_default_char_targets() {
        assert_eq!(FormatMode::parse("ultra"), Some(FormatMode::Ultra));
        assert_eq!(FormatMode::parse("balanced"), Some(FormatMode::Balanced));
        assert_eq!(FormatMode::parse("verbose"), Some(FormatMode::Verbose));
        assert_eq!(FormatMode::parse("invalid"), None);

        assert_eq!(FormatMode::Ultra.default_max_chars(), 4_000);
        assert_eq!(FormatMode::Balanced.default_max_chars(), 12_000);
        assert_eq!(FormatMode::Verbose.default_max_chars(), 24_000);
    }
}

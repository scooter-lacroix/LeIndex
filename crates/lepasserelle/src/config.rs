// Project Configuration
//
// *La Configuration* (The Configuration) - Project settings for LeIndex

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::fs;
use std::path::Path;

/// Default configuration file name
pub const DEFAULT_CONFIG_FILE: &str = ".leindex/config.toml";

/// Project configuration
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ProjectConfig {
    /// Language filtering settings
    pub languages: LanguageConfig,

    /// Path exclusion patterns
    pub exclusions: ExclusionConfig,

    /// Token budget settings
    pub tokens: TokenConfig,

    /// Storage configuration
    pub storage: StorageConfig,

    /// Memory management settings
    pub memory: MemoryConfig,
}

impl ProjectConfig {
    /// Load configuration from a directory
    ///
    /// Looks for `.leindex/config.toml` in the project directory.
    /// If not found, returns default configuration.
    ///
    /// # Arguments
    ///
    /// * `project_path` - Path to the project directory
    ///
    /// # Returns
    ///
    /// `Result<ProjectConfig>` - Loaded or default configuration
    pub fn load<P: AsRef<Path>>(project_path: P) -> Result<Self> {
        let config_path = project_path.as_ref().join(DEFAULT_CONFIG_FILE);

        if !config_path.exists() {
            // Return default configuration if file doesn't exist
            return Ok(ProjectConfig::default());
        }

        // Read and parse TOML file
        let content = fs::read_to_string(&config_path)
            .with_context(|| format!("Failed to read config file: {:?}", config_path))?;

        let config: ProjectConfig = toml::from_str(&content)
            .with_context(|| format!("Failed to parse config file: {:?}", config_path))?;

        Ok(config)
    }

    /// Save configuration to a directory
    ///
    /// Creates `.leindex` directory if it doesn't exist.
    ///
    /// # Arguments
    ///
    /// * `project_path` - Path to the project directory
    ///
    /// # Returns
    ///
    /// `Result<()>` - Success or error
    pub fn save<P: AsRef<Path>>(&self, project_path: P) -> Result<()> {
        let config_dir = project_path.as_ref().join(".leindex");
        fs::create_dir_all(&config_dir)
            .with_context(|| format!("Failed to create config directory: {:?}", config_dir))?;

        let config_path = config_dir.join("config.toml");

        let toml_string =
            toml::to_string_pretty(self).context("Failed to serialize configuration")?;

        fs::write(&config_path, toml_string)
            .with_context(|| format!("Failed to write config file: {:?}", config_path))?;

        Ok(())
    }

    /// Get enabled languages as a set of extensions
    pub fn enabled_extensions(&self) -> HashSet<String> {
        self.languages.enabled_extensions()
    }

    /// Check if a path should be excluded
    pub fn should_exclude<P: AsRef<Path>>(&self, path: P) -> bool {
        let path = path.as_ref();

        // Check directory exclusions
        if let Some(parent) = path.parent() {
            if let Some(dir_name) = parent.file_name() {
                let dir = dir_name.to_string_lossy();
                for pattern in &self.exclusions.directory_patterns {
                    if pattern.matches(&dir) {
                        return true;
                    }
                }
            }
        }

        // Check file exclusions
        if let Some(file_name) = path.file_name() {
            let name = file_name.to_string_lossy();
            for pattern in &self.exclusions.file_patterns {
                if pattern.matches(&name) {
                    return true;
                }
            }
        }

        // Check path patterns
        let path_str = path.to_string_lossy();
        for pattern in &self.exclusions.path_patterns {
            if pattern.matches(&path_str) {
                return true;
            }
        }

        false
    }

    /// Get token budget for analysis
    pub fn token_budget(&self) -> usize {
        self.tokens.default_budget
    }

    /// Get maximum tokens for context expansion
    pub fn max_context_tokens(&self) -> usize {
        self.tokens.max_context
    }
}

/// Language filtering configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LanguageConfig {
    /// Enable all supported languages
    pub enable_all: bool,

    /// Explicitly enabled languages (by extension or name)
    pub enabled: Vec<String>,

    /// Explicitly disabled languages
    pub disabled: Vec<String>,
}

impl Default for LanguageConfig {
    fn default() -> Self {
        Self {
            enable_all: true,
            enabled: Vec::new(),
            disabled: vec!["vim".to_string()], // Disable vim scripts by default
        }
    }
}

impl LanguageConfig {
    /// Get enabled language extensions
    pub fn enabled_extensions(&self) -> HashSet<String> {
        let mut extensions = HashSet::new();

        // All supported extensions
        let all_extensions = [
            "rs", "py", "js", "ts", "tsx", "jsx", // Main languages
            "go", "java", "cpp", "c", "h", "hpp", // Systems languages
            "rb", "php", "lua", "scala", // Scripting languages
        ];

        if self.enable_all {
            for ext in &all_extensions {
                if !self.disabled.contains(&ext.to_string()) {
                    extensions.insert(ext.to_string());
                }
            }
        } else {
            for lang in &self.enabled {
                // Check if it's an extension
                if all_extensions.contains(&lang.as_str()) {
                    extensions.insert(lang.clone());
                } else {
                    // It's a language name - map to extensions
                    let exts = Self::language_to_extensions(lang);
                    for ext in exts {
                        if !self.disabled.contains(&ext.to_string()) {
                            extensions.insert(ext.to_string());
                        }
                    }
                }
            }
        }

        extensions
    }

    /// Map language name to file extensions
    fn language_to_extensions(lang: &str) -> Vec<&'static str> {
        match lang.to_lowercase().as_str() {
            "rust" => vec!["rs"],
            "python" => vec!["py"],
            "javascript" => vec!["js", "jsx"],
            "typescript" => vec!["ts", "tsx"],
            "go" => vec!["go"],
            "java" => vec!["java"],
            "cpp" | "c++" => vec!["cpp", "cc", "cxx", "c", "h", "hpp"],
            "c" => vec!["c", "h"],
            "ruby" => vec!["rb"],
            "php" => vec!["php"],
            "lua" => vec!["lua"],
            "scala" => vec!["scala"],
            _ => vec![],
        }
    }

    /// Check if a language (by extension) is enabled
    pub fn is_extension_enabled(&self, ext: &str) -> bool {
        self.enabled_extensions().contains(ext)
    }
}

/// Path exclusion configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExclusionConfig {
    /// Directory name patterns to exclude
    pub directory_patterns: Vec<StringPattern>,

    /// File name patterns to exclude
    pub file_patterns: Vec<StringPattern>,

    /// Full path patterns to exclude
    pub path_patterns: Vec<StringPattern>,
}

impl Default for ExclusionConfig {
    fn default() -> Self {
        Self {
            directory_patterns: vec![
                "target".into(),
                "node_modules".into(),
                "vendor".into(),
                ".git".into(),
                "dist".into(),
                "build".into(),
                "out".into(),
                ".venv".into(),
                "venv".into(),
                "env".into(),
                "__pycache__".into(),
            ],
            file_patterns: vec![
                "*.min.js".into(),
                "*.min.css".into(),
                "*.pb.go".into(), // Generated protobuf files
                "*.generated.rs".into(),
            ],
            path_patterns: vec!["*/target/*".into(), "*/node_modules/*".into()],
        }
    }
}

impl ExclusionConfig {
    /// Check if a path should be excluded based on configured patterns
    pub fn should_exclude(&self, path: &str) -> bool {
        // Check directory patterns
        for segment in path.split('/') {
            for pattern in &self.directory_patterns {
                if pattern.matches(segment) {
                    return true;
                }
            }
        }

        // Check file patterns (only the filename part)
        if let Some(filename) = path.rsplit('/').next() {
            for pattern in &self.file_patterns {
                if pattern.matches(filename) {
                    return true;
                }
            }
        }

        // Check full path patterns
        for pattern in &self.path_patterns {
            if pattern.matches(path) {
                return true;
            }
        }

        false
    }
}

/// String pattern for matching (supports wildcards)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StringPattern {
    /// The pattern string, which may contain wildcards
    pub pattern: String,
}

impl From<&str> for StringPattern {
    fn from(s: &str) -> Self {
        Self {
            pattern: s.to_string(),
        }
    }
}

impl From<String> for StringPattern {
    fn from(s: String) -> Self {
        Self { pattern: s }
    }
}

impl StringPattern {
    /// Check if this pattern matches a string
    ///
    /// Supports `*` wildcards.
    pub fn matches(&self, text: &str) -> bool {
        if self.pattern == "*" {
            return true;
        }

        if !self.pattern.contains('*') {
            return self.pattern == text;
        }

        // Simple wildcard matching
        let parts: Vec<&str> = self.pattern.split('*').collect();
        if parts.len() == 2 {
            let prefix = parts[0];
            let suffix = parts[1];
            text.starts_with(prefix) && text.ends_with(suffix)
        } else {
            // Multiple wildcards - check each part in sequence
            let mut idx = 0;
            for (i, part) in parts.iter().enumerate() {
                if i == parts.len() - 1 && !part.is_empty() {
                    // Last part - check suffix
                    if !text.ends_with(part) {
                        return false;
                    }
                } else if !part.is_empty() {
                    // Middle or first part - find it
                    if let Some(pos) = text[idx..].find(part) {
                        idx = pos + part.len();
                    } else {
                        return false;
                    }
                }
            }
            true
        }
    }
}

/// Token budget configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TokenConfig {
    /// Default token budget for analysis
    pub default_budget: usize,

    /// Maximum tokens for context expansion
    pub max_context: usize,

    /// Minimum tokens for results
    pub min_results: usize,

    /// Maximum number of results to return
    pub max_results: usize,
}

impl Default for TokenConfig {
    fn default() -> Self {
        Self {
            default_budget: 2000,
            max_context: 5000,
            min_results: 5,
            max_results: 20,
        }
    }
}

/// Storage configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StorageConfig {
    /// Storage backend to use
    pub backend: StorageBackend,

    /// Database path relative to project root
    pub db_path: Option<String>,

    /// Whether to enable WAL mode
    pub wal_enabled: bool,

    /// Cache size in pages
    pub cache_size_pages: Option<usize>,

    /// Connection timeout in seconds
    pub connection_timeout_secs: Option<u64>,
}

impl Default for StorageConfig {
    fn default() -> Self {
        Self {
            backend: StorageBackend::SQLite,
            db_path: None, // Use default
            wal_enabled: true,
            cache_size_pages: Some(10000),
            connection_timeout_secs: Some(30),
        }
    }
}

/// Storage backend type
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum StorageBackend {
    /// SQLite (default, embedded)
    SQLite,

    /// Turso (remote libsql)
    Turso {
        /// Turso database URL (e.g., libsql://...")
        database_url: String,
        /// Turso authentication token
        auth_token: Option<String>,
    },
}

/// Memory management configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryConfig {
    /// RSS threshold for spilling (0.0-1.0)
    pub spill_threshold: f64,

    /// Whether to enable automatic spilling
    pub auto_spill: bool,

    /// Maximum memory to use in MB (0 = unlimited)
    pub max_memory_mb: usize,
}

impl Default for MemoryConfig {
    fn default() -> Self {
        Self {
            spill_threshold: 0.9,
            auto_spill: true,
            max_memory_mb: 8192, // 8 GB default
        }
    }
}

/// Configuration error
#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    /// Input/Output error during configuration file handling
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    /// Failed to serialize configuration to TOML/JSON
    #[error("Serialization error: {0}")]
    Serialization(String),

    /// Failed to parse configuration from file
    #[error("Parse error: {0}")]
    Parse(String),

    /// The configuration contains invalid values or settings
    #[error("Invalid configuration: {0}")]
    Invalid(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_config() {
        let config = ProjectConfig::default();
        assert!(config.languages.enable_all);
        assert_eq!(config.tokens.default_budget, 2000);
    }

    #[test]
    fn test_language_extensions() {
        let config = LanguageConfig::default();
        let exts = config.enabled_extensions();
        assert!(exts.contains("rs"));
        assert!(exts.contains("py"));
        assert!(!exts.contains("vim"));
    }

    #[test]
    fn test_exclusion_patterns() {
        let config = ExclusionConfig::default();
        assert!(config.should_exclude("target/main.rs"));
        assert!(config.should_exclude("node_modules/package/index.js"));
        assert!(!config.should_exclude("src/main.rs"));
    }

    #[test]
    fn test_string_pattern() {
        let pattern = StringPattern::from("*.min.js");
        assert!(pattern.matches("file.min.js"));
        assert!(pattern.matches("path/to/file.min.js"));
        assert!(!pattern.matches("file.js"));

        let wildcard = StringPattern::from("*");
        assert!(wildcard.matches("anything"));
    }

    #[test]
    fn test_config_serialization() {
        let config = ProjectConfig::default();
        let toml_string = toml::to_string(&config).unwrap();
        println!("{}", toml_string);

        let deserialized: ProjectConfig = toml::from_str(&toml_string).unwrap();
        assert_eq!(deserialized.tokens.default_budget, 2000);
    }
}

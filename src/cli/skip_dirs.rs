//! Shared directory exclusion list for all filesystem traversals.
//!
//! Used by source file scanning, text search, and dependency manifest discovery.
//! One place to update when adding new exclusion patterns.

/// Directories to skip during all filesystem traversals.
///
/// Covers: version control, build outputs, package managers, Python environments,
/// IDE metadata, framework-generated directories, and archived code.
///
/// **Must remain sorted alphabetically** if used with `binary_search`.
pub const SKIP_DIRS: &[&str] = &[
    // Archived / deprecated code
    // Matches any directory named "archive" or ".archive" at any depth
    // in the tree (e.g., archive/, .archive/, docs/archive/, maestro/archive/).
    ".archive",
    "archive",
    // Build outputs
    "build",
    "coverage",
    "dist",
    "out",
    "target",
    // IDE / editor metadata
    ".idea",
    ".vscode",
    // Index data
    ".leindex",
    // Package managers / dependencies
    "bower_components",
    "node_modules",
    "vendor",
    // Python caches
    "__pycache__",
    ".mypy_cache",
    ".pytest_cache",
    ".ruff_cache",
    ".tox",
    // Python virtual environments
    ".venv",
    "env",
    "venv",
    // Web frameworks
    ".next",
    ".nuxt",
    // Version control
    ".git",
    ".hg",
    ".svn",
];

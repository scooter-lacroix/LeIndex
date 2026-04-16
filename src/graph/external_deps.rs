//! External dependency resolution via lock files.
//!
//! Scans the project root for language-specific lock/manifest files and builds
//! a lookup table of known external packages.  This lets us annotate synthetic
//! "external" module nodes in the PDG with package name, version, and source
//! instead of leaving them as opaque unresolved references.
//!
//! Supported ecosystems:
//!
//! | Language   | Lock/manifest file            | Fields extracted          |
//! |------------|-------------------------------|---------------------------|
//! | Rust       | `Cargo.lock`                  | name, version             |
//! | Rust       | `Cargo.toml`                  | name, version constraint  |
//! | JavaScript | `package-lock.json`           | name, version             |
//! | JavaScript | `package.json`                | name, version constraint  |
//! | JavaScript | `yarn.lock`                   | name, version             |
//! | JavaScript | `pnpm-lock.yaml`              | name, version             |
//! | Python     | `requirements.txt`            | name, version constraint  |
//! | Python     | `Pipfile.lock`                | name, version             |
//! | Python     | `pyproject.toml`              | name, version constraint  |
//! | Python     | `poetry.lock`                 | name, version             |
//! | Go         | `go.sum`                      | module path, version      |
//! | Ruby       | `Gemfile.lock`                | name, version             |
//! | PHP        | `composer.lock`               | name, version             |

use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

/// A resolved external dependency with package metadata.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ExternalDependency {
    /// The canonical package name (e.g., `serde`, `react`, `numpy`).
    pub name: String,
    /// The locked version (e.g., `1.0.210`, `^18.2.0`).
    pub version: String,
    /// The ecosystem this dependency belongs to.
    pub ecosystem: Ecosystem,
}

/// Language ecosystem / package manager.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub enum Ecosystem {
    /// Rust / crates.io
    Cargo,
    /// Node.js / npm / yarn
    Npm,
    /// Python / PyPI
    Python,
    /// Go modules
    GoModules,
    /// Ruby / RubyGems
    RubyGems,
    /// PHP / Packagist
    Composer,
}

impl std::fmt::Display for Ecosystem {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Ecosystem::Cargo => write!(f, "cargo"),
            Ecosystem::Npm => write!(f, "npm"),
            Ecosystem::Python => write!(f, "python"),
            Ecosystem::GoModules => write!(f, "go"),
            Ecosystem::RubyGems => write!(f, "rubygems"),
            Ecosystem::Composer => write!(f, "composer"),
        }
    }
}

/// Registry of known external dependencies keyed by package name.
///
/// The registry supports lookup by both the canonical package name and by
/// common import path prefixes so that we can match PDG "external" module
/// nodes like `third.party.lib` to a known package.
#[derive(Debug, Clone, Default)]
pub struct ExternalDependencyRegistry {
    /// Canonical name → dependency metadata.
    by_name: HashMap<String, ExternalDependency>,
    /// Normalised import prefix → canonical name.
    /// E.g., `serde_json` → `serde_json`, `@types/react` → `@types/react`,
    /// `github.com/gorilla/mux` → `github.com/gorilla/mux`.
    prefix_map: HashMap<String, String>,
}

impl ExternalDependencyRegistry {
    /// Create an empty registry.
    pub fn new() -> Self {
        Self::default()
    }

    /// Scan the project root for lock/manifest files and build the registry.
    pub fn from_project(root: &Path) -> Self {
        Self::from_manifest_paths(root, &discover_dependency_manifests(root, None))
    }
}

/// Lockfiles (priority 1) override manifest-derived ranges (priority 0).
fn source_priority(file_name: &str) -> u8 {
    match file_name {
        "Cargo.toml" | "package.json" | "pyproject.toml" | "go.mod" => 0,
        _ => 1, // lockfiles and fully resolved metadata
    }
}

impl ExternalDependencyRegistry {
    /// Build the registry from an already-discovered manifest list.
    pub fn from_manifest_paths(root: &Path, manifest_paths: &[PathBuf]) -> Self {
        let mut registry = Self::new();
        // Sort so lockfiles (priority 1) are parsed AFTER manifests (priority 0),
        // ensuring lockfile versions overwrite the looser manifest-derived ranges.
        let mut sorted_paths = manifest_paths.to_vec();
        sorted_paths.sort_by(|a, b| {
            let a_name = a.file_name().and_then(|n| n.to_str()).unwrap_or("");
            let b_name = b.file_name().and_then(|n| n.to_str()).unwrap_or("");
            source_priority(a_name).cmp(&source_priority(b_name))
        });
        for manifest_path in &sorted_paths {
            let path = if manifest_path.is_absolute() {
                manifest_path.clone()
            } else {
                root.join(manifest_path)
            };
            let Some(file_name) = path.file_name().and_then(|name| name.to_str()) else {
                continue;
            };
            let Ok(content) = std::fs::read_to_string(&path) else {
                continue;
            };

            match file_name {
                "Cargo.lock" => registry.parse_cargo_lock(&content),
                "Cargo.toml" => registry.parse_cargo_toml(&content),
                "package-lock.json" | "npm-shrinkwrap.json" => registry.parse_package_lock_json(&content),
                "package.json" => registry.parse_package_json(&content),
                "yarn.lock" => registry.parse_yarn_lock(&content),
                "pnpm-lock.yaml" => registry.parse_pnpm_lock(&content),
                "requirements.txt" => registry.parse_requirements_txt(&content),
                "Pipfile.lock" => registry.parse_pipfile_lock(&content),
                "pyproject.toml" => registry.parse_pyproject_toml(&content),
                "poetry.lock" => registry.parse_poetry_lock(&content),
                "go.mod" => registry.parse_go_mod(&content),
                "go.sum" => registry.parse_go_sum(&content),
                "Gemfile.lock" => registry.parse_gemfile_lock(&content),
                "composer.lock" => registry.parse_composer_lock(&content),
                _ => {}
            }
        }

        registry
    }

    /// Number of known external dependencies.
    pub fn len(&self) -> usize {
        self.by_name.len()
    }

    /// Whether the registry is empty.
    pub fn is_empty(&self) -> bool {
        self.by_name.is_empty()
    }

    /// List all known dependencies.
    pub fn all_dependencies(&self) -> Vec<&ExternalDependency> {
        self.by_name.values().collect()
    }

    /// Resolve an import path to a known external dependency.
    ///
    /// Attempts:
    /// 1. Exact match by normalised name.
    /// 2. Prefix match (e.g., `serde.de` → `serde`).
    /// 3. Underscore/hyphen normalisation (e.g., `serde-json` ↔ `serde_json`).
    pub fn resolve(&self, import_path: &str) -> Option<&ExternalDependency> {
        let normalised = normalise_import(import_path);

        // 1. Exact match
        if let Some(canonical) = self.prefix_map.get(&normalised) {
            return self.by_name.get(canonical);
        }

        // 2. Prefix match: try progressively shorter prefixes
        let parts: Vec<&str> = normalised.split('.').collect();
        for i in (1..parts.len()).rev() {
            let prefix = parts[..i].join(".");
            if let Some(canonical) = self.prefix_map.get(&prefix) {
                return self.by_name.get(canonical);
            }
        }

        // 3. Hyphen/underscore normalisation
        let alt = normalised.replace('-', "_");
        if alt != normalised {
            if let Some(canonical) = self.prefix_map.get(&alt) {
                return self.by_name.get(canonical);
            }
        }
        let alt2 = normalised.replace('_', "-");
        if alt2 != normalised {
            if let Some(canonical) = self.prefix_map.get(&alt2) {
                return self.by_name.get(canonical);
            }
        }

        None
    }

    /// Insert a dependency into the registry.
    fn insert(&mut self, dep: ExternalDependency) {
        let normalised = normalise_import(&dep.name);
        self.prefix_map.insert(normalised, dep.name.clone());
        if dep.ecosystem == Ecosystem::Python {
            for alias in python_import_aliases(&dep.name) {
                self.prefix_map.insert(alias, dep.name.clone());
            }
        }
        self.by_name.insert(dep.name.clone(), dep);
    }

    // ========================================================================
    // Parsers
    // ========================================================================

    /// Parse Cargo.lock (TOML-like format, but we use simple line parsing).
    ///
    /// ```text
    /// [[package]]
    /// name = "serde"
    /// version = "1.0.210"
    /// ```
    fn parse_cargo_lock(&mut self, content: &str) {
        let mut current_name: Option<String> = None;
        let mut current_version: Option<String> = None;

        for line in content.lines() {
            let trimmed = line.trim();

            if trimmed == "[[package]]" {
                // Flush previous
                if let (Some(name), Some(version)) = (current_name.take(), current_version.take()) {
                    self.insert(ExternalDependency {
                        name,
                        version,
                        ecosystem: Ecosystem::Cargo,
                    });
                }
                current_name = None;
                current_version = None;
                continue;
            }

            if let Some(rest) = trimmed.strip_prefix("name = ") {
                current_name = Some(unquote(rest));
            } else if let Some(rest) = trimmed.strip_prefix("version = ") {
                current_version = Some(unquote(rest));
            }
        }

        // Flush last
        if let (Some(name), Some(version)) = (current_name, current_version) {
            self.insert(ExternalDependency {
                name,
                version,
                ecosystem: Ecosystem::Cargo,
            });
        }
    }

    /// Parse Cargo.toml dependency tables as a fallback when Cargo.lock is absent.
    fn parse_cargo_toml(&mut self, content: &str) {
        let mut in_dependencies = false;

        for raw in content.lines() {
            let line = raw.split('#').next().unwrap_or("").trim();
            if line.is_empty() {
                continue;
            }
            if line.starts_with('[') && line.ends_with(']') {
                let section = line.trim_matches(&['[', ']'][..]).to_lowercase();
                in_dependencies = section.contains("dependencies");
                continue;
            }
            if !in_dependencies {
                continue;
            }

            let Some((name_raw, value_raw)) = line.split_once('=') else {
                continue;
            };
            let name = name_raw.trim().trim_matches('"').trim_matches('\'');
            if name.is_empty() {
                continue;
            }
            let value = value_raw.trim();

            let version = if value.starts_with('"') || value.starts_with('\'') {
                unquote(value)
            } else if let Some(pos) = value.find("version") {
                let rest = &value[pos + "version".len()..];
                if let Some((_, rhs)) = rest.split_once('=') {
                    let candidate = rhs
                        .split(',')
                        .next()
                        .unwrap_or(rhs)
                        .trim()
                        .trim_end_matches('}');
                    let parsed = unquote(candidate);
                    if parsed.is_empty() {
                        "*".to_string()
                    } else {
                        parsed
                    }
                } else {
                    "*".to_string()
                }
            } else {
                "*".to_string()
            };

            self.insert(ExternalDependency {
                name: name.to_string(),
                version,
                ecosystem: Ecosystem::Cargo,
            });
        }
    }

    /// Parse package-lock.json (npm v2/v3 format).
    fn parse_package_lock_json(&mut self, content: &str) {
        let Ok(parsed) = serde_json::from_str::<serde_json::Value>(content) else {
            return;
        };

        // v2/v3 format: packages."node_modules/<name>"
        if let Some(packages) = parsed.get("packages").and_then(|v| v.as_object()) {
            for (key, val) in packages {
                let name = key.strip_prefix("node_modules/").unwrap_or(key).to_string();
                if name.is_empty() || name == "." {
                    continue;
                }
                let version = val
                    .get("version")
                    .and_then(|v| v.as_str())
                    .unwrap_or("*")
                    .to_string();
                self.insert(ExternalDependency {
                    name,
                    version,
                    ecosystem: Ecosystem::Npm,
                });
            }
            return;
        }

        // v1 format: dependencies.<name>
        if let Some(deps) = parsed.get("dependencies").and_then(|v| v.as_object()) {
            for (name, val) in deps {
                let version = val
                    .get("version")
                    .and_then(|v| v.as_str())
                    .unwrap_or("*")
                    .to_string();
                self.insert(ExternalDependency {
                    name: name.clone(),
                    version,
                    ecosystem: Ecosystem::Npm,
                });
            }
        }
    }

    /// Parse package.json dependency sections as a fallback manifest source.
    fn parse_package_json(&mut self, content: &str) {
        let Ok(parsed) = serde_json::from_str::<serde_json::Value>(content) else {
            return;
        };

        for section in &[
            "dependencies",
            "devDependencies",
            "peerDependencies",
            "optionalDependencies",
        ] {
            if let Some(deps) = parsed.get(section).and_then(|v| v.as_object()) {
                for (name, version) in deps {
                    let version = version.as_str().unwrap_or("*").to_string();
                    self.insert(ExternalDependency {
                        name: name.clone(),
                        version,
                        ecosystem: Ecosystem::Npm,
                    });
                }
            }
        }
    }

    /// Parse yarn.lock (simple line-based format).
    ///
    /// ```text
    /// "@babel/core@^7.0.0":
    ///   version "7.24.0"
    /// ```
    fn parse_yarn_lock(&mut self, content: &str) {
        let mut current_name: Option<String> = None;

        for line in content.lines() {
            let trimmed = line.trim();

            // Entry line: starts with a package spec (may be quoted)
            if !trimmed.starts_with(' ') && !trimmed.starts_with('#') && trimmed.ends_with(':') {
                let spec = trimmed.trim_end_matches(':');
                // Handle quoted entries like `"@babel/core@^7.0.0":`
                let spec = spec.trim_matches('"');
                // Extract package name (everything before the last @version)
                let name = if spec.starts_with('@') {
                    // Scoped package: @scope/name@version
                    if let Some(at_pos) = spec[1..].rfind('@') {
                        spec[..at_pos + 1].to_string()
                    } else {
                        spec.to_string()
                    }
                } else if let Some(at_pos) = spec.rfind('@') {
                    spec[..at_pos].to_string()
                } else {
                    spec.to_string()
                };
                // Handle comma-separated specs (multiple version ranges)
                let name = name.split(',').next().unwrap_or(&name).trim().to_string();
                current_name = Some(name);
                continue;
            }

            if let Some(rest) = trimmed.strip_prefix("version ") {
                if let Some(ref name) = current_name {
                    let version = unquote(rest);
                    self.insert(ExternalDependency {
                        name: name.clone(),
                        version,
                        ecosystem: Ecosystem::Npm,
                    });
                    current_name = None;
                }
            }
        }
    }

    /// Parse pnpm-lock.yaml package entries (line-based parser).
    fn parse_pnpm_lock(&mut self, content: &str) {
        for line in content.lines() {
            let trimmed = line.trim();
            if !trimmed.starts_with('/') || !trimmed.ends_with(':') {
                continue;
            }
            let spec = trimmed.trim_end_matches(':').trim_start_matches('/');
            if spec.is_empty() {
                continue;
            }

            let (name, mut version) = if spec.starts_with('@') {
                if let Some(pos) = spec[1..].rfind('@').map(|p| p + 1) {
                    (spec[..pos].to_string(), spec[pos + 1..].to_string())
                } else {
                    (spec.to_string(), "*".to_string())
                }
            } else if let Some(pos) = spec.rfind('@') {
                (spec[..pos].to_string(), spec[pos + 1..].to_string())
            } else {
                (spec.to_string(), "*".to_string())
            };

            if let Some(paren) = version.find('(') {
                version = version[..paren].to_string();
            }
            if !name.is_empty() {
                self.insert(ExternalDependency {
                    name,
                    version,
                    ecosystem: Ecosystem::Npm,
                });
            }
        }
    }

    /// Parse requirements.txt (pip format).
    ///
    /// ```text
    /// numpy==1.26.0
    /// requests>=2.31.0
    /// flask
    /// ```
    fn parse_requirements_txt(&mut self, content: &str) {
        for line in content.lines() {
            let trimmed = line.trim();
            if trimmed.is_empty() || trimmed.starts_with('#') || trimmed.starts_with('-') {
                continue;
            }

            // Split on version specifiers: ==, >=, <=, ~=, !=, >, <
            let (name, version) = if let Some(pos) = trimmed.find(|c: char| "=<>~!".contains(c)) {
                let n = trimmed[..pos].trim();
                let v = trimmed[pos..].trim();
                (n.to_string(), v.to_string())
            } else {
                (trimmed.to_string(), "*".to_string())
            };

            // Skip extras like package[extra]
            let name = name.split('[').next().unwrap_or(&name).trim().to_string();
            if name.is_empty() {
                continue;
            }

            self.insert(ExternalDependency {
                name,
                version,
                ecosystem: Ecosystem::Python,
            });
        }
    }

    /// Parse Pipfile.lock (JSON format).
    fn parse_pipfile_lock(&mut self, content: &str) {
        let Ok(parsed) = serde_json::from_str::<serde_json::Value>(content) else {
            return;
        };

        for section in &["default", "develop"] {
            if let Some(deps) = parsed.get(section).and_then(|v| v.as_object()) {
                for (name, val) in deps {
                    let version = val
                        .get("version")
                        .and_then(|v| v.as_str())
                        .unwrap_or("*")
                        .to_string();
                    self.insert(ExternalDependency {
                        name: name.clone(),
                        version,
                        ecosystem: Ecosystem::Python,
                    });
                }
            }
        }
    }

    /// Parse pyproject.toml dependency declarations (PEP 621 + Poetry style).
    fn parse_pyproject_toml(&mut self, content: &str) {
        let mut section = String::new();
        let mut collecting_project_deps = false;
        let mut project_deps_buf = String::new();

        for raw in content.lines() {
            let line = raw.split('#').next().unwrap_or("").trim();
            if line.is_empty() {
                continue;
            }

            if line.starts_with('[') && line.ends_with(']') {
                section = line.trim_matches(&['[', ']'][..]).to_lowercase();
                collecting_project_deps = false;
                project_deps_buf.clear();
                continue;
            }

            if section == "project" {
                if let Some(rhs) = line.strip_prefix("dependencies = ") {
                    if rhs.contains(']') {
                        for dep in parse_python_dependency_list(rhs) {
                            self.insert(dep);
                        }
                    } else {
                        collecting_project_deps = true;
                        project_deps_buf.push_str(rhs);
                        project_deps_buf.push('\n');
                    }
                    continue;
                }
                if collecting_project_deps {
                    project_deps_buf.push_str(line);
                    project_deps_buf.push('\n');
                    if line.contains(']') {
                        collecting_project_deps = false;
                        for dep in parse_python_dependency_list(&project_deps_buf) {
                            self.insert(dep);
                        }
                        project_deps_buf.clear();
                    }
                }
                continue;
            }

            if section.starts_with("tool.poetry.dependencies")
                || section.starts_with("tool.poetry.group.") && section.ends_with(".dependencies")
            {
                let Some((name_raw, value_raw)) = line.split_once('=') else {
                    continue;
                };
                let name = name_raw.trim().trim_matches('"').trim_matches('\'');
                if name.is_empty() || name.eq_ignore_ascii_case("python") {
                    continue;
                }
                let version = value_raw
                    .trim()
                    .split(',')
                    .next()
                    .map(unquote)
                    .filter(|v| !v.is_empty())
                    .unwrap_or_else(|| "*".to_string());
                self.insert(ExternalDependency {
                    name: name.to_string(),
                    version,
                    ecosystem: Ecosystem::Python,
                });
            }
        }
    }

    /// Parse poetry.lock package blocks.
    fn parse_poetry_lock(&mut self, content: &str) {
        let mut current_name: Option<String> = None;
        let mut current_version: Option<String> = None;

        for line in content.lines() {
            let trimmed = line.trim();
            if trimmed == "[[package]]" {
                if let (Some(name), Some(version)) = (current_name.take(), current_version.take()) {
                    self.insert(ExternalDependency {
                        name,
                        version,
                        ecosystem: Ecosystem::Python,
                    });
                }
                current_name = None;
                current_version = None;
                continue;
            }

            if let Some(rest) = trimmed.strip_prefix("name = ") {
                current_name = Some(unquote(rest));
            } else if let Some(rest) = trimmed.strip_prefix("version = ") {
                current_version = Some(unquote(rest));
            }
        }

        if let (Some(name), Some(version)) = (current_name, current_version) {
            self.insert(ExternalDependency {
                name,
                version,
                ecosystem: Ecosystem::Python,
            });
        }
    }

    /// Parse go.sum.
    ///
    /// ```text
    /// github.com/gorilla/mux v1.8.1 h1:abc...
    /// github.com/gorilla/mux v1.8.1/go.mod h1:xyz...
    /// ```
    fn parse_go_sum(&mut self, content: &str) {
        let mut seen = std::collections::HashSet::new();
        for line in content.lines() {
            let parts: Vec<&str> = line.split_whitespace().collect();
            if parts.len() < 2 {
                continue;
            }
            let module = parts[0];
            let version = parts[1].trim_end_matches("/go.mod");
            let key = format!("{}@{}", module, version);
            if seen.contains(&key) {
                continue;
            }
            seen.insert(key);
            self.insert(ExternalDependency {
                name: module.to_string(),
                version: version.to_string(),
                ecosystem: Ecosystem::GoModules,
            });
        }
    }

    /// Parse go.mod requirements.
    ///
    /// ```text
    /// require github.com/gorilla/mux v1.8.1
    /// require (
    ///   golang.org/x/net v0.24.0
    /// )
    /// ```
    fn parse_go_mod(&mut self, content: &str) {
        let mut in_require_block = false;
        for raw in content.lines() {
            let line = raw.split("//").next().unwrap_or("").trim();
            if line.is_empty() {
                continue;
            }

            if line.starts_with("require (") {
                in_require_block = true;
                continue;
            }
            if in_require_block && line == ")" {
                in_require_block = false;
                continue;
            }

            if in_require_block {
                if let Some((name, version)) = parse_go_requirement(line) {
                    self.insert(ExternalDependency {
                        name,
                        version,
                        ecosystem: Ecosystem::GoModules,
                    });
                }
                continue;
            }

            if let Some(rest) = line.strip_prefix("require ") {
                if let Some((name, version)) = parse_go_requirement(rest.trim()) {
                    self.insert(ExternalDependency {
                        name,
                        version,
                        ecosystem: Ecosystem::GoModules,
                    });
                }
            }
        }
    }

    /// Parse Gemfile.lock.
    ///
    /// ```text
    /// GEM
    ///   specs:
    ///     rails (7.1.0)
    ///     activesupport (7.1.0)
    /// ```
    fn parse_gemfile_lock(&mut self, content: &str) {
        let mut in_specs = false;
        for line in content.lines() {
            let trimmed = line.trim();

            if trimmed == "specs:" {
                in_specs = true;
                continue;
            }

            // End of specs block
            if in_specs && !line.starts_with(' ') && !line.starts_with('\t') && !trimmed.is_empty()
            {
                in_specs = false;
                continue;
            }

            if in_specs {
                // Top-level gem: "    gemname (version)"
                // Sub-dependency: "      gemname (version)" — deeper indent
                let indent = line.len() - line.trim_start().len();
                if indent <= 6 && trimmed.contains('(') {
                    if let Some((name, rest)) = trimmed.split_once('(') {
                        let version = rest.trim_end_matches(')').trim().to_string();
                        let name = name.trim().to_string();
                        if !name.is_empty() {
                            self.insert(ExternalDependency {
                                name,
                                version,
                                ecosystem: Ecosystem::RubyGems,
                            });
                        }
                    }
                }
            }
        }
    }

    /// Parse composer.lock (PHP / JSON format).
    fn parse_composer_lock(&mut self, content: &str) {
        let Ok(parsed) = serde_json::from_str::<serde_json::Value>(content) else {
            return;
        };

        for section in &["packages", "packages-dev"] {
            if let Some(pkgs) = parsed.get(section).and_then(|v| v.as_array()) {
                for pkg in pkgs {
                    let name = pkg.get("name").and_then(|v| v.as_str()).unwrap_or("");
                    let version = pkg.get("version").and_then(|v| v.as_str()).unwrap_or("*");
                    if !name.is_empty() {
                        self.insert(ExternalDependency {
                            name: name.to_string(),
                            version: version.to_string(),
                            ecosystem: Ecosystem::Composer,
                        });
                    }
                }
            }
        }
    }
}

/// Normalise an import path for comparison.
///
/// Converts path separators, strips common prefixes, and lowercases.
fn normalise_import(raw: &str) -> String {
    raw.replace("::", ".")
        .replace('/', ".")
        .replace('\\', ".")
        .replace(':', ".")
        .replace("..", ".")
        .trim_matches('.')
        .to_lowercase()
}

/// Remove surrounding quotes from a TOML/YAML/JSON-like value.
fn unquote(s: &str) -> String {
    s.trim().trim_matches('"').trim_matches('\'').to_string()
}

fn parse_python_dependency_list(raw: &str) -> Vec<ExternalDependency> {
    let mut deps = Vec::new();
    let mut item = String::new();
    let mut in_quotes = false;

    for ch in raw.chars() {
        match ch {
            '"' | '\'' => {
                in_quotes = !in_quotes;
            }
            ',' if !in_quotes => {
                let parsed = item.trim();
                if !parsed.is_empty() {
                    if let Some(dep) = parse_python_requirement_spec(parsed) {
                        deps.push(dep);
                    }
                }
                item.clear();
            }
            '[' | ']' if !in_quotes => {}
            _ => item.push(ch),
        }
    }

    let parsed = item.trim();
    if !parsed.is_empty() {
        if let Some(dep) = parse_python_requirement_spec(parsed) {
            deps.push(dep);
        }
    }

    deps
}

fn parse_python_requirement_spec(spec: &str) -> Option<ExternalDependency> {
    let spec = spec
        .trim()
        .trim_matches('"')
        .trim_matches('\'')
        .split(';')
        .next()
        .unwrap_or(spec)
        .trim();
    if spec.is_empty() {
        return None;
    }

    let first_constraint = spec
        .find(|c: char| c.is_whitespace() || "=<>!~".contains(c))
        .unwrap_or(spec.len());
    let name = spec[..first_constraint]
        .trim()
        .split('[')
        .next()
        .unwrap_or("")
        .trim();
    if name.is_empty() {
        return None;
    }

    let version = if first_constraint < spec.len() {
        spec[first_constraint..].trim().to_string()
    } else {
        "*".to_string()
    };

    Some(ExternalDependency {
        name: name.to_string(),
        version,
        ecosystem: Ecosystem::Python,
    })
}

fn parse_go_requirement(raw: &str) -> Option<(String, String)> {
    let parts: Vec<&str> = raw.split_whitespace().collect();
    if parts.len() < 2 {
        return None;
    }
    let name = parts[0].trim();
    let version = parts[1].trim();
    if name.is_empty() || version.is_empty() {
        return None;
    }
    Some((name.to_string(), version.to_string()))
}

fn python_import_aliases(package_name: &str) -> Vec<String> {
    let canonical = package_name.to_lowercase();
    let mut aliases = vec![
        normalise_import(&canonical),
        normalise_import(&canonical.replace('-', "_")),
        normalise_import(&canonical.replace('_', "-")),
    ];

    if let Some(stripped) = canonical.strip_prefix("python-") {
        aliases.push(normalise_import(stripped));
    }

    // Common distribution-name -> import-name mappings.
    let known = [
        ("beautifulsoup4", "bs4"),
        ("pyyaml", "yaml"),
        ("python-dateutil", "dateutil"),
        ("opencv-python", "cv2"),
        ("scikit-learn", "sklearn"),
        ("scikit-image", "skimage"),
        ("pillow", "pil"),
        ("pyjwt", "jwt"),
    ];
    for (dist, import) in known {
        if canonical == dist {
            aliases.push(normalise_import(import));
        }
    }

    aliases.sort();
    aliases.dedup();
    aliases
}

fn is_probable_builtin_import(import_name: &str) -> bool {
    let normalized = normalise_import(import_name);
    if normalized.is_empty() {
        return false;
    }

    const RUST_BUILTINS: &[&str] = &["std", "core", "alloc", "proc_macro", "test"];
    const NODE_BUILTINS: &[&str] = &[
        "assert",
        "buffer",
        "child_process",
        "crypto",
        "events",
        "fs",
        "http",
        "https",
        "net",
        "os",
        "path",
        "stream",
        "tls",
        "url",
        "util",
        "worker_threads",
        "zlib",
    ];
    const PY_BUILTINS: &[&str] = &[
        "abc",
        "argparse",
        "asyncio",
        "collections",
        "datetime",
        "functools",
        "itertools",
        "json",
        "logging",
        "math",
        "os",
        "pathlib",
        "re",
        "subprocess",
        "sys",
        "time",
        "typing",
        "unittest",
    ];
    const GO_BUILTINS: &[&str] = &[
        "bufio",
        "bytes",
        "context",
        "crypto",
        "database.sql",
        "encoding.json",
        "errors",
        "fmt",
        "io",
        "log",
        "math",
        "net.http",
        "net.url",
        "os",
        "path",
        "regexp",
        "sort",
        "strconv",
        "strings",
        "sync",
        "testing",
        "time",
    ];

    RUST_BUILTINS
        .iter()
        .chain(NODE_BUILTINS.iter())
        .chain(PY_BUILTINS.iter())
        .chain(GO_BUILTINS.iter())
        .any(|prefix| {
            normalized == *prefix
                || normalized.starts_with(&format!("{}.", prefix))
                || normalized == format!("node.{}", prefix)
                || normalized.starts_with(&format!("node.{}.", prefix))
        })
}

/// Annotate PDG external module nodes with resolved dependency metadata.
///
/// Walks all nodes with `language == "external"` and attempts to resolve them
/// against the registry.  Resolved nodes get their `language` field updated
/// from `"external"` to `"external:<ecosystem>"` and the node name is enriched
/// with the locked version.
pub fn annotate_external_nodes(
    pdg: &mut crate::graph::pdg::ProgramDependenceGraph,
    registry: &ExternalDependencyRegistry,
) -> AnnotationStats {
    use crate::graph::pdg::NodeType;
    let mut stats = AnnotationStats::default();
    let mut unresolved_imports = HashSet::new();

    let external_nodes: Vec<crate::graph::pdg::NodeId> = pdg
        .node_indices()
        .filter(|&idx| {
            pdg.get_node(idx)
                .map(|n| matches!(n.node_type, NodeType::External))
                .unwrap_or(false)
        })
        .collect();

    stats.total_external = external_nodes.len();

    for node_id in external_nodes {
        let import_name = {
            let Some(node) = pdg.get_node(node_id) else {
                continue;
            };
            node.name.clone()
        };

        if let Some(dep) = registry.resolve(&import_name) {
            stats.resolved += 1;
            // Update node metadata to reflect the resolved package
            if let Some(node) = pdg.get_node_mut(node_id) {
                node.language = format!("external:{}", dep.ecosystem);
                // Preserve original name but append version info
                if !dep.version.is_empty() && dep.version != "*" {
                    node.id = format!(
                        "{}@{}",
                        node.id.split('@').next().unwrap_or(&node.id),
                        dep.version
                    );
                }
            }
        } else if is_probable_builtin_import(&import_name) {
            stats.builtin += 1;
            if let Some(node) = pdg.get_node_mut(node_id) {
                node.language = "external:system".to_string();
            }
        } else {
            unresolved_imports.insert(import_name);
        }
    }

    stats.unresolved = unresolved_imports.len();
    stats
}

/// Statistics from annotating external nodes.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AnnotationStats {
    /// Total external module nodes found.
    pub total_external: usize,
    /// Successfully resolved via lock file.
    pub resolved: usize,
    /// Recognized as builtin/system modules.
    pub builtin: usize,
    /// Unique external imports still unresolved after manifest/lockfile matching.
    pub unresolved: usize,
}

/// Discover dependency manifests and lockfiles while respecting project exclusion config.
///
/// When `exclude_dirs` is provided, those directory names are skipped in addition to
/// the default hidden-directory heuristic. This allows callers to pass the directory
/// patterns from `ExclusionConfig` so that user-excluded directories are respected.
pub fn discover_dependency_manifests(root: &Path, exclude_dirs: Option<&[String]>) -> Vec<std::path::PathBuf> {
    const MANIFEST_NAMES: &[&str] = &[
        "Cargo.lock",
        "Cargo.toml",
        "package-lock.json",
        "npm-shrinkwrap.json",
        "package.json",
        "yarn.lock",
        "pnpm-lock.yaml",
        "requirements.txt",
        "Pipfile.lock",
        "pyproject.toml",
        "poetry.lock",
        "go.mod",
        "go.sum",
        "Gemfile.lock",
        "composer.lock",
    ];
    const SKIP_DIRS: &[&str] = &[
        ".git",
        ".hg",
        ".svn",
        ".idea",
        ".vscode",
        "node_modules",
        "target",
        "dist",
        "build",
        "out",
        "coverage",
        ".venv",
        "venv",
        "env",
        "__pycache__",
        "vendor",
    ];

    let manifest_names: HashSet<&str> = MANIFEST_NAMES.iter().copied().collect();
    let mut discovered = Vec::new();

    let mut walker = walkdir::WalkDir::new(root).follow_links(false).into_iter();
    while let Some(entry) = walker.next() {
        let Ok(entry) = entry else {
            continue;
        };
        let path = entry.path();
        let file_name = entry.file_name().to_string_lossy();

        if path != root && file_name.starts_with('.') && entry.file_type().is_dir() {
            walker.skip_current_dir();
            continue;
        }

        if entry.file_type().is_dir() {
            // Skip hardcoded common non-project directories
            if SKIP_DIRS.contains(&file_name.as_ref()) {
                walker.skip_current_dir();
                continue;
            }
            // Also skip any caller-provided exclusion patterns
            if let Some(excluded) = exclude_dirs {
                if excluded.iter().any(|p| {
                    // Match the directory name against the pattern's leaf segment.
                    // For patterns like "target/debug", only match if the relative path
                    // from root starts with the pattern — not if any segment equals the leaf.
                    let trimmed = p.trim_matches('*').trim_matches('/');
                    let relative = path.strip_prefix(root).ok()
                        .and_then(|r| r.to_str())
                        .unwrap_or("");
                    // Check if the relative directory path matches or is a prefix
                    let dir_relative = if relative.ends_with('/') {
                        relative.to_string()
                    } else {
                        format!("{}/", relative)
                    };
                    // Exact match: this directory equals the pattern
                    trimmed == relative
                        || trimmed == relative.trim_end_matches('/')
                        // Prefix match: this directory is inside the excluded pattern
                        || dir_relative.starts_with(&format!("{}/", trimmed))
                }) {
                    walker.skip_current_dir();
                    continue;
                }
            }
            continue;
        }

        if entry.file_type().is_file() && manifest_names.contains(file_name.as_ref()) {
            discovered.push(path.to_path_buf());
        }
    }

    discovered.sort_by(|a, b| {
        let a_depth = a
            .strip_prefix(root)
            .map(|p| p.components().count())
            .unwrap_or(usize::MAX);
        let b_depth = b
            .strip_prefix(root)
            .map(|p| p.components().count())
            .unwrap_or(usize::MAX);
        a_depth.cmp(&b_depth).then_with(|| a.cmp(b))
    });
    discovered
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_cargo_lock() {
        let content = r#"
[[package]]
name = "serde"
version = "1.0.210"

[[package]]
name = "serde_json"
version = "1.0.128"

[[package]]
name = "tokio"
version = "1.40.0"
"#;
        let mut registry = ExternalDependencyRegistry::new();
        registry.parse_cargo_lock(content);

        assert_eq!(registry.len(), 3);
        let serde = registry.resolve("serde").unwrap();
        assert_eq!(serde.name, "serde");
        assert_eq!(serde.version, "1.0.210");
        assert_eq!(serde.ecosystem, Ecosystem::Cargo);

        let serde_json = registry.resolve("serde_json").unwrap();
        assert_eq!(serde_json.name, "serde_json");
        assert_eq!(serde_json.version, "1.0.128");
    }

    #[test]
    fn parse_package_lock_json_v2() {
        let content = r#"{
  "packages": {
    "": { "name": "my-app" },
    "node_modules/react": { "version": "18.2.0" },
    "node_modules/@types/react": { "version": "18.2.45" },
    "node_modules/lodash": { "version": "4.17.21" }
  }
}"#;
        let mut registry = ExternalDependencyRegistry::new();
        registry.parse_package_lock_json(content);

        assert_eq!(registry.len(), 3);
        let react = registry.resolve("react").unwrap();
        assert_eq!(react.version, "18.2.0");
        assert_eq!(react.ecosystem, Ecosystem::Npm);

        let types_react = registry.resolve("@types/react").unwrap();
        assert_eq!(types_react.version, "18.2.45");
    }

    #[test]
    fn parse_package_json_dependencies() {
        let content = r#"{
  "dependencies": {
    "react": "^18.2.0"
  },
  "devDependencies": {
    "typescript": "^5.4.0"
  }
}"#;
        let mut registry = ExternalDependencyRegistry::new();
        registry.parse_package_json(content);

        assert!(registry.resolve("react").is_some());
        assert!(registry.resolve("typescript").is_some());
    }

    #[test]
    fn parse_yarn_lock() {
        let content = r#"# yarn lockfile v1

"@babel/core@^7.0.0":
  version "7.24.0"
  resolved "https://registry.yarnpkg.com/@babel/core/-/core-7.24.0.tgz"

react@^18.0.0:
  version "18.2.0"
  resolved "https://registry.yarnpkg.com/react/-/react-18.2.0.tgz"
"#;
        let mut registry = ExternalDependencyRegistry::new();
        registry.parse_yarn_lock(content);

        assert_eq!(registry.len(), 2);
        let react = registry.resolve("react").unwrap();
        assert_eq!(react.version, "18.2.0");
    }

    #[test]
    fn parse_pnpm_lock() {
        let content = r#"
/react@18.2.0:
  resolution: {integrity: sha512-abc}
/@babel/core@7.24.0:
  resolution: {integrity: sha512-def}
"#;
        let mut registry = ExternalDependencyRegistry::new();
        registry.parse_pnpm_lock(content);

        assert!(registry.resolve("react").is_some());
        assert!(registry.resolve("@babel/core").is_some());
    }

    #[test]
    fn parse_requirements_txt() {
        let content = r#"
numpy==1.26.0
requests>=2.31.0
flask
# comment
-e git+https://example.com

beautifulsoup4[extra]==4.12.0
"#;
        let mut registry = ExternalDependencyRegistry::new();
        registry.parse_requirements_txt(content);

        assert_eq!(registry.len(), 4);
        let numpy = registry.resolve("numpy").unwrap();
        assert_eq!(numpy.version, "==1.26.0");
        assert_eq!(numpy.ecosystem, Ecosystem::Python);

        let flask = registry.resolve("flask").unwrap();
        assert_eq!(flask.version, "*");

        let bs4 = registry.resolve("beautifulsoup4").unwrap();
        assert_eq!(bs4.version, "==4.12.0");
    }

    #[test]
    fn parse_pipfile_lock() {
        let content = r#"{
  "_meta": {},
  "default": {
    "numpy": { "version": "==1.26.0" },
    "requests": { "version": "==2.31.0" }
  },
  "develop": {
    "pytest": { "version": "==7.4.0" }
  }
}"#;
        let mut registry = ExternalDependencyRegistry::new();
        registry.parse_pipfile_lock(content);

        assert_eq!(registry.len(), 3);
        let numpy = registry.resolve("numpy").unwrap();
        assert_eq!(numpy.version, "==1.26.0");

        let pytest = registry.resolve("pytest").unwrap();
        assert_eq!(pytest.version, "==7.4.0");
    }

    #[test]
    fn parse_pyproject_toml_project_dependencies() {
        let content = r#"
[project]
dependencies = [
  "requests>=2.31.0",
  "beautifulsoup4==4.12.0"
]
"#;
        let mut registry = ExternalDependencyRegistry::new();
        registry.parse_pyproject_toml(content);

        assert!(registry.resolve("requests").is_some());
        // Alias mapping should resolve bs4 imports.
        assert!(registry.resolve("bs4").is_some());
    }

    #[test]
    fn parse_poetry_lock() {
        let content = r#"
[[package]]
name = "fastapi"
version = "0.110.0"

[[package]]
name = "uvicorn"
version = "0.29.0"
"#;
        let mut registry = ExternalDependencyRegistry::new();
        registry.parse_poetry_lock(content);

        assert!(registry.resolve("fastapi").is_some());
        assert!(registry.resolve("uvicorn").is_some());
    }

    #[test]
    fn python_aliases_resolve_import_names() {
        let mut registry = ExternalDependencyRegistry::new();
        registry.insert(ExternalDependency {
            name: "python-dateutil".to_string(),
            version: "2.9.0".to_string(),
            ecosystem: Ecosystem::Python,
        });
        registry.insert(ExternalDependency {
            name: "beautifulsoup4".to_string(),
            version: "4.12.0".to_string(),
            ecosystem: Ecosystem::Python,
        });

        assert!(registry.resolve("dateutil").is_some());
        assert!(registry.resolve("bs4").is_some());
    }

    #[test]
    fn parse_go_sum() {
        let content = r#"github.com/gorilla/mux v1.8.1 h1:abc123
github.com/gorilla/mux v1.8.1/go.mod h1:xyz789
github.com/stretchr/testify v1.9.0 h1:def456
"#;
        let mut registry = ExternalDependencyRegistry::new();
        registry.parse_go_sum(content);

        assert_eq!(registry.len(), 2);
        let mux = registry.resolve("github.com/gorilla/mux").unwrap();
        assert_eq!(mux.version, "v1.8.1");
        assert_eq!(mux.ecosystem, Ecosystem::GoModules);
    }

    #[test]
    fn parse_gemfile_lock() {
        let content = r#"GEM
  remote: https://rubygems.org/
  specs:
    rails (7.1.0)
      activesupport (= 7.1.0)
    activesupport (7.1.0)
    minitest (5.20.0)

PLATFORMS
  ruby
"#;
        let mut registry = ExternalDependencyRegistry::new();
        registry.parse_gemfile_lock(content);

        assert!(registry.len() >= 2);
        let rails = registry.resolve("rails").unwrap();
        assert_eq!(rails.version, "7.1.0");
        assert_eq!(rails.ecosystem, Ecosystem::RubyGems);
    }

    #[test]
    fn parse_composer_lock() {
        let content = r#"{
  "packages": [
    { "name": "monolog/monolog", "version": "3.5.0" },
    { "name": "symfony/console", "version": "v6.4.0" }
  ],
  "packages-dev": [
    { "name": "phpunit/phpunit", "version": "10.5.0" }
  ]
}"#;
        let mut registry = ExternalDependencyRegistry::new();
        registry.parse_composer_lock(content);

        assert_eq!(registry.len(), 3);
        let monolog = registry.resolve("monolog/monolog").unwrap();
        assert_eq!(monolog.version, "3.5.0");
        assert_eq!(monolog.ecosystem, Ecosystem::Composer);
    }

    #[test]
    fn resolve_with_prefix_matching() {
        let mut registry = ExternalDependencyRegistry::new();
        registry.insert(ExternalDependency {
            name: "serde".to_string(),
            version: "1.0.210".to_string(),
            ecosystem: Ecosystem::Cargo,
        });

        // Exact match
        assert!(registry.resolve("serde").is_some());

        // Prefix match (import path has sub-module)
        assert!(registry.resolve("serde.de").is_some());
        assert!(registry.resolve("serde.ser.Serializer").is_some());

        // No match
        assert!(registry.resolve("tokio").is_none());
    }

    #[test]
    fn resolve_with_hyphen_underscore_normalisation() {
        let mut registry = ExternalDependencyRegistry::new();
        registry.insert(ExternalDependency {
            name: "serde-json".to_string(),
            version: "1.0.0".to_string(),
            ecosystem: Ecosystem::Cargo,
        });

        // Underscore variant should match
        assert!(registry.resolve("serde_json").is_some());
        // Original hyphen should also match
        assert!(registry.resolve("serde-json").is_some());
    }

    #[test]
    fn resolve_normalises_import_path() {
        let mut registry = ExternalDependencyRegistry::new();
        registry.insert(ExternalDependency {
            name: "github.com/gorilla/mux".to_string(),
            version: "v1.8.1".to_string(),
            ecosystem: Ecosystem::GoModules,
        });

        // Path separator variants
        assert!(registry.resolve("github.com/gorilla/mux").is_some());
        assert!(registry.resolve("github.com.gorilla.mux").is_some());
    }

    #[test]
    fn annotate_external_nodes_works() {
        use crate::graph::pdg::{Node, NodeType, ProgramDependenceGraph};

        let mut pdg = ProgramDependenceGraph::new();
        let ext_id = pdg.add_node(Node {
            id: "serde".to_string(),
            name: "serde".to_string(),
            node_type: NodeType::External,
            file_path: "".to_string(),
            byte_range: (0, 0),
            language: "external".to_string(),
            complexity: 0,
        });

        let internal_id = pdg.add_node(Node {
            id: "my_func".to_string(),
            name: "my_func".to_string(),
            node_type: NodeType::Function,
            file_path: "src/lib.rs".to_string(),
            byte_range: (0, 100),
            language: "rust".to_string(),
            complexity: 5,
        });

        let mut registry = ExternalDependencyRegistry::new();
        registry.insert(ExternalDependency {
            name: "serde".to_string(),
            version: "1.0.210".to_string(),
            ecosystem: Ecosystem::Cargo,
        });

        let stats = annotate_external_nodes(&mut pdg, &registry);

        assert_eq!(stats.total_external, 1);
        assert_eq!(stats.resolved, 1);
        assert_eq!(stats.unresolved, 0);

        // Check the node was updated
        let node = pdg.get_node(ext_id).unwrap();
        assert_eq!(node.language, "external:cargo");
        assert!(node.id.contains("1.0.210"));

        // Internal node should be unchanged
        let internal = pdg.get_node(internal_id).unwrap();
        assert_eq!(internal.language, "rust");
    }

    #[test]
    fn from_project_empty_dir_returns_empty() {
        let dir = tempfile::tempdir().expect("tempdir");
        let registry = ExternalDependencyRegistry::from_project(dir.path());
        assert_eq!(registry.len(), 0);
    }

    #[test]
    fn from_project_with_cargo_lock() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            dir.path().join("Cargo.lock"),
            "[[package]]\nname = \"anyhow\"\nversion = \"1.0.86\"\n",
        )
        .expect("write");

        let registry = ExternalDependencyRegistry::from_project(dir.path());
        assert_eq!(registry.len(), 1);
        assert!(registry.resolve("anyhow").is_some());
    }

    #[test]
    fn from_project_with_cargo_toml_fallback() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            dir.path().join("Cargo.toml"),
            "[package]\nname = \"demo\"\nversion = \"0.1.0\"\n\n[dependencies]\nserde = \"1.0\"\n",
        )
        .expect("write");

        let registry = ExternalDependencyRegistry::from_project(dir.path());
        assert!(registry.resolve("serde").is_some());
    }

    #[test]
    fn from_project_with_requirements_txt() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            dir.path().join("requirements.txt"),
            "numpy==1.26.0\nrequests>=2.31.0\n",
        )
        .expect("write");

        let registry = ExternalDependencyRegistry::from_project(dir.path());
        assert_eq!(registry.len(), 2);
        assert!(registry.resolve("numpy").is_some());
        assert!(registry.resolve("requests").is_some());
    }

    #[test]
    fn from_project_with_package_json() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            dir.path().join("package.json"),
            r#"{"dependencies":{"react":"^18.2.0"}}"#,
        )
        .expect("write");

        let registry = ExternalDependencyRegistry::from_project(dir.path());
        assert!(registry.resolve("react").is_some());
    }

    #[test]
    fn from_project_discovers_nested_workspace_manifests() {
        let dir = tempfile::tempdir().expect("tempdir");
        let nested = dir.path().join("packages").join("web");
        std::fs::create_dir_all(&nested).expect("mkdir");
        std::fs::write(
            nested.join("package.json"),
            r#"{"dependencies":{"react":"^18.2.0","zod":"^3.23.8"}}"#,
        )
        .expect("write");

        let registry = ExternalDependencyRegistry::from_project(dir.path());
        assert!(registry.resolve("react").is_some());
        assert!(registry.resolve("zod").is_some());
    }

    #[test]
    fn from_project_with_go_mod() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            dir.path().join("go.mod"),
            "module demo\n\nrequire github.com/gorilla/mux v1.8.1\n",
        )
        .expect("write");

        let registry = ExternalDependencyRegistry::from_project(dir.path());
        assert!(registry.resolve("github.com/gorilla/mux").is_some());
    }

    #[test]
    fn annotate_external_nodes_marks_builtin_modules() {
        use crate::graph::pdg::{Node, NodeType, ProgramDependenceGraph};

        let mut pdg = ProgramDependenceGraph::new();
        let ext_id = pdg.add_node(Node {
            id: "std".to_string(),
            name: "std".to_string(),
            node_type: NodeType::External,
            file_path: "".to_string(),
            byte_range: (0, 0),
            language: "external".to_string(),
            complexity: 0,
        });

        let registry = ExternalDependencyRegistry::new();
        let stats = annotate_external_nodes(&mut pdg, &registry);
        assert_eq!(stats.total_external, 1);
        assert_eq!(stats.resolved, 0);
        assert_eq!(stats.builtin, 1);
        assert_eq!(stats.unresolved, 0);
        assert_eq!(pdg.get_node(ext_id).unwrap().language, "external:system");
    }

    #[test]
    fn annotate_external_nodes_deduplicates_unresolved_import_names() {
        use crate::graph::pdg::{Node, NodeType, ProgramDependenceGraph};

        let mut pdg = ProgramDependenceGraph::new();
        pdg.add_node(Node {
            id: "src/main.rs:__external__:react".to_string(),
            name: "react".to_string(),
            node_type: NodeType::External,
            file_path: "src/main.rs".to_string(),
            byte_range: (0, 0),
            language: "external".to_string(),
            complexity: 0,
        });
        pdg.add_node(Node {
            id: "src/app.ts:__external__:react".to_string(),
            name: "react".to_string(),
            node_type: NodeType::External,
            file_path: "src/app.ts".to_string(),
            byte_range: (0, 0),
            language: "external".to_string(),
            complexity: 0,
        });

        let registry = ExternalDependencyRegistry::new();
        let stats = annotate_external_nodes(&mut pdg, &registry);
        assert_eq!(stats.total_external, 2);
        assert_eq!(stats.unresolved, 1);
    }
}

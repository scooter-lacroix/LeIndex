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
    /// Build a dependency registry from manifest/lockfile paths.
    ///
    /// In monorepos, manifests are grouped by workspace directory before merging,
    /// so lockfile versions from one workspace don't overwrite entries from another.
    /// The final merge still uses last-wins across workspaces, but each workspace's
    /// lockfile entries take precedence over that workspace's manifest entries.
    pub fn from_manifest_paths(root: &Path, manifest_paths: &[PathBuf]) -> Self {
        // Sort so lockfiles (priority 1) are parsed AFTER manifests (priority 0),
        // ensuring lockfile versions overwrite the looser manifest-derived ranges.
        let mut sorted_paths = manifest_paths.to_vec();
        sorted_paths.sort_by(|a, b| {
            let a_name = a.file_name().and_then(|n| n.to_str()).unwrap_or("");
            let b_name = b.file_name().and_then(|n| n.to_str()).unwrap_or("");
            source_priority(a_name).cmp(&source_priority(b_name))
        });

        // Group manifests by their parent directory (workspace root).
        // Within each workspace, parse with local priority ordering, then merge
        // all workspaces into a single registry.
        let mut workspaces: std::collections::BTreeMap<PathBuf, Vec<PathBuf>> =
            std::collections::BTreeMap::new();
        for manifest_path in &sorted_paths {
            let path = if manifest_path.is_absolute() {
                manifest_path.clone()
            } else {
                root.join(manifest_path)
            };
            let ws_root = path.parent().unwrap_or(root).to_path_buf();
            workspaces.entry(ws_root).or_default().push(path);
        }

        let mut registry = Self::new();
        for (_ws_root, paths) in workspaces {
            // Parse each workspace's manifests into a local registry,
            // then merge into the global one.
            let mut local = Self::new();
            for path in &paths {
                let Some(file_name) = path.file_name().and_then(|name| name.to_str()) else {
                    continue;
                };
                let Ok(content) = std::fs::read_to_string(path) else {
                    continue;
                };

                match file_name {
                    "Cargo.lock" => local.parse_cargo_lock(&content),
                    "Cargo.toml" => local.parse_cargo_toml(&content),
                    "package-lock.json" | "npm-shrinkwrap.json" => {
                        local.parse_package_lock_json(&content)
                    }
                    "package.json" => local.parse_package_json(&content),
                    "yarn.lock" => local.parse_yarn_lock(&content),
                    "pnpm-lock.yaml" => local.parse_pnpm_lock(&content),
                    "bun.lock" => local.parse_bun_lock(&content),
                    "bun.lockb" => {} // Binary format, skip
                    "requirements.txt" => local.parse_requirements_txt(&content),
                    "Pipfile.lock" => local.parse_pipfile_lock(&content),
                    "pyproject.toml" => local.parse_pyproject_toml(&content),
                    "poetry.lock" => local.parse_poetry_lock(&content),
                    "go.mod" => local.parse_go_mod(&content),
                    "go.sum" => local.parse_go_sum(&content),
                    "Gemfile.lock" => local.parse_gemfile_lock(&content),
                    "composer.lock" => local.parse_composer_lock(&content),
                    _ => {}
                }
            }
            // Merge local workspace into the global registry.
            // Each workspace's lockfile entries override earlier entries for
            // the same package name, which is correct within-workspace.
            registry.merge(local);
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

    /// Merge another registry into this one. Entries from `other` overwrite
    /// entries with the same key in `self`, preserving lockfile-derived entries
    /// over manifest-derived ranges within each workspace.
    pub fn merge(&mut self, other: ExternalDependencyRegistry) {
        for (name, dep) in other.by_name {
            self.by_name.insert(name, dep);
        }
        for (prefix, canonical) in other.prefix_map {
            self.prefix_map.insert(prefix, canonical);
        }
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
                let name = if let Some(stripped) = spec.strip_prefix('@') {
                    // Scoped package: @scope/name@version
                    if let Some(at_pos) = stripped.rfind('@') {
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

            let (name, mut version) = if let Some(stripped) = spec.strip_prefix('@') {
                if let Some(pos) = stripped.rfind('@').map(|p| p + 1) {
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

    /// Parse bun.lock (JSONC format with trailing commas).
    ///
    /// The `packages` section maps package names to arrays where the first
    /// element is `"name@version"`.  The `workspaces` section contains
    /// `dependencies` and `devDependencies` maps with version constraints.
    fn parse_bun_lock(&mut self, content: &str) {
        // bun.lock uses JSONC comments and trailing commas. Normalize it before parsing.
        let cleaned = strip_trailing_commas(&strip_jsonc_comments(content));
        let Ok(parsed) = serde_json::from_str::<serde_json::Value>(&cleaned) else {
            return;
        };

        // Parse the "packages" section (resolved lockfile entries).
        if let Some(packages) = parsed.get("packages").and_then(|v| v.as_object()) {
            for (key, val) in packages {
                // Each value is an array: ["name@version", "", {dependencies}, "integrity"]
                if let Some(arr) = val.as_array() {
                    if let Some(spec) = arr.first().and_then(|v| v.as_str()) {
                        // Extract version from "name@version" string.
                        // For scoped packages like "@babel/core@7.29.0", we need
                        // to find the last '@' after the scope.
                        let version = if let Some(stripped) = spec.strip_prefix('@') {
                            stripped
                                .rfind('@')
                                .map(|pos| stripped[pos + 1..].to_string())
                        } else {
                            spec.rfind('@').map(|pos| spec[pos + 1..].to_string())
                        };
                        let version = version.unwrap_or_else(|| "*".to_string());
                        self.insert(ExternalDependency {
                            name: key.clone(),
                            version,
                            ecosystem: Ecosystem::Npm,
                        });
                    }
                }
            }
        }

        // Also parse workspace dependency constraints as fallback.
        if let Some(workspaces) = parsed.get("workspaces").and_then(|v| v.as_object()) {
            for (_ws_name, ws_val) in workspaces {
                for section in &["dependencies", "devDependencies"] {
                    if let Some(deps) = ws_val.get(section).and_then(|v| v.as_object()) {
                        for (name, version) in deps {
                            // Don't overwrite lockfile entries.
                            if self.by_name.contains_key(name) {
                                continue;
                            }
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

            if let Some(section_name) = pyproject_section_name(line) {
                section = section_name.to_lowercase();
                collecting_project_deps = false;
                project_deps_buf.clear();
                continue;
            }

            if section == "project" {
                self.parse_project_dependency_line(
                    line,
                    &mut collecting_project_deps,
                    &mut project_deps_buf,
                );
                continue;
            }

            self.parse_poetry_dependency_line(&section, line);
        }
    }

    fn parse_project_dependency_line(
        &mut self,
        line: &str,
        collecting: &mut bool,
        dependencies: &mut String,
    ) {
        if let Some(rhs) = line.strip_prefix("dependencies = ") {
            if rhs.contains(']') {
                for dependency in parse_python_dependency_list(rhs) {
                    self.insert(dependency);
                }
            } else {
                *collecting = true;
                dependencies.push_str(rhs);
                dependencies.push('\n');
            }
            return;
        }

        if !*collecting {
            return;
        }

        dependencies.push_str(line);
        dependencies.push('\n');
        if line.contains(']') {
            *collecting = false;
            for dependency in parse_python_dependency_list(dependencies) {
                self.insert(dependency);
            }
            dependencies.clear();
        }
    }

    fn parse_poetry_dependency_line(&mut self, section: &str, line: &str) {
        if !is_poetry_dependency_section(section) {
            return;
        }
        let Some((name_raw, value_raw)) = line.split_once('=') else {
            return;
        };

        let name = name_raw.trim().trim_matches('"').trim_matches('\'');
        if name.is_empty() || name.eq_ignore_ascii_case("python") {
            return;
        }

        // Poetry deps may use an inline table: `package = { version = "1.2.3", ... }`.
        // Extract the quoted `version` field from the table; otherwise the simple
        // `name = "1.2.3"` constraint form.
        let value_trim = value_raw.trim();
        let version = if value_trim.starts_with('{') {
            value_trim
                .trim_start_matches('{')
                .trim_end_matches('}')
                .split(',')
                .find_map(|field| {
                    let field = field.trim();
                    let rest = field.strip_prefix("version")?;
                    unquote(rest.trim_start_matches(['=', ' ']).trim()).into()
                })
                .filter(|v| !v.is_empty())
                .unwrap_or_else(|| "*".to_string())
        } else {
            value_trim
                .split(',')
                .next()
                .map(unquote)
                .filter(|value| !value.is_empty())
                .unwrap_or_else(|| "*".to_string())
        };
        self.insert(ExternalDependency {
            name: name.to_string(),
            version,
            ecosystem: Ecosystem::Python,
        });
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
        .replace(['/', '\\', ':'], ".")
        .replace("..", ".")
        .trim_matches('.')
        .to_lowercase()
}

/// Remove surrounding quotes from a TOML/YAML/JSON-like value.
fn unquote(s: &str) -> String {
    s.trim().trim_matches('"').trim_matches('\'').to_string()
}

fn strip_jsonc_comments(content: &str) -> String {
    let mut result = String::with_capacity(content.len());
    let mut chars = content.chars().peekable();
    let mut in_string = false;

    while let Some(c) = chars.next() {
        if in_string {
            result.push(c);
            if c == '"' {
                let mut backslashes = 0usize;
                for prev in result[..result.len() - c.len_utf8()].chars().rev() {
                    if prev == '\\' {
                        backslashes += 1;
                    } else {
                        break;
                    }
                }
                if backslashes % 2 == 0 {
                    in_string = false;
                }
            }
            continue;
        }

        if c == '"' {
            in_string = true;
            result.push(c);
            continue;
        }

        if c == '/' {
            match chars.peek().copied() {
                Some('/') => {
                    chars.next();
                    for next in chars.by_ref() {
                        if next == '\n' {
                            result.push('\n');
                            break;
                        }
                    }
                    continue;
                }
                Some('*') => {
                    chars.next();
                    let mut prev = '\0';
                    for next in chars.by_ref() {
                        if prev == '*' && next == '/' {
                            break;
                        }
                        prev = next;
                    }
                    continue;
                }
                _ => {}
            }
        }

        result.push(c);
    }

    result
}

/// Strip trailing commas from JSONC content to make it valid JSON.
/// Removes commas that appear immediately before `}` or `]`, handling
/// whitespace and newlines between the comma and the closing bracket.
fn strip_trailing_commas(content: &str) -> String {
    let mut result = String::with_capacity(content.len());
    let chars: Vec<char> = content.chars().collect();
    let mut in_string = false;
    let mut index = 0;

    while index < chars.len() {
        let character = chars[index];
        if in_string {
            result.push(character);
            if character == '"' && is_unescaped_quote(&chars, index) {
                in_string = false;
            }
        } else if character == '"' {
            in_string = true;
            result.push(character);
        } else if character != ',' || !is_trailing_json_comma(&chars, index + 1) {
            result.push(character);
        }
        index += 1;
    }

    result
}

fn is_unescaped_quote(chars: &[char], index: usize) -> bool {
    chars[..index]
        .iter()
        .rev()
        .take_while(|&&character| character == '\\')
        .count()
        % 2
        == 0
}

fn is_trailing_json_comma(chars: &[char], start: usize) -> bool {
    chars
        .iter()
        .skip(start)
        .find(|&&character| !matches!(character, ' ' | '\t' | '\n' | '\r'))
        .is_some_and(|&character| matches!(character, '}' | ']'))
}

fn pyproject_section_name(line: &str) -> Option<&str> {
    line.strip_prefix('[')?.strip_suffix(']')
}

fn is_poetry_dependency_section(section: &str) -> bool {
    section.starts_with("tool.poetry.dependencies")
        || section.starts_with("tool.poetry.group.") && section.ends_with(".dependencies")
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

    const RUST_BUILTINS: &[&str] = &[
        "std",
        "core",
        "alloc",
        "proc_macro",
        "test",
        // Rust path keywords that are internal, not external deps.
        "crate",
        "super",
        "self",
        "Self",
    ];
    const NODE_BUILTINS: &[&str] = &[
        "assert",
        "async_hooks",
        "buffer",
        "child_process",
        "cluster",
        "console",
        "constants",
        "crypto",
        "dgram",
        "diagnostics_channel",
        "dns",
        "domain",
        "events",
        "fs",
        "http",
        "http2",
        "https",
        "inspector",
        "module",
        "net",
        "os",
        "path",
        "perf_hooks",
        "process",
        "punycode",
        "querystring",
        "readline",
        "repl",
        "stream",
        "string_decoder",
        "sys",
        "timers",
        "tls",
        "trace_events",
        "tty",
        "url",
        "util",
        "v8",
        "vm",
        "wasi",
        "worker_threads",
        "zlib",
    ];
    const PY_BUILTINS: &[&str] = &[
        "abc",
        "argparse",
        "asyncio",
        "base64",
        "bisect",
        "calendar",
        "collections",
        "concurrent",
        "configparser",
        "contextlib",
        "copy",
        "csv",
        "datetime",
        "decimal",
        "difflib",
        "email",
        "enum",
        "functools",
        "glob",
        "hashlib",
        "heapq",
        "html",
        "http",
        "importlib",
        "inspect",
        "io",
        "itertools",
        "json",
        "logging",
        "math",
        "multiprocessing",
        "operator",
        "os",
        "pathlib",
        "pickle",
        "platform",
        "pprint",
        "queue",
        "random",
        "re",
        "shutil",
        "signal",
        "socket",
        "sqlite3",
        "ssl",
        "statistics",
        "string",
        "struct",
        "subprocess",
        "sys",
        "tarfile",
        "tempfile",
        "textwrap",
        "threading",
        "time",
        "traceback",
        "typing",
        "unittest",
        "urllib",
        "uuid",
        "warnings",
        "weakref",
        "xml",
        "zipfile",
    ];
    const GO_BUILTINS: &[&str] = &[
        "bufio",
        "bytes",
        "compress",
        "context",
        "container",
        "crypto",
        "database/sql",
        "debug",
        "embed",
        "encoding",
        "encoding/json",
        "errors",
        "expvar",
        "flag",
        "fmt",
        "go",
        "hash",
        "html",
        "image",
        "io",
        "log",
        "math",
        "mime",
        "net",
        "net/http",
        "net/url",
        "os",
        "path",
        "plugin",
        "reflect",
        "regexp",
        "runtime",
        "sort",
        "strconv",
        "strings",
        "sync",
        "syscall",
        "testing",
        "text",
        "time",
        "unicode",
        "unsafe",
    ];

    RUST_BUILTINS
        .iter()
        .chain(NODE_BUILTINS.iter())
        .chain(PY_BUILTINS.iter())
        .chain(GO_BUILTINS.iter())
        .any(|prefix| {
            // Normalize prefix the same way as the import so that Go
            // builtins written with slashes (e.g. "database/sql") match
            // the dot-normalized import name (e.g. "database.sql").
            let norm_prefix = normalise_import(prefix);
            normalized == norm_prefix
                || normalized.starts_with(&format!("{}.", norm_prefix))
                || normalized == format!("node.{}", norm_prefix)
                || normalized.starts_with(&format!("node.{}.", norm_prefix))
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
                .map(|n| {
                    matches!(n.node_type, NodeType::External) || n.language == "external"
                    // Legacy compat
                })
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
pub fn discover_dependency_manifests(
    root: &Path,
    exclude_dirs: Option<&[String]>,
) -> Vec<std::path::PathBuf> {
    const MANIFEST_NAMES: &[&str] = &[
        "Cargo.lock",
        "Cargo.toml",
        "Gemfile.lock",
        "Pipfile.lock",
        "bun.lock",
        "bun.lockb",
        "composer.lock",
        "go.mod",
        "go.sum",
        "npm-shrinkwrap.json",
        "package-lock.json",
        "package.json",
        "pnpm-lock.yaml",
        "poetry.lock",
        "pyproject.toml",
        "requirements.txt",
        "yarn.lock",
    ];
    use crate::cli::skip_dirs::SKIP_DIRS;

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
                    let relative = path
                        .strip_prefix(root)
                        .ok()
                        .and_then(|r| r.to_str())
                        .unwrap_or("");
                    // Check if the relative directory path matches or is a prefix
                    let dir_relative = if relative.ends_with('/') {
                        relative.to_string()
                    } else {
                        format!("{}/", relative)
                    };
                    // Leaf-name match: any directory component equals the pattern
                    // (consistent with SKIP_DIRS behavior — matches anywhere in tree)
                    file_name.as_ref() == trimmed
                        // Exact match: this directory equals the pattern
                        || trimmed == relative
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

        if entry.file_type().is_file() && MANIFEST_NAMES.binary_search(&file_name.as_ref()).is_ok()
        {
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
#[path = "external_deps_test.rs"]
mod tests;

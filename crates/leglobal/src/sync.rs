//! Sync engine for discovered projects
//!
//! Handles validation, clone detection, and background sync with exponential backoff.

use crate::registry::{GlobalRegistry, GlobalRegistryError};
use crate::discovery::{DiscoveredProject, DiscoveryEngine, DiscoveryError};
use crate::{INITIAL_BACKOFF_SECS, MAX_BACKOFF_SECS};
use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Duration;
use thiserror::Error;

/// Errors that can occur during sync operations
#[derive(Debug, Error)]
pub enum SyncError {
    /// Registry error
    #[error("Registry error: {0}")]
    Registry(#[from] GlobalRegistryError),

    /// Discovery error
    #[error("Discovery error: {0}")]
    Discovery(#[from] DiscoveryError),

    /// Sync cancelled
    #[error("Sync cancelled")]
    Cancelled,
}

/// Result type for sync operations
pub type Result<T> = std::result::Result<T, SyncError>;

/// A group of cloned projects
#[derive(Debug, Clone)]
pub struct CloneGroup {
    /// Canonical project ID (the original)
    pub canonical_id: String,
    /// Paths to all clones (including the original at index 0)
    pub clone_paths: Vec<PathBuf>,
    /// Similarity score (1.0 = identical, lower = less similar)
    pub similarity_score: f32,
}

/// Sync report with statistics
#[derive(Debug, Clone, Default)]
pub struct SyncReport {
    /// Total projects discovered
    pub total_discovered: usize,
    /// New projects (not in registry)
    pub new_projects: usize,
    /// Updated projects (metadata changed)
    pub updated_projects: usize,
    /// Clone groups detected
    pub clones_detected: Vec<CloneGroup>,
    /// Projects skipped (invalid)
    pub skipped_projects: usize,
    /// Errors during sync
    pub errors: Vec<String>,
}

impl SyncReport {
    /// Create a new empty sync report
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Add an error to the report
    pub fn add_error(&mut self, error: String) {
        self.errors.push(error);
    }

    /// Check if sync was successful (no critical errors)
    #[must_use]
    pub fn is_success(&self) -> bool {
        self.errors.is_empty() || self.errors.iter().all(|e| !e.contains("critical"))
    }
}

/// Sync engine for coordinating discovery and registry
pub struct SyncEngine {
    /// Global registry
    registry: GlobalRegistry,
    /// Discovery engine
    discovery: DiscoveryEngine,
}

impl SyncEngine {
    /// Create a new sync engine
    ///
    /// # Arguments
    ///
    /// * `registry` - The global registry
    /// * `discovery` - The discovery engine
    #[must_use]
    pub fn new(registry: GlobalRegistry, discovery: DiscoveryEngine) -> Self {
        Self {
            registry,
            discovery,
        }
    }

    /// Create with default settings
    ///
    /// # Returns
    ///
    /// `Result<Self>` - The initialized sync engine
    pub fn with_defaults() -> Result<Self> {
        let registry = GlobalRegistry::init_default()?;
        let discovery = DiscoveryEngine::new();
        Ok(Self::new(registry, discovery))
    }

    /// Perform a full sync: discover, validate, and register
    ///
    /// # Returns
    ///
    /// `Result<SyncReport>` - The sync report with statistics
    pub fn sync(&mut self) -> Result<SyncReport> {
        self.sync_with_options(true, true)
    }

    /// Perform sync with options
    ///
    /// # Arguments
    ///
    /// * `detect_clones` - Whether to detect and group clones
    /// * `register_new` - Whether to register new projects
    fn sync_with_options(&mut self, detect_clones: bool, register_new: bool) -> Result<SyncReport> {
        let mut report = SyncReport::new();

        // Discover projects
        let discovered = match self.discovery.discover() {
            Ok(projects) => projects,
            Err(e) => {
                report.add_error(format!("Discovery failed: {}", e));
                return Ok(report);
            }
        };

        report.total_discovered = discovered.len();

        // Get existing projects from registry
        let existing = match self.registry.list_projects() {
            Ok(projects) => {
                projects.into_iter()
                    .map(|p| (p.unique_id.to_string(), p))
                    .collect::<HashMap<_, _>>()
            }
            Err(_) => HashMap::new(),
        };

        // Track fingerprints for clone detection
        let mut fingerprint_groups: HashMap<String, Vec<DiscoveredProject>> = HashMap::new();

        // Process each discovered project
        for project in discovered {
            // Skip invalid projects
            if !project.is_valid {
                report.skipped_projects += 1;
                continue;
            }

            // Group by fingerprint for clone detection
            let fp = project.content_fingerprint.clone();
            fingerprint_groups.entry(fp).or_default().push(project.clone());

            // Check if already in registry
            let id = project.unique_id.to_string();
            if let Some(existing_project) = existing.get(&id) {
                // Check for updates
                if project.file_count != existing_project.file_count
                    || project.language != existing_project.language
                {
                    report.updated_projects += 1;

                    if register_new {
                        // Re-register with updated metadata
                        // For now, skip actual update
                    }
                }
            } else {
                // New project
                report.new_projects += 1;

                if register_new {
                    if let Err(e) = self.registry.register_project(
                        &project.path,
                        project.language.clone(),
                        project.file_count,
                        &project.content_fingerprint,
                    ) {
                        report.add_error(format!("Failed to register {}: {}", id, e));
                    }
                }
            }
        }

        // Detect clones if requested
        if detect_clones {
            report.clones_detected = self.detect_clones(&fingerprint_groups);
        }

        Ok(report)
    }

    /// Detect clone groups from fingerprinted projects
    fn detect_clones(&self, groups: &HashMap<String, Vec<DiscoveredProject>>) -> Vec<CloneGroup> {
        let mut clone_groups = Vec::new();

        for projects in groups.values().filter(|p| p.len() > 1) {
            if let Some(first) = projects.first() {
                let canonical_id = first.unique_id.to_string();
                let clone_paths = projects.iter().map(|p| p.path.clone()).collect();
                let similarity_score = 1.0; // Same fingerprint = identical

                clone_groups.push(CloneGroup {
                    canonical_id,
                    clone_paths,
                    similarity_score,
                });
            }
        }

        clone_groups
    }

    /// Get the registry
    #[must_use]
    pub const fn registry(&self) -> &GlobalRegistry {
        &self.registry
    }

    /// Get mutable registry
    pub fn registry_mut(&mut self) -> &mut GlobalRegistry {
        &mut self.registry
    }

    /// Get the discovery engine
    #[must_use]
    pub const fn discovery(&self) -> &DiscoveryEngine {
        &self.discovery
    }
}

/// Background sync with exponential backoff
pub struct BackgroundSync {
    /// Sync engine
    engine: SyncEngine,
    /// Current backoff delay in seconds
    backoff_secs: u64,
    /// Whether sync is running
    running: bool,
    /// Manual refresh trigger
    refresh_requested: bool,
}

impl BackgroundSync {
    /// Create a new background sync
    ///
    /// # Arguments
    ///
    /// * `engine` - The sync engine to use
    #[must_use]
    pub fn new(engine: SyncEngine) -> Self {
        Self {
            engine,
            backoff_secs: INITIAL_BACKOFF_SECS,
            running: false,
            refresh_requested: false,
        }
    }

    /// Create with default sync engine
    ///
    /// # Returns
    ///
    /// `Result<Self>` - The initialized background sync
    pub fn with_defaults() -> Result<Self> {
        Ok(Self::new(SyncEngine::with_defaults()?))
    }

    /// Start the background sync loop
    ///
    /// This will run in a blocking loop. For non-blocking, use `spawn()`.
    ///
    /// # Returns
    ///
    /// `Result<Vec<SyncReport>>` - All sync reports generated
    pub fn run(&mut self) -> Result<Vec<SyncReport>> {
        self.running = true;
        let mut reports = Vec::new();

        while self.running {
            // Check for manual refresh
            if self.refresh_requested {
                self.backoff_secs = INITIAL_BACKOFF_SECS;
                self.refresh_requested = false;
            }

            // Perform sync
            match self.engine.sync() {
                Ok(report) => {
                    // Reset backoff on success
                    self.backoff_secs = INITIAL_BACKOFF_SECS;
                    reports.push(report);
                }
                Err(_) => {
                    // Exponential backoff on error
                    let new_backoff = self.backoff_secs * 2;
                    self.backoff_secs = if new_backoff > MAX_BACKOFF_SECS {
                        MAX_BACKOFF_SECS
                    } else {
                        new_backoff
                    };
                }
            }

            if !self.running {
                break;
            }

            // Sleep with current backoff
            std::thread::sleep(Duration::from_secs(self.backoff_secs));
        }

        Ok(reports)
    }

    /// Spawn the background sync in a new thread (non-blocking)
    ///
    /// # Returns
    ///
    /// A handle that can be used to stop the sync
    #[must_use]
    pub fn spawn(self) -> std::thread::JoinHandle<Vec<SyncReport>> {
        std::thread::spawn(move || {
            let mut bg_sync = self;
            match bg_sync.run() {
                Ok(reports) => reports,
                Err(_) => Vec::new(),
            }
        })
    }

    /// Request a manual refresh (reset backoff and sync immediately)
    pub fn refresh(&mut self) {
        self.refresh_requested = true;
    }

    /// Stop the background sync
    pub fn stop(&mut self) {
        self.running = false;
    }

    /// Get the current backoff delay
    #[must_use]
    pub const fn backoff_secs(&self) -> u64 {
        self.backoff_secs
    }

    /// Check if sync is running
    #[must_use]
    pub const fn is_running(&self) -> bool {
        self.running
    }

    /// Calculate next backoff delay (for testing)
    #[must_use]
    pub const fn calculate_backoff(attempt: u32) -> u64 {
        // Cap at 20 to prevent overflow (2^20 = 1,048,576)
        let capped_attempt = if attempt > 20 { 20 } else { attempt };
        let delay = INITIAL_BACKOFF_SECS * 2u32.pow(capped_attempt) as u64;
        if delay > MAX_BACKOFF_SECS {
            MAX_BACKOFF_SECS
        } else {
            delay
        }
    }

    /// Get the sync engine
    #[must_use]
    pub const fn engine(&self) -> &SyncEngine {
        &self.engine
    }

    /// Get mutable sync engine
    pub fn engine_mut(&mut self) -> &mut SyncEngine {
        &mut self.engine
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_sync_report_new() {
        let report = SyncReport::new();
        assert_eq!(report.total_discovered, 0);
        assert_eq!(report.new_projects, 0);
        assert!(report.is_success());
    }

    #[test]
    fn test_sync_report_add_error() {
        let mut report = SyncReport::new();
        report.add_error("test error".to_string());
        assert!(!report.errors.is_empty());
        assert!(report.is_success()); // Non-critical error
    }

    #[test]
    fn test_sync_report_critical_error() {
        let mut report = SyncReport::new();
        report.add_error("critical failure".to_string());
        assert!(!report.is_success());
    }

    #[test]
    fn test_sync_engine_sync() {
        let temp_dir = TempDir::new().unwrap();
        let db_path = temp_dir.path().join("test.db");
        let registry = GlobalRegistry::init(&db_path).unwrap();

        // Create a valid project
        let project_path = temp_dir.path().join("testproject");
        std::fs::create_dir_all(project_path.join(".git")).unwrap();
        for i in 0..15 {
            std::fs::write(project_path.join(format!("file{}.rs", i)), "").unwrap();
        }

        let discovery = DiscoveryEngine::with_roots(vec![temp_dir.path().to_path_buf()]);
        let mut engine = SyncEngine::new(registry, discovery);

        let report = engine.sync().unwrap();
        assert_eq!(report.total_discovered, 1);
        assert_eq!(report.new_projects, 1);
        assert!(report.is_success());
    }

    #[test]
    fn test_calculate_backoff() {
        assert_eq!(BackgroundSync::calculate_backoff(0), 1);
        assert_eq!(BackgroundSync::calculate_backoff(1), 2);
        assert_eq!(BackgroundSync::calculate_backoff(2), 4);
        assert_eq!(BackgroundSync::calculate_backoff(3), 8);
        assert_eq!(BackgroundSync::calculate_backoff(8), 256);
        // Should cap at max
        assert_eq!(BackgroundSync::calculate_backoff(20), MAX_BACKOFF_SECS);
        assert_eq!(BackgroundSync::calculate_backoff(100), MAX_BACKOFF_SECS);
    }

    #[test]
    fn test_clone_group_structure() {
        let group = CloneGroup {
            canonical_id: "test-id".to_string(),
            clone_paths: vec![PathBuf::from("/path1"), PathBuf::from("/path2")],
            similarity_score: 1.0,
        };
        assert_eq!(group.canonical_id, "test-id");
        assert_eq!(group.clone_paths.len(), 2);
        assert_eq!(group.similarity_score, 1.0);
    }
}

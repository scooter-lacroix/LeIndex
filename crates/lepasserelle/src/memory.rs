// Memory-aware resource management

use psutil::{process, process::Process};
use serde::{Deserialize, Serialize};
use std::time::Duration;

/// Memory manager configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryConfig {
    /// RSS threshold for spilling (0.0-1.0)
    pub spill_threshold: f64,

    /// Check interval in seconds
    pub check_interval_secs: u64,

    /// Whether to enable automatic spilling
    pub auto_spill: bool,
}

impl Default for MemoryConfig {
    fn default() -> Self {
        Self {
            spill_threshold: 0.9,
            check_interval_secs: 30,
            auto_spill: true,
        }
    }
}

/// Memory manager for resource-aware operations
pub struct MemoryManager {
    config: MemoryConfig,
    current_process: Process,
}

impl MemoryManager {
    /// Create a new memory manager
    pub fn new(config: MemoryConfig) -> Result<Self, Error> {
        let current_process = Process::current()
            .map_err(|e| Error::ProcessAccess(e.to_string()))?;

        Ok(Self {
            config,
            current_process,
        })
    }

    /// Get current RSS memory in bytes
    pub fn get_rss_bytes(&self) -> Result<usize, Error> {
        self.current_process
            .memory_info()
            .map(|info| info.rss() as usize)
            .map_err(|e| Error::MemoryInfo(e.to_string()))
    }

    /// Get total system memory in bytes
    pub fn get_total_memory(&self) -> Result<usize, Error> {
        psutil::memory::virtual_memory()
            .map(|mem| mem.total() as usize)
            .map_err(|e| Error::MemoryInfo(e.to_string()))
    }

    /// Check if RSS threshold is exceeded
    pub fn is_threshold_exceeded(&self) -> Result<bool, Error> {
        let rss = self.get_rss_bytes()?;
        let total = self.get_total_memory()?;
        let ratio = rss as f64 / total as f64;

        Ok(ratio > self.config.spill_threshold)
    }

    /// Trigger cache spill for non-active projects
    pub fn spill_cache(&mut self) -> Result<SpillResult, Error> {
        let rss_before = self.get_rss_bytes()?;

        // Placeholder - will clear PDG cache during sub-track
        // This is where we'd call into legraphe to clear non-active project caches

        let rss_after = self.get_rss_bytes()?;

        Ok(SpillResult {
            memory_freed: rss_before.saturating_sub(rss_after),
            caches_cleared: vec!["pdg_cache".to_string()],
        })
    }

    /// Spill analysis cache to DuckDB
    pub fn spill_to_duckdb(&mut self) -> Result<(), Error> {
        // Placeholder - will implement during sub-track
        Ok(())
    }

    /// Trigger Python garbage collection
    pub fn trigger_python_gc(&self) -> Result<(), Error> {
        // Placeholder - will call via Python bridge during sub-track
        Ok(())
    }
}

impl Default for MemoryManager {
    fn default() -> Self {
        Self::new(MemoryConfig::default()).unwrap()
    }
}

/// Result of cache spill operation
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SpillResult {
    /// Memory freed in bytes
    pub memory_freed: usize,

    /// Names of caches cleared
    pub caches_cleared: Vec<String>,
}

/// Memory management errors
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("Process access failed: {0}")]
    ProcessAccess(String),

    #[error("Failed to get memory info: {0}")]
    MemoryInfo(String),

    #[error("Spill operation failed: {0}")]
    SpillFailed(String),
}

/// Memory-aware spilling strategy
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpillStrategy {
    /// Clear all caches
    ClearAll,

    /// Clear only non-active project caches
    NonActiveProjects,

    /// Spill to DuckDB instead of clearing
    SpillToDuckDB,
}

/// Memory monitoring for proactive spilling
pub struct MemoryMonitor {
    manager: MemoryManager,
    strategy: SpillStrategy,
}

impl MemoryMonitor {
    /// Create a new memory monitor
    pub fn new(manager: MemoryManager, strategy: SpillStrategy) -> Self {
        Self { manager, strategy }
    }

    /// Check memory and spill if needed
    pub fn check_and_spill(&mut self) -> Result<Option<SpillResult>, Error> {
        if self.manager.is_threshold_exceeded()? {
            match self.strategy {
                SpillStrategy::ClearAll => {
                    Ok(Some(self.manager.spill_cache()?))
                }
                SpillStrategy::NonActiveProjects => {
                    // Placeholder - would track active projects
                    Ok(Some(self.manager.spill_cache()?))
                }
                SpillStrategy::SpillToDuckDB => {
                    self.manager.spill_to_duckdb()?;
                    Ok(None)
                }
            }
        } else {
            Ok(None)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_memory_manager_creation() {
        let manager = MemoryManager::new(MemoryConfig::default());
        assert!(manager.is_ok());
    }

    #[test]
    fn test_memory_info() {
        let manager = MemoryManager::new(MemoryConfig::default()).unwrap();
        let rss = manager.get_rss_bytes();
        assert!(rss.is_ok());
        assert!(rss.unwrap() > 0);
    }

    #[test]
    fn test_spill_config_default() {
        let config = MemoryConfig::default();
        assert_eq!(config.spill_threshold, 0.9);
        assert_eq!(config.check_interval_secs, 30);
        assert!(config.auto_spill);
    }
}

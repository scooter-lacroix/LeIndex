use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};

/// Generic cache envelope for phase summaries.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CachedSummary<T> {
    /// Project identifier.
    pub project_id: String,
    /// Freshness generation hash.
    pub generation: String,
    /// Phase number (1..=5).
    pub phase: u8,
    /// Optional options-sensitive discriminator (phases 3..=5).
    #[serde(default)]
    pub options_hash: Option<String>,
    /// Stored payload.
    pub payload: T,
}

/// Lightweight file-backed cache for phase summaries.
pub struct PhaseCache {
    root: PathBuf,
}

impl PhaseCache {
    /// Create cache rooted at `<project>/.leindex/phase_cache`.
    pub fn new(project_root: &Path) -> Self {
        Self {
            root: project_root.join(".leindex").join("phase_cache"),
        }
    }

    fn path_for(
        &self,
        project_id: &str,
        generation: &str,
        phase: u8,
        options_hash: Option<&str>,
    ) -> PathBuf {
        let suffix = options_hash.map(|h| format!("_{}", h)).unwrap_or_default();
        self.root
            .join(project_id)
            .join(format!("{}_phase{}{}.json", generation, phase, suffix))
    }

    /// Load a cached summary if present.
    pub fn load<T: for<'de> Deserialize<'de>>(
        &self,
        project_id: &str,
        generation: &str,
        phase: u8,
    ) -> Result<Option<CachedSummary<T>>> {
        self.load_with_options(project_id, generation, phase, None)
    }

    /// Load a cached summary with optional options-sensitive key.
    pub fn load_with_options<T: for<'de> Deserialize<'de>>(
        &self,
        project_id: &str,
        generation: &str,
        phase: u8,
        options_hash: Option<&str>,
    ) -> Result<Option<CachedSummary<T>>> {
        let path = self.path_for(project_id, generation, phase, options_hash);
        if !path.exists() {
            return Ok(None);
        }

        let bytes = fs::read(&path)?;
        let cached: CachedSummary<T> = match serde_json::from_slice(&bytes) {
            Ok(value) => value,
            Err(_) => {
                // Corrupted cache entries should degrade gracefully to a cache miss.
                let _ = fs::remove_file(&path);
                return Ok(None);
            }
        };

        if cached.project_id != project_id
            || cached.generation != generation
            || cached.phase != phase
            || cached.options_hash.as_deref() != options_hash
        {
            return Ok(None);
        }

        Ok(Some(cached))
    }

    /// Persist a summary cache entry.
    pub fn save<T: Serialize>(
        &self,
        project_id: &str,
        generation: &str,
        phase: u8,
        payload: &T,
    ) -> Result<()> {
        self.save_with_options(project_id, generation, phase, None, payload)
    }

    /// Persist a summary cache entry with optional options-sensitive key.
    pub fn save_with_options<T: Serialize>(
        &self,
        project_id: &str,
        generation: &str,
        phase: u8,
        options_hash: Option<&str>,
        payload: &T,
    ) -> Result<()> {
        let path = self.path_for(project_id, generation, phase, options_hash);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }

        let envelope = CachedSummary {
            project_id: project_id.to_string(),
            generation: generation.to_string(),
            phase,
            options_hash: options_hash.map(|v| v.to_string()),
            payload,
        };

        fs::write(path, serde_json::to_vec_pretty(&envelope)?)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn cache_corruption_is_treated_as_miss() {
        let dir = tempdir().expect("tempdir");
        let cache = PhaseCache::new(dir.path());
        let path = cache.path_for("proj", "gen", 1, None);
        std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
        std::fs::write(&path, b"{invalid json").expect("write corrupted cache");

        let loaded = cache
            .load::<serde_json::Value>("proj", "gen", 1)
            .expect("load should not fail");
        assert!(loaded.is_none(), "corrupted cache should return miss");
        assert!(!path.exists(), "corrupted cache file should be removed");
    }

    #[test]
    fn cache_envelope_mismatch_is_miss() {
        let dir = tempdir().expect("tempdir");
        let cache = PhaseCache::new(dir.path());
        let path = cache.path_for("proj", "gen", 2, None);
        std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");

        let payload = serde_json::json!({
            "project_id": "other_project",
            "generation": "gen",
            "phase": 2,
            "payload": {"ok": true}
        });
        std::fs::write(&path, serde_json::to_vec(&payload).expect("json")).expect("write");

        let loaded = cache
            .load::<serde_json::Value>("proj", "gen", 2)
            .expect("load mismatch");
        assert!(loaded.is_none());
    }

    #[test]
    fn options_sensitive_cache_uses_hash_discriminator() {
        let dir = tempdir().expect("tempdir");
        let cache = PhaseCache::new(dir.path());

        cache
            .save_with_options(
                "proj",
                "gen",
                3,
                Some("abcd1234"),
                &serde_json::json!({"n":1}),
            )
            .expect("save");

        let hit = cache
            .load_with_options::<serde_json::Value>("proj", "gen", 3, Some("abcd1234"))
            .expect("load hit");
        assert!(hit.is_some());

        let miss = cache
            .load_with_options::<serde_json::Value>("proj", "gen", 3, Some("deadbeef"))
            .expect("load miss");
        assert!(miss.is_none());
    }
}

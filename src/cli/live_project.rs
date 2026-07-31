//! Canonical project paths and storage locations for live MCP reads.

use std::path::{Path, PathBuf};

/// A canonical project identity that can be used without hydrating an index.
#[derive(Debug, Clone)]
pub struct LiveProject {
    root: PathBuf,
    storage: PathBuf,
}

impl LiveProject {
    /// Resolve a project root and its existing storage location without writes.
    pub fn resolve(raw: &str) -> std::io::Result<Self> {
        let root = Path::new(raw).canonicalize()?;
        let storage = crate::cli::leindex::resolve_existing_storage_path(&root)
            .unwrap_or_else(|| root.join(".leindex"));
        Ok(Self { root, storage })
    }

    /// Canonical project root.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Existing (or conventional in-project) storage root.
    pub fn storage(&self) -> &Path {
        &self.storage
    }

    /// Return completed generation storage when `CURRENT` is valid, otherwise
    /// fall back to the legacy root. This read-only selector never creates or
    /// repairs artifacts, so live tools remain safe during an interrupted build.
    pub fn active_storage(&self) -> PathBuf {
        let Ok(raw) = std::fs::read_to_string(self.storage.join("CURRENT")) else {
            return self.storage.clone();
        };
        let value = raw.trim();
        if value.is_empty() || !value.bytes().all(|byte| byte.is_ascii_digit()) {
            return self.storage.clone();
        }
        let generation = self.storage.join("generations").join(value);
        if generation.join("leindex.db").is_file() && generation.join("index-state.json").is_file()
        {
            generation
        } else {
            self.storage.clone()
        }
    }

    /// Resolve a file hint and reject paths that escape the project root.
    pub fn file(&self, raw: &str) -> std::io::Result<PathBuf> {
        let candidate = if Path::new(raw).is_absolute() {
            PathBuf::from(raw)
        } else {
            self.root.join(raw)
        };
        let canonical = candidate.canonicalize()?;
        if canonical.starts_with(&self.root) {
            Ok(canonical)
        } else {
            Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                format!(
                    "{} is outside the project boundary {}",
                    canonical.display(),
                    self.root.display()
                ),
            ))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn active_storage_accepts_only_completed_decimal_generation() {
        let temp = tempfile::tempdir().expect("live project tempdir");
        let storage = temp.path().join(".leindex");
        let generation = storage.join("generations/7");
        std::fs::create_dir_all(&generation).expect("generation directory");
        std::fs::write(storage.join("CURRENT"), b"7\n").expect("current marker");
        std::fs::write(generation.join("leindex.db"), b"db").expect("catalog marker");
        std::fs::write(generation.join("index-state.json"), b"{}").expect("health marker");
        let live = LiveProject {
            root: temp.path().to_path_buf(),
            storage: storage.clone(),
        };
        assert_eq!(live.active_storage(), generation);

        std::fs::write(storage.join("CURRENT"), b"../7\n").expect("unsafe marker");
        assert_eq!(live.active_storage(), storage);
    }
}

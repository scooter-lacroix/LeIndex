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

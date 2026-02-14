//! Unique Project Identification
//!
//! *L'Identifiant Unique* (Unique ID) - Deterministic project identification using BLAKE3 path hashing

use blake3;
use serde::{Deserialize, Serialize};
use std::path::Path;

/// Unique project identifier with conflict resolution via instance counter

/// Unique project identifier with conflict resolution via instance counter
///
/// Each project gets a deterministic ID based on:
/// - `base_name`: The directory name of the project
/// - `path_hash`: First 8 hex characters of BLAKE3 hash of canonical path
/// - `instance`: Counter starting at 0, incremented for path conflicts
///
/// Format: `<base_name>_<path_hash[:8]>_<instance>`
///
/// # Example
///
/// ```
/// use lestockage::UniqueProjectId;
/// use std::path::Path;
///
/// // Original project
/// let path = Path::new("/home/user/projects/leindex");
/// let id = UniqueProjectId::generate(&path, &[]);
/// assert!(id.to_string().starts_with("leindex_"));
/// assert!(id.to_string().ends_with("_0"));
///
/// // Clone at different path (different base_name due to directory name)
/// let clone_path = Path::new("/home/user/projects/leindex-copy");
/// let clone_id = UniqueProjectId::generate(&clone_path, &[id.clone()]);
/// assert!(clone_id.to_string().starts_with("leindex-copy_"));
/// assert!(clone_id.instance == 0); // different base_name means no conflict
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct UniqueProjectId {
    /// Base name extracted from project directory name
    pub base_name: String,

    /// First 8 hex characters of BLAKE3 hash of canonical path
    pub path_hash: String,

    /// Instance counter (0 for original, incremented for clones)
    pub instance: u32,
}

impl UniqueProjectId {
    /// Character length of path hash (BLAKE3 is 256-bit = 64 hex chars, we use first 8)
    const HASH_LEN: usize = 8;

    /// Create a new UniqueProjectId from components
    ///
    /// # Arguments
    ///
    /// * `base_name` - Project directory name
    /// * `path_hash` - First 8 hex chars of BLAKE3 hash
    /// * `instance` - Instance counter (0 for original)
    #[must_use]
    pub fn new(base_name: String, path_hash: String, instance: u32) -> Self {
        Self {
            base_name,
            path_hash,
            instance,
        }
    }

    /// Generate a unique project ID for the given path
    ///
    /// This method:
    /// 1. Extracts base_name from directory name
    /// 2. Computes BLAKE3 hash of canonical path
    /// 3. Checks for conflicts with existing IDs
    /// 4. Returns appropriate instance number
    ///
    /// # Arguments
    ///
    /// * `project_path` - Path to the project directory
    /// * `existing_ids` - List of existing project IDs to check for conflicts
    ///
    /// # Returns
    ///
    /// A new `UniqueProjectId` with appropriate instance number
    ///
    /// # Example
    ///
    /// ```
    /// use lestockage::UniqueProjectId;
    /// use std::path::Path;
    ///
    /// let path = Path::new("/home/user/projects/leindex");
    /// let id = UniqueProjectId::generate(&path, &[]);
    /// assert_eq!(id.instance, 0);
    /// assert_eq!(id.base_name, "leindex");
    /// ```
    #[must_use]
    pub fn generate(project_path: &Path, existing_ids: &[UniqueProjectId]) -> Self {
        // Extract base name from directory
        let base_name = project_path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("unknown")
            .to_string();

        // Get canonical path for hashing
        let canonical_path = project_path
            .canonicalize()
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_else(|_| project_path.to_string_lossy().to_string());

        // Compute BLAKE3 hash
        let path_hash = Self::hash_path(&canonical_path);

        // Check for conflicts (same base_name + path_hash combination)
        // In practice, path_hash should be unique due to canonical path,
        // but we handle edge cases like symlinks to same directory
        let instance = Self::find_next_instance(&base_name, existing_ids);

        Self {
            base_name,
            path_hash,
            instance,
        }
    }

    /// Compute BLAKE3 hash of path and return first 8 hex characters
    ///
    /// # Arguments
    ///
    /// * `path` - Path string to hash
    ///
    /// # Returns
    ///
    /// First 8 hex characters of BLAKE3 hash
    #[must_use]
    fn hash_path(path: &str) -> String {
        let hash = blake3::hash(path.as_bytes());
        hash.to_hex()[..Self::HASH_LEN].to_string()
    }

    /// Find the next available instance number for a given base_name
    ///
    /// Scans existing IDs with same base_name and returns max(instance) + 1
    ///
    /// # Arguments
    ///
    /// * `base_name` - Base name to check for conflicts
    /// * `existing_ids` - List of existing project IDs
    ///
    /// # Returns
    ///
    /// Next available instance number (0 if no conflicts)
    #[must_use]
    fn find_next_instance(base_name: &str, existing_ids: &[UniqueProjectId]) -> u32 {
        existing_ids
            .iter()
            .filter(|id| id.base_name == base_name)
            .map(|id| id.instance)
            .max()
            .map(|max| max + 1)
            .unwrap_or(0)
    }

    /// Convert to full string representation
    ///
    /// Format: `<base_name>_<path_hash[:8]>_<instance>`
    ///
    /// # Example
    ///
    /// ```
    /// use lestockage::UniqueProjectId;
    ///
    /// let id = UniqueProjectId::new(
    ///     "leindex".to_string(),
    ///     "a3f7d9e2".to_string(),
    ///     0
    /// );
    /// assert_eq!(id.to_string(), "leindex_a3f7d9e2_0");
    /// ```
    #[must_use]
    pub fn to_string(&self) -> String {
        format!("{}_{}_{}", self.base_name, self.path_hash, self.instance)
    }

    /// User-friendly display name with clone indicator
    ///
    /// Format:
    /// - Original (instance=0): `<base_name>`
    /// - Clone (instance>0): `<base_name> (clone #<instance>)`
    ///
    /// # Example
    ///
    /// ```
    /// use lestockage::UniqueProjectId;
    ///
    /// let original = UniqueProjectId::new(
    ///     "leindex".to_string(),
    ///     "a3f7d9e2".to_string(),
    ///     0
    /// );
    /// assert_eq!(original.display(), "leindex");
    ///
    /// let clone = UniqueProjectId::new(
    ///     "leindex".to_string(),
    ///     "b4e8f1a3".to_string(),
    ///     1
    /// );
    /// assert_eq!(clone.display(), "leindex (clone #1)");
    /// ```
    #[must_use]
    pub fn display(&self) -> String {
        if self.instance == 0 {
            self.base_name.clone()
        } else {
            format!("{} (clone #{})", self.base_name, self.instance)
        }
    }

    /// Parse a unique project ID from its string representation
    ///
    /// # Arguments
    ///
    /// * `s` - String in format `<base_name>_<path_hash>_<instance>`
    ///
    /// # Returns
    ///
    /// `Option<UniqueProjectId>` - None if format is invalid
    ///
    /// # Example
    ///
    /// ```
    /// use lestockage::UniqueProjectId;
    ///
    /// let id = UniqueProjectId::from_str("leindex_a3f7d9e2_0");
    /// assert!(id.is_some());
    /// assert_eq!(id.unwrap().base_name, "leindex");
    /// ```
    #[must_use]
    pub fn from_str(s: &str) -> Option<Self> {
        let parts: Vec<&str> = s.rsplitn(3, '_').collect();
        if parts.len() != 3 {
            return None;
        }

        let instance = parts[0].parse().ok()?;
        let path_hash = parts[1].to_string();
        let base_name = parts[2].to_string();

        // Validate hash is 8 hex chars
        if path_hash.len() != Self::HASH_LEN || !path_hash.chars().all(|c| c.is_ascii_hexdigit()) {
            return None;
        }

        Some(Self {
            base_name,
            path_hash,
            instance,
        })
    }

    /// Check if this ID represents a clone (instance > 0)
    ///
    /// # Returns
    ///
    /// true if instance > 0, false otherwise
    #[must_use]
    pub fn is_clone(&self) -> bool {
        self.instance > 0
    }

    /// Get the unique project ID string (alias for to_string)
    #[must_use]
    pub fn as_unique_id(&self) -> String {
        self.to_string()
    }
}

impl std::fmt::Display for UniqueProjectId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.to_string())
    }
}

impl From<&UniqueProjectId> for String {
    fn from(id: &UniqueProjectId) -> Self {
        id.to_string()
    }
}

impl From<UniqueProjectId> for String {
    fn from(id: UniqueProjectId) -> Self {
        id.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_generate_creates_valid_id() {
        let path = Path::new("/home/user/projects/leindex");
        let id = UniqueProjectId::generate(path, &[]);

        assert_eq!(id.base_name, "leindex");
        assert_eq!(id.path_hash.len(), 8);
        assert_eq!(id.instance, 0);
    }

    #[test]
    fn test_hash_path_returns_8_chars() {
        let hash = UniqueProjectId::hash_path("/test/path");
        assert_eq!(hash.len(), 8);
        assert!(hash.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn test_hash_path_is_deterministic() {
        let path = "/test/path/to/project";
        let hash1 = UniqueProjectId::hash_path(path);
        let hash2 = UniqueProjectId::hash_path(path);
        assert_eq!(hash1, hash2);
    }

    #[test]
    fn test_hash_path_is_different_for_different_paths() {
        let hash1 = UniqueProjectId::hash_path("/path/one");
        let hash2 = UniqueProjectId::hash_path("/path/two");
        assert_ne!(hash1, hash2);
    }

    #[test]
    fn test_instance_starts_at_zero() {
        let path = Path::new("/home/user/projects/leindex");
        let id = UniqueProjectId::generate(path, &[]);
        assert_eq!(id.instance, 0);
    }

    #[test]
    fn test_instance_increments_for_same_base_name() {
        let path1 = Path::new("/home/user/projects/leindex");
        let id1 = UniqueProjectId::generate(path1, &[]);

        let path2 = Path::new("/different/path/leindex");
        let id2 = UniqueProjectId::generate(path2, &[id1.clone()]);

        assert_eq!(id1.instance, 0);
        assert_eq!(id2.instance, 1);
    }

    #[test]
    fn test_instance_counts_correctly() {
        let base_name = "myproject";

        let id1 = UniqueProjectId::new(base_name.to_string(), "hash1".to_string(), 0);
        let id2 = UniqueProjectId::new(base_name.to_string(), "hash2".to_string(), 1);
        let id3 = UniqueProjectId::new(base_name.to_string(), "hash3".to_string(), 2);

        let next = UniqueProjectId::find_next_instance(base_name, &[id1, id2, id3]);
        assert_eq!(next, 3);
    }

    #[test]
    fn test_to_string_format() {
        let id = UniqueProjectId::new("leindex".to_string(), "a3f7d9e2".to_string(), 0);
        assert_eq!(id.to_string(), "leindex_a3f7d9e2_0");
    }

    #[test]
    fn test_to_string_with_instance() {
        let id = UniqueProjectId::new("leindex".to_string(), "a3f7d9e2".to_string(), 2);
        assert_eq!(id.to_string(), "leindex_a3f7d9e2_2");
    }

    #[test]
    fn test_display_original() {
        let id = UniqueProjectId::new("leindex".to_string(), "a3f7d9e2".to_string(), 0);
        assert_eq!(id.display(), "leindex");
    }

    #[test]
    fn test_display_clone() {
        let id = UniqueProjectId::new("leindex".to_string(), "a3f7d9e2".to_string(), 1);
        assert_eq!(id.display(), "leindex (clone #1)");

        let id2 = UniqueProjectId::new("leindex".to_string(), "a3f7d9e2".to_string(), 5);
        assert_eq!(id2.display(), "leindex (clone #5)");
    }

    #[test]
    fn test_from_str_valid() {
        let id = UniqueProjectId::from_str("leindex_a3f7d9e2_0");
        assert!(id.is_some());
        let id = id.unwrap();
        assert_eq!(id.base_name, "leindex");
        assert_eq!(id.path_hash, "a3f7d9e2");
        assert_eq!(id.instance, 0);
    }

    #[test]
    fn test_from_str_invalid_format() {
        assert!(UniqueProjectId::from_str("invalid").is_none());
        assert!(UniqueProjectId::from_str("only_two_parts").is_none());
    }

    #[test]
    fn test_from_str_invalid_hash() {
        // Wrong hash length
        assert!(UniqueProjectId::from_str("leindex_a3f7_0").is_none());
        // Non-hex characters
        assert!(UniqueProjectId::from_str("leindex_xyzxyz9_0").is_none());
    }

    #[test]
    fn test_from_str_roundtrip() {
        let original = UniqueProjectId::new("myproject".to_string(), "b4e8f1a3".to_string(), 3);
        let s = original.to_string();
        let parsed = UniqueProjectId::from_str(&s).unwrap();
        assert_eq!(parsed, original);
    }

    #[test]
    fn test_is_clone() {
        let original = UniqueProjectId::new("leindex".to_string(), "hash1".to_string(), 0);
        assert!(!original.is_clone());

        let clone = UniqueProjectId::new("leindex".to_string(), "hash2".to_string(), 1);
        assert!(clone.is_clone());
    }

    #[test]
    fn test_as_unique_id() {
        let id = UniqueProjectId::new("test".to_string(), "abcd1234".to_string(), 0);
        assert_eq!(id.as_unique_id(), "test_abcd1234_0");
    }

    #[test]
    fn test_display_trait() {
        let id = UniqueProjectId::new("leindex".to_string(), "a3f7d9e2".to_string(), 0);
        assert_eq!(format!("{}", id), "leindex_a3f7d9e2_0");
    }

    #[test]
    fn test_from_string_trait() {
        let id = UniqueProjectId::new("leindex".to_string(), "a3f7d9e2".to_string(), 0);
        let s: String = (&id).into();
        assert_eq!(s, "leindex_a3f7d9e2_0");

        let s2: String = id.clone().into();
        assert_eq!(s2, "leindex_a3f7d9e2_0");
    }

    #[test]
    fn test_handle_unicode_directory_name() {
        // Test with unicode characters
        let path = Path::new("/home/user/projects/été");
        let id = UniqueProjectId::generate(path, &[]);
        assert_eq!(id.base_name, "été");
        assert_eq!(id.path_hash.len(), 8);
    }

    #[test]
    fn test_handle_special_characters() {
        let path = Path::new("/home/user/projects/my-project_v2.0");
        let id = UniqueProjectId::generate(path, &[]);
        assert_eq!(id.base_name, "my-project_v2.0");
        assert_eq!(id.path_hash.len(), 8);
    }

    #[test]
    fn test_unknown_fallback_for_invalid_path() {
        // Current directory doesn't have a file name
        let path = Path::new(".");
        let id = UniqueProjectId::generate(path, &[]);
        // When there's no file name, should use "unknown" fallback
        // or the path itself if it can be represented
        assert!(!id.base_name.is_empty());
    }

    #[test]
    fn test_serialization() {
        let id = UniqueProjectId::new("leindex".to_string(), "a3f7d9e2".to_string(), 2);
        let json = serde_json::to_string(&id).unwrap();
        let deserialized: UniqueProjectId = serde_json::from_str(&json).unwrap();
        assert_eq!(id, deserialized);
    }

    #[test]
    fn test_generate_with_conflicting_names() {
        let path1 = Path::new("/path/to/project");
        let id1 = UniqueProjectId::generate(path1, &[]);

        let path2 = Path::new("/different/path/project");
        let id2 = UniqueProjectId::generate(path2, &[id1.clone()]);

        let path3 = Path::new("/another/path/project");
        let id3 = UniqueProjectId::generate(path3, &[id1.clone(), id2.clone()]);

        assert_eq!(id1.instance, 0);
        assert_eq!(id2.instance, 1);
        assert_eq!(id3.instance, 2);
    }
}

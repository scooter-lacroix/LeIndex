use anyhow::Result;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

/// Freshness output comparing current inventory to stored file hashes.
#[derive(Debug, Clone, Default)]
pub struct FreshnessState {
    /// Deterministic generation hash for the full current inventory.
    pub generation_hash: String,
    /// Current file inventory (path + hash).
    pub file_inventory: Vec<(PathBuf, String)>,
    /// Files that are new or changed.
    pub changed_files: Vec<PathBuf>,
    /// Files removed since previous run.
    pub deleted_files: Vec<String>,
}

/// Compare current inventory against indexed files and compute generation hash.
pub fn compute_freshness(
    root: &Path,
    inventory: Vec<(PathBuf, String)>,
    indexed_files: &HashMap<String, String>,
) -> Result<FreshnessState> {
    let mut changed_files = Vec::new();

    let inventory_by_normalized = inventory
        .iter()
        .map(|(path, hash)| {
            (
                normalize_key(root, &path.display().to_string()),
                hash.clone(),
            )
        })
        .collect::<HashMap<_, _>>();

    let indexed_by_normalized = indexed_files
        .iter()
        .map(|(path, hash)| (normalize_key(root, path), (path.clone(), hash.clone())))
        .collect::<HashMap<_, _>>();

    let current_set: HashSet<String> = inventory_by_normalized.keys().cloned().collect();

    for (path, hash) in &inventory {
        let normalized = normalize_key(root, &path.display().to_string());
        let unchanged = indexed_by_normalized
            .get(&normalized)
            .map(|(_, indexed_hash)| indexed_hash == hash)
            .unwrap_or(false);

        if !unchanged {
            changed_files.push(path.clone());
        }
    }

    let deleted_files = indexed_by_normalized
        .iter()
        .filter(|(normalized, _)| !current_set.contains(*normalized))
        .map(|(_, (original_key, _))| original_key.clone())
        .collect::<Vec<_>>();

    let generation_hash = generation_from_inventory(root, &inventory);

    Ok(FreshnessState {
        generation_hash,
        file_inventory: inventory,
        changed_files,
        deleted_files,
    })
}

fn normalize_key(root: &Path, key: &str) -> String {
    let path = Path::new(key);

    if path.is_relative() {
        return path.display().to_string();
    }

    if let Ok(relative) = path.strip_prefix(root) {
        return relative.display().to_string();
    }

    if let Ok(absolute) = path.canonicalize() {
        if let Ok(relative) = absolute.strip_prefix(root) {
            return relative.display().to_string();
        }
        return absolute.display().to_string();
    }

    path.display().to_string()
}

fn generation_from_inventory(root: &Path, inventory: &[(PathBuf, String)]) -> String {
    let mut hasher = blake3::Hasher::new();
    for (path, hash) in inventory {
        hasher.update(normalize_key(root, &path.display().to_string()).as_bytes());
        hasher.update(b"\0");
        hasher.update(hash.as_bytes());
        hasher.update(b"\n");
    }
    hasher.finalize().to_hex().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn freshness_normalizes_relative_and_absolute_index_keys() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path();
        let file = root.join("src/lib.rs");
        std::fs::create_dir_all(file.parent().expect("parent")).expect("mkdir");
        std::fs::write(&file, "pub fn x(){}\n").expect("write");

        let hash = blake3::hash(std::fs::read(&file).expect("read").as_slice())
            .to_hex()
            .to_string();

        let mut indexed = HashMap::new();
        indexed.insert("src/lib.rs".to_string(), hash.clone());

        let freshness = compute_freshness(root, vec![(file, hash)], &indexed).expect("freshness");
        assert!(freshness.changed_files.is_empty());
        assert!(freshness.deleted_files.is_empty());
    }
}

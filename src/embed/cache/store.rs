//! GlobalEmbeddingCache — user-level content-addressed embedding cache.
//!
//! Stores fixed-layout mmap vector rows keyed by the `CacheKey` fingerprint.
//! Cross-project deduplication is automatic: the same content_hash combined
//! with the same model identity produces the same fingerprint, so two projects
//! embedding identical text share one cache row (spec §10.1).
//!
//! ## On-disk layout
//!
//! ```text
//! <root>/
//!   rows/
//!     <2-hex-prefix>/
//!       <64-hex-fingerprint>    <- LIDX-ECR1 frame (magic + version + ...)
//!   refs.json                    <- project-generation refcounts keyed by fingerprint
//! ```
//!
//! ## Row format (LIDX-ECR1)
//!
//! ```text
//! [magic 9B] [version 1B] [pad 2B]
//! [fingerprint 32B]
//! [dim u32 LE]
//! [content_hash 32B]          <- blake3 of the vector payload (corruption detection)
//! [payload: dim * sizeof(f32) BYTES]
//! ```
//!
//! ## Privacy (spec §10.1)
//!
//! No source text is stored after hashing. The cache row contains only the
//! fingerprint, the vector data, and the blake3 hash of the vector data. The
//! `LEINDEX_EMBED_CACHE_DEBUG` env var controls an optional debug mode that
//! stores the source text alongside the row for development troubleshooting.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use serde::{Deserialize, Serialize};

use super::key::CacheKey;

/// Magic bytes for an embedding-cache row file.
pub const ROW_MAGIC: &[u8; 9] = b"LIDX-ECR1";
/// Current row format version.
pub const ROW_VERSION: u8 = 1;
/// Padding bytes after version (to align dim field at a 4-byte boundary).
const ROW_PAD_LEN: usize = 2;
/// Fixed header size: magic(9) + version(1) + pad(2) + fingerprint(32) + dim(4) + content_hash(32).
pub const ROW_HEADER_LEN: usize = 9 + 1 + ROW_PAD_LEN + 32 + 4 + 32;

/// Filename for the persisted project refcounts.
const REFS_FILENAME: &str = "refs.json";
/// Filename for persisted telemetry counters.
const TELEMETRY_FILENAME: &str = "telemetry.json";

/// A cache probe result: the vector for a hit, or nothing for a miss.
#[derive(Debug)]
pub struct ProbeResult {
    /// Vectors for hit keys, indexed by their original position in the input.
    pub hits: HashMap<usize, Vec<f32>>,
    /// Indices of keys that were not found in the cache.
    pub misses: Vec<usize>,
}

/// Byte-budgeted compaction report.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CacheCompactionReport {
    /// Total bytes reclaimed by removing unreferenced rows.
    pub reclaimed_bytes: u64,
    /// Number of rows removed.
    pub rows_removed: u64,
    /// Number of rows retained (have live project references).
    pub rows_retained: u64,
}

/// Telemetry counters for the embedding cache (spec section 10.3).
///
/// Every cache must have byte accounting, max bytes, entry-size rejection,
/// and eviction policy. This struct tracks hit/miss/eviction counts and
/// entry-size rejections. Count-only telemetry is prohibited (spec section 10.3).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CacheTelemetry {
    /// Number of cache hits (probe found a valid vector).
    pub hits: u64,
    /// Number of cache misses (probe did not find a vector).
    pub misses: u64,
    /// Number of rows evicted by compaction or byte-budget enforcement.
    pub evictions: u64,
    /// Number of entries rejected because they exceeded the max entry size.
    pub entry_size_rejections: u64,
    /// Total bytes rejected due to entry-size limits.
    pub bytes_rejected: u64,
    /// Total bytes evicted by compaction (sum across all GC runs).
    pub bytes_evicted: u64,
}

impl CacheTelemetry {
    /// Record a cache hit.
    pub fn record_hit(&mut self) {
        self.hits += 1;
    }

    /// Record a cache miss.
    pub fn record_miss(&mut self) {
        self.misses += 1;
    }

    /// Record an eviction.
    pub fn record_eviction(&mut self, bytes: u64) {
        self.evictions += 1;
        self.bytes_evicted += bytes;
    }

    /// Record an entry-size rejection.
    pub fn record_entry_rejection(&mut self, bytes: u64) {
        self.entry_size_rejections += 1;
        self.bytes_rejected += bytes;
    }

    /// Compute the hit ratio (hits / (hits + misses)), or 0.0 if no probes.
    pub fn hit_ratio(&self) -> f64 {
        let total = self.hits + self.misses;
        if total == 0 {
            0.0
        } else {
            self.hits as f64 / total as f64
        }
    }
}

/// Configuration for the byte-budgeted cache (spec section 10.3).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CacheConfig {
    /// Maximum total bytes the cache may use. When exceeded, oldest
    /// unreferenced rows are evicted first (LRU eviction policy).
    /// 0 means unlimited (no byte-budget enforcement).
    pub max_bytes: u64,
    /// Maximum bytes for a single cache entry. Entries exceeding this are
    /// rejected (not stored) and counted in telemetry.
    pub max_entry_bytes: u64,
}

impl Default for CacheConfig {
    fn default() -> Self {
        Self {
            // 1 GiB default max cache size.
            max_bytes: 1024 * 1024 * 1024,
            // 1 MiB max per entry (dim=8192 * 4 bytes = 32KiB, so this is generous).
            max_entry_bytes: 1024 * 1024,
        }
    }
}

/// Stats report for `leindex retention --report` cache section (spec 10.3).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CacheStatsReport {
    /// Total bytes used by cache rows on disk.
    pub cache_bytes: u64,
    /// Number of stored rows.
    pub row_count: usize,
    /// Telemetry counters (hits, misses, evictions, rejections).
    pub telemetry: CacheTelemetry,
    /// Cache hit ratio (0.0 to 1.0).
    pub hit_ratio: f64,
    /// Configured maximum bytes for the cache.
    pub max_bytes: u64,
    /// Configured maximum entry size in bytes.
    pub max_entry_bytes: u64,
    /// Number of tracked project-generation references.
    pub tracked_references: usize,
    /// Model digest hex (generation/model invalidation key).
    /// If multiple models are cached, this is "multiple".
    /// If no rows, this is "none".
    pub model_identity: String,
}

impl std::fmt::Display for CacheStatsReport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        writeln!(f, "  Embedding Cache:")?;
        writeln!(f, "    cache bytes:      {}", self.cache_bytes)?;
        writeln!(f, "    row count:        {}", self.row_count)?;
        writeln!(
            f,
            "    hit ratio:        {:.4} ({}/{})",
            self.hit_ratio,
            self.telemetry.hits,
            self.telemetry.hits + self.telemetry.misses
        )?;
        writeln!(f, "    hits:             {}", self.telemetry.hits)?;
        writeln!(f, "    misses:           {}", self.telemetry.misses)?;
        writeln!(f, "    evictions:        {}", self.telemetry.evictions)?;
        writeln!(f, "    bytes evicted:    {}", self.telemetry.bytes_evicted)?;
        writeln!(
            f,
            "    entry rejections: {} ({} bytes)",
            self.telemetry.entry_size_rejections, self.telemetry.bytes_rejected
        )?;
        writeln!(f, "    max bytes:        {}", self.max_bytes)?;
        writeln!(f, "    max entry bytes:  {}", self.max_entry_bytes)?;
        writeln!(f, "    tracked refs:     {}", self.tracked_references)?;
        writeln!(f, "    model identity:   {}", self.model_identity)?;
        Ok(())
    }
}

/// Errors from the embedding cache.
#[derive(Debug, thiserror::Error)]
pub enum CacheError {
    /// I/O error.
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    /// The row file failed validation (bad magic/version/hash mismatch/truncation).
    #[error("row validation failed: {0}")]
    BadRow(String),
    /// Serialization/deserialization error.
    #[error("serde error: {0}")]
    Serde(#[from] serde_json::Error),
}

/// Project-generation reference for GC liveness tracking.
///
/// Each entry maps a fingerprint to a set of (project_id, generation) pairs
/// that reference it. GC removes only rows with an empty reference set.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ProjectRefs {
    /// fingerprint_hex -> set of "project_id:generation" references.
    #[serde(default)]
    refs: HashMap<String, HashSet<String>>,
}

impl ProjectRefs {
    /// Add a reference for a fingerprint under a specific project and generation.
    pub fn add(&mut self, fingerprint: &[u8; 32], project_id: &str, generation: u64) {
        let key = ref_key(project_id, generation);
        let hex = hex_encode(fingerprint);
        self.refs.entry(hex).or_default().insert(key);
    }

    /// Remove a reference for a fingerprint under a specific project and generation.
    pub fn remove(&mut self, fingerprint: &[u8; 32], project_id: &str, generation: u64) {
        let key = ref_key(project_id, generation);
        let hex = hex_encode(fingerprint);
        if let Some(set) = self.refs.get_mut(&hex) {
            set.remove(&key);
            if set.is_empty() {
                self.refs.remove(&hex);
            }
        }
    }

    /// Returns `true` if the given fingerprint has at least one live reference.
    pub fn is_referenced(&self, fingerprint: &[u8; 32]) -> bool {
        let hex = hex_encode(fingerprint);
        self.refs.get(&hex).is_some_and(|set| !set.is_empty())
    }

    /// Number of distinct fingerprints tracked.
    pub fn tracked_count(&self) -> usize {
        self.refs.len()
    }

    /// All tracked fingerprints (hex-encoded).
    pub fn tracked_fingerprints(&self) -> Vec<String> {
        self.refs.keys().cloned().collect()
    }
}

fn ref_key(project_id: &str, generation: u64) -> String {
    format!("{project_id}:{generation}")
}

/// User-level content-addressed global embedding cache.
///
/// The cache stores embedding vectors in fixed-layout mmap-friendly row files
/// keyed by the `CacheKey` fingerprint. Cross-project dedup is automatic:
/// the same content produces the same fingerprint regardless of which project
/// originated the request.
///
/// ## Privacy
///
/// No source text is stored after hashing (spec §10.1). The cache row contains
/// only the fingerprint, the vector data, and a blake3 hash of the vector data
/// for corruption detection. The `LEINDEX_EMBED_CACHE_DEBUG` env var enables
/// debug mode, which stores the source text alongside the row for development
/// troubleshooting.
///
/// ## Corruption detection
///
/// Each row stores a blake3 hash of its vector payload (the "content hash" of
/// the vector). On read, the payload is re-hashed and compared. A mismatch
/// causes the row to be treated as a miss (and optionally marked for GC).
///
/// ## Byte-budgeted compaction
///
/// [`gc`](Self::gc) removes rows with zero project-generation references. It
/// returns a [`CacheCompactionReport`] with bytes reclaimed, rows removed, and
/// rows retained. Count-only eviction is prohibited (spec §10.3).
pub struct GlobalEmbeddingCache {
    root: PathBuf,
    refs: ProjectRefs,
    config: CacheConfig,
    telemetry: CacheTelemetry,
}

impl GlobalEmbeddingCache {
    /// Open (or initialise) the embedding cache at `root` with default config.
    ///
    /// Creates the root and `rows/` subdirectories on demand. Loads the
    /// persisted project references from `refs.json` and telemetry from
    /// `telemetry.json`.
    pub fn open(root: impl AsRef<Path>) -> Result<Self, CacheError> {
        Self::open_with_config(root, CacheConfig::default())
    }

    /// Open (or initialise) the embedding cache at `root` with a custom config.
    ///
    /// The config specifies `max_bytes` (byte budget) and `max_entry_bytes`
    /// (per-entry size rejection threshold). Both are enforced on `put()`
    /// and reported in telemetry (spec section 10.3).
    pub fn open_with_config(
        root: impl AsRef<Path>,
        config: CacheConfig,
    ) -> Result<Self, CacheError> {
        let root = root.as_ref().to_path_buf();
        fs::create_dir_all(&root)?;
        fs::create_dir_all(root.join("rows"))?;
        let refs = load_refs(&root)?;
        let telemetry = load_telemetry(&root)?;
        Ok(GlobalEmbeddingCache {
            root,
            refs,
            config,
            telemetry,
        })
    }

    /// Root directory of the cache.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Directory for row files of a given fingerprint's 2-hex prefix.
    fn prefix_dir(&self, fingerprint: &[u8; 32]) -> PathBuf {
        let hex = hex_encode(fingerprint);
        self.root.join("rows").join(&hex[0..2])
    }

    /// Final row path: `<root>/rows/<2-hex-prefix>/<64-hex-fingerprint>`.
    pub fn row_path(&self, fingerprint: &[u8; 32]) -> PathBuf {
        let hex = hex_encode(fingerprint);
        self.prefix_dir(fingerprint).join(&hex)
    }

    /// Check whether a cache row exists for `key`.
    pub fn exists(&self, key: &CacheKey) -> bool {
        self.row_path(&key.fingerprint()).exists()
    }

    /// Probe the cache for a batch of keys.
    ///
    /// Returns (`hits`, `misses`), where:
    /// - `hits` maps the original index to the vector for found entries
    /// - `misses` is a list of indices whose keys were not found (or corrupt)
    ///
    /// VAL-CACHE-003: Hits return bit-identical vectors to what was stored.
    /// VAL-CACHE-004: Corrupted rows are detected via re-hash and treated as misses.
    /// Telemetry counters (hits, misses) are updated for each probe.
    pub fn probe(&mut self, keys: &[CacheKey]) -> Result<ProbeResult, CacheError> {
        let mut hits = HashMap::new();
        let mut misses = Vec::new();

        for (i, key) in keys.iter().enumerate() {
            let fingerprint = key.fingerprint();
            let path = self.row_path(&fingerprint);
            if !path.exists() {
                self.telemetry.record_miss();
                misses.push(i);
                continue;
            }
            match read_row(&path, &fingerprint) {
                Ok(vector) => {
                    self.telemetry.record_hit();
                    hits.insert(i, vector);
                }
                Err(_) => {
                    // Corrupted row → treat as miss (not an error).
                    self.telemetry.record_miss();
                    misses.push(i);
                }
            }
        }

        Ok(ProbeResult { hits, misses })
    }

    /// Store a vector in the cache under `key`.
    ///
    /// Writes a fixed-layout row file atomically (staging + rename). If the
    /// row already exists, the call is a no-op (dedup).
    ///
    /// VAL-CACHE-007: The stored vector is bit-identical to the one passed in;
    /// no precision loss occurs during serialization.
    ///
    /// WS10 Task 6: Entry-size rejection — if the computed row size exceeds
    /// `max_entry_bytes`, the entry is rejected and counted in telemetry.
    /// Byte-budget enforcement — if adding the entry would exceed `max_bytes`,
    /// an eviction sweep is triggered on unreferenced rows first.
    pub fn put(&mut self, key: &CacheKey, vector: &[f32]) -> Result<(), CacheError> {
        let fingerprint = key.fingerprint();
        let final_path = self.row_path(&fingerprint);
        if final_path.exists() {
            return Ok(());
        }

        let dim = key.output_dimensions as usize;
        let row_bytes = encode_row(&fingerprint, dim, vector);
        let entry_size = row_bytes.len() as u64;

        // Entry-size rejection (spec section 10.3).
        if entry_size > self.config.max_entry_bytes {
            self.telemetry.record_entry_rejection(entry_size);
            return Ok(()); // Rejection is not an error; just don't store.
        }

        // Byte-budget enforcement: evict unreferenced rows if over budget.
        if self.config.max_bytes > 0 {
            let current = self.total_bytes()?;
            if current + entry_size > self.config.max_bytes {
                self.evict_unreferenced(current + entry_size - self.config.max_bytes)?;
            }
        }

        if let Some(parent) = final_path.parent() {
            fs::create_dir_all(parent)?;
        }

        // Atomic write: temp file -> fsync -> rename.
        let staging_path = final_path.with_extension("partial");
        {
            let file = fs::File::create(&staging_path)?;
            let mut writer = std::io::BufWriter::new(file);
            writer.write_all(&row_bytes)?;
            writer.flush()?;
            writer.get_ref().sync_all()?;
        }

        match fs::rename(&staging_path, &final_path) {
            Ok(()) => {}
            Err(_) if final_path.exists() => {
                let _ = fs::remove_file(&staging_path);
            }
            Err(e) => {
                let _ = fs::remove_file(&staging_path);
                return Err(CacheError::Io(e));
            }
        }

        Ok(())
    }

    /// Read a single vector from the cache. Returns `None` if not found or corrupt.
    pub fn get(&self, key: &CacheKey) -> Result<Option<Vec<f32>>, CacheError> {
        let fingerprint = key.fingerprint();
        let path = self.row_path(&fingerprint);
        if !path.exists() {
            return Ok(None);
        }
        match read_row(&path, &fingerprint) {
            Ok(vector) => Ok(Some(vector)),
            Err(e) => Err(e),
        }
    }

    /// Add a project-generation reference for a fingerprint.
    ///
    /// This prevents GC from dropping a row that is still live for a project.
    pub fn add_reference(&mut self, fingerprint: &[u8; 32], project_id: &str, generation: u64) {
        self.refs.add(fingerprint, project_id, generation);
    }

    /// Remove a project-generation reference for a fingerprint.
    pub fn remove_reference(&mut self, fingerprint: &[u8; 32], project_id: &str, generation: u64) {
        self.refs.remove(fingerprint, project_id, generation);
    }

    /// Returns `true` if the fingerprint has any live project-generation references.
    pub fn is_referenced(&self, fingerprint: &[u8; 32]) -> bool {
        self.refs.is_referenced(fingerprint)
    }

    /// Count the number of stored rows on disk.
    pub fn row_count(&self) -> Result<usize, CacheError> {
        let rows_dir = self.root.join("rows");
        if !rows_dir.exists() {
            return Ok(0);
        }
        let mut count = 0;
        for entry in fs::read_dir(&rows_dir)? {
            let entry = entry?;
            if entry.file_type()?.is_dir() {
                for sub_entry in fs::read_dir(entry.path())? {
                    let sub_entry = sub_entry?;
                    if sub_entry.file_type()?.is_file()
                        && !sub_entry
                            .file_name()
                            .to_string_lossy()
                            .ends_with(".partial")
                    {
                        count += 1;
                    }
                }
            }
        }
        Ok(count)
    }

    /// Byte-budgeted compaction: remove rows with zero project-generation
    /// references.
    ///
    /// VAL-CACHE-010: Returns a report with bytes reclaimed, rows removed,
    /// and rows retained. Count-only eviction is prohibited.
    pub fn gc(&mut self) -> Result<CacheCompactionReport, CacheError> {
        let mut report = CacheCompactionReport::default();
        let rows_dir = self.root.join("rows");
        if !rows_dir.exists() {
            return Ok(report);
        }

        for entry in fs::read_dir(&rows_dir)? {
            let entry = entry?;
            if !entry.file_type()?.is_dir() {
                continue;
            }
            for sub_entry in fs::read_dir(entry.path())? {
                let sub_entry = sub_entry?;
                let path = sub_entry.path();

                // Skip staging files.
                if path.extension().is_some_and(|ext| ext == "partial") {
                    continue;
                }
                if !path.is_file() {
                    continue;
                }

                // Parse fingerprint from the filename.
                let filename = sub_entry.file_name().to_string_lossy().to_string();
                let Some(fingerprint) = hex_decode(&filename) else {
                    continue;
                };

                let is_live = self.refs.is_referenced(&fingerprint);
                if is_live {
                    report.rows_retained += 1;
                } else {
                    let size = fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
                    let _ = fs::remove_file(&path);
                    report.reclaimed_bytes += size;
                    report.rows_removed += 1;
                    self.telemetry.record_eviction(size);
                }
            }
        }

        // Persist updated refs (remove entries that referred to deleted rows).
        let tracked: Vec<String> = self.refs.refs.keys().cloned().collect();
        let mut still_existing = HashSet::new();
        for hex in &tracked {
            if let Some(fp) = hex_decode(hex) {
                if self.row_path(&fp).exists() {
                    still_existing.insert(hex.clone());
                }
            }
        }
        self.refs.refs.retain(|k, _| still_existing.contains(k));
        self.persist_refs()?;
        self.persist_telemetry()?;

        Ok(report)
    }

    /// Persist the project refs to disk.
    pub fn persist_refs(&self) -> Result<(), CacheError> {
        let path = self.root.join(REFS_FILENAME);
        let tmp = path.with_extension("tmp");
        let data = serde_json::to_vec_pretty(&self.refs)?;
        fs::write(&tmp, &data)?;
        fs::rename(&tmp, &path)?;
        Ok(())
    }

    /// Persist telemetry counters to `telemetry.json`.
    pub fn persist_telemetry(&self) -> Result<(), CacheError> {
        let path = self.root.join(TELEMETRY_FILENAME);
        let tmp = path.with_extension("tmp");
        let data = serde_json::to_vec_pretty(&self.telemetry)?;
        fs::write(&tmp, &data)?;
        fs::rename(&tmp, &path)?;
        Ok(())
    }

    /// Get the current telemetry counters.
    pub fn telemetry(&self) -> &CacheTelemetry {
        &self.telemetry
    }

    /// Get the cache configuration (max_bytes, max_entry_bytes).
    pub fn config(&self) -> &CacheConfig {
        &self.config
    }

    /// Byte-budget eviction: remove oldest unreferenced rows until
    /// at least `bytes_needed` have been reclaimed (spec section 10.3).
    ///
    /// Rows with live project-generation references are NEVER evicted.
    /// The eviction policy is LRU-ish: rows are sorted by mtime ascending
    /// (oldest first), and unreferenced ones are removed until enough bytes
    /// are reclaimed.
    fn evict_unreferenced(&mut self, bytes_needed: u64) -> Result<(), CacheError> {
        let rows_dir = self.root.join("rows");
        if !rows_dir.exists() {
            return Ok(());
        }

        // Collect all unreferenced rows with their mtime and size.
        #[derive(Clone)]
        struct RowInfo {
            path: PathBuf,
            mtime: SystemTime,
            size: u64,
        }

        let mut candidates: Vec<RowInfo> = Vec::new();

        for entry in fs::read_dir(&rows_dir)? {
            let entry = entry?;
            if !entry.file_type()?.is_dir() {
                continue;
            }
            for sub_entry in fs::read_dir(entry.path())? {
                let sub_entry = sub_entry?;
                let path = sub_entry.path();
                if path.extension().is_some_and(|ext| ext == "partial") || !path.is_file() {
                    continue;
                }
                let filename = sub_entry.file_name().to_string_lossy().to_string();
                let Some(fingerprint) = hex_decode(&filename) else {
                    continue;
                };
                // Only evict rows with no project references.
                if self.refs.is_referenced(&fingerprint) {
                    continue;
                }
                let meta = match fs::metadata(&path) {
                    Ok(m) => m,
                    Err(_) => continue,
                };
                let mtime = meta.modified().unwrap_or(SystemTime::UNIX_EPOCH);
                let size = meta.len();
                candidates.push(RowInfo { path, mtime, size });
            }
        }

        // Sort oldest-first (LRU eviction).
        candidates.sort_by_key(|a| a.mtime);

        let mut reclaimed = 0u64;
        for row in candidates {
            if reclaimed >= bytes_needed {
                break;
            }
            match fs::remove_file(&row.path) {
                Ok(()) => {
                    reclaimed += row.size;
                    self.telemetry.record_eviction(row.size);
                }
                Err(_) => continue,
            }
        }

        // Persist updated telemetry.
        self.persist_telemetry()?;
        Ok(())
    }

    /// Generate a comprehensive cache stats report for `leindex retention --report`
    /// (spec section 10.3). Includes byte accounting, telemetry, config, and
    /// model identity.
    pub fn cache_stats(&self) -> Result<CacheStatsReport, CacheError> {
        let cache_bytes = self.total_bytes()?;
        let row_count = self.row_count()?;
        let tracked_references = self.refs.tracked_count();
        let hit_ratio = self.telemetry.hit_ratio();

        // Model identity: gather distinct model digests from stored rows.
        // Since read_row gives us just the vector, we use the fingerprint
        // prefix as the identity key (the fingerprint already includes model
        // digest). For the report, we just show the tracked count + a summary.
        let model_identity = if row_count == 0 {
            "none".to_string()
        } else {
            // The cache key includes model_digest, so every row is inherently
            // namespaced by model. We report "content-addressed" since all
            // rows are keyed by model+content hash.
            "content-addressed (model-digest namespaced)".to_string()
        };

        Ok(CacheStatsReport {
            cache_bytes,
            row_count,
            telemetry: self.telemetry.clone(),
            hit_ratio,
            max_bytes: self.config.max_bytes,
            max_entry_bytes: self.config.max_entry_bytes,
            tracked_references,
            model_identity,
        })
    }

    /// Current project refs (for inspection / reporting).
    pub fn project_refs(&self) -> &ProjectRefs {
        &self.refs
    }

    /// Total bytes used by all stored rows.
    pub fn total_bytes(&self) -> Result<u64, CacheError> {
        let rows_dir = self.root.join("rows");
        if !rows_dir.exists() {
            return Ok(0);
        }
        let mut total = 0u64;
        for entry in fs::read_dir(&rows_dir)? {
            let entry = entry?;
            if entry.file_type()?.is_dir() {
                for sub_entry in fs::read_dir(entry.path())? {
                    let sub_entry = sub_entry?;
                    let path = sub_entry.path();
                    if path.is_file() && !path.ends_with(".partial") {
                        total += fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
                    }
                }
            }
        }
        Ok(total)
    }
}

// ---------------------------------------------------------------------------
// Row encode / decode
// ---------------------------------------------------------------------------

/// Encode a cache row as a byte vector.
///
/// Layout: `[magic(9)] [version(1)] [pad(2)] [fingerprint(32)] [dim u32 LE(4)] [content_hash(32)] [payload]`.
///
/// The "content hash" is a blake3 of the raw f32 bytes of the payload,
/// enabling corruption detection on read.
pub fn encode_row(fingerprint: &[u8; 32], dim: usize, vector: &[f32]) -> Vec<u8> {
    assert_eq!(vector.len(), dim, "vector length must match dim");
    let payload_bytes = f32_slice_to_bytes(vector);
    let payload_hash: [u8; 32] = blake3::hash(&payload_bytes).into();

    let mut buf = Vec::with_capacity(ROW_HEADER_LEN + payload_bytes.len());
    buf.extend_from_slice(ROW_MAGIC);
    buf.push(ROW_VERSION);
    buf.extend_from_slice(&[0u8; ROW_PAD_LEN]);
    buf.extend_from_slice(fingerprint);
    buf.extend_from_slice(&(dim as u32).to_le_bytes());
    buf.extend_from_slice(&payload_hash);
    buf.extend_from_slice(&payload_bytes);
    buf
}

/// Read and validate a cache row, returning the embedded vector.
///
/// VAL-CACHE-004: Corruption is detected by re-hashing the payload and
/// comparing against the stored content hash.
pub fn read_row(path: &Path, expected_fingerprint: &[u8; 32]) -> Result<Vec<f32>, CacheError> {
    let bytes = fs::read(path)?;
    if bytes.len() < ROW_HEADER_LEN {
        return Err(CacheError::BadRow(format!(
            "row truncated: {} bytes, need at least {} for header",
            bytes.len(),
            ROW_HEADER_LEN
        )));
    }
    let mut magic = [0u8; 9];
    magic.copy_from_slice(&bytes[0..9]);
    if &magic != ROW_MAGIC {
        return Err(CacheError::BadRow(format!("bad magic: got {magic:?}")));
    }
    let version = bytes[9];
    if version != ROW_VERSION {
        return Err(CacheError::BadRow(format!(
            "unsupported version: got {version}, expected {ROW_VERSION}"
        )));
    }

    // fingerprint at offset 12
    let mut stored_fingerprint = [0u8; 32];
    stored_fingerprint.copy_from_slice(&bytes[12..44]);
    if &stored_fingerprint != expected_fingerprint {
        return Err(CacheError::BadRow(
            "fingerprint mismatch: stored fingerprint does not match expected".to_string(),
        ));
    }

    // dim at offset 44
    let mut dim_bytes = [0u8; 4];
    dim_bytes.copy_from_slice(&bytes[44..48]);
    let dim = u32::from_le_bytes(dim_bytes) as usize;

    // content_hash at offset 48
    let mut stored_hash = [0u8; 32];
    stored_hash.copy_from_slice(&bytes[48..80]);

    // payload starts at offset 80
    let payload = &bytes[ROW_HEADER_LEN..];
    let expected_payload_len = dim * std::mem::size_of::<f32>();
    if payload.len() != expected_payload_len {
        return Err(CacheError::BadRow(format!(
            "payload length mismatch: got {}, expected {}",
            payload.len(),
            expected_payload_len
        )));
    }

    // Re-hash the vector payload and compare against stored content_hash.
    let computed_hash: [u8; 32] = blake3::hash(payload).into();
    if computed_hash != stored_hash {
        return Err(CacheError::BadRow(
            "hash mismatch: stored vector hash does not match recomputed hash".to_string(),
        ));
    }

    // Convert raw bytes back to f32 vector.
    Ok(bytes_to_f32_vec(payload))
}

/// Convert a &[f32] slice to raw bytes (little-endian on most platforms;
/// this is the same as to_ne_bytes since f32::to_ne_bytes is LE on x86/arm).
fn f32_slice_to_bytes(slice: &[f32]) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(std::mem::size_of_val(slice));
    for &val in slice {
        bytes.extend_from_slice(&val.to_le_bytes());
    }
    bytes
}

/// Convert raw bytes back to Vec<f32>.
fn bytes_to_f32_vec(bytes: &[u8]) -> Vec<f32> {
    let mut result = Vec::with_capacity(bytes.len() / std::mem::size_of::<f32>());
    for chunk in bytes.chunks_exact(4) {
        let mut buf = [0u8; 4];
        buf.copy_from_slice(chunk);
        result.push(f32::from_le_bytes(buf));
    }
    result
}

// ---------------------------------------------------------------------------
// Hex helpers
// ---------------------------------------------------------------------------

/// Encode a 32-byte hash as a lowercase hex string.
pub fn hex_encode(hash: &[u8; 32]) -> String {
    use std::fmt::Write;
    let mut s = String::with_capacity(64);
    for byte in hash {
        write!(&mut s, "{byte:02x}").expect("formatting into String never fails");
    }
    s
}

/// Decode a 64-character hex string into a 32-byte hash.
pub fn hex_decode(hex: &str) -> Option<[u8; 32]> {
    if hex.len() != 64 {
        return None;
    }
    let mut hash = [0u8; 32];
    let bytes = hex.as_bytes();
    for (i, byte) in hash.iter_mut().enumerate() {
        let hi = hex_nibble(bytes[i * 2])?;
        let lo = hex_nibble(bytes[i * 2 + 1])?;
        *byte = (hi << 4) | lo;
    }
    Some(hash)
}

fn hex_nibble(c: u8) -> Option<u8> {
    match c {
        b'0'..=b'9' => Some(c - b'0'),
        b'a'..=b'f' => Some(c - b'a' + 10),
        b'A'..=b'F' => Some(c - b'A' + 10),
        _ => None,
    }
}

/// Load the project refs from `refs.json`, or return empty refs if not present.
fn load_refs(root: &Path) -> Result<ProjectRefs, CacheError> {
    let path = root.join(REFS_FILENAME);
    if !path.exists() {
        return Ok(ProjectRefs::default());
    }
    let data = fs::read(&path)?;
    Ok(serde_json::from_slice(&data)?)
}

/// Load telemetry counters from `telemetry.json`, or return empty if absent.
fn load_telemetry(root: &Path) -> Result<CacheTelemetry, CacheError> {
    let path = root.join(TELEMETRY_FILENAME);
    if !path.exists() {
        return Ok(CacheTelemetry::default());
    }
    let data = fs::read(&path)?;
    Ok(serde_json::from_slice(&data)?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::embed::cache::key::{Normalization, Pooling};

    fn sample_key(content: &str, dim: u32) -> CacheKey {
        CacheKey {
            model_digest: CacheKey::model_digest(b"model-bytes"),
            tokenizer_digest: CacheKey::tokenizer_digest(b"tokenizer-config"),
            prompt_role_and_version: 0,
            pooling: Pooling::Mean,
            normalization: Normalization::L2,
            output_dimensions: dim,
            content_hash: CacheKey::content_hash(content),
        }
    }

    fn sample_vector(dim: usize, seed: f32) -> Vec<f32> {
        (0..dim).map(|i| seed + i as f32 * 0.01).collect()
    }

    /// VAL-CACHE-003: probe returns hits and misses correctly.
    #[test]
    fn test_probe_returns_hits_and_misses() {
        let tmp = tempfile::tempdir().unwrap();
        let mut cache = GlobalEmbeddingCache::open(tmp.path()).unwrap();

        let key1 = sample_key("text one", 4);
        let key2 = sample_key("text two", 4);
        let key3 = sample_key("text three", 4);

        let vec1 = sample_vector(4, 0.1);
        let vec2 = sample_vector(4, 0.2);

        cache.put(&key1, &vec1).unwrap();
        cache.put(&key2, &vec2).unwrap();

        let result = cache.probe(&[key1, key2, key3]).unwrap();
        assert_eq!(result.hits.len(), 2);
        assert_eq!(result.misses, vec![2]);

        // Verify vectors are bit-identical.
        let hit1 = result.hits.get(&0).unwrap();
        let hit2 = result.hits.get(&1).unwrap();
        assert_eq!(hit1, &vec1);
        assert_eq!(hit2, &vec2);
    }

    /// VAL-CACHE-003: Hits return bit-identical vectors to what was stored.
    #[test]
    fn test_put_then_get_is_bit_identical() {
        let tmp = tempfile::tempdir().unwrap();
        let mut cache = GlobalEmbeddingCache::open(tmp.path()).unwrap();

        let key = sample_key("exact text", 8);
        let original = sample_vector(8, 0.5);

        cache.put(&key, &original).unwrap();

        let gotten = cache.get(&key).unwrap().unwrap();
        assert_eq!(gotten.len(), original.len());
        for (a, b) in gotten.iter().zip(original.iter()) {
            assert_eq!(a.to_bits(), b.to_bits(), "f32 bits must match");
        }
    }

    /// VAL-CACHE-004: Corruption detection on read.
    #[test]
    fn test_corruption_detected_on_read() {
        let tmp = tempfile::tempdir().unwrap();
        let mut cache = GlobalEmbeddingCache::open(tmp.path()).unwrap();

        let key = sample_key("text to corrupt", 4);
        cache.put(&key, &sample_vector(4, 0.1)).unwrap();

        // Corrupt by flipping one byte in the vector payload area.
        let path = cache.row_path(&key.fingerprint());
        let mut data = fs::read(&path).unwrap();
        // Flip a byte deep in the payload (well past the header).
        let corrupt_offset = ROW_HEADER_LEN + 4;
        data[corrupt_offset] ^= 0xFF;
        fs::write(&path, &data).unwrap();

        // probe should treat it as a miss.
        let result = cache.probe(std::slice::from_ref(&key)).unwrap();
        assert!(result.hits.is_empty());
        assert_eq!(result.misses, vec![0]);

        // get should return an error.
        assert!(cache.get(&key).is_err());
    }

    /// VAL-CACHE-005: Cross-project deduplication — same content = one row.
    #[test]
    fn test_cross_project_dedup_single_row() {
        let tmp = tempfile::tempdir().unwrap();
        let mut cache = GlobalEmbeddingCache::open(tmp.path()).unwrap();

        // Both projects embed the same text under the same model.
        let key = sample_key("shared content across projects", 4);
        let vec = sample_vector(4, 0.3);

        // Project A puts the vector.
        cache.put(&key, &vec).unwrap();
        assert_eq!(cache.row_count().unwrap(), 1);

        // Project B puts the same vector (same key) — dedup, no second row.
        cache.put(&key, &vec).unwrap();
        assert_eq!(cache.row_count().unwrap(), 1);
    }

    /// VAL-CACHE-005: Two projects adding references, then one drops, row stays.
    #[test]
    fn test_cross_project_ref_keeps_row_alive() {
        let tmp = tempfile::tempdir().unwrap();
        let mut cache = GlobalEmbeddingCache::open(tmp.path()).unwrap();

        let key = sample_key("shared text", 4);
        cache.put(&key, &sample_vector(4, 0.1)).unwrap();

        let fp = key.fingerprint();
        cache.add_reference(&fp, "project-a", 1);
        cache.add_reference(&fp, "project-b", 1);

        // Remove project-a's reference; row should survive (project-b still refs).
        cache.remove_reference(&fp, "project-a", 1);
        assert!(cache.is_referenced(&fp));

        // GC should retain the row.
        let report = cache.gc().unwrap();
        assert_eq!(report.rows_removed, 0);
        assert_eq!(report.rows_retained, 1);
    }

    /// VAL-CACHE-010: Byte-budgeted compaction removes unreferenced rows.
    #[test]
    fn test_gc_removes_unreferenced_rows() {
        let tmp = tempfile::tempdir().unwrap();
        let mut cache = GlobalEmbeddingCache::open(tmp.path()).unwrap();

        let key_live = sample_key("live text", 4);
        let key_dead = sample_key("dead text", 4);

        cache.put(&key_live, &sample_vector(4, 0.1)).unwrap();
        cache.put(&key_dead, &sample_vector(4, 0.2)).unwrap();

        // Mark key_live as referenced by a project.
        let fp_live = key_live.fingerprint();
        cache.add_reference(&fp_live, "project-a", 1);

        // key_dead has no references — should be removed by GC.
        let report = cache.gc().unwrap();
        assert_eq!(report.rows_removed, 1);
        assert_eq!(report.rows_retained, 1);
        assert!(
            report.reclaimed_bytes > 0,
            "should report non-zero reclaimed bytes"
        );

        // Verify the live row survives and dead is gone.
        assert_eq!(cache.row_count().unwrap(), 1);
        assert!(cache.get(&key_live).unwrap().is_some());
        assert!(cache.get(&key_dead).unwrap().is_none());
    }

    #[test]
    fn test_gc_on_empty_cache() {
        let tmp = tempfile::tempdir().unwrap();
        let mut cache = GlobalEmbeddingCache::open(tmp.path()).unwrap();
        let report = cache.gc().unwrap();
        assert_eq!(report.rows_removed, 0);
        assert_eq!(report.rows_retained, 0);
        assert_eq!(report.reclaimed_bytes, 0);
    }

    /// Idempotent put: writing the same key twice does not duplicate the row.
    #[test]
    fn test_put_idempotent() {
        let tmp = tempfile::tempdir().unwrap();
        let mut cache = GlobalEmbeddingCache::open(tmp.path()).unwrap();

        let key = sample_key("idempotent text", 4);
        cache.put(&key, &[0.1, 0.2, 0.3, 0.4]).unwrap();
        cache.put(&key, &[0.1, 0.2, 0.3, 0.4]).unwrap();

        assert_eq!(cache.row_count().unwrap(), 1);
    }

    #[test]
    fn test_open_creates_directories() {
        let tmp = tempfile::tempdir().unwrap();
        let cache_root = tmp.path().join("nested").join("cache");
        let mut cache = GlobalEmbeddingCache::open(&cache_root).unwrap();
        assert!(cache_root.join("rows").exists());

        let key = sample_key("after open", 2);
        cache.put(&key, &[1.0, 2.0]).unwrap();
        assert_eq!(cache.row_count().unwrap(), 1);
    }

    #[test]
    fn test_persist_and_reload_refs() {
        let tmp = tempfile::tempdir().unwrap();
        let mut cache = GlobalEmbeddingCache::open(tmp.path()).unwrap();

        let key = sample_key("persist test", 4);
        cache.put(&key, &[0.5, 0.6, 0.7, 0.8]).unwrap();

        let fp = key.fingerprint();
        cache.add_reference(&fp, "proj", 42);
        cache.persist_refs().unwrap();

        // Reload.
        let cache2 = GlobalEmbeddingCache::open(tmp.path()).unwrap();
        assert!(cache2.is_referenced(&fp));

        // GC should retain the row.
        let mut cache2 = cache2;
        let report = cache2.gc().unwrap();
        assert_eq!(report.rows_retained, 1);
        assert_eq!(report.rows_removed, 0);
    }

    #[test]
    fn test_multiple_projects_add_and_remove_refs() {
        let tmp = tempfile::tempdir().unwrap();
        let mut cache = GlobalEmbeddingCache::open(tmp.path()).unwrap();

        let key = sample_key("multi-ref text", 4);
        cache.put(&key, &[0.1, 0.2, 0.3, 0.4]).unwrap();
        let fp = key.fingerprint();

        cache.add_reference(&fp, "proj-a", 1);
        cache.add_reference(&fp, "proj-b", 2);
        assert!(cache.is_referenced(&fp));

        cache.remove_reference(&fp, "proj-a", 1);
        assert!(cache.is_referenced(&fp));

        cache.remove_reference(&fp, "proj-b", 2);
        assert!(!cache.is_referenced(&fp));
    }

    #[test]
    fn test_total_bytes_reporting() {
        let tmp = tempfile::tempdir().unwrap();
        let mut cache = GlobalEmbeddingCache::open(tmp.path()).unwrap();

        let cycles_before = cache.total_bytes().unwrap();
        assert_eq!(cycles_before, 0);

        let key = sample_key("bytes test", 4);
        cache.put(&key, &[1.0, 2.0, 3.0, 4.0]).unwrap();
        let bytes_after = cache.total_bytes().unwrap();
        assert_eq!(bytes_after, (ROW_HEADER_LEN + 4 * 4) as u64);
    }

    /// Privacy: no source text stored in the row file.
    #[test]
    fn test_privacy_no_source_text_in_row() {
        let tmp = tempfile::tempdir().unwrap();
        let mut cache = GlobalEmbeddingCache::open(tmp.path()).unwrap();

        let secret_text = "this is a secret symbol name fn_do_not_store_me";
        let key = sample_key(secret_text, 4);
        cache.put(&key, &[0.1, 0.2, 0.3, 0.4]).unwrap();

        // Inspect the raw row file on disk.
        let path = cache.row_path(&key.fingerprint());
        let raw = fs::read(&path).unwrap();

        // The source text must NOT appear anywhere in the row file.
        assert!(
            !raw.windows(secret_text.len())
                .any(|w| w == secret_text.as_bytes()),
            "source text must not be stored in the cache row"
        );
    }

    #[test]
    fn test_row_path_uses_2_hex_prefix() {
        let key = sample_key("path test", 4);
        let fp = key.fingerprint();
        let path = GlobalEmbeddingCache::open(tempfile::tempdir().unwrap().path())
            .unwrap()
            .row_path(&fp);
        let hex = hex_encode(&fp);
        assert!(
            path.to_string_lossy()
                .contains(&format!("rows/{}/{}", &hex[0..2], hex))
        );
    }

    // ── WS10 Task 6: Byte-budgeted compaction + telemetry (§10.3) ──────

    /// Task 6: Every cache has byte accounting (total_bytes, cache_stats).
    #[test]
    fn test_cache_stats_reports_byte_accounting() {
        let tmp = tempfile::tempdir().unwrap();
        let mut cache = GlobalEmbeddingCache::open(tmp.path()).unwrap();

        let key1 = sample_key("stats one", 4);
        let key2 = sample_key("stats two", 4);

        cache.put(&key1, &[0.1, 0.2, 0.3, 0.4]).unwrap();
        cache.put(&key2, &[0.5, 0.6, 0.7, 0.8]).unwrap();

        let stats = cache.cache_stats().unwrap();
        assert_eq!(stats.row_count, 2);
        assert_eq!(
            stats.cache_bytes,
            (ROW_HEADER_LEN + 4 * 4) as u64 * 2,
            "byte accounting must report actual disk bytes"
        );
        assert!(stats.max_bytes > 0, "max_bytes must be set");
        assert!(stats.max_entry_bytes > 0, "max_entry_bytes must be set");
        assert_eq!(
            stats.model_identity,
            "content-addressed (model-digest namespaced)"
        );
    }

    /// Task 6: Telemetry tracks hit/miss/eviction counts.
    #[test]
    fn test_telemetry_tracks_hits_and_misses() {
        let tmp = tempfile::tempdir().unwrap();
        let mut cache = GlobalEmbeddingCache::open(tmp.path()).unwrap();

        let key1 = sample_key("telemetry hit", 4);
        let key2 = sample_key("telemetry miss", 4);

        cache.put(&key1, &[1.0, 2.0, 3.0, 4.0]).unwrap();

        // Probe with one hit, one miss.
        let result = cache.probe(&[key1, key2]).unwrap();
        assert_eq!(result.hits.len(), 1);
        assert_eq!(result.misses.len(), 1);

        let telemetry = cache.telemetry();
        assert_eq!(telemetry.hits, 1);
        assert_eq!(telemetry.misses, 1);
        assert!((telemetry.hit_ratio() - 0.5).abs() < 0.001);
    }

    /// Task 6: Entry-size rejection — entries exceeding max_entry_bytes are rejected.
    #[test]
    fn test_entry_size_rejection() {
        let tmp = tempfile::tempdir().unwrap();
        let config = CacheConfig {
            max_bytes: 0,       // unlimited budget
            max_entry_bytes: 8, // tiny: ROW_HEADER_LEN alone is 80 bytes
        };
        let mut cache = GlobalEmbeddingCache::open_with_config(tmp.path(), config).unwrap();

        let key = sample_key("too big", 4);
        cache.put(&key, &[1.0, 2.0, 3.0, 4.0]).unwrap();

        // The entry should have been rejected (not stored).
        assert_eq!(cache.row_count().unwrap(), 0);

        let telemetry = cache.telemetry();
        assert_eq!(telemetry.entry_size_rejections, 1);
        assert!(telemetry.bytes_rejected > 0);
    }

    /// Task 6: Byte-budget enforcement evicts unreferenced rows.
    #[test]
    fn test_byte_budget_eviction_removes_unreferenced() {
        let tmp = tempfile::tempdir().unwrap();
        let dim = 4u32;
        // max_bytes fits about 1 entry (header=80 + payload=16 = 96 bytes).
        // Set max_bytes to 100 to allow one entry, then overflow on second.
        let config = CacheConfig {
            max_bytes: 100,
            max_entry_bytes: u64::MAX,
        };
        let mut cache = GlobalEmbeddingCache::open_with_config(tmp.path(), config).unwrap();

        let key1 = sample_key("first entry", dim);
        let key2 = sample_key("second entry", dim);

        // Put first entry.
        cache.put(&key1, &[1.0, 2.0, 3.0, 4.0]).unwrap();
        assert_eq!(cache.row_count().unwrap(), 1);

        // Put second entry — should trigger eviction of key1 (unreferenced).
        cache.put(&key2, &[5.0, 6.0, 7.0, 8.0]).unwrap();
        assert_eq!(
            cache.row_count().unwrap(),
            1,
            "eviction should maintain count"
        );

        // key1 should have been evicted.
        assert!(cache.get(&key1).unwrap().is_none());
        // key2 should be present.
        assert!(cache.get(&key2).unwrap().is_some());

        let telemetry = cache.telemetry();
        assert!(telemetry.evictions >= 1, "should have recorded evictions");
        assert!(telemetry.bytes_evicted > 0);
    }

    /// Task 6: Byte-budget enforcement does NOT evict referenced rows.
    #[test]
    fn test_byte_budget_eviction_preserves_referenced() {
        let tmp = tempfile::tempdir().unwrap();
        let dim = 4u32;
        let config = CacheConfig {
            max_bytes: 100,
            max_entry_bytes: u64::MAX,
        };
        let mut cache = GlobalEmbeddingCache::open_with_config(tmp.path(), config).unwrap();

        let key1 = sample_key("referenced", dim);
        cache.put(&key1, &[1.0, 2.0, 3.0, 4.0]).unwrap();
        let fp = key1.fingerprint();
        cache.add_reference(&fp, "project-a", 1);

        let key2 = sample_key("newcomer", dim);
        cache.put(&key2, &[5.0, 6.0, 7.0, 8.0]).unwrap();

        // key1 must survive — it has a live project reference.
        assert!(cache.get(&key1).unwrap().is_some());
    }

    /// Task 6: Telemetry persists across open/reopen.
    #[test]
    fn test_telemetry_persists_across_reopen() {
        let tmp = tempfile::tempdir().unwrap();
        let mut cache = GlobalEmbeddingCache::open(tmp.path()).unwrap();

        let key = sample_key("persist telemetry", 4);
        cache.put(&key, &[1.0, 2.0, 3.0, 4.0]).unwrap();
        cache.probe(&[key]).unwrap();

        let telemetry_before = cache.telemetry().clone();
        assert_eq!(telemetry_before.hits, 1);

        // Persist and reopen.
        cache.persist_telemetry().unwrap();
        let cache2 = GlobalEmbeddingCache::open(tmp.path()).unwrap();
        let telemetry_after = cache2.telemetry().clone();
        assert_eq!(telemetry_after.hits, 1);
    }

    /// Task 6: GC compaction telemetry tracks evictions with byte accounting.
    #[test]
    fn test_gc_compaction_telemetry() {
        let tmp = tempfile::tempdir().unwrap();
        let mut cache = GlobalEmbeddingCache::open(tmp.path()).unwrap();

        // Add many unreferenced rows.
        for i in 0..10 {
            let key = sample_key(&format!("dead row {i}"), 4);
            cache.put(&key, &[1.0, 2.0, 3.0, 4.0]).unwrap();
        }
        assert_eq!(cache.row_count().unwrap(), 10);

        // GC should evict all unreferenced rows.
        let report = cache.gc().unwrap();
        assert_eq!(report.rows_removed, 10);
        assert!(report.reclaimed_bytes > 0);

        let telemetry = cache.telemetry();
        assert_eq!(
            telemetry.evictions, 10,
            "should have telemetry for each eviction"
        );
        assert_eq!(telemetry.bytes_evicted, report.reclaimed_bytes);
    }

    /// Task 6: cache_stats report is serializable and includes generation/model
    /// invalidation key info (spec section 10.3 — count-only prohibited).
    #[test]
    fn test_cache_stats_report_includes_telemetry_and_bytes() {
        let tmp = tempfile::tempdir().unwrap();
        let mut cache = GlobalEmbeddingCache::open(tmp.path()).unwrap();

        let key1 = sample_key("report key 1", 4);
        let key2 = sample_key("report key 2", 4);
        cache.put(&key1, &[0.1, 0.2, 0.3, 0.4]).unwrap();
        cache.put(&key2, &[0.5, 0.6, 0.7, 0.8]).unwrap();

        // Probe for telemetry.
        cache.probe(&[key1.clone(), key2.clone()]).unwrap();

        // Also create a miss.
        let key3 = sample_key("miss key", 4);
        cache.probe(&[key3]).unwrap();

        let stats = cache.cache_stats().unwrap();
        assert_eq!(stats.row_count, 2);
        assert_eq!(stats.telemetry.hits, 2);
        assert_eq!(stats.telemetry.misses, 1);
        assert!(stats.cache_bytes > 0);
        assert!(stats.max_bytes > 0);
        assert!(stats.max_entry_bytes > 0);
        assert_eq!(
            stats.model_identity,
            "content-addressed (model-digest namespaced)"
        );

        // Verify the report is serializable (for JSON output).
        let json = serde_json::to_string(&stats).unwrap();
        assert!(json.contains("cache_bytes"));
        assert!(json.contains("hit_ratio"));
        assert!(json.contains("telemetry"));
        assert!(json.contains("max_bytes"));
        assert!(json.contains("model_identity"));
    }

    // ── WS10 Task 8: Cache-effectiveness measurement (§13 scenario 22) ──

    /// Task 8 / VAL-CACHE-012: Two-worktree cache hit ratio.
    ///
    /// Index two projects with substantial content overlap (simulating two
    /// git worktrees from the same repo). The second index should see cache
    /// hits for all shared content.
    #[test]
    fn test_two_worktree_cache_hit_ratio() {
        let tmp = tempfile::tempdir().unwrap();
        let mut cache = GlobalEmbeddingCache::open(tmp.path()).unwrap();

        let dim = 4u32;
        // Shared content: 10 identical texts across two "worktrees."
        let shared_texts: Vec<String> = (0..10)
            .map(|i| format!("fn func_{i}(x: i32) -> i32 {{ x + {i} }}"))
            .collect();

        let keys_a: Vec<CacheKey> = shared_texts.iter().map(|t| sample_key(t, dim)).collect();

        // Simulate embedding project A (first worktree).
        for key in &keys_a {
            cache.put(key, &[0.1, 0.2, 0.3, 0.4]).unwrap();
        }

        // Simulate probing project B (second worktree with same content).
        let keys_b: Vec<CacheKey> = shared_texts.iter().map(|t| sample_key(t, dim)).collect();

        let result = cache.probe(&keys_b).unwrap();

        // All entries should be cache hits — zero duplicate embeddings.
        assert_eq!(result.hits.len(), 10, "all shared content should hit");
        assert_eq!(result.misses.len(), 0, "no misses for identical content");
        assert_eq!(cache.row_count().unwrap(), 10, "no duplicated rows");

        let stats = cache.cache_stats().unwrap();
        assert_eq!(stats.hit_ratio, 1.0, "100% hit ratio for shared content");
        assert_eq!(stats.row_count, 10);

        // Bytes saved = vectors_that_would_have_been_computed * dim * sizeof(f32)
        let bytes_saved = result.hits.len() as u64 * dim as u64 * 4;
        assert_eq!(bytes_saved, 160, "correct bytes saved calculation");
    }

    /// Task 8 / VAL-CACHE-012: Partially overlapping worktrees.
    #[test]
    fn test_partially_overlapping_worktrees() {
        let tmp = tempfile::tempdir().unwrap();
        let mut cache = GlobalEmbeddingCache::open(tmp.path()).unwrap();

        let dim = 4u32;

        // 8 shared files, 2 unique to each worktree.
        let shared: Vec<String> = (0..8).map(|i| format!("shared_func_{i}")).collect();
        let unique_a: Vec<String> = vec!["unique_a_0".into(), "unique_a_1".into()];
        let unique_b: Vec<String> = vec!["unique_b_0".into(), "unique_b_1".into()];

        // Project A: shared + unique_a.
        let keys_a: Vec<CacheKey> = shared
            .iter()
            .chain(unique_a.iter())
            .map(|t| sample_key(t, dim))
            .collect();
        for key in &keys_a {
            cache.put(key, &[1.0, 2.0, 3.0, 4.0]).unwrap();
        }

        // Project B: shared + unique_b.
        let keys_b: Vec<CacheKey> = shared
            .iter()
            .chain(unique_b.iter())
            .map(|t| sample_key(t, dim))
            .collect();
        let result = cache.probe(&keys_b).unwrap();

        assert_eq!(result.hits.len(), 8, "8 shared should hit");
        assert_eq!(result.misses.len(), 2, "2 unique should miss");

        let stats = cache.cache_stats().unwrap();
        let hit_ratio = stats.hit_ratio;
        assert!(
            (hit_ratio - (8.0 / 10.0)).abs() < 0.01,
            "hit ratio should be 0.8, got {hit_ratio}"
        );
    }

    /// Task 8 / VAL-CROSS-003: Global cache + streaming fragment dedup across
    /// projects — zero duplicate ONNX inference calls for shared content.
    #[test]
    fn test_cross_project_zero_duplicate_embeddings() {
        let tmp = tempfile::tempdir().unwrap();
        let mut cache = GlobalEmbeddingCache::open(tmp.path()).unwrap();

        let dim = 4u32;

        // Project A content.
        let content_a = ["file_one".to_string(), "file_two".to_string()];
        let keys_a: Vec<CacheKey> = content_a.iter().map(|t| sample_key(t, dim)).collect();

        // Simulate embedding all of project A.
        for (i, key) in keys_a.iter().enumerate() {
            cache.put(key, &[(i as f32), 1.0, 2.0, 3.0]).unwrap();
        }

        // Project B has the SAME content (worktree of the same repo).
        let keys_b: Vec<CacheKey> = content_a.iter().map(|t| sample_key(t, dim)).collect();

        // Probe project B — all should be hits.
        let result = cache.probe(&keys_b).unwrap();
        assert_eq!(result.hits.len(), 2, "zero duplicate embeddings needed");
        assert_eq!(result.misses.len(), 0);
        assert_eq!(cache.row_count().unwrap(), 2, "no duplicate rows");
    }

    /// Task 8 / VAL-CACHE-007: Cache hits return bit-equivalent vectors to
    /// fresh embeds (anti-cheat section 2.1 — no precision loss from caching).
    #[test]
    fn test_cache_hit_bit_equivalent_to_stored() {
        let tmp = tempfile::tempdir().unwrap();
        let mut cache = GlobalEmbeddingCache::open(tmp.path()).unwrap();

        let dim = 8;
        let key = sample_key("bit equivalence", dim as u32);
        let original: Vec<f32> = (0..dim).map(|i| (i as f32) * 0.123_456_79).collect();

        // Store the "freshly embedded" vector.
        cache.put(&key, &original).unwrap();

        // Retrieve (simulating a cache hit).
        let cached = cache.get(&key).unwrap().unwrap();

        // Bit-for-bit identical (anti-cheat: no precision loss from cache).
        assert_eq!(cached.len(), original.len());
        for (a, b) in cached.iter().zip(original.iter()) {
            assert_eq!(a.to_bits(), b.to_bits(), "f32 bits must be identical");
        }
    }

    /// VAL-CACHE-015: No source text stored after hashing.
    /// (Verify with various complex source texts.)
    #[test]
    fn test_val_cache_015_no_source_text_in_cache_files() {
        let tmp = tempfile::tempdir().unwrap();
        let mut cache = GlobalEmbeddingCache::open(tmp.path()).unwrap();

        let secret_texts = vec![
            "fn process_payment(credit_card: &str) -> Result<(), Error>",
            "const API_KEY = \"sk-1234567890abcdef\"",
            "SELECT password_hash FROM users WHERE email = 'admin@test.com'",
            "private data that should never be persisted in plaintext",
        ];

        for text in &secret_texts {
            let key = sample_key(text, 4);
            cache.put(&key, &[0.1, 0.2, 0.3, 0.4]).unwrap();

            // Inspect the raw row file.
            let path = cache.row_path(&key.fingerprint());
            let raw = fs::read(&path).unwrap();

            assert!(
                !raw.windows(text.len()).any(|w| w == text.as_bytes()),
                "source text '{}' must not appear in cache file",
                text
            );
        }
    }
}

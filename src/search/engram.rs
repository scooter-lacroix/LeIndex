//! Engram: a persistent, content-addressed phrase-book of query embeddings.
//!
//! A neural query embedding is a pure function of the embedder's identity
//! (model files, dimension, provider) and the exact query text. It does not
//! depend on the project, the index, or any generation, so it can be reused
//! across processes, projects, branches and reindexes without invalidation:
//! only a change of embedder identity produces a different key.
//!
//! Layout (one immutable row per entry, staging + atomic rename):
//!
//! ```text
//! <root>/<2 hex>/<64 hex>.vec   magic(8) version(u32) dim(u32) blake3(payload)(32) payload(dim * f32 LE)
//! <root>/tmp/                   staging area for in-flight writes
//! ```
//!
//! Rows are verified against their blake3 checksum on every read; a corrupt or
//! truncated row is deleted and reported as a miss. The table is bounded by
//! entry count and byte size, and evicts the least recently used rows (file
//! modification time is refreshed when a row is served from disk). A small
//! in-process front serves repeat queries without touching the disk at all.
//!
//! Only real embedder output is ever stored; callers must not put degraded
//! (TF-IDF fallback) vectors here.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, SystemTime};

use serde::Serialize;

use crate::fast_hash::FastMap;

const MAGIC: &[u8; 8] = b"LEENGRM1";
const FORMAT_VERSION: u32 = 1;
const HEADER_LEN: usize = 8 + 4 + 4 + 32;
const DEFAULT_MAX_ENTRIES: usize = 20_000;
const DEFAULT_MAX_BYTES: u64 = 256 * 1024 * 1024;
const MEMORY_FRONT_CAP: usize = 256;
/// Sweep for eviction after this many successful writes.
const EVICT_EVERY_PUTS: u64 = 32;
/// Refresh a row's recency on disk hits only if it is older than this.
const TOUCH_AFTER: Duration = Duration::from_secs(600);
/// Staging files older than this are leftovers from a crashed writer.
const STALE_TMP_AFTER: Duration = Duration::from_secs(3600);

type Key = [u8; 32];

/// Size limits for the on-disk table.
#[derive(Debug, Clone, Copy)]
pub struct EngramLimits {
    /// Maximum number of rows.
    pub max_entries: usize,
    /// Maximum total bytes of rows.
    pub max_bytes: u64,
}

impl Default for EngramLimits {
    fn default() -> Self {
        Self::from_env()
    }
}

impl EngramLimits {
    /// Defaults (20k rows, 256 MiB), overridable with
    /// `LEINDEX_ENGRAM_MAX_ENTRIES` and `LEINDEX_ENGRAM_MAX_MB`.
    pub fn from_env() -> Self {
        let max_entries = std::env::var("LEINDEX_ENGRAM_MAX_ENTRIES")
            .ok()
            .and_then(|v| v.parse::<usize>().ok())
            .filter(|v| *v > 0)
            .unwrap_or(DEFAULT_MAX_ENTRIES);
        let max_bytes = std::env::var("LEINDEX_ENGRAM_MAX_MB")
            .ok()
            .and_then(|v| v.parse::<u64>().ok())
            .filter(|v| *v > 0)
            .map(|mb| mb.saturating_mul(1024 * 1024))
            .unwrap_or(DEFAULT_MAX_BYTES);
        Self {
            max_entries,
            max_bytes,
        }
    }
}

/// Snapshot of Engram state and counters for diagnostics.
#[derive(Debug, Clone, Serialize)]
pub struct EngramStats {
    /// Whether the `Engram` feature flag is on.
    pub enabled: bool,
    /// Whether the table is open in this process.
    pub open: bool,
    /// Table directory, when open.
    pub root: Option<String>,
    /// On-disk format version.
    pub format_version: u32,
    /// Lookups answered from the table (memory or disk).
    pub hits: u64,
    /// Subset of `hits` served from the in-process front.
    pub memory_hits: u64,
    /// Lookups that found nothing usable.
    pub misses: u64,
    /// Rows written.
    pub puts: u64,
    /// Rows removed by eviction.
    pub evictions: u64,
    /// Corrupt or dimension-mismatched rows discarded on read.
    pub corrupt_rows: u64,
    /// Rows currently on disk.
    pub entries: usize,
    /// Bytes currently on disk.
    pub bytes: u64,
    /// Configured row limit.
    pub max_entries: usize,
    /// Configured byte limit.
    pub max_bytes: u64,
}

struct Front {
    tick: u64,
    rows: FastMap<Key, (u64, std::sync::Arc<Vec<f32>>)>,
}

/// The phrase-book. Cheap to share; all methods take `&self`.
pub struct Engram {
    root: PathBuf,
    limits: EngramLimits,
    front: Mutex<Front>,
    hits: AtomicU64,
    memory_hits: AtomicU64,
    misses: AtomicU64,
    puts: AtomicU64,
    evictions: AtomicU64,
    corrupt: AtomicU64,
    puts_since_sweep: AtomicU64,
    staging_counter: AtomicU64,
}

impl Engram {
    /// Open (creating if needed) a table rooted at `root`, then sweep it so the
    /// limits hold from the start.
    pub fn open(root: impl Into<PathBuf>, limits: EngramLimits) -> std::io::Result<Self> {
        let root = root.into();
        fs::create_dir_all(root.join("tmp"))?;
        let engram = Self {
            root,
            limits,
            front: Mutex::new(Front {
                tick: 0,
                rows: FastMap::default(),
            }),
            hits: AtomicU64::new(0),
            memory_hits: AtomicU64::new(0),
            misses: AtomicU64::new(0),
            puts: AtomicU64::new(0),
            evictions: AtomicU64::new(0),
            corrupt: AtomicU64::new(0),
            puts_since_sweep: AtomicU64::new(0),
            staging_counter: AtomicU64::new(0),
        };
        engram.sweep();
        Ok(engram)
    }

    /// Process-wide table under [`default_root`], or `None` when the `Engram`
    /// feature flag is off or the directory cannot be opened.
    pub fn global() -> Option<&'static Engram> {
        if !crate::feature_flags::FeatureFlag::Engram.is_enabled() {
            return None;
        }
        static GLOBAL: OnceLock<Option<Engram>> = OnceLock::new();
        GLOBAL
            .get_or_init(|| {
                let root = default_root();
                match Engram::open(&root, EngramLimits::from_env()) {
                    Ok(engram) => Some(engram),
                    Err(error) => {
                        tracing::warn!(root = %root.display(), %error, "engram unavailable");
                        None
                    }
                }
            })
            .as_ref()
    }

    /// Root directory of this table.
    pub fn root(&self) -> &Path {
        &self.root
    }

    fn key(identity: &str, text: &str, dim: usize) -> Key {
        let mut hasher = blake3::Hasher::new();
        hasher.update(b"engram-key-v1\0");
        hasher.update(identity.as_bytes());
        hasher.update(&[0]);
        hasher.update(&(dim as u64).to_le_bytes());
        hasher.update(text.as_bytes());
        *hasher.finalize().as_bytes()
    }

    fn row_path(&self, key: &Key) -> PathBuf {
        let hex = hex32(key);
        self.root.join(&hex[..2]).join(format!("{hex}.vec"))
    }

    /// Look up the embedding of `text` under `identity`. `dim` is the expected
    /// dimension; rows of any other size are discarded.
    pub fn get(&self, identity: &str, text: &str, dim: usize) -> Option<Vec<f32>> {
        if dim == 0 {
            return None;
        }
        let key = Self::key(identity, text, dim);
        if let Some(vector) = self.front_get(&key) {
            self.hits.fetch_add(1, Ordering::Relaxed);
            self.memory_hits.fetch_add(1, Ordering::Relaxed);
            return Some(vector.as_ref().clone());
        }
        match self.read_row(&key, dim) {
            Some(vector) => {
                self.hits.fetch_add(1, Ordering::Relaxed);
                let shared = std::sync::Arc::new(vector);
                self.front_put(key, std::sync::Arc::clone(&shared));
                Some(shared.as_ref().clone())
            }
            None => {
                self.misses.fetch_add(1, Ordering::Relaxed);
                None
            }
        }
    }

    /// Store `vector` for `text` under `identity`. Empty, wrong-sized or
    /// non-finite vectors are ignored. Best effort: I/O failures are logged and
    /// never surface to the caller.
    pub fn put(&self, identity: &str, text: &str, vector: &[f32]) {
        let dim = vector.len();
        if dim == 0 || vector.iter().any(|v| !v.is_finite()) {
            return;
        }
        let key = Self::key(identity, text, dim);
        if let Err(error) = self.write_row(&key, vector) {
            tracing::debug!(%error, "engram put failed");
            return;
        }
        self.front_put(key, std::sync::Arc::new(vector.to_vec()));
        self.puts.fetch_add(1, Ordering::Relaxed);
        if self.puts_since_sweep.fetch_add(1, Ordering::Relaxed) + 1 >= EVICT_EVERY_PUTS {
            self.puts_since_sweep.store(0, Ordering::Relaxed);
            self.sweep();
        }
    }

    fn front_get(&self, key: &Key) -> Option<std::sync::Arc<Vec<f32>>> {
        let mut front = self.front.lock().ok()?;
        front.tick += 1;
        let tick = front.tick;
        front.rows.get_mut(key).map(|(seen, vector)| {
            *seen = tick;
            std::sync::Arc::clone(vector)
        })
    }

    fn front_put(&self, key: Key, vector: std::sync::Arc<Vec<f32>>) {
        let Ok(mut front) = self.front.lock() else {
            return;
        };
        front.tick += 1;
        let tick = front.tick;
        if front.rows.len() >= MEMORY_FRONT_CAP && !front.rows.contains_key(&key) {
            // Drop the least recently used quarter in one pass.
            let mut ticks: Vec<u64> = front.rows.values().map(|(seen, _)| *seen).collect();
            ticks.sort_unstable();
            let cutoff = ticks[ticks.len() / 4];
            front.rows.retain(|_, (seen, _)| *seen > cutoff);
        }
        front.rows.insert(key, (tick, vector));
    }

    fn read_row(&self, key: &Key, dim: usize) -> Option<Vec<f32>> {
        let path = self.row_path(key);
        let mut file = fs::File::open(&path).ok()?;
        let modified = file.metadata().ok().and_then(|m| m.modified().ok());
        let mut bytes = Vec::with_capacity(HEADER_LEN + dim * 4);
        std::io::Read::read_to_end(&mut file, &mut bytes).ok()?;
        match decode_row(&bytes, dim) {
            Some(vector) => {
                let stale = modified
                    .and_then(|m| SystemTime::now().duration_since(m).ok())
                    .is_none_or(|age| age > TOUCH_AFTER);
                if stale {
                    let _ = file.set_modified(SystemTime::now());
                }
                Some(vector)
            }
            None => {
                self.corrupt.fetch_add(1, Ordering::Relaxed);
                let _ = fs::remove_file(&path);
                None
            }
        }
    }

    fn write_row(&self, key: &Key, vector: &[f32]) -> std::io::Result<()> {
        let final_path = self.row_path(key);
        if let Some(parent) = final_path.parent() {
            fs::create_dir_all(parent)?;
        }
        let staging = self.root.join("tmp").join(format!(
            "{}.{}.{}",
            hex32(key),
            std::process::id(),
            self.staging_counter.fetch_add(1, Ordering::Relaxed)
        ));
        let encoded = encode_row(vector);
        let result = (|| {
            let mut file = fs::File::create(&staging)?;
            file.write_all(&encoded)?;
            file.flush()?;
            fs::rename(&staging, &final_path)
        })();
        if result.is_err() {
            let _ = fs::remove_file(&staging);
        }
        result
    }

    /// Enforce the entry/byte limits (evicting least recently used rows down to
    /// 90% of each limit) and remove stale staging files.
    fn sweep(&self) {
        let mut rows: Vec<(SystemTime, u64, PathBuf)> = Vec::new();
        let mut total_bytes = 0u64;
        for shard in read_dir_paths(&self.root) {
            if shard.file_name().is_some_and(|n| n == "tmp") {
                for staged in read_dir_paths(&shard) {
                    let old = fs::metadata(&staged)
                        .and_then(|m| m.modified())
                        .ok()
                        .and_then(|m| SystemTime::now().duration_since(m).ok())
                        .is_some_and(|age| age > STALE_TMP_AFTER);
                    if old {
                        let _ = fs::remove_file(&staged);
                    }
                }
                continue;
            }
            if !shard.is_dir() {
                continue;
            }
            for row in read_dir_paths(&shard) {
                if row.extension().is_none_or(|e| e != "vec") {
                    continue;
                }
                if let Ok(meta) = fs::metadata(&row) {
                    let len = meta.len();
                    total_bytes += len;
                    rows.push((meta.modified().unwrap_or(SystemTime::UNIX_EPOCH), len, row));
                }
            }
        }
        if rows.len() <= self.limits.max_entries && total_bytes <= self.limits.max_bytes {
            return;
        }
        let target_entries = self.limits.max_entries * 9 / 10;
        let target_bytes = self.limits.max_bytes / 10 * 9;
        rows.sort_by_key(|(modified, _, _)| *modified);
        let mut remaining = rows.len();
        for (_, len, path) in rows {
            if remaining <= target_entries && total_bytes <= target_bytes {
                break;
            }
            if fs::remove_file(&path).is_ok() {
                remaining -= 1;
                total_bytes = total_bytes.saturating_sub(len);
                self.evictions.fetch_add(1, Ordering::Relaxed);
            }
        }
    }

    /// Counters plus a live scan of the table for row and byte totals.
    pub fn stats(&self) -> EngramStats {
        let mut entries = 0usize;
        let mut bytes = 0u64;
        for shard in read_dir_paths(&self.root) {
            if !shard.is_dir() || shard.file_name().is_some_and(|n| n == "tmp") {
                continue;
            }
            for row in read_dir_paths(&shard) {
                if row.extension().is_some_and(|e| e == "vec") {
                    entries += 1;
                    bytes += fs::metadata(&row).map(|m| m.len()).unwrap_or(0);
                }
            }
        }
        EngramStats {
            enabled: true,
            open: true,
            root: Some(self.root.display().to_string()),
            format_version: FORMAT_VERSION,
            hits: self.hits.load(Ordering::Relaxed),
            memory_hits: self.memory_hits.load(Ordering::Relaxed),
            misses: self.misses.load(Ordering::Relaxed),
            puts: self.puts.load(Ordering::Relaxed),
            evictions: self.evictions.load(Ordering::Relaxed),
            corrupt_rows: self.corrupt.load(Ordering::Relaxed),
            entries,
            bytes,
            max_entries: self.limits.max_entries,
            max_bytes: self.limits.max_bytes,
        }
    }
}

/// Stats for diagnostics: real numbers when the flag is on and the table is
/// open, otherwise an explicit "disabled"/"unavailable" record.
pub fn global_stats() -> EngramStats {
    let enabled = crate::feature_flags::FeatureFlag::Engram.is_enabled();
    match Engram::global() {
        Some(engram) => engram.stats(),
        None => {
            let limits = EngramLimits::from_env();
            EngramStats {
                enabled,
                open: false,
                root: None,
                format_version: FORMAT_VERSION,
                hits: 0,
                memory_hits: 0,
                misses: 0,
                puts: 0,
                evictions: 0,
                corrupt_rows: 0,
                entries: 0,
                bytes: 0,
                max_entries: limits.max_entries,
                max_bytes: limits.max_bytes,
            }
        }
    }
}

/// Default table location: `$LEINDEX_ENGRAM_DIR`, else `$LEINDEX_HOME/engram`,
/// else `~/.leindex/engram`. Query embeddings are project-independent, so the
/// table is user-level and shared by every project and branch.
pub fn default_root() -> PathBuf {
    if let Ok(dir) = std::env::var("LEINDEX_ENGRAM_DIR") {
        return PathBuf::from(dir);
    }
    if let Ok(home) = std::env::var("LEINDEX_HOME") {
        return PathBuf::from(home).join("engram");
    }
    if let Ok(home) = std::env::var("HOME") {
        return PathBuf::from(home).join(".leindex").join("engram");
    }
    PathBuf::from(".leindex").join("engram")
}

fn read_dir_paths(dir: &Path) -> Vec<PathBuf> {
    fs::read_dir(dir)
        .map(|entries| entries.filter_map(|e| e.ok().map(|e| e.path())).collect())
        .unwrap_or_default()
}

fn hex32(bytes: &[u8; 32]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(64);
    for byte in bytes {
        out.push(HEX[(byte >> 4) as usize] as char);
        out.push(HEX[(byte & 0x0f) as usize] as char);
    }
    out
}

fn encode_row(vector: &[f32]) -> Vec<u8> {
    let mut payload = Vec::with_capacity(vector.len() * 4);
    for value in vector {
        payload.extend_from_slice(&value.to_le_bytes());
    }
    let checksum = blake3::hash(&payload);
    let mut out = Vec::with_capacity(HEADER_LEN + payload.len());
    out.extend_from_slice(MAGIC);
    out.extend_from_slice(&FORMAT_VERSION.to_le_bytes());
    out.extend_from_slice(&(vector.len() as u32).to_le_bytes());
    out.extend_from_slice(checksum.as_bytes());
    out.extend_from_slice(&payload);
    out
}

fn decode_row(bytes: &[u8], dim: usize) -> Option<Vec<f32>> {
    if bytes.len() != HEADER_LEN + dim * 4 || &bytes[..8] != MAGIC {
        return None;
    }
    let version = u32::from_le_bytes(bytes[8..12].try_into().ok()?);
    let stored_dim = u32::from_le_bytes(bytes[12..16].try_into().ok()?) as usize;
    if version != FORMAT_VERSION || stored_dim != dim {
        return None;
    }
    let payload = &bytes[HEADER_LEN..];
    if blake3::hash(payload).as_bytes() != &bytes[16..48] {
        return None;
    }
    let vector: Vec<f32> = payload
        .chunks_exact(4)
        .map(|chunk| f32::from_le_bytes(chunk.try_into().expect("chunk of 4")))
        .collect();
    vector.iter().all(|v| v.is_finite()).then_some(vector)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vector(seed: f32, dim: usize) -> Vec<f32> {
        (0..dim).map(|i| seed + i as f32 * 0.25).collect()
    }

    fn open(dir: &Path) -> Engram {
        Engram::open(
            dir,
            EngramLimits {
                max_entries: 1000,
                max_bytes: 64 * 1024 * 1024,
            },
        )
        .unwrap()
    }

    #[test]
    fn test_put_then_get_round_trips_exactly() {
        let dir = tempfile::tempdir().unwrap();
        let engram = open(dir.path());
        let v = vector(1.5, 64);
        assert!(engram.get("model-a", "how does search work", 64).is_none());
        engram.put("model-a", "how does search work", &v);
        assert_eq!(engram.get("model-a", "how does search work", 64), Some(v));
        let stats = engram.stats();
        assert_eq!((stats.hits, stats.misses, stats.puts), (1, 1, 1));
        assert_eq!(stats.entries, 1);
    }

    #[test]
    fn test_identity_dimension_and_text_partition_the_key_space() {
        let dir = tempfile::tempdir().unwrap();
        let engram = open(dir.path());
        engram.put("model-a", "query", &vector(1.0, 8));
        assert!(engram.get("model-b", "query", 8).is_none(), "identity");
        assert!(engram.get("model-a", "query", 16).is_none(), "dimension");
        assert!(engram.get("model-a", "Query", 8).is_none(), "exact text");
        assert!(engram.get("model-a", "query ", 8).is_none(), "exact text");
        assert!(engram.get("model-a", "query", 8).is_some());
    }

    #[test]
    fn test_rows_survive_reopen_across_processes() {
        let dir = tempfile::tempdir().unwrap();
        let v = vector(2.0, 32);
        open(dir.path()).put("m", "persisted", &v);
        let reopened = open(dir.path());
        assert_eq!(reopened.get("m", "persisted", 32), Some(v));
        assert_eq!(reopened.stats().memory_hits, 0, "served from disk");
    }

    #[test]
    fn test_corrupt_row_is_a_miss_and_removed() {
        let dir = tempfile::tempdir().unwrap();
        let engram = open(dir.path());
        engram.put("m", "q", &vector(3.0, 16));
        let key = Engram::key("m", "q", 16);
        let path = engram.row_path(&key);
        let mut bytes = fs::read(&path).unwrap();
        let last = bytes.len() - 1;
        bytes[last] ^= 0xff;
        fs::write(&path, bytes).unwrap();

        let fresh = open(dir.path());
        assert!(fresh.get("m", "q", 16).is_none());
        assert!(!path.exists(), "corrupt row must be deleted");
        assert_eq!(fresh.stats().corrupt_rows, 1);
    }

    #[test]
    fn test_truncated_and_foreign_files_are_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let engram = open(dir.path());
        engram.put("m", "q", &vector(1.0, 8));
        let path = engram.row_path(&Engram::key("m", "q", 8));
        let bytes = fs::read(&path).unwrap();
        fs::write(&path, &bytes[..bytes.len() - 3]).unwrap();
        assert!(open(dir.path()).get("m", "q", 8).is_none());
    }

    #[test]
    fn test_degenerate_vectors_are_never_stored() {
        let dir = tempfile::tempdir().unwrap();
        let engram = open(dir.path());
        engram.put("m", "empty", &[]);
        engram.put("m", "nan", &[1.0, f32::NAN]);
        engram.put("m", "inf", &[f32::INFINITY, 1.0]);
        assert_eq!(engram.stats().entries, 0);
        assert_eq!(engram.stats().puts, 0);
    }

    #[test]
    fn test_eviction_bounds_entries_and_keeps_recent_rows() {
        let dir = tempfile::tempdir().unwrap();
        let engram = Engram::open(
            dir.path(),
            EngramLimits {
                max_entries: 40,
                max_bytes: 64 * 1024 * 1024,
            },
        )
        .unwrap();
        for i in 0..200 {
            engram.put("m", &format!("query {i}"), &vector(i as f32, 8));
            // Distinct mtimes so eviction order is well defined.
            std::thread::sleep(Duration::from_millis(2));
        }
        engram.sweep();
        let stats = engram.stats();
        assert!(stats.entries <= 40, "entries {} over limit", stats.entries);
        assert!(stats.evictions >= 160);
        let fresh = open(dir.path());
        assert!(fresh.get("m", "query 199", 8).is_some(), "newest survives");
        assert!(fresh.get("m", "query 0", 8).is_none(), "oldest evicted");
    }

    #[test]
    fn test_byte_limit_is_enforced() {
        let dir = tempfile::tempdir().unwrap();
        let row_bytes = (HEADER_LEN + 256 * 4) as u64;
        let engram = Engram::open(
            dir.path(),
            EngramLimits {
                max_entries: 10_000,
                max_bytes: row_bytes * 20,
            },
        )
        .unwrap();
        for i in 0..100 {
            engram.put("m", &format!("q{i}"), &vector(i as f32, 256));
            std::thread::sleep(Duration::from_millis(2));
        }
        engram.sweep();
        assert!(engram.stats().bytes <= row_bytes * 20);
    }

    #[test]
    fn test_memory_front_serves_repeat_hits_without_disk() {
        let dir = tempfile::tempdir().unwrap();
        let engram = open(dir.path());
        let v = vector(4.0, 16);
        engram.put("m", "hot", &v);
        fs::remove_file(engram.row_path(&Engram::key("m", "hot", 16))).unwrap();
        assert_eq!(engram.get("m", "hot", 16), Some(v));
        assert_eq!(engram.stats().memory_hits, 1);
    }

    #[test]
    fn test_concurrent_writers_never_expose_partial_rows() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_path_buf();
        let handles: Vec<_> = (0..4)
            .map(|t| {
                let root = root.clone();
                std::thread::spawn(move || {
                    let engram = open(&root);
                    for i in 0..50 {
                        // Same keys from every thread: last rename wins, all
                        // versions are identical and complete.
                        engram.put("m", &format!("shared {i}"), &vector(i as f32, 64));
                        let _ = engram.get("m", &format!("shared {}", (i + t) % 50), 64);
                    }
                })
            })
            .collect();
        for handle in handles {
            handle.join().unwrap();
        }
        let reader = open(&root);
        for i in 0..50 {
            assert_eq!(
                reader.get("m", &format!("shared {i}"), 64),
                Some(vector(i as f32, 64))
            );
        }
        assert_eq!(reader.stats().corrupt_rows, 0);
    }
}

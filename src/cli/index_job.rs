//! Registry-owned indexing jobs.

#![allow(missing_docs)]

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::sync::{Mutex, Notify};

static JOB_COUNTER: AtomicU64 = AtomicU64::new(1);

/// Lifecycle status for a registry-owned indexing job.
///
/// Serialized as lowercase strings (`"running"`, `"complete"`, `"failed"`)
/// for JSON compatibility with MCP clients that polled the previous
/// `String`-typed field.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum JobStatus {
    /// The job is currently running (scan/parse/publish phases).
    Running,
    /// The job completed successfully and the result generation is published.
    Complete,
    /// The job failed; see `IndexJobSnapshot::last_error` for details.
    Failed,
}

impl fmt::Display for JobStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            JobStatus::Running => write!(f, "running"),
            JobStatus::Complete => write!(f, "complete"),
            JobStatus::Failed => write!(f, "failed"),
        }
    }
}

/// Stable progress snapshot returned by MCP indexing calls.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IndexJobSnapshot {
    /// Stable job identifier.
    pub job_id: String,
    /// Current lifecycle status of the job.
    pub status: JobStatus,
    /// Current indexing phase.
    pub phase: String,
    /// Published generation, when known.
    pub generation: u64,
    /// Completed work units.
    pub completed_units: usize,
    /// Total work units, when known.
    pub total_units: usize,
    /// Which core/enhancement layers are published.
    pub published: PublishedLayers,
    /// Last bounded failure message.
    pub last_error: Option<String>,
}

/// Publication flags for an index job.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PublishedLayers {
    /// PDG layer is durable and queryable.
    pub pdg: bool,
    /// TF-IDF/lexical layer is durable and queryable.
    pub lexical: bool,
    /// Neural layer is durable and queryable.
    pub neural: bool,
}

/// Paths for one restartable indexing attempt.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JobPaths {
    /// Job directory (`.leindex/jobs/<generation>`).
    pub root: PathBuf,
    /// Monotonic generation proposed by this attempt.
    pub generation: u64,
}

impl JobPaths {
    /// Build paths without creating files. Callers create artifacts atomically.
    pub fn new(storage_root: &Path, generation: u64) -> Self {
        Self {
            root: storage_root.join("jobs").join(generation.to_string()),
            generation,
        }
    }

    pub fn state(&self) -> PathBuf {
        self.root.join("state.json")
    }

    /// Registry-facing status snapshot, kept separate from checkpoint state.
    pub fn job_status(&self) -> PathBuf {
        self.root.join("job-status.json")
    }

    pub fn scan(&self) -> PathBuf {
        self.root.join("scan.bin")
    }

    pub fn parsed_dir(&self) -> PathBuf {
        self.root.join("parsed")
    }

    pub fn parsed(&self, source_hash: &str) -> PathBuf {
        self.parsed_dir().join(format!("{source_hash}.bin"))
    }

    /// Deterministic bucket artifact for parsed checkpoints. Hash buckets keep
    /// restart reuse per file while avoiding one fsync/rename pair per source.
    pub fn parsed_bucket(&self, source_hash: &str) -> PathBuf {
        let bucket = source_hash.chars().take(2).collect::<String>();
        self.parsed_dir().join(format!("bucket-{bucket}.bin"))
    }

    pub fn parse(&self) -> PathBuf {
        self.root.join("parse.complete")
    }

    pub fn pdg(&self) -> PathBuf {
        self.root.join("pdg.bin")
    }

    pub fn pdg_checkpoint(&self) -> PathBuf {
        self.root.join("pdg.complete")
    }

    pub fn lexical(&self) -> PathBuf {
        self.root.join("lexical.complete")
    }

    pub fn neural(&self) -> PathBuf {
        self.root.join("neural.complete")
    }
}

/// One source fingerprint in a scan checkpoint.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct FileFingerprint {
    pub canonical_path: PathBuf,
    pub blake3: String,
    pub bytes: u64,
    pub language: String,
}

/// Reusable inventory output.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ScanCheckpoint {
    pub input_hash: String,
    pub files: Vec<FileFingerprint>,
}

/// Reusable parser output. Source bytes are deliberately not retained.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ParsedFileCheckpoint {
    pub file_path: PathBuf,
    pub language: String,
    pub signatures: Vec<crate::parse::prelude::SignatureInfo>,
    pub parse_time_ms: u64,
}

/// Aggregate parser checkpoint. Hash-named per-file artifacts remain the
/// reusable source of truth; this marker only records that the parse phase
/// completed for the scan inventory.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ParseCheckpoint {
    pub scan_hash: String,
    pub artifact_paths: Vec<PathBuf>,
    #[serde(default)]
    pub artifact_hashes: BTreeMap<String, String>,
}

/// Metadata for an atomically persisted PDG artifact.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PdgCheckpoint {
    pub scan_hash: String,
    pub artifact_path: PathBuf,
    pub artifact_hash: String,
    pub nodes: usize,
    pub edges: usize,
}

/// Metadata for lexical publication.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LexicalCheckpoint {
    pub pdg_hash: String,
    pub snapshot_path: PathBuf,
    pub tfidf_path: PathBuf,
}

/// Metadata for neural publication.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct NeuralCheckpoint {
    pub lexical_hash: String,
    pub mmap_path: PathBuf,
    pub rows: usize,
    pub provider: String,
    pub model: String,
}

/// Result of an atomic immutable-generation publication.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PublishedGeneration {
    pub generation: u64,
    pub storage_path: PathBuf,
    pub health: crate::cli::leindex::IndexHealth,
}

/// Durable phase state. It is advisory until the referenced artifact validates.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct JobCheckpointState {
    pub job_id: String,
    pub input_generation: u64,
    pub artifact_hashes: BTreeMap<String, String>,
    pub last_reusable_phase: Option<String>,
    pub updated_at_unix_ms: u64,
}

/// Atomic artifact storage for one indexing attempt.
#[derive(Debug, Clone)]
pub struct CheckpointStore {
    pub paths: JobPaths,
}

impl CheckpointStore {
    pub fn new(storage_root: &Path, generation: u64) -> Self {
        Self {
            paths: JobPaths::new(storage_root, generation),
        }
    }

    pub fn from_paths(paths: JobPaths) -> Self {
        Self { paths }
    }

    pub fn write_scan(&self, checkpoint: &ScanCheckpoint) -> Result<String> {
        let bytes = bincode::serialize(checkpoint).context("serialize scan checkpoint")?;
        let hash = atomic_write(&self.paths.scan(), &bytes)?;
        Ok(hash)
    }

    pub fn read_scan(&self) -> Result<Option<ScanCheckpoint>> {
        read_verified(&self.paths.scan())
    }

    pub fn write_parsed(
        &self,
        source_hash: &str,
        checkpoint: &ParsedFileCheckpoint,
    ) -> Result<String> {
        let bytes = bincode::serialize(checkpoint).context("serialize parsed checkpoint")?;
        atomic_write(&self.paths.parsed(source_hash), &bytes)
    }

    /// Persist a bucket of parsed files as one validated artifact. The map
    /// value is a vector because identical source bytes can belong to several
    /// paths; path identity is checked when a file is resumed.
    pub fn write_parsed_batch(
        &self,
        source_hash: &str,
        files: &BTreeMap<String, Vec<ParsedFileCheckpoint>>,
    ) -> Result<String> {
        let bytes = bincode::serialize(files).context("serialize parsed checkpoint bucket")?;
        atomic_write(&self.paths.parsed_bucket(source_hash), &bytes)
    }

    pub fn read_parsed(&self, source_hash: &str) -> Result<Option<ParsedFileCheckpoint>> {
        read_verified(&self.paths.parsed(source_hash))
    }

    pub fn read_parsed_verified(
        &self,
        source_hash: &str,
        expected_hash: &str,
    ) -> Result<Option<ParsedFileCheckpoint>> {
        let path = self.paths.parsed(source_hash);
        let bytes = match fs::read(&path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error.into()),
        };
        if blake3::hash(&bytes).to_hex().as_str() != expected_hash {
            return Ok(None);
        }
        Ok(bincode::deserialize(&bytes).ok())
    }

    /// Read one path from a bucketed parsed artifact, falling back to the
    /// legacy one-file artifact format used by older jobs.
    pub fn read_parsed_for_path_verified(
        &self,
        source_hash: &str,
        expected_hash: &str,
        expected_path: &Path,
    ) -> Result<Option<ParsedFileCheckpoint>> {
        if let Some(bytes) =
            read_bytes_verified(&self.paths.parsed_bucket(source_hash), expected_hash)?
        {
            if let Ok(files) =
                bincode::deserialize::<BTreeMap<String, Vec<ParsedFileCheckpoint>>>(&bytes)
            {
                return Ok(files
                    .get(source_hash)
                    .and_then(|entries| {
                        entries
                            .iter()
                            .find(|entry| entry.file_path == expected_path)
                    })
                    .cloned());
            }
        }
        Ok(self
            .read_parsed_verified(source_hash, expected_hash)?
            .filter(|parsed| parsed.file_path == expected_path))
    }

    pub fn write_parse(&self, checkpoint: &ParseCheckpoint) -> Result<String> {
        let bytes = serde_json::to_vec(checkpoint).context("serialize parse checkpoint")?;
        atomic_write(&self.paths.parse(), &bytes)
    }

    pub fn read_parse(&self) -> Result<Option<ParseCheckpoint>> {
        read_verified(&self.paths.parse())
    }

    pub fn write_pdg(
        &self,
        scan_hash: String,
        pdg: &crate::graph::pdg::ProgramDependenceGraph,
    ) -> Result<PdgCheckpoint> {
        let bytes = pdg.serialize().map_err(anyhow::Error::msg)?;
        let artifact_hash = atomic_write(&self.paths.pdg(), &bytes)?;
        let checkpoint = PdgCheckpoint {
            scan_hash,
            artifact_path: self.paths.pdg(),
            artifact_hash,
            nodes: pdg.node_count(),
            edges: pdg.edge_count(),
        };
        let metadata = serde_json::to_vec(&checkpoint).context("serialize PDG checkpoint")?;
        atomic_write(&self.paths.pdg_checkpoint(), &metadata)?;
        Ok(checkpoint)
    }

    pub fn read_pdg(
        &self,
        checkpoint: &PdgCheckpoint,
    ) -> Result<Option<crate::graph::pdg::ProgramDependenceGraph>> {
        let Some(bytes) =
            read_bytes_verified(&checkpoint.artifact_path, &checkpoint.artifact_hash)?
        else {
            return Ok(None);
        };
        Ok(crate::graph::pdg::ProgramDependenceGraph::deserialize(&bytes).ok())
    }

    /// Load the raw PDG artifact when its state hash matches.
    pub fn read_pdg_artifact(
        &self,
        expected_hash: &str,
    ) -> Result<Option<crate::graph::pdg::ProgramDependenceGraph>> {
        let Some(bytes) = read_bytes_verified(&self.paths.pdg(), expected_hash)? else {
            return Ok(None);
        };
        Ok(crate::graph::pdg::ProgramDependenceGraph::deserialize(&bytes).ok())
    }

    pub fn read_pdg_checkpoint(&self) -> Result<Option<PdgCheckpoint>> {
        read_verified(&self.paths.pdg_checkpoint())
    }

    pub fn write_lexical(&self, checkpoint: &LexicalCheckpoint) -> Result<String> {
        let bytes = serde_json::to_vec(checkpoint).context("serialize lexical checkpoint")?;
        atomic_write(&self.paths.lexical(), &bytes)
    }

    /// Read a lexical checkpoint only when its JSON and checksum are intact.
    pub fn read_lexical(&self) -> Result<Option<LexicalCheckpoint>> {
        read_verified(&self.paths.lexical())
    }

    pub fn write_neural(&self, checkpoint: &NeuralCheckpoint) -> Result<String> {
        let bytes = serde_json::to_vec(checkpoint).context("serialize neural checkpoint")?;
        atomic_write(&self.paths.neural(), &bytes)
    }

    /// Read a neural checkpoint only when its JSON and checksum are intact.
    pub fn read_neural(&self) -> Result<Option<NeuralCheckpoint>> {
        read_verified(&self.paths.neural())
    }

    pub fn write_state(&self, state: &JobCheckpointState) -> Result<String> {
        let bytes = serde_json::to_vec_pretty(state).context("serialize checkpoint state")?;
        atomic_write(&self.paths.state(), &bytes)
    }

    pub fn read_state(&self) -> Result<Option<JobCheckpointState>> {
        read_verified(&self.paths.state())
    }
}

/// Compute a deterministic hash over sorted source fingerprints.
pub fn scan_hash(files: &[FileFingerprint]) -> String {
    let mut sorted = files.to_vec();
    sorted.sort_by(|a, b| a.canonical_path.cmp(&b.canonical_path));
    let mut hasher = blake3::Hasher::new();
    for file in sorted {
        hasher.update(file.canonical_path.to_string_lossy().as_bytes());
        hasher.update(&[0]);
        hasher.update(file.blake3.as_bytes());
        hasher.update(&[0]);
        hasher.update(file.bytes.to_string().as_bytes());
        hasher.update(&[0]);
        hasher.update(file.language.as_bytes());
        hasher.update(&[0]);
    }
    hasher.finalize().to_hex().to_string()
}

pub fn checkpoint_now_unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis().min(u64::MAX as u128) as u64)
        .unwrap_or_default()
}

fn atomic_write(path: &Path, bytes: &[u8]) -> Result<String> {
    let parent = path
        .parent()
        .ok_or_else(|| anyhow::anyhow!("checkpoint has no parent"))?;
    fs::create_dir_all(parent)?;
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| anyhow::anyhow!("checkpoint has invalid file name"))?;
    let next = parent.join(format!("{file_name}.next"));
    let mut file = OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .open(&next)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    drop(file);
    #[cfg(windows)]
    if path.exists() {
        fs::remove_file(path)?;
    }
    fs::rename(&next, path)?;
    #[cfg(unix)]
    File::open(parent)?.sync_all()?;
    Ok(blake3::hash(bytes).to_hex().to_string())
}

fn read_bytes_verified(path: &Path, expected_hash: &str) -> Result<Option<Vec<u8>>> {
    let bytes = match fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    if blake3::hash(&bytes).to_hex().as_str() != expected_hash {
        return Ok(None);
    }
    Ok(Some(bytes))
}

fn read_verified<T: for<'de> Deserialize<'de>>(path: &Path) -> Result<Option<T>> {
    let bytes = match fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    if path.extension().and_then(|extension| extension.to_str()) == Some("bin") {
        return Ok(bincode::deserialize(&bytes).ok());
    }
    Ok(serde_json::from_slice(&bytes).ok())
}

/// Return newest incomplete job state, ignoring corrupt/incomplete files.
pub fn latest_incomplete_job(storage_root: &Path) -> Option<(JobPaths, JobCheckpointState)> {
    let jobs = storage_root.join("jobs");
    let mut candidates = fs::read_dir(jobs)
        .ok()?
        .filter_map(Result::ok)
        .filter_map(|entry| entry.file_name().to_str()?.parse::<u64>().ok())
        .collect::<Vec<_>>();
    candidates.sort_unstable_by(|a, b| b.cmp(a));
    for generation in candidates {
        let paths = JobPaths::new(storage_root, generation);
        let Some(state) = CheckpointStore::from_paths(paths.clone())
            .read_state()
            .ok()
            .flatten()
        else {
            continue;
        };
        if state.last_reusable_phase.as_deref() != Some("complete") {
            return Some((paths, state));
        }
    }
    None
}

impl Default for IndexJobSnapshot {
    fn default() -> Self {
        Self {
            job_id: String::new(),
            status: JobStatus::Running,
            phase: "scan".to_string(),
            generation: 0,
            completed_units: 0,
            total_units: 0,
            published: PublishedLayers {
                pdg: false,
                lexical: false,
                neural: false,
            },
            last_error: None,
        }
    }
}

/// In-process state shared by the registry task and MCP pollers.
pub struct IndexJobState {
    snapshot: Mutex<IndexJobSnapshot>,
    done: Notify,
    started: AtomicBool,
    state_path: Option<PathBuf>,
}

impl IndexJobState {
    /// Create a new registry-owned job.
    pub fn new(job_id: String) -> Self {
        let snapshot = IndexJobSnapshot {
            job_id,
            ..IndexJobSnapshot::default()
        };
        Self {
            snapshot: Mutex::new(snapshot),
            done: Notify::new(),
            started: AtomicBool::new(false),
            state_path: None,
        }
    }

    /// Create a job whose small progress snapshot is also durable.
    pub fn with_state_path(job_id: String, state_path: PathBuf) -> Self {
        let mut state = Self::new(job_id);
        state.state_path = Some(state_path);
        let snapshot = state
            .snapshot
            .try_lock()
            .expect("new index job snapshot lock is uncontended")
            .clone();
        state.persist_snapshot_sync(&snapshot);
        state
    }

    /// Restore a job snapshot after the owning process restarted.
    pub fn from_persisted(snapshot: IndexJobSnapshot, state_path: PathBuf) -> Self {
        Self {
            snapshot: Mutex::new(snapshot),
            done: Notify::new(),
            started: AtomicBool::new(false),
            state_path: Some(state_path),
        }
    }

    fn persist_snapshot_sync(&self, snapshot: &IndexJobSnapshot) {
        let Some(path) = &self.state_path else {
            return;
        };
        let Ok(bytes) = serde_json::to_vec_pretty(snapshot) else {
            return;
        };
        let _ = atomic_write(path, &bytes);
    }

    /// Return the current progress snapshot.
    pub async fn snapshot(&self) -> IndexJobSnapshot {
        self.snapshot.lock().await.clone()
    }

    /// Update the running phase.
    pub async fn set_phase(&self, phase: impl Into<String>, completed: usize, total: usize) {
        let mut snapshot = self.snapshot.lock().await;
        snapshot.phase = phase.into();
        snapshot.completed_units = completed;
        snapshot.total_units = total;
        self.persist_snapshot_sync(&snapshot);
    }

    /// Claim the single task slot for this job.
    pub fn try_start(&self) -> bool {
        self.started
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
    }

    /// Mark the core generation complete.
    pub async fn complete(&self, generation: u64) {
        let mut snapshot = self.snapshot.lock().await;
        snapshot.status = JobStatus::Complete;
        snapshot.phase = "complete".to_string();
        snapshot.generation = generation;
        snapshot.published.pdg = true;
        snapshot.published.lexical = true;
        self.persist_snapshot_sync(&snapshot);
        self.done.notify_waiters();
    }

    /// Record a durable core publication while the configured neural phase is
    /// still running. A later failure must not erase the fact that PDG/TF-IDF is
    /// already queryable through `CURRENT`.
    pub async fn mark_core_published(&self, generation: u64) {
        let mut snapshot = self.snapshot.lock().await;
        snapshot.generation = generation;
        snapshot.published.pdg = true;
        snapshot.published.lexical = true;
        self.persist_snapshot_sync(&snapshot);
    }

    /// Mark configured neural rows as part of the published hybrid result.
    pub async fn mark_neural_published(&self) {
        let mut snapshot = self.snapshot.lock().await;
        snapshot.published.neural = true;
        self.persist_snapshot_sync(&snapshot);
    }

    /// Mark the job failed without cancelling any already-persisted artifacts.
    pub async fn fail(&self, error: impl Into<String>) {
        let mut snapshot = self.snapshot.lock().await;
        snapshot.status = JobStatus::Failed;
        snapshot.last_error = Some(error.into());
        self.persist_snapshot_sync(&snapshot);
        self.done.notify_waiters();
    }

    /// Wait until the owned task publishes a terminal state.
    pub async fn wait(&self) -> IndexJobSnapshot {
        loop {
            // Subscribe before reading the snapshot so a completion between
            // the read and `notified().await` cannot be lost.
            let notified = self.done.notified();
            let snapshot = self.snapshot().await;
            if snapshot.status != JobStatus::Running {
                return snapshot;
            }
            notified.await;
        }
    }
}

/// Generate a per-project job ID that is stable enough for polling while
/// remaining unique across force-reindex generations.
pub fn new_job_id(project_path: &std::path::Path) -> String {
    let digest = blake3::hash(project_path.to_string_lossy().as_bytes()).to_hex();
    format!(
        "{}-{}",
        &digest[..16],
        JOB_COUNTER.fetch_add(1, Ordering::Relaxed)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn job_is_single_flight_and_publishes_core_layers() {
        let state = IndexJobState::new("job-1".to_string());
        assert!(state.try_start());
        assert!(!state.try_start());
        state.set_phase("parse", 2, 4).await;
        let running = state.snapshot().await;
        assert_eq!(running.phase, "parse");
        state.complete(7).await;
        let done = state.wait().await;
        assert_eq!(done.status, JobStatus::Complete);
        assert_eq!(done.generation, 7);
        assert!(done.published.pdg);
        assert!(done.published.lexical);
        assert!(!done.published.neural);
    }

    #[tokio::test]
    async fn registry_snapshot_uses_durable_status_file() {
        let temp = tempfile::tempdir().expect("temp job status root");
        let path = temp.path().join("jobs").join("1").join("job-status.json");
        let state = IndexJobState::with_state_path("job-1".to_string(), path.clone());
        state.set_phase("pdg", 1, 2).await;
        let saved: IndexJobSnapshot =
            serde_json::from_slice(&std::fs::read(path).expect("read durable job status"))
                .expect("decode durable job status");
        assert_eq!(saved.job_id, "job-1");
        assert_eq!(saved.phase, "pdg");
    }

    #[test]
    fn checkpoint_artifacts_are_atomic_and_hash_validated() {
        let temp = tempfile::tempdir().expect("temp checkpoint root");
        let store = CheckpointStore::new(temp.path(), 4);
        let scan = ScanCheckpoint {
            input_hash: "scan-hash".to_string(),
            files: vec![FileFingerprint {
                canonical_path: PathBuf::from("src/lib.rs"),
                blake3: "file-hash".to_string(),
                bytes: 12,
                language: "rust".to_string(),
            }],
        };
        let scan_hash = store.write_scan(&scan).expect("write scan");
        assert_eq!(store.read_scan().expect("read scan"), Some(scan));

        let pdg = crate::graph::pdg::ProgramDependenceGraph::default();
        let pdg_checkpoint = store
            .write_pdg("scan-hash".to_string(), &pdg)
            .expect("write pdg");
        assert!(store.read_pdg(&pdg_checkpoint).expect("read pdg").is_some());

        let mut state = JobCheckpointState {
            job_id: "job".to_string(),
            input_generation: 3,
            ..JobCheckpointState::default()
        };
        state.last_reusable_phase = Some("scan".to_string());
        state.artifact_hashes.insert("scan".to_string(), scan_hash);
        store.write_state(&state).expect("write state");
        assert_eq!(
            latest_incomplete_job(temp.path()).map(|(_, s)| s.job_id),
            Some("job".to_string())
        );

        std::fs::write(store.paths.scan(), b"corrupt").expect("corrupt scan");
        assert!(store.read_scan().expect("read corrupt scan").is_none());
    }
}

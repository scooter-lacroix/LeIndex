//! Optional SCIP precision ingestion for the Tier-0 PDG.
//!
//! The precision tier is deliberately additive: when an external SCIP indexer
//! is unavailable or a run is refused by its resource rails, callers retain the
//! existing tree-sitter PDG unchanged.

use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock, TryLockError};
use std::time::{Duration, Instant};

use crate::graph::pdg::ProgramDependenceGraph;

pub mod indexer;
pub mod merge;
pub mod scip_ingest;

pub use indexer::{IndexerSpec, discover_indexer, run_indexer};
pub use merge::{PrecisionReport, merge_facts};
pub use scip_ingest::{
    CompactScipFacts, DefinitionFact, RelationshipFact, ScipIngestError, ingest_bytes,
};

/// Per-project precision-run guards.
///
/// The guard must be per PROJECT, not process-global. The pass holds the
/// project write lock for the whole index, so the only run it can deadlock
/// against is one on the *same* project. A single global mutex serialized
/// unrelated projects: a second project's index would skip precision entirely
/// whenever any other project happened to be running it, and every test that
/// indexes a scratch repo contended with every other one.
static PRECISION_RUN_LOCKS: OnceLock<Mutex<HashMap<PathBuf, Arc<Mutex<()>>>>> = OnceLock::new();
static PRECISION_RUN_COUNTER: AtomicU64 = AtomicU64::new(1);

/// Guard serializing precision runs for one project root.
///
/// Entries are deliberately never pruned: a project needs a fresh mutex only
/// once, the map holds one small allocation per distinct root a process ever
/// runs precision for, and never dropping a live entry is what makes the
/// unheld-mutex invariant safe.
///
/// The key is canonicalised first. Two paths to the same project — a symlink,
/// a relative path, `/repo` vs `/repo/` — would otherwise get two locks, and
/// the whole point of this guard is that two runs on ONE project cannot
/// overlap. `canonicalize` fails on a path that does not exist, so fall back to
/// the given path: a bogus root has no runs to serialize anyway.
fn precision_run_lock(project_root: &Path) -> Arc<Mutex<()>> {
    let key = std::fs::canonicalize(project_root).unwrap_or_else(|_| project_root.to_path_buf());
    let mut locks = PRECISION_RUN_LOCKS
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    Arc::clone(locks.entry(key).or_default())
}

/// Marker recording the last precision attempt (unix seconds) so repeated
/// tool-driven invocations do not re-spawn external indexers back to back.
fn precision_attempt_marker(project_root: &Path) -> std::path::PathBuf {
    project_root.join(".leindex").join("precision_attempt")
}

fn min_attempt_interval_secs() -> u64 {
    std::env::var("LEINDEX_SCIP_MIN_ATTEMPT_INTERVAL_SECS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(900)
}

fn precision_attempt_allowed(project_root: &Path) -> bool {
    if std::env::var("LEINDEX_SCIP_FORCE")
        .is_ok_and(|value| value == "1" || value.eq_ignore_ascii_case("true"))
    {
        return true;
    }
    let interval = min_attempt_interval_secs();
    if interval == 0 {
        return true;
    }
    let Ok(contents) = std::fs::read_to_string(precision_attempt_marker(project_root)) else {
        return true;
    };
    let Ok(last) = contents.trim().parse::<u64>() else {
        return true;
    };
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0);
    if now.saturating_sub(last) < interval {
        tracing::debug!(
            last_attempt_secs_ago = now.saturating_sub(last),
            interval_secs = interval,
            "SCIP precision attempt throttled; set LEINDEX_SCIP_FORCE=1 to override"
        );
        return false;
    }
    true
}

fn record_precision_attempt(project_root: &Path) {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0);
    let marker = precision_attempt_marker(project_root);
    if let Some(parent) = marker.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = std::fs::write(&marker, now.to_string());
}

/// Run the bounded, serialized SCIP precision pass for languages with a
/// discoverable external indexer.
///
/// Missing indexers, low memory, timeout, malformed output, and oversized
/// outputs all degrade silently to the canonical Tier-0 PDG. Temporary SCIP
/// outputs are removed after every language attempt. Attempts are throttled
/// per project (default once per 15 minutes; `LEINDEX_SCIP_FORCE=1` or
/// `LEINDEX_SCIP_MIN_ATTEMPT_INTERVAL_SECS=0` overrides) so interactive
/// tool paths never spawn external indexers back to back.
///
/// Two bounds keep this pass from ever blocking a caller indefinitely:
/// `LEINDEX_SCIP_TIMEOUT_SECS` caps a single indexer and
/// `LEINDEX_SCIP_TOTAL_TIMEOUT_SECS` caps the whole pass across languages. A
/// concurrent run skips (try-lock) rather than queueing, because this pass
/// executes under the project write lock.
pub fn run_precision_ingest(
    pdg: &mut ProgramDependenceGraph,
    project_root: &Path,
) -> PrecisionReport {
    let mut total = PrecisionReport::default();
    // Try-lock, never block, on THIS project's guard. The pass runs under the
    // project write lock, so a caller that parked here would wait for another
    // precision run on the same project to finish while holding a lock that
    // run needs to progress — the deadlock that wedged indexing until
    // `leindexd` was killed. Precision is additive by design, so a busy run
    // means "skip, keep Tier-0".
    let run_lock = precision_run_lock(project_root);
    let _run_guard = match run_lock.try_lock() {
        Ok(guard) => guard,
        Err(TryLockError::WouldBlock) => {
            tracing::info!(
                "SCIP precision run already in progress; keeping the Tier-0 PDG for this project"
            );
            total.skipped_concurrent_run = true;
            return total;
        }
        Err(TryLockError::Poisoned(poisoned)) => poisoned.into_inner(),
    };
    if !precision_attempt_allowed(project_root) {
        return total;
    }
    record_precision_attempt(project_root);
    let languages: BTreeSet<String> = pdg
        .node_indices()
        .filter_map(|node_id| {
            pdg.get_node(node_id)
                .map(|node| node.language.to_ascii_lowercase())
        })
        .filter(|language| language != "unknown" && language != "external")
        .collect();
    if languages.is_empty() {
        return total;
    }

    let minimum_mb = std::env::var("LEINDEX_SCIP_MIN_AVAILABLE_MB")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(4096);
    if let Some(available_kib) = mem_available_kib()
        && available_kib < minimum_mb.saturating_mul(1024)
    {
        tracing::info!(
            available_mb = available_kib / 1024,
            minimum_mb,
            "SCIP precision skipped below MemAvailable floor"
        );
        return total;
    }

    let timeout_secs = std::env::var("LEINDEX_SCIP_TIMEOUT_SECS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(600);
    // `timeout_secs` bounds ONE indexer. A repo with many languages would run
    // N x timeout back to back while holding the project write lock, so the
    // pass needs its own ceiling too. Both defaults are 600s for now: the
    // aggregate budget must not change current behaviour on a normal repo, and
    // `LEINDEX_SCIP_TIMEOUT_SECS` must keep meaning what it always meant. A
    // tight budget is opt-in via `LEINDEX_SCIP_TOTAL_TIMEOUT_SECS`.
    let total_budget_secs = std::env::var("LEINDEX_SCIP_TOTAL_TIMEOUT_SECS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(timeout_secs);
    let deadline = Instant::now() + Duration::from_secs(total_budget_secs);
    let run_id = PRECISION_RUN_COUNTER.fetch_add(1, Ordering::Relaxed);
    let job_dir = project_root
        .join(".leindex")
        .join("jobs")
        .join(format!("precision-{}-{run_id}", std::process::id()));
    let output_dir = job_dir.join("scip");
    if let Err(error) = std::fs::create_dir_all(&output_dir) {
        tracing::debug!(%error, "SCIP precision temporary directory unavailable");
        return total;
    }

    for language in languages {
        let Some(spec) = discover_indexer(&language) else {
            continue;
        };
        // Each indexer gets the smaller of its own timeout and what is left of
        // the aggregate budget, so one slow language cannot consume the whole
        // pass. Whatever is left when the budget runs out stays Tier-0, which
        // is the documented degradation for this pass.
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            tracing::info!(
                total_budget_secs,
                "SCIP precision aggregate budget exhausted; remaining languages stay Tier-0"
            );
            break;
        }
        let language_timeout = remaining.min(Duration::from_secs(timeout_secs));
        let output = output_dir.join(format!("{language}.scip"));
        let result = run_indexer(&spec, project_root, &output, language_timeout);
        let run_ok = result.is_ok();
        if let Err(error) = result {
            tracing::info!(language = %language, %error, "SCIP indexer unavailable or failed; using Tier-0");
        }
        if run_ok {
            let too_large = mem_available_kib().is_some_and(|available_kib| {
                std::fs::metadata(&output)
                    .map(|metadata| metadata.len() > available_kib.saturating_mul(1024) / 2)
                    .unwrap_or(false)
            });
            if too_large {
                tracing::info!(language = %language, "SCIP output refused above half MemAvailable");
            } else if let Ok(facts) =
                scip_ingest::ingest_file_from_root(&output, Some(project_root))
            {
                let report = merge_facts(pdg, &facts);
                total.definitions_seen += report.definitions_seen;
                total.definitions_matched += report.definitions_matched;
                total.definitions_unmatched += report.definitions_unmatched;
                total.relationships_seen += report.relationships_seen;
                total.relationships_upgraded += report.relationships_upgraded;
                total.relationships_added += report.relationships_added;
                total.relationships_unmatched += report.relationships_unmatched;
            } else {
                tracing::info!(language = %language, "SCIP output could not be decoded; using Tier-0");
            }
        }
        let _ = std::fs::remove_file(&output);
    }
    // Remove the now-empty per-run output directory and job directory. Do not
    // remove the shared jobs root, which may contain other runs.
    let _ = std::fs::remove_dir(&output_dir);
    let _ = std::fs::remove_dir(&job_dir);
    total
}

fn mem_available_kib() -> Option<u64> {
    let contents = std::fs::read_to_string("/proc/meminfo").ok()?;
    contents.lines().find_map(|line| {
        let mut fields = line.split_whitespace();
        (fields.next() == Some("MemAvailable:")).then(|| fields.next()?.parse().ok())?
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::pdg::{Node, NodeType};
    use std::sync::Arc;

    #[cfg(unix)]
    fn shell_fixture(script: &str) -> (tempfile::TempDir, std::path::PathBuf) {
        use std::os::unix::fs::PermissionsExt;

        let directory = tempfile::tempdir().unwrap();
        let executable = directory.path().join("fixture.sh");
        std::fs::write(&executable, format!("#!/bin/sh\\n{script}\\n")).unwrap();
        let mut permissions = std::fs::metadata(&executable).unwrap().permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&executable, permissions).unwrap();
        (directory, executable)
    }

    /// Repro for the post-index hang: `run_precision_ingest` executes with the
    /// project write lock held, so if it parked on a precision mutex instead of
    /// trying it, a second index of the SAME project would block for as long as
    /// the first precision pass ran — indefinitely when that pass was itself
    /// stuck in an indexer. The caller must skip and keep Tier-0.
    ///
    /// The scope is the project, not the process: a different project must not
    /// be refused precision just because another project is running it.
    #[test]
    fn test_precision_run_skips_when_the_same_project_already_has_one_in_flight() {
        let _guard = crate::feature_flags::lock_flag_tests();
        // Simulate a precision run already in flight on THIS project.
        let project_lock = precision_run_lock(Path::new("/fixture/project"));
        let held = project_lock
            .try_lock()
            .expect("no other test or thread may hold this project's precision lock here");

        let mut pdg = ProgramDependenceGraph::new();
        pdg.add_node(Node {
            id: "src/main.py:main".to_owned(),
            node_type: NodeType::Function,
            name: "main".to_owned(),
            file_path: Arc::from("src/main.py"),
            byte_range: (0, 4),
            complexity: 1,
            language: "python".to_owned(),
        });

        let started = Instant::now();
        let report = run_precision_ingest(&mut pdg, Path::new("/fixture/project"));
        let elapsed = started.elapsed();
        drop(held);

        // Blocking would deadlock the test; the 5s ceiling turns that into a
        // failure instead of a hung suite.
        assert!(
            report.skipped_concurrent_run,
            "a refused pass must say it was skipped, not report an empty run"
        );
        assert_eq!(report.definitions_seen, 0);
        assert!(
            elapsed < Duration::from_secs(5),
            "precision pass waited {elapsed:?} on a lock held by another run"
        );
    }

    /// The other half of the invariant: the guards are keyed per project, so a
    /// run on one project must not be refused because a *different* project is
    /// mid-pass. A single process-global mutex made every unrelated index and
    /// every test that indexes a scratch repo contend with all the others.
    #[test]
    fn test_precision_run_guard_is_per_project_not_process_global() {
        let _guard = crate::feature_flags::lock_flag_tests();
        // Bind the Arcs: a guard borrows the mutex, so the Arc owning it must
        // outlive the guard.
        let one = precision_run_lock(Path::new("/fixture/one"));
        let two = precision_run_lock(Path::new("/fixture/two"));
        let held = one
            .try_lock()
            .expect("no other test or thread may hold this project's precision lock here");

        // Same project: a second run must be refused, or it would queue behind
        // the first while holding that project's write lock.
        assert!(
            one.try_lock().is_err(),
            "a second precision run on the same project must be refused"
        );
        // Different project: must be acquirable. A process-global mutex failed
        // exactly here, serializing unrelated projects and starving them.
        assert!(
            two.try_lock().is_ok(),
            "an unrelated project must not be blocked by another project's precision run"
        );
        drop(held);
    }

    #[cfg(unix)]
    #[test]
    fn test_precision_run_cleans_nested_scip_output_directory_on_ingest_failure() {
        let _guard = crate::feature_flags::lock_flag_tests();
        let (directory, executable) = shell_fixture("printf invalid-scip > \\\"$2\\\"");
        let mut pdg = ProgramDependenceGraph::new();
        pdg.add_node(Node {
            id: "src/main.py:main".to_owned(),
            node_type: NodeType::Function,
            name: "main".to_owned(),
            file_path: Arc::from("src/main.py"),
            byte_range: (0, 4),
            complexity: 1,
            language: "python".to_owned(),
        });

        unsafe {
            std::env::set_var("LEINDEX_SCIP_PYTHON_BIN", &executable);
            std::env::set_var("LEINDEX_SCIP_MIN_AVAILABLE_MB", "0");
            std::env::set_var("LEINDEX_SCIP_TIMEOUT_SECS", "1");
        }
        let report = run_precision_ingest(&mut pdg, directory.path());
        unsafe {
            std::env::remove_var("LEINDEX_SCIP_PYTHON_BIN");
            std::env::remove_var("LEINDEX_SCIP_MIN_AVAILABLE_MB");
            std::env::remove_var("LEINDEX_SCIP_TIMEOUT_SECS");
        }

        assert_eq!(report, PrecisionReport::default());
        assert!(directory.path().join(".leindex/jobs").is_dir());
        assert!(
            directory
                .path()
                .join(".leindex/jobs")
                .read_dir()
                .unwrap()
                .next()
                .is_none()
        );
    }

    #[cfg(unix)]
    #[test]
    fn test_precision_attempt_throttled_within_interval_and_forced() {
        use std::os::unix::fs::PermissionsExt;
        let _guard = crate::feature_flags::lock_flag_tests();

        let directory = tempfile::tempdir().unwrap();
        let ran_marker = directory.path().join("indexer_ran");
        let executable = directory.path().join("fixture.sh");
        std::fs::write(
            &executable,
            format!(
                "#!/bin/sh\ntouch {}\ncp /dev/null \"$2\"\n",
                ran_marker.display()
            ),
        )
        .unwrap();
        let mut permissions = std::fs::metadata(&executable).unwrap().permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&executable, permissions).unwrap();

        let mut pdg = ProgramDependenceGraph::new();
        pdg.add_node(Node {
            id: "src/main.py:main".to_owned(),
            node_type: NodeType::Function,
            name: "main".to_owned(),
            file_path: std::sync::Arc::from("src/main.py"),
            byte_range: (0, 4),
            complexity: 1,
            language: "python".to_owned(),
        });

        // A fresh attempt marker dated "now" must suppress the run entirely.
        let project_root = directory.path().join("project");
        std::fs::create_dir_all(project_root.join(".leindex")).unwrap();
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        std::fs::write(
            project_root.join(".leindex/precision_attempt"),
            now.to_string(),
        )
        .unwrap();

        unsafe {
            std::env::set_var("LEINDEX_SCIP_PYTHON_BIN", &executable);
            std::env::set_var("LEINDEX_SCIP_MIN_AVAILABLE_MB", "0");
            std::env::set_var("LEINDEX_SCIP_TIMEOUT_SECS", "5");
        }
        let throttled = run_precision_ingest(&mut pdg, &project_root);
        assert_eq!(throttled, PrecisionReport::default());
        assert!(
            !ran_marker.is_file(),
            "throttled attempt must not spawn the indexer"
        );

        // LEINDEX_SCIP_FORCE=1 overrides the throttle and records a new attempt.
        unsafe {
            std::env::set_var("LEINDEX_SCIP_FORCE", "1");
        }
        let forced = run_precision_ingest(&mut pdg, &project_root);
        unsafe {
            std::env::remove_var("LEINDEX_SCIP_FORCE");
            std::env::remove_var("LEINDEX_SCIP_PYTHON_BIN");
            std::env::remove_var("LEINDEX_SCIP_MIN_AVAILABLE_MB");
            std::env::remove_var("LEINDEX_SCIP_TIMEOUT_SECS");
        }
        assert_eq!(forced, PrecisionReport::default()); // empty .scip degrades
        assert!(
            ran_marker.is_file(),
            "forced attempt must spawn the indexer"
        );
    }
}

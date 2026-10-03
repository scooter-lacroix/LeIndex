//! Optional SCIP precision ingestion for the Tier-0 PDG.
//!
//! The precision tier is deliberately additive: when an external SCIP indexer
//! is unavailable or a run is refused by its resource rails, callers retain the
//! existing tree-sitter PDG unchanged.

use std::collections::BTreeSet;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use crate::graph::pdg::ProgramDependenceGraph;

pub mod indexer;
pub mod merge;
pub mod scip_ingest;

pub use indexer::{IndexerSpec, discover_indexer, run_indexer};
pub use merge::{PrecisionReport, merge_facts};
pub use scip_ingest::{
    CompactScipFacts, DefinitionFact, RelationshipFact, ScipIngestError, ingest_bytes,
};

static PRECISION_RUN_LOCK: OnceLock<Mutex<()>> = OnceLock::new();
static PRECISION_RUN_COUNTER: AtomicU64 = AtomicU64::new(1);

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
pub fn run_precision_ingest(
    pdg: &mut ProgramDependenceGraph,
    project_root: &Path,
) -> PrecisionReport {
    let mut total = PrecisionReport::default();
    let _run_guard = PRECISION_RUN_LOCK
        .get_or_init(|| Mutex::new(()))
        .lock()
        .expect("SCIP precision run lock poisoned");
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
    if let Some(available_kib) = mem_available_kib() {
        if available_kib < minimum_mb.saturating_mul(1024) {
            tracing::info!(
                available_mb = available_kib / 1024,
                minimum_mb,
                "SCIP precision skipped below MemAvailable floor"
            );
            return total;
        }
    }

    let timeout_secs = std::env::var("LEINDEX_SCIP_TIMEOUT_SECS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(600);
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
        let output = output_dir.join(format!("{language}.scip"));
        let result = run_indexer(
            &spec,
            project_root,
            &output,
            Duration::from_secs(timeout_secs),
        );
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

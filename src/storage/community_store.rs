//! Persistence for Leiden community results (roadmap Part IV).
//!
//! One batched transaction per index run — the session-9 fsync lesson applied
//! from day one. `intel_communities` carries the algorithm/quality/resolution
//! identity so a stale partition from different parameters can never be
//! served (the plan's cache-key warning honored in the data model).

use rusqlite::params;

use super::schema::Storage;
use crate::graph::community::{CommunityStats, community_label, community_relevant};
use crate::graph::pdg::{NodeId, ProgramDependenceGraph};

/// Build node-key assignments and labels from a detected partition.
pub fn assignments_for_pdg(
    pdg: &ProgramDependenceGraph,
    communities: &std::collections::HashMap<NodeId, u32>,
) -> (Vec<(String, u32)>, Vec<(u32, usize, String)>) {
    let assignments = communities
        .iter()
        .filter_map(|(&node_id, &community)| {
            pdg.get_node(node_id)
                .filter(|node| community_relevant(&node.node_type))
                .map(|node| (node.id.clone(), community))
        })
        .collect();
    let mut members: std::collections::HashMap<u32, Vec<NodeId>> = std::collections::HashMap::new();
    for (&node_id, &community) in communities {
        members.entry(community).or_default().push(node_id);
    }
    let labels = members
        .into_iter()
        .map(|(community, members)| (community, members.len(), community_label(pdg, &members)))
        .collect();
    (assignments, labels)
}

/// Compute, attach, and persist communities for a completed PDG.
pub fn compute_and_persist(
    storage: &mut Storage,
    project_id: &str,
    pdg: &mut ProgramDependenceGraph,
) -> rusqlite::Result<CommunityStats> {
    let (communities, stats) = crate::graph::community::detect_communities(pdg);
    pdg.communities = communities.clone();
    let (assignments, labels) = assignments_for_pdg(pdg, &communities);
    save_communities_by_node_id(
        storage,
        project_id,
        crate::graph::community::COMMUNITY_ALGORITHM,
        crate::graph::community::COMMUNITY_QUALITY,
        crate::graph::community::COMMUNITY_RESOLUTION,
        stats.quality,
        stats.recompute_ms,
        &assignments,
        &labels,
    )?;
    Ok(stats)
}

/// Persist a full community assignment for a project, replacing any previous
/// run's rows for that (algorithm, quality, resolution) identity.
pub fn save_communities(
    storage: &mut Storage,
    project_id: &str,
    algorithm: &str,
    quality_name: &str,
    resolution: f64,
    quality_score: f64,
    recompute_ms: u64,
    assignments: &[(i64, i64)],    // (node db id, community)
    labels: &[(i64, usize, &str)], // (community, node_count, label)
) -> rusqlite::Result<()> {
    let tx = storage.conn_mut().transaction()?;

    tx.execute(
        "DELETE FROM intel_communities WHERE project_id = ?1 AND algorithm = ?2 \
         AND quality_name = ?3 AND resolution = ?4",
        params![project_id, algorithm, quality_name, resolution],
    )?;
    apply_assignments(&tx, project_id, assignments)?;
    insert_labels(
        &tx,
        project_id,
        algorithm,
        quality_name,
        resolution,
        quality_score,
        labels,
    )?;

    // Timing metric (singleton row).
    tx.execute(
        "UPDATE cache_telemetry SET community_recompute_ms = ?1 WHERE id = 1",
        params![recompute_ms],
    )?;

    tx.commit()
}

/// Bring `intel_nodes.community_id` to exactly `assignments` ((node db id,
/// community)), writing only the rows that differ.
///
/// Nodes filtered out of the current projection (external markers, doc
/// sections) must not keep a previous run's community, so persisted ids absent
/// from `assignments` are cleared. Rewriting every row through one dynamic
/// statement per community (thousands of them, most singletons) cost ~0.5 s per
/// index; a diff through two prepared statements writes nothing when the
/// communities did not change.
fn apply_assignments(
    tx: &rusqlite::Transaction<'_>,
    project_id: &str,
    assignments: &[(i64, i64)],
) -> rusqlite::Result<()> {
    let mut existing: std::collections::HashMap<i64, i64> = std::collections::HashMap::new();
    {
        let mut stmt = tx.prepare(
            "SELECT id, community_id FROM intel_nodes \
             WHERE project_id = ?1 AND community_id IS NOT NULL",
        )?;
        let rows = stmt.query_map(params![project_id], |row| {
            Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?))
        })?;
        for row in rows {
            let (id, community) = row?;
            existing.insert(id, community);
        }
    }
    let desired: std::collections::HashMap<i64, i64> = assignments.iter().copied().collect();

    let mut clear =
        tx.prepare_cached("UPDATE intel_nodes SET community_id = NULL WHERE id = ?1")?;
    for id in existing.keys().filter(|id| !desired.contains_key(id)) {
        clear.execute(params![id])?;
    }
    let mut set = tx.prepare_cached("UPDATE intel_nodes SET community_id = ?1 WHERE id = ?2")?;
    for (id, community) in &desired {
        if existing.get(id) != Some(community) {
            set.execute(params![community, id])?;
        }
    }
    Ok(())
}

/// Insert the community metadata rows through one prepared statement.
fn insert_labels(
    tx: &rusqlite::Transaction<'_>,
    project_id: &str,
    algorithm: &str,
    quality_name: &str,
    resolution: f64,
    quality_score: f64,
    labels: &[(i64, usize, &str)],
) -> rusqlite::Result<()> {
    let computed_at = chrono::Utc::now().timestamp();
    let mut insert = tx.prepare_cached(
        "INSERT INTO intel_communities \
         (project_id, community, algorithm, quality_name, resolution, node_count, \
          quality_score, label, computed_at) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
    )?;
    for &(community, node_count, label) in labels {
        insert.execute(params![
            project_id,
            community,
            algorithm,
            quality_name,
            resolution,
            node_count,
            quality_score,
            label,
            computed_at,
        ])?;
    }
    Ok(())
}

/// String-keyed variant used by the indexing pipeline: resolves node db ids
/// via a single `intel_nodes` lookup, then delegates to [`save_communities`].
pub fn save_communities_by_node_id(
    storage: &mut Storage,
    project_id: &str,
    algorithm: &str,
    quality_name: &str,
    resolution: f64,
    quality_score: f64,
    recompute_ms: u64,
    assignments: &[(String, u32)],
    labels: &[(u32, usize, String)],
) -> rusqlite::Result<()> {
    let tx = storage.conn_mut().transaction()?;
    // One SELECT building the (db id, community) pairs.
    let mut id_pairs: Vec<(i64, i64)> = Vec::with_capacity(assignments.len());
    {
        let mut stmt =
            tx.prepare("SELECT id FROM intel_nodes WHERE project_id = ?1 AND node_id = ?2")?;
        for (node_id, community) in assignments {
            if let Ok(db_id) =
                stmt.query_row(params![project_id, node_id], |row| row.get::<_, i64>(0))
            {
                id_pairs.push((db_id, i64::from(*community)));
            }
        }
    }
    let i64_labels: Vec<(i64, usize, &str)> = labels
        .iter()
        .map(|&(community, count, ref label)| (i64::from(community), count, label.as_str()))
        .collect();
    // Run the same persist logic inside this transaction by inlining the
    // statements (save_communities opens its own transaction; we already
    // hold one).
    tx.execute(
        "DELETE FROM intel_communities WHERE project_id = ?1 AND algorithm = ?2 \
         AND quality_name = ?3 AND resolution = ?4",
        params![project_id, algorithm, quality_name, resolution],
    )?;
    apply_assignments(&tx, project_id, &id_pairs)?;
    insert_labels(
        &tx,
        project_id,
        algorithm,
        quality_name,
        resolution,
        quality_score,
        &i64_labels,
    )?;
    tx.execute(
        "UPDATE cache_telemetry SET community_recompute_ms = ?1 WHERE id = 1",
        params![recompute_ms],
    )?;
    tx.commit()
}

/// Hydrate the in-memory PDG membership map from persisted assignments.
///
/// Community ids are persisted against the stable storage node key. The PDG
/// loader reconstructs fresh `NodeIndex` values, so this lookup deliberately
/// joins through `node_id` rather than assuming database ids are graph ids.
pub fn load_community_memberships(
    storage: &Storage,
    project_id: &str,
    pdg: &mut ProgramDependenceGraph,
) -> rusqlite::Result<usize> {
    let mut stmt = storage.conn().prepare(
        "SELECT node_id, community_id FROM intel_nodes \
         WHERE project_id = ?1 AND community_id IS NOT NULL",
    )?;
    let rows = stmt.query_map(params![project_id], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, u32>(1)?))
    })?;
    let mut loaded = 0;
    for row in rows {
        let (node_id, community) = row?;
        if let Some(graph_id) = pdg.find_by_id(&node_id) {
            pdg.communities.insert(graph_id, community);
            loaded += 1;
        }
    }
    Ok(loaded)
}

/// Load community labels for a project under the current identity.
pub fn load_community_labels(
    storage: &Storage,
    project_id: &str,
    algorithm: &str,
    quality_name: &str,
    resolution: f64,
) -> rusqlite::Result<Vec<(i64, usize, String)>> {
    let mut stmt = storage.conn().prepare(
        "SELECT community, node_count, COALESCE(label, '') FROM intel_communities \
         WHERE project_id = ?1 AND algorithm = ?2 AND quality_name = ?3 AND resolution = ?4 \
         ORDER BY node_count DESC",
    )?;
    let rows = stmt.query_map(
        params![project_id, algorithm, quality_name, resolution],
        |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, i64>(1)? as usize,
                row.get::<_, String>(2)?,
            ))
        },
    )?;
    rows.collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_save_and_load_roundtrip() {
        let temp = tempfile::NamedTempFile::new().unwrap();
        let mut storage = Storage::open(temp.path()).unwrap();

        // Seed three nodes so UPDATE ... WHERE id IN has real rows.
        for id in 1..=3i64 {
            storage
                .conn_mut()
                .execute(
                    "INSERT INTO intel_nodes (project_id, file_path, node_id, symbol_name, \
                     qualified_name, language, node_type, content_hash, created_at, updated_at) \
                     VALUES ('proj', 'f.rs', ?1, 'n', 'n', 'rust', 'function', 'h', 0, 0)",
                    [id],
                )
                .unwrap();
        }

        save_communities(
            &mut storage,
            "proj",
            COMMUNITY_ALGORITHM_TEST,
            "modularity",
            1.0,
            0.42,
            7,
            &[(1, 0), (2, 0), (3, 1)],
            &[(0, 2, "src/alpha"), (1, 1, "src/beta")],
        )
        .unwrap();

        let labels = load_community_labels(
            &storage,
            "proj",
            COMMUNITY_ALGORITHM_TEST,
            "modularity",
            1.0,
        )
        .unwrap();
        assert_eq!(labels.len(), 2);
        assert_eq!(labels[0].2, "src/alpha"); // largest first

        // Node assignments persisted.
        let community: Option<i64> = storage
            .conn()
            .query_row(
                "SELECT community_id FROM intel_nodes WHERE id = 3",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(community, Some(1));

        // Timing metric recorded.
        let ms: i64 = storage
            .conn()
            .query_row(
                "SELECT community_recompute_ms FROM cache_telemetry WHERE id = 1",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(ms, 7);
    }

    const COMMUNITY_ALGORITHM_TEST: &str = "leiden";

    #[cfg(feature = "community")]
    #[test]
    fn test_load_community_memberships_uses_node_keys() {
        let temp = tempfile::NamedTempFile::new().unwrap();
        let mut storage = Storage::open(temp.path()).unwrap();
        storage
            .conn_mut()
            .execute(
                "INSERT INTO intel_nodes (project_id, file_path, node_id, symbol_name, \
                 qualified_name, language, node_type, content_hash, created_at, updated_at, \
                 community_id) VALUES ('proj', 'f.rs', 'stable::node', 'node', 'node', \
                 'rust', 'function', 'h', 0, 0, 7)",
                [],
            )
            .unwrap();

        let mut pdg = ProgramDependenceGraph::new();
        let graph_id = pdg.add_node(crate::graph::pdg::Node {
            id: "stable::node".to_string(),
            node_type: crate::graph::pdg::NodeType::Function,
            name: "node".to_string(),
            file_path: std::sync::Arc::from("f.rs"),
            byte_range: (0, 1),
            complexity: 1,
            language: "rust".to_string(),
        });
        assert_eq!(
            load_community_memberships(&storage, "proj", &mut pdg).unwrap(),
            1
        );
        assert_eq!(pdg.communities.get(&graph_id), Some(&7));
    }
}

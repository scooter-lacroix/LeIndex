//! Persistence for Leiden community labels and derived memberships.
//!
//! Algorithm, quality, and resolution identify each persisted partition so a
//! stale membership set cannot be served after its detection parameters change.
//! Graph nodes remain exclusively in the PDG generation layer; the SQLite
//! membership table is derived metadata keyed by stable node id.

use rusqlite::params;

use super::schema::Storage;
use crate::graph::community::{CommunityStats, community_label, community_relevant};
use crate::graph::pdg::{NodeId, ProgramDependenceGraph};

/// Build stable node-key assignments and labels from a detected partition.
pub fn assignments_for_pdg(
    pdg: &ProgramDependenceGraph,
    communities: &std::collections::HashMap<NodeId, u32>,
) -> (Vec<(String, u32)>, Vec<(u32, usize, String)>) {
    let mut assignments: Vec<(String, u32)> = communities
        .iter()
        .filter_map(|(&node_id, &community)| {
            pdg.get_node(node_id)
                .filter(|node| community_relevant(&node.node_type))
                .map(|node| (node.id.clone(), community))
        })
        .collect();
    assignments.sort_unstable_by(|a, b| a.0.cmp(&b.0));

    let mut members: std::collections::HashMap<u32, Vec<NodeId>> = std::collections::HashMap::new();
    for (&node_id, &community) in communities {
        members.entry(community).or_default().push(node_id);
    }
    let mut labels: Vec<(u32, usize, String)> = members
        .into_iter()
        .map(|(community, members)| (community, members.len(), community_label(pdg, &members)))
        .collect();
    labels.sort_unstable_by_key(|entry| entry.0);
    (assignments, labels)
}

/// Compute, attach, and persist memberships for a completed PDG.
pub fn compute_and_persist(
    storage: &mut Storage,
    project_id: &str,
    pdg: &mut ProgramDependenceGraph,
) -> rusqlite::Result<CommunityStats> {
    let (communities, stats) = crate::graph::community::detect_communities(pdg);
    pdg.communities = communities
        .iter()
        .map(|(&node_id, &community)| (node_id, community))
        .collect();
    let (assignments, labels) = assignments_for_pdg(pdg, &communities);
    save_communities_by_node_id(
        storage,
        project_id,
        pdg,
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

/// Persist community labels and stable node-id memberships for one identity.
/// Membership rows are validated against the authoritative in-memory PDG.
pub fn save_communities_by_node_id(
    storage: &mut Storage,
    project_id: &str,
    pdg: &ProgramDependenceGraph,
    algorithm: &str,
    quality_name: &str,
    resolution: f64,
    quality_score: f64,
    recompute_ms: u64,
    assignments: &[(String, u32)],
    labels: &[(u32, usize, String)],
) -> rusqlite::Result<()> {
    let valid_node_ids: std::collections::HashSet<&str> = pdg
        .node_indices()
        .filter_map(|index| pdg.get_node(index))
        .filter(|node| community_relevant(&node.node_type))
        .map(|node| node.id.as_str())
        .collect();
    let mut desired = std::collections::HashMap::<String, u32>::new();
    for (node_id, community) in assignments {
        if valid_node_ids.contains(node_id.as_str()) {
            desired.insert(node_id.clone(), *community);
        }
    }
    let mut desired: Vec<(String, u32)> = desired.into_iter().collect();
    desired.sort_unstable_by(|a, b| a.0.cmp(&b.0));

    let tx = storage.conn_mut().transaction()?;
    tx.execute(
        "DELETE FROM intel_community_memberships WHERE project_id = ?1 AND algorithm = ?2 \
         AND quality_name = ?3 AND resolution = ?4",
        params![project_id, algorithm, quality_name, resolution],
    )?;
    let mut membership_insert = tx.prepare_cached(
        "INSERT INTO intel_community_memberships \
         (project_id, algorithm, quality_name, resolution, node_id, community) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
    )?;
    for (node_id, community) in &desired {
        membership_insert.execute(params![
            project_id,
            algorithm,
            quality_name,
            resolution,
            node_id,
            community,
        ])?;
    }
    drop(membership_insert);

    tx.execute(
        "DELETE FROM intel_communities WHERE project_id = ?1 AND algorithm = ?2 \
         AND quality_name = ?3 AND resolution = ?4",
        params![project_id, algorithm, quality_name, resolution],
    )?;
    insert_labels(
        &tx,
        project_id,
        algorithm,
        quality_name,
        resolution,
        quality_score,
        labels,
    )?;
    tx.execute(
        "UPDATE cache_telemetry SET community_recompute_ms = ?1 WHERE id = 1",
        params![recompute_ms],
    )?;
    tx.commit()
}

/// Insert community labels using one prepared statement.
fn insert_labels(
    tx: &rusqlite::Transaction<'_>,
    project_id: &str,
    algorithm: &str,
    quality_name: &str,
    resolution: f64,
    quality_score: f64,
    labels: &[(u32, usize, String)],
) -> rusqlite::Result<()> {
    let computed_at = chrono::Utc::now().timestamp();
    let mut insert = tx.prepare_cached(
        "INSERT INTO intel_communities \
         (project_id, community, algorithm, quality_name, resolution, node_count, \
          quality_score, label, computed_at) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
    )?;
    for (community, node_count, label) in labels {
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

/// Hydrate memberships from derived records, intersecting them with live PDG
/// node ids and excluding graph nodes outside the community projection.
pub fn load_community_memberships(
    storage: &Storage,
    project_id: &str,
    algorithm: &str,
    quality_name: &str,
    resolution: f64,
    pdg: &mut ProgramDependenceGraph,
) -> rusqlite::Result<usize> {
    let mut stmt = storage.conn().prepare(
        "SELECT node_id, community FROM intel_community_memberships \
         WHERE project_id = ?1 AND algorithm = ?2 AND quality_name = ?3 AND resolution = ?4",
    )?;
    let rows = stmt.query_map(
        params![project_id, algorithm, quality_name, resolution],
        |row| Ok((row.get::<_, String>(0)?, row.get::<_, u32>(1)?)),
    )?;
    let mut loaded = 0;
    for row in rows {
        let (node_id, community) = row?;
        if let Some(graph_id) = pdg.find_by_id(&node_id) {
            if pdg
                .get_node(graph_id)
                .is_some_and(|node| community_relevant(&node.node_type))
            {
                pdg.communities.insert(graph_id, community);
                loaded += 1;
            }
        }
    }
    Ok(loaded)
}

/// Load labels for a project under the selected community identity.
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
    use crate::graph::pdg::{Node, NodeType};
    use std::sync::Arc;

    const TEST_ALGORITHM: &str = "leiden";
    const TEST_QUALITY: &str = "modularity";
    const TEST_RESOLUTION: f64 = 1.0;

    fn node(id: &str, path: &str, name: &str, node_type: NodeType) -> Node {
        Node {
            id: id.to_string(),
            node_type,
            name: name.to_string(),
            file_path: Arc::from(path),
            byte_range: (0, 10),
            complexity: 1,
            language: "rust".to_string(),
        }
    }

    #[cfg(feature = "community")]
    #[test]
    fn test_membership_roundtrip_uses_derived_table_and_validates_graph_ids() {
        let temp = tempfile::NamedTempFile::new().unwrap();
        let mut storage = Storage::open(temp.path()).unwrap();
        let mut pdg = ProgramDependenceGraph::new();
        let first = pdg.add_node(node(
            "src/a.rs:first",
            "src/a.rs",
            "first",
            NodeType::Function,
        ));
        let second = pdg.add_node(node(
            "src/b.rs:second",
            "src/b.rs",
            "second",
            NodeType::Class,
        ));
        let external = pdg.add_node(node(
            "external::third",
            "<external>",
            "third",
            NodeType::External,
        ));

        let assignments = vec![
            ("src/a.rs:first".to_string(), 0),
            ("src/b.rs:second".to_string(), 1),
            ("not/in/graph".to_string(), 9),
            ("external::third".to_string(), 8),
        ];
        let labels = vec![
            (0, 1, "src/a.rs".to_string()),
            (1, 1, "src/b.rs".to_string()),
        ];
        save_communities_by_node_id(
            &mut storage,
            "proj",
            &pdg,
            TEST_ALGORITHM,
            TEST_QUALITY,
            TEST_RESOLUTION,
            0.42,
            7,
            &assignments,
            &labels,
        )
        .unwrap();

        let memberships: Vec<(String, i64)> = storage
            .conn()
            .prepare(
                "SELECT node_id, community FROM intel_community_memberships \
                 WHERE project_id = 'proj' ORDER BY node_id",
            )
            .unwrap()
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        assert_eq!(
            memberships,
            vec![
                ("src/a.rs:first".to_string(), 0),
                ("src/b.rs:second".to_string(), 1)
            ]
        );
        // Memberships are derived metadata: no graph rows are written at all,
        // so nothing can leak into `intel_nodes`.
        let graph_rows: i64 = storage
            .conn()
            .query_row("SELECT COUNT(*) FROM intel_nodes", [], |row| row.get(0))
            .unwrap();
        assert_eq!(graph_rows, 0, "memberships must not write intel_nodes rows");

        // Simulate an obsolete membership whose node is absent from this PDG;
        // hydration ignores it and any non-projection node membership.
        storage
            .conn()
            .execute(
                "INSERT INTO intel_community_memberships \
                 (project_id, algorithm, quality_name, resolution, node_id, community) \
                 VALUES ('proj', ?1, ?2, ?3, 'deleted::node', 9)",
                params![TEST_ALGORITHM, TEST_QUALITY, TEST_RESOLUTION],
            )
            .unwrap();
        let loaded = load_community_memberships(
            &storage,
            "proj",
            TEST_ALGORITHM,
            TEST_QUALITY,
            TEST_RESOLUTION,
            &mut pdg,
        )
        .unwrap();
        assert_eq!(loaded, 2);
        assert_eq!(pdg.communities.get(&first), Some(&0));
        assert_eq!(pdg.communities.get(&second), Some(&1));
        assert!(!pdg.communities.contains_key(&external));

        let labels = load_community_labels(
            &storage,
            "proj",
            TEST_ALGORITHM,
            TEST_QUALITY,
            TEST_RESOLUTION,
        )
        .unwrap();
        assert_eq!(labels.len(), 2);
        assert_eq!(labels[0].2, "src/a.rs");
        let recompute_ms: i64 = storage
            .conn()
            .query_row(
                "SELECT community_recompute_ms FROM cache_telemetry WHERE id = 1",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(recompute_ms, 7);
    }
}

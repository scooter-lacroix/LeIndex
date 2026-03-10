use crate::context::PhaseExecutionContext;
use crate::options::PhaseOptions;
use lerecherche::HybridScorer;
use serde::{Deserialize, Serialize};

/// Single hotspot candidate.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Hotspot {
    /// Node id.
    pub node_id: String,
    /// Composite score.
    pub score: f32,
    /// Complexity value used.
    pub complexity: u32,
    /// Reachability count used.
    pub impact_size: usize,
}

/// Critical-path output.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Phase4Summary {
    /// Ranked hotspot list.
    pub hotspots: Vec<Hotspot>,
}

/// Run phase 4 critical-path analysis.
pub fn run(context: &PhaseExecutionContext, options: &PhaseOptions) -> Phase4Summary {
    let scorer = HybridScorer::new().with_weights(0.45, 0.45, 0.10);
    let keyword_signals = options
        .hotspot_keywords
        .iter()
        .map(|k| k.to_ascii_lowercase())
        .collect::<Vec<_>>();

    let mut max_complexity = 1u32;
    let mut max_impact = 1usize;
    let mut raw = Vec::new();

    for node_idx in context.pdg.node_indices() {
        let Some(node) = context.pdg.get_node(node_idx) else {
            continue;
        };

        let impact = context.pdg.get_forward_impact(node_idx).len();
        max_complexity = max_complexity.max(node.complexity);
        max_impact = max_impact.max(impact);
        raw.push((node.id.clone(), node.complexity, impact, node.name.clone()));
    }

    let mut hotspots = raw
        .into_iter()
        .map(|(node_id, complexity, impact, name)| {
            let complexity_signal = complexity as f32 / max_complexity as f32;
            let impact_signal = impact as f32 / max_impact as f32;
            // Heuristic text signal with configurable keyword list from PhaseOptions.
            let normalized_name = name.to_ascii_lowercase();
            let text_signal = if keyword_signals
                .iter()
                .any(|keyword| normalized_name.contains(keyword))
            {
                1.0
            } else {
                0.2
            };

            let score = scorer
                .score(complexity_signal, impact_signal, text_signal)
                .overall;

            Hotspot {
                node_id,
                score,
                complexity,
                impact_size: impact,
            }
        })
        .collect::<Vec<_>>();

    hotspots.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.node_id.cmp(&b.node_id))
    });

    hotspots.truncate(options.top_n.max(1));
    Phase4Summary { hotspots }
}

#[cfg(test)]
mod tests {
    use super::*;
    use legraphe::pdg::{Node, NodeType, ProgramDependenceGraph};
    use std::collections::HashMap;

    fn context_with_node(name: &str, complexity: u32) -> PhaseExecutionContext {
        let unique = format!(
            "lephase-phase4-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("time")
                .as_nanos()
        );
        let root = std::env::temp_dir().join(unique);
        std::fs::create_dir_all(root.join(".leindex")).expect("mkdir");
        let storage = lestockage::schema::Storage::open(root.join(".leindex").join("leindex.db"))
            .expect("storage");

        let mut pdg = ProgramDependenceGraph::new();
        let _ = pdg.add_node(Node {
            id: format!("src/lib.rs:{name}"),
            node_type: NodeType::Function,
            name: name.to_string(),
            file_path: "src/lib.rs".to_string(),
            byte_range: (0, 1),
            complexity,
            language: "rust".to_string(),
            embedding: None,
        });

        PhaseExecutionContext {
            root,
            project_id: "test".to_string(),
            storage,
            file_inventory: Vec::new(),
            changed_files: Vec::new(),
            deleted_files: Vec::new(),
            parse_results: Vec::new(),
            signatures_by_file: HashMap::new(),
            pdg,
            docs_summary: None,
            generation_hash: "gen".to_string(),
        }
    }

    #[test]
    fn phase4_uses_configurable_hotspot_keywords() {
        let context = context_with_node("payment_authorizer", 10);

        let keyword_hit = run(
            &context,
            &PhaseOptions {
                root: context.root.clone(),
                top_n: 1,
                hotspot_keywords: vec!["authorizer".to_string()],
                ..PhaseOptions::default()
            },
        );

        let keyword_miss = run(
            &context,
            &PhaseOptions {
                root: context.root.clone(),
                top_n: 1,
                hotspot_keywords: vec!["completely-different".to_string()],
                ..PhaseOptions::default()
            },
        );

        assert_eq!(keyword_hit.hotspots.len(), 1);
        assert_eq!(keyword_miss.hotspots.len(), 1);
        assert!(keyword_hit.hotspots[0].score > keyword_miss.hotspots[0].score);
    }
}

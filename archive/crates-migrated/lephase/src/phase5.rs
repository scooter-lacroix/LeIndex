use crate::context::PhaseExecutionContext;
use crate::phase1::Phase1Summary;
use crate::phase2::Phase2Summary;
use crate::phase3::Phase3Summary;
use crate::phase4::Phase4Summary;
use crate::recommendations::{Confidence, Recommendation};
use lestockage::GlobalSymbolTable;
use serde::{Deserialize, Serialize};

/// Final optimization output.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Phase5Summary {
    /// Prioritized recommendations.
    pub recommendations: Vec<Recommendation>,
    /// Cross-project public symbol hint count (if present in storage).
    pub public_symbol_hints: usize,
}

/// Run phase 5 optimization synthesis.
pub fn run(
    context: &PhaseExecutionContext,
    phase1: &Phase1Summary,
    phase2: &Phase2Summary,
    phase3: &Phase3Summary,
    phase4: &Phase4Summary,
) -> Phase5Summary {
    let mut recommendations = Vec::new();

    if phase2.external_import_edges > 0 {
        recommendations.push(Recommendation {
            message: format!(
                "Resolve {} external import edges to improve dependency certainty",
                phase2.external_import_edges
            ),
            priority: 0.95,
            confidence: Confidence::External,
            rationale: "Unresolved imports lower phase-2 confidence and reduce graph precision"
                .to_string(),
        });
    }

    if phase1.parse_failures > 0 {
        recommendations.push(Recommendation {
            message: format!(
                "Fix {} parse failures to stabilize structural scan",
                phase1.parse_failures
            ),
            priority: 0.90,
            confidence: Confidence::Exact,
            rationale: "Failed parses remove symbols from downstream dependency and flow analysis"
                .to_string(),
        });
    }

    if !phase3.focus_files.is_empty() {
        recommendations.push(Recommendation {
            message: format!(
                "Prioritize test/refactor work in top focus file: {}",
                phase3.focus_files[0]
            ),
            priority: 0.80,
            confidence: Confidence::Heuristic,
            rationale: "Phase-3 forward impact indicates high fan-out from selected entry points"
                .to_string(),
        });
    }

    if let Some(top_hotspot) = phase4.hotspots.first() {
        recommendations.push(Recommendation {
            message: format!(
                "Review hotspot {} (complexity={}, impact={})",
                top_hotspot.node_id, top_hotspot.complexity, top_hotspot.impact_size
            ),
            priority: top_hotspot.score,
            confidence: Confidence::Heuristic,
            rationale: "HybridScorer combined complexity + graph impact + textual risk markers"
                .to_string(),
        });
    }

    recommendations.sort_by(|a, b| {
        b.priority
            .partial_cmp(&a.priority)
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    let symbol_table = GlobalSymbolTable::new(&context.storage);
    let public_symbol_hints = symbol_table
        .find_public_symbols(&context.project_id)
        .map(|v| v.len())
        .unwrap_or(0);

    Phase5Summary {
        recommendations,
        public_symbol_hints,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        context::PhaseExecutionContext,
        phase1::Phase1Summary,
        phase2::Phase2Summary,
        phase3::Phase3Summary,
        phase4::{Hotspot, Phase4Summary},
    };
    use std::collections::HashMap;
    use tempfile::tempdir;

    #[test]
    fn phase5_prioritizes_expected_recommendations() {
        let dir = tempdir().expect("tempdir");
        let db_path = dir.path().join(".leindex").join("leindex.db");
        std::fs::create_dir_all(db_path.parent().expect("parent")).expect("mkdir");
        let storage = lestockage::schema::Storage::open(db_path).expect("storage");

        let context = PhaseExecutionContext {
            root: dir.path().to_path_buf(),
            project_id: "test".to_string(),
            storage,
            file_inventory: Vec::new(),
            changed_files: Vec::new(),
            deleted_files: Vec::new(),
            parse_results: Vec::new(),
            signatures_by_file: HashMap::new(),
            pdg: legraphe::pdg::ProgramDependenceGraph::new(),
            docs_summary: None,
            generation_hash: "gen".to_string(),
        };

        let phase1 = Phase1Summary {
            parse_failures: 2,
            ..Phase1Summary::default()
        };
        let phase2 = Phase2Summary {
            external_import_edges: 3,
            ..Phase2Summary::default()
        };
        let phase3 = Phase3Summary {
            focus_files: vec!["src/lib.rs".to_string()],
            ..Phase3Summary::default()
        };
        let phase4 = Phase4Summary {
            hotspots: vec![Hotspot {
                node_id: "src/lib.rs:critical".to_string(),
                score: 0.82,
                complexity: 7,
                impact_size: 10,
            }],
        };

        let summary = run(&context, &phase1, &phase2, &phase3, &phase4);

        assert!(summary.recommendations.len() >= 4);
        assert!(
            summary.recommendations[0].priority >= summary.recommendations[1].priority,
            "recommendations should be sorted by priority desc"
        );
    }

    #[test]
    fn phase5_handles_empty_inputs_without_panicking() {
        let dir = tempdir().expect("tempdir");
        let db_path = dir.path().join(".leindex").join("leindex.db");
        std::fs::create_dir_all(db_path.parent().expect("parent")).expect("mkdir");
        let storage = lestockage::schema::Storage::open(db_path).expect("storage");

        let context = PhaseExecutionContext {
            root: dir.path().to_path_buf(),
            project_id: "test".to_string(),
            storage,
            file_inventory: Vec::new(),
            changed_files: Vec::new(),
            deleted_files: Vec::new(),
            parse_results: Vec::new(),
            signatures_by_file: HashMap::new(),
            pdg: legraphe::pdg::ProgramDependenceGraph::new(),
            docs_summary: None,
            generation_hash: "gen".to_string(),
        };

        let summary = run(
            &context,
            &Phase1Summary::default(),
            &Phase2Summary::default(),
            &Phase3Summary::default(),
            &Phase4Summary::default(),
        );

        assert!(summary.recommendations.is_empty());
    }
}

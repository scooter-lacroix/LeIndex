//! Time cold hydration of a project's graph and search engine:
//! `cargo run --release --example engine_profile -- <project dir>`
use leindex::cli::leindex::LeIndex;
use std::time::Instant;

fn main() {
    let dir = std::env::args().nth(1).expect("project dir");
    for round in 0..3 {
        let t = Instant::now();
        let mut index = LeIndex::new(std::path::Path::new(&dir)).expect("open");
        let open = t.elapsed();
        let t = Instant::now();
        index.ensure_pdg_loaded_graph_only().expect("graph");
        let graph = t.elapsed();
        let t = Instant::now();
        index.ensure_analysis_context_loaded().expect("engine");
        println!(
            "round {round}: open {open:?}, graph {graph:?}, engine {:?}",
            t.elapsed()
        );
    }
}

//! Time PDG hydration stages against a real generation:
//! `cargo run --release --example hydrate_profile -- <generation dir> <project_id>`
use leindex::storage::pdg_store::load_pdg;
use leindex::storage::schema::Storage;
use std::time::Instant;

fn main() {
    let mut args = std::env::args().skip(1);
    let dir = args.next().expect("generation dir");
    let project_id = args.next().expect("project id");
    let db = std::path::Path::new(&dir).join("leindex.db");
    for round in 0..3 {
        let t = Instant::now();
        let storage = Storage::open_readonly(&db).expect("open");
        let open = t.elapsed();
        let t = Instant::now();
        let pdg = load_pdg(&storage, &project_id).expect("load");
        println!(
            "round {round}: open {open:?}, load_pdg {:?} ({} nodes, {} edges)",
            t.elapsed(),
            pdg.node_count(),
            pdg.edge_count()
        );
    }
}

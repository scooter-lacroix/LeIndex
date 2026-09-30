//! Quick manual benchmark: `cargo run --release --example textsearch_bench -- <root> <pattern>...`
use leindex::search::textsearch::*;
use std::{collections::HashMap, path::PathBuf, sync::Arc, time::Instant};

fn main() {
    let mut args = std::env::args().skip(1);
    let root = PathBuf::from(args.next().expect("root"));
    let out = std::env::temp_dir().join("textsearch_bench.idx");
    let t = Instant::now();
    let stats = build_index(&root, &out, HashMap::new()).unwrap();
    println!("build: {:?} in {:?}", stats, t.elapsed());
    let index = Arc::new(TextIndex::open(&out).unwrap());
    for pattern in args {
        for (label, idx) in [("indexed", Some(index.clone())), ("live   ", None)] {
            let q = Query {
                pattern: pattern.clone(),
                regex: pattern.contains(['\\', '[', '(', '|', '*']),
                case: CaseMode::Smart,
                word: false,
            };
            let compiled = q.compile().unwrap();
            let spec = RootSpec {
                root: root.clone(),
                index: idx,
                filter: FileFilter::default(),
            };
            let opts = SearchOptions {
                limit: None,
                collect_hits: false,
                ..Default::default()
            };
            let _ = search(std::slice::from_ref(&spec), &compiled, &opts); // warm
            let t = Instant::now();
            let r = search(std::slice::from_ref(&spec), &compiled, &opts);
            println!(
                "{label} {:<28} {:>7.2?}  files={} lines={} candidates={} scanned={}",
                pattern,
                t.elapsed(),
                r.stats.files_matched,
                r.stats.match_lines,
                r.stats.candidates,
                r.stats.scanned
            );
        }
    }
}

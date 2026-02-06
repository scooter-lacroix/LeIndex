use lephase::{run_phase_analysis, PhaseOptions, PhaseSelection};
use std::fs;
use std::path::Path;
use std::time::Instant;
use tempfile::TempDir;

fn write_project(root: &Path, files: usize) {
    let src = root.join("src");
    fs::create_dir_all(&src).expect("create src");

    for i in 0..files {
        fs::write(
            src.join(format!("mod_{i}.rs")),
            format!("pub fn f_{i}(x:i32)->i32{{ if x>0 {{x+{i}}} else {{x-{i}}} }}\n"),
        )
        .expect("write source");
    }
}

fn write_project_with_imports(root: &Path, files: usize) {
    let src = root.join("src");
    fs::create_dir_all(&src).expect("create src");

    for i in 0..files {
        let import = if i > 0 {
            format!("use crate::mod_{}::f_{};\n", i - 1, i - 1)
        } else {
            String::new()
        };
        let call = if i > 0 {
            format!("let _ = f_{}(x);", i - 1)
        } else {
            String::new()
        };

        fs::write(
            src.join(format!("mod_{i}.rs")),
            format!(
                "{}pub fn f_{}(x:i32)->i32{{ {} if x>0 {{x+{}}} else {{x-{}}} }}\n",
                import, i, call, i, i
            ),
        )
        .expect("write source");
    }
}

fn quantile(sorted: &[u128], q: f64) -> u128 {
    if sorted.is_empty() {
        return 0;
    }
    let idx = ((sorted.len() - 1) as f64 * q).round() as usize;
    sorted[idx]
}

fn run_once(root: &Path) -> u128 {
    let start = Instant::now();
    let _ = run_phase_analysis(
        PhaseOptions {
            root: root.to_path_buf(),
            ..PhaseOptions::default()
        },
        PhaseSelection::All,
    )
    .expect("phase analysis");
    start.elapsed().as_millis()
}

fn summarize(mut values: Vec<u128>) -> (u128, u128, u128) {
    values.sort_unstable();
    (
        quantile(&values, 0.50),
        quantile(&values, 0.95),
        quantile(&values, 0.99),
    )
}

fn main() {
    let iterations = 5;

    // XS: cold/warm/incremental.
    let mut cold = Vec::new();
    for _ in 0..iterations {
        let dir = TempDir::new().expect("tempdir");
        write_project(dir.path(), 20);
        cold.push(run_once(dir.path()));
    }

    let dir = TempDir::new().expect("tempdir");
    write_project(dir.path(), 20);
    let _ = run_once(dir.path()); // warmup

    let mut warm = Vec::new();
    for _ in 0..iterations {
        warm.push(run_once(dir.path()));
    }

    let mut incremental = Vec::new();
    for i in 0..iterations {
        fs::write(
            dir.path().join("src/mod_0.rs"),
            format!(
                "pub fn f_0(x:i32)->i32{{ if x>0 {{x+1}} else {{x-1}} }}\npub fn bump_{i}()->usize{{{i}}}\n"
            ),
        )
        .expect("mutate source");
        incremental.push(run_once(dir.path()));
    }

    // S: import-linked graph with incremental mutation.
    let s_iterations = 3;
    let mut s_cold = Vec::new();
    for _ in 0..s_iterations {
        let dir = TempDir::new().expect("tempdir");
        write_project_with_imports(dir.path(), 240);
        s_cold.push(run_once(dir.path()));
    }

    let s_dir = TempDir::new().expect("tempdir");
    write_project_with_imports(s_dir.path(), 240);
    let _ = run_once(s_dir.path());

    let mut s_incremental = Vec::new();
    for i in 0..s_iterations {
        fs::write(
            s_dir.path().join("src/mod_239.rs"),
            format!(
                "use crate::mod_238::f_238;\npub fn f_239(x:i32)->i32{{ let _ = f_238(x); if x>0 {{x+239}} else {{x-239}} }}\npub fn bump_s_{i}()->usize{{{i}}}\n"
            ),
        )
        .expect("mutate s source");
        s_incremental.push(run_once(s_dir.path()));
    }

    let (cold_p50, cold_p95, cold_p99) = summarize(cold);
    let (warm_p50, warm_p95, warm_p99) = summarize(warm);
    let (inc_p50, inc_p95, inc_p99) = summarize(incremental);
    let (s_cold_p50, s_cold_p95, s_cold_p99) = summarize(s_cold);
    let (s_inc_p50, s_inc_p95, s_inc_p99) = summarize(s_incremental);

    println!(
        "{{\n  \"XS\": {{\n    \"iterations\": {iterations},\n    \"cold_ms\": {{\"p50\": {cold_p50}, \"p95\": {cold_p95}, \"p99\": {cold_p99}}},\n    \"warm_ms\": {{\"p50\": {warm_p50}, \"p95\": {warm_p95}, \"p99\": {warm_p99}}},\n    \"incremental_ms\": {{\"p50\": {inc_p50}, \"p95\": {inc_p95}, \"p99\": {inc_p99}}}\n  }},\n  \"S\": {{\n    \"iterations\": {s_iterations},\n    \"cold_ms\": {{\"p50\": {s_cold_p50}, \"p95\": {s_cold_p95}, \"p99\": {s_cold_p99}}},\n    \"incremental_ms\": {{\"p50\": {s_inc_p50}, \"p95\": {s_inc_p95}, \"p99\": {s_inc_p99}}}\n  }}\n}}"
    );
}

use criterion::{criterion_group, criterion_main, BatchSize, Criterion};
use lephase::{run_phase_analysis, PhaseOptions, PhaseSelection};
use std::fs;
use std::path::Path;
use tempfile::TempDir;

fn write_project(root: &Path, files: usize) {
    let src = root.join("src");
    fs::create_dir_all(&src).expect("create src");

    for i in 0..files {
        let file = src.join(format!("mod_{i}.rs"));
        fs::write(
            file,
            format!("pub fn f_{i}(x:i32)->i32{{ if x>0 {{x+{i}}} else {{x-{i}}} }}\n"),
        )
        .expect("write source");
    }
}

fn write_project_with_imports(root: &Path, files: usize) {
    let src = root.join("src");
    fs::create_dir_all(&src).expect("create src");

    for i in 0..files {
        let file = src.join(format!("mod_{i}.rs"));
        let import = if i > 0 {
            format!("use crate::mod_{}::f_{};\n", i - 1, i - 1)
        } else {
            String::new()
        };
        let call = if i > 0 {
            format!("let _ = f_{}(x);", i - 1)
        } else {
            "".to_string()
        };

        fs::write(
            file,
            format!(
                "{}pub fn f_{}(x:i32)->i32{{ {} if x>0 {{x+{}}} else {{x-{}}} }}\n",
                import, i, call, i, i
            ),
        )
        .expect("write source");
    }
}

fn run_all(root: &Path) {
    let options = PhaseOptions {
        root: root.to_path_buf(),
        max_files: 10_000,
        ..PhaseOptions::default()
    };

    let _ = run_phase_analysis(options, PhaseSelection::All).expect("phase analysis");
}

fn bench_phase_cold_xs(c: &mut Criterion) {
    c.bench_function("phase_all_cold_xs", |b| {
        b.iter_batched(
            || {
                let dir = TempDir::new().expect("tempdir");
                write_project(dir.path(), 20);
                dir
            },
            |dir| run_all(dir.path()),
            BatchSize::SmallInput,
        )
    });
}

fn bench_phase_warm_xs(c: &mut Criterion) {
    let dir = TempDir::new().expect("tempdir");
    write_project(dir.path(), 20);
    run_all(dir.path()); // prime cache

    c.bench_function("phase_all_warm_xs", |b| {
        b.iter(|| run_all(dir.path()));
    });
}

fn bench_phase_incremental_xs(c: &mut Criterion) {
    let dir = TempDir::new().expect("tempdir");
    write_project(dir.path(), 20);
    run_all(dir.path()); // baseline

    let mut counter = 0usize;
    c.bench_function("phase_all_incremental_xs", |b| {
        b.iter(|| {
            let file = dir.path().join("src/mod_0.rs");
            counter += 1;
            fs::write(
                &file,
                format!(
                    "pub fn f_0(x:i32)->i32{{ if x>0 {{x+1}} else {{x-1}} }}\npub fn bump_{counter}()->usize{{{counter}}}\n"
                ),
            )
            .expect("update source");

            run_all(dir.path());
        });
    });
}

fn bench_phase_cold_s_imports(c: &mut Criterion) {
    c.bench_function("phase_all_cold_s_imports", |b| {
        b.iter_batched(
            || {
                let dir = TempDir::new().expect("tempdir");
                write_project_with_imports(dir.path(), 240);
                dir
            },
            |dir| run_all(dir.path()),
            BatchSize::SmallInput,
        )
    });
}

fn bench_phase_incremental_s_imports(c: &mut Criterion) {
    let dir = TempDir::new().expect("tempdir");
    write_project_with_imports(dir.path(), 240);
    run_all(dir.path());

    let mut counter = 0usize;
    c.bench_function("phase_all_incremental_s_imports", |b| {
        b.iter(|| {
            let file = dir.path().join("src/mod_239.rs");
            counter += 1;
            fs::write(
                &file,
                format!(
                    "use crate::mod_238::f_238;\npub fn f_239(x:i32)->i32{{ let _ = f_238(x); if x>0 {{x+239}} else {{x-239}} }}\npub fn bump_s_{counter}()->usize{{{counter}}}\n"
                ),
            )
            .expect("update s source");
            run_all(dir.path());
        });
    });
}

criterion_group!(
    phase_benches,
    bench_phase_cold_xs,
    bench_phase_warm_xs,
    bench_phase_incremental_xs,
    bench_phase_cold_s_imports,
    bench_phase_incremental_s_imports
);
criterion_main!(phase_benches);

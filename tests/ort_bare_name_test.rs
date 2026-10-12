// Process-isolated negative test for the bare-name ORT loadability probe.
//
// `try_init_candidate` verifies bare-name candidates by committing ort's
// process-wide environment and then touching a symbol. A REJECTED non-ORT
// candidate (e.g. libc) still leaves that poisoned environment behind, so
// this test must live in its own binary: in the lib test harness it would
// make every later ort symbol query panic on machines that do have a working
// ORT (observed as 15 collateral failures in `cargo test --lib --features
// onnx embed::`).

#![cfg(feature = "onnx")]

#[test]
fn test_bare_name_probe_rejects_non_ort_dylib() {
    // Find a real, dlopen-able library that is NOT ONNX Runtime (e.g. libc or libm).
    let candidates = [
        "/usr/lib/x86_64-linux-gnu/libc.so.6",
        "/usr/lib/x86_64-linux-gnu/libm.so.6",
        "/usr/lib/libc.so.6",
        "/usr/lib/libm.so.6",
        "/lib64/libc.so.6",
        "/lib/x86_64-linux-gnu/libc.so.6",
    ];
    let non_ort_lib = candidates
        .iter()
        .map(std::path::PathBuf::from)
        .find(|p| p.exists());
    let Some(non_ort_lib) = non_ort_lib else {
        eprintln!("skipping: no non-ORT system dylib found on this platform");
        return;
    };

    let mut searched = Vec::new();
    let mut last_error = None;
    let outcome = leindex::embed::ort_discovery::try_init_candidate(
        leindex::embed::ort_discovery::DiscoverySource::System,
        non_ort_lib.clone(),
        false,
        &mut searched,
        &mut last_error,
    );

    assert!(
        outcome.is_none(),
        "dlopen-able non-ORT library must fail the symbol verification probe, not return Initialized"
    );
    assert!(
        searched.iter().any(|(source, path)| *source
            == leindex::embed::ort_discovery::DiscoverySource::System
            && path == &non_ort_lib.display().to_string()),
        "failed candidate must be recorded in searched list: {searched:?}"
    );
    assert!(
        last_error.is_some(),
        "failure reason must be recorded in last_error"
    );
}

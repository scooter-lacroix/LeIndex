use super::*;

/// Read the value of a `flag value` pair that setup pinned into `args`.
///
/// Returns `None` when the flag is absent, so a test can assert that the
/// ceiling contract is actually in force rather than silently skipped.
/// Delegates to the production `flag_value` so both spellings (`-flag value`
/// and `-flag=value`) are read the same way the repair code writes them.
fn batch_value(args: &[String], flag: &str) -> Option<u64> {
    flag_value(args, flag)?.parse().ok()
}

fn external() -> LeIndexConfig {
    let mut config = LeIndexConfig::default();
    let embedding = &mut config.indexing.embeddings;
    embedding.remote_provider = "custom".into();
    embedding.remote_endpoint = Some("http://127.0.0.1:9000/v1".into());
    embedding.remote_model = Some("fixture".into());
    embedding.remote_dimension = Some(1024);
    embedding.remote_api_key_env = Some("LEINDEX_TEST_UNSET_KEY_NAME".into());
    embedding.remote_doc_template = Some("document {text}".into());
    embedding.remote_query_template = Some("query {text}".into());
    config
}

#[test]
fn test_external_rerun_preserves_unspecified_fields() {
    let mut config = external();
    let before = serde_json::to_value(&config.indexing.embeddings).unwrap();
    configure_external(&mut config, &SetupOptions::default()).unwrap();
    assert_eq!(
        before,
        serde_json::to_value(&config.indexing.embeddings).unwrap()
    );
}

#[test]
fn test_external_empty_flags_clear_templates_and_authentication() {
    let mut config = external();
    let options = SetupOptions {
        embedding_api_key_env: Some(String::new()),
        embedding_doc_template: Some(String::new()),
        embedding_query_template: Some(String::new()),
        ..Default::default()
    };
    configure_external(&mut config, &options).unwrap();
    assert!(config.indexing.embeddings.remote_api_key_env.is_none());
    assert!(config.indexing.embeddings.remote_api_key.is_none());
    assert!(config.indexing.embeddings.remote_doc_template.is_none());
    assert!(config.indexing.embeddings.remote_query_template.is_none());
    assert_eq!(config.indexing.embeddings.remote_dimension, Some(1024));
}

#[test]
fn test_external_rejects_url_query_fragment_and_zero_dimension() {
    for endpoint in [
        "http://127.0.0.1:9000/v1?token=not-a-secret",
        "http://127.0.0.1:9000/v1#fragment",
    ] {
        let mut config = external();
        let before = serde_json::to_value(&config).unwrap();
        let options = SetupOptions {
            embedding_endpoint: Some(endpoint.into()),
            ..Default::default()
        };
        assert!(configure_external(&mut config, &options).is_err());
        assert_eq!(before, serde_json::to_value(&config).unwrap());
    }
    assert!(
        configure_external(
            &mut external(),
            &SetupOptions {
                embedding_dimension: Some(0),
                ..Default::default()
            }
        )
        .is_err()
    );
}

#[test]
fn test_strip_gpu_options_handles_a_trailing_flag() {
    for flag in ["-ngl", "--gpu-layers", "--n-gpu-layers"] {
        let mut args = vec!["--ctx-size".into(), "8192".into(), flag.into()];
        strip_gpu_options(&mut args);
        assert_eq!(args, ["--ctx-size", "8192"]);
    }
}

#[test]
fn test_gpu_arguments_preserve_and_override_without_duplicates() {
    let mut args: Vec<String> = [
        "--ctx-size",
        "8192",
        "--batch-size",
        "8192",
        "--embedding",
        "--pooling",
        "mean",
        "--gpu-layers=0",
    ]
    .into_iter()
    .map(String::from)
    .collect();
    let before = args.clone();
    configure_args(&mut args, None, false).unwrap();
    // The batch ceilings are appended exactly once, after the operator's own
    // arguments, which are left untouched.
    assert_eq!(&args[..before.len()], &before[..]);
    assert_eq!(
        &args[before.len()..],
        [
            "-b",
            &EMBEDDING_BATCH.to_string(),
            "-ub",
            &EMBEDDING_UBATCH_MIN.to_string()
        ]
    );
    // Idempotent: a rerun must not append a second copy.
    let after_once = args.clone();
    configure_args(&mut args, None, false).unwrap();
    assert_eq!(args, after_once);
    configure_args(&mut args, Some(7), false).unwrap();
    assert_eq!(gpu_layers(&args).unwrap(), Some(7));
    assert!(args.windows(2).any(|pair| pair == ["--batch-size", "8192"]));
    assert_eq!(
        args.iter()
            .filter(|arg| arg.as_str() == "--embedding")
            .count(),
        1
    );
    assert!(configure_args(&mut args, None, true).is_err());
    configure_args(&mut args, Some(0), true).unwrap();
    assert_eq!(gpu_layers(&args).unwrap(), Some(0));
    assert!(gpu_layers(&["-ngl".into()]).is_err());
    assert!(configure_args(&mut vec!["--pooling".into()], None, false).is_err());
}

#[test]
fn test_launch_arguments_never_guess_a_host_specific_device() {
    // `--device` names are per-host (`Vulkan1` is one machine's discrete GPU and
    // names nothing on a CUDA or CPU llama build), so setup must never invent
    // one: a wrong name makes the managed server fail to start at all.
    let mut args = Vec::new();
    configure_args(&mut args, None, false).unwrap();
    assert!(
        !args.iter().any(|arg| arg.starts_with("--device")),
        "setup must not pin a device it cannot know: {args:?}"
    );
    // Batch ceilings ARE pinned — but for a different reason than the device.
    // A batch ceiling is not an inference about the host: it is the other half
    // of a contract with this client. The client admits documents up to 64 KiB
    // (`BatchBudget`), a 64 KiB Rust document tokenizes to ~21,846 tokens, and
    // llama-server answers HTTP 500 for any input larger than its logical batch
    // — which the caller's failure budget then counts. Leaving `-ub` at the
    // server default is what produced 92,605 rejections in one measured run.
    let ubatch = batch_value(&args, "-ub").expect("setup must pin the logical batch ceiling");
    let batch = batch_value(&args, "-b").expect("setup must pin the physical batch ceiling");
    assert!(
        ubatch >= 21_846,
        "-ub must cover the largest legal 64 KiB document (~21,846 tokens): {ubatch}"
    );
    assert!(
        batch >= ubatch,
        "llama-server requires the physical batch to be at least the logical batch: \
         -b {batch} < -ub {ubatch}"
    );
    // Every pinned ceiling stays operator-overridable (see `add_if_absent`).
    assert!(args.windows(2).any(|pair| pair == ["-ngl", "99"]));

    // A CPU launch gets no GPU offload at all.
    let mut cpu = Vec::new();
    configure_args(&mut cpu, Some(0), true).unwrap();
    assert!(cpu.windows(2).any(|pair| pair == ["-ngl", "0"]));
    assert!(!cpu.iter().any(|arg| arg.starts_with("--device")));
}

/// A stale ceiling written by an EARLIER LeIndex build must be raised, not
/// preserved. `add_if_absent` treats any present flag as operator intent, which
/// would strand every existing install on `-ub 24576` forever — re-running
/// setup, the one action a user takes to pick up a corrected default, would
/// change nothing while the server kept answering HTTP 500 for documents this
/// client admits.
#[test]
fn test_stale_batch_ceilings_are_raised_but_larger_operator_values_survive() {
    // Exactly what the live managed service was running (measured via ps).
    let mut stale: Vec<String> = ["-ngl", "99", "-b", "24576", "-ub", "24576"]
        .into_iter()
        .map(String::from)
        .collect();
    configure_args(&mut stale, None, false).unwrap();
    assert_eq!(
        batch_value(&stale, "-ub"),
        Some(EMBEDDING_UBATCH_MIN),
        "a stale -ub below the largest document must be raised, got {stale:?}"
    );
    assert!(
        batch_value(&stale, "-b").is_some_and(|batch| batch >= EMBEDDING_UBATCH_MIN),
        "-b must be raised with -ub so llama-server still starts: {stale:?}"
    );
    // A repaired install must converge on what a FRESH install gets, or it runs
    // with a smaller physical batch than the measured optimum forever.
    assert_eq!(
        batch_value(&stale, "-b"),
        Some(EMBEDDING_BATCH),
        "a repaired -b must converge on a fresh install's value, got {stale:?}"
    );
    // The existing offload layer must not be disturbed by the repair.
    assert_eq!(gpu_layers(&stale).unwrap(), Some(99));

    // An operator who deliberately configured MORE headroom keeps it.
    let mut generous: Vec<String> = ["-ub", "65536", "-b"]
        .into_iter()
        .map(String::from)
        .collect();
    configure_args(&mut generous, None, false).unwrap();
    assert_eq!(
        batch_value(&generous, "-ub"),
        Some(65536),
        "a larger operator value is never lowered: {generous:?}"
    );

    // The `-flag=value` spelling is repaired too, not just the split form.
    let mut equals_form: Vec<String> = ["-ub=24576"].into_iter().map(String::from).collect();
    configure_args(&mut equals_form, None, false).unwrap();
    assert_eq!(batch_value(&equals_form, "-ub"), Some(EMBEDDING_UBATCH_MIN));
}

/// Repairing the ceiling must be idempotent: the second run is a no-op.
#[test]
fn test_batch_ceiling_repair_is_idempotent() {
    let mut args = vec!["-ub".to_string(), "24576".to_string()];
    configure_args(&mut args, None, false).unwrap();
    let repaired = args.clone();
    configure_args(&mut args, None, false).unwrap();
    assert_eq!(args, repaired, "a repaired ceiling must not keep changing");
}

/// llama-server refuses to START when `-b < -ub`, so the repair must never leave
/// that state behind — even when the operator's `-ub` is far above the floor and
/// the `-b` sitting next to it is a stale default.
#[test]
fn test_repaired_physical_batch_always_covers_the_effective_logical_batch() {
    // A stale `-b` next to a deliberately raised `-ub`: raising `-b` only to the
    // floor constant would produce `-b 32768 < -ub 65536`, a server that will
    // not boot.
    let mut args: Vec<String> = ["-b", "24576", "-ub", "65536"]
        .into_iter()
        .map(String::from)
        .collect();
    configure_args(&mut args, None, false).unwrap();
    let batch = batch_value(&args, "-b").expect("-b present");
    let ubatch = batch_value(&args, "-ub").expect("-ub present");
    assert_eq!(ubatch, 65536, "the operator's logical batch is preserved");
    assert!(
        batch >= ubatch,
        "llama-server would refuse to start: -b {batch} < -ub {ubatch} ({args:?})"
    );
}

#[test]
fn test_managed_public_binding_and_invalid_timeout_fail_before_runtime_io() {
    for host in ["0.0.0.0", "::", "example.com", ""] {
        let mut config = LeIndexConfig::default();
        let options = SetupOptions {
            download_llama: Some("cpu".into()),
            embedding_host: Some(host.into()),
            ..Default::default()
        };
        let result = configure_gemma(&mut config, &options)
            .unwrap_err()
            .to_string();
        assert!(result.contains("loopback"), "{result}");
        assert!(config.services.is_empty());
    }
    let mut config = LeIndexConfig::default();
    config.services.insert(
        "gemma".into(),
        crate::config::ServiceConfig {
            start_timeout_secs: 0,
            ..Default::default()
        },
    );
    let result = configure_gemma(
        &mut config,
        &SetupOptions {
            download_llama: Some("cpu".into()),
            ..Default::default()
        },
    )
    .unwrap_err()
    .to_string();
    assert!(result.contains("timeout"));
    assert!(require_neural(false).is_err());
    assert!(require_neural(true).is_ok());
}

#[cfg(target_os = "linux")]
#[test]
fn test_managed_rerun_preserves_alias_autostart_and_extra_arguments() {
    use std::os::unix::fs::PermissionsExt;
    let temp = tempfile::tempdir().unwrap();
    let binary = temp.path().join("llama-server");
    let model = temp.path().join("fixture.gguf");
    std::fs::write(&binary, b"#!/bin/sh\nprintf 'fixture version\\n'\n").unwrap();
    std::fs::set_permissions(&binary, std::fs::Permissions::from_mode(0o755)).unwrap();
    std::fs::write(&model, b"fixture").unwrap();
    let service = crate::config::ServiceConfig {
        binary: binary.to_str().unwrap().into(),
        model: Some(model.to_str().unwrap().into()),
        alias: Some("custom-gemma".into()),
        port: 8080,
        auto_start: false,
        extra_args: [
            "--embedding",
            "--pooling",
            "mean",
            "-ngl",
            "0",
            "--batch-size",
            "8192",
        ]
        .into_iter()
        .map(String::from)
        .collect(),
        ..Default::default()
    };
    let mut config = LeIndexConfig::default();
    config.services.insert("gemma".into(), service.clone());
    configure_gemma(&mut config, &SetupOptions::default()).unwrap();
    let configured = &config.services["gemma"];
    // Operator identity is preserved across a rerun...
    assert_eq!(configured.binary, service.binary);
    assert_eq!(configured.model, service.model);
    assert_eq!(configured.alias, service.alias);
    assert_eq!(configured.host, service.host);
    assert_eq!(configured.port, service.port);
    assert_eq!(configured.auto_start, service.auto_start);
    assert_eq!(configured.start_timeout_secs, service.start_timeout_secs);
    // ...their explicit arguments survive untouched (no duplicate flags)...
    // Strip each ceiling as a flag/value PAIR: the values are derived from the
    // constants, so they are matched by their rendered form.
    let batch_tokens = [
        "-b".to_string(),
        "-ub".to_string(),
        EMBEDDING_BATCH.to_string(),
        EMBEDDING_UBATCH_MIN.to_string(),
    ];
    let blanked: Vec<&String> = configured
        .extra_args
        .iter()
        .filter(|arg| !batch_tokens.contains(*arg))
        .collect();
    let expected: Vec<&String> = service.extra_args.iter().collect();
    assert_eq!(
        blanked, expected,
        "a rerun must preserve the operator's arguments exactly"
    );
    // ...the batch ceilings end up present exactly once...
    assert_eq!(
        configured
            .extra_args
            .iter()
            .filter(|arg| arg.as_str() == "-b")
            .count(),
        1,
        "a rerun must not duplicate -b"
    );
    assert_eq!(
        configured
            .extra_args
            .iter()
            .filter(|arg| arg.as_str() == "-ub")
            .count(),
        1,
        "a rerun must not duplicate -ub"
    );
    // ...and the resolved model still drives the embedding route.
    assert_eq!(
        config.indexing.embeddings.remote_model.as_deref(),
        Some("custom-gemma")
    );
}

#[test]
fn test_external_rerun_preserves_legacy_auth_and_local_model_override() {
    let mut config = external();
    config.indexing.embeddings.remote_api_key =
        Some(String::from_iter(['f', 'i', 'x', 't', 'u', 'r', 'e']));
    config.indexing.embeddings.onnx_model_path = Some("/fixture/local.onnx".into());
    let before = serde_json::to_value(&config.indexing.embeddings).unwrap();
    configure_external(&mut config, &SetupOptions::default()).unwrap();
    assert!(before == serde_json::to_value(&config.indexing.embeddings).unwrap());
    configure_external(
        &mut config,
        &SetupOptions {
            embedding_api_key_env: Some(String::new()),
            ..Default::default()
        },
    )
    .unwrap();
    assert!(config.indexing.embeddings.remote_api_key.is_none());
}

#[test]
fn test_qwen_clears_remote_resolution() {
    let mut config = external();
    let choices = SetupChoices {
        neural_enabled: true,
        provider: None,
        remote_gemma: false,
        options: SetupOptions {
            embedding: Some("qwen3".into()),
            ..Default::default()
        },
    };
    configure(&mut config, &choices).unwrap();
    assert!(
        config
            .indexing
            .embeddings
            .resolved_remote_with(&config.services)
            .is_none()
    );
}

use super::*;

/// Read the value of a batch-ceiling option that setup pinned into `args`.
///
/// Returns `None` when no spelling of the option is present, so a test can
/// assert that the ceiling contract is actually in force rather than silently
/// skipped. Delegates to the production `flag_value` so every spelling
/// (`-flag value`, `--flag value`, `-flag=value`, `--flag=value`) is read the
/// same way the repair code writes them.
fn batch_value(args: &[String], flags: &[&str]) -> Option<u64> {
    flag_value(args, flags)?.parse().ok()
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
        "2048",
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
    // The operator's arguments keep their order, arity and spelling. The only
    // thing that changes is the stale ceiling VALUE, repaired in the spelling it
    // was found in: `-b` and `--batch-size` are ONE llama-server option and the
    // LAST occurrence wins, so appending `-b` beside `--batch-size 2048` would
    // outvote the operator instead of repairing them.
    assert_eq!(
        &args[..before.len()],
        [
            "--ctx-size",
            "8192",
            "--batch-size",
            &EMBEDDING_BATCH.to_string(),
            "--embedding",
            "--pooling",
            "mean",
            "--gpu-layers=0",
        ]
    );
    // Only the logical ceiling is appended; the physical one was repaired in
    // place rather than duplicated.
    assert_eq!(
        &args[before.len()..],
        ["-ub", &EMBEDDING_UBATCH_MIN.to_string()]
    );
    // Idempotent: a rerun must not append a second copy.
    let after_once = args.clone();
    configure_args(&mut args, None, false).unwrap();
    assert_eq!(args, after_once);
    configure_args(&mut args, Some(7), false).unwrap();
    assert_eq!(gpu_layers(&args).unwrap(), Some(7));
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
    let ubatch =
        batch_value(&args, &UBATCH_FLAGS).expect("setup must pin the logical batch ceiling");
    let batch =
        batch_value(&args, &BATCH_FLAGS).expect("setup must pin the physical batch ceiling");
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
/// preserved. `add_if_absent` treats any present option as operator intent,
/// which would strand every existing install on `-ub 24576` forever — re-running
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
        batch_value(&stale, &UBATCH_FLAGS),
        Some(EMBEDDING_UBATCH_MIN),
        "a stale -ub below the largest document must be raised, got {stale:?}"
    );
    assert!(
        batch_value(&stale, &BATCH_FLAGS).is_some_and(|batch| batch >= EMBEDDING_UBATCH_MIN),
        "-b must be raised with -ub so llama-server still starts: {stale:?}"
    );
    // A repaired install must converge on what a FRESH install gets, or it runs
    // with a smaller physical batch than the measured optimum forever.
    assert_eq!(
        batch_value(&stale, &BATCH_FLAGS),
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
        batch_value(&generous, &UBATCH_FLAGS),
        Some(65536),
        "a larger operator value is never lowered: {generous:?}"
    );

    // The `-flag=value` spelling is repaired too, not just the split form.
    let mut equals_form: Vec<String> = ["-ub=24576"].into_iter().map(String::from).collect();
    configure_args(&mut equals_form, None, false).unwrap();
    assert_eq!(
        batch_value(&equals_form, &UBATCH_FLAGS),
        Some(EMBEDDING_UBATCH_MIN)
    );
}

/// The operator's spelling of a ceiling is the one that must be repaired.
///
/// `-b`/`--batch-size` are ONE llama-server option and every occurrence assigns,
/// so a later `-b` outvotes an earlier `--batch-size`. Appending the canonical
/// short form beside an operator's long form therefore overrode their
/// deliberate value — the exact opposite of the "operator's explicit value
/// always wins" contract. Repair in place, in the spelling found, and stay put.
#[test]
fn test_long_form_ceiling_is_repaired_in_the_spelling_the_operator_used() {
    // A long-form ceiling the API server would accept, next to nothing else.
    let mut args: Vec<String> = ["--batch-size", "32768"]
        .into_iter()
        .map(String::from)
        .collect();
    configure_args(&mut args, None, false).unwrap();
    // Still exactly ONE occurrence: no `-b` was appended to outvote it.
    assert_eq!(
        args.iter()
            .filter(|arg| BATCH_FLAGS.contains(&arg.as_str()))
            .count(),
        1,
        "a canonically-spelled duplicate must not be appended: {args:?}"
    );
    // The operator's value and spelling both survive.
    assert!(
        args.windows(2)
            .any(|pair| pair == ["--batch-size", "32768"]),
        "the operator's argument is preserved verbatim: {args:?}"
    );
    // The ceiling setup owns is still supplied, exactly once.
    assert_eq!(
        args.iter()
            .filter(|arg| UBATCH_FLAGS.contains(&arg.as_str()))
            .count(),
        1,
        "the absent logical ceiling is appended once: {args:?}"
    );
    assert_eq!(
        batch_value(&args, &UBATCH_FLAGS),
        Some(EMBEDDING_UBATCH_MIN),
        "setup must still pin the logical ceiling it owns: {args:?}"
    );

    // A long-form `--ubatch-size` below the floor is repaired in place too.
    let mut long_ubatch: Vec<String> = ["--ubatch-size", "24576"]
        .into_iter()
        .map(String::from)
        .collect();
    configure_args(&mut long_ubatch, None, false).unwrap();
    assert_eq!(
        batch_value(&long_ubatch, &UBATCH_FLAGS),
        Some(EMBEDDING_UBATCH_MIN),
        "a stale long-form logical ceiling must be raised: {long_ubatch:?}"
    );
    assert!(
        long_ubatch
            .windows(2)
            .any(|pair| pair == ["--ubatch-size", "32768"]),
        "repaired in the spelling it was found in: {long_ubatch:?}"
    );

    // Two spellings of the SAME option with conflicting values: the effective
    // one is the LAST, and that is the one repaired.
    let mut conflicting: Vec<String> = ["-ub", "65536", "--ubatch-size", "24576"]
        .into_iter()
        .map(String::from)
        .collect();
    configure_args(&mut conflicting, None, false).unwrap();
    let raised = EMBEDDING_UBATCH_MIN.to_string();
    assert_eq!(
        flag_value(&conflicting, &UBATCH_FLAGS),
        Some(raised.as_str()),
        "the LAST occurrence is what llama-server runs on, so it is what is repaired: \
         {conflicting:?}"
    );
    // The operator's deliberate 65536 is not clobbered by the repair.
    assert!(
        conflicting.windows(2).any(|pair| pair == ["-ub", "65536"]),
        "an unrelated larger value is untouched: {conflicting:?}"
    );
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
    let batch = batch_value(&args, &BATCH_FLAGS).expect("-b present");
    let ubatch = batch_value(&args, &UBATCH_FLAGS).expect("-ub present");
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
    // Strip each ceiling as a flag/value PAIR, in ANY spelling: a ceiling the
    // operator already expressed is repaired in place, so it keeps its position,
    // its spelling and its arity — only its value may change. Matching the flag
    // alone would leave a repaired value behind and compare it as if it were one
    // of the operator's own arguments.
    let ceiling_flag = |arg: &str| BATCH_FLAGS.contains(&arg) || UBATCH_FLAGS.contains(&arg);
    let without_ceilings = |args: &[String]| -> Vec<String> {
        let mut kept = Vec::new();
        let mut skip_value = false;
        for arg in args {
            if skip_value {
                skip_value = false;
            } else if ceiling_flag(arg) {
                skip_value = true;
            } else {
                kept.push(arg.clone());
            }
        }
        kept
    };
    assert_eq!(
        without_ceilings(&configured.extra_args),
        without_ceilings(&service.extra_args),
        "a rerun must preserve the operator's arguments exactly"
    );
    // ...the ceilings end up present exactly once, in the operator's spelling...
    for (flags, value) in [
        (
            &BATCH_FLAGS[..],
            batch_value(&configured.extra_args, &BATCH_FLAGS),
        ),
        (
            &UBATCH_FLAGS[..],
            batch_value(&configured.extra_args, &UBATCH_FLAGS),
        ),
    ] {
        assert_eq!(
            configured
                .extra_args
                .iter()
                .filter(|arg| flags.contains(&arg.as_str()))
                .count(),
            1,
            "a rerun must not duplicate {flags:?}"
        );
        assert!(value.is_some(), "setup must pin {flags:?}");
    }
    // ...the stale 8192 the operator was carrying is repaired, not preserved...
    assert_eq!(
        batch_value(&configured.extra_args, &BATCH_FLAGS),
        Some(EMBEDDING_BATCH),
        "a stale ceiling must converge on a fresh install's value: {:?}",
        configured.extra_args
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

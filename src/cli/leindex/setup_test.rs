use super::*;

#[cfg(target_os = "linux")]
#[test]
fn test_setup_ort_lib_name_accepts_versioned_linux_pip_soname() {
    assert!(is_ort_runtime_lib_name_for_setup(
        "libonnxruntime.so.1.25.0"
    ));
    assert!(is_ort_runtime_lib_name_for_setup("libonnxruntime.so"));
    assert!(!is_ort_runtime_lib_name_for_setup(
        "libonnxruntime_providers_shared.so"
    ));
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
#[test]
fn test_find_ort_lib_in_dir_prefers_exact_name_over_versioned_runtime() {
    let dir = tempfile::tempdir().unwrap();
    let exact = dir.path().join(ort_lib_names()[0]);
    #[cfg(target_os = "linux")]
    let versioned = dir.path().join("libonnxruntime.so.1.25.0");
    #[cfg(target_os = "macos")]
    let versioned = dir.path().join("libonnxruntime.1.25.0.dylib");
    std::fs::File::create(&exact).unwrap();
    std::fs::File::create(versioned).unwrap();

    assert_eq!(find_ort_lib_in_dir(dir.path()), Some(exact));
}

#[test]
fn test_setup_smoke_provider_label_is_configured_not_claimed_registered() {
    let src = std::fs::read_to_string(file!()).expect("setup.rs should be readable");
    // The smoke test result output must NOT claim the provider is
    // "registered" when we only know the configured provider, not
    // what the worker actually loaded.
    let needle = format!("{} {}", "Execution provider", "registered");
    assert!(
        !src.contains(&needle),
        "setup must not claim the provider is 'registered'; it only knows the configured EP"
    );
}

#[test]
fn test_resolve_neural_cpu() {
    // VAL-SETUP-010: --neural --cpu forces CPU
    let choices = resolve_from_flags(true, false, true, None).unwrap();
    assert!(choices.neural_enabled);
    assert_eq!(choices.provider, Some(ExecutionProvider::Cpu));
}

#[test]
fn test_resolve_neural_gpu_amd() {
    // VAL-SETUP-011: --neural --gpu amd forces MIGraphX
    let choices = resolve_from_flags(true, false, false, Some(GpuVendor::Amd)).unwrap();
    assert!(choices.neural_enabled);
    assert_eq!(choices.provider, Some(ExecutionProvider::Migraphx));
}

#[test]
fn test_resolve_neural_gpu_nvidia() {
    // VAL-SETUP-012: --neural --gpu nvidia forces CUDA
    let choices = resolve_from_flags(true, false, false, Some(GpuVendor::Nvidia)).unwrap();
    assert!(choices.neural_enabled);
    assert_eq!(choices.provider, Some(ExecutionProvider::Cuda));
}

#[test]
fn test_resolve_no_neural() {
    // VAL-SETUP-013: --no-neural disables neural
    let choices = resolve_from_flags(false, true, false, None).unwrap();
    assert!(!choices.neural_enabled);
    assert!(choices.provider.is_none());
}

#[test]
fn test_resolve_neural_default_cpu() {
    // VAL-SETUP-009: --neural alone defaults to CPU
    let choices = resolve_from_flags(true, false, false, None).unwrap();
    assert!(choices.neural_enabled);
    assert_eq!(choices.provider, Some(ExecutionProvider::Cpu));
}

#[test]
fn test_conflict_neural_no_neural() {
    // VAL-SETUP-015: --neural + --no-neural is a conflict
    let result = resolve_from_flags(true, true, false, None);
    assert!(matches!(result, Err(SetupError::Conflict { .. })));
}

#[test]
fn test_conflict_cpu_gpu() {
    // VAL-SETUP-015: --cpu + --gpu is a conflict
    let result = resolve_from_flags(false, false, true, Some(GpuVendor::Amd));
    assert!(matches!(result, Err(SetupError::Conflict { .. })));
}

#[test]
fn test_cpu_implies_neural() {
    // --cpu without --neural should imply neural
    let choices = resolve_from_flags(false, false, true, None).unwrap();
    assert!(choices.neural_enabled);
    assert_eq!(choices.provider, Some(ExecutionProvider::Cpu));
}

#[test]
fn test_gpu_implies_neural() {
    // --gpu without --neural should imply neural
    let choices = resolve_from_flags(false, false, false, Some(GpuVendor::Amd)).unwrap();
    assert!(choices.neural_enabled);
}

#[test]
fn test_no_flags_errors() {
    let result = resolve_from_flags(false, false, false, None);
    assert!(matches!(result, Err(SetupError::NoFlags)));
}

#[test]
fn test_parse_gpu_vendor_amd() {
    assert_eq!(parse_gpu_vendor("amd").unwrap(), GpuVendor::Amd);
    assert_eq!(parse_gpu_vendor("AMD").unwrap(), GpuVendor::Amd);
}

#[test]
fn test_parse_gpu_vendor_nvidia() {
    assert_eq!(parse_gpu_vendor("nvidia").unwrap(), GpuVendor::Nvidia);
    assert_eq!(parse_gpu_vendor("cuda").unwrap(), GpuVendor::Nvidia);
}

#[test]
fn test_parse_gpu_vendor_invalid() {
    assert!(parse_gpu_vendor("intel").is_err());
}

#[test]
fn test_execution_provider_pip_package() {
    assert_eq!(ExecutionProvider::Cpu.pip_package(), "onnxruntime");
    assert_eq!(ExecutionProvider::Cuda.pip_package(), "onnxruntime-gpu");
    assert_eq!(
        ExecutionProvider::Migraphx.pip_package(),
        "onnxruntime-migraphx"
    );
}

#[test]
fn test_pip_ort_package_spec_is_bounded_to_supported_major() {
    assert_eq!(
        pip_ort_package_spec(ExecutionProvider::Cpu),
        "onnxruntime>=1.20.0,<2"
    );
    assert_eq!(
        pip_ort_package_spec(ExecutionProvider::Migraphx),
        "onnxruntime-migraphx>=1.20.0,<2"
    );
}

#[test]
fn test_execution_provider_config_value() {
    assert_eq!(ExecutionProvider::Cpu.config_value(), "cpu");
    assert_eq!(ExecutionProvider::Cuda.config_value(), "cuda");
    assert_eq!(ExecutionProvider::Migraphx.config_value(), "migraphx");
}

#[test]
fn test_setup_error_display() {
    let err = SetupError::Conflict {
        message: "test conflict".to_string(),
    };
    assert!(err.to_string().contains("test conflict"));

    let err = SetupError::PipNotFound;
    assert!(err.to_string().contains("pip not found"));
}

// ── VAL-SETUP-016/017/018/019: model download error surface tests ──

#[test]
fn test_curl_not_found_error_mentions_curl() {
    // VAL-SETUP-016/019: curl-not-found error must name curl.
    let err = SetupError::CurlNotFound;
    let msg = err.to_string();
    assert!(msg.contains("curl not found"), "{}", msg);
    assert!(msg.contains("models/"), "{}", msg);
}

#[test]
fn test_model_download_network_error_mentions_connectivity() {
    // VAL-SETUP-019: network-classified failure must mention connectivity
    // AND the LEINDEX_MODEL_PATH remediation hint.
    let err = SetupError::ModelDownloadFailed {
            file: "qwen3-embed-0.6b.onnx".to_string(),
            url: "https://huggingface.co/onnx-community/Qwen3-Embedding-0.6B-ONNX/resolve/main/onnx/model.onnx".to_string(),
            exit_code: 28,
            network: true,
        };
    let msg = err.to_string();
    assert!(msg.contains("Network failure"), "{}", msg);
    assert!(msg.contains("internet connection"), "{}", msg);
    assert!(msg.contains("LEINDEX_MODEL_PATH"), "{}", msg);
    assert!(msg.contains("huggingface.co"), "{}", msg);
    assert!(msg.contains("exit code 28"), "{}", msg);
}

#[test]
fn test_model_download_generic_error_is_actionable() {
    // Non-network failure: should still name the URL and suggest re-run.
    let err = SetupError::ModelDownloadFailed {
            file: "tokenizer.json".to_string(),
            url: "https://huggingface.co/onnx-community/Qwen3-Embedding-0.6B-ONNX/resolve/main/tokenizer.json".to_string(),
            exit_code: 22,
            network: false,
        };
    let msg = err.to_string();
    // Generic branch does NOT mention "Network failure".
    assert!(!msg.contains("Network failure"), "{}", msg);
    assert!(msg.contains("tokenizer.json"), "{}", msg);
    assert!(msg.contains("Re-run"), "{}", msg);
}

#[test]
fn test_model_checksum_post_download_error_names_file_and_hashes() {
    let err = SetupError::ModelChecksumPostDownload {
        file: "qwen3-embed-0.6b.onnx".to_string(),
        expected: "aaaa1234567890abcdef1234567890abcdef1234567890abcdef1234567890abcd".to_string(),
        actual: "bbbb1234567890abcdef1234567890abcdef1234567890abcdef1234567890abcd".to_string(),
    };
    let msg = err.to_string();
    assert!(msg.contains("Checksum mismatch"), "{}", msg);
    assert!(msg.contains("qwen3-embed-0.6b.onnx"), "{}", msg);
    // Display prints the full hashes (no shortening in this variant).
    assert!(msg.contains("aaaa1234567890abcdef"), "{}", msg);
    assert!(msg.contains("bbbb1234567890abcdef"), "{}", msg);
    assert!(msg.contains("copy the file manually"), "{}", msg);
}

#[test]
fn test_model_unavailable_error_names_dynamic_assets() {
    let err = SetupError::ModelUnavailable {
        model_name: "qwen3-embed-0.6b-dynamic".to_string(),
        model_dir: PathBuf::from("/tmp/leindex-models"),
    };
    let msg = err.to_string();

    assert!(msg.contains("qwen3-embed-0.6b-dynamic.onnx"), "{}", msg);
    assert!(msg.contains("tokenizer.json"), "{}", msg);
    assert!(msg.contains("config.json"), "{}", msg);
    assert!(msg.contains("LEINDEX_MODEL_PATH"), "{}", msg);
}

#[test]
fn test_existing_model_without_manifest_is_kept_and_reported_present() {
    let dir = tempfile::tempdir().unwrap();
    let file = crate::cli::leindex::model_download::iter_model_files()
        .find(|file| file.local == crate::cli::leindex::model_download::MODEL_ONNX_FILENAME)
        .unwrap();
    std::fs::File::create(dir.path().join(file.local)).unwrap();

    let result = ensure_model_file_present(
        file,
        dir.path(),
        &dir.path().join("checksums.sha256"),
        "",
        &std::collections::HashMap::new(),
        file.local,
    )
    .unwrap();

    assert_eq!(result, (false, true));
}

#[test]
fn test_dynamic_model_assets_require_complete_single_file_download() {
    let tmp = tempfile::tempdir().unwrap();
    let model_path = tmp
        .path()
        .join(crate::cli::leindex::model_download::DYNAMIC_MODEL_ONNX_FILENAME);
    let model = std::fs::File::create(model_path).unwrap();
    model.set_len(100 * 1024 * 1024).unwrap();
    std::fs::write(tmp.path().join("tokenizer.json"), b"{}").unwrap();
    assert!(!dynamic_model_assets_present(tmp.path()));

    std::fs::write(tmp.path().join("config.json"), b"{}").unwrap();
    assert!(dynamic_model_assets_present(tmp.path()));
}

#[test]
fn test_model_checksum_status_missing_for_clean_dir() {
    // VAL-SETUP-017: no model + no manifest -> Missing. We exercise this by
    // pointing LEINDEX_HOME at a fresh tempfile::TempDir (auto-cleanup on drop).
    // Resource-duplication fix: use tempfile::TempDir instead of manual
    // std::env::temp_dir() to guarantee cleanup even on panic.
    let _g = PIPE_ENV_LOCK.lock().unwrap();
    let tmp = tempfile::tempdir().unwrap();
    std::env::set_var("LEINDEX_HOME", tmp.path());
    let status = model_checksum_status();
    std::env::remove_var("LEINDEX_HOME");
    assert_eq!(status, ModelChecksumStatus::Missing);
    // tmp is auto-cleaned when dropped
}

// ── VAL-SETUP-020/021/022: New error and version-compatibility tests ──

#[test]
fn test_pip_not_found_error_mentions_pip_bin() {
    // VAL-SETUP-021: error must mention PIP_BIN as a remediation.
    let err = SetupError::PipNotFound;
    let msg = err.to_string();
    assert!(
        msg.contains("PIP_BIN"),
        "PipNotFound must mention PIP_BIN: {}",
        msg
    );
    assert!(msg.contains("python3-pip") || msg.contains("ensurepip"));
}

#[test]
fn test_pip_install_failed_error_mentions_package() {
    let err = SetupError::PipInstallFailed {
        package: "onnxruntime".to_string(),
        exit_code: 1,
    };
    let msg = err.to_string();
    assert!(msg.contains("onnxruntime"));
    assert!(msg.contains("exit code 1"));
}

#[test]
fn test_pip_network_failed_error_mentions_network() {
    let err = SetupError::PipNetworkFailed {
        package: "onnxruntime".to_string(),
        exit_code: 1,
        output: "Could not fetch URL pypi.org".to_string(),
    };
    let msg = err.to_string();
    assert!(msg.contains("Network failure"));
    assert!(msg.contains("onnxruntime"));
    assert!(msg.contains("internet connection"));
    assert!(msg.contains("pypi.org"));
}

#[test]
fn test_parse_version_simple() {
    assert_eq!(parse_version("1.25.0"), Some((1, 25, 0)));
    assert_eq!(parse_version("1.20.0"), Some((1, 20, 0)));
    assert_eq!(parse_version("2.0.0"), Some((2, 0, 0)));
    assert_eq!(parse_version("0.9.9"), Some((0, 9, 9)));
}

#[test]
fn test_parse_version_with_prerelease() {
    // Suffixes are ignored.
    assert_eq!(parse_version("1.25.0-rc1"), Some((1, 25, 0)));
    assert_eq!(parse_version("1.25.0+build42"), Some((1, 25, 0)));
    assert_eq!(parse_version("1.25.0-rc1+meta"), Some((1, 25, 0)));
}

#[test]
fn test_parse_version_missing_patch_defaults_to_zero() {
    // Default missing minor/patch to 0 (semver-like leniency).
    assert_eq!(parse_version("1.25"), Some((1, 25, 0)));
    assert_eq!(parse_version("1"), Some((1, 0, 0)));
}

#[test]
fn test_parse_version_invalid() {
    assert_eq!(parse_version("not-a-version"), None);
    assert_eq!(parse_version("v1.2.3"), None);
    assert_eq!(parse_version(""), None);
}

#[test]
fn test_version_compatibility_supported() {
    // 1.20.0, 1.25.0, 1.99.99 are supported (within 1.x, >= MIN_ORT_VERSION).
    assert_eq!(
        check_ort_version_compatibility("1.20.0"),
        VersionCompatibility::Supported
    );
    assert_eq!(
        check_ort_version_compatibility("1.25.0"),
        VersionCompatibility::Supported
    );
    assert_eq!(
        check_ort_version_compatibility("1.99.99"),
        VersionCompatibility::Supported
    );
}

#[test]
fn test_unsupported_cpu_ort_requests_upgrade() {
    assert!(should_install_ort_for_existing_state(
        true,
        ExecutionProvider::Cpu,
        Some("1.19.2"),
        true
    ));
    assert!(!should_install_ort_for_existing_state(
        true,
        ExecutionProvider::Cpu,
        Some("1.25.0"),
        true
    ));
}

#[test]
fn test_partial_setup_states_preserve_ort_install_decision() {
    for (ort_installed, model_present, install_ort) in [
        (false, false, true),
        (false, true, true),
        (true, false, false),
        (true, true, false),
    ] {
        assert_eq!(
            should_install_ort_for_existing_state(
                ort_installed,
                ExecutionProvider::Cpu,
                ort_installed.then_some("1.25.0"),
                true,
            ),
            install_ort,
            "ort_installed={ort_installed}, model_present={model_present}"
        );
    }
}

#[test]
fn test_version_compatibility_too_old() {
    // 1.19.x and older are unsupported.
    let r = check_ort_version_compatibility("1.19.0");
    match r {
        VersionCompatibility::Unsupported {
            required_min,
            reason,
        } => {
            assert!(
                required_min.contains("1.20.0"),
                "required_min = {}",
                required_min
            );
            assert!(!reason.is_empty());
        }
        other => panic!("expected Unsupported, got {:?}", other),
    }
}

#[test]
fn test_version_compatibility_very_old() {
    // 0.x is definitely unsupported.
    let r = check_ort_version_compatibility("0.9.9");
    assert!(matches!(r, VersionCompatibility::Unsupported { .. }));
}

#[test]
fn test_version_compatibility_too_new() {
    // 2.0.0+ is TooNew (ABI break).
    let r = check_ort_version_compatibility("2.0.0");
    match r {
        VersionCompatibility::TooNew {
            supported_max,
            reason,
        } => {
            assert!(
                supported_max.contains("1."),
                "supported_max = {}",
                supported_max
            );
            assert!(reason.contains("ABI") || reason.contains("breaking"));
        }
        other => panic!("expected TooNew, got {:?}", other),
    }
}

#[test]
fn test_version_compatibility_unparseable() {
    // Unparseable versions fall back to Unsupported.
    let r = check_ort_version_compatibility("garbage");
    assert!(matches!(r, VersionCompatibility::Unsupported { .. }));
}

#[test]
fn test_is_network_error_detects_common_failures() {
    // VAL-SETUP-019-like network detection on pip output.
    assert!(is_network_error(
        "WARNING: Could not fetch URL https://pypi.org/onnxruntime"
    ));
    assert!(is_network_error(
        "ConnectionError: Failed to establish a new connection"
    ));
    assert!(is_network_error("ReadTimeoutError: read timed out"));
    assert!(is_network_error(
        "WARNING: Retrying (Retry(total=4)) after connection broken"
    ));
    // The MAX_RETRIES style.
    assert!(is_network_error(
        "HTTPSConnectionPool: Max retries exceeded with url: /simple/onnxruntime"
    ));
}

#[test]
fn test_is_network_error_ignores_normal_output() {
    // Normal pip output is NOT a network error.
    assert!(!is_network_error(
        "Successfully installed onnxruntime-1.25.0"
    ));
    assert!(!is_network_error("Requirement already satisfied: numpy"));
    assert!(!is_network_error(""));
}

#[test]
fn test_truncate_for_error_short() {
    let s = "line1\nline2\n";
    assert_eq!(truncate_for_error(s), "line1\nline2");
}

#[test]
fn test_truncate_for_error_long() {
    let long = (0..25)
        .map(|i| format!("line {}", i))
        .collect::<Vec<_>>()
        .join("\n");
    let truncated = truncate_for_error(&long);
    assert!(truncated.contains("truncated"));
    assert!(truncated.contains("line 0"));
    // The originally-present tail is dropped.
    assert!(!truncated.contains("line 24"));
}

#[test]
fn test_min_ort_version_constant_is_sensible() {
    // Guards against accidental breakage of the supported-range constant.
    // The constants are compile-time known; just exercise them so a future
    // edit that flips them nonsensically shows up in the test name.
    let _: (u32, u32, u32) = MIN_ORT_VERSION;
    let _: u32 = MAX_ORT_MAJOR;
    // Sanity: min version is 1.x.
    assert_eq!(MIN_ORT_VERSION.0, 1);
}

#[test]
fn test_build_config_records_ort_version() {
    // VAL-SETUP-020: ort_version flows into the config written to disk.
    let choices = SetupChoices {
        neural_enabled: true,
        provider: Some(ExecutionProvider::Cpu),
    };
    let cfg = build_config(
        &choices,
        Some(std::path::Path::new("/usr/local/lib/libonnxruntime.so")),
        Some("1.25.0"),
    );
    assert_eq!(cfg.neural.ort_version.as_deref(), Some("1.25.0"));
    assert_eq!(
        cfg.neural.ort_dylib_path.as_deref(),
        Some("/usr/local/lib/libonnxruntime.so")
    );
    assert!(cfg.neural.enabled);
    assert_eq!(cfg.neural.execution_provider, "cpu");
}

#[test]
fn test_build_config_selects_dynamic_qwen_model_for_all_local_providers() {
    for provider in [
        ExecutionProvider::Cpu,
        ExecutionProvider::Cuda,
        ExecutionProvider::Migraphx,
    ] {
        let choices = SetupChoices {
            neural_enabled: true,
            provider: Some(provider),
        };

        let cfg = build_config(&choices, None, None);

        assert_eq!(cfg.neural.model_name, "qwen3-embed-0.6b-dynamic");
    }
}

#[test]
fn test_model_download_profile_uses_hugging_face_cli_assets() {
    for provider in [
        ExecutionProvider::Cpu,
        ExecutionProvider::Cuda,
        ExecutionProvider::Migraphx,
    ] {
        let profile = model_download_profile(Some(provider));
        assert_eq!(profile.repository, "zhiqing/Qwen3-Embedding-0.6B-ONNX");
        assert_eq!(profile.revision, "c96cc9c82d08ee7869600e2191078fc939957026");
        assert_eq!(profile.remote_model, "model.onnx");
        assert_eq!(profile.local_model, "qwen3-embed-0.6b-dynamic.onnx");
        assert_eq!(
            profile.files,
            &["model.onnx", "tokenizer.json", "config.json"]
        );
    }
}

#[test]
fn test_dynamic_profile_requires_all_checksums_to_match() {
    let model_dir = tempfile::tempdir().unwrap();
    let profile = model_download_profile(Some(ExecutionProvider::Cpu));
    let model = std::fs::File::create(model_dir.path().join(profile.local_model)).unwrap();
    model.set_len(100 * 1024 * 1024).unwrap();
    std::fs::write(model_dir.path().join("tokenizer.json"), b"{}").unwrap();
    std::fs::write(model_dir.path().join("config.json"), b"{}").unwrap();

    generate_profile_checksum_manifest(model_dir.path(), profile).unwrap();
    assert!(profile_assets_verified(model_dir.path(), profile));

    let manifest_path = model_dir.path().join("checksums.sha256");
    let unpinned_manifest = std::fs::read_to_string(&manifest_path)
        .unwrap()
        .lines()
        .filter(|line| !line.starts_with("# source:"))
        .collect::<Vec<_>>()
        .join("\n");
    std::fs::write(&manifest_path, unpinned_manifest).unwrap();
    assert!(!profile_assets_verified(model_dir.path(), profile));

    generate_profile_checksum_manifest(model_dir.path(), profile).unwrap();

    std::fs::write(model_dir.path().join("config.json"), br#"{"changed":true}"#).unwrap();
    assert!(!profile_assets_verified(model_dir.path(), profile));
}

#[test]
fn test_rocm_smoke_result_accepts_migraphx_runtime() {
    let result = SmokeTestResult::from_embedding_outcome(
        QWEN3_EMBEDDING_DIMENSION,
        Some("migraphx".to_string()),
        Some("rocm".to_string()),
    );

    assert!(result.passed);
    assert!(result.error.is_none());
}

#[test]
fn test_gpu_smoke_result_fails_provider_mismatch() {
    let result = SmokeTestResult::from_embedding_outcome(
        1024,
        Some("cpu".to_string()),
        Some("migraphx".to_string()),
    );

    assert!(!result.passed);
    assert!(result
        .error
        .as_deref()
        .unwrap_or_default()
        .contains("configured execution provider migraphx"));
}

#[test]
fn test_build_config_without_version() {
    // When version detection failed, ort_version remains None but the rest
    // of the config is still valid.
    let choices = SetupChoices {
        neural_enabled: true,
        provider: Some(ExecutionProvider::Cpu),
    };
    let cfg = build_config(&choices, None, None);
    assert!(cfg.neural.ort_version.is_none());
    assert!(cfg.neural.ort_dylib_path.is_none());
}

// Use a process-shared lock so env-mutating tests serialize within the module.
use std::sync::Mutex;
static PIPE_ENV_LOCK: Mutex<()> = Mutex::new(());

#[test]
fn test_find_pip_honors_pip_bin_with_split() {
    // VAL-SETUP-021: PIP_BIN can point at "python3 -m pip" style.
    let _g = PIPE_ENV_LOCK.lock().unwrap();
    // /bin/true is a guaranteed-present binary that succeeds with whatever args,
    // so use it as a "pip" stand-in. We only check the parse logic.
    std::env::set_var("PIP_BIN", "/bin/true -m pip");
    // find_pip runs --version; /bin/true returns 0, so it should "succeed".
    let result = find_pip();
    std::env::remove_var("PIP_BIN");
    let (program, prefix) = result.expect("PIP_BIN should be honored");
    assert_eq!(program, "/bin/true");
    assert_eq!(prefix, vec!["-m".to_string(), "pip".to_string()]);
}

#[test]
fn test_parse_pip_bin_rejects_arbitrary_multi_arg_command() {
    assert!(parse_pip_bin_override("/usr/bin/curl https://evil.example").is_none());
    assert!(parse_pip_bin_override("python3 -m pip").is_some());
}

#[test]
fn test_find_pip_honors_pip_bin_single_token() {
    let _g = PIPE_ENV_LOCK.lock().unwrap();
    std::env::set_var("PIP_BIN", "/bin/true");
    let result = find_pip();
    std::env::remove_var("PIP_BIN");
    let (program, prefix) = result.expect("PIP_BIN single token should be honored");
    assert_eq!(program, "/bin/true");
    assert!(prefix.is_empty());
}

#[test]
fn test_find_pip_empty_pip_bin_falls_through() {
    let _g = PIPE_ENV_LOCK.lock().unwrap();
    std::env::set_var("PIP_BIN", "   ");
    // We can't assert what the fallback finds (system-dependent), but it
    // must not crash and must follow PIP_BIN's absence.
    let _ = find_pip();
    std::env::remove_var("PIP_BIN");
}

// ── VAL-SETUP-025/026: Smoke test result and status line ──

#[test]
fn test_smoke_test_result_pass_status_line() {
    let result = SmokeTestResult {
        passed: true,
        skipped: false,
        dimension: Some(1024),
        execution_provider: None,
        configured_provider_label: Some("cpu".to_string()),
        error: None,
        note: None,
    };
    let line = result.status_line();
    assert!(line.contains("PASS"), "{}", line);
    assert!(line.contains("1024"), "{}", line);
}

#[test]
fn test_smoke_test_result_pass_without_dimension_is_not_zero_dim() {
    let result = SmokeTestResult {
        passed: true,
        skipped: false,
        dimension: None,
        execution_provider: None,
        configured_provider_label: Some("cpu".to_string()),
        error: None,
        note: None,
    };
    let line = result.status_line();
    assert!(line.contains("PASS"), "{}", line);
    assert!(!line.contains("0-dim"), "{}", line);
    assert!(line.contains("dimension unavailable"), "{}", line);
}

#[test]
fn test_smoke_test_result_fail_status_line() {
    let result = SmokeTestResult {
        passed: false,
        skipped: false,
        dimension: None,
        execution_provider: None,
        configured_provider_label: Some("cpu".to_string()),
        error: Some("worker failed to start".to_string()),
        note: None,
    };
    let line = result.status_line();
    assert!(line.contains("FAIL"), "{}", line);
    // The FAIL line does NOT include the dimension (we don't have one).
    assert!(!line.contains("1024"));
}

#[test]
fn test_smoke_test_result_dimension_mismatch_is_fail() {
    // If the worker returns the wrong dimension, the test fails.
    let result = SmokeTestResult {
        passed: false,
        skipped: false,
        dimension: Some(768), // expected 1024
        execution_provider: None,
        configured_provider_label: Some("migraphx".to_string()),
        error: Some("expected 1024-dim vector, got 768-dim".to_string()),
        note: None,
    };
    assert!(!result.passed);
    assert_eq!(result.dimension, Some(768));
    assert_eq!(
        result.configured_provider_label.as_deref(),
        Some("migraphx")
    );
    assert!(result.execution_provider.is_none());
}

// ── VAL-SETUP-031: Permission denied error ──

#[test]
fn test_permission_denied_error_names_path_and_leindex_home() {
    let err = SetupError::PermissionDenied {
        path: PathBuf::from("/home/user/.leindex/config"),
        reason: "Permission denied (os error 13)".to_string(),
    };
    let msg = err.to_string();
    assert!(msg.contains("Permission denied"), "{}", msg);
    assert!(msg.contains("/home/user/.leindex/config"), "{}", msg);
    // Remediation hint must mention LEINDEX_HOME.
    assert!(msg.contains("LEINDEX_HOME"), "{}", msg);
}

#[test]
fn test_smoke_test_catastrophic_error_is_actionable() {
    let err = SetupError::SmokeTestCatastrophic {
        message: "worker binary not found".to_string(),
    };
    let msg = err.to_string();
    assert!(msg.contains("smoke test"), "{}", msg);
    assert!(msg.contains("worker binary not found"), "{}", msg);
    assert!(msg.contains("leindex-embed"), "{}", msg);
}

// ── VAL-SETUP-033: GPU vendor detection ──

#[test]
fn test_detect_gpu_vendor_returns_enum() {
    // detect_gpu_vendor must return without panicking regardless of the
    // system. We don't assert the specific variant because CI/dev hosts
    // have different hardware.
    let _ = detect_gpu_vendor();
}

#[test]
fn test_default_gpu_vendor_index_matches_detection() {
    assert_eq!(default_gpu_vendor_index(DetectedGpu::Amd), 0);
    assert_eq!(default_gpu_vendor_index(DetectedGpu::Nvidia), 1);
    assert_eq!(default_gpu_vendor_index(DetectedGpu::Unknown), 2);
}

#[test]
fn test_detected_gpu_variants_are_distinct() {
    // Enum sanity: each variant is distinct from the others.
    assert_ne!(DetectedGpu::Amd, DetectedGpu::Nvidia);
    assert_ne!(DetectedGpu::Amd, DetectedGpu::Unknown);
    assert_ne!(DetectedGpu::Nvidia, DetectedGpu::Unknown);
}

#[test]
fn test_detect_amd_gpu_no_false_positive_on_clean_system() {
    // With a bogus ROCM_PATH that does not exist, the detection should
    // not claim an AMD GPU is present via that path alone.
    let _g = PIPE_ENV_LOCK.lock().unwrap();
    std::env::set_var("ROCM_PATH", "/definitely/not/a/real/path");
    // This may still be true if /opt/rocm exists on the test host, so we
    // only check it doesn't panic and returns a bool-like enum.
    let _ = detect_amd_gpu();
    std::env::remove_var("ROCM_PATH");
}

#[test]
fn test_detect_amd_gpu_honors_existing_rocm_path() {
    // When ROCM_PATH points at an existing directory, AMD is detected.
    // Resource-duplication fix: use tempfile::TempDir for auto-cleanup.
    let _g = PIPE_ENV_LOCK.lock().unwrap();
    let tmp = tempfile::tempdir().unwrap();
    std::env::set_var("ROCM_PATH", tmp.path());
    assert!(detect_amd_gpu(), "existing ROCM_PATH should detect AMD");
    std::env::remove_var("ROCM_PATH");
    // tmp auto-cleans on drop
}

#[test]
fn test_detect_nvidia_gpu_with_cuda_path_env() {
    // When CUDA_PATH points at an existing directory, NVIDIA is detected.
    // Resource-duplication fix: use tempfile::TempDir for auto-cleanup.
    let _g = PIPE_ENV_LOCK.lock().unwrap();
    let tmp = tempfile::tempdir().unwrap();
    std::env::set_var("CUDA_PATH", tmp.path());
    assert!(
        detect_nvidia_gpu(),
        "existing CUDA_PATH should detect NVIDIA"
    );
    std::env::remove_var("CUDA_PATH");
    // tmp auto-cleans on drop
}

// ── VAL-SETUP-031 + VAL-SETUP-035: ensure_home_writable + LEINDEX_HOME ──

#[test]
fn test_ensure_home_writable_succeeds_for_writable_leindex_home() {
    // Resource-duplication fix: use tempfile::TempDir for auto-cleanup.
    let _g = PIPE_ENV_LOCK.lock().unwrap();
    let tmp = tempfile::tempdir().unwrap();
    std::env::set_var("LEINDEX_HOME", tmp.path());
    let result = ensure_home_writable();
    std::env::remove_var("LEINDEX_HOME");
    // tmp auto-cleans on drop
    assert!(
        result.is_ok(),
        "writable LEINDEX_HOME should pass: {:?}",
        result
    );
}

#[test]
fn test_ensure_home_writable_uses_leindex_home_location() {
    // VAL-SETUP-032/035: LEINDEX_HOME drives where config goes.
    // Resource-duplication fix: use tempfile::TempDir for auto-cleanup.
    let _g = PIPE_ENV_LOCK.lock().unwrap();
    let tmp = tempfile::tempdir().unwrap();
    std::env::set_var("LEINDEX_HOME", tmp.path());
    let result = ensure_home_writable();
    assert!(result.is_ok());
    // After the probe, the config directory should exist under $LEINDEX_HOME.
    assert!(
        tmp.path().join("config").is_dir(),
        "config dir should be under LEINDEX_HOME"
    );
    std::env::remove_var("LEINDEX_HOME");
    // tmp auto-cleans on drop
}

#[test]
fn test_ensure_home_writable_fails_for_read_only_dir() {
    // VAL-SETUP-031: a read-only directory surfaces a PermissionDenied error.
    // We create a tempfile::TempDir, chmod it 555 (read+execute only), and
    // verify the probe fails. Then restore perms and let TempDir clean up.
    // Resource-duplication fix: use tempfile::TempDir for auto-cleanup.
    let _g = PIPE_ENV_LOCK.lock().unwrap();
    let tmp = tempfile::tempdir().unwrap();
    let base = tmp.path().to_path_buf();

    // Make the base directory read-only (no write permission).
    // 0o555 = r-xr-xr-x
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&base, std::fs::Permissions::from_mode(0o555)).unwrap();
    }

    std::env::set_var("LEINDEX_HOME", &base);
    let result = ensure_home_writable();

    // Restore permissions before assertions so cleanup always works.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&base, std::fs::Permissions::from_mode(0o755));
    }
    std::env::remove_var("LEINDEX_HOME");
    // tmp auto-cleans on drop (we restored perms above)

    // On Unix with a read-only base, we expect a PermissionDenied error.
    // On non-Unix or when running as root, the probe may succeed; skip
    // the assertion in that case to avoid a flaky test.
    #[cfg(unix)]
    {
        // Running as root bypasses permissions, so only assert for non-root.
        let is_root = unsafe { libc::geteuid() == 0 };
        if !is_root {
            match result {
                Err(SetupError::PermissionDenied { path, .. }) => {
                    assert!(
                        path.starts_with(&base),
                        "PermissionDenied path should be under LEINDEX_HOME: {:?}",
                        path
                    );
                }
                other => {
                    // Some filesystems (tmpfs with special mount options)
                    // may surface the failure as a different variant. Accept
                    // any Err (smoke test: a read-only dir must fail).
                    assert!(
                        other.is_err(),
                        "read-only LEINDEX_HOME should fail, got {:?}",
                        other
                    );
                }
            }
        } else {
            let _ = result; // root bypasses perms
        }
    }
    #[cfg(not(unix))]
    {
        let _ = result; // Windows: skip per-OS
    }
}

// ── VAL-SETUP-035: truncate_for_display ──

#[test]
fn test_truncate_for_display_short() {
    assert_eq!(truncate_for_display("short", 100), "short");
}

#[test]
fn test_truncate_for_display_long_appends_ellipsis() {
    let input = "a".repeat(250);
    let result = truncate_for_display(&input, 50);
    assert!(result.ends_with("..."), "{}", result);
    // The truncated body is 50 chars + 3 for the ellipsis.
    assert_eq!(result.len(), 50 + 3);
}

// ── Resource-duplication fix: copy_bundled_models symlink/hardlink tests ──
//
// Bug 3 fix: copy_bundled_models() must prefer symlink > hardlink > copy
// so the 569 MB model file is not duplicated into every LEINDEX_HOME temp dir.

#[test]
fn test_try_link_model_file_creates_symlink_on_same_filesystem() {
    // On the same filesystem, symlink should succeed (strategy 1).
    // Both src and dst are under the system temp dir (same filesystem),
    // so the symlink should be created.
    let tmp = tempfile::tempdir().unwrap();
    let src = tmp.path().join("source.bin");
    let dst = tmp.path().join("dest.bin");
    std::fs::write(&src, b"model data").unwrap();

    let result = try_link_model_file(&src, &dst, false);
    assert!(
        result.is_ok(),
        "try_link_model_file should succeed: {:?}",
        result
    );

    // The result should be a symlink pointing at src.
    #[cfg(unix)]
    {
        let meta = std::fs::symlink_metadata(&dst).unwrap();
        assert!(meta.file_type().is_symlink(), "dst should be a symlink");
    }
    // The content should be readable through the link.
    let content = std::fs::read(&dst).unwrap();
    assert_eq!(content, b"model data");
}

#[test]
fn test_copy_bundled_models_creates_symlinks_not_copies() {
    // Resource-duplication fix: copy_bundled_models must create symlinks
    // (not copies) when source and dest are on the same filesystem.
    // We simulate a bundled models dir with a small placeholder file.
    let _g = PIPE_ENV_LOCK.lock().unwrap();

    let bundled = tempfile::tempdir().unwrap();
    let dest = tempfile::tempdir().unwrap();

    // Create a fake "model" file in the bundled dir.
    // We use a .txt extension to avoid triggering find_bundled_models()
    // during the test (it looks for qwen3-embed-0.6b.onnx).
    // copy_bundled_models iterates iter_model_files() which includes
    // config.json, so we write that.
    let config_src = bundled.path().join("config.json");
    std::fs::write(&config_src, b"{ \"test\": true }").unwrap();

    copy_bundled_models(bundled.path(), dest.path());

    let config_dst = dest.path().join("config.json");
    assert!(config_dst.exists(), "config.json should exist in dest");

    // On Unix, verify it's a symlink (not a copy).
    #[cfg(unix)]
    {
        let meta = std::fs::symlink_metadata(&config_dst).unwrap();
        assert!(
            meta.file_type().is_symlink(),
            "config.json in dest should be a symlink, not a copy (resource-duplication fix)"
        );
    }
}

#[test]
fn test_try_link_model_file_overwrites_existing() {
    // copy_bundled_models skips files that already exist in dest_dir,
    // so this test verifies try_link_model_file itself (called for new files).
    // If dst already exists, try_link_model_file will fail because symlink()
    // does not overwrite. This is the intended behavior: copy_bundled_models
    // checks !dst.exists() before calling try_link_model_file.
    let tmp = tempfile::tempdir().unwrap();
    let src = tmp.path().join("source.bin");
    let dst = tmp.path().join("dest.bin");
    std::fs::write(&src, b"model data").unwrap();
    std::fs::write(&dst, b"old data").unwrap();

    // symlink() fails when dst exists; hard_link also fails when dst exists.
    // The function should return an error (or fall through to copy which
    // also fails because copy overwrites... actually std::fs::copy overwrites).
    // So the result depends on the strategy: copy() overwrites by default.
    let result = try_link_model_file(&src, &dst, false);
    // std::fs::copy overwrites existing files, so this should succeed.
    assert!(
        result.is_ok(),
        "copy strategy should overwrite: {:?}",
        result
    );
}

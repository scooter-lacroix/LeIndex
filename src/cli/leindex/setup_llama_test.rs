use super::*;
use flate2::{Compression, write::GzEncoder};

fn archive_fixture(path: &Path, aliases: bool) {
    let file = File::create(path).unwrap();
    let mut archive = tar::Builder::new(GzEncoder::new(file, Compression::default()));
    let bytes = b"#!/bin/sh\nprintf 'llama fixture version\\n'\n";
    let mut header = tar::Header::new_gnu();
    header.set_size(bytes.len() as u64);
    header.set_mode(0o755);
    header.set_cksum();
    archive
        .append_data(&mut header, "runtime/bin/llama-server", &bytes[..])
        .unwrap();
    let mut header = tar::Header::new_gnu();
    header.set_size(7);
    header.set_mode(0o644);
    header.set_cksum();
    archive
        .append_data(&mut header, "runtime/lib/libggml.so.1", &b"library"[..])
        .unwrap();
    if aliases {
        let mut header = tar::Header::new_gnu();
        header.set_entry_type(tar::EntryType::Symlink);
        header.set_size(0);
        header.set_mode(0o777);
        archive
            .append_link(&mut header, "runtime/lib/libggml.so", "libggml.so.1")
            .unwrap();
    }
    archive.into_inner().unwrap().finish().unwrap();
}

#[test]
fn test_asset_table_rejects_unsupported_backends_and_platforms() {
    assert_eq!(
        asset_name("linux", "x86_64", "cpu").unwrap(),
        "llama-b11574-bin-ubuntu-x64.tar.gz"
    );
    assert_eq!(
        asset_name("linux", "aarch64", "vulkan").unwrap(),
        "llama-b11574-bin-ubuntu-vulkan-arm64.tar.gz"
    );
    assert!(asset_name("linux", "aarch64", "cuda").is_err());
    assert!(asset_name("linux", "x86_64", "rocm").is_err());
    assert!(asset_name("windows", "x86_64", "cpu").is_err());
}

#[test]
fn test_release_metadata_requires_tag_url_digest_and_unique_asset() {
    let name = asset_name("linux", "x86_64", "cpu").unwrap();
    let asset = serde_json::json!({"name": name, "browser_download_url": format!("https://github.com/ggml-org/llama.cpp/releases/download/{TAG}/{name}"), "digest": format!("sha256:{}", "a".repeat(64)), "size": 42});
    let mut release = serde_json::json!({"tag_name": TAG, "assets": [asset.clone()]});
    assert!(select_asset(&serde_json::to_vec(&release).unwrap(), &name).is_ok());
    release["tag_name"] = "latest".into();
    assert!(select_asset(&serde_json::to_vec(&release).unwrap(), &name).is_err());
    release["tag_name"] = TAG.into();
    release["assets"][0]["digest"] = serde_json::Value::Null;
    assert!(select_asset(&serde_json::to_vec(&release).unwrap(), &name).is_err());
    release["assets"] = serde_json::json!([asset.clone(), asset.clone()]);
    assert!(select_asset(&serde_json::to_vec(&release).unwrap(), &name).is_err());
    release["assets"] = serde_json::json!([asset]);
    release["assets"][0]["browser_download_url"] = "https://example.com/runtime".into();
    assert!(select_asset(&serde_json::to_vec(&release).unwrap(), &name).is_err());
}

#[test]
fn test_archive_paths_and_internal_link_resolution() {
    for path in ["../escape", "/absolute", "C:/drive", "a\\b", "a\nname"] {
        assert!(safe_relative(Path::new(path)).is_err(), "{path:?}");
    }
    assert_eq!(
        safe_relative(Path::new("./runtime/lib/libggml.so")).unwrap(),
        Path::new("runtime/lib/libggml.so")
    );
    assert_eq!(
        link_target(
            Path::new("runtime/lib/alias"),
            Path::new("../bin/llama-server")
        )
        .unwrap(),
        Path::new("runtime/bin/llama-server")
    );
    assert!(link_target(Path::new("runtime/alias"), Path::new("../../outside")).is_err());
    assert!(link_target(Path::new("runtime/alias"), Path::new("/outside")).is_err());
}

#[test]
fn test_extraction_preserves_layout_copies_aliases_and_detects_tampering() {
    let temp = tempfile::tempdir().unwrap();
    let archive = temp.path().join("runtime.tar.gz");
    archive_fixture(&archive, true);
    let expected = temp.path().join("expected");
    let installed = temp.path().join("installed");
    fs::create_dir(&expected).unwrap();
    fs::create_dir(&installed).unwrap();
    extract_archive(&archive, &expected).unwrap();
    extract_archive(&archive, &installed).unwrap();
    assert_eq!(
        fs::read(installed.join("runtime/lib/libggml.so")).unwrap(),
        b"library"
    );
    assert!(
        !fs::symlink_metadata(installed.join("runtime/lib/libggml.so"))
            .unwrap()
            .file_type()
            .is_symlink()
    );
    assert_eq!(
        locate_server(&installed).unwrap(),
        installed.join("runtime/bin/llama-server")
    );
    compare_bundle(&expected, &installed).unwrap();
    fs::write(installed.join("runtime/lib/libggml.so.1"), b"tampered").unwrap();
    assert!(compare_bundle(&expected, &installed).is_err());
}

#[test]
fn test_extraction_rejects_escaping_symlink_and_hardlink() {
    for (kind, target) in [
        (tar::EntryType::Symlink, "../../outside"),
        (tar::EntryType::Link, "runtime/file"),
    ] {
        let temp = tempfile::tempdir().unwrap();
        let archive_path = temp.path().join("bad.tar.gz");
        let mut archive = tar::Builder::new(GzEncoder::new(
            File::create(&archive_path).unwrap(),
            Compression::default(),
        ));
        let mut header = tar::Header::new_gnu();
        header.set_entry_type(kind);
        header.set_size(0);
        header.set_mode(0o777);
        archive
            .append_link(&mut header, "runtime/alias", target)
            .unwrap();
        archive.into_inner().unwrap().finish().unwrap();
        let root = temp.path().join("bundle");
        fs::create_dir(&root).unwrap();
        assert!(extract_archive(&archive_path, &root).is_err());
        assert!(!temp.path().join("outside").exists());
    }
}

#[cfg(target_os = "linux")]
#[test]
fn test_explicit_binary_resolution_and_nonexecutable_rejection() {
    use std::os::unix::fs::PermissionsExt;
    let temp = tempfile::tempdir().unwrap();
    let binary = temp.path().join("llama-server");
    fs::write(&binary, b"#!/bin/sh\nprintf 'fixture version\\n'\n").unwrap();
    fs::set_permissions(&binary, fs::Permissions::from_mode(0o755)).unwrap();
    assert_eq!(
        ensure_binary(temp.path()).unwrap(),
        binary.to_str().unwrap()
    );
    assert!(ensure_binary(Path::new("llama-server")).is_err());
    fs::create_dir(temp.path().join("bin")).unwrap();
    fs::copy(&binary, temp.path().join("bin/llama-server")).unwrap();
    assert!(ensure_binary(temp.path()).is_err());
    fs::set_permissions(&binary, fs::Permissions::from_mode(0o644)).unwrap();
    assert!(ensure_binary(&binary).is_err());
}

#[cfg(target_os = "linux")]
#[test]
fn test_probe_uses_production_cwd_and_surfaces_failure() {
    use std::os::unix::fs::PermissionsExt;
    let temp = tempfile::tempdir().unwrap();
    let binary = temp.path().join("llama-server");
    fs::write(
        &binary,
        b"#!/bin/sh\n[ \"$PWD\" = / ] || exit 4\nprintf 'fixture version\\n'\n",
    )
    .unwrap();
    fs::set_permissions(&binary, fs::Permissions::from_mode(0o755)).unwrap();
    ensure_binary(&binary).unwrap();
    fs::write(&binary, b"#!/bin/sh\nexit 5\n").unwrap();
    assert!(ensure_binary(&binary).is_err());
}

#[test]
fn test_extraction_rejects_duplicate_members_and_missing_alias_targets() {
    for missing_alias in [false, true] {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("bad.tar.gz");
        let mut archive = tar::Builder::new(GzEncoder::new(
            File::create(&path).unwrap(),
            Compression::default(),
        ));
        if missing_alias {
            let mut header = tar::Header::new_gnu();
            header.set_entry_type(tar::EntryType::Symlink);
            header.set_size(0);
            header.set_mode(0o777);
            archive
                .append_link(&mut header, "runtime/alias", "missing")
                .unwrap();
        } else {
            for _ in 0..2 {
                let mut header = tar::Header::new_gnu();
                header.set_size(1);
                header.set_mode(0o644);
                header.set_cksum();
                archive
                    .append_data(&mut header, "runtime/file", &b"x"[..])
                    .unwrap();
            }
        }
        archive.into_inner().unwrap().finish().unwrap();
        let root = temp.path().join("bundle");
        fs::create_dir(&root).unwrap();
        assert!(extract_archive(&path, &root).is_err());
    }
}

#[cfg(target_os = "linux")]
#[test]
fn test_bounded_command_rejects_excess_output_even_when_child_exits() {
    let temp = tempfile::tempdir().unwrap();
    let mut command = Command::new("/bin/sh");
    command.args(["-c", "printf 'output beyond the limit'"]);
    assert!(
        run_bounded(
            &mut command,
            &temp.path().join("output"),
            2,
            Duration::from_secs(2)
        )
        .is_err()
    );
}

#[cfg(target_os = "linux")]
#[test]
fn test_bounded_command_terminates_stalled_child() {
    let temp = tempfile::tempdir().unwrap();
    let mut command = Command::new("/bin/sh");
    command.args(["-c", "while :; do :; done"]);
    let started = Instant::now();
    assert!(
        run_bounded(
            &mut command,
            &temp.path().join("output"),
            128,
            Duration::from_millis(80)
        )
        .is_err()
    );
    assert!(started.elapsed() < Duration::from_secs(5));
}

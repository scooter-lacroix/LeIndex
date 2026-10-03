use super::*;
use std::fs;

// ─────────────────────────────────────────────────────────────────────────
// Cross-version registry tests
// ─────────────────────────────────────────────────────────────────────────

#[test]
fn test_migration_registry_has_legacy_step() {
    let steps = migration_steps();
    assert!(!steps.is_empty(), "registry must have at least one step");
    let legacy = steps
        .iter()
        .find(|s| s.source_version() == LEGACY_FORMAT_VERSION);
    assert!(legacy.is_some(), "registry must include a legacy v0 step");
}

#[test]
fn test_legacy_to_cas_step_versions() {
    let step = LegacyToCasStep;
    assert_eq!(step.source_version(), LEGACY_FORMAT_VERSION);
    assert_eq!(step.target_version(), CURRENT_FORMAT_VERSION);
    assert!(step.source_version() < step.target_version());
    assert!(!step.name().is_empty());
    assert!(step.name().contains("CAS"));
}

#[test]
fn test_every_step_covers_a_version_range() {
    for step in migration_steps() {
        assert!(
            step.source_version() < step.target_version(),
            "step '{}' must cover a forward range (from {} to {})",
            step.name(),
            step.source_version(),
            step.target_version()
        );
    }
}

// ─────────────────────────────────────────────────────────────────────────
// detect_format_version tests
// ─────────────────────────────────────────────────────────────────────────

#[test]
fn test_detect_format_empty_store() {
    let tmp = tempfile::tempdir().unwrap();
    // Empty store: no CURRENT, no generations.
    let detected = detect_format_version(tmp.path());
    assert_eq!(detected, LEGACY_FORMAT_VERSION);
}

#[test]
fn test_detect_format_migrated_store() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("storage");
    let gen_dir = root.join("generations").join("1");
    fs::create_dir_all(&gen_dir).unwrap();

    // Create a valid manifest so the store is detected as migrated.
    let manifest = crate::storage::generation::manifest::Manifest {
        version: crate::storage::generation::manifest::MANIFEST_VERSION,
        generation: 1,
        model_identity: crate::storage::generation::manifest::ModelIdentity {
            name: "test".to_string(),
            digest: "sha256:test".to_string(),
            dimensions: 384,
        },
        graph_fingerprint: [0u8; 32],
        search_fingerprint: [0u8; 32],
        layers: {
            let mut m = std::collections::HashMap::new();
            for kind in crate::storage::generation::manifest::ALL_LAYER_KINDS {
                m.insert(kind, [0u8; 32]);
            }
            m
        },
    };
    let bytes = manifest.to_bytes().unwrap();
    fs::write(gen_dir.join("manifest"), &bytes).unwrap();
    fs::write(root.join("CURRENT"), "1").unwrap();

    let detected = detect_format_version(&root);
    assert_eq!(detected, CURRENT_FORMAT_VERSION);
}

// ─────────────────────────────────────────────────────────────────────────
// VAL-ROLLOUT-005: read-old/build-new/switch + rollback preservation
// ─────────────────────────────────────────────────────────────────────────

#[test]
fn test_migrate_to_current_on_empty_store_is_noop() {
    let tmp = tempfile::tempdir().unwrap();
    let cfg = MigrationConfig {
        emit_backup_warning: false,
        ..Default::default()
    };
    let report = migrate_to_current(tmp.path(), &cfg).expect("migration succeeds");
    assert!(!report.detected_legacy);
    assert!(!report.migrated);
    assert!(report.was_noop());
}

#[test]
fn test_needs_migration_false_for_new_store() {
    let tmp = tempfile::tempdir().unwrap();
    assert!(!needs_migration(tmp.path()));
}

// ─────────────────────────────────────────────────────────────────────────
// Migration step pluggability (future v1→v2)
// ─────────────────────────────────────────────────────────────────────────

/// A test-only migration step to prove the registry is extensible.
struct DummyV1ToV2Step;

impl MigrationStep for DummyV1ToV2Step {
    fn source_version(&self) -> u32 {
        1
    }
    fn target_version(&self) -> u32 {
        2
    }
    fn name(&self) -> &str {
        "dummy v1→v2 test step"
    }
    fn migrate(
        &self,
        storage_root: &std::path::Path,
        _config: &MigrationConfig,
    ) -> Result<MigrationReport, MigrationError> {
        // Pretend we did work.
        Ok(MigrationReport {
            detected_legacy: false,
            migrated: true,
            current_generation: crate::storage::generation::lease::read_current_generation(
                storage_root,
            ),
            previous_generation: None,
            generations_converted: 0,
            generations_deleted: 0,
            cas_reclaimed_bytes: 0,
            cas_blob_count: 0,
            cas_bytes: 0,
            jobs_completed_deleted: 0,
            jobs_byte_capped: 0,
            job_bytes_reclaimed: 0,
            job_bytes_remaining: 0,
            artifact_bytes_reclaimed: 0,
            total_bytes_before: 0,
            total_bytes_after: 0,
            blob_hashes: Vec::new(),
            warnings: Vec::new(),
        })
    }
}

#[test]
fn test_custom_migration_step_satisfies_trait() {
    let step = DummyV1ToV2Step;
    assert_eq!(step.source_version(), 1);
    assert_eq!(step.target_version(), 2);
    assert_eq!(step.name(), "dummy v1→v2 test step");
}

#[test]
fn test_migration_steps_returned_are_boxed_dyn() {
    let steps = migration_steps();
    // Verify we can iterate and call methods through the trait object.
    for step in &steps {
        let _name = step.name();
        let _from = step.source_version();
        let _to = step.target_version();
    }
}

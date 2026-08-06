//! Cross-version artifact migration: read-old / build-new / switch with
//! rollback preservation (spec §12.2, WS12-13 Task 3).
//!
//! The migration converts a legacy full-copy `.leindex/` layout into the
//! CAS-backed generation store. The conversion follows a strict **read-old,
//! build-new, switch** sequence that is idempotent and crash-safe:
//!
//! 1. **Read-old**: read the legacy generation (full-copy directory with
//!    `leindex.db`, `embeddings.bin`, etc.).
//! 2. **Build-new**: convert each layer into a CAS blob, write a new
//!    `LIDX-GEN1` manifest beside the old data.
//! 3. **Validate**: verify the new manifest's magic/version/layers.
//! 4. **Switch**: atomically update `CURRENT` to point to the new manifest-
//!    based generation.
//! 5. **Preserve rollback**: the prior generation is retained for rollback.
//!
//! Because `CURRENT` is the single commit point, a crash at any step before
//! the switch leaves the old store serving; re-running the migration resumes
//! cleanly. A crash after the switch leaves a fully migrated store.
//!
//! ## Cross-Version Registry
//!
//! The registry is extensible: future v2→v3 migrations can plug additional
//! [`MigrationStep`] implementations into the registry without modifying the
//! core migration logic. Each step is identified by a source/target version
//! pair and implements the conversion function.

pub mod artifact;

use std::path::Path;

// Re-export the existing WS4 migration primitives so this module is the
// single import surface for all migration concerns.
pub use crate::storage::generation::migrate::{
    MigrationConfig, MigrationError, MigrationReport, is_legacy_full_copy_layout,
    is_migrated_store, migrate_legacy_store,
};
pub use artifact::{
    ArtifactError, ArtifactOutcome, RebuildReason, extract_blob_payload, validate_blob,
    validate_generation_manifest, validate_manifest, validate_manifest_fingerprints,
};

/// Current artifact format major version for the generation store.
pub const CURRENT_FORMAT_VERSION: u32 = 1;

/// Legacy artifact format version (pre-CAS full-copy layout).
pub const LEGACY_FORMAT_VERSION: u32 = 0;

// ─────────────────────────────────────────────────────────────────────────
// Cross-Version Registry
// ─────────────────────────────────────────────────────────────────────────

/// A single migration step in the version chain.
///
/// Each step converts from one format version to the next, plugging into the
/// registry for future v1→v2, v2→v3, etc. migrations.
pub trait MigrationStep: Send + Sync {
    /// Source format version (inclusive).
    fn source_version(&self) -> u32;
    /// Target format version (inclusive).
    fn target_version(&self) -> u32;
    /// Human-readable name for logging.
    fn name(&self) -> &str;
    /// Execute the migration on `storage_root`. Must be idempotent and
    /// crash-safe.
    fn migrate(
        &self,
        storage_root: &Path,
        config: &MigrationConfig,
    ) -> Result<MigrationReport, MigrationError>;
}

/// The built-in legacy v0 → CAS v1 migration step.
///
/// Delegates to [`migrate_legacy_store`] (WS4 Task 10), which implements the
/// read-old/build-new/switch sequence with atomic `CURRENT` swap and rollback
/// preservation.
pub struct LegacyToCasStep;

impl MigrationStep for LegacyToCasStep {
    fn source_version(&self) -> u32 {
        LEGACY_FORMAT_VERSION
    }
    fn target_version(&self) -> u32 {
        CURRENT_FORMAT_VERSION
    }
    fn name(&self) -> &str {
        "legacy-full-copy → CAS-LIDX-GEN1"
    }
    fn migrate(
        &self,
        storage_root: &Path,
        config: &MigrationConfig,
    ) -> Result<MigrationReport, MigrationError> {
        migrate_legacy_store(storage_root, config)
    }
}

/// Ordered registry of all known migration steps.
///
/// The registry is queried by [`migrate_to_current`] to find and run the
/// applicable steps for a given storage root. Future versions add new steps
/// here; no existing code needs to change.
pub fn migration_steps() -> Vec<Box<dyn MigrationStep>> {
    vec![Box::new(LegacyToCasStep)]
}

/// Detect the current format version of a storage root.
///
/// - Returns [`LEGACY_FORMAT_VERSION`] if the layout is legacy full-copy.
/// - Returns [`CURRENT_FORMAT_VERSION`] if the store is already migrated to
///   the CAS generation format.
/// - Returns 0 (legacy/unknown) if neither detection heuristic matches
///   (empty store, first run).
pub fn detect_format_version(storage_root: &Path) -> u32 {
    if is_migrated_store(storage_root) {
        CURRENT_FORMAT_VERSION
    } else if is_legacy_full_copy_layout(storage_root) {
        LEGACY_FORMAT_VERSION
    } else {
        // Empty or new store: no migration needed.
        LEGACY_FORMAT_VERSION
    }
}

/// Run all applicable migration steps to bring `storage_root` up to
/// [`CURRENT_FORMAT_VERSION`].
///
/// This is the top-level entry point for the read-old/build-new/switch
/// migration. It:
///
/// 1. Detects the current format version.
/// 2. Finds the migration step that covers the detected version.
/// 3. Runs it (idempotent; a no-op if already at target).
/// 4. Returns the migration report.
///
/// If no migration is needed (store is already at `CURRENT_FORMAT_VERSION`),
/// returns a no-op report without touching the filesystem.
pub fn migrate_to_current(
    storage_root: &Path,
    config: &MigrationConfig,
) -> Result<MigrationReport, MigrationError> {
    let detected = detect_format_version(storage_root);

    if detected == CURRENT_FORMAT_VERSION {
        // Already at the target version. Check if this is a truly-migrated
        // store or just an empty/new one.
        if is_migrated_store(storage_root) {
            tracing::debug!(
                "migrate_to_current: store already at v{}, no migration needed",
                CURRENT_FORMAT_VERSION
            );
            // Run cleanup to finish an interrupted transition (idempotent).
            return migrate_legacy_store(storage_root, config);
        }
        // Empty or new store: no migration needed.
        return Ok(MigrationReport {
            detected_legacy: false,
            migrated: false,
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
        });
    }

    // Find the applicable migration step.
    for step in migration_steps() {
        if detected >= step.source_version() && detected < step.target_version() {
            tracing::info!(
                "migrate_to_current: running migration step '{}' (v{} → v{})",
                step.name(),
                step.source_version(),
                step.target_version()
            );
            return step.migrate(storage_root, config);
        }
    }

    // No matching step found. This is fine for new/empty stores.
    tracing::debug!(
        "migrate_to_current: no migration step for detected version {}",
        detected
    );
    Ok(MigrationReport {
        detected_legacy: false,
        migrated: false,
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

/// Check whether the storage root needs migration to the current format
/// version.
pub fn needs_migration(storage_root: &Path) -> bool {
    is_legacy_full_copy_layout(storage_root)
}

#[cfg(test)]
#[path = "migration_test.rs"]
mod tests;

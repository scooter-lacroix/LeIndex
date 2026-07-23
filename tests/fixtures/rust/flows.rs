//! Fixture: cross-function data dependencies and struct definitions.
//!
//! Used by parser fixture tests (VAL-FIXTURES-003) to verify that the Rust
//! parser extracts standalone functions (`run_installation`,
//! `verify_registry`, `detect_arch`) and structs (`RegistryRecord`) with
//! cross-function call relationships where `run_installation` calls both
//! `verify_registry` and `detect_arch`.

use std::collections::HashMap;
use std::path::PathBuf;

/// Record describing a single registry entry.
pub struct RegistryRecord {
    /// Canonical name of the package.
    pub name: String,
    /// Registry URL or file path.
    pub registry_url: String,
    /// Integrity hash (SHA-256 hex).
    pub integrity: String,
}

/// Detect the host architecture triple.
fn detect_arch() -> String {
    #[cfg(target_arch = "x86_64")]
    {
        "x86_64".to_string()
    }
    #[cfg(target_arch = "aarch64")]
    {
        "aarch64".to_string()
    }
    #[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
    {
        "unknown".to_string()
    }
}

/// Verify that the registry record is well-formed and not tampered with.
fn verify_registry(record: &RegistryRecord, required_keys: &[&str]) -> bool {
    if record.name.is_empty() || record.registry_url.is_empty() {
        return false;
    }
    for key in required_keys {
        match *key {
            "name" => {
                if record.name.is_empty() {
                    return false;
                }
            }
            "integrity" => {
                if record.integrity.len() != 64 {
                    return false;
                }
            }
            _ => {}
        }
    }
    true
}

/// Run the full installation pipeline.
///
/// This function demonstrates cross-function data dependencies by calling
/// both `detect_arch` and `verify_registry` before proceeding.
fn run_installation(record: &RegistryRecord, dest: &PathBuf) -> Result<String, String> {
    let arch = detect_arch();
    if !verify_registry(record, &["name", "integrity"]) {
        return Err(format!("registry verification failed for {}", record.name));
    }

    let mut metadata: HashMap<&str, String> = HashMap::new();
    metadata.insert("arch", arch.clone());
    metadata.insert("dest", dest.display().to_string());
    metadata.insert("source", record.registry_url.clone());

    Ok(format!(
        "installed {} ({}) -> {}",
        record.name,
        metadata.get("arch").unwrap(),
        metadata.get("dest").unwrap(),
    ))
}

/// Standalone entry point demonstrating function calls.
fn main() {
    let record = RegistryRecord {
        name: "leindex".to_string(),
        registry_url: "https://registry.example.com/leindex".to_string(),
        integrity: "a".repeat(64),
    };
    let dest = PathBuf::from("/usr/local/leindex");
    match run_installation(&record, &dest) {
        Ok(msg) => println!("{msg}"),
        Err(e) => eprintln!("installation failed: {e}"),
    }
}

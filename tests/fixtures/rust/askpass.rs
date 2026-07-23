//! Fixture: askpass helper for credential retrieval.
//!
//! Used by parser fixture tests (VAL-FIXTURES-001) to verify that the Rust
//! parser extracts structs, impl methods with qualified names, standalone
//! functions, and constants.

use std::path::PathBuf;
use std::process::{Command, Stdio};

/// Maximum number of retry attempts before giving up.
const MAX_RETRIES: u32 = 3;

/// Default timeout for the askpass prompt in seconds.
const DEFAULT_TIMEOUT_SECS: u64 = 30;

/// Environment variable used to override the askpass binary path.
const ASKPASS_ENV_VAR: &str = "LEINDEX_ASKPASS_PATH";

/// Represents an askpass credential helper that shells out to an external
/// binary to retrieve a password from the user.
pub struct Askpass {
    /// Path to the askpass binary.
    binary_path: PathBuf,
    /// Display name shown in the prompt.
    prompt: String,
}

impl Askpass {
    /// Create a new Askpass instance with the given binary path.
    pub fn new(binary_path: PathBuf) -> Self {
        Askpass {
            binary_path,
            prompt: String::from("Password: "),
        }
    }

    /// Return the configured binary path.
    pub fn path(&self) -> &PathBuf {
        &self.binary_path
    }

    /// Return the prompt string used for the credential dialog.
    pub fn prompt_text(&self) -> &str {
        &self.prompt
    }

    /// Invoke the askpass binary and return the retrieved password.
    pub fn retrieve(&self, key: &str) -> Result<String, std::io::Error> {
        let output = Command::new(&self.binary_path)
            .env("ASKPASS_PROMPT", &self.prompt)
            .arg(key)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()?;
        Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
    }
}

/// Entry point: demonstrates a standalone function outside any impl block.
fn main() {
    let binary = std::env::var(ASKPASS_ENV_VAR)
        .unwrap_or_else(|_| "/usr/bin/ssh-askpass".to_string());
    let askpass = Askpass::new(PathBuf::from(binary));
    if let Err(e) = askpass.retrieve("token") {
        eprintln!("askpass failed: {e}");
        std::process::exit(1);
    }
}

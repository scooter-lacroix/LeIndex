//! Fixture: nested CLI with modules, enums, and associated functions.
//!
//! Used by parser fixture tests (VAL-FIXTURES-002) to verify that the Rust
//! parser extracts nested module items with qualified names (e.g.
//! `config::Config`), enum variants (e.g. `Commands::Install`), and
//! associated functions inside impl blocks that lack a `self` parameter.

use std::path::PathBuf;

mod config {
    use std::path::PathBuf;

    /// Configuration loaded from a TOML file, scoped inside `mod config`.
    pub struct Config {
        /// Root directory for package operations.
        pub root_dir: PathBuf,
        /// Whether to use verbose output.
        pub verbose: bool,
    }

    impl Config {
        /// Create a new Config (associated function, no `self`).
        pub fn new(root_dir: PathBuf) -> Self {
            Config {
                root_dir,
                verbose: false,
            }
        }

        /// Toggle verbosity on this config (method, has `&mut self`).
        pub fn set_verbose(&mut self, verbose: bool) {
            self.verbose = verbose;
        }
    }
}

/// Top-level CLI struct parsed by Clap.
pub struct Cli {
    /// Path to the config file.
    pub config_path: PathBuf,
    /// Whether to run in dry-run mode.
    pub dry_run: bool,
}

/// Clap-style command enum with multiple variants.
pub enum Commands {
    /// Install a package from the registry.
    Install {
        /// Package name to install.
        name: String,
        /// Optional version constraint.
        version: Option<String>,
    },
    /// Remove an installed package.
    Uninstall {
        /// Package name to remove.
        name: String,
    },
    /// Show installation status.
    Status,
}

impl Cli {
    /// Parse CLI from env args (associated function, no `self`).
    pub fn from_env() -> Self {
        Cli {
            config_path: PathBuf::from("leindex.toml"),
            dry_run: false,
        }
    }

    /// Run the selected command (method, has `&self`).
    pub fn run(&self, cmd: &Commands) -> i32 {
        match cmd {
            Commands::Install { .. } => 0,
            Commands::Uninstall { .. } => 0,
            Commands::Status => 0,
        }
    }
}

impl Commands {
    /// Return a human-readable label for the variant (method, has `&self`).
    pub fn label(&self) -> &str {
        match self {
            Commands::Install { .. } => "install",
            Commands::Uninstall { .. } => "uninstall",
            Commands::Status => "status",
        }
    }
}

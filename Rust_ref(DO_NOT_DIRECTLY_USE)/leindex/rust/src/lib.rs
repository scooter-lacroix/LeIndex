//!
//! # Maestro LeIndex Migration Framework
//!
//! A comprehensive database migration framework for SQLite/DuckDB/Tantivy to Turso migrations.
//!
//! ## Modules
//!
//! - `migrations`: Core migration management with async support
//!
//! ## Example
//!
//! ```rust
//! use maestro_leindex::migrations::{Migration, MigrationManager};
//!
//! #[derive(Debug)]
//! struct CreateUsersTable;
//!
//! #[async_trait::async_trait]
//! impl Migration for CreateUsersTable {
//!     fn version(&self) -> &str {
//!         "2024_01_01_001_create_users_table"
//!     }
//!
//!     async fn up(&self, db: &libsql::Database) -> anyhow::Result<()> {
//!         Ok(())
//!     }
//!
//!     async fn down(&self, db: &libsql::Database) -> anyhow::Result<()> {
//!         Ok(())
//!     }
//! }
//! ```
//!

pub mod migrations;

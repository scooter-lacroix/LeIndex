//! Application configuration model.

use serde::{Deserialize, Serialize};

/// Top-level application configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppConfig {
    pub database: DatabaseConfig,
    pub server: ServerConfig,
    pub cache: CacheConfig,
    pub logging: LoggingConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DatabaseConfig {
    pub url: String,
    pub max_connections: u32,
    pub connection_timeout_secs: u64,
    pub idle_timeout_secs: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServerConfig {
    pub host: String,
    pub port: u16,
    pub workers: u32,
    pub request_timeout_secs: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CacheConfig {
    pub enabled: bool,
    pub ttl_secs: u64,
    pub max_entries: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LoggingConfig {
    pub level: String,
    pub format: String,
    pub output_path: Option<String>,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            database: DatabaseConfig {
                url: String::from("sqlite::memory:"),
                max_connections: 10,
                connection_timeout_secs: 30,
                idle_timeout_secs: 600,
            },
            server: ServerConfig {
                host: String::from("127.0.0.1"),
                port: 8080,
                workers: 4,
                request_timeout_secs: 60,
            },
            cache: CacheConfig {
                enabled: true,
                ttl_secs: 300,
                max_entries: 1000,
            },
            logging: LoggingConfig {
                level: String::from("info"),
                format: String::from("json"),
                output_path: None,
            },
        }
    }
}

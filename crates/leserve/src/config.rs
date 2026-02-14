//! Server configuration from TOML or environment

use serde::{Deserialize, Serialize};
use std::net::SocketAddr;

/// Default host address
pub const DEFAULT_HOST: &str = "127.0.0.1";

/// Default port number
pub const DEFAULT_PORT: u16 = 47269;

/// Default CORS origins (localhost for development)
pub const DEFAULT_CORS_ORIGINS: &[&str] = &[
    "http://localhost:5173",
    "http://localhost:5174",
    "http://127.0.0.1:5173",
    "http://127.0.0.1:5174",
];

/// Maximum number of WebSocket connections
pub const MAX_WS_CONNECTIONS: usize = 100;

/// WebSocket heartbeat interval in seconds
pub const WS_HEARTBEAT_INTERVAL_SECS: u64 = 30;

/// Server configuration loaded from TOML
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServerConfig {
    /// Server host address
    pub host: String,

    /// Server port
    pub port: u16,

    /// Allowed CORS origins
    pub cors_origins: Vec<String>,

    /// Path to SQLite database
    pub db_path: String,

    /// Maximum WebSocket connections
    pub max_ws_connections: usize,

    /// WebSocket heartbeat interval in seconds
    pub ws_heartbeat_interval_secs: u64,

    /// Enable request logging
    pub enable_logging: bool,

    /// Log level for tracing
    pub log_level: String,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            host: Self::default_host(),
            port: Self::default_port(),
            cors_origins: Self::default_cors_origins(),
            db_path: Self::default_db_path(),
            max_ws_connections: Self::default_max_ws(),
            ws_heartbeat_interval_secs: Self::default_heartbeat(),
            enable_logging: Self::default_logging(),
            log_level: Self::default_log_level(),
        }
    }
}

impl ServerConfig {
    /// Default host value
    fn default_host() -> String {
        DEFAULT_HOST.to_string()
    }

    /// Default port value
    fn default_port() -> u16 {
        DEFAULT_PORT
    }

    /// Default CORS origins
    fn default_cors_origins() -> Vec<String> {
        DEFAULT_CORS_ORIGINS.iter().map(|s| s.to_string()).collect()
    }

    /// Default database path
    fn default_db_path() -> String {
        "leindex.db".to_string()
    }

    /// Default max WebSocket connections
    fn default_max_ws() -> usize {
        MAX_WS_CONNECTIONS
    }

    /// Default heartbeat interval
    fn default_heartbeat() -> u64 {
        WS_HEARTBEAT_INTERVAL_SECS
    }

    /// Default logging enabled
    fn default_logging() -> bool {
        true
    }

    /// Default log level
    fn default_log_level() -> String {
        "info".to_string()
    }

    /// Load config from environment variables with fallback to defaults
    ///
    /// Environment variables:
    /// - `LESERVE_HOST` - Server host
    /// - `LESERVE_PORT` - Server port
    /// - `LESERVE_DB_PATH` - Database path
    /// - `LESERVE_LOG_LEVEL` - Log level (trace, debug, info, warn, error)
    ///
    /// # Returns
    ///
    /// Server configuration with env vars applied
    #[must_use]
    pub fn from_env() -> Self {
        let mut config = Self::default();

        if let Ok(host) = std::env::var("LESERVE_HOST") {
            config.host = host;
        }

        if let Ok(port_str) = std::env::var("LESERVE_PORT") {
            if let Ok(port) = port_str.parse::<u16>() {
                config.port = port;
            }
        }

        if let Ok(db_path) = std::env::var("LESERVE_DB_PATH") {
            config.db_path = db_path;
        }

        if let Ok(log_level) = std::env::var("LESERVE_LOG_LEVEL") {
            config.log_level = log_level;
        }

        config
    }

    /// Get the socket address for the server
    ///
    /// # Returns
    ///
    /// `Result<SocketAddr, String>` - Parsed address or error message
    #[must_use]
    pub fn socket_addr(&self) -> Result<SocketAddr, String> {
        format!("{}:{}", self.host, self.port)
            .parse()
            .map_err(|e| format!("Invalid address: {}", e))
    }

    /// Get the full server URL
    ///
    /// # Returns
    ///
    /// Formatted URL string (e.g., "http://127.0.0.1:47269")
    #[must_use]
    pub fn server_url(&self) -> String {
        format!("http://{}:{}", self.host, self.port)
    }

    /// Get the WebSocket URL
    ///
    /// # Returns
    ///
    /// Formatted WebSocket URL (e.g., "ws://127.0.0.1:47269/ws/events")
    #[must_use]
    pub fn websocket_url(&self) -> String {
        format!("ws://{}:{}/ws/events", self.host, self.port)
    }

    /// Validate configuration
    ///
    /// # Returns
    ///
    /// `Result<(), String>` - Ok if valid, error otherwise
    #[must_use]
    pub fn validate(&self) -> Result<(), String> {
        // Validate port range
        if self.port == 0 {
            return Err("Port cannot be zero".to_string());
        }

        // Validate host is not empty
        if self.host.is_empty() {
            return Err("Host cannot be empty".to_string());
        }

        // Validate max connections
        if self.max_ws_connections == 0 {
            return Err("Max WebSocket connections must be greater than zero".to_string());
        }

        // Validate heartbeat interval
        if self.ws_heartbeat_interval_secs == 0 {
            return Err("Heartbeat interval must be greater than zero".to_string());
        }

        // Validate log level
        match self.log_level.as_str() {
            "trace" | "debug" | "info" | "warn" | "error" => {},
            _ => {
                return Err(format!(
                    "Invalid log level: {}. Must be one of: trace, debug, info, warn, error",
                    self.log_level
                ));
            }
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_config() {
        let config = ServerConfig::default();
        assert_eq!(config.host, DEFAULT_HOST);
        assert_eq!(config.port, DEFAULT_PORT);
        assert!(config.cors_origins.len() > 0);
        assert_eq!(config.db_path, "leindex.db");
        assert_eq!(config.max_ws_connections, MAX_WS_CONNECTIONS);
        assert_eq!(config.ws_heartbeat_interval_secs, WS_HEARTBEAT_INTERVAL_SECS);
        assert_eq!(config.enable_logging, true);
        assert_eq!(config.log_level, "info");
    }

    #[test]
    fn test_config_from_env() {
        std::env::set_var("LESERVE_HOST", "0.0.0.0");
        std::env::set_var("LESERVE_PORT", "8080");
        std::env::set_var("LESERVE_DB_PATH", "/tmp/test.db");
        std::env::set_var("LESERVE_LOG_LEVEL", "debug");

        let config = ServerConfig::from_env();

        assert_eq!(config.host, "0.0.0.0");
        assert_eq!(config.port, 8080);
        assert_eq!(config.db_path, "/tmp/test.db");
        assert_eq!(config.log_level, "debug");

        // Clean up
        std::env::remove_var("LESERVE_HOST");
        std::env::remove_var("LESERVE_PORT");
        std::env::remove_var("LESERVE_DB_PATH");
        std::env::remove_var("LESERVE_LOG_LEVEL");
    }

    #[test]
    fn test_config_socket_addr() {
        let config = ServerConfig::default();
        let addr = config.socket_addr().expect("Default socket address should be valid");
        assert_eq!(addr.ip(), std::net::Ipv4Addr::new(127, 0, 0, 1));
        assert_eq!(addr.port(), 47269);
    }

    #[test]
    fn test_config_server_url() {
        let config = ServerConfig {
            host: "localhost".to_string(),
            port: 3000,
            ..Default::default()
        };
        assert_eq!(config.server_url(), "http://localhost:3000");
    }

    #[test]
    fn test_config_websocket_url() {
        let config = ServerConfig {
            host: "localhost".to_string(),
            port: 3000,
            ..Default::default()
        };
        assert_eq!(config.websocket_url(), "ws://localhost:3000/ws/events");
    }

    #[test]
    fn test_config_validate_success() {
        let config = ServerConfig::default();
        assert!(config.validate().is_ok());
    }

    #[test]
    fn test_config_validate_port_zero() {
        let config = ServerConfig {
            port: 0,
            ..Default::default()
        };
        assert!(config.validate().is_err());
    }

    #[test]
    fn test_config_validate_empty_host() {
        let config = ServerConfig {
            host: String::new(),
            ..Default::default()
        };
        assert!(config.validate().is_err());
    }

    #[test]
    fn test_config_validate_invalid_log_level() {
        let config = ServerConfig {
            log_level: "invalid".to_string(),
            ..Default::default()
        };
        assert!(config.validate().is_err());
    }
}

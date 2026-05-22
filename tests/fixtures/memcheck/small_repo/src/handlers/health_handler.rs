//! Health check handler.

/// Health check response.
#[derive(Debug, serde::Serialize)]
pub struct HealthResponse {
    pub status: String,
    pub version: String,
    pub uptime_secs: u64,
}

/// Handle a health check request.
pub fn health_check() -> HealthResponse {
    HealthResponse {
        status: String::from("ok"),
        version: String::from("0.1.0"),
        uptime_secs: 0,
    }
}

/// Readiness check.
pub fn readiness_check() -> bool {
    true
}

/// Liveness check.
pub fn liveness_check() -> bool {
    true
}

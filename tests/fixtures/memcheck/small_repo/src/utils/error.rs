//! Error types and utilities.

/// Application error type.
#[derive(Debug)]
pub enum AppError {
    NotFound(String),
    Validation(String),
    Unauthorized(String),
    Internal(String),
}

impl std::fmt::Display for AppError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AppError::NotFound(msg) => write!(f, "Not found: {}", msg),
            AppError::Validation(msg) => write!(f, "Validation error: {}", msg),
            AppError::Unauthorized(msg) => write!(f, "Unauthorized: {}", msg),
            AppError::Internal(msg) => write!(f, "Internal error: {}", msg),
        }
    }
}

impl std::error::Error for AppError {}

/// Convert any error into an internal AppError.
pub fn internal(msg: impl Into<String>) -> AppError {
    AppError::Internal(msg.into())
}

/// Create a not-found error.
pub fn not_found(msg: impl Into<String>) -> AppError {
    AppError::NotFound(msg.into())
}

/// Create a validation error.
pub fn validation(msg: impl Into<String>) -> AppError {
    AppError::Validation(msg.into())
}

//! API error types

use axum::{
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use serde::Serialize;
use thiserror::Error;

/// Result type for API operations
pub type ApiResult<T> = Result<T, ApiError>;

/// API error with HTTP status code
#[derive(Debug, Clone, Serialize, Error)]
pub struct ApiError {
    /// HTTP status code
    #[serde(skip)]
    pub status: StatusCode,

    /// Error message
    pub message: String,

    /// Optional error code for client handling
    #[serde(skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
}

impl ApiError {
    /// Create a new API error
    pub fn new(status: StatusCode, message: impl Into<String>) -> Self {
        Self {
            status,
            message: message.into(),
            code: None,
        }
    }

    /// Create a new API error with code
    pub fn with_code(
        status: StatusCode,
        message: impl Into<String>,
        code: impl Into<String>,
    ) -> Self {
        Self {
            status,
            message: message.into(),
            code: Some(code.into()),
        }
    }

    /// 400 Bad Request
    pub fn bad_request(message: impl Into<String>) -> Self {
        Self::new(StatusCode::BAD_REQUEST, message)
    }

    /// 404 Not Found
    pub fn not_found(resource: impl Into<String>) -> Self {
        Self::with_code(
            StatusCode::NOT_FOUND,
            format!("Resource not found: {}", resource.into()),
            "NOT_FOUND",
        )
    }

    /// 422 Unprocessable Entity
    pub fn validation(message: impl Into<String>) -> Self {
        Self::with_code(
            StatusCode::UNPROCESSABLE_ENTITY,
            message,
            "VALIDATION_ERROR",
        )
    }

    /// 500 Internal Server Error
    pub fn internal(message: impl Into<String>) -> Self {
        Self::with_code(
            StatusCode::INTERNAL_SERVER_ERROR,
            message,
            "INTERNAL_ERROR",
        )
    }

    /// 501 Not Implemented
    pub fn not_implemented(feature: impl Into<String>) -> Self {
        Self::with_code(
            StatusCode::NOT_IMPLEMENTED,
            format!("Feature not implemented: {}", feature.into()),
            "NOT_IMPLEMENTED",
        )
    }

    /// 503 Service Unavailable
    pub fn unavailable(message: impl Into<String>) -> Self {
        Self::with_code(
            StatusCode::SERVICE_UNAVAILABLE,
            message,
            "SERVICE_UNAVAILABLE",
        )
    }
}

impl std::fmt::Display for ApiError {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        match &self.code {
            Some(code) => write!(f, "[{:?}] [{}] {}", self.status, code, self.message),
            None => write!(f, "[{:?}] {}", self.status, self.message),
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let body = Json(serde_json::json!({
            "success": false,
            "error": self.message,
            "code": self.code,
        }));

        (self.status, body).into_response()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_api_error_bad_request() {
        let error = ApiError::bad_request("Invalid input");
        assert_eq!(error.status, StatusCode::BAD_REQUEST);
        assert!(error.message.contains("Invalid input"));
    }

    #[test]
    fn test_api_error_not_found() {
        let error = ApiError::not_found("project_123");
        assert_eq!(error.status, StatusCode::NOT_FOUND);
        assert!(error.message.contains("project_123"));
        assert_eq!(error.code, Some("NOT_FOUND".to_string()));
    }

    #[test]
    fn test_api_error_validation() {
        let error = ApiError::validation("Invalid query parameter");
        assert_eq!(error.status, StatusCode::UNPROCESSABLE_ENTITY);
        assert_eq!(error.code, Some("VALIDATION_ERROR".to_string()));
    }

    #[test]
    fn test_api_error_internal() {
        let error = ApiError::internal("Something went wrong");
        assert_eq!(error.status, StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(error.code, Some("INTERNAL_ERROR".to_string()));
    }

    #[test]
    fn test_api_error_not_implemented() {
        let error = ApiError::not_implemented("edit endpoint");
        assert_eq!(error.status, StatusCode::NOT_IMPLEMENTED);
        assert!(error.message.contains("edit endpoint"));
        assert_eq!(error.code, Some("NOT_IMPLEMENTED".to_string()));
    }

    #[test]
    fn test_api_error_unavailable() {
        let error = ApiError::unavailable("Service temporarily down");
        assert_eq!(error.status, StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(error.code, Some("SERVICE_UNAVAILABLE".to_string()));
    }

    #[test]
    fn test_api_error_display() {
        let error = ApiError::not_found("test");
        let display = format!("{}", error);
        assert!(display.contains("NOT_FOUND"));
        assert!(display.contains("test"));
    }

    #[test]
    fn test_api_error_into_response() {
        let error = ApiError::bad_request("test error");
        let response = error.into_response();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }
}

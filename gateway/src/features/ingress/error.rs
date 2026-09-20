use axum::{
    http::StatusCode,
    response::{IntoResponse, Json, Response},
};
use serde_json::json;

use crate::features::executor::error::ExecutorError;

/// Errors for ingress AI routing processing operations.
///
/// Each variant represents a specific business operation failure,
/// providing clear context for debugging and monitoring.
#[derive(Debug, thiserror::Error)]
pub enum IngressError {
    /// Invalid client request (400 Bad Request).
    #[error("Invalid request format: {0}")]
    InvalidRequest(String),

    /// Missing authentication (401 Unauthorized).
    #[error("Missing required authentication")]
    AuthenticationRequired,

    /// Insufficient permissions (403 Forbidden).
    #[error("Insufficient permissions for this operation")]
    AuthorizationFailed,

    /// Configured route lookup failed (500 Internal Server Error).
    #[error("Request orchestration failed")]
    RequestOrchestrationFailed,

    /// LLM execution failed (wraps ExecutorError).
    #[error(transparent)]
    ExecutionFailed(#[from] ExecutorError),
}

impl IntoResponse for IngressError {
    /// Converts ingress errors into HTTP JSON responses with appropriate status codes.
    fn into_response(self) -> Response {
        match self {
            Self::ExecutionFailed(e) => e.into_response(),
            _ => {
                let (status, message) = match self {
                    Self::InvalidRequest(msg) => (StatusCode::BAD_REQUEST, msg),
                    Self::AuthenticationRequired => (
                        StatusCode::UNAUTHORIZED,
                        "Authentication is required to access this resource.".to_string(),
                    ),
                    Self::AuthorizationFailed => (
                        StatusCode::FORBIDDEN,
                        "You do not have permission to perform this operation."
                            .to_string(),
                    ),
                    Self::RequestOrchestrationFailed => (
                        StatusCode::INTERNAL_SERVER_ERROR,
                        "An internal error occurred while processing your request."
                            .to_string(),
                    ),
                    Self::ExecutionFailed(_) => unreachable!(),
                };

                let body = Json(json!({ "error": message }));
                (status, body).into_response()
            }
        }
    }
}

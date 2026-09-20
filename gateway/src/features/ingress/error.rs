use axum::response::{IntoResponse, Response};

use crate::core::http_error::{error_response, ErrorCode};
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

    /// Request body exceeded the configured admission limit.
    #[error("Request body is too large")]
    PayloadTooLarge,

    /// Local generation admission is saturated.
    #[error("The service is overloaded")]
    Overloaded,

    /// LLM execution failed (wraps ExecutorError).
    #[error(transparent)]
    ExecutionFailed(#[from] ExecutorError),
}

impl IngressError {
    fn http_mapping(&self) -> (ErrorCode, String) {
        match self {
            Self::InvalidRequest(message) => (ErrorCode::InvalidRequest, message.clone()),
            Self::AuthenticationRequired => {
                (ErrorCode::Unauthorized, "Authentication failed".to_string())
            }
            Self::AuthorizationFailed => (
                ErrorCode::Forbidden,
                "You do not have permission to perform this operation.".to_string(),
            ),
            Self::RequestOrchestrationFailed => (
                ErrorCode::InternalError,
                "An internal error occurred".to_string(),
            ),
            Self::PayloadTooLarge => (
                ErrorCode::PayloadTooLarge,
                "Request body is too large".to_string(),
            ),
            Self::Overloaded => (
                ErrorCode::Overloaded,
                "The service is overloaded".to_string(),
            ),
            Self::ExecutionFailed(_) => unreachable!("mapped by IntoResponse"),
        }
    }
}

impl IntoResponse for IngressError {
    /// Converts ingress errors into the canonical protected-API envelope.
    fn into_response(self) -> Response {
        match self {
            Self::ExecutionFailed(error) => error.into_response(),
            other => {
                let (code, message) = other.http_mapping();
                error_response(code, message, None)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{body, http::StatusCode};

    async fn envelope(error: IngressError) -> (StatusCode, serde_json::Value) {
        let response = error.into_response();
        let status = response.status();
        let bytes = body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        (status, serde_json::from_slice(&bytes).unwrap())
    }

    #[tokio::test]
    async fn invalid_request_uses_canonical_envelope() {
        let (status, body) =
            envelope(IngressError::InvalidRequest("Unknown route".into())).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(body["error"]["code"], "INVALID_REQUEST");
        assert_eq!(body["error"]["message"], "Unknown route");
        assert!(body["error"]["request_id"].is_string());
        assert!(body.get("response").is_none());
    }

    #[tokio::test]
    async fn execution_failures_preserve_executor_mapping() {
        let (status, body) = envelope(IngressError::ExecutionFailed(
            ExecutorError::AuthenticationFailed("openai".into()),
        ))
        .await;
        assert_eq!(status, StatusCode::BAD_GATEWAY);
        assert_eq!(body["error"]["code"], "UPSTREAM_AUTHENTICATION");
        assert_ne!(status, StatusCode::UNAUTHORIZED);
        assert!(!body.to_string().contains("openai"));
    }

    #[tokio::test]
    async fn future_admission_variants_map_without_enforcement() {
        let (too_large_status, too_large) = envelope(IngressError::PayloadTooLarge).await;
        assert_eq!(too_large_status, StatusCode::PAYLOAD_TOO_LARGE);
        assert_eq!(too_large["error"]["code"], "PAYLOAD_TOO_LARGE");

        let (overloaded_status, overloaded) = envelope(IngressError::Overloaded).await;
        assert_eq!(overloaded_status, StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(overloaded["error"]["code"], "OVERLOADED");
    }
}

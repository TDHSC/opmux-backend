use axum::response::{IntoResponse, Response};

use crate::core::http_error::{error_response, ErrorCode};
use crate::features::{auth, health, ingress};

/// The single, top-level error type for the entire application.
#[derive(Debug, thiserror::Error)]
pub enum AppError {
    /// Authentication and key-management failures.
    #[error(transparent)]
    Auth(#[from] auth::error::AuthError),

    /// Health and readiness probe failures.
    #[error(transparent)]
    Health(#[from] health::error::HealthError),

    /// Ingress routing and execution failures.
    #[error(transparent)]
    Ingress(#[from] ingress::error::IngressError),

    /// Unexpected internal fault. Always a generic 500 envelope.
    #[error("internal error")]
    Internal,
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        match self {
            AppError::Auth(error) => error.into_response(),
            AppError::Health(error) => {
                tracing::error!(
                    error_code = "health_check_failed",
                    "health check failed"
                );
                error.into_response()
            }
            AppError::Ingress(error) => error.into_response(),
            AppError::Internal => error_response(
                ErrorCode::InternalError,
                "An internal error occurred",
                None,
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::correlation::RequestContext;
    use crate::core::http_error::scope_request_context;
    use axum::body;
    use axum::http::StatusCode;

    #[tokio::test]
    async fn unexpected_internal_app_error_is_generic_500() {
        let ctx = RequestContext::new("req-app-internal".to_string(), None);
        let response =
            scope_request_context(ctx, async { AppError::Internal.into_response() })
                .await;
        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
        let bytes = body::to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("body");
        let body: serde_json::Value = serde_json::from_slice(&bytes).expect("json");
        assert_eq!(body["error"]["code"], "INTERNAL_ERROR");
        assert_eq!(body["error"]["message"], "An internal error occurred");
        assert_eq!(body["error"]["request_id"], "req-app-internal");
        assert!(body.get("response").is_none());
        let encoded = body.to_string();
        assert!(!encoded.contains("stack"));
        assert!(!encoded.contains("panic"));
    }
}

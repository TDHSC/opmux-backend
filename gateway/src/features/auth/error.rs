//! Authentication Feature Error Types
//!
//! Errors specific to authentication operations following the business
//! operation model.

use axum::response::{IntoResponse, Response};

use super::persist::{AuthStoreError, MAX_KEY_LIST_LIMIT};
use super::provision::ProvisionError;
use crate::core::http_error::{error_response, ErrorCode};

/// Errors specific to authentication operations.
/// Each variant corresponds to a failed business operation.
#[derive(Debug, thiserror::Error)]
pub enum AuthError {
    /// Presented credential is missing, malformed, unknown, revoked, or ambiguous.
    #[error("API key validation failed")]
    InvalidCredentials,

    /// Authenticated key kind cannot perform the requested operation.
    #[error("Insufficient permissions for this operation")]
    CapabilityDenied,

    /// Create/list request fields are invalid or attempt ownership override.
    #[error("Invalid key management request: {0}")]
    InvalidInput(String),

    /// Same-tenant key is missing. Other-tenant identifiers use this too.
    #[error("API key was not found")]
    KeyNotFound,

    /// The authentication datastore is unreachable or timed out.
    #[error("Authentication dependency unavailable")]
    StoreUnavailable,

    /// The protected-request deadline elapsed during authentication.
    #[error("The request deadline was exceeded")]
    DeadlineExceeded,
}

impl From<AuthStoreError> for AuthError {
    fn from(error: AuthStoreError) -> Self {
        match error {
            AuthStoreError::InvalidLimit => Self::InvalidInput(format!(
                "limit must be between 1 and {MAX_KEY_LIST_LIMIT}"
            )),
            _ => Self::StoreUnavailable,
        }
    }
}

impl From<ProvisionError> for AuthError {
    fn from(error: ProvisionError) -> Self {
        match error {
            ProvisionError::InvalidName => {
                Self::InvalidInput("name must be 1 to 128 characters".to_string())
            }
            _ => Self::StoreUnavailable,
        }
    }
}

impl AuthError {
    fn http_mapping(&self) -> (ErrorCode, String) {
        match self {
            Self::InvalidCredentials => {
                (ErrorCode::Unauthorized, "Authentication failed".to_string())
            }
            Self::CapabilityDenied => (
                ErrorCode::Forbidden,
                "You do not have permission to perform this operation.".to_string(),
            ),
            Self::InvalidInput(message) => (ErrorCode::InvalidRequest, message.clone()),
            Self::KeyNotFound => {
                (ErrorCode::NotFound, "API key was not found".to_string())
            }
            Self::StoreUnavailable => (
                ErrorCode::AuthDependencyUnavailable,
                "Authentication dependency unavailable".to_string(),
            ),
            Self::DeadlineExceeded => (
                ErrorCode::DeadlineExceeded,
                "The request deadline was exceeded".to_string(),
            ),
        }
    }
}

impl IntoResponse for AuthError {
    fn into_response(self) -> Response {
        let (code, message) = self.http_mapping();
        error_response(code, message, None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{body, http::StatusCode, response::IntoResponse};

    async fn envelope(error: AuthError) -> (StatusCode, serde_json::Value) {
        let response = error.into_response();
        let status = response.status();
        let bytes = body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        (status, serde_json::from_slice(&bytes).unwrap())
    }

    #[tokio::test]
    async fn invalid_credentials_map_to_401() {
        let (status, body) = envelope(AuthError::InvalidCredentials).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        assert_eq!(body["error"]["code"], "UNAUTHORIZED");
        assert_eq!(body["error"]["message"], "Authentication failed");
        assert!(body["error"]["request_id"].is_string());
        let encoded = body.to_string();
        assert!(!encoded.contains("digest"));
        assert!(!encoded.contains("postgres"));
    }

    #[tokio::test]
    async fn capability_denied_maps_to_403() {
        let (status, body) = envelope(AuthError::CapabilityDenied).await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        assert_eq!(body["error"]["code"], "FORBIDDEN");
        assert!(body["error"]["message"]
            .as_str()
            .unwrap()
            .contains("permission"));
        assert!(!body.to_string().contains("digest"));
    }

    #[tokio::test]
    async fn invalid_input_maps_to_400() {
        let (status, body) = envelope(AuthError::InvalidInput(
            "kind must be management or inference".into(),
        ))
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(body["error"]["code"], "INVALID_REQUEST");
        assert_eq!(
            body["error"]["message"],
            "kind must be management or inference"
        );
        assert!(!body.to_string().contains("digest"));
    }

    #[tokio::test]
    async fn key_not_found_maps_to_indistinguishable_404() {
        let (status, body) = envelope(AuthError::KeyNotFound).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(body["error"]["code"], "NOT_FOUND");
        assert_eq!(body["error"]["message"], "API key was not found");
        let encoded = body.to_string();
        assert!(!encoded.contains("tenant"));
        assert!(!encoded.contains("digest"));
        assert!(!encoded.contains("revoked"));
    }

    #[tokio::test]
    async fn store_unavailable_maps_to_503() {
        let (status, body) = envelope(AuthError::StoreUnavailable).await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(body["error"]["code"], "AUTH_DEPENDENCY_UNAVAILABLE");
        assert!(body["error"]["message"]
            .as_str()
            .unwrap()
            .contains("unavailable"));
        assert_ne!(status, StatusCode::UNAUTHORIZED);
        assert!(!body.to_string().contains("postgres://"));
    }

    #[tokio::test]
    async fn deadline_exceeded_maps_to_504() {
        let (status, body) = envelope(AuthError::DeadlineExceeded).await;
        assert_eq!(status, StatusCode::GATEWAY_TIMEOUT);
        assert_eq!(body["error"]["code"], "DEADLINE_EXCEEDED");
        assert_eq!(
            body["error"]["message"],
            "The request deadline was exceeded"
        );
        assert!(!body.to_string().contains("postgres://"));
    }
}

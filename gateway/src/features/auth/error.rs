//! Authentication Feature Error Types
//!
//! Errors specific to authentication operations following the business operation model

use axum::{
    http::StatusCode,
    response::{IntoResponse, Json, Response},
};
use serde_json::json;

use super::persist::{AuthStoreError, MAX_KEY_LIST_LIMIT};
use super::provision::ProvisionError;

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

impl IntoResponse for AuthError {
    fn into_response(self) -> Response {
        let (status, message) = match self {
            Self::InvalidCredentials => (
                StatusCode::UNAUTHORIZED,
                "Authentication failed".to_string(),
            ),
            Self::CapabilityDenied => (
                StatusCode::FORBIDDEN,
                "You do not have permission to perform this operation.".to_string(),
            ),
            Self::InvalidInput(message) => (StatusCode::BAD_REQUEST, message),
            Self::KeyNotFound => {
                (StatusCode::NOT_FOUND, "API key was not found".to_string())
            }
            Self::StoreUnavailable => (
                StatusCode::SERVICE_UNAVAILABLE,
                "Authentication dependency unavailable".to_string(),
            ),
        };

        let body = Json(json!({ "error": message }));
        (status, body).into_response()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{body, response::IntoResponse};

    #[tokio::test]
    async fn invalid_credentials_map_to_401() {
        let err = AuthError::InvalidCredentials;
        let resp = err.into_response();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
        let bytes = body::to_bytes(resp.into_body(), usize::MAX).await.unwrap();
        let s = String::from_utf8(bytes.to_vec()).unwrap();
        assert!(s.contains("Authentication failed"));
        assert!(!s.contains("digest"));
        assert!(!s.contains("postgres"));
    }

    #[tokio::test]
    async fn capability_denied_maps_to_403() {
        let err = AuthError::CapabilityDenied;
        let resp = err.into_response();
        assert_eq!(resp.status(), StatusCode::FORBIDDEN);
        let bytes = body::to_bytes(resp.into_body(), usize::MAX).await.unwrap();
        let s = String::from_utf8(bytes.to_vec()).unwrap();
        assert!(s.contains("permission"));
        assert!(!s.contains("digest"));
    }

    #[tokio::test]
    async fn invalid_input_maps_to_400() {
        let err = AuthError::InvalidInput("kind must be management or inference".into());
        let resp = err.into_response();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
        let bytes = body::to_bytes(resp.into_body(), usize::MAX).await.unwrap();
        let s = String::from_utf8(bytes.to_vec()).unwrap();
        assert!(s.contains("kind must be management or inference"));
        assert!(!s.contains("digest"));
    }

    #[tokio::test]
    async fn key_not_found_maps_to_indistinguishable_404() {
        let err = AuthError::KeyNotFound;
        let resp = err.into_response();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
        let bytes = body::to_bytes(resp.into_body(), usize::MAX).await.unwrap();
        let s = String::from_utf8(bytes.to_vec()).unwrap();
        assert!(s.contains("API key was not found"));
        assert!(!s.contains("tenant"));
        assert!(!s.contains("digest"));
        assert!(!s.contains("revoked"));
    }

    #[tokio::test]
    async fn store_unavailable_maps_to_503() {
        let err = AuthError::StoreUnavailable;
        let resp = err.into_response();
        assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
        let bytes = body::to_bytes(resp.into_body(), usize::MAX).await.unwrap();
        let s = String::from_utf8(bytes.to_vec()).unwrap();
        assert!(s.contains("unavailable"));
        assert!(!s.contains("401"));
        assert!(!s.contains("invalid"));
    }
}

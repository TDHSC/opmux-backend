//! Authentication Feature Error Types
//!
//! Errors specific to authentication operations following the business operation model

use axum::{
    http::StatusCode,
    response::{IntoResponse, Json, Response},
};
use serde_json::json;

/// Errors specific to authentication operations.
/// Each variant corresponds to a failed business operation.
#[derive(Debug, thiserror::Error)]
pub enum AuthError {
    /// Presented credential is missing, malformed, unknown, revoked, or ambiguous.
    #[error("API key validation failed")]
    InvalidCredentials,

    /// The authentication datastore is unreachable or timed out.
    #[error("Authentication dependency unavailable")]
    StoreUnavailable,
}

impl IntoResponse for AuthError {
    fn into_response(self) -> Response {
        let (status, message) = match self {
            Self::InvalidCredentials => (
                StatusCode::UNAUTHORIZED,
                "Authentication failed".to_string(),
            ),
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

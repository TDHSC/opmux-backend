//! Authentication Data Models
//!
//! Contains all data structures used in the authentication system

use super::persist::ApiKeyKind;
use uuid::Uuid;

/// Authentication context injected into requests after successful authentication.
///
/// Tenant, key, and kind come from the persisted credential. Request metadata
/// cannot replace these fields.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthContext {
    /// Owning tenant identifier from the authenticated key.
    pub client_id: Uuid,
    /// Persisted key identifier.
    pub key_id: Uuid,
    /// Immutable management or inference kind.
    pub kind: ApiKeyKind,
}

/// Axum extractor for AuthContext
/// Allows handlers to easily access authentication context
impl<S> axum::extract::FromRequestParts<S> for AuthContext
where
    S: Send + Sync,
{
    type Rejection = axum::http::StatusCode;

    async fn from_request_parts(
        parts: &mut axum::http::request::Parts,
        _state: &S,
    ) -> Result<Self, Self::Rejection> {
        parts
            .extensions
            .get::<AuthContext>()
            .cloned()
            .ok_or(axum::http::StatusCode::INTERNAL_SERVER_ERROR)
    }
}

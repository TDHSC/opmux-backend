//! Authentication Data Models
//!
//! Contains all data structures used in the authentication system

use super::persist::{ApiKeyKind, ApiKeyRecord, MAX_KEY_LIST_LIMIT};
use chrono::{DateTime, Utc};
use serde::Serialize;
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

/// Safe public key metadata. Omits credential and digest.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ApiKeyMetadata {
    /// Owning tenant identifier from the stored row.
    pub client_id: Uuid,
    /// Persisted key identifier.
    pub key_id: Uuid,
    /// Safe public display identifier.
    pub display_id: String,
    /// Operator-assigned key name.
    pub name: String,
    /// Immutable `management` or `inference` kind.
    pub kind: String,
    /// Creation timestamp.
    pub created_at: DateTime<Utc>,
    /// Last successful authentication, if any.
    pub last_used_at: Option<DateTime<Utc>>,
    /// Revocation timestamp, if revoked.
    pub revoked_at: Option<DateTime<Utc>>,
}

impl From<ApiKeyRecord> for ApiKeyMetadata {
    fn from(record: ApiKeyRecord) -> Self {
        Self {
            client_id: record.client_id,
            key_id: record.id,
            display_id: record.display_id,
            name: record.name,
            kind: record.kind.as_str().to_string(),
            created_at: record.created_at,
            last_used_at: record.last_used_at,
            revoked_at: record.revoked_at,
        }
    }
}

/// Validated inventory paging and kind filter.
///
/// Tenant scope is never taken from these fields. Handlers reject ownership
/// selectors before constructing this type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KeyListOptions {
    /// Page size, from 1 through [`MAX_KEY_LIST_LIMIT`].
    pub limit: i64,
    /// Number of newest-first keys to skip.
    pub offset: i64,
    /// Optional immutable kind filter.
    pub kind: Option<ApiKeyKind>,
}

impl Default for KeyListOptions {
    fn default() -> Self {
        Self {
            limit: MAX_KEY_LIST_LIMIT,
            offset: 0,
            kind: None,
        }
    }
}

/// Tenant-scoped inventory of safe key metadata.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct KeyInventory {
    /// Keys owned by the authenticated tenant, newest first.
    pub keys: Vec<ApiKeyMetadata>,
    /// True when more same-tenant keys exist after this page.
    pub has_more: bool,
}

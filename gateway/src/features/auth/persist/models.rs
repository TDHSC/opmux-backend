//! Internal persisted authentication records.
//!
//! These types are not HTTP DTOs. Digests are omitted from `Debug`.

use super::error::AuthStoreError;
use chrono::{DateTime, Utc};
use std::fmt;
use uuid::Uuid;

/// SHA-256 digest length in bytes.
pub const KEY_DIGEST_LEN: usize = 32;
/// Documented maximum keys returned by a tenant inventory query.
pub const MAX_KEY_LIST_LIMIT: i64 = 100;
/// Version recorded by the Supabase migration history for this schema.
pub const AUTH_MIGRATION_VERSION: &str = "20260919194500";

/// Immutable API key kind stored as text with a database check constraint.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApiKeyKind {
    /// Tenant management credential.
    Management,
    /// Inference-only credential.
    Inference,
}

impl ApiKeyKind {
    /// Returns the stored kind token.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Management => "management",
            Self::Inference => "inference",
        }
    }

    pub(crate) fn parse(raw: &str) -> Result<Self, sqlx::Error> {
        match raw {
            "management" => Ok(Self::Management),
            "inference" => Ok(Self::Inference),
            _ => Err(sqlx::Error::Decode(
                "unexpected api key kind".to_string().into(),
            )),
        }
    }
}

/// Fixed-length SHA-256 digest of a generated credential.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct KeyDigest([u8; KEY_DIGEST_LEN]);

impl KeyDigest {
    /// Wraps an already-hashed 32-byte digest.
    pub fn from_bytes(bytes: [u8; KEY_DIGEST_LEN]) -> Self {
        Self(bytes)
    }

    /// Copies a slice into a digest.
    ///
    /// # Errors
    /// Returns `InvalidDigestLength` when the slice is not 32 bytes.
    pub fn from_slice(bytes: &[u8]) -> Result<Self, AuthStoreError> {
        let bytes: [u8; KEY_DIGEST_LEN] = bytes
            .try_into()
            .map_err(|_| AuthStoreError::InvalidDigestLength)?;
        Ok(Self(bytes))
    }

    /// Returns the digest bytes for parameterized SQL.
    pub fn as_bytes(&self) -> &[u8; KEY_DIGEST_LEN] {
        &self.0
    }
}

impl fmt::Debug for KeyDigest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("KeyDigest([redacted])")
    }
}

/// Insert payload for a tenant/client row.
#[derive(Debug, Clone)]
pub struct NewClient {
    /// Caller-supplied client identifier.
    pub id: Uuid,
    /// Operator-visible display name.
    pub display_name: String,
    /// Creation timestamp.
    pub created_at: DateTime<Utc>,
}

/// Insert payload for an API key. The digest must already be hashed.
#[derive(Clone)]
pub struct NewApiKey {
    /// Caller-supplied key identifier.
    pub id: Uuid,
    /// Owning client identifier.
    pub client_id: Uuid,
    /// SHA-256 digest of the generated credential.
    pub digest: KeyDigest,
    /// Safe public display identifier, not the secret.
    pub display_id: String,
    /// Operator-assigned name.
    pub name: String,
    /// Immutable management or inference kind.
    pub kind: ApiKeyKind,
    /// Creation timestamp.
    pub created_at: DateTime<Utc>,
}

impl fmt::Debug for NewApiKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("NewApiKey")
            .field("id", &self.id)
            .field("client_id", &self.client_id)
            .field("digest", &self.digest)
            .field("display_id", &self.display_id)
            .field("name", &self.name)
            .field("kind", &self.kind)
            .field("created_at", &self.created_at)
            .finish()
    }
}

/// Persisted client/tenant record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClientRecord {
    /// Client identifier.
    pub id: Uuid,
    /// Display name.
    pub display_name: String,
    /// Creation timestamp.
    pub created_at: DateTime<Utc>,
}

/// Persisted API key record. Not a public response DTO.
#[derive(Clone, PartialEq, Eq)]
pub struct ApiKeyRecord {
    /// Key identifier.
    pub id: Uuid,
    /// Owning client identifier.
    pub client_id: Uuid,
    /// Stored SHA-256 digest.
    pub digest: KeyDigest,
    /// Safe display identifier.
    pub display_id: String,
    /// Key name.
    pub name: String,
    /// Immutable kind.
    pub kind: ApiKeyKind,
    /// Creation timestamp.
    pub created_at: DateTime<Utc>,
    /// Last successful authentication, if any.
    pub last_used_at: Option<DateTime<Utc>>,
    /// Revocation timestamp, if revoked.
    pub revoked_at: Option<DateTime<Utc>>,
}

impl ApiKeyRecord {
    /// Returns true when the key has a revocation timestamp.
    pub fn is_revoked(&self) -> bool {
        self.revoked_at.is_some()
    }
}

impl fmt::Debug for ApiKeyRecord {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ApiKeyRecord")
            .field("id", &self.id)
            .field("client_id", &self.client_id)
            .field("digest", &"[redacted]")
            .field("display_id", &self.display_id)
            .field("name", &self.name)
            .field("kind", &self.kind)
            .field("created_at", &self.created_at)
            .field("last_used_at", &self.last_used_at)
            .field("revoked_at", &self.revoked_at)
            .finish()
    }
}

/// Result of a tenant-scoped revocation attempt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RevokeOutcome {
    /// The key was active and is now revoked.
    Revoked(ApiKeyRecord),
    /// The key was already revoked; the original timestamp is unchanged.
    AlreadyRevoked(ApiKeyRecord),
    /// No same-tenant key exists. Other-tenant IDs use this variant too.
    NotFound,
}

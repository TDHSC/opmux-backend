//! Fallible `Send + Sync` authentication store interface.

use super::error::AuthStoreError;
use super::models::{
    ApiKeyKind, ApiKeyRecord, ClientRecord, KeyDigest, NewApiKey, NewClient,
    RevokeOutcome,
};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use uuid::Uuid;

/// Persistence operations for clients and API keys.
///
/// Implementations must parameterize SQL, include tenant predicates on
/// scoped writes, and return `Err` when the database fails. Lookup by digest
/// is global because authentication does not yet know the tenant.
#[async_trait]
pub trait AuthStore: Send + Sync {
    /// Inserts a client and its first key in one transaction.
    ///
    /// # Parameters
    /// - `client` - New tenant row
    /// - `key` - New key that must belong to `client.id`
    ///
    /// # Returns
    /// The committed client and key records
    ///
    /// # Errors
    /// Propagates constraint and availability failures. A key failure leaves
    /// no client row.
    async fn provision_client_with_key(
        &self,
        client: NewClient,
        key: NewApiKey,
    ) -> Result<(ClientRecord, ApiKeyRecord), AuthStoreError>;

    /// Inserts a key for an existing client.
    ///
    /// # Parameters
    /// - `key` - New key including `client_id`
    async fn insert_key(&self, key: NewApiKey) -> Result<ApiKeyRecord, AuthStoreError>;

    /// Looks up a key by SHA-256 digest, including revoked rows.
    ///
    /// # Parameters
    /// - `digest` - Pre-hashed credential digest
    async fn find_key_by_digest(
        &self,
        digest: &KeyDigest,
    ) -> Result<Option<ApiKeyRecord>, AuthStoreError>;

    /// Authenticates a digest under a row lock and records last-used.
    ///
    /// Missing keys return `None`. Revoked keys return the committed row
    /// without updating `last_used_at`. Active keys update `last_used_at`
    /// monotonically inside the same bounded transaction, then return the
    /// committed row. Callers must deny revoked records. This is the
    /// authoritative authentication write, not a detached touch. Cancelling
    /// the future must not keep a shared pool slot until session statement
    /// timeout; it does not promise instantaneous remote rollback.
    ///
    /// # Parameters
    /// - `digest` - Pre-hashed credential digest
    /// - `used_at` - Successful authentication time
    async fn authenticate_digest(
        &self,
        digest: &KeyDigest,
        used_at: DateTime<Utc>,
    ) -> Result<Option<ApiKeyRecord>, AuthStoreError>;

    /// Records last-used when the timestamp is newer and the key is active.
    ///
    /// # Parameters
    /// - `client_id` - Authenticated tenant
    /// - `key_id` - Authenticated key
    /// - `used_at` - Successful authentication time
    ///
    /// # Returns
    /// `true` when a row was updated
    async fn touch_last_used(
        &self,
        client_id: Uuid,
        key_id: Uuid,
        used_at: DateTime<Utc>,
    ) -> Result<bool, AuthStoreError>;

    /// Lists keys for one tenant, newest first, up to the requested page.
    ///
    /// # Parameters
    /// - `client_id` - Tenant whose inventory is requested
    /// - `limit` - Positive page size, at most `MAX_KEY_LIST_LIMIT`
    /// - `offset` - Non-negative number of newest-first rows to skip
    /// - `kind` - Optional kind filter; `None` returns both kinds
    async fn list_keys_for_client(
        &self,
        client_id: Uuid,
        limit: i64,
        offset: i64,
        kind: Option<ApiKeyKind>,
    ) -> Result<Vec<ApiKeyRecord>, AuthStoreError>;

    /// Revokes a same-tenant key without deleting the row.
    ///
    /// Missing and other-tenant identifiers both yield `NotFound`.
    ///
    /// # Parameters
    /// - `client_id` - Authenticated tenant
    /// - `key_id` - Target key
    /// - `revoked_at` - Revocation timestamp for a first revoke
    async fn revoke_key(
        &self,
        client_id: Uuid,
        key_id: Uuid,
        revoked_at: DateTime<Utc>,
    ) -> Result<RevokeOutcome, AuthStoreError>;
}

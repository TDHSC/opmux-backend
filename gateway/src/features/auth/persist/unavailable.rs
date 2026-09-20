//! Fail-closed store used when authentication persistence cannot be reached.

use super::error::AuthStoreError;
use super::models::{
    ApiKeyKind, ApiKeyRecord, ClientRecord, KeyDigest, NewApiKey, NewClient,
    RevokeOutcome,
};
use super::store::AuthStore;
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use uuid::Uuid;

/// Authentication store that reports unavailability for every operation.
///
/// Used by tests that need a fail-closed dependency without a live database.
#[derive(Debug, Default, Clone, Copy)]
pub struct UnavailableAuthStore;

#[async_trait]
impl AuthStore for UnavailableAuthStore {
    async fn provision_client_with_key(
        &self,
        _client: NewClient,
        _key: NewApiKey,
    ) -> Result<(ClientRecord, ApiKeyRecord), AuthStoreError> {
        Err(AuthStoreError::Unavailable)
    }

    async fn insert_key(&self, _key: NewApiKey) -> Result<ApiKeyRecord, AuthStoreError> {
        Err(AuthStoreError::Unavailable)
    }

    async fn find_key_by_digest(
        &self,
        _digest: &KeyDigest,
    ) -> Result<Option<ApiKeyRecord>, AuthStoreError> {
        Err(AuthStoreError::Unavailable)
    }

    async fn authenticate_digest(
        &self,
        _digest: &KeyDigest,
        _used_at: DateTime<Utc>,
    ) -> Result<Option<ApiKeyRecord>, AuthStoreError> {
        Err(AuthStoreError::Unavailable)
    }

    async fn touch_last_used(
        &self,
        _client_id: Uuid,
        _key_id: Uuid,
        _used_at: DateTime<Utc>,
    ) -> Result<bool, AuthStoreError> {
        Err(AuthStoreError::Unavailable)
    }

    async fn list_keys_for_client(
        &self,
        _client_id: Uuid,
        _limit: i64,
        _offset: i64,
        _kind: Option<ApiKeyKind>,
    ) -> Result<Vec<ApiKeyRecord>, AuthStoreError> {
        Err(AuthStoreError::Unavailable)
    }

    async fn revoke_key(
        &self,
        _client_id: Uuid,
        _key_id: Uuid,
        _revoked_at: DateTime<Utc>,
    ) -> Result<RevokeOutcome, AuthStoreError> {
        Err(AuthStoreError::Unavailable)
    }
}

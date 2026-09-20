//! Service Layer - Authentication Business Logic
//!
//! Resolves presented credentials against the persisted store, derives tenant
//! identity and kind, records last-used on successful authentication, and
//! issues tenant-scoped keys through the shared provisioning service.

use super::credentials::hash_credential;
use super::error::AuthError;
use super::models::{ApiKeyMetadata, AuthContext, KeyInventory};
use super::persist::{ApiKeyKind, AuthStore, AuthStoreError, MAX_KEY_LIST_LIMIT};
use super::provision::{IssuedKey, ProvisioningService};
use chrono::Utc;
use std::sync::Arc;

/// Failures from authenticating a presented credential.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthenticateError {
    /// Missing, unknown, revoked, or otherwise unusable credential.
    InvalidCredentials,
    /// Database timeout or unavailability. Fail closed; do not treat as invalid.
    StoreUnavailable,
}

impl From<AuthenticateError> for AuthError {
    fn from(error: AuthenticateError) -> Self {
        match error {
            AuthenticateError::InvalidCredentials => Self::InvalidCredentials,
            AuthenticateError::StoreUnavailable => Self::StoreUnavailable,
        }
    }
}

impl From<AuthStoreError> for AuthenticateError {
    fn from(_: AuthStoreError) -> Self {
        Self::StoreUnavailable
    }
}

/// Authentication service backed by the shared [`AuthStore`].
///
/// Successful authentication updates `last_used_at` before the request is
/// admitted. That write is part of authentication, not a detached task, and
/// happens even if later inference fails. There is no in-process key map:
/// every request hashes then queries the store. HTTP key issuance reuses
/// [`ProvisioningService`] so CLI and API share generation and hashing.
pub struct AuthService {
    store: Arc<dyn AuthStore>,
    provisioning: ProvisioningService,
}

impl AuthService {
    /// Builds a service around a persistence implementation.
    ///
    /// # Parameters
    /// - `store` - Digest lookup, last-used writer, and key inventory
    pub fn new(store: Arc<dyn AuthStore>) -> Self {
        Self {
            provisioning: ProvisioningService::new(store.clone()),
            store,
        }
    }

    /// Requires a management credential for key administration.
    ///
    /// # Parameters
    /// - `actor` - Authenticated context from the persisted key
    ///
    /// # Errors
    /// Returns `CapabilityDenied` for inference keys.
    pub fn require_management(actor: &AuthContext) -> Result<(), AuthError> {
        if actor.kind != ApiKeyKind::Management {
            return Err(AuthError::CapabilityDenied);
        }
        Ok(())
    }

    /// Issues a key for the authenticated tenant.
    ///
    /// Tenant ownership is taken from `actor.client_id`. Request body fields
    /// cannot select another client. Generation and hashing use the same
    /// [`ProvisioningService`] as `opmux-admin`.
    ///
    /// # Parameters
    /// - `actor` - Authenticated management context
    /// - `name` - Key name, 1–128 characters
    /// - `kind` - Immutable management or inference kind
    ///
    /// # Returns
    /// Safe metadata plus the one-time credential
    ///
    /// # Errors
    /// - `CapabilityDenied` when the actor is not a management key
    /// - `InvalidInput` when the name is invalid
    /// - `StoreUnavailable` when persistence fails
    pub async fn create_key(
        &self,
        actor: &AuthContext,
        name: &str,
        kind: ApiKeyKind,
    ) -> Result<IssuedKey, AuthError> {
        Self::require_management(actor)?;
        self.provisioning
            .issue_key(actor.client_id, kind, name)
            .await
            .map_err(AuthError::from)
    }

    /// Lists safe key metadata for the authenticated tenant.
    ///
    /// # Parameters
    /// - `actor` - Authenticated management context
    ///
    /// # Returns
    /// Same-tenant inventory without credentials or digests
    ///
    /// # Errors
    /// - `CapabilityDenied` when the actor is not a management key
    /// - `StoreUnavailable` when persistence fails
    pub async fn list_keys(
        &self,
        actor: &AuthContext,
    ) -> Result<KeyInventory, AuthError> {
        Self::require_management(actor)?;
        let records = self
            .store
            .list_keys_for_client(actor.client_id, MAX_KEY_LIST_LIMIT)
            .await?;
        Ok(KeyInventory {
            keys: records.into_iter().map(ApiKeyMetadata::from).collect(),
        })
    }

    /// Validates a presented credential and returns persisted identity.
    ///
    /// # Flow
    /// 1. Hashes the presented secret before any store access
    /// 2. Looks up the digest, including revoked rows
    /// 3. Rejects missing and revoked keys as invalid
    /// 4. Synchronously records last-used for an active key
    /// 5. Returns client id, key id, and stored kind
    ///
    /// # Parameters
    /// - `presented` - Raw `X-API-Key` value after header parsing
    ///
    /// # Returns
    /// Authenticated context derived only from the stored row
    ///
    /// # Errors
    /// - `InvalidCredentials` when the digest is unknown or revoked
    /// - `StoreUnavailable` when the database fails; callers must fail closed
    #[tracing::instrument(level = "debug", skip(self, presented))]
    pub async fn authenticate(
        &self,
        presented: &str,
    ) -> Result<AuthContext, AuthenticateError> {
        let start = std::time::Instant::now();
        let digest = hash_credential(presented);
        let record = match self.store.find_key_by_digest(&digest).await {
            Ok(record) => record,
            Err(_) => {
                tracing::debug!(
                    duration_ms = start.elapsed().as_millis(),
                    success = false,
                    reason = "store_unavailable",
                    "API key validation failed"
                );
                return Err(AuthenticateError::StoreUnavailable);
            }
        };

        let Some(record) = record else {
            tracing::debug!(
                duration_ms = start.elapsed().as_millis(),
                success = false,
                reason = "unknown_key",
                "API key validation failed"
            );
            return Err(AuthenticateError::InvalidCredentials);
        };

        if record.is_revoked() {
            tracing::debug!(
                duration_ms = start.elapsed().as_millis(),
                success = false,
                reason = "revoked_key",
                "API key validation failed"
            );
            return Err(AuthenticateError::InvalidCredentials);
        }

        if self
            .store
            .touch_last_used(record.client_id, record.id, Utc::now())
            .await
            .is_err()
        {
            tracing::debug!(
                duration_ms = start.elapsed().as_millis(),
                success = false,
                reason = "store_unavailable",
                "API key validation failed"
            );
            return Err(AuthenticateError::StoreUnavailable);
        }

        tracing::debug!(
            duration_ms = start.elapsed().as_millis(),
            success = true,
            "API key validation completed"
        );

        Ok(AuthContext {
            client_id: record.client_id,
            key_id: record.id,
            kind: record.kind,
        })
    }
}

const _: fn() = || {
    fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<AuthService>();
};

#[cfg(test)]
mod tests {
    use super::*;
    use crate::features::auth::persist::{
        ApiKeyKind, ApiKeyRecord, AuthStoreError, ClientRecord, KeyDigest, NewApiKey,
        NewClient, RevokeOutcome, KEY_DIGEST_LEN,
    };
    use crate::features::auth::AuthContext;
    use async_trait::async_trait;
    use chrono::{DateTime, Utc};
    use std::collections::HashMap;
    use std::sync::Mutex;
    use uuid::Uuid;

    struct ScriptedStore {
        records: Mutex<HashMap<KeyDigest, ApiKeyRecord>>,
        lookups: Mutex<u32>,
        touches: Mutex<u32>,
        inserts: Mutex<u32>,
        fail_lookup: bool,
        fail_touch: bool,
    }

    impl ScriptedStore {
        fn with_record(record: ApiKeyRecord) -> Self {
            let mut records = HashMap::new();
            records.insert(record.digest, record);
            Self {
                records: Mutex::new(records),
                lookups: Mutex::new(0),
                touches: Mutex::new(0),
                inserts: Mutex::new(0),
                fail_lookup: false,
                fail_touch: false,
            }
        }

        fn unavailable() -> Self {
            Self {
                records: Mutex::new(HashMap::new()),
                lookups: Mutex::new(0),
                touches: Mutex::new(0),
                inserts: Mutex::new(0),
                fail_lookup: true,
                fail_touch: false,
            }
        }
    }

    #[async_trait]
    impl AuthStore for ScriptedStore {
        async fn provision_client_with_key(
            &self,
            _client: NewClient,
            _key: NewApiKey,
        ) -> Result<(ClientRecord, ApiKeyRecord), AuthStoreError> {
            Err(AuthStoreError::Unavailable)
        }

        async fn insert_key(
            &self,
            key: NewApiKey,
        ) -> Result<ApiKeyRecord, AuthStoreError> {
            *self.inserts.lock().expect("lock") += 1;
            let record = ApiKeyRecord {
                id: key.id,
                client_id: key.client_id,
                digest: key.digest,
                display_id: key.display_id,
                name: key.name,
                kind: key.kind,
                created_at: key.created_at,
                last_used_at: None,
                revoked_at: None,
            };
            self.records
                .lock()
                .expect("lock")
                .insert(record.digest, record.clone());
            Ok(record)
        }

        async fn find_key_by_digest(
            &self,
            digest: &KeyDigest,
        ) -> Result<Option<ApiKeyRecord>, AuthStoreError> {
            *self.lookups.lock().expect("lock") += 1;
            if self.fail_lookup {
                return Err(AuthStoreError::Unavailable);
            }
            Ok(self.records.lock().expect("lock").get(digest).cloned())
        }

        async fn touch_last_used(
            &self,
            _client_id: Uuid,
            _key_id: Uuid,
            _used_at: DateTime<Utc>,
        ) -> Result<bool, AuthStoreError> {
            *self.touches.lock().expect("lock") += 1;
            if self.fail_touch {
                return Err(AuthStoreError::Unavailable);
            }
            Ok(true)
        }

        async fn list_keys_for_client(
            &self,
            client_id: Uuid,
            _limit: i64,
        ) -> Result<Vec<ApiKeyRecord>, AuthStoreError> {
            Ok(self
                .records
                .lock()
                .expect("lock")
                .values()
                .filter(|record| record.client_id == client_id)
                .cloned()
                .collect())
        }

        async fn revoke_key(
            &self,
            _client_id: Uuid,
            _key_id: Uuid,
            _revoked_at: DateTime<Utc>,
        ) -> Result<RevokeOutcome, AuthStoreError> {
            Ok(RevokeOutcome::NotFound)
        }
    }

    fn record(kind: ApiKeyKind, revoked: bool) -> (String, ApiKeyRecord) {
        let presented = format!("opmx_v1_fixture-{}", Uuid::new_v4().simple());
        let digest = hash_credential(&presented);
        let record = ApiKeyRecord {
            id: Uuid::new_v4(),
            client_id: Uuid::new_v4(),
            digest,
            display_id: format!("opk_{}", Uuid::new_v4().simple()),
            name: "fixture".to_string(),
            kind,
            created_at: Utc::now(),
            last_used_at: None,
            revoked_at: revoked.then(Utc::now),
        };
        (presented, record)
    }

    #[tokio::test]
    async fn authenticate_returns_persisted_client_key_and_kind() {
        let (presented, stored) = record(ApiKeyKind::Inference, false);
        let store = Arc::new(ScriptedStore::with_record(stored.clone()));
        let svc = AuthService::new(store.clone());
        let ctx = svc.authenticate(&presented).await.expect("auth");
        assert_eq!(ctx.client_id, stored.client_id);
        assert_eq!(ctx.key_id, stored.id);
        assert_eq!(ctx.kind, ApiKeyKind::Inference);
        assert_eq!(*store.lookups.lock().expect("lock"), 1);
        assert_eq!(*store.touches.lock().expect("lock"), 1);
    }

    #[tokio::test]
    async fn authenticate_rejects_unknown_and_revoked_keys() {
        let store = Arc::new(ScriptedStore::with_record(
            record(ApiKeyKind::Management, true).1,
        ));
        let svc = AuthService::new(store);
        let unknown = svc
            .authenticate("opmx_v1_missing")
            .await
            .expect_err("unknown");
        assert_eq!(unknown, AuthenticateError::InvalidCredentials);

        let (presented, stored) = record(ApiKeyKind::Management, true);
        let store = Arc::new(ScriptedStore::with_record(stored));
        let svc = AuthService::new(store.clone());
        let revoked = svc.authenticate(&presented).await.expect_err("revoked");
        assert_eq!(revoked, AuthenticateError::InvalidCredentials);
        assert_eq!(*store.touches.lock().expect("lock"), 0);
    }

    #[tokio::test]
    async fn authenticate_fails_closed_on_store_errors() {
        let svc = AuthService::new(Arc::new(ScriptedStore::unavailable()));
        let err = svc
            .authenticate("opmx_v1_any")
            .await
            .expect_err("unavailable");
        assert_eq!(err, AuthenticateError::StoreUnavailable);

        let (presented, stored) = record(ApiKeyKind::Inference, false);
        let mut store = ScriptedStore::with_record(stored);
        store.fail_touch = true;
        let svc = AuthService::new(Arc::new(store));
        let err = svc.authenticate(&presented).await.expect_err("touch");
        assert_eq!(err, AuthenticateError::StoreUnavailable);
    }

    #[tokio::test]
    async fn kind_comes_from_stored_row_not_key_name() {
        let (presented, mut stored) = record(ApiKeyKind::Management, false);
        stored.name = "inference-looking-name".to_string();
        let svc = AuthService::new(Arc::new(ScriptedStore::with_record(stored.clone())));
        let ctx = svc.authenticate(&presented).await.expect("auth");
        assert_eq!(ctx.kind, ApiKeyKind::Management);
        assert_eq!(ctx.kind, stored.kind);
    }

    #[test]
    fn digest_length_is_fixed() {
        let digest = hash_credential("opmx_v1_example");
        assert_eq!(digest.as_bytes().len(), KEY_DIGEST_LEN);
    }

    #[tokio::test]
    async fn authenticate_debug_logs_omit_secret_and_digest() {
        let (presented, stored) = record(ApiKeyKind::Inference, false);
        let digest_hex = stored
            .digest
            .as_bytes()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        let buf = std::sync::Arc::new(Mutex::new(Vec::<u8>::new()));
        let writer = buf.clone();
        let subscriber = tracing_subscriber::fmt()
            .with_max_level(tracing::Level::DEBUG)
            .with_writer(move || VecWriter(writer.clone()))
            .finish();
        let _guard = tracing::subscriber::set_default(subscriber);
        let svc = AuthService::new(Arc::new(ScriptedStore::with_record(stored)));
        let _ = svc.authenticate(&presented).await.expect("auth");
        let logs = String::from_utf8(buf.lock().expect("lock").clone()).expect("utf8");
        assert!(!logs.contains(&presented));
        assert!(!logs.contains(&digest_hex));
    }

    #[tokio::test]
    async fn create_key_requires_management_and_uses_actor_tenant() {
        let (_, stored) = record(ApiKeyKind::Management, false);
        let store = Arc::new(ScriptedStore::with_record(stored.clone()));
        let svc = AuthService::new(store.clone());
        let actor = AuthContext {
            client_id: stored.client_id,
            key_id: stored.id,
            kind: ApiKeyKind::Management,
        };
        let issued = svc
            .create_key(&actor, "new-inference", ApiKeyKind::Inference)
            .await
            .expect("create");
        assert_eq!(issued.client_id, actor.client_id);
        assert_eq!(issued.kind, "inference");
        assert_eq!(issued.name, "new-inference");
        assert_eq!(*store.inserts.lock().expect("lock"), 1);
        assert!(issued.credential().starts_with("opmx_v1_"));

        let inference_actor = AuthContext {
            client_id: stored.client_id,
            key_id: stored.id,
            kind: ApiKeyKind::Inference,
        };
        let denied = svc
            .create_key(
                &inference_actor,
                "should-not-create",
                ApiKeyKind::Management,
            )
            .await
            .expect_err("denied");
        assert!(matches!(denied, AuthError::CapabilityDenied));
        assert_eq!(*store.inserts.lock().expect("lock"), 1);

        let listed = svc.list_keys(&actor).await.expect("list");
        assert!(listed
            .keys
            .iter()
            .any(|item| item.key_id == issued.key_id && item.kind == "inference"));
        let list_denied = svc
            .list_keys(&inference_actor)
            .await
            .expect_err("list denied");
        assert!(matches!(list_denied, AuthError::CapabilityDenied));
    }

    struct VecWriter(std::sync::Arc<Mutex<Vec<u8>>>);

    impl std::io::Write for VecWriter {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.lock().expect("lock").extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
}

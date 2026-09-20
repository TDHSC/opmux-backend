//! Tenant and API-key provisioning service.
//!
//! Generates credentials, hashes them, then writes through [`AuthStore`].
//! Public issuance types never include digests. Internal store records never
//! include plaintext secrets.

use super::credentials::{
    generate_credential, hash_credential, EntropyError, SecretSource, SystemEntropy,
    INITIAL_MANAGEMENT_KEY_NAME,
};
use super::persist::{
    ApiKeyKind, ApiKeyRecord, AuthStore, AuthStoreError, NewApiKey, NewClient,
};
use crate::core::config::SecretString;
use chrono::{DateTime, Utc};
use serde::Serialize;
use std::fmt;
use std::sync::Arc;
use uuid::Uuid;

const MIN_NAME_CHARS: usize = 1;
const MAX_NAME_CHARS: usize = 128;

/// Sanitized provisioning failures. Messages never include secrets or SQL.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum ProvisionError {
    /// Display name or key name is empty or longer than 128 characters.
    #[error("name must be 1 to 128 characters")]
    InvalidName,
    /// The referenced client does not exist.
    #[error("unknown client")]
    UnknownClient,
    /// Operating-system CSPRNG failed.
    #[error("entropy source failed")]
    EntropyUnavailable,
    /// The database is unreachable or timed out.
    #[error("auth store unavailable")]
    StoreUnavailable,
    /// A client primary key already exists.
    #[error("duplicate client")]
    DuplicateClient,
    /// The SHA-256 digest is already stored.
    #[error("duplicate key digest")]
    DuplicateDigest,
    /// The safe display identifier is already stored.
    #[error("duplicate display id")]
    DuplicateDisplayId,
    /// A check constraint rejected the write.
    #[error("check constraint rejected")]
    CheckViolation,
    /// The connected role lacks the required grant.
    #[error("permission denied")]
    PermissionDenied,
    /// Another uniqueness conflict on a persisted identity column.
    #[error("conflicting persisted identity")]
    Conflict,
}

impl From<AuthStoreError> for ProvisionError {
    fn from(err: AuthStoreError) -> Self {
        match err {
            AuthStoreError::Unavailable | AuthStoreError::InvalidLimit => {
                Self::StoreUnavailable
            }
            AuthStoreError::DuplicateClient => Self::DuplicateClient,
            AuthStoreError::DuplicateDigest => Self::DuplicateDigest,
            AuthStoreError::DuplicateDisplayId => Self::DuplicateDisplayId,
            AuthStoreError::UnknownClient => Self::UnknownClient,
            AuthStoreError::CheckViolation => Self::CheckViolation,
            AuthStoreError::PermissionDenied => Self::PermissionDenied,
            AuthStoreError::Conflict | AuthStoreError::InvalidDigestLength => {
                Self::Conflict
            }
        }
    }
}

impl From<EntropyError> for ProvisionError {
    fn from(_: EntropyError) -> Self {
        Self::EntropyUnavailable
    }
}

/// One-time issuance result. The secret is present only on this object.
#[derive(Clone, Serialize)]
pub struct IssuedKey {
    /// Owning tenant identifier.
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
    #[serde(serialize_with = "serialize_secret")]
    credential: SecretString,
}

impl IssuedKey {
    fn from_record(record: ApiKeyRecord, credential: SecretString) -> Self {
        Self {
            client_id: record.client_id,
            key_id: record.id,
            display_id: record.display_id,
            name: record.name,
            kind: record.kind.as_str().to_string(),
            created_at: record.created_at,
            credential,
        }
    }

    /// Returns the one-time credential for intentional issuance output.
    pub fn credential(&self) -> &str {
        self.credential.expose()
    }
}

impl fmt::Debug for IssuedKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("IssuedKey")
            .field("client_id", &self.client_id)
            .field("key_id", &self.key_id)
            .field("display_id", &self.display_id)
            .field("name", &self.name)
            .field("kind", &self.kind)
            .field("created_at", &self.created_at)
            .field("credential", &"[redacted]")
            .finish()
    }
}

/// Tenant creation result, including the initial management key.
#[derive(Clone, Serialize)]
pub struct CreatedTenant {
    /// Tenant display name.
    pub display_name: String,
    #[serde(flatten)]
    pub key: IssuedKey,
}

impl fmt::Debug for CreatedTenant {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CreatedTenant")
            .field("display_name", &self.display_name)
            .field("key", &self.key)
            .finish()
    }
}

/// Persisted identity derived from a presented credential.
///
/// This is not an HTTP DTO and does not include a digest or secret.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeyIdentity {
    /// Owning tenant identifier.
    pub client_id: Uuid,
    /// Persisted key identifier.
    pub key_id: Uuid,
    /// Immutable key kind.
    pub kind: ApiKeyKind,
    /// Operator-assigned name.
    pub name: String,
    /// Safe display identifier.
    pub display_id: String,
    /// Whether the key has a revocation timestamp.
    pub revoked: bool,
}

impl KeyIdentity {
    fn from_record(record: ApiKeyRecord) -> Self {
        Self {
            client_id: record.client_id,
            key_id: record.id,
            kind: record.kind,
            name: record.name,
            display_id: record.display_id,
            revoked: record.revoked_at.is_some(),
        }
    }
}

/// Shared provisioning and credential-resolution service.
pub struct ProvisioningService {
    store: Arc<dyn AuthStore>,
    entropy: Arc<dyn SecretSource>,
}

impl ProvisioningService {
    /// Builds a service that uses the operating-system CSPRNG.
    ///
    /// # Parameters
    /// - `store` - Persistence implementation
    pub fn new(store: Arc<dyn AuthStore>) -> Self {
        Self::with_entropy(store, Arc::new(SystemEntropy))
    }

    /// Builds a service with an injected entropy source.
    ///
    /// # Parameters
    /// - `store` - Persistence implementation
    /// - `entropy` - CSPRNG used for secret and display-id bytes
    pub fn with_entropy(
        store: Arc<dyn AuthStore>,
        entropy: Arc<dyn SecretSource>,
    ) -> Self {
        Self { store, entropy }
    }

    /// Creates a tenant and its initial management key in one transaction.
    ///
    /// # Parameters
    /// - `display_name` - Tenant display name, 1–128 characters
    ///
    /// # Returns
    /// Safe identifiers plus the one-time management credential
    ///
    /// # Errors
    /// Returns a sanitized error when validation, entropy, or persistence fails.
    /// A key-insert failure leaves no client row.
    pub async fn create_tenant(
        &self,
        display_name: &str,
    ) -> Result<CreatedTenant, ProvisionError> {
        let display_name = validate_name(display_name)?;
        let generated = generate_credential(self.entropy.as_ref())?;
        let created_at = Utc::now();
        let client = NewClient {
            id: Uuid::new_v4(),
            display_name: display_name.clone(),
            created_at,
        };
        let key = NewApiKey {
            id: Uuid::new_v4(),
            client_id: client.id,
            digest: generated.digest(),
            display_id: generated.display_id().to_string(),
            name: INITIAL_MANAGEMENT_KEY_NAME.to_string(),
            kind: ApiKeyKind::Management,
            created_at,
        };
        let (client_record, key_record) = self
            .store
            .provision_client_with_key(client, key)
            .await
            .map_err(ProvisionError::from)?;
        Ok(CreatedTenant {
            display_name: client_record.display_name,
            key: IssuedKey::from_record(key_record, generated.into_secret()),
        })
    }

    /// Issues a key for an existing tenant without creating another client.
    ///
    /// # Parameters
    /// - `client_id` - Existing tenant identifier
    /// - `kind` - Immutable management or inference kind
    /// - `name` - Key name, 1–128 characters
    ///
    /// # Returns
    /// Safe identifiers plus the one-time credential
    ///
    /// # Errors
    /// Unknown clients and constraint failures add no replacement key.
    pub async fn issue_key(
        &self,
        client_id: Uuid,
        kind: ApiKeyKind,
        name: &str,
    ) -> Result<IssuedKey, ProvisionError> {
        let name = validate_name(name)?;
        let generated = generate_credential(self.entropy.as_ref())?;
        let key = NewApiKey {
            id: Uuid::new_v4(),
            client_id,
            digest: generated.digest(),
            display_id: generated.display_id().to_string(),
            name,
            kind,
            created_at: Utc::now(),
        };
        let record = self
            .store
            .insert_key(key)
            .await
            .map_err(ProvisionError::from)?;
        Ok(IssuedKey::from_record(record, generated.into_secret()))
    }

    /// Resolves a presented credential to persisted identity and kind.
    ///
    /// Hashes before repository access. Missing credentials return `Ok(None)`,
    /// not an invalid-key store error. The digest is never returned.
    ///
    /// # Parameters
    /// - `credential` - Presented API key
    ///
    /// # Returns
    /// Identity when a matching digest exists
    pub async fn resolve_credential(
        &self,
        credential: &str,
    ) -> Result<Option<KeyIdentity>, ProvisionError> {
        let digest = hash_credential(credential);
        let record = self
            .store
            .find_key_by_digest(&digest)
            .await
            .map_err(ProvisionError::from)?;
        Ok(record.map(KeyIdentity::from_record))
    }
}

fn validate_name(raw: &str) -> Result<String, ProvisionError> {
    let name = raw.trim();
    let chars = name.chars().count();
    if !(MIN_NAME_CHARS..=MAX_NAME_CHARS).contains(&chars) {
        return Err(ProvisionError::InvalidName);
    }
    Ok(name.to_string())
}

fn serialize_secret<S>(secret: &SecretString, serializer: S) -> Result<S::Ok, S::Error>
where
    S: serde::Serializer,
{
    serializer.serialize_str(secret.expose())
}

const _: fn() = || {
    fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<ProvisioningService>();
};

#[cfg(test)]
mod tests {
    use super::super::persist::{ClientRecord, KeyDigest, RevokeOutcome, KEY_DIGEST_LEN};
    use super::*;
    use async_trait::async_trait;
    use std::sync::Mutex;

    struct RecordingStore {
        last_key: Mutex<Option<NewApiKey>>,
    }

    impl RecordingStore {
        fn new() -> Self {
            Self {
                last_key: Mutex::new(None),
            }
        }
    }

    fn record_from_new(key: &NewApiKey) -> ApiKeyRecord {
        ApiKeyRecord {
            id: key.id,
            client_id: key.client_id,
            digest: key.digest,
            display_id: key.display_id.clone(),
            name: key.name.clone(),
            kind: key.kind,
            created_at: key.created_at,
            last_used_at: None,
            revoked_at: None,
        }
    }

    #[async_trait]
    impl AuthStore for RecordingStore {
        async fn provision_client_with_key(
            &self,
            client: NewClient,
            key: NewApiKey,
        ) -> Result<(ClientRecord, ApiKeyRecord), AuthStoreError> {
            *self.last_key.lock().expect("lock") = Some(key.clone());
            Ok((
                ClientRecord {
                    id: client.id,
                    display_name: client.display_name,
                    created_at: client.created_at,
                },
                record_from_new(&key),
            ))
        }

        async fn insert_key(
            &self,
            key: NewApiKey,
        ) -> Result<ApiKeyRecord, AuthStoreError> {
            *self.last_key.lock().expect("lock") = Some(key.clone());
            Ok(record_from_new(&key))
        }

        async fn find_key_by_digest(
            &self,
            digest: &KeyDigest,
        ) -> Result<Option<ApiKeyRecord>, AuthStoreError> {
            let guard = self.last_key.lock().expect("lock");
            Ok(guard.as_ref().and_then(|key| {
                if &key.digest == digest {
                    Some(record_from_new(key))
                } else {
                    None
                }
            }))
        }

        async fn authenticate_digest(
            &self,
            digest: &KeyDigest,
            _used_at: DateTime<Utc>,
        ) -> Result<Option<ApiKeyRecord>, AuthStoreError> {
            self.find_key_by_digest(digest).await
        }

        async fn touch_last_used(
            &self,
            _client_id: Uuid,
            _key_id: Uuid,
            _used_at: DateTime<Utc>,
        ) -> Result<bool, AuthStoreError> {
            Ok(false)
        }

        async fn list_keys_for_client(
            &self,
            _client_id: Uuid,
            _limit: i64,
            _offset: i64,
            _kind: Option<ApiKeyKind>,
        ) -> Result<Vec<ApiKeyRecord>, AuthStoreError> {
            Ok(Vec::new())
        }

        async fn revoke_key(
            &self,
            _client_id: Uuid,
            _key_id: Uuid,
            _revoked_at: DateTime<Utc>,
        ) -> Result<RevokeOutcome, AuthStoreError> {
            Ok(RevokeOutcome::NotFound)
        }

        async fn probe_authentication_access(&self) -> Result<(), AuthStoreError> {
            Ok(())
        }
    }

    #[tokio::test]
    async fn hashes_before_store_and_omits_secret_from_internal_record() {
        let store = Arc::new(RecordingStore::new());
        let service = ProvisioningService::new(store.clone());
        let created = service
            .create_tenant("tenant-a")
            .await
            .expect("create tenant");
        let stored = store
            .last_key
            .lock()
            .expect("lock")
            .clone()
            .expect("stored key");
        let secret = created.key.credential().to_string();
        assert_eq!(stored.digest, hash_credential(&secret));
        assert_eq!(stored.digest.as_bytes().len(), KEY_DIGEST_LEN);
        assert_ne!(stored.display_id, secret);
        assert!(!stored.display_id.contains(&secret));
        assert_eq!(stored.kind, ApiKeyKind::Management);
        assert_eq!(stored.name, INITIAL_MANAGEMENT_KEY_NAME);
        assert_eq!(created.key.kind, "management");

        let rendered = format!("{created:?}");
        assert!(!rendered.contains(&secret));
        let json = serde_json::to_string(&created).expect("json");
        assert!(json.contains("\"credential\""));
        assert!(json.contains(&secret));
    }

    #[tokio::test]
    async fn issue_key_hashes_before_insert() {
        let store = Arc::new(RecordingStore::new());
        let service = ProvisioningService::new(store.clone());
        let client_id = Uuid::new_v4();
        let issued = service
            .issue_key(client_id, ApiKeyKind::Inference, "route-key")
            .await
            .expect("issue");
        let stored = store
            .last_key
            .lock()
            .expect("lock")
            .clone()
            .expect("stored key");
        let secret = issued.credential().to_string();
        assert_eq!(stored.digest, hash_credential(&secret));
        assert_eq!(stored.client_id, client_id);
        assert_eq!(stored.kind, ApiKeyKind::Inference);
        assert_eq!(issued.kind, "inference");
        let identity = service
            .resolve_credential(&secret)
            .await
            .expect("resolve")
            .expect("found");
        assert_eq!(identity.client_id, client_id);
        assert_eq!(identity.key_id, issued.key_id);
        assert_eq!(identity.kind, ApiKeyKind::Inference);
    }

    #[tokio::test]
    async fn rejects_blank_and_oversized_names() {
        let store = Arc::new(RecordingStore::new());
        let service = ProvisioningService::new(store);
        assert_eq!(
            service.create_tenant("   ").await.unwrap_err(),
            ProvisionError::InvalidName
        );
        let long = "n".repeat(129);
        assert_eq!(
            service
                .issue_key(Uuid::new_v4(), ApiKeyKind::Management, &long)
                .await
                .unwrap_err(),
            ProvisionError::InvalidName
        );
    }

    #[tokio::test]
    async fn unknown_credential_resolves_to_none() {
        let store = Arc::new(RecordingStore::new());
        let service = ProvisioningService::new(store);
        let missing = service
            .resolve_credential("opmx_v1_unknown")
            .await
            .expect("resolve");
        assert!(missing.is_none());
    }
}

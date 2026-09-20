//! Authentication Feature Module
//!
//! Provides API key authentication functionality following 3-layer architecture:
//! - Handler Layer: `POST`/`GET /api/v1/auth/keys` for tenant-scoped issuance
//! - Service Layer: persisted request authentication plus shared provisioning
//! - Repository Layer: fallible Postgres store (`persist`)
//!
//! HTTP authentication hashes presented credentials, looks them up in
//! `opmux_private`, and derives tenant/key/kind from the stored row. Management
//! keys may create and list same-tenant keys; inference keys generate only.
//! Mock key acceptance and development bypass are test-only leftovers, not
//! runtime behavior.

pub use config::{get_auth_config, AuthConfig};
pub use credentials::{
    generate_credential, hash_credential, parse_credential_payload, EntropyError,
    SecretSource, SystemEntropy, CREDENTIAL_PREFIX, DISPLAY_ID_RANDOM_LEN,
    INITIAL_MANAGEMENT_KEY_NAME, SECRET_PAYLOAD_LEN,
};
pub use error::AuthError;
pub use handler::{create_api_key, list_api_keys};
pub use models::{ApiKeyMetadata, AuthContext, KeyInventory};
pub use persist::{
    ApiKeyKind, ApiKeyRecord, AuthStore, AuthStoreError, ClientRecord, KeyDigest,
    NewApiKey, NewClient, PostgresAuthStore, RevokeOutcome, UnavailableAuthStore,
    AUTH_MIGRATION_VERSION, KEY_DIGEST_LEN, MAX_KEY_LIST_LIMIT,
};
pub use provision::{
    CreatedTenant, IssuedKey, KeyIdentity, ProvisionError, ProvisioningService,
};
pub use service::{AuthService, AuthenticateError};

pub mod config;
pub mod credentials;
pub mod error;
pub mod handler;
mod models;
pub mod persist;
pub mod provision;
mod service;

#[cfg(test)]
mod mockdata;
#[cfg(test)]
mod repository;

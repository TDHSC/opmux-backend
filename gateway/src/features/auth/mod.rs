//! Authentication Feature Module
//!
//! Provides API key authentication functionality following 3-layer architecture:
//! - Handler Layer: HTTP endpoints for API key management (future)
//! - Service Layer: persisted request authentication plus shared provisioning
//! - Repository Layer: fallible Postgres store (`persist`)
//!
//! HTTP authentication hashes presented credentials, looks them up in
//! `opmux_private`, and derives tenant/key/kind from the stored row. Mock key
//! acceptance and development bypass are test-only leftovers, not runtime
//! behavior.

pub use config::{get_auth_config, AuthConfig};
pub use credentials::{
    generate_credential, hash_credential, parse_credential_payload, EntropyError,
    SecretSource, SystemEntropy, CREDENTIAL_PREFIX, DISPLAY_ID_RANDOM_LEN,
    INITIAL_MANAGEMENT_KEY_NAME, SECRET_PAYLOAD_LEN,
};
pub use error::AuthError;
pub use models::*;
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
mod models;
pub mod persist;
pub mod provision;
mod service;

#[cfg(test)]
mod mockdata;
#[cfg(test)]
mod repository;

// Future: handler module for API key management endpoints
// pub mod handler;

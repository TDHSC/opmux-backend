//! Authentication Feature Module
//!
//! Provides API key authentication functionality following 3-layer architecture:
//! - Handler Layer: HTTP endpoints for API key management (future)
//! - Service Layer: mock request authentication plus shared provisioning
//! - Repository Layer: mock request authentication plus a fallible Postgres store
//!
//! Request authentication still uses the mock repository. Tenant and key
//! provisioning use [`ProvisioningService`] and `opmux-admin`. Fail-closed HTTP
//! wiring of persisted credentials is a later feature.

// Export public interfaces
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
    NewApiKey, NewClient, PostgresAuthStore, RevokeOutcome, AUTH_MIGRATION_VERSION,
    KEY_DIGEST_LEN, MAX_KEY_LIST_LIMIT,
};
pub use provision::{
    CreatedTenant, IssuedKey, KeyIdentity, ProvisionError, ProvisioningService,
};
pub use service::AuthService;

// Internal modules
pub mod config;
pub mod credentials;
pub mod error;
mod mockdata;
mod models;
pub mod persist;
pub mod provision;
mod repository;
mod service;

// Future: handler module for API key management endpoints
// pub mod handler;

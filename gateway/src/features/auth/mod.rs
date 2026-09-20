//! Authentication Feature Module
//!
//! Provides API key authentication functionality following 3-layer architecture:
//! - Handler Layer: HTTP endpoints for API key management (future)
//! - Service Layer: Authentication business logic
//! - Repository Layer: mock request authentication plus a fallible Postgres store
//!
//! Request authentication still uses the mock repository. The persisted store is
//! the SQLx foundation for later provisioning and fail-closed auth wiring.

// Export public interfaces
pub use config::{get_auth_config, AuthConfig};
pub use error::AuthError;
pub use models::*;
pub use persist::{
    ApiKeyKind, ApiKeyRecord, AuthStore, AuthStoreError, ClientRecord, KeyDigest,
    NewApiKey, NewClient, PostgresAuthStore, RevokeOutcome, AUTH_MIGRATION_VERSION,
    KEY_DIGEST_LEN, MAX_KEY_LIST_LIMIT,
};
pub use service::AuthService;

// Internal modules
pub mod config;
pub mod error;
mod mockdata;
mod models;
pub mod persist;
mod repository;
mod service;

// Future: handler module for API key management endpoints
// pub mod handler;

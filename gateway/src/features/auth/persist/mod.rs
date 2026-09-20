//! Persisted authentication models and SQLx store.
//!
//! Runtime SQL is used so crate builds do not need a live database or a
//! checked-in SQLx query ledger. Schema changes live under
//! `supabase/migrations` and are applied as an explicit setup step.

mod error;
mod models;
mod postgres;
mod store;

pub use error::AuthStoreError;
pub use models::{
    ApiKeyKind, ApiKeyRecord, ClientRecord, KeyDigest, NewApiKey, NewClient,
    RevokeOutcome, AUTH_MIGRATION_VERSION, KEY_DIGEST_LEN, MAX_KEY_LIST_LIMIT,
};
pub use postgres::PostgresAuthStore;
pub use store::AuthStore;

const _: fn() = || {
    fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<PostgresAuthStore>();
    assert_send_sync::<std::sync::Arc<dyn AuthStore>>();
};

#[cfg(test)]
mod tests {
    use super::KeyDigest;

    #[test]
    fn digest_debug_is_redacted() {
        let digest = KeyDigest::from_bytes([7; 32]);
        let rendered = format!("{digest:?}");
        assert_eq!(rendered, "KeyDigest([redacted])");
        assert!(!rendered.contains("7"));
    }

    #[test]
    fn digest_rejects_wrong_length() {
        assert!(KeyDigest::from_slice(&[1, 2, 3]).is_err());
    }

    #[test]
    fn postgres_store_uses_runtime_sql_not_sqlx_migrator() {
        let source = include_str!("postgres.rs");
        assert!(!source.contains("sqlx::migrate"));
        assert!(!source.contains("_sqlx_migrations"));
        assert!(source.contains("opmux_private.api_keys"));
        assert!(source.contains("WHERE client_id = $1"));
        assert!(source.contains("WHERE key_digest = $1"));
    }
}

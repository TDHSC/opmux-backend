//! Owned-Supabase helpers for HTTP authentication fixtures.

#![allow(dead_code)]

use gateway::{
    core::db::DatabasePoolConfig,
    features::auth::{ApiKeyKind, AuthService, PostgresAuthStore, ProvisioningService},
};
use std::sync::Arc;
use std::time::Duration;
use uuid::Uuid;

/// Issued inference credential captured privately for a fixture.
pub struct IssuedInference {
    pub client_id: Uuid,
    pub key_id: Uuid,
    pub credential: String,
}

/// Requires `DATABASE_URL` for the owned local Supabase. Does not skip.
pub fn required_database_url() -> String {
    match std::env::var("DATABASE_URL") {
        Ok(url) if !url.trim().is_empty() => url,
        _ => panic!(
            "DATABASE_URL is required for persisted authentication tests and must point at the owned local Supabase on 127.0.0.1:55432. Tests do not skip when the database is unavailable."
        ),
    }
}

/// Connects a bounded test pool to the owned database.
pub async fn test_pool() -> sqlx::PgPool {
    let config = DatabasePoolConfig::new(required_database_url())
        .expect("DATABASE_URL must parse")
        .with_max_connections(2)
        .expect("test pool size")
        .with_acquire_timeout(Duration::from_secs(10))
        .expect("test acquire timeout");
    let pool = config.connect().await.unwrap_or_else(|_| {
        panic!(
            "failed to connect to DATABASE_URL; persisted authentication tests require the owned local Supabase and do not skip"
        )
    });
    let present: bool =
        sqlx::query_scalar("SELECT to_regclass('opmux_private.api_keys') IS NOT NULL")
            .fetch_one(&pool)
            .await
            .expect("database must answer catalog queries");
    if !present {
        panic!(
            "opmux_private.api_keys is missing; run scripts/db-migrate.sh against the owned local database. Persistence tests do not skip."
        );
    }
    pool
}

/// Builds an `AuthService` against an existing pool.
pub fn auth_service_from_pool(pool: sqlx::PgPool) -> Arc<AuthService> {
    Arc::new(AuthService::new(Arc::new(PostgresAuthStore::new(pool))))
}

/// Provisions one tenant and one inference key. Caller must clean up.
pub async fn provision_inference_key(pool: &sqlx::PgPool) -> IssuedInference {
    let service =
        ProvisioningService::new(Arc::new(PostgresAuthStore::new(pool.clone())));
    let tenant = format!("http-fix-{}", Uuid::new_v4().simple());
    let created = service.create_tenant(&tenant).await.expect("tenant");
    let inference = service
        .issue_key(
            created.key.client_id,
            ApiKeyKind::Inference,
            "fixture-inference",
        )
        .await
        .expect("inference key");
    IssuedInference {
        client_id: inference.client_id,
        key_id: inference.key_id,
        credential: inference.credential().to_string(),
    }
}

/// Deletes fixture clients and their keys.
pub async fn cleanup_clients(pool: &sqlx::PgPool, client_ids: &[Uuid]) {
    for client_id in client_ids {
        let _ = sqlx::query("DELETE FROM opmux_private.api_keys WHERE client_id = $1")
            .bind(client_id)
            .execute(pool)
            .await;
        let _ = sqlx::query("DELETE FROM opmux_private.clients WHERE id = $1")
            .bind(client_id)
            .execute(pool)
            .await;
    }
}

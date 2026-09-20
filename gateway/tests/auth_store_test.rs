//! Real owned-Supabase tests for the private auth schema and SQLx store.
//!
//! These tests fail when DATABASE_URL is missing or the owned database is
//! unreachable. They do not skip, contact hosted databases, or print secrets.

mod support;

use chrono::{Duration, TimeZone, Utc};
use gateway::core::db::DatabasePoolConfig;
use gateway::features::auth::{
    ApiKeyKind, AuthStore, AuthStoreError, KeyDigest, NewApiKey, NewClient,
    PostgresAuthStore, RevokeOutcome, AUTH_MIGRATION_VERSION, KEY_DIGEST_LEN,
    MAX_KEY_LIST_LIMIT,
};
use serial_test::serial;
use sqlx::Row;
use std::str::FromStr;
use std::sync::Arc;
use support::{required_database_url, test_pool};
use uuid::Uuid;

fn unique_digest() -> KeyDigest {
    let mut bytes = [0_u8; KEY_DIGEST_LEN];
    let id = Uuid::new_v4();
    bytes[..16].copy_from_slice(id.as_bytes());
    bytes[16..].copy_from_slice(Uuid::new_v4().as_bytes());
    KeyDigest::from_bytes(bytes)
}

fn display_id() -> String {
    format!("opk_{}", Uuid::new_v4().simple())
}

fn created_at() -> chrono::DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 9, 19, 12, 0, 0).unwrap()
}

fn new_client() -> NewClient {
    NewClient {
        id: Uuid::new_v4(),
        display_name: format!("tenant-{}", Uuid::new_v4().simple()),
        created_at: created_at(),
    }
}

fn new_key(client_id: Uuid, kind: ApiKeyKind) -> NewApiKey {
    NewApiKey {
        id: Uuid::new_v4(),
        client_id,
        digest: unique_digest(),
        display_id: display_id(),
        name: "fixture-key".to_string(),
        kind,
        created_at: created_at(),
    }
}

async fn cleanup(pool: &sqlx::PgPool, client_ids: &[Uuid]) {
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

async fn has_privilege(
    pool: &sqlx::PgPool,
    role: &str,
    table: &str,
    priv_type: &str,
) -> bool {
    sqlx::query_scalar("SELECT has_table_privilege($1, $2, $3)")
        .bind(role)
        .bind(table)
        .bind(priv_type)
        .fetch_one(pool)
        .await
        .expect("privilege inquiry")
}

#[tokio::test]
#[serial]
async fn supabase_migration_history_records_private_auth_schema() {
    let pool = test_pool().await;
    let version: Option<String> = sqlx::query_scalar(
        "SELECT version FROM supabase_migrations.schema_migrations WHERE version = $1",
    )
    .bind(AUTH_MIGRATION_VERSION)
    .fetch_optional(&pool)
    .await
    .expect("migration history must exist after scripts/db-migrate.sh");
    assert!(
        version.is_some(),
        "supabase migration history must record {AUTH_MIGRATION_VERSION}"
    );

    let sqlx_ledger: bool = sqlx::query_scalar(
        "SELECT to_regclass('public._sqlx_migrations') IS NOT NULL
         OR to_regclass('_sqlx_migrations') IS NOT NULL",
    )
    .fetch_one(&pool)
    .await
    .expect("sqlx ledger probe");
    assert!(
        !sqlx_ledger,
        "must not create a competing SQLx migration ledger"
    );
}

#[tokio::test]
#[serial]
async fn public_anon_and_service_roles_cannot_read_keys_or_digests() {
    let pool = test_pool().await;
    for role in ["anon", "authenticated", "authenticator", "service_role"] {
        for table in ["opmux_private.clients", "opmux_private.api_keys"] {
            assert!(
                !has_privilege(&pool, role, table, "SELECT").await,
                "{role} must not SELECT {table}"
            );
            assert!(
                !has_privilege(&pool, role, table, "INSERT").await,
                "{role} must not INSERT {table}"
            );
        }
    }

    let mut tx = pool.begin().await.expect("tx");
    sqlx::query("SET LOCAL ROLE anon")
        .execute(&mut *tx)
        .await
        .expect("set anon");
    let denied = sqlx::query("SELECT key_digest FROM opmux_private.api_keys")
        .fetch_optional(&mut *tx)
        .await
        .expect_err("anon SELECT must fail");
    assert_eq!(
        AuthStoreError::from_sqlx(denied),
        AuthStoreError::PermissionDenied
    );
    tx.rollback().await.expect("rollback anon tx");
}

#[tokio::test]
#[serial]
async fn runtime_and_operator_grants_are_narrow() {
    let pool = test_pool().await;
    assert!(
        has_privilege(&pool, "opmux_runtime", "opmux_private.clients", "SELECT").await
    );
    assert!(
        !has_privilege(&pool, "opmux_runtime", "opmux_private.clients", "INSERT").await
    );
    assert!(
        !has_privilege(&pool, "opmux_runtime", "opmux_private.clients", "DELETE").await
    );
    assert!(
        has_privilege(&pool, "opmux_runtime", "opmux_private.api_keys", "SELECT").await
    );
    assert!(
        has_privilege(&pool, "opmux_runtime", "opmux_private.api_keys", "INSERT").await
    );
    assert!(
        has_privilege(&pool, "opmux_runtime", "opmux_private.api_keys", "UPDATE").await
    );
    assert!(
        !has_privilege(&pool, "opmux_runtime", "opmux_private.api_keys", "DELETE").await
    );

    assert!(
        has_privilege(&pool, "opmux_operator", "opmux_private.clients", "INSERT").await
    );
    assert!(
        has_privilege(&pool, "opmux_operator", "opmux_private.api_keys", "INSERT").await
    );
    assert!(
        !has_privilege(&pool, "opmux_operator", "opmux_private.clients", "DELETE").await
    );

    let mut tx = pool.begin().await.expect("tx");
    sqlx::query("SET LOCAL ROLE opmux_runtime")
        .execute(&mut *tx)
        .await
        .expect("set runtime");
    let denied = sqlx::query(
        "INSERT INTO opmux_private.clients (id, display_name, created_at)
         VALUES ($1, $2, $3)",
    )
    .bind(Uuid::new_v4())
    .bind("runtime-should-fail")
    .bind(created_at())
    .execute(&mut *tx)
    .await
    .expect_err("runtime must not insert clients");
    assert_eq!(
        AuthStoreError::from_sqlx(denied),
        AuthStoreError::PermissionDenied
    );
    tx.rollback().await.expect("rollback runtime tx");
}

#[tokio::test]
#[serial]
async fn digest_length_and_kind_constraints_reject_invalid_rows() {
    let pool = test_pool().await;
    let client = new_client();
    sqlx::query(
        "INSERT INTO opmux_private.clients (id, display_name, created_at)
         VALUES ($1, $2, $3)",
    )
    .bind(client.id)
    .bind(&client.display_name)
    .bind(client.created_at)
    .execute(&pool)
    .await
    .expect("client fixture");

    let short = sqlx::query(
        "INSERT INTO opmux_private.api_keys (
             id, client_id, key_digest, display_id, name, kind, created_at
         ) VALUES ($1, $2, $3, $4, $5, $6, $7)",
    )
    .bind(Uuid::new_v4())
    .bind(client.id)
    .bind(vec![1_u8; 16])
    .bind(display_id())
    .bind("bad-digest")
    .bind("management")
    .bind(created_at())
    .execute(&pool)
    .await
    .expect_err("short digest");
    assert_eq!(
        AuthStoreError::from_sqlx(short),
        AuthStoreError::CheckViolation
    );

    let kind = sqlx::query(
        "INSERT INTO opmux_private.api_keys (
             id, client_id, key_digest, display_id, name, kind, created_at
         ) VALUES ($1, $2, $3, $4, $5, $6, $7)",
    )
    .bind(Uuid::new_v4())
    .bind(client.id)
    .bind(unique_digest().as_bytes().as_slice())
    .bind(display_id())
    .bind("bad-kind")
    .bind("admin")
    .bind(created_at())
    .execute(&pool)
    .await
    .expect_err("unknown kind");
    assert_eq!(
        AuthStoreError::from_sqlx(kind),
        AuthStoreError::CheckViolation
    );

    cleanup(&pool, &[client.id]).await;
    pool.close().await;
}

#[tokio::test]
#[serial]
async fn foreign_key_and_unique_digest_constraints_hold() {
    let pool = test_pool().await;
    let store = PostgresAuthStore::new(pool.clone());
    let missing_client = Uuid::new_v4();
    let err = store
        .insert_key(new_key(missing_client, ApiKeyKind::Inference))
        .await
        .expect_err("missing client");
    assert_eq!(err, AuthStoreError::UnknownClient);

    let client = new_client();
    let key = new_key(client.id, ApiKeyKind::Management);
    store
        .provision_client_with_key(client.clone(), key.clone())
        .await
        .expect("provision");
    let duplicate = NewApiKey {
        id: Uuid::new_v4(),
        client_id: client.id,
        digest: key.digest,
        display_id: display_id(),
        name: "dup-digest".to_string(),
        kind: ApiKeyKind::Inference,
        created_at: created_at(),
    };
    let err = store
        .insert_key(duplicate)
        .await
        .expect_err("duplicate digest");
    assert_eq!(err, AuthStoreError::DuplicateDigest);
    cleanup(&pool, &[client.id]).await;
}

#[tokio::test]
#[serial]
async fn identity_columns_cannot_be_mutated() {
    let pool = test_pool().await;
    let store = PostgresAuthStore::new(pool.clone());
    let client = new_client();
    let key = new_key(client.id, ApiKeyKind::Inference);
    let (_, stored) = store
        .provision_client_with_key(client.clone(), key)
        .await
        .expect("provision");
    let kind = sqlx::query("UPDATE opmux_private.api_keys SET kind = $2 WHERE id = $1")
        .bind(stored.id)
        .bind("management")
        .execute(&pool)
        .await
        .expect_err("kind mutation");
    assert_eq!(
        AuthStoreError::from_sqlx(kind),
        AuthStoreError::CheckViolation
    );
    cleanup(&pool, &[client.id]).await;
}

#[tokio::test]
#[serial]
async fn provision_rolls_back_client_when_key_insert_fails() {
    let pool = test_pool().await;
    let store = PostgresAuthStore::new(pool.clone());
    let existing = new_client();
    let existing_key = new_key(existing.id, ApiKeyKind::Management);
    store
        .provision_client_with_key(existing.clone(), existing_key.clone())
        .await
        .expect("control tenant");

    let orphan = new_client();
    let colliding = NewApiKey {
        id: Uuid::new_v4(),
        client_id: orphan.id,
        digest: existing_key.digest,
        display_id: display_id(),
        name: "should-rollback".to_string(),
        kind: ApiKeyKind::Management,
        created_at: created_at(),
    };
    let err = store
        .provision_client_with_key(orphan.clone(), colliding)
        .await
        .expect_err("duplicate digest in provision");
    assert_eq!(err, AuthStoreError::DuplicateDigest);

    let leftover: Option<Uuid> =
        sqlx::query_scalar("SELECT id FROM opmux_private.clients WHERE id = $1")
            .bind(orphan.id)
            .fetch_optional(&pool)
            .await
            .expect("orphan probe");
    assert!(
        leftover.is_none(),
        "failed provision must not leave a client"
    );
    cleanup(&pool, &[existing.id, orphan.id]).await;
}

#[tokio::test]
#[serial]
async fn lookup_touch_list_and_revoke_are_tenant_scoped() {
    let pool = test_pool().await;
    let store = PostgresAuthStore::new(pool.clone());
    let tenant_a = new_client();
    let tenant_b = new_client();
    let key_a = new_key(tenant_a.id, ApiKeyKind::Management);
    let key_a_infer = new_key(tenant_a.id, ApiKeyKind::Inference);
    let key_b = new_key(tenant_b.id, ApiKeyKind::Management);
    store
        .provision_client_with_key(tenant_a.clone(), key_a.clone())
        .await
        .expect("tenant a");
    store
        .insert_key(key_a_infer)
        .await
        .expect("tenant a inference");
    store
        .provision_client_with_key(tenant_b.clone(), key_b.clone())
        .await
        .expect("tenant b");

    let found = store
        .find_key_by_digest(&key_a.digest)
        .await
        .expect("lookup")
        .expect("present");
    assert_eq!(found.client_id, tenant_a.id);
    assert_eq!(found.id, key_a.id);
    assert_eq!(found.kind, ApiKeyKind::Management);
    assert!(!found.is_revoked());
    assert!(found.last_used_at.is_none());

    let missing = store
        .find_key_by_digest(&unique_digest())
        .await
        .expect("unknown digest is Ok(None), not an invalid-key signal");
    assert!(missing.is_none());

    let later = created_at() + Duration::seconds(30);
    let earlier = created_at() + Duration::seconds(10);
    assert!(store
        .touch_last_used(tenant_a.id, key_a.id, later)
        .await
        .expect("touch"));
    assert!(!store
        .touch_last_used(tenant_a.id, key_a.id, earlier)
        .await
        .expect("older touch must not win"));
    assert!(!store
        .touch_last_used(tenant_b.id, key_a.id, later + Duration::seconds(5))
        .await
        .expect("cross-tenant touch"));

    let listed_a = store
        .list_keys_for_client(tenant_a.id, MAX_KEY_LIST_LIMIT, 0, None)
        .await
        .expect("list a");
    assert_eq!(listed_a.len(), 2);
    assert!(listed_a.iter().all(|key| key.client_id == tenant_a.id));
    assert!(!listed_a.iter().any(|key| key.id == key_b.id));
    let listed_b = store
        .list_keys_for_client(tenant_b.id, 10, 0, None)
        .await
        .expect("list b");
    assert_eq!(listed_b.len(), 1);
    assert_eq!(listed_b[0].id, key_b.id);

    match store
        .revoke_key(tenant_b.id, key_a.id, later)
        .await
        .expect("cross-tenant revoke")
    {
        RevokeOutcome::NotFound => {}
        other => panic!("expected NotFound, got {other:?}"),
    }
    match store
        .revoke_key(tenant_a.id, key_a.id, later)
        .await
        .expect("revoke")
    {
        RevokeOutcome::Revoked(record) => {
            assert_eq!(record.revoked_at, Some(later));
        }
        other => panic!("expected Revoked, got {other:?}"),
    }
    match store
        .revoke_key(tenant_a.id, key_a.id, later + Duration::seconds(20))
        .await
        .expect("idempotent revoke")
    {
        RevokeOutcome::AlreadyRevoked(record) => {
            assert_eq!(record.revoked_at, Some(later));
        }
        other => panic!("expected AlreadyRevoked, got {other:?}"),
    }
    assert!(!store
        .touch_last_used(tenant_a.id, key_a.id, later + Duration::seconds(60))
        .await
        .expect("revoked keys are not touched"));

    let stored = sqlx::query(
        "SELECT octet_length(key_digest) AS digest_len, key_digest
         FROM opmux_private.api_keys WHERE id = $1",
    )
    .bind(key_a.id)
    .fetch_one(&pool)
    .await
    .expect("digest facts");
    let digest_len: i32 = stored.get("digest_len");
    let digest: Vec<u8> = stored.get("key_digest");
    assert_eq!(digest_len, KEY_DIGEST_LEN as i32);
    assert_eq!(digest.as_slice(), key_a.digest.as_bytes().as_slice());
    assert_eq!(digest.len(), 32);

    cleanup(&pool, &[tenant_a.id, tenant_b.id]).await;
}

async fn last_used_at(
    pool: &sqlx::PgPool,
    key_id: Uuid,
) -> Option<chrono::DateTime<Utc>> {
    sqlx::query_scalar("SELECT last_used_at FROM opmux_private.api_keys WHERE id = $1")
        .bind(key_id)
        .fetch_one(pool)
        .await
        .expect("last_used_at")
}

async fn concurrent_pool() -> sqlx::PgPool {
    let config = DatabasePoolConfig::new(required_database_url())
        .expect("DATABASE_URL must parse")
        .with_max_connections(4)
        .expect("pool size")
        .with_acquire_timeout(std::time::Duration::from_secs(10))
        .expect("acquire timeout");
    config.connect().await.unwrap_or_else(|_| {
        panic!(
            "failed to connect to DATABASE_URL; persisted authentication tests require the owned local Supabase and do not skip"
        )
    })
}

#[tokio::test]
#[serial]
async fn authenticate_digest_is_monotonic_and_skips_revoked_keys() {
    let pool = test_pool().await;
    let store = PostgresAuthStore::new(pool.clone());
    let tenant = new_client();
    let key = new_key(tenant.id, ApiKeyKind::Inference);
    store
        .provision_client_with_key(tenant.clone(), key.clone())
        .await
        .expect("tenant");

    let unused = store
        .authenticate_digest(&unique_digest(), created_at() + Duration::seconds(1))
        .await
        .expect("unknown digest");
    assert!(unused.is_none());
    assert!(last_used_at(&pool, key.id).await.is_none());

    let first = created_at() + Duration::seconds(20);
    let second = created_at() + Duration::seconds(40);
    let older = created_at() + Duration::seconds(10);
    let active = store
        .authenticate_digest(&key.digest, first)
        .await
        .expect("first auth")
        .expect("present");
    assert!(!active.is_revoked());
    assert_eq!(active.last_used_at, Some(first));
    assert_eq!(last_used_at(&pool, key.id).await, Some(first));

    let again = store
        .authenticate_digest(&key.digest, second)
        .await
        .expect("newer auth")
        .expect("present");
    assert_eq!(again.last_used_at, Some(second));
    let stale = store
        .authenticate_digest(&key.digest, older)
        .await
        .expect("older auth")
        .expect("present");
    assert!(!stale.is_revoked());
    assert_eq!(stale.last_used_at, Some(second));
    assert_eq!(last_used_at(&pool, key.id).await, Some(second));

    match store
        .revoke_key(tenant.id, key.id, created_at() + Duration::seconds(50))
        .await
        .expect("revoke")
    {
        RevokeOutcome::Revoked(_) => {}
        other => panic!("expected Revoked, got {other:?}"),
    }
    let revoked = store
        .authenticate_digest(&key.digest, created_at() + Duration::seconds(80))
        .await
        .expect("revoked auth")
        .expect("row retained");
    assert!(revoked.is_revoked());
    assert_eq!(revoked.last_used_at, Some(second));
    assert_eq!(last_used_at(&pool, key.id).await, Some(second));

    cleanup(&pool, &[tenant.id]).await;
}

#[tokio::test]
#[serial]
async fn overlapping_touches_never_move_last_used_backward() {
    let pool = concurrent_pool().await;
    let store = Arc::new(PostgresAuthStore::new(pool.clone()));
    let tenant = new_client();
    let key = new_key(tenant.id, ApiKeyKind::Inference);
    store
        .provision_client_with_key(tenant.clone(), key.clone())
        .await
        .expect("tenant");

    let older = created_at() + Duration::seconds(15);
    let newer = created_at() + Duration::seconds(45);
    let (older_result, newer_result) = tokio::join!(
        store.touch_last_used(tenant.id, key.id, older),
        store.touch_last_used(tenant.id, key.id, newer),
    );
    let older_wrote = older_result.expect("older touch");
    let newer_wrote = newer_result.expect("newer touch");
    assert!(newer_wrote || !older_wrote);
    assert_eq!(last_used_at(&pool, key.id).await, Some(newer));

    let (older_auth, newer_auth) = tokio::join!(
        store.authenticate_digest(&key.digest, older),
        store.authenticate_digest(&key.digest, newer + Duration::seconds(10)),
    );
    let older_record = older_auth.expect("older digest auth").expect("present");
    let newer_record = newer_auth.expect("newer digest auth").expect("present");
    assert!(!older_record.is_revoked());
    assert!(!newer_record.is_revoked());
    let stored = last_used_at(&pool, key.id).await.expect("last used");
    assert!(stored >= newer);
    assert_eq!(stored, newer + Duration::seconds(10));

    cleanup(&pool, &[tenant.id]).await;
}

#[tokio::test]
#[serial]
async fn authenticate_digest_serializes_with_revoke() {
    let pool = concurrent_pool().await;
    let store = Arc::new(PostgresAuthStore::new(pool.clone()));
    let tenant = new_client();
    let key = new_key(tenant.id, ApiKeyKind::Inference);
    store
        .provision_client_with_key(tenant.clone(), key.clone())
        .await
        .expect("tenant");

    let used_at = created_at() + Duration::seconds(25);
    let revoked_at = created_at() + Duration::seconds(30);
    let (auth_result, revoke_result) = tokio::join!(
        store.authenticate_digest(&key.digest, used_at),
        store.revoke_key(tenant.id, key.id, revoked_at),
    );
    let authenticated = auth_result.expect("digest auth");
    let outcome = revoke_result.expect("revoke");
    match outcome {
        RevokeOutcome::Revoked(_) | RevokeOutcome::AlreadyRevoked(_) => {}
        RevokeOutcome::NotFound => panic!("same-tenant revoke must find the key"),
    }

    let stored = store
        .find_key_by_digest(&key.digest)
        .await
        .expect("lookup")
        .expect("retained");
    assert!(stored.is_revoked());
    if let Some(record) = authenticated {
        if record.is_revoked() {
            assert!(record.last_used_at.is_none());
            assert_eq!(stored.last_used_at, None);
        } else {
            assert_eq!(record.last_used_at, Some(used_at));
            assert_eq!(stored.last_used_at, Some(used_at));
        }
    } else {
        panic!("digest lookup must return the committed row");
    }

    let later = store
        .authenticate_digest(&key.digest, created_at() + Duration::seconds(90))
        .await
        .expect("post-revoke auth")
        .expect("row retained");
    assert!(later.is_revoked());
    assert_eq!(later.last_used_at, stored.last_used_at);

    cleanup(&pool, &[tenant.id]).await;
}

#[tokio::test]
#[serial]
async fn closed_pool_propagates_unavailable_instead_of_none() {
    let config = DatabasePoolConfig::new(required_database_url())
        .unwrap()
        .with_max_connections(1)
        .unwrap();
    let pool = config.connect().await.expect("connect");
    let store = PostgresAuthStore::new(pool.clone());
    pool.close().await;
    let err = store
        .find_key_by_digest(&unique_digest())
        .await
        .expect_err("closed pool");
    assert_eq!(err, AuthStoreError::Unavailable);
}

#[tokio::test]
#[serial]
async fn list_returns_at_most_max_limit_newest_first_for_one_tenant() {
    let pool = test_pool().await;
    let store = PostgresAuthStore::new(pool.clone());
    let tenant_a = new_client();
    let tenant_b = new_client();
    let key_a = new_key(tenant_a.id, ApiKeyKind::Management);
    let key_b = new_key(tenant_b.id, ApiKeyKind::Management);
    store
        .provision_client_with_key(tenant_a.clone(), key_a.clone())
        .await
        .expect("tenant a");
    store
        .provision_client_with_key(tenant_b.clone(), key_b.clone())
        .await
        .expect("tenant b");

    let extra = (MAX_KEY_LIST_LIMIT + 1) as usize;
    let base = created_at() + Duration::seconds(1);
    let mut extra_ids = Vec::with_capacity(extra);
    for index in 0..extra {
        let mut key = new_key(tenant_a.id, ApiKeyKind::Inference);
        key.name = format!("bound-{index}");
        key.created_at = base + Duration::milliseconds(index as i64);
        extra_ids.push(key.id);
        store.insert_key(key).await.expect("extra key");
    }

    let listed = store
        .list_keys_for_client(tenant_a.id, MAX_KEY_LIST_LIMIT, 0, None)
        .await
        .expect("bound list");
    assert_eq!(listed.len(), MAX_KEY_LIST_LIMIT as usize);
    assert!(listed.iter().all(|key| key.client_id == tenant_a.id));
    assert!(!listed.iter().any(|key| key.id == key_b.id));
    assert!(!listed.iter().any(|key| key.id == extra_ids[0]));
    assert_eq!(listed[0].id, extra_ids[extra - 1]);
    assert!(listed.windows(2).all(|pair| {
        pair[0].created_at > pair[1].created_at
            || (pair[0].created_at == pair[1].created_at && pair[0].id > pair[1].id)
    }));

    cleanup(&pool, &[tenant_a.id, tenant_b.id]).await;
}

#[tokio::test]
#[serial]
async fn list_applies_offset_and_kind_without_crossing_tenants() {
    let pool = test_pool().await;
    let store = PostgresAuthStore::new(pool.clone());
    let tenant_a = new_client();
    let tenant_b = new_client();
    let mut key_a_mgmt = new_key(tenant_a.id, ApiKeyKind::Management);
    let mut key_a_infer = new_key(tenant_a.id, ApiKeyKind::Inference);
    let key_b = new_key(tenant_b.id, ApiKeyKind::Inference);
    key_a_mgmt.created_at = created_at();
    key_a_infer.created_at = created_at() + Duration::seconds(1);
    store
        .provision_client_with_key(tenant_a.clone(), key_a_mgmt.clone())
        .await
        .expect("tenant a");
    store
        .insert_key(key_a_infer.clone())
        .await
        .expect("a inference");
    store
        .provision_client_with_key(tenant_b.clone(), key_b.clone())
        .await
        .expect("tenant b");

    let newest = store
        .list_keys_for_client(tenant_a.id, 1, 0, None)
        .await
        .expect("newest");
    assert_eq!(newest.len(), 1);
    assert_eq!(newest[0].id, key_a_infer.id);

    let older = store
        .list_keys_for_client(tenant_a.id, 1, 1, None)
        .await
        .expect("offset");
    assert_eq!(older.len(), 1);
    assert_eq!(older[0].id, key_a_mgmt.id);

    let inference_only = store
        .list_keys_for_client(
            tenant_a.id,
            MAX_KEY_LIST_LIMIT,
            0,
            Some(ApiKeyKind::Inference),
        )
        .await
        .expect("kind");
    assert_eq!(inference_only.len(), 1);
    assert_eq!(inference_only[0].id, key_a_infer.id);
    assert!(!inference_only.iter().any(|key| key.id == key_b.id));

    let empty = store
        .list_keys_for_client(tenant_a.id, MAX_KEY_LIST_LIMIT, 50, None)
        .await
        .expect("past end");
    assert!(empty.is_empty());

    cleanup(&pool, &[tenant_a.id, tenant_b.id]).await;
}

#[tokio::test]
#[serial]
async fn store_trait_object_is_send_sync_and_list_limit_is_enforced() {
    let pool = test_pool().await;
    let store: Arc<dyn AuthStore> = Arc::new(PostgresAuthStore::new(pool.clone()));
    fn assert_send_sync<T: Send + Sync>(_: &T) {}
    assert_send_sync(&store);
    let err = store
        .list_keys_for_client(Uuid::new_v4(), 0, 0, None)
        .await
        .expect_err("zero limit");
    assert_eq!(err, AuthStoreError::InvalidLimit);
    let too_large = store
        .list_keys_for_client(Uuid::new_v4(), MAX_KEY_LIST_LIMIT + 1, 0, None)
        .await
        .expect_err("above max");
    assert_eq!(too_large, AuthStoreError::InvalidLimit);
    let negative_offset = store
        .list_keys_for_client(Uuid::new_v4(), 1, -1, None)
        .await
        .expect_err("negative offset");
    assert_eq!(negative_offset, AuthStoreError::InvalidLimit);
}

#[test]
fn record_debug_does_not_include_digest_bytes() {
    let digest = KeyDigest::from_bytes([9; 32]);
    let key = NewApiKey {
        id: Uuid::nil(),
        client_id: Uuid::nil(),
        digest,
        display_id: "opk_fixture".to_string(),
        name: "n".to_string(),
        kind: ApiKeyKind::Inference,
        created_at: created_at(),
    };
    let rendered = format!("{key:?}");
    assert!(rendered.contains("KeyDigest([redacted])"));
    assert!(!rendered.contains("9, 9, 9"));
}

#[test]
fn crate_sources_do_not_embed_a_sqlx_migrator() {
    let postgres = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/src/features/auth/persist/postgres.rs"
    ));
    let db = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/src/core/db.rs"));
    assert!(!postgres.contains("sqlx::migrate!"));
    assert!(!db.contains("sqlx::migrate!"));
    assert!(!postgres.contains("_sqlx_migrations"));
}

#[tokio::test]
#[serial]
async fn authentication_probe_requires_schema_and_table_access() {
    let pool = test_pool().await;
    let store = PostgresAuthStore::new(pool.clone());
    store
        .probe_authentication_access()
        .await
        .expect("owned authentication schema must be readable");

    let mut tx = pool.begin().await.expect("tx");
    sqlx::query("SET LOCAL ROLE anon")
        .execute(&mut *tx)
        .await
        .expect("set anon");
    sqlx::query("SELECT 1 FROM opmux_private.api_keys LIMIT 0")
        .execute(&mut *tx)
        .await
        .expect_err("anon cannot read api_keys");
    tx.rollback().await.expect("rollback");

    let url = required_database_url();
    let options = sqlx::postgres::PgConnectOptions::from_str(&url)
        .expect("owned url")
        .database("template1");
    let template_pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(1)
        .connect_with(options)
        .await
        .expect("template1 must exist on the owned cluster");
    let missing = PostgresAuthStore::new(template_pool);
    missing
        .probe_authentication_access()
        .await
        .expect_err("missing product schema cannot report ready");
}

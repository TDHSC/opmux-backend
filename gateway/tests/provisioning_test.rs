//! Real owned-Supabase tests for shared key provisioning.
//!
//! Fail when DATABASE_URL is missing. Do not print credentials or digests.

use gateway::core::db::DatabasePoolConfig;
use gateway::features::auth::{
    hash_credential, parse_credential_payload, ApiKeyKind, AuthStore, EntropyError,
    PostgresAuthStore, ProvisionError, ProvisioningService, SecretSource,
    CREDENTIAL_PREFIX, INITIAL_MANAGEMENT_KEY_NAME, KEY_DIGEST_LEN, SECRET_PAYLOAD_LEN,
};
use serial_test::serial;
use sqlx::Row;
use std::collections::{HashSet, VecDeque};
use std::sync::{Arc, Mutex};
use uuid::Uuid;

fn required_database_url() -> String {
    match std::env::var("DATABASE_URL") {
        Ok(url) if !url.trim().is_empty() => url,
        _ => panic!(
            "DATABASE_URL is required for persistence tests and must point at the owned local Supabase on 127.0.0.1:55432. Tests do not skip when the database is unavailable."
        ),
    }
}

async fn test_pool() -> sqlx::PgPool {
    let config = DatabasePoolConfig::new(required_database_url())
        .expect("DATABASE_URL must parse")
        .with_max_connections(2)
        .expect("test pool size")
        .with_acquire_timeout(std::time::Duration::from_secs(10))
        .expect("test acquire timeout");
    let pool = config.connect().await.unwrap_or_else(|_| {
        panic!(
            "failed to connect to DATABASE_URL; persistence tests require the owned local Supabase and do not skip"
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

struct ScriptedEntropy {
    chunks: Mutex<VecDeque<Vec<u8>>>,
}

impl ScriptedEntropy {
    fn new(chunks: Vec<Vec<u8>>) -> Self {
        Self {
            chunks: Mutex::new(VecDeque::from(chunks)),
        }
    }
}

fn unique_bytes<const N: usize>(tag: u8) -> [u8; N] {
    let mut bytes = [tag; N];
    let id = Uuid::new_v4();
    let copy = N.min(16);
    bytes[..copy].copy_from_slice(&id.as_bytes()[..copy]);
    bytes
}

impl SecretSource for ScriptedEntropy {
    fn fill_bytes(&self, dest: &mut [u8]) -> Result<(), EntropyError> {
        let next = self
            .chunks
            .lock()
            .expect("lock")
            .pop_front()
            .ok_or(EntropyError::Unavailable)?;
        if next.len() != dest.len() {
            return Err(EntropyError::Unavailable);
        }
        dest.copy_from_slice(&next);
        Ok(())
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

async fn key_count(pool: &sqlx::PgPool, client_id: Uuid) -> i64 {
    sqlx::query_scalar("SELECT COUNT(*) FROM opmux_private.api_keys WHERE client_id = $1")
        .bind(client_id)
        .fetch_one(pool)
        .await
        .expect("key count")
}

async fn client_exists(pool: &sqlx::PgPool, client_id: Uuid) -> bool {
    sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM opmux_private.clients WHERE id = $1)")
        .bind(client_id)
        .fetch_one(pool)
        .await
        .expect("client exists")
}

async fn digest_facts_match(
    pool: &sqlx::PgPool,
    key_id: Uuid,
    credential: &str,
) -> (bool, bool, bool) {
    let row = sqlx::query(
        "SELECT octet_length(key_digest) AS digest_len, key_digest, display_id, name
         FROM opmux_private.api_keys WHERE id = $1",
    )
    .bind(key_id)
    .fetch_one(pool)
    .await
    .expect("digest facts");
    let digest_len: i32 = row.get("digest_len");
    let digest: Vec<u8> = row.get("key_digest");
    let display_id: String = row.get("display_id");
    let name: String = row.get("name");
    let expected = hash_credential(credential);
    let length_ok = digest_len == KEY_DIGEST_LEN as i32 && digest.len() == 32;
    let digest_ok = digest.as_slice() == expected.as_bytes().as_slice();
    let secret_not_stored = display_id != credential
        && name != credential
        && !display_id.contains(credential)
        && !name.contains(credential);
    (length_ok, digest_ok, secret_not_stored)
}

#[tokio::test]
#[serial]
async fn provisions_two_tenants_and_resolves_six_identities() {
    let pool = test_pool().await;
    let service =
        ProvisioningService::new(Arc::new(PostgresAuthStore::new(pool.clone())));
    let tenant_a = format!("tenant-a-{}", Uuid::new_v4().simple());
    let tenant_b = format!("tenant-b-{}", Uuid::new_v4().simple());

    let created_a = service.create_tenant(&tenant_a).await.expect("tenant a");
    let created_b = service.create_tenant(&tenant_b).await.expect("tenant b");
    let a_mgr2 = service
        .issue_key(
            created_a.key.client_id,
            ApiKeyKind::Management,
            "a-manager-2",
        )
        .await
        .expect("a manager 2");
    let a_inf = service
        .issue_key(
            created_a.key.client_id,
            ApiKeyKind::Inference,
            "a-inference",
        )
        .await
        .expect("a inference");
    let b_mgr2 = service
        .issue_key(
            created_b.key.client_id,
            ApiKeyKind::Management,
            "b-manager-2",
        )
        .await
        .expect("b manager 2");
    let b_inf = service
        .issue_key(
            created_b.key.client_id,
            ApiKeyKind::Inference,
            "b-inference",
        )
        .await
        .expect("b inference");

    let issued = [
        (
            "a0",
            created_a.key.credential(),
            created_a.key.client_id,
            created_a.key.key_id,
            ApiKeyKind::Management,
        ),
        (
            "b0",
            created_b.key.credential(),
            created_b.key.client_id,
            created_b.key.key_id,
            ApiKeyKind::Management,
        ),
        (
            "a1",
            a_mgr2.credential(),
            a_mgr2.client_id,
            a_mgr2.key_id,
            ApiKeyKind::Management,
        ),
        (
            "a2",
            a_inf.credential(),
            a_inf.client_id,
            a_inf.key_id,
            ApiKeyKind::Inference,
        ),
        (
            "b1",
            b_mgr2.credential(),
            b_mgr2.client_id,
            b_mgr2.key_id,
            ApiKeyKind::Management,
        ),
        (
            "b2",
            b_inf.credential(),
            b_inf.client_id,
            b_inf.key_id,
            ApiKeyKind::Inference,
        ),
    ];

    let mut secrets = HashSet::new();
    for (_label, secret, client_id, key_id, kind) in issued {
        let unique = secrets.insert(secret.to_string());
        assert!(unique, "issued credentials must be distinct");
        assert!(
            secret.starts_with(CREDENTIAL_PREFIX),
            "credential must use the versioned prefix"
        );
        let payload = parse_credential_payload(secret);
        assert!(
            payload.is_some_and(|bytes| bytes.len() == SECRET_PAYLOAD_LEN),
            "credential payload must be 32 bytes"
        );
        let identity = service
            .resolve_credential(secret)
            .await
            .expect("resolve")
            .expect("identity");
        assert_eq!(identity.client_id, client_id);
        assert_eq!(identity.key_id, key_id);
        assert_eq!(identity.kind, kind);
        assert!(!identity.revoked);
        let (length_ok, digest_ok, secret_not_stored) =
            digest_facts_match(&pool, key_id, secret).await;
        assert!(length_ok, "digest must be 32 bytes");
        assert!(digest_ok, "stored digest must equal sha256 of the secret");
        assert!(secret_not_stored, "plaintext secret must not be stored");
    }
    assert_eq!(secrets.len(), 6);

    assert_eq!(created_a.key.name, INITIAL_MANAGEMENT_KEY_NAME);
    assert_eq!(created_a.key.kind, "management");
    assert_ne!(created_a.key.client_id, created_b.key.client_id);
    assert_eq!(key_count(&pool, created_a.key.client_id).await, 3);
    assert_eq!(key_count(&pool, created_b.key.client_id).await, 3);

    let mock_hashes = [
        hash_credential("test-api-key-123"),
        hash_credential("dev-api-key-456"),
    ];
    for digest in mock_hashes {
        let found = PostgresAuthStore::new(pool.clone())
            .find_key_by_digest(&digest)
            .await
            .expect("mock lookup");
        assert!(
            found.is_none(),
            "public mock credentials must not be seeded"
        );
    }

    cleanup(&pool, &[created_a.key.client_id, created_b.key.client_id]).await;
}

#[tokio::test]
#[serial]
async fn reconnect_retains_provisioned_identity() {
    let pool = test_pool().await;
    let service =
        ProvisioningService::new(Arc::new(PostgresAuthStore::new(pool.clone())));
    let created = service
        .create_tenant(&format!("retain-{}", Uuid::new_v4().simple()))
        .await
        .expect("create");
    let client_id = created.key.client_id;
    let key_id = created.key.key_id;
    let secret = created.key.credential().to_string();
    pool.close().await;

    let pool = test_pool().await;
    let service =
        ProvisioningService::new(Arc::new(PostgresAuthStore::new(pool.clone())));
    let identity = service
        .resolve_credential(&secret)
        .await
        .expect("resolve after reconnect")
        .expect("retained");
    assert_eq!(identity.client_id, client_id);
    assert_eq!(identity.key_id, key_id);
    assert_eq!(identity.kind, ApiKeyKind::Management);
    cleanup(&pool, &[client_id]).await;
}

#[tokio::test]
#[serial]
async fn failed_tenant_create_rolls_back_partial_client() {
    let pool = test_pool().await;
    let payload = unique_bytes::<32>(9);
    let display_a = unique_bytes::<16>(1);
    let display_b = unique_bytes::<16>(2);
    let entropy = Arc::new(ScriptedEntropy::new(vec![
        payload.to_vec(),
        display_a.to_vec(),
        payload.to_vec(),
        display_b.to_vec(),
    ]));
    let service = ProvisioningService::with_entropy(
        Arc::new(PostgresAuthStore::new(pool.clone())),
        entropy,
    );

    let created = service
        .create_tenant(&format!("control-{}", Uuid::new_v4().simple()))
        .await
        .expect("control tenant");
    let control_id = created.key.client_id;
    let control_secret = created.key.credential().to_string();
    let clients_before: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM opmux_private.clients")
            .fetch_one(&pool)
            .await
            .expect("count");

    let err = service
        .create_tenant(&format!("orphan-{}", Uuid::new_v4().simple()))
        .await
        .expect_err("duplicate digest must fail");
    assert_eq!(err, ProvisionError::DuplicateDigest);

    let clients_after: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM opmux_private.clients")
            .fetch_one(&pool)
            .await
            .expect("count after");
    assert_eq!(clients_before, clients_after);
    assert!(client_exists(&pool, control_id).await);
    let identity = service
        .resolve_credential(&control_secret)
        .await
        .expect("control still resolves")
        .expect("control identity");
    assert_eq!(identity.client_id, control_id);
    cleanup(&pool, &[control_id]).await;
}

#[tokio::test]
#[serial]
async fn failed_existing_client_issue_preserves_prior_keys() {
    let pool = test_pool().await;
    let payload = unique_bytes::<32>(11);
    let display_a = unique_bytes::<16>(3);
    let display_b = unique_bytes::<16>(4);
    let entropy = Arc::new(ScriptedEntropy::new(vec![
        payload.to_vec(),
        display_a.to_vec(),
        payload.to_vec(),
        display_b.to_vec(),
        vec![5_u8; 32],
        vec![6_u8; 16],
    ]));
    let service = ProvisioningService::with_entropy(
        Arc::new(PostgresAuthStore::new(pool.clone())),
        entropy,
    );
    let created = service
        .create_tenant(&format!("keep-{}", Uuid::new_v4().simple()))
        .await
        .expect("tenant");
    let client_id = created.key.client_id;
    let original_key = created.key.key_id;
    let original_secret = created.key.credential().to_string();
    let before = key_count(&pool, client_id).await;

    let err = service
        .issue_key(client_id, ApiKeyKind::Inference, "should-not-land")
        .await
        .expect_err("duplicate digest");
    assert_eq!(err, ProvisionError::DuplicateDigest);
    assert_eq!(key_count(&pool, client_id).await, before);

    let missing = Uuid::new_v4();
    let err = service
        .issue_key(missing, ApiKeyKind::Management, "ghost")
        .await
        .expect_err("unknown client");
    assert_eq!(err, ProvisionError::UnknownClient);
    assert!(!client_exists(&pool, missing).await);

    let identity = service
        .resolve_credential(&original_secret)
        .await
        .expect("original still works")
        .expect("identity");
    assert_eq!(identity.key_id, original_key);
    assert_eq!(identity.kind, ApiKeyKind::Management);
    cleanup(&pool, &[client_id, missing]).await;
}

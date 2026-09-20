//! Actual `opmux-admin` subprocess tests against owned local Supabase.
//!
//! Capture issued credentials privately. Do not print secrets, digests, or
//! raw CLI stdout from successful issuance.

use gateway::core::db::DatabasePoolConfig;
use gateway::features::auth::{
    hash_credential, parse_credential_payload, ApiKeyKind, PostgresAuthStore,
    ProvisioningService, CREDENTIAL_PREFIX, INITIAL_MANAGEMENT_KEY_NAME,
    SECRET_PAYLOAD_LEN,
};
use serial_test::serial;
use sqlx::Row;
use std::collections::HashSet;
use std::path::PathBuf;
use std::process::{Command, Output};
use std::sync::Arc;
use uuid::Uuid;

fn required_database_url() -> String {
    match std::env::var("DATABASE_URL") {
        Ok(url) if !url.trim().is_empty() => url,
        _ => panic!(
            "DATABASE_URL is required for CLI persistence tests and must point at the owned local Supabase on 127.0.0.1:55432. Tests do not skip when the database is unavailable."
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
    config.connect().await.unwrap_or_else(|_| {
        panic!(
            "failed to connect to DATABASE_URL; persistence tests require the owned local Supabase and do not skip"
        )
    })
}

fn admin_bin() -> PathBuf {
    std::path::Path::new(env!("CARGO_BIN_EXE_gateway")).with_file_name("opmux-admin")
}

fn admin_command() -> Command {
    let mut command = Command::new(admin_bin());
    command
        .env("DATABASE_URL", required_database_url())
        .env("OPMUX_DB_ROLE", "opmux_operator")
        .env("NO_PROXY", "*")
        .env_remove("OPENAI_API_KEY")
        .env_remove("ANTHROPIC_API_KEY");
    command
}

fn run_admin(args: &[&str]) -> Output {
    admin_command()
        .args(args)
        .output()
        .expect("spawn opmux-admin")
}

fn stdout_text(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stderr_text(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

fn assert_no_issued_secret(output: &Output) {
    let stdout = stdout_text(output);
    let stderr = stderr_text(output);
    assert!(
        !stdout.contains(CREDENTIAL_PREFIX),
        "failed CLI stdout must not include a credential"
    );
    assert!(
        !stderr.contains(CREDENTIAL_PREFIX),
        "CLI stderr must not include a credential"
    );
    assert!(
        !stdout.contains("\"credential\""),
        "failed CLI stdout must not include a credential field"
    );
}

struct Issued {
    client_id: Uuid,
    key_id: Uuid,
    display_id: String,
    name: String,
    kind: String,
    credential: String,
}

fn parse_issued(output: &Output) -> Issued {
    assert!(
        output.status.success(),
        "opmux-admin exited nonzero; output omitted"
    );
    let stdout = stdout_text(output);
    let value: serde_json::Value = serde_json::from_str(stdout.trim())
        .unwrap_or_else(|_| panic!("opmux-admin stdout was not JSON; output omitted"));
    let credential = value
        .get("credential")
        .and_then(|item| item.as_str())
        .unwrap_or("")
        .to_string();
    let client_id = value
        .get("client_id")
        .and_then(|item| item.as_str())
        .and_then(|item| Uuid::parse_str(item).ok())
        .expect("client_id");
    let key_id = value
        .get("key_id")
        .and_then(|item| item.as_str())
        .and_then(|item| Uuid::parse_str(item).ok())
        .expect("key_id");
    let display_id = value
        .get("display_id")
        .and_then(|item| item.as_str())
        .unwrap_or("")
        .to_string();
    let name = value
        .get("name")
        .and_then(|item| item.as_str())
        .unwrap_or("")
        .to_string();
    let kind = value
        .get("kind")
        .and_then(|item| item.as_str())
        .unwrap_or("")
        .to_string();
    assert!(
        credential.starts_with(CREDENTIAL_PREFIX),
        "issued JSON must include a versioned credential"
    );
    assert!(
        parse_credential_payload(&credential)
            .is_some_and(|bytes| bytes.len() == SECRET_PAYLOAD_LEN),
        "issued credential payload must be 32 bytes"
    );
    Issued {
        client_id,
        key_id,
        display_id,
        name,
        kind,
        credential,
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

async fn client_count_for(pool: &sqlx::PgPool, client_id: Uuid) -> i64 {
    sqlx::query_scalar("SELECT COUNT(*) FROM opmux_private.clients WHERE id = $1")
        .bind(client_id)
        .fetch_one(pool)
        .await
        .expect("client count")
}

#[test]
fn help_documents_secure_output_and_operator_privileges() {
    let output = run_admin(&["--help"]);
    assert!(output.status.success());
    let stdout = stdout_text(&output);
    let stderr = stderr_text(&output);
    let help = format!("{stdout}{stderr}");
    assert!(help.contains("stdout"));
    assert!(help.contains("once"));
    assert!(help.contains("opmux_operator"));
    assert!(help.contains("scripts/db-migrate.sh"));
    assert!(!help.contains(CREDENTIAL_PREFIX));
}

#[tokio::test]
#[serial]
async fn cli_provisions_two_tenants_and_six_typed_keys() {
    let pool = test_pool().await;
    let name_a = format!("cli-a-{}", Uuid::new_v4().simple());
    let name_b = format!("cli-b-{}", Uuid::new_v4().simple());

    let created_a = parse_issued(&run_admin(&["tenant", "create", "--name", &name_a]));
    let created_b = parse_issued(&run_admin(&["tenant", "create", "--name", &name_b]));
    let a_mgr = parse_issued(&run_admin(&[
        "key",
        "issue",
        "--client-id",
        &created_a.client_id.to_string(),
        "--kind",
        "management",
        "--name",
        "a-manager-2",
    ]));
    let a_inf = parse_issued(&run_admin(&[
        "key",
        "issue",
        "--client-id",
        &created_a.client_id.to_string(),
        "--kind",
        "inference",
        "--name",
        "a-inference",
    ]));
    let b_mgr = parse_issued(&run_admin(&[
        "key",
        "issue",
        "--client-id",
        &created_b.client_id.to_string(),
        "--kind",
        "management",
        "--name",
        "b-manager-2",
    ]));
    let b_inf = parse_issued(&run_admin(&[
        "key",
        "issue",
        "--client-id",
        &created_b.client_id.to_string(),
        "--kind",
        "inference",
        "--name",
        "b-inference",
    ]));

    assert_eq!(created_a.name, INITIAL_MANAGEMENT_KEY_NAME);
    assert_eq!(created_a.kind, "management");
    assert_eq!(created_b.kind, "management");
    assert_ne!(created_a.client_id, created_b.client_id);
    assert_eq!(a_mgr.client_id, created_a.client_id);
    assert_eq!(a_inf.client_id, created_a.client_id);
    assert_eq!(b_mgr.client_id, created_b.client_id);
    assert_eq!(b_inf.client_id, created_b.client_id);
    assert_eq!(a_mgr.kind, "management");
    assert_eq!(a_inf.kind, "inference");
    assert_eq!(b_mgr.kind, "management");
    assert_eq!(b_inf.kind, "inference");
    assert_eq!(client_count_for(&pool, created_a.client_id).await, 1);
    assert_eq!(client_count_for(&pool, created_b.client_id).await, 1);
    assert_eq!(key_count(&pool, created_a.client_id).await, 3);
    assert_eq!(key_count(&pool, created_b.client_id).await, 3);

    let service =
        ProvisioningService::new(Arc::new(PostgresAuthStore::new(pool.clone())));
    let expected = [
        (&created_a, ApiKeyKind::Management),
        (&created_b, ApiKeyKind::Management),
        (&a_mgr, ApiKeyKind::Management),
        (&a_inf, ApiKeyKind::Inference),
        (&b_mgr, ApiKeyKind::Management),
        (&b_inf, ApiKeyKind::Inference),
    ];
    let mut secrets = HashSet::new();
    for (issued, kind) in expected {
        assert!(secrets.insert(issued.credential.clone()));
        let identity = service
            .resolve_credential(&issued.credential)
            .await
            .expect("resolve")
            .expect("identity");
        assert_eq!(identity.client_id, issued.client_id);
        assert_eq!(identity.key_id, issued.key_id);
        assert_eq!(identity.kind, kind);
        assert_eq!(identity.display_id, issued.display_id);
        let row = sqlx::query(
            "SELECT octet_length(key_digest) AS digest_len, key_digest, kind
             FROM opmux_private.api_keys WHERE id = $1",
        )
        .bind(issued.key_id)
        .fetch_one(&pool)
        .await
        .expect("row");
        let digest_len: i32 = row.get("digest_len");
        let digest: Vec<u8> = row.get("key_digest");
        let stored_kind: String = row.get("kind");
        let expected_digest = hash_credential(&issued.credential);
        assert_eq!(digest_len, 32);
        assert!(digest.as_slice() == expected_digest.as_bytes().as_slice());
        assert_eq!(stored_kind, kind.as_str());
    }
    assert!(secrets.len() == 6, "CLI credentials must be distinct");

    cleanup(&pool, &[created_a.client_id, created_b.client_id]).await;
}

#[tokio::test]
#[serial]
async fn invalid_cli_issuance_leaves_no_partial_state() {
    let pool = test_pool().await;
    let created = parse_issued(&run_admin(&[
        "tenant",
        "create",
        "--name",
        &format!("cli-keep-{}", Uuid::new_v4().simple()),
    ]));
    let before_keys = key_count(&pool, created.client_id).await;
    let before_clients: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM opmux_private.clients")
            .fetch_one(&pool)
            .await
            .expect("clients");

    let malformed = run_admin(&[
        "key",
        "issue",
        "--client-id",
        "not-a-uuid",
        "--kind",
        "management",
        "--name",
        "bad-id",
    ]);
    assert!(!malformed.status.success());
    assert_no_issued_secret(&malformed);

    let missing_id = Uuid::new_v4();
    let missing = run_admin(&[
        "key",
        "issue",
        "--client-id",
        &missing_id.to_string(),
        "--kind",
        "inference",
        "--name",
        "ghost",
    ]);
    assert!(!missing.status.success());
    assert_no_issued_secret(&missing);
    let stderr = stderr_text(&missing);
    assert!(
        stderr.contains("unknown client"),
        "nonexistent client must fail closed"
    );

    let bad_kind = run_admin(&[
        "key",
        "issue",
        "--client-id",
        &created.client_id.to_string(),
        "--kind",
        "admin",
        "--name",
        "not-a-kind",
    ]);
    assert!(!bad_kind.status.success());
    assert_no_issued_secret(&bad_kind);

    let after_clients: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM opmux_private.clients")
            .fetch_one(&pool)
            .await
            .expect("clients after");
    assert_eq!(before_clients, after_clients);
    assert_eq!(key_count(&pool, created.client_id).await, before_keys);
    assert_eq!(client_count_for(&pool, missing_id).await, 0);

    let service =
        ProvisioningService::new(Arc::new(PostgresAuthStore::new(pool.clone())));
    let identity = service
        .resolve_credential(&created.credential)
        .await
        .expect("control resolve")
        .expect("control identity");
    assert_eq!(identity.key_id, created.key_id);
    assert_eq!(identity.kind, ApiKeyKind::Management);

    cleanup(&pool, &[created.client_id]).await;
}

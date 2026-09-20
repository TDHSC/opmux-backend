//! Production-router HTTP tests for key revocation, rotation, and operator
//! recovery against owned local Supabase.
//!
//! Capture issued credentials privately. Do not print secrets, digests, or
//! bodies that contain them.

mod support;

use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use chrono::{DateTime, Utc};
use gateway::{
    core::metrics::MetricsConfig,
    features::auth::{
        ApiKeyKind, AuthService, PostgresAuthStore, ProvisioningService,
        CREDENTIAL_PREFIX,
    },
};
use serial_test::serial;
use std::path::PathBuf;
use std::process::{Command, Output};
use std::sync::Arc;
use support::{
    cleanup_clients, isolate_provider_environment, production_router_with_auth,
    required_database_url, test_pool, OpenAiSimulator, SIMULATED_CONTENT,
};
use tower::ServiceExt;
use uuid::Uuid;

const ROUTE_BODY: &str = r#"{"prompt":"lifecycle generation","metadata":{}}"#;

struct Issued {
    client_id: Uuid,
    key_id: Uuid,
    credential: String,
}

struct TwoTenantFixture {
    pool: sqlx::PgPool,
    a_management: Issued,
    a_inference: Issued,
    b_management: Issued,
    b_inference: Issued,
}

impl TwoTenantFixture {
    async fn provision() -> Self {
        let pool = test_pool().await;
        let service =
            ProvisioningService::new(Arc::new(PostgresAuthStore::new(pool.clone())));
        let created_a = service
            .create_tenant(&format!("life-a-{}", Uuid::new_v4().simple()))
            .await
            .expect("tenant a");
        let created_b = service
            .create_tenant(&format!("life-b-{}", Uuid::new_v4().simple()))
            .await
            .expect("tenant b");
        let a_inference = service
            .issue_key(
                created_a.key.client_id,
                ApiKeyKind::Inference,
                "a-inference",
            )
            .await
            .expect("a inference");
        let b_inference = service
            .issue_key(
                created_b.key.client_id,
                ApiKeyKind::Inference,
                "b-inference",
            )
            .await
            .expect("b inference");
        Self {
            pool,
            a_management: Issued {
                client_id: created_a.key.client_id,
                key_id: created_a.key.key_id,
                credential: created_a.key.credential().to_string(),
            },
            a_inference: Issued {
                client_id: a_inference.client_id,
                key_id: a_inference.key_id,
                credential: a_inference.credential().to_string(),
            },
            b_management: Issued {
                client_id: created_b.key.client_id,
                key_id: created_b.key.key_id,
                credential: created_b.key.credential().to_string(),
            },
            b_inference: Issued {
                client_id: b_inference.client_id,
                key_id: b_inference.key_id,
                credential: b_inference.credential().to_string(),
            },
        }
    }

    fn client_ids(&self) -> [Uuid; 2] {
        [self.a_management.client_id, self.b_management.client_id]
    }

    fn auth_service(&self) -> Arc<AuthService> {
        Arc::new(AuthService::new(Arc::new(PostgresAuthStore::new(
            self.pool.clone(),
        ))))
    }

    async fn drop_rows(&self) {
        cleanup_clients(&self.pool, &self.client_ids()).await;
    }
}

async fn body_string(response: axum::http::Response<Body>) -> String {
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("response body");
    String::from_utf8(bytes.to_vec()).expect("utf8 body")
}

fn route_request(api_key: Option<&str>) -> Request<Body> {
    let mut builder = Request::builder()
        .method("POST")
        .uri("/api/v1/route")
        .header("content-type", "application/json");
    if let Some(key) = api_key {
        builder = builder.header("x-api-key", key);
    }
    builder.body(Body::from(ROUTE_BODY)).unwrap()
}

fn keys_request(
    method: &str,
    uri: &str,
    api_key: Option<&str>,
    body: Option<String>,
) -> Request<Body> {
    let mut builder = Request::builder()
        .method(method)
        .uri(uri)
        .header("content-type", "application/json");
    if let Some(key) = api_key {
        builder = builder.header("x-api-key", key);
    }
    builder.body(Body::from(body.unwrap_or_default())).unwrap()
}

fn delete_key(api_key: Option<&str>, key_id: Uuid) -> Request<Body> {
    keys_request(
        "DELETE",
        &format!("/api/v1/auth/keys/{key_id}"),
        api_key,
        None,
    )
}

fn create_key(api_key: &str, name: &str, kind: &str) -> Request<Body> {
    keys_request(
        "POST",
        "/api/v1/auth/keys",
        Some(api_key),
        Some(format!(r#"{{"name":"{name}","kind":"{kind}"}}"#)),
    )
}

fn list_keys(api_key: &str) -> Request<Body> {
    keys_request("GET", "/api/v1/auth/keys", Some(api_key), None)
}

async fn create_named_key(
    app: &axum::Router,
    manager: &str,
    name: &str,
    kind: &str,
) -> Issued {
    let response = app
        .clone()
        .oneshot(create_key(manager, name, kind))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::CREATED);
    parse_issued_http(body_string(response).await)
}

fn parse_issued_http(body: String) -> Issued {
    let value: serde_json::Value = serde_json::from_str(&body).expect("issued json");
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
    assert!(credential.starts_with(CREDENTIAL_PREFIX));
    Issued {
        client_id,
        key_id,
        credential,
    }
}

fn error_message(body: &str) -> String {
    let value: serde_json::Value =
        serde_json::from_str(body).unwrap_or(serde_json::json!({}));
    match value.get("error") {
        Some(serde_json::Value::String(message)) => message.clone(),
        Some(object) => object
            .get("message")
            .and_then(|item| item.as_str())
            .unwrap_or("")
            .to_string(),
        None => String::new(),
    }
}

fn inventory_item(body: &str, key_id: Uuid) -> Option<serde_json::Value> {
    let value: serde_json::Value = serde_json::from_str(body).expect("inventory json");
    value
        .get("keys")
        .and_then(|item| item.as_array())
        .and_then(|keys| {
            keys.iter().find(|item| {
                item.get("key_id").and_then(|value| value.as_str())
                    == Some(key_id.to_string()).as_deref()
            })
        })
        .cloned()
}

async fn key_row(
    pool: &sqlx::PgPool,
    key_id: Uuid,
) -> Option<(Uuid, String, String, Option<DateTime<Utc>>)> {
    sqlx::query_as(
        "SELECT client_id, name, kind, revoked_at
         FROM opmux_private.api_keys WHERE id = $1",
    )
    .bind(key_id)
    .fetch_optional(pool)
    .await
    .expect("key row")
}

async fn client_exists(pool: &sqlx::PgPool, client_id: Uuid) -> bool {
    let count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM opmux_private.clients WHERE id = $1")
            .bind(client_id)
            .fetch_one(pool)
            .await
            .expect("client count");
    count == 1
}

fn admin_bin() -> PathBuf {
    std::path::Path::new(env!("CARGO_BIN_EXE_gateway")).with_file_name("opmux-admin")
}

fn run_admin(args: &[&str]) -> Output {
    Command::new(admin_bin())
        .args(args)
        .env("DATABASE_URL", required_database_url())
        .env("OPMUX_DB_ROLE", "opmux_operator")
        .env("NO_PROXY", "*")
        .env_remove("OPENAI_API_KEY")
        .env_remove("ANTHROPIC_API_KEY")
        .output()
        .expect("spawn opmux-admin")
}

#[tokio::test]
#[serial]
async fn revoke_is_committed_selective_and_idempotent() {
    isolate_provider_environment();
    let fixture = TwoTenantFixture::provision().await;
    let simulator = OpenAiSimulator::start().await;
    let app = production_router_with_auth(
        &simulator,
        fixture.auth_service(),
        MetricsConfig::disabled(),
    );

    let extra_manager = create_named_key(
        &app,
        &fixture.a_management.credential,
        "a-extra-manager",
        "management",
    )
    .await;
    let control = create_named_key(
        &app,
        &fixture.a_management.credential,
        "a-control-inference",
        "inference",
    )
    .await;
    assert_eq!(extra_manager.client_id, fixture.a_management.client_id);
    assert_eq!(control.client_id, fixture.a_management.client_id);

    let first_inference = app
        .clone()
        .oneshot(delete_key(
            Some(&fixture.a_management.credential),
            fixture.a_inference.key_id,
        ))
        .await
        .unwrap();
    assert_eq!(first_inference.status(), StatusCode::NO_CONTENT);
    assert!(body_string(first_inference).await.is_empty());

    let first_manager = app
        .clone()
        .oneshot(delete_key(
            Some(&fixture.a_management.credential),
            extra_manager.key_id,
        ))
        .await
        .unwrap();
    assert_eq!(first_manager.status(), StatusCode::NO_CONTENT);
    assert!(body_string(first_manager).await.is_empty());

    let inference_row = key_row(&fixture.pool, fixture.a_inference.key_id)
        .await
        .expect("inference row retained");
    let manager_row = key_row(&fixture.pool, extra_manager.key_id)
        .await
        .expect("manager row retained");
    assert_eq!(inference_row.0, fixture.a_management.client_id);
    assert_eq!(inference_row.2, "inference");
    assert!(inference_row.3.is_some());
    assert_eq!(manager_row.0, fixture.a_management.client_id);
    assert_eq!(manager_row.2, "management");
    assert!(manager_row.3.is_some());
    let inference_revoked_at = inference_row.3;
    let manager_revoked_at = manager_row.3;

    let list = app
        .clone()
        .oneshot(list_keys(&fixture.a_management.credential))
        .await
        .unwrap();
    assert_eq!(list.status(), StatusCode::OK);
    let list_body = body_string(list).await;
    let listed_inference =
        inventory_item(&list_body, fixture.a_inference.key_id).expect("listed inference");
    let listed_manager =
        inventory_item(&list_body, extra_manager.key_id).expect("listed manager");
    assert!(listed_inference
        .get("revoked_at")
        .and_then(|v| v.as_str())
        .is_some());
    assert!(listed_manager
        .get("revoked_at")
        .and_then(|v| v.as_str())
        .is_some());
    assert!(!list_body.contains(&fixture.a_inference.credential));
    assert!(!list_body.contains(&extra_manager.credential));
    assert!(!list_body.contains("digest"));

    let repeat_inference = app
        .clone()
        .oneshot(delete_key(
            Some(&fixture.a_management.credential),
            fixture.a_inference.key_id,
        ))
        .await
        .unwrap();
    let repeat_manager = app
        .clone()
        .oneshot(delete_key(
            Some(&fixture.a_management.credential),
            extra_manager.key_id,
        ))
        .await
        .unwrap();
    assert_eq!(repeat_inference.status(), StatusCode::NO_CONTENT);
    assert_eq!(repeat_manager.status(), StatusCode::NO_CONTENT);
    assert_eq!(
        key_row(&fixture.pool, fixture.a_inference.key_id)
            .await
            .expect("inference after repeat")
            .3,
        inference_revoked_at
    );
    assert_eq!(
        key_row(&fixture.pool, extra_manager.key_id)
            .await
            .expect("manager after repeat")
            .3,
        manager_revoked_at
    );

    let before_denied = simulator.generation_count();
    let revoked_inference = app
        .clone()
        .oneshot(route_request(Some(&fixture.a_inference.credential)))
        .await
        .unwrap();
    assert_eq!(revoked_inference.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(simulator.generation_count(), before_denied);

    let revoked_manager = app
        .clone()
        .oneshot(list_keys(&extra_manager.credential))
        .await
        .unwrap();
    assert_eq!(revoked_manager.status(), StatusCode::UNAUTHORIZED);

    let control_generate = app
        .clone()
        .oneshot(route_request(Some(&control.credential)))
        .await
        .unwrap();
    assert_eq!(control_generate.status(), StatusCode::OK);
    let control_body = body_string(control_generate).await;
    assert!(control_body.contains(SIMULATED_CONTENT));

    let a_manager_list = app
        .clone()
        .oneshot(list_keys(&fixture.a_management.credential))
        .await
        .unwrap();
    assert_eq!(a_manager_list.status(), StatusCode::OK);

    let b_list = app
        .clone()
        .oneshot(list_keys(&fixture.b_management.credential))
        .await
        .unwrap();
    assert_eq!(b_list.status(), StatusCode::OK);
    let b_generate = app
        .clone()
        .oneshot(route_request(Some(&fixture.b_inference.credential)))
        .await
        .unwrap();
    assert_eq!(b_generate.status(), StatusCode::OK);

    fixture.drop_rows().await;
}

#[tokio::test]
#[serial]
async fn revocation_denies_cross_tenant_targets_without_revealing_existence() {
    isolate_provider_environment();
    let fixture = TwoTenantFixture::provision().await;
    let simulator = OpenAiSimulator::start().await;
    let app = production_router_with_auth(
        &simulator,
        fixture.auth_service(),
        MetricsConfig::disabled(),
    );
    let missing_id = Uuid::new_v4();
    let b_before = key_row(&fixture.pool, fixture.b_inference.key_id)
        .await
        .expect("b inference before");
    let a_before = key_row(&fixture.pool, fixture.a_inference.key_id)
        .await
        .expect("a inference before");

    let cross = app
        .clone()
        .oneshot(delete_key(
            Some(&fixture.a_management.credential),
            fixture.b_inference.key_id,
        ))
        .await
        .unwrap();
    let absent = app
        .clone()
        .oneshot(delete_key(
            Some(&fixture.a_management.credential),
            missing_id,
        ))
        .await
        .unwrap();
    assert_eq!(cross.status(), StatusCode::NOT_FOUND);
    assert_eq!(absent.status(), StatusCode::NOT_FOUND);
    let cross_body = body_string(cross).await;
    let absent_body = body_string(absent).await;
    assert_eq!(error_message(&cross_body), error_message(&absent_body));
    assert!(!error_message(&cross_body).is_empty());
    assert!(!cross_body.contains(&fixture.b_inference.credential));
    assert!(!absent_body.contains(&fixture.a_management.credential));

    assert_eq!(
        key_row(&fixture.pool, fixture.b_inference.key_id)
            .await
            .expect("b inference unchanged"),
        b_before
    );
    let b_generate = app
        .clone()
        .oneshot(route_request(Some(&fixture.b_inference.credential)))
        .await
        .unwrap();
    assert_eq!(b_generate.status(), StatusCode::OK);

    let missing = app
        .clone()
        .oneshot(delete_key(None, fixture.a_inference.key_id))
        .await
        .unwrap();
    let unknown = app
        .clone()
        .oneshot(delete_key(
            Some("opmx_v1_unknown-lifecycle-key"),
            fixture.a_inference.key_id,
        ))
        .await
        .unwrap();
    assert_eq!(missing.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(unknown.status(), StatusCode::UNAUTHORIZED);

    let inference_self = app
        .clone()
        .oneshot(delete_key(
            Some(&fixture.a_inference.credential),
            fixture.a_inference.key_id,
        ))
        .await
        .unwrap();
    assert_eq!(inference_self.status(), StatusCode::FORBIDDEN);
    assert_eq!(
        key_row(&fixture.pool, fixture.a_inference.key_id)
            .await
            .expect("a inference unchanged"),
        a_before
    );
    assert_eq!(
        key_row(&fixture.pool, fixture.b_management.key_id)
            .await
            .expect("b manager unchanged")
            .3,
        None
    );

    fixture.drop_rows().await;
}

#[tokio::test]
#[serial]
async fn management_rotation_and_final_manager_self_revocation_have_operator_recovery() {
    isolate_provider_environment();
    let fixture = TwoTenantFixture::provision().await;
    let simulator = OpenAiSimulator::start().await;
    let app = production_router_with_auth(
        &simulator,
        fixture.auth_service(),
        MetricsConfig::disabled(),
    );
    let inference_before = key_row(&fixture.pool, fixture.a_inference.key_id)
        .await
        .expect("inference before rotation");

    let replacement = create_named_key(
        &app,
        &fixture.a_management.credential,
        "a-replacement-manager",
        "management",
    )
    .await;
    assert_eq!(replacement.client_id, fixture.a_management.client_id);

    let overlap_list = app
        .clone()
        .oneshot(list_keys(&replacement.credential))
        .await
        .unwrap();
    assert_eq!(overlap_list.status(), StatusCode::OK);
    let overlap_body = body_string(overlap_list).await;
    assert!(inventory_item(&overlap_body, fixture.a_management.key_id).is_some());
    assert!(inventory_item(&overlap_body, replacement.key_id).is_some());

    let revoke_old = app
        .clone()
        .oneshot(delete_key(
            Some(&replacement.credential),
            fixture.a_management.key_id,
        ))
        .await
        .unwrap();
    assert_eq!(revoke_old.status(), StatusCode::NO_CONTENT);
    let old_denied = app
        .clone()
        .oneshot(list_keys(&fixture.a_management.credential))
        .await
        .unwrap();
    assert_eq!(old_denied.status(), StatusCode::UNAUTHORIZED);
    let replacement_still = app
        .clone()
        .oneshot(list_keys(&replacement.credential))
        .await
        .unwrap();
    assert_eq!(replacement_still.status(), StatusCode::OK);

    let self_revoke = app
        .clone()
        .oneshot(delete_key(
            Some(&replacement.credential),
            replacement.key_id,
        ))
        .await
        .unwrap();
    assert_eq!(self_revoke.status(), StatusCode::NO_CONTENT);
    let self_denied = app
        .clone()
        .oneshot(list_keys(&replacement.credential))
        .await
        .unwrap();
    assert_eq!(self_denied.status(), StatusCode::UNAUTHORIZED);

    let recovered_name = format!("a-recovered-{}", Uuid::new_v4().simple());
    let client_id = fixture.a_management.client_id.to_string();
    let output = run_admin(&[
        "key",
        "issue",
        "--client-id",
        &client_id,
        "--kind",
        "management",
        "--name",
        &recovered_name,
    ]);
    assert!(
        output.status.success(),
        "operator recovery must succeed; output omitted"
    );
    let recovered =
        parse_issued_http(String::from_utf8_lossy(&output.stdout).into_owned());
    assert_eq!(recovered.client_id, fixture.a_management.client_id);
    assert!(!String::from_utf8_lossy(&output.stderr).contains(CREDENTIAL_PREFIX));

    let recovered_list = app
        .clone()
        .oneshot(list_keys(&recovered.credential))
        .await
        .unwrap();
    assert_eq!(recovered_list.status(), StatusCode::OK);
    let recovered_body = body_string(recovered_list).await;
    let old_listed = inventory_item(&recovered_body, fixture.a_management.key_id)
        .expect("old manager");
    let replacement_listed =
        inventory_item(&recovered_body, replacement.key_id).expect("replacement");
    assert!(old_listed
        .get("revoked_at")
        .and_then(|v| v.as_str())
        .is_some());
    assert!(replacement_listed
        .get("revoked_at")
        .and_then(|v| v.as_str())
        .is_some());
    assert!(inventory_item(&recovered_body, recovered.key_id).is_some());
    assert!(!recovered_body.contains(&recovered.credential));

    assert!(client_exists(&fixture.pool, fixture.a_management.client_id).await);
    assert_eq!(
        key_row(&fixture.pool, fixture.a_inference.key_id)
            .await
            .expect("inference after recovery"),
        inference_before
    );
    let inference_ok = app
        .clone()
        .oneshot(route_request(Some(&fixture.a_inference.credential)))
        .await
        .unwrap();
    assert_eq!(inference_ok.status(), StatusCode::OK);
    let still_revoked = app
        .clone()
        .oneshot(list_keys(&fixture.a_management.credential))
        .await
        .unwrap();
    assert_eq!(still_revoked.status(), StatusCode::UNAUTHORIZED);
    let replacement_still_revoked = app
        .clone()
        .oneshot(list_keys(&replacement.credential))
        .await
        .unwrap();
    assert_eq!(replacement_still_revoked.status(), StatusCode::UNAUTHORIZED);

    fixture.drop_rows().await;
}

#[test]
fn delete_route_and_revoke_sql_are_tenant_scoped() {
    let app = include_str!("../src/app.rs");
    let handler = include_str!("../src/features/auth/handler.rs");
    let service = include_str!("../src/features/auth/service.rs");
    let postgres = include_str!("../src/features/auth/persist/postgres.rs");
    assert!(app.contains("/api/v1/auth/keys/{id}"));
    assert!(app.contains("revoke_api_key") || handler.contains("revoke_api_key"));
    assert!(handler.contains("DELETE") || handler.contains("revoke"));
    assert!(service.contains("revoke_key"));
    assert!(service.contains("actor.client_id"));
    assert!(postgres.contains("WHERE id = $1 AND client_id = $2 AND revoked_at IS NULL"));
}

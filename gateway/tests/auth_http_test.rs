//! Persisted fail-closed HTTP authentication against owned Supabase and the
//! local OpenAI simulator.
//!
//! Capture issued credentials privately. Do not print secrets, digests, or
//! raw debug bodies that contain them.

mod support;

use axum::{
    body::Body,
    http::{HeaderValue, Request, StatusCode},
};
use gateway::{
    core::{db::DatabasePoolConfig, metrics::MetricsConfig},
    features::auth::{
        hash_credential, ApiKeyKind, AuthService, AuthStore, PostgresAuthStore,
        ProvisioningService, CREDENTIAL_PREFIX,
    },
};
use serial_test::serial;
use std::io::Read;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};
use support::{
    isolate_provider_environment, production_router_with_auth, OpenAiSimulator,
    MOCK_GATEWAY_API_KEY, SIMULATED_CONTENT,
};
use tower::ServiceExt;
use uuid::Uuid;

const LEGACY_DEV_KEY: &str = "dev-api-key-456";
const ROUTE_BODY: &str = r#"{"prompt":"persisted auth generation","metadata":{}}"#;

fn required_database_url() -> String {
    match std::env::var("DATABASE_URL") {
        Ok(url) if !url.trim().is_empty() => url,
        _ => panic!(
            "DATABASE_URL is required for persisted authentication tests and must point at the owned local Supabase on 127.0.0.1:55432. Tests do not skip when the database is unavailable."
        ),
    }
}

async fn test_pool() -> sqlx::PgPool {
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

async fn last_used_is_set(pool: &sqlx::PgPool, key_id: Uuid) -> bool {
    sqlx::query_scalar(
        "SELECT last_used_at IS NOT NULL FROM opmux_private.api_keys WHERE id = $1",
    )
    .bind(key_id)
    .fetch_one(pool)
    .await
    .expect("last_used query")
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

struct Issued {
    client_id: Uuid,
    key_id: Uuid,
    kind: ApiKeyKind,
    credential: String,
}

struct TwoTenantFixture {
    pool: sqlx::PgPool,
    service: ProvisioningService,
    a_management: Issued,
    a_management2: Issued,
    a_inference: Issued,
    b_management: Issued,
    b_management2: Issued,
    b_inference: Issued,
}

impl TwoTenantFixture {
    async fn provision() -> Self {
        let pool = test_pool().await;
        let service =
            ProvisioningService::new(Arc::new(PostgresAuthStore::new(pool.clone())));
        let tenant_a = format!("http-a-{}", Uuid::new_v4().simple());
        let tenant_b = format!("http-b-{}", Uuid::new_v4().simple());
        let created_a = service.create_tenant(&tenant_a).await.expect("tenant a");
        let created_b = service.create_tenant(&tenant_b).await.expect("tenant b");
        let a_management2 = service
            .issue_key(
                created_a.key.client_id,
                ApiKeyKind::Management,
                "a-manager-2",
            )
            .await
            .expect("a manager 2");
        let a_inference = service
            .issue_key(
                created_a.key.client_id,
                ApiKeyKind::Inference,
                "a-inference",
            )
            .await
            .expect("a inference");
        let b_management2 = service
            .issue_key(
                created_b.key.client_id,
                ApiKeyKind::Management,
                "b-manager-2",
            )
            .await
            .expect("b manager 2");
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
            service,
            a_management: Issued {
                client_id: created_a.key.client_id,
                key_id: created_a.key.key_id,
                kind: ApiKeyKind::Management,
                credential: created_a.key.credential().to_string(),
            },
            a_management2: Issued {
                client_id: a_management2.client_id,
                key_id: a_management2.key_id,
                kind: ApiKeyKind::Management,
                credential: a_management2.credential().to_string(),
            },
            a_inference: Issued {
                client_id: a_inference.client_id,
                key_id: a_inference.key_id,
                kind: ApiKeyKind::Inference,
                credential: a_inference.credential().to_string(),
            },
            b_management: Issued {
                client_id: created_b.key.client_id,
                key_id: created_b.key.key_id,
                kind: ApiKeyKind::Management,
                credential: created_b.key.credential().to_string(),
            },
            b_management2: Issued {
                client_id: b_management2.client_id,
                key_id: b_management2.key_id,
                kind: ApiKeyKind::Management,
                credential: b_management2.credential().to_string(),
            },
            b_inference: Issued {
                client_id: b_inference.client_id,
                key_id: b_inference.key_id,
                kind: ApiKeyKind::Inference,
                credential: b_inference.credential().to_string(),
            },
        }
    }

    fn client_ids(&self) -> [Uuid; 2] {
        [self.a_management.client_id, self.b_management.client_id]
    }

    fn all_issued(&self) -> [&Issued; 6] {
        [
            &self.a_management,
            &self.a_management2,
            &self.a_inference,
            &self.b_management,
            &self.b_management2,
            &self.b_inference,
        ]
    }

    fn auth_service(&self) -> Arc<AuthService> {
        Arc::new(AuthService::new(Arc::new(PostgresAuthStore::new(
            self.pool.clone(),
        ))))
    }

    async fn drop_rows(&self) {
        cleanup(&self.pool, &self.client_ids()).await;
    }
}

#[tokio::test]
#[serial]
async fn cli_two_tenant_inference_keys_generate_through_production_router() {
    isolate_provider_environment();
    let fixture = TwoTenantFixture::provision().await;
    let simulator = OpenAiSimulator::start().await;
    let app = production_router_with_auth(
        &simulator,
        fixture.auth_service(),
        MetricsConfig::disabled(),
    );

    for issued in [&fixture.a_inference, &fixture.b_inference] {
        let response = app
            .clone()
            .oneshot(route_request(Some(&issued.credential)))
            .await
            .unwrap();
        assert_eq!(
            response.status(),
            StatusCode::OK,
            "persisted inference credentials must generate"
        );
        let body = body_string(response).await;
        assert!(body.contains(SIMULATED_CONTENT));
        assert!(last_used_is_set(&fixture.pool, issued.key_id).await);
    }
    assert!(simulator.generation_count() >= 2);
    fixture.drop_rows().await;
}

#[tokio::test]
#[serial]
async fn six_provisioned_credentials_resolve_persisted_identity_and_kind() {
    isolate_provider_environment();
    let fixture = TwoTenantFixture::provision().await;
    let auth = fixture.auth_service();
    for issued in fixture.all_issued() {
        let identity = fixture
            .service
            .resolve_credential(&issued.credential)
            .await
            .expect("resolve")
            .expect("identity");
        assert_eq!(identity.client_id, issued.client_id);
        assert_eq!(identity.key_id, issued.key_id);
        assert_eq!(identity.kind, issued.kind);
        assert!(!identity.revoked);
        assert_ne!(identity.kind.as_str(), issued.credential.as_str());
        let context = auth
            .authenticate(&issued.credential)
            .await
            .expect("authenticate");
        assert_eq!(context.client_id, issued.client_id);
        assert_eq!(context.key_id, issued.key_id);
        assert_eq!(context.kind, issued.kind);
    }
    fixture.drop_rows().await;
}

#[tokio::test]
#[serial]
async fn forged_metadata_cannot_replace_authenticated_identity() {
    isolate_provider_environment();
    let fixture = TwoTenantFixture::provision().await;
    let simulator = OpenAiSimulator::start().await;
    let app = production_router_with_auth(
        &simulator,
        fixture.auth_service(),
        MetricsConfig::disabled(),
    );
    let before = simulator.generation_count();
    let body = format!(
        r#"{{"prompt":"identity check","metadata":{{"client_id":"{}","kind":"management","tenant_id":"{}"}}}}"#,
        fixture.b_management.client_id, fixture.b_management.client_id
    );
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/route")
                .header("content-type", "application/json")
                .header("x-api-key", &fixture.a_inference.credential)
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert!(simulator.generation_count() > before);
    let identity = fixture
        .service
        .resolve_credential(&fixture.a_inference.credential)
        .await
        .expect("resolve")
        .expect("identity");
    assert_eq!(identity.client_id, fixture.a_inference.client_id);
    assert_eq!(identity.kind, ApiKeyKind::Inference);
    assert_ne!(identity.client_id, fixture.b_management.client_id);
    fixture.drop_rows().await;
}

#[tokio::test]
#[serial]
async fn backdated_key_still_authenticates_without_expiry() {
    isolate_provider_environment();
    let fixture = TwoTenantFixture::provision().await;
    sqlx::query(
        "UPDATE opmux_private.api_keys
         SET created_at = TIMESTAMPTZ '2018-01-01 00:00:00+00'
         WHERE id = $1",
    )
    .bind(fixture.a_inference.key_id)
    .execute(&fixture.pool)
    .await
    .expect("backdate");
    let simulator = OpenAiSimulator::start().await;
    let app = production_router_with_auth(
        &simulator,
        fixture.auth_service(),
        MetricsConfig::disabled(),
    );
    let response = app
        .oneshot(route_request(Some(&fixture.a_inference.credential)))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    fixture.drop_rows().await;
}

#[tokio::test]
#[serial]
async fn rejected_headers_and_legacy_keys_never_admit_generation() {
    isolate_provider_environment();
    let fixture = TwoTenantFixture::provision().await;
    let simulator = OpenAiSimulator::start().await;
    let app = production_router_with_auth(
        &simulator,
        fixture.auth_service(),
        MetricsConfig::disabled(),
    );
    let before = simulator.generation_count();

    let missing = app
        .clone()
        .oneshot(route_request(None))
        .await
        .unwrap()
        .status();
    let empty = {
        let mut request = Request::builder()
            .method("POST")
            .uri("/api/v1/route")
            .header("content-type", "application/json")
            .body(Body::from(ROUTE_BODY))
            .unwrap();
        request
            .headers_mut()
            .insert("x-api-key", HeaderValue::from_static(" "));
        app.clone().oneshot(request).await.unwrap().status()
    };
    let malformed = {
        let mut request = Request::builder()
            .method("POST")
            .uri("/api/v1/route")
            .header("content-type", "application/json")
            .body(Body::from(ROUTE_BODY))
            .unwrap();
        request.headers_mut().insert(
            "x-api-key",
            HeaderValue::from_bytes(&[0xff, 0xfe, 0xfd]).expect("opaque header"),
        );
        app.clone().oneshot(request).await.unwrap().status()
    };
    let unknown = app
        .clone()
        .oneshot(route_request(Some("opmx_v1_unknown-well-formed-key")))
        .await
        .unwrap()
        .status();
    let duplicate_identical = {
        let mut request = Request::builder()
            .method("POST")
            .uri("/api/v1/route")
            .header("content-type", "application/json")
            .body(Body::from(ROUTE_BODY))
            .unwrap();
        request.headers_mut().append(
            "x-api-key",
            HeaderValue::from_str(&fixture.a_inference.credential).unwrap(),
        );
        request.headers_mut().append(
            "x-api-key",
            HeaderValue::from_str(&fixture.a_inference.credential).unwrap(),
        );
        app.clone().oneshot(request).await.unwrap().status()
    };
    let duplicate_conflicting = {
        let mut request = Request::builder()
            .method("POST")
            .uri("/api/v1/route")
            .header("content-type", "application/json")
            .body(Body::from(ROUTE_BODY))
            .unwrap();
        request.headers_mut().append(
            "x-api-key",
            HeaderValue::from_str(&fixture.a_inference.credential).unwrap(),
        );
        request
            .headers_mut()
            .append("x-api-key", HeaderValue::from_static(MOCK_GATEWAY_API_KEY));
        app.clone().oneshot(request).await.unwrap().status()
    };
    let comma_joined = app
        .clone()
        .oneshot(route_request(Some(&format!(
            "{},{}",
            fixture.a_inference.credential, fixture.b_inference.credential
        ))))
        .await
        .unwrap()
        .status();
    let legacy_test = app
        .clone()
        .oneshot(route_request(Some(MOCK_GATEWAY_API_KEY)))
        .await
        .unwrap()
        .status();
    let legacy_dev = app
        .clone()
        .oneshot(route_request(Some(LEGACY_DEV_KEY)))
        .await
        .unwrap()
        .status();

    let statuses = [
        ("missing", missing),
        ("empty", empty),
        ("malformed", malformed),
        ("unknown", unknown),
        ("duplicate-identical", duplicate_identical),
        ("duplicate-conflicting", duplicate_conflicting),
        ("comma-joined", comma_joined),
        ("legacy-test-api-key-123", legacy_test),
        ("legacy-dev-api-key-456", legacy_dev),
    ];
    for (label, status) in statuses {
        assert_eq!(
            status,
            StatusCode::UNAUTHORIZED,
            "{label} must return 401 and never admit work"
        );
    }
    assert_eq!(
        simulator.generation_count(),
        before,
        "rejected credentials must start zero simulator generation calls"
    );

    let control = app
        .oneshot(route_request(Some(&fixture.a_inference.credential)))
        .await
        .unwrap();
    assert_eq!(control.status(), StatusCode::OK);
    fixture.drop_rows().await;
}

#[tokio::test]
#[serial]
async fn revoked_and_unknown_credentials_produce_no_authenticated_success() {
    isolate_provider_environment();
    let fixture = TwoTenantFixture::provision().await;
    let store = PostgresAuthStore::new(fixture.pool.clone());
    store
        .revoke_key(
            fixture.a_inference.client_id,
            fixture.a_inference.key_id,
            chrono::Utc::now(),
        )
        .await
        .expect("revoke");
    let simulator = OpenAiSimulator::start().await;
    let app = production_router_with_auth(
        &simulator,
        fixture.auth_service(),
        MetricsConfig::disabled(),
    );
    let before = simulator.generation_count();
    let revoked = app
        .clone()
        .oneshot(route_request(Some(&fixture.a_inference.credential)))
        .await
        .unwrap();
    let unknown = app
        .oneshot(route_request(Some("opmx_v1_not-in-store")))
        .await
        .unwrap();
    assert_eq!(revoked.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(unknown.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(simulator.generation_count(), before);
    fixture.drop_rows().await;
}

#[tokio::test]
#[serial]
async fn authentication_diagnostics_omit_secrets_and_digests() {
    isolate_provider_environment();
    let fixture = TwoTenantFixture::provision().await;
    let secret = fixture.a_inference.credential.clone();
    let digest = hash_credential(&secret);
    let digest_hex = digest
        .as_bytes()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    let simulator = OpenAiSimulator::start().await;
    let app = production_router_with_auth(
        &simulator,
        fixture.auth_service(),
        MetricsConfig::disabled(),
    );
    let response = app.oneshot(route_request(Some(&secret))).await.unwrap();
    let status = response.status();
    let body = body_string(response).await;
    assert_eq!(status, StatusCode::OK);
    assert!(!body.contains(&secret));
    assert!(!body.contains(&digest_hex));
    let unknown = production_router_with_auth(
        &simulator,
        fixture.auth_service(),
        MetricsConfig::disabled(),
    )
    .oneshot(route_request(Some("opmx_v1_diagnostic-unknown")))
    .await
    .unwrap();
    let unknown_status = unknown.status();
    let unknown_body = body_string(unknown).await;
    assert!(!unknown_body.contains("opmx_v1_diagnostic-unknown"));
    assert!(!unknown_body.contains(&digest_hex));
    assert!(
        !unknown_body.contains(CREDENTIAL_PREFIX) || unknown_status != StatusCode::OK
    );
    fixture.drop_rows().await;
}

#[tokio::test]
#[serial]
async fn datastore_outage_fails_closed_then_recovers() {
    isolate_provider_environment();
    let fixture = TwoTenantFixture::provision().await;
    let simulator = OpenAiSimulator::start().await;
    let closed = DatabasePoolConfig::new("postgres://127.0.0.1:1/postgres")
        .expect("parseable closed-port url")
        .with_max_connections(1)
        .expect("pool size")
        .with_acquire_timeout(Duration::from_millis(200))
        .expect("short acquire");
    let down_pool = closed.connect_lazy().expect("lazy pool");
    let down_auth = Arc::new(AuthService::new(Arc::new(PostgresAuthStore::new(
        down_pool,
    ))));
    let app =
        production_router_with_auth(&simulator, down_auth, MetricsConfig::disabled());
    let before = simulator.generation_count();
    let response = app
        .oneshot(route_request(Some(&fixture.a_inference.credential)))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    let body = body_string(response).await;
    assert!(!body.contains(&fixture.a_inference.credential));
    assert!(!body.to_lowercase().contains("postgres"));
    assert_eq!(simulator.generation_count(), before);

    let recovered = production_router_with_auth(
        &simulator,
        fixture.auth_service(),
        MetricsConfig::disabled(),
    )
    .oneshot(route_request(Some(&fixture.a_inference.credential)))
    .await
    .unwrap();
    assert_eq!(recovered.status(), StatusCode::OK);
    fixture.drop_rows().await;
}

#[tokio::test]
#[serial]
async fn operator_cli_inference_keys_generate_over_http() {
    isolate_provider_environment();
    let pool = test_pool().await;
    let name_a = format!("cli-http-a-{}", Uuid::new_v4().simple());
    let name_b = format!("cli-http-b-{}", Uuid::new_v4().simple());
    let created_a =
        parse_admin_issued(&run_admin(&["tenant", "create", "--name", &name_a]));
    let created_b =
        parse_admin_issued(&run_admin(&["tenant", "create", "--name", &name_b]));
    let a_inf = parse_admin_issued(&run_admin(&[
        "key",
        "issue",
        "--client-id",
        &created_a.client_id.to_string(),
        "--kind",
        "inference",
        "--name",
        "a-inference",
    ]));
    let b_inf = parse_admin_issued(&run_admin(&[
        "key",
        "issue",
        "--client-id",
        &created_b.client_id.to_string(),
        "--kind",
        "inference",
        "--name",
        "b-inference",
    ]));
    let simulator = OpenAiSimulator::start().await;
    let app = production_router_with_auth(
        &simulator,
        Arc::new(AuthService::new(Arc::new(PostgresAuthStore::new(
            pool.clone(),
        )))),
        MetricsConfig::disabled(),
    );
    for credential in [&a_inf.credential, &b_inf.credential] {
        let response = app
            .clone()
            .oneshot(route_request(Some(credential)))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = body_string(response).await;
        assert!(body.contains(SIMULATED_CONTENT));
        assert!(!body.contains(credential));
    }
    cleanup(&pool, &[created_a.client_id, created_b.client_id]).await;
}

fn admin_bin() -> PathBuf {
    std::path::Path::new(env!("CARGO_BIN_EXE_gateway")).with_file_name("opmux-admin")
}

fn run_admin(args: &[&str]) -> std::process::Output {
    Command::new(admin_bin())
        .env("DATABASE_URL", required_database_url())
        .env("OPMUX_DB_ROLE", "opmux_operator")
        .env("NO_PROXY", "*")
        .env_remove("OPENAI_API_KEY")
        .env_remove("ANTHROPIC_API_KEY")
        .args(args)
        .output()
        .expect("spawn opmux-admin")
}

struct AdminIssued {
    client_id: Uuid,
    credential: String,
}

fn parse_admin_issued(output: &std::process::Output) -> AdminIssued {
    assert!(
        output.status.success(),
        "opmux-admin exited nonzero; output omitted"
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
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
    assert!(
        credential.starts_with(CREDENTIAL_PREFIX),
        "issued JSON must include a versioned credential"
    );
    AdminIssued {
        client_id,
        credential,
    }
}

fn example_catalog_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../config/opmux.example.json")
}

fn unused_loopback_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .expect("bind probe")
        .local_addr()
        .expect("probe addr")
        .port()
}

fn gateway_command() -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_gateway"));
    for name in [
        "OPENAI_API_KEY",
        "ANTHROPIC_API_KEY",
        "OPENAI_BASE_URL",
        "OPMUX_CONFIG_FILE",
        "HTTP_PROXY",
        "HTTPS_PROXY",
        "ALL_PROXY",
        "http_proxy",
        "https_proxy",
        "all_proxy",
        "RUST_LOG",
        "LOG_LEVEL",
        "LOG_FORMAT",
        "LOG_JSON",
    ] {
        command.env_remove(name);
    }
    command
        .env("AUTH_DEVELOPMENT_MODE", "false")
        .env("SERVER_HOST", "127.0.0.1")
        .env("METRICS_ENABLED", "false")
        .env("RUST_LOG", "debug")
        .env("LOG_FORMAT", "json")
        .env("NO_PROXY", "*");
    command
}

fn run_to_exit(mut command: Command, timeout: Duration) -> (Option<i32>, String) {
    let mut child = command
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn gateway");
    let started = Instant::now();
    loop {
        match child.try_wait().expect("wait") {
            Some(status) => {
                let mut stdout = String::new();
                let mut stderr = String::new();
                if let Some(mut out) = child.stdout.take() {
                    let _ = out.read_to_string(&mut stdout);
                }
                if let Some(mut err) = child.stderr.take() {
                    let _ = err.read_to_string(&mut stderr);
                }
                return (status.code(), format!("{stdout}{stderr}"));
            }
            None if started.elapsed() > timeout => {
                let _ = child.kill();
                let _ = child.wait();
                panic!("gateway did not exit before timeout");
            }
            None => std::thread::sleep(Duration::from_millis(20)),
        }
    }
}

fn wait_health(port: u16, child: &mut std::process::Child) -> bool {
    let started = Instant::now();
    while started.elapsed() < Duration::from_secs(8) {
        if let Ok(Some(_)) = child.try_wait() {
            return false;
        }
        let url = format!("http://127.0.0.1:{port}/health");
        if let Ok(output) = Command::new("curl")
            .args(["--noproxy", "*", "-fsS", "--max-time", "1", &url])
            .output()
        {
            if output.status.success() {
                return true;
            }
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    false
}

fn curl_route(port: u16, extra: &[&str]) -> (u16, String) {
    let url = format!("http://127.0.0.1:{port}/api/v1/route");
    let mut args = vec![
        "--noproxy".to_string(),
        "*".to_string(),
        "-sS".to_string(),
        "-o".to_string(),
        "-".to_string(),
        "-w".to_string(),
        "\n%{http_code}".to_string(),
        "--max-time".to_string(),
        "5".to_string(),
        "-X".to_string(),
        "POST".to_string(),
        "-H".to_string(),
        "Content-Type: application/json".to_string(),
        "-d".to_string(),
        ROUTE_BODY.to_string(),
    ];
    for item in extra {
        args.push((*item).to_string());
    }
    args.push(url);
    let output = Command::new("curl").args(args).output().expect("curl");
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let status = stdout
        .trim()
        .rsplit('\n')
        .next()
        .and_then(|value| value.parse().ok())
        .unwrap_or(0);
    (status, stdout)
}

#[test]
fn missing_and_invalid_database_url_exit_before_listen() {
    let catalog = example_catalog_path();
    let port = unused_loopback_port();
    let mut missing = gateway_command();
    missing
        .env_remove("DATABASE_URL")
        .env("SERVER_PORT", port.to_string())
        .env("OPMUX_CONFIG_FILE", &catalog)
        .env("OPENAI_API_KEY", "test-dummy-openai-key")
        .env("OPENAI_BASE_URL", "http://127.0.0.1:9/v1");
    let (code, text) = run_to_exit(missing, Duration::from_secs(5));
    assert_ne!(code, Some(0));
    assert!(text.contains("missing_database_url"));
    assert!(!text.contains("postgresql://"));

    let port = unused_loopback_port();
    let mut invalid = gateway_command();
    invalid
        .env("DATABASE_URL", "not-a-postgres-url")
        .env("SERVER_PORT", port.to_string())
        .env("OPMUX_CONFIG_FILE", &catalog)
        .env("OPENAI_API_KEY", "test-dummy-openai-key")
        .env("OPENAI_BASE_URL", "http://127.0.0.1:9/v1");
    let (code, text) = run_to_exit(invalid, Duration::from_secs(5));
    assert_ne!(code, Some(0));
    assert!(text.contains("invalid_database_url"));
    assert!(!text.contains("not-a-postgres-url"));
}

#[test]
fn development_bypass_settings_do_not_admit_legacy_keys() {
    let catalog = example_catalog_path();
    let port = unused_loopback_port();
    let url = required_database_url();
    let mut command = gateway_command();
    command
        .env("DATABASE_URL", &url)
        .env("AUTH_DEVELOPMENT_MODE", "true")
        .env("AUTH_DEV_CLIENT_ID", "forged-dev-client")
        .env("SERVER_PORT", port.to_string())
        .env("OPMUX_CONFIG_FILE", &catalog)
        .env("OPENAI_API_KEY", "test-dummy-openai-key")
        .env("OPENAI_BASE_URL", "http://127.0.0.1:9/v1")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = command.spawn().expect("spawn gateway");
    let healthy = wait_health(port, &mut child);
    if !healthy {
        let _ = child.kill();
        let output = child.wait_with_output().expect("wait");
        let text = format!(
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert_ne!(output.status.code(), Some(0), "process may reject startup");
        assert!(!text.contains("Authentication is BYPASSED"));
        return;
    }
    let header = format!("X-API-Key: {MOCK_GATEWAY_API_KEY}");
    let (status, body) = curl_route(port, &["-H", &header]);
    let _ = child.kill();
    let _ = child.wait();
    assert_eq!(status, 401);
    assert!(!body.contains(MOCK_GATEWAY_API_KEY));
    assert!(!body.contains("forged-dev-client"));
}

#[test]
fn database_outage_keeps_process_live_and_fails_closed() {
    let catalog = example_catalog_path();
    let port = unused_loopback_port();
    let db_port = unused_loopback_port();
    let mut command = gateway_command();
    command
        .env(
            "DATABASE_URL",
            format!("postgres://127.0.0.1:{db_port}/postgres"),
        )
        .env("OPMUX_DB_ACQUIRE_TIMEOUT_MS", "100")
        .env("SERVER_PORT", port.to_string())
        .env("OPMUX_CONFIG_FILE", &catalog)
        .env("OPENAI_API_KEY", "test-dummy-openai-key")
        .env("OPENAI_BASE_URL", "http://127.0.0.1:9/v1")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = command.spawn().expect("spawn gateway");
    let healthy = wait_health(port, &mut child);
    if !healthy {
        let _ = child.kill();
        let output = child.wait_with_output().expect("wait");
        panic!(
            "gateway with valid catalog must remain live during database outage; status={:?}",
            output.status.code()
        );
    }
    let (status, body) = curl_route(port, &["-H", "X-API-Key: opmx_v1_outage-key"]);
    let _ = child.kill();
    let _ = child.wait();
    assert_eq!(status, 503);
    assert!(!body.contains("opmx_v1_outage-key"));
    assert!(!body.to_lowercase().contains("postgres"));
}

#[test]
fn auth_sources_skip_secrets_and_do_not_cache() {
    let service = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/src/features/auth/service.rs"
    ));
    let middleware = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/src/middleware/auth.rs"
    ));
    let implementation = service.split("mod tests").next().expect("impl");
    assert!(
        !implementation.contains("tokio::spawn"),
        "last-used must be synchronous"
    );
    assert!(
        !implementation.to_lowercase().contains("cache"),
        "authentication must not introduce a cache"
    );
    assert!(
        !middleware.contains("create_dev_context"),
        "development bypass must not remain in middleware"
    );
}

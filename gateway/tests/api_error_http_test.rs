//! Canonical protected-API error envelope through the production router.
//!
//! Evidence omits credentials, prompts, metadata, SQL, and provider bodies.

mod support;

use axum::{
    body::Body,
    http::{header, HeaderMap, Request, StatusCode},
};
use gateway::{
    app::Application,
    core::{config::Settings, db::DatabasePoolConfig, metrics::MetricsConfig},
    features::auth::{
        hash_credential, ApiKeyKind, AuthService, PostgresAuthStore, ProvisioningService,
    },
};
use serial_test::serial;
use std::io::Write;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use support::{
    cleanup_clients, isolate_provider_environment, min_padded_chat_completion_len,
    padded_chat_completion_bytes, production_router_with_auth, test_pool,
    OpenAiSimulator, ScriptedResponse,
};
use tower::ServiceExt;
use uuid::Uuid;

const PROMPT_SENTINEL: &str = "PROMPT_ERROR_SENTINEL_DO_NOT_ECHO";
const METADATA_SENTINEL: &str = "METADATA_ERROR_SENTINEL_DO_NOT_ECHO";
const UPSTREAM_BODY_SENTINEL: &str = "UPSTREAM_BODY_SENTINEL_DO_NOT_ECHO";
const DB_SECRET_SENTINEL: &str = "SENTINEL_DB_SECRET";
const ROUTE_BODY: &str = r#"{"prompt":"error-envelope","metadata":{}}"#;

#[allow(dead_code)]
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
            .create_tenant(&format!("err-a-{}", Uuid::new_v4().simple()))
            .await
            .expect("tenant a");
        let created_b = service
            .create_tenant(&format!("err-b-{}", Uuid::new_v4().simple()))
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

    fn auth_service(&self) -> Arc<AuthService> {
        Arc::new(AuthService::new(Arc::new(PostgresAuthStore::new(
            self.pool.clone(),
        ))))
    }

    fn client_ids(&self) -> [Uuid; 2] {
        [self.a_management.client_id, self.b_management.client_id]
    }

    async fn drop_rows(&self) {
        cleanup_clients(&self.pool, &self.client_ids()).await;
    }
}

struct DebugCapture {
    buf: Arc<Mutex<Vec<u8>>>,
}

struct CaptureWriter(Arc<Mutex<Vec<u8>>>);

impl Write for CaptureWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().expect("capture lock").extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl DebugCapture {
    fn install() -> (Self, tracing::subscriber::DefaultGuard) {
        let buf = Arc::new(Mutex::new(Vec::new()));
        let writer = buf.clone();
        let subscriber = tracing_subscriber::fmt()
            .with_ansi(false)
            .with_env_filter(tracing_subscriber::EnvFilter::new("gateway=debug"))
            .with_writer(move || CaptureWriter(writer.clone()))
            .finish();
        let guard = tracing::subscriber::set_default(subscriber);
        (Self { buf }, guard)
    }

    fn take(&self) -> String {
        let mut buf = self.buf.lock().expect("capture lock");
        let text = String::from_utf8(buf.clone()).unwrap_or_default();
        buf.clear();
        text
    }
}

fn no_retry_router(
    simulator: &OpenAiSimulator,
    auth_service: Arc<AuthService>,
) -> axum::Router {
    let mut settings =
        Settings::for_tests_with_provider(simulator.base_url(), simulator.credential());
    settings.limits.retries_per_target = 0;
    Application::from_settings(Arc::new(settings), auth_service)
        .expect("application should build")
        .into_router(MetricsConfig::disabled())
}

fn bounded_router(
    simulator: &OpenAiSimulator,
    auth_service: Arc<AuthService>,
    max_upstream_response_bytes: u64,
) -> axum::Router {
    let mut settings =
        Settings::for_tests_with_provider(simulator.base_url(), simulator.credential());
    settings.limits.retries_per_target = 0;
    settings.limits.max_upstream_response_bytes = max_upstream_response_bytes;
    Application::from_settings(Arc::new(settings), auth_service)
        .expect("application should build")
        .into_router(MetricsConfig::disabled())
}

async fn send(app: axum::Router, request: Request<Body>) -> axum::http::Response<Body> {
    app.oneshot(request).await.unwrap()
}

async fn json_of(response: axum::http::Response<Body>) -> serde_json::Value {
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("body");
    serde_json::from_slice(&bytes).expect("json envelope")
}

fn assert_envelope(
    status: StatusCode,
    headers: &HeaderMap,
    body: &serde_json::Value,
    expected_status: StatusCode,
    code: &str,
) {
    assert_eq!(status, expected_status);
    let content_type = headers
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default();
    assert!(
        content_type.starts_with("application/json"),
        "content-type {content_type}"
    );
    assert_eq!(body["error"]["code"], code);
    let message = body["error"]["message"].as_str().unwrap_or_default();
    assert!(!message.is_empty());
    let request_id = body["error"]["request_id"].as_str().unwrap_or_default();
    assert!(!request_id.is_empty());
    let header_id = headers
        .get("X-Request-ID")
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default();
    assert_eq!(request_id, header_id);
    assert!(body.get("response").is_none());
    assert!(body.get("keys").is_none());
    assert!(body.get("credential").is_none());
    assert!(body.get("model_used").is_none());
}

fn omit_sentinels(haystack: &str) {
    for sentinel in [
        PROMPT_SENTINEL,
        METADATA_SENTINEL,
        UPSTREAM_BODY_SENTINEL,
        DB_SECRET_SENTINEL,
        "postgres://",
        "Bearer ",
    ] {
        assert!(
            !haystack.contains(sentinel),
            "diagnostic content must omit {sentinel}"
        );
    }
}

fn owning_terminal_events(logs: &str) -> Vec<&str> {
    logs.lines()
        .filter(|line| line.contains("request failed") && line.contains("error_code"))
        .collect()
}

fn assert_one_owning_terminal_event(logs: &str, request_id: &str, error_code: &str) {
    let events = owning_terminal_events(logs);
    assert_eq!(
        events.len(),
        1,
        "expected one owning HTTP-boundary terminal event, found {}: {logs}",
        events.len()
    );
    let event = events[0];
    assert!(
        event.contains(error_code),
        "terminal event missing category {error_code}: {event}"
    );
    assert!(
        event.contains(request_id),
        "terminal event missing request_id: {event}"
    );
}

fn assert_no_executor_terminal_summaries(logs: &str) {
    for needle in [
        "Non-retryable error",
        "Max retries exceeded",
        "No fallback plans available",
        "returning primary error",
    ] {
        assert!(
            !logs.contains(needle),
            "executor must not emit redundant terminal summary {needle}: {logs}"
        );
    }
    for needle in [
        "error=AuthenticationFailed",
        "error=ApiCallFailed",
        "error=NetworkError",
        "error=RateLimitExceeded",
        "error=TimeoutError",
        "error=InvalidUpstreamResult",
        "error=JsonError",
        "error=UpstreamRejected",
    ] {
        assert!(
            !logs.contains(needle),
            "executor must not echo whole errors on terminal failure paths: {logs}"
        );
    }
}

fn retry_once_router(
    simulator: &OpenAiSimulator,
    auth_service: Arc<AuthService>,
) -> axum::Router {
    let mut settings =
        Settings::for_tests_with_provider(simulator.base_url(), simulator.credential());
    settings.limits.retries_per_target = 1;
    Application::from_settings(Arc::new(settings), auth_service)
        .expect("application should build")
        .into_router(MetricsConfig::disabled())
}

fn route_request(api_key: Option<&str>, body: impl Into<String>) -> Request<Body> {
    let mut builder = Request::builder()
        .method("POST")
        .uri("/api/v1/route")
        .header("content-type", "application/json");
    if let Some(key) = api_key {
        builder = builder.header("x-api-key", key);
    }
    builder.body(Body::from(body.into())).unwrap()
}

#[tokio::test]
#[serial]
async fn input_and_authorization_errors_share_canonical_envelope() {
    isolate_provider_environment();
    let fixture = TwoTenantFixture::provision().await;
    let simulator = OpenAiSimulator::start().await;
    let app = production_router_with_auth(
        &simulator,
        fixture.auth_service(),
        MetricsConfig::disabled(),
    );

    let missing = send(app.clone(), route_request(None, ROUTE_BODY)).await;
    let missing_body = json_of(missing).await;
    let missing_headers = send(app.clone(), route_request(None, ROUTE_BODY)).await;
    let missing_repeat = json_of(missing_headers).await;
    assert_eq!(missing_body["error"]["code"], "UNAUTHORIZED");
    assert_eq!(missing_repeat["error"]["code"], "UNAUTHORIZED");

    let unknown = send(
        app.clone(),
        route_request(Some("opmx_v1_unknown-key"), ROUTE_BODY),
    )
    .await;
    let unknown_status = unknown.status();
    let unknown_headers = unknown.headers().clone();
    let unknown_body = json_of(unknown).await;
    assert_envelope(
        unknown_status,
        &unknown_headers,
        &unknown_body,
        StatusCode::UNAUTHORIZED,
        "UNAUTHORIZED",
    );
    assert_eq!(unknown_body["error"]["code"], missing_body["error"]["code"]);

    let capability = send(
        app.clone(),
        route_request(Some(&fixture.a_management.credential), ROUTE_BODY),
    )
    .await;
    let capability_status = capability.status();
    let capability_headers = capability.headers().clone();
    let capability_body = json_of(capability).await;
    assert_envelope(
        capability_status,
        &capability_headers,
        &capability_body,
        StatusCode::FORBIDDEN,
        "FORBIDDEN",
    );

    let inference_create = send(
        app.clone(),
        Request::builder()
            .method("POST")
            .uri("/api/v1/auth/keys")
            .header("content-type", "application/json")
            .header("x-api-key", &fixture.a_inference.credential)
            .body(Body::from(r#"{"name":"x","kind":"inference"}"#))
            .unwrap(),
    )
    .await;
    assert_eq!(inference_create.status(), StatusCode::FORBIDDEN);
    let inference_create_body = json_of(inference_create).await;
    assert_eq!(inference_create_body["error"]["code"], "FORBIDDEN");

    let invalid_control = send(
        app.clone(),
        route_request(
            Some(&fixture.a_inference.credential),
            serde_json::json!({
                "prompt": "hello",
                "metadata": {},
                "parameters": { "unknown_param": 1 }
            })
            .to_string(),
        ),
    )
    .await;
    let invalid_status = invalid_control.status();
    let invalid_headers = invalid_control.headers().clone();
    let invalid_body = json_of(invalid_control).await;
    assert_envelope(
        invalid_status,
        &invalid_headers,
        &invalid_body,
        StatusCode::BAD_REQUEST,
        "INVALID_REQUEST",
    );

    let unknown_route = send(
        app.clone(),
        route_request(
            Some(&fixture.a_inference.credential),
            serde_json::json!({
                "prompt": "hello",
                "metadata": {},
                "route": "does-not-exist"
            })
            .to_string(),
        ),
    )
    .await;
    assert_eq!(unknown_route.status(), StatusCode::BAD_REQUEST);
    let unknown_route_body = json_of(unknown_route).await;
    assert_eq!(unknown_route_body["error"]["code"], "INVALID_REQUEST");

    let create_invalid = send(
        app.clone(),
        Request::builder()
            .method("POST")
            .uri("/api/v1/auth/keys")
            .header("content-type", "application/json")
            .header("x-api-key", &fixture.a_management.credential)
            .body(Body::from(r#"{"name":"x","kind":"admin"}"#))
            .unwrap(),
    )
    .await;
    assert_eq!(create_invalid.status(), StatusCode::BAD_REQUEST);
    let create_invalid_body = json_of(create_invalid).await;
    assert_eq!(create_invalid_body["error"]["code"], "INVALID_REQUEST");

    let list_missing = send(
        app.clone(),
        Request::builder()
            .method("GET")
            .uri("/api/v1/auth/keys")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(list_missing.status(), StatusCode::UNAUTHORIZED);
    let list_missing_body = json_of(list_missing).await;
    assert_eq!(list_missing_body["error"]["code"], "UNAUTHORIZED");

    let malformed = send(
        app.clone(),
        Request::builder()
            .method("POST")
            .uri("/api/v1/route")
            .header("content-type", "application/json")
            .header("x-api-key", &fixture.a_inference.credential)
            .body(Body::from(format!(
                "{{\"prompt\":\"{PROMPT_SENTINEL}\", invalid"
            )))
            .unwrap(),
    )
    .await;
    let malformed_status = malformed.status();
    let malformed_headers = malformed.headers().clone();
    let malformed_body = json_of(malformed).await;
    assert_envelope(
        malformed_status,
        &malformed_headers,
        &malformed_body,
        StatusCode::BAD_REQUEST,
        "INVALID_JSON",
    );
    omit_sentinels(&malformed_body.to_string());

    let media = send(
        app.clone(),
        Request::builder()
            .method("POST")
            .uri("/api/v1/route")
            .header("content-type", "text/plain")
            .header("x-api-key", &fixture.a_inference.credential)
            .body(Body::from(ROUTE_BODY))
            .unwrap(),
    )
    .await;
    assert_eq!(media.status(), StatusCode::UNSUPPORTED_MEDIA_TYPE);
    let media_body = json_of(media).await;
    assert_eq!(media_body["error"]["code"], "UNSUPPORTED_MEDIA_TYPE");

    let bad_path = send(
        app.clone(),
        Request::builder()
            .method("DELETE")
            .uri("/api/v1/auth/keys/not-a-uuid")
            .header("x-api-key", &fixture.a_management.credential)
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(bad_path.status(), StatusCode::BAD_REQUEST);
    let bad_path_body = json_of(bad_path).await;
    assert_eq!(bad_path_body["error"]["code"], "INVALID_PATH");
    assert!(!bad_path_body.to_string().contains("not-a-uuid"));

    let other_tenant = send(
        app.clone(),
        Request::builder()
            .method("DELETE")
            .uri(format!("/api/v1/auth/keys/{}", fixture.b_inference.key_id))
            .header("x-api-key", &fixture.a_management.credential)
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    let absent = send(
        app.clone(),
        Request::builder()
            .method("DELETE")
            .uri(format!("/api/v1/auth/keys/{}", Uuid::new_v4()))
            .header("x-api-key", &fixture.a_management.credential)
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(other_tenant.status(), StatusCode::NOT_FOUND);
    assert_eq!(absent.status(), StatusCode::NOT_FOUND);
    let other_body = json_of(other_tenant).await;
    let absent_body = json_of(absent).await;
    assert_eq!(other_body["error"]["code"], "NOT_FOUND");
    assert_eq!(absent_body["error"]["code"], "NOT_FOUND");
    assert_eq!(
        other_body["error"]["message"],
        absent_body["error"]["message"]
    );
    assert_ne!(
        other_body["error"]["request_id"],
        absent_body["error"]["request_id"]
    );
    assert_eq!(simulator.generation_count(), 0);
    fixture.drop_rows().await;
}

#[tokio::test]
#[serial]
async fn dependency_and_upstream_errors_are_not_gateway_key_failures() {
    isolate_provider_environment();
    let fixture = TwoTenantFixture::provision().await;
    let simulator = OpenAiSimulator::start().await;
    let app = no_retry_router(&simulator, fixture.auth_service());

    simulator.enqueue_chat(ScriptedResponse::json_status(
        401,
        serde_json::json!({"error":{"message":UPSTREAM_BODY_SENTINEL,"type":"invalid_api_key"}}),
    ));
    simulator.enqueue_chat(ScriptedResponse::json_status(
        403,
        serde_json::json!({"error":{"message":UPSTREAM_BODY_SENTINEL}}),
    ));
    simulator.enqueue_chat(ScriptedResponse::json_status(
        400,
        serde_json::json!({"error":{"message":UPSTREAM_BODY_SENTINEL,"code":"bad_request"}}),
    ));
    simulator.enqueue_chat(ScriptedResponse::raw_json_bytes(
        format!("{{not-json {UPSTREAM_BODY_SENTINEL}").into_bytes(),
    ));
    simulator.enqueue_chat(ScriptedResponse::Json {
        status: 429,
        body: serde_json::json!({"error":{"message":UPSTREAM_BODY_SENTINEL}}),
        retry_after: Some("2".to_string()),
    });

    let auth_401 = send(
        app.clone(),
        route_request(Some(&fixture.a_inference.credential), ROUTE_BODY),
    )
    .await;
    let auth_401_status = auth_401.status();
    let auth_401_headers = auth_401.headers().clone();
    let auth_401_body = json_of(auth_401).await;
    assert_envelope(
        auth_401_status,
        &auth_401_headers,
        &auth_401_body,
        StatusCode::BAD_GATEWAY,
        "UPSTREAM_AUTHENTICATION",
    );
    omit_sentinels(&auth_401_body.to_string());

    let auth_403 = send(
        app.clone(),
        route_request(Some(&fixture.a_inference.credential), ROUTE_BODY),
    )
    .await;
    assert_eq!(auth_403.status(), StatusCode::BAD_GATEWAY);
    let auth_403_body = json_of(auth_403).await;
    assert_eq!(auth_403_body["error"]["code"], "UPSTREAM_AUTHENTICATION");
    omit_sentinels(&auth_403_body.to_string());

    let rejected = send(
        app.clone(),
        route_request(Some(&fixture.a_inference.credential), ROUTE_BODY),
    )
    .await;
    assert_eq!(rejected.status(), StatusCode::BAD_GATEWAY);
    let rejected_body = json_of(rejected).await;
    assert_eq!(rejected_body["error"]["code"], "UPSTREAM_ERROR");
    omit_sentinels(&rejected_body.to_string());

    let malformed = send(
        app.clone(),
        route_request(Some(&fixture.a_inference.credential), ROUTE_BODY),
    )
    .await;
    assert_eq!(malformed.status(), StatusCode::BAD_GATEWAY);
    let malformed_body = json_of(malformed).await;
    assert_eq!(malformed_body["error"]["code"], "UPSTREAM_PROTOCOL");
    omit_sentinels(&malformed_body.to_string());

    let throttled = send(
        app.clone(),
        route_request(Some(&fixture.a_inference.credential), ROUTE_BODY),
    )
    .await;
    assert_eq!(throttled.status(), StatusCode::TOO_MANY_REQUESTS);
    let throttled_body = json_of(throttled).await;
    assert_eq!(throttled_body["error"]["code"], "UPSTREAM_RATE_LIMIT");
    assert_ne!(throttled_body["error"]["code"], "UNAUTHORIZED");
    omit_sentinels(&throttled_body.to_string());

    let bound = min_padded_chat_completion_len() + 8;
    let over = padded_chat_completion_bytes(bound + 1);
    let size_sim = OpenAiSimulator::start().await;
    size_sim.enqueue_chat(ScriptedResponse::raw_json_bytes(over));
    let size_app = bounded_router(&size_sim, fixture.auth_service(), bound as u64);
    let oversized = send(
        size_app,
        route_request(Some(&fixture.a_inference.credential), ROUTE_BODY),
    )
    .await;
    assert_eq!(oversized.status(), StatusCode::BAD_GATEWAY);
    assert_ne!(oversized.status(), StatusCode::PAYLOAD_TOO_LARGE);
    let oversized_body = json_of(oversized).await;
    assert_eq!(oversized_body["error"]["code"], "UPSTREAM_PROTOCOL");
    assert!(!oversized_body.to_string().contains("aaaaaaaa"));

    let closed = DatabasePoolConfig::new(format!(
        "postgres://opmux:{DB_SECRET_SENTINEL}@127.0.0.1:1/postgres"
    ))
    .expect("parseable closed-port url")
    .with_max_connections(1)
    .expect("pool size")
    .with_acquire_timeout(Duration::from_millis(200))
    .expect("short acquire");
    let down_auth = Arc::new(AuthService::new(Arc::new(PostgresAuthStore::new(
        closed.connect_lazy().expect("lazy pool"),
    ))));
    let down_sim = OpenAiSimulator::start().await;
    let before = down_sim.generation_count();
    let down_app =
        production_router_with_auth(&down_sim, down_auth, MetricsConfig::disabled());
    let failed = send(
        down_app,
        route_request(Some(&fixture.a_inference.credential), ROUTE_BODY),
    )
    .await;
    let failed_status = failed.status();
    let failed_body = json_of(failed).await;
    assert_eq!(failed_status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(failed_body["error"]["code"], "AUTH_DEPENDENCY_UNAVAILABLE");
    assert_ne!(failed_status, StatusCode::UNAUTHORIZED);
    omit_sentinels(&failed_body.to_string());
    assert_eq!(down_sim.generation_count(), before);
    fixture.drop_rows().await;
}

#[tokio::test]
#[serial]
async fn correlation_survives_early_rejections_and_execution_failures() {
    isolate_provider_environment();
    let fixture = TwoTenantFixture::provision().await;
    let simulator = OpenAiSimulator::start().await;
    simulator.enqueue_chat(ScriptedResponse::json_status(
        500,
        serde_json::json!({"error":{"message":"simulated upstream"}}),
    ));
    let app = no_retry_router(&simulator, fixture.auth_service());
    let correlation = "corr-error-matrix-123";

    async fn with_corr(
        app: axum::Router,
        request: Request<Body>,
        correlation: Option<&str>,
    ) -> (axum::http::Response<Body>, serde_json::Value) {
        let (parts, body) = request.into_parts();
        let mut builder = Request::builder()
            .method(parts.method)
            .uri(parts.uri)
            .version(parts.version);
        for (name, value) in parts.headers.iter() {
            builder = builder.header(name, value);
        }
        if let Some(value) = correlation {
            builder = builder.header("X-Correlation-ID", value);
        }
        let request = builder.body(body).unwrap();
        let response = app.oneshot(request).await.unwrap();
        let status = response.status();
        let headers = response.headers().clone();
        let body = json_of(response).await;
        let mut rebuilt = axum::http::Response::new(Body::empty());
        *rebuilt.status_mut() = status;
        *rebuilt.headers_mut() = headers;
        (rebuilt, body)
    }

    let (missing, missing_body) = with_corr(
        app.clone(),
        route_request(None, ROUTE_BODY),
        Some(correlation),
    )
    .await;
    assert_eq!(missing.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(
        missing.headers().get("X-Request-ID").unwrap(),
        missing_body["error"]["request_id"].as_str().unwrap()
    );
    assert_eq!(
        missing.headers().get("X-Correlation-ID").unwrap(),
        correlation
    );
    assert!(!missing_body["error"]["code"]
        .as_str()
        .unwrap()
        .contains(correlation));
    assert!(!missing_body["error"]["message"]
        .as_str()
        .unwrap()
        .contains(correlation));

    let (invalid_json, invalid_json_body) = with_corr(
        app.clone(),
        Request::builder()
            .method("POST")
            .uri("/api/v1/route")
            .header("content-type", "application/json")
            .header("x-api-key", &fixture.a_inference.credential)
            .body(Body::from("{"))
            .unwrap(),
        Some(correlation),
    )
    .await;
    assert_eq!(invalid_json.status(), StatusCode::BAD_REQUEST);
    assert_eq!(
        invalid_json.headers().get("X-Request-ID").unwrap(),
        invalid_json_body["error"]["request_id"].as_str().unwrap()
    );
    assert_eq!(
        invalid_json.headers().get("X-Correlation-ID").unwrap(),
        correlation
    );

    let (invalid_route, invalid_route_body) = with_corr(
        app.clone(),
        route_request(
            Some(&fixture.a_inference.credential),
            serde_json::json!({
                "prompt": "hello",
                "metadata": {},
                "route": "missing"
            })
            .to_string(),
        ),
        Some(correlation),
    )
    .await;
    assert_eq!(invalid_route.status(), StatusCode::BAD_REQUEST);
    assert_eq!(
        invalid_route.headers().get("X-Request-ID").unwrap(),
        invalid_route_body["error"]["request_id"].as_str().unwrap()
    );

    let (upstream, upstream_body) = with_corr(
        app.clone(),
        route_request(Some(&fixture.a_inference.credential), ROUTE_BODY),
        Some(correlation),
    )
    .await;
    assert_eq!(upstream.status(), StatusCode::BAD_GATEWAY);
    assert_eq!(
        upstream.headers().get("X-Request-ID").unwrap(),
        upstream_body["error"]["request_id"].as_str().unwrap()
    );
    assert_eq!(
        upstream.headers().get("X-Correlation-ID").unwrap(),
        correlation
    );

    let (absent, absent_body) =
        with_corr(app.clone(), route_request(None, ROUTE_BODY), None).await;
    assert!(absent.headers().contains_key("X-Request-ID"));
    assert!(!absent.headers().contains_key("X-Correlation-ID"));
    assert_eq!(
        absent.headers().get("X-Request-ID").unwrap(),
        absent_body["error"]["request_id"].as_str().unwrap()
    );

    let long_id = "a".repeat(300);
    let (replaced, replaced_body) =
        with_corr(app.clone(), route_request(None, ROUTE_BODY), Some(&long_id)).await;
    assert!(replaced.headers().contains_key("X-Request-ID"));
    assert!(!replaced.headers().contains_key("X-Correlation-ID"));
    assert!(!replaced_body.to_string().contains(&long_id));
    assert_eq!(
        replaced.headers().get("X-Request-ID").unwrap(),
        replaced_body["error"]["request_id"].as_str().unwrap()
    );

    simulator.enqueue_chat(ScriptedResponse::chat_ok());
    let success = send(
        app,
        Request::builder()
            .method("POST")
            .uri("/api/v1/route")
            .header("content-type", "application/json")
            .header("x-api-key", &fixture.a_inference.credential)
            .header("X-Correlation-ID", correlation)
            .body(Body::from(ROUTE_BODY))
            .unwrap(),
    )
    .await;
    assert_eq!(success.status(), StatusCode::OK);
    assert!(success.headers().contains_key("X-Request-ID"));
    assert_eq!(
        success.headers().get("X-Correlation-ID").unwrap(),
        correlation
    );
    fixture.drop_rows().await;
}

#[tokio::test]
#[serial]
async fn unexpected_faults_and_diagnostics_remain_sanitized() {
    isolate_provider_environment();
    let (capture, _guard) = DebugCapture::install();
    let fixture = TwoTenantFixture::provision().await;
    let digest = hash_credential(&fixture.a_inference.credential);
    let simulator = OpenAiSimulator::start().await;
    simulator.enqueue_chat(ScriptedResponse::json_status(
        401,
        serde_json::json!({
            "error": {
                "message": UPSTREAM_BODY_SENTINEL,
                "api_key": "sk-sentinel-provider"
            }
        }),
    ));
    let app = no_retry_router(&simulator, fixture.auth_service());
    let body = serde_json::json!({
        "prompt": PROMPT_SENTINEL,
        "metadata": { "note": METADATA_SENTINEL }
    })
    .to_string();
    let response = send(
        app,
        route_request(Some(&fixture.a_inference.credential), body),
    )
    .await;
    let encoded_headers: String = response
        .headers()
        .iter()
        .filter_map(|(name, value)| {
            value.to_str().ok().map(|text| format!("{name}: {text}"))
        })
        .collect::<Vec<_>>()
        .join("\n");
    let status = response.status();
    let body = json_of(response).await;
    let logs = capture.take();
    assert_eq!(status, StatusCode::BAD_GATEWAY);
    assert_eq!(body["error"]["code"], "UPSTREAM_AUTHENTICATION");
    omit_sentinels(&body.to_string());
    omit_sentinels(&encoded_headers);
    omit_sentinels(&logs);
    assert!(!body.to_string().contains(&fixture.a_inference.credential));
    assert!(!logs.contains(&fixture.a_inference.credential));
    assert!(!logs.contains(&format!("{digest:?}")));
    assert!(!encoded_headers.contains(&fixture.a_inference.credential));
    let request_id = body["error"]["request_id"].as_str().unwrap_or_default();
    assert_one_owning_terminal_event(&logs, request_id, "UPSTREAM_AUTHENTICATION");
    assert_no_executor_terminal_summaries(&logs);
    assert!(
        !logs.contains("Retrying execution"),
        "nonretryable failure must not start a retry: {logs}"
    );

    let closed = DatabasePoolConfig::new(format!(
        "postgres://opmux:{DB_SECRET_SENTINEL}@127.0.0.1:1/postgres"
    ))
    .expect("parseable closed-port url")
    .with_max_connections(1)
    .expect("pool size")
    .with_acquire_timeout(Duration::from_millis(200))
    .expect("short acquire");
    let down_auth = Arc::new(AuthService::new(Arc::new(PostgresAuthStore::new(
        closed.connect_lazy().expect("lazy pool"),
    ))));
    let down_sim = OpenAiSimulator::start().await;
    let down_app =
        production_router_with_auth(&down_sim, down_auth, MetricsConfig::disabled());
    let failed = send(
        down_app,
        route_request(Some(&fixture.a_inference.credential), ROUTE_BODY),
    )
    .await;
    let failed_headers: String = failed
        .headers()
        .iter()
        .filter_map(|(name, value)| {
            value.to_str().ok().map(|text| format!("{name}: {text}"))
        })
        .collect::<Vec<_>>()
        .join("\n");
    let failed_body = json_of(failed).await;
    let failed_logs = capture.take();
    assert_eq!(failed_body["error"]["code"], "AUTH_DEPENDENCY_UNAVAILABLE");
    omit_sentinels(&failed_body.to_string());
    omit_sentinels(&failed_headers);
    omit_sentinels(&failed_logs);
    fixture.drop_rows().await;
}

#[tokio::test]
#[serial]
async fn exhausted_retry_without_fallback_has_one_owning_terminal_error_log() {
    isolate_provider_environment();
    let (capture, _guard) = DebugCapture::install();
    let fixture = TwoTenantFixture::provision().await;
    let digest = hash_credential(&fixture.a_inference.credential);
    let simulator = OpenAiSimulator::start().await;
    for _ in 0..2 {
        simulator.enqueue_chat(ScriptedResponse::json_status(
            500,
            serde_json::json!({
                "error": { "message": UPSTREAM_BODY_SENTINEL }
            }),
        ));
    }
    let app = retry_once_router(&simulator, fixture.auth_service());
    let body = serde_json::json!({
        "prompt": PROMPT_SENTINEL,
        "metadata": { "note": METADATA_SENTINEL }
    })
    .to_string();
    let response = send(
        app,
        route_request(Some(&fixture.a_inference.credential), body),
    )
    .await;
    let encoded_headers: String = response
        .headers()
        .iter()
        .filter_map(|(name, value)| {
            value.to_str().ok().map(|text| format!("{name}: {text}"))
        })
        .collect::<Vec<_>>()
        .join("\n");
    let status = response.status();
    let headers = response.headers().clone();
    let body = json_of(response).await;
    let logs = capture.take();
    assert_envelope(
        status,
        &headers,
        &body,
        StatusCode::BAD_GATEWAY,
        "UPSTREAM_ERROR",
    );
    omit_sentinels(&body.to_string());
    omit_sentinels(&encoded_headers);
    omit_sentinels(&logs);
    assert!(!body.to_string().contains(&fixture.a_inference.credential));
    assert!(!logs.contains(&fixture.a_inference.credential));
    assert!(!logs.contains(&format!("{digest:?}")));
    let request_id = body["error"]["request_id"].as_str().unwrap_or_default();
    assert_one_owning_terminal_event(&logs, request_id, "UPSTREAM_ERROR");
    assert_no_executor_terminal_summaries(&logs);
    assert!(
        logs.contains("Retryable error"),
        "exhausted retry should keep attempt-level retry telemetry: {logs}"
    );
    assert!(
        logs.contains("Retrying execution"),
        "exhausted retry should keep actual backoff/retry telemetry: {logs}"
    );
    assert_eq!(simulator.generation_count(), 2);
    fixture.drop_rows().await;
}

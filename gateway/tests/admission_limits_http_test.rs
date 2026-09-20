//! Raw-body, metadata, and concurrent generation admission through the production router.
//!
//! Evidence omits credentials, prompts, metadata, and raw provider bodies.

mod support;

use axum::{
    body::{Body, Bytes},
    http::{header, HeaderMap, Request, StatusCode},
};
use futures_util::stream;
use gateway::{
    core::metrics::MetricsConfig,
    features::auth::{ApiKeyKind, AuthService, PostgresAuthStore, ProvisioningService},
};
use serial_test::serial;
use std::sync::Arc;
use std::time::{Duration, Instant};
use support::{
    cleanup_clients, isolate_provider_environment, min_padded_chat_completion_len,
    padded_chat_completion_bytes, production_router_with_settings,
    settings_for_simulator_with, test_pool, OpenAiSimulator, ScriptedResponse,
};
use tokio::time::sleep;
use tower::ServiceExt;
use uuid::Uuid;

const RAW_BODY_BOUND: u64 = 192;
const METADATA_BOUND: u64 = 24;
const CORRELATION_ID: &str = "limit-corr-1";
const ROUTE_JSON: &str = r#"{"prompt":"ok","metadata":{}}"#;
const METADATA_OVERHEAD: usize = 8;

struct Issued {
    client_id: Uuid,
    credential: String,
}

struct LimitFixture {
    pool: sqlx::PgPool,
    management: Issued,
    inference: Issued,
}

impl LimitFixture {
    async fn provision() -> Self {
        let pool = test_pool().await;
        let service =
            ProvisioningService::new(Arc::new(PostgresAuthStore::new(pool.clone())));
        let created = service
            .create_tenant(&format!("limit-{}", Uuid::new_v4().simple()))
            .await
            .expect("tenant");
        let inference = service
            .issue_key(
                created.key.client_id,
                ApiKeyKind::Inference,
                "limit-inference",
            )
            .await
            .expect("inference key");
        Self {
            pool,
            management: Issued {
                client_id: created.key.client_id,
                credential: created.key.credential().to_string(),
            },
            inference: Issued {
                client_id: inference.client_id,
                credential: inference.credential().to_string(),
            },
        }
    }

    fn auth_service(&self) -> Arc<AuthService> {
        Arc::new(AuthService::new(Arc::new(PostgresAuthStore::new(
            self.pool.clone(),
        ))))
    }

    async fn drop_rows(&self) {
        cleanup_clients(&self.pool, &[self.management.client_id]).await;
    }
}

fn limits_router(
    simulator: &OpenAiSimulator,
    auth_service: Arc<AuthService>,
    mutate: impl FnOnce(&mut gateway::core::config::Settings),
) -> axum::Router {
    limits_router_with(simulator, auth_service, mutate, MetricsConfig::disabled())
}

fn limits_router_with(
    simulator: &OpenAiSimulator,
    auth_service: Arc<AuthService>,
    mutate: impl FnOnce(&mut gateway::core::config::Settings),
    metrics: MetricsConfig,
) -> axum::Router {
    production_router_with_settings(
        settings_for_simulator_with(simulator, mutate),
        auth_service,
        metrics,
    )
}

fn pad_json(base: &str, len: usize) -> String {
    assert!(
        base.len() <= len,
        "fixture JSON is already larger than the bound"
    );
    let mut body = String::from(base);
    body.push_str(&" ".repeat(len - base.len()));
    assert_eq!(body.len(), len);
    body
}

fn metadata_with_serialized_len(len: usize) -> serde_json::Value {
    assert!(len >= METADATA_OVERHEAD);
    let payload = "a".repeat(len - METADATA_OVERHEAD);
    let metadata = serde_json::json!({ "k": payload });
    let encoded = serde_json::to_vec(&metadata).expect("metadata json");
    assert_eq!(encoded.len(), len);
    metadata
}

fn stream_body(bytes: String, chunk_size: usize) -> Body {
    let data = Bytes::from(bytes);
    let chunk_size = chunk_size.max(1);
    let chunks: Vec<Result<Bytes, std::io::Error>> = data
        .chunks(chunk_size)
        .map(|chunk| Ok(Bytes::copy_from_slice(chunk)))
        .collect();
    Body::from_stream(stream::iter(chunks))
}

fn route_request(
    credential: &str,
    body: Body,
    content_length: Option<usize>,
) -> Request<Body> {
    json_request("/api/v1/route", credential, body, content_length)
}

fn create_key_request(
    credential: &str,
    body: Body,
    content_length: Option<usize>,
) -> Request<Body> {
    json_request("/api/v1/auth/keys", credential, body, content_length)
}

fn json_request(
    uri: &str,
    credential: &str,
    body: Body,
    content_length: Option<usize>,
) -> Request<Body> {
    let mut builder = Request::builder()
        .method("POST")
        .uri(uri)
        .header(header::CONTENT_TYPE, "application/json")
        .header("x-api-key", credential)
        .header("x-correlation-id", CORRELATION_ID);
    if let Some(len) = content_length {
        builder = builder.header(header::CONTENT_LENGTH, len.to_string());
    }
    builder.body(body).unwrap()
}

async fn send(app: axum::Router, request: Request<Body>) -> axum::http::Response<Body> {
    app.oneshot(request).await.unwrap()
}

async fn json_of(response: axum::http::Response<Body>) -> serde_json::Value {
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("body");
    serde_json::from_slice(&bytes).expect("json")
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
        .get("x-request-id")
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default();
    assert_eq!(request_id, header_id);
    assert_eq!(
        headers
            .get("x-correlation-id")
            .and_then(|value| value.to_str().ok()),
        Some(CORRELATION_ID)
    );
    assert!(body.get("response").is_none());
    assert!(body.get("credential").is_none());
    assert!(body.get("model_used").is_none());
}

async fn key_count(pool: &sqlx::PgPool, client_id: Uuid) -> i64 {
    sqlx::query_scalar("SELECT count(*) FROM opmux_private.api_keys WHERE client_id = $1")
        .bind(client_id)
        .fetch_one(pool)
        .await
        .expect("key count")
}

#[tokio::test]
#[serial]
async fn exact_declared_raw_body_is_accepted_and_over_limit_is_413_before_generation() {
    isolate_provider_environment();
    let fixture = LimitFixture::provision().await;
    let simulator = OpenAiSimulator::start().await;
    let app = limits_router(&simulator, fixture.auth_service(), |settings| {
        settings.limits.max_request_body_bytes = RAW_BODY_BOUND;
    });
    let bound = RAW_BODY_BOUND as usize;

    let exact = pad_json(ROUTE_JSON, bound);
    assert_eq!(exact.len() as u64, RAW_BODY_BOUND);
    let exact_response = send(
        app.clone(),
        route_request(
            &fixture.inference.credential,
            Body::from(exact.clone()),
            Some(exact.len()),
        ),
    )
    .await;
    assert_eq!(exact_response.status(), StatusCode::OK);
    assert_eq!(simulator.generation_count(), 1);

    let over = pad_json(ROUTE_JSON, bound + 1);
    assert_eq!(over.len() as u64, RAW_BODY_BOUND + 1);
    let over_response = send(
        app,
        route_request(
            &fixture.inference.credential,
            Body::from(over.clone()),
            Some(over.len()),
        ),
    )
    .await;
    let headers = over_response.headers().clone();
    let status = over_response.status();
    let body = json_of(over_response).await;
    assert_envelope(
        status,
        &headers,
        &body,
        StatusCode::PAYLOAD_TOO_LARGE,
        "PAYLOAD_TOO_LARGE",
    );
    assert_eq!(simulator.generation_count(), 1);

    fixture.drop_rows().await;
}

#[tokio::test]
#[serial]
async fn streamed_raw_body_bounds_match_declared_length_and_reject_before_generation() {
    isolate_provider_environment();
    let fixture = LimitFixture::provision().await;
    let simulator = OpenAiSimulator::start().await;
    let app = limits_router(&simulator, fixture.auth_service(), |settings| {
        settings.limits.max_request_body_bytes = RAW_BODY_BOUND;
    });
    let bound = RAW_BODY_BOUND as usize;

    let exact = pad_json(ROUTE_JSON, bound);
    let exact_response = send(
        app.clone(),
        route_request(&fixture.inference.credential, stream_body(exact, 16), None),
    )
    .await;
    assert_eq!(exact_response.status(), StatusCode::OK);
    assert_eq!(simulator.generation_count(), 1);

    let over = pad_json(ROUTE_JSON, bound + 1);
    let over_response = send(
        app,
        route_request(&fixture.inference.credential, stream_body(over, 16), None),
    )
    .await;
    let headers = over_response.headers().clone();
    let status = over_response.status();
    let body = json_of(over_response).await;
    assert_envelope(
        status,
        &headers,
        &body,
        StatusCode::PAYLOAD_TOO_LARGE,
        "PAYLOAD_TOO_LARGE",
    );
    assert_eq!(simulator.generation_count(), 1);

    fixture.drop_rows().await;
}

#[tokio::test]
#[serial]
async fn advertised_oversize_content_length_is_rejected_before_generation() {
    isolate_provider_environment();
    let fixture = LimitFixture::provision().await;
    let simulator = OpenAiSimulator::start().await;
    let app = limits_router(&simulator, fixture.auth_service(), |settings| {
        settings.limits.max_request_body_bytes = RAW_BODY_BOUND;
    });

    let response = send(
        app,
        route_request(
            &fixture.inference.credential,
            Body::from(ROUTE_JSON),
            Some((RAW_BODY_BOUND as usize) + 1),
        ),
    )
    .await;
    let headers = response.headers().clone();
    let status = response.status();
    let body = json_of(response).await;
    assert_envelope(
        status,
        &headers,
        &body,
        StatusCode::PAYLOAD_TOO_LARGE,
        "PAYLOAD_TOO_LARGE",
    );
    assert_eq!(simulator.generation_count(), 0);

    fixture.drop_rows().await;
}

#[tokio::test]
#[serial]
async fn key_create_raw_body_bounds_use_the_same_inclusive_limit() {
    isolate_provider_environment();
    let fixture = LimitFixture::provision().await;
    let simulator = OpenAiSimulator::start().await;
    let app = limits_router(&simulator, fixture.auth_service(), |settings| {
        settings.limits.max_request_body_bytes = RAW_BODY_BOUND;
    });
    let bound = RAW_BODY_BOUND as usize;
    let before = key_count(&fixture.pool, fixture.management.client_id).await;

    let exact_json = format!(
        r#"{{"name":"limit-{}","kind":"inference"}}"#,
        Uuid::new_v4().simple()
    );
    let exact = pad_json(&exact_json, bound);
    let exact_response = send(
        app.clone(),
        create_key_request(
            &fixture.management.credential,
            Body::from(exact.clone()),
            Some(exact.len()),
        ),
    )
    .await;
    assert_eq!(exact_response.status(), StatusCode::CREATED);
    assert_eq!(
        key_count(&fixture.pool, fixture.management.client_id).await,
        before + 1
    );

    let over_json = format!(
        r#"{{"name":"limit-{}","kind":"inference"}}"#,
        Uuid::new_v4().simple()
    );
    let over = pad_json(&over_json, bound + 1);
    let over_response = send(
        app.clone(),
        create_key_request(
            &fixture.management.credential,
            Body::from(over.clone()),
            Some(over.len()),
        ),
    )
    .await;
    let headers = over_response.headers().clone();
    let status = over_response.status();
    let body = json_of(over_response).await;
    assert_envelope(
        status,
        &headers,
        &body,
        StatusCode::PAYLOAD_TOO_LARGE,
        "PAYLOAD_TOO_LARGE",
    );
    assert_eq!(
        key_count(&fixture.pool, fixture.management.client_id).await,
        before + 1
    );

    let streamed_over = pad_json(
        &format!(
            r#"{{"name":"limit-{}","kind":"inference"}}"#,
            Uuid::new_v4().simple()
        ),
        bound + 1,
    );
    let streamed = send(
        app,
        create_key_request(
            &fixture.management.credential,
            stream_body(streamed_over, 16),
            None,
        ),
    )
    .await;
    assert_eq!(streamed.status(), StatusCode::PAYLOAD_TOO_LARGE);
    assert_eq!(
        key_count(&fixture.pool, fixture.management.client_id).await,
        before + 1
    );
    assert_eq!(simulator.generation_count(), 0);

    fixture.drop_rows().await;
}

#[tokio::test]
#[serial]
async fn metadata_serialized_byte_bound_is_inclusive_and_rejects_before_generation() {
    isolate_provider_environment();
    let fixture = LimitFixture::provision().await;
    let simulator = OpenAiSimulator::start().await;
    let app = limits_router(&simulator, fixture.auth_service(), |settings| {
        settings.limits.max_metadata_bytes = METADATA_BOUND;
    });

    let exact_metadata = metadata_with_serialized_len(METADATA_BOUND as usize);
    assert_eq!(
        serde_json::to_vec(&exact_metadata)
            .expect("exact metadata")
            .len() as u64,
        METADATA_BOUND
    );
    let exact_response = send(
        app.clone(),
        route_request(
            &fixture.inference.credential,
            Body::from(
                serde_json::json!({
                    "prompt": "ok",
                    "metadata": exact_metadata
                })
                .to_string(),
            ),
            None,
        ),
    )
    .await;
    assert_eq!(exact_response.status(), StatusCode::OK);
    assert_eq!(simulator.generation_count(), 1);

    let over_metadata = metadata_with_serialized_len((METADATA_BOUND as usize) + 1);
    assert!(
        serde_json::to_vec(&over_metadata)
            .expect("over metadata")
            .len() as u64
            > METADATA_BOUND
    );
    let over_response = send(
        app,
        route_request(
            &fixture.inference.credential,
            Body::from(
                serde_json::json!({
                    "prompt": "ok",
                    "metadata": over_metadata
                })
                .to_string(),
            ),
            None,
        ),
    )
    .await;
    let headers = over_response.headers().clone();
    let status = over_response.status();
    let body = json_of(over_response).await;
    assert_envelope(
        status,
        &headers,
        &body,
        StatusCode::BAD_REQUEST,
        "INVALID_REQUEST",
    );
    let message = body["error"]["message"].as_str().unwrap_or_default();
    assert!(message.contains("Metadata exceeds maximum size"));
    assert!(message.contains(&METADATA_BOUND.to_string()));
    assert_eq!(simulator.generation_count(), 1);

    fixture.drop_rows().await;
}

#[tokio::test]
#[serial]
async fn small_protected_body_limit_does_not_restrict_health_probes() {
    isolate_provider_environment();
    let fixture = LimitFixture::provision().await;
    let simulator = OpenAiSimulator::start().await;
    let app = limits_router(&simulator, fixture.auth_service(), |settings| {
        settings.limits.max_request_body_bytes = RAW_BODY_BOUND;
    });

    let health = send(
        app.clone(),
        Request::builder()
            .uri("/health")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(health.status(), StatusCode::OK);

    let ready = send(
        app,
        Request::builder()
            .uri("/ready")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(ready.status(), StatusCode::OK);
    assert_eq!(simulator.generation_count(), 0);

    fixture.drop_rows().await;
}

const CONCURRENCY_N: u32 = 2;
const OVERLOAD_PROMPT_BOUND: Duration = Duration::from_millis(400);
const GENERATION_WAIT_BOUND: Duration = Duration::from_secs(2);
const PROBE_BOUND: Duration = Duration::from_secs(2);

fn route_json_request(credential: &str) -> Request<Body> {
    route_request(credential, Body::from(ROUTE_JSON), None)
}

async fn wait_generation_at_least(simulator: &OpenAiSimulator, expected: usize) {
    let started = Instant::now();
    while simulator.generation_count() < expected {
        assert!(
            started.elapsed() < GENERATION_WAIT_BOUND,
            "timed out waiting for {expected} generation calls, saw {}",
            simulator.generation_count()
        );
        sleep(Duration::from_millis(5)).await;
    }
}

async fn spawn_route(
    app: axum::Router,
    credential: String,
) -> tokio::task::JoinHandle<axum::http::Response<Body>> {
    tokio::spawn(async move { send(app, route_json_request(&credential)).await })
}

async fn get_uri(app: axum::Router, uri: &str) -> axum::http::Response<Body> {
    send(
        app,
        Request::builder().uri(uri).body(Body::empty()).unwrap(),
    )
    .await
}

fn assert_overloaded(status: StatusCode, headers: &HeaderMap, body: &serde_json::Value) {
    assert_envelope(
        status,
        headers,
        body,
        StatusCode::TOO_MANY_REQUESTS,
        "OVERLOADED",
    );
    assert_ne!(body["error"]["code"], "UPSTREAM_RATE_LIMIT");
    let retry_after = headers
        .get(header::RETRY_AFTER)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default();
    assert_eq!(retry_after, "1");
    let parsed: u64 = retry_after.parse().expect("retry-after seconds");
    assert!(parsed > 0);
}

#[tokio::test]
#[serial]
async fn concurrent_generations_cap_rejects_overload_without_queuing() {
    isolate_provider_environment();
    let fixture = LimitFixture::provision().await;
    let simulator = OpenAiSimulator::start().await;
    let app = limits_router_with(
        &simulator,
        fixture.auth_service(),
        |settings| {
            settings.limits.max_concurrent_generations = CONCURRENCY_N;
            settings.limits.retries_per_target = 0;
        },
        MetricsConfig::production(),
    );

    let mut holds = Vec::new();
    let mut admitted = Vec::new();
    for _ in 0..CONCURRENCY_N {
        let (held, hold) = ScriptedResponse::chat_ok().hold();
        simulator.enqueue_chat(held);
        holds.push(hold);
        admitted
            .push(spawn_route(app.clone(), fixture.inference.credential.clone()).await);
    }
    wait_generation_at_least(&simulator, CONCURRENCY_N as usize).await;
    assert_eq!(simulator.generation_count(), CONCURRENCY_N as usize);

    let extra_started = Instant::now();
    let extra = tokio::time::timeout(
        OVERLOAD_PROMPT_BOUND,
        send(
            app.clone(),
            route_json_request(&fixture.inference.credential),
        ),
    )
    .await
    .expect("local overload should reject without waiting for a slot");
    assert!(
        extra_started.elapsed() < OVERLOAD_PROMPT_BOUND,
        "overload waited {:?}",
        extra_started.elapsed()
    );
    let extra_headers = extra.headers().clone();
    let extra_status = extra.status();
    let extra_body = json_of(extra).await;
    assert_overloaded(extra_status, &extra_headers, &extra_body);
    assert_eq!(simulator.generation_count(), CONCURRENCY_N as usize);

    let probe_started = Instant::now();
    let health = tokio::time::timeout(PROBE_BOUND, get_uri(app.clone(), "/health"))
        .await
        .expect("health should stay responsive");
    assert_eq!(health.status(), StatusCode::OK);
    let ready = tokio::time::timeout(PROBE_BOUND, get_uri(app.clone(), "/ready"))
        .await
        .expect("ready should stay responsive");
    assert_eq!(ready.status(), StatusCode::OK);
    let metrics = tokio::time::timeout(PROBE_BOUND, get_uri(app.clone(), "/metrics"))
        .await
        .expect("metrics should stay responsive");
    assert_eq!(metrics.status(), StatusCode::OK);
    let metrics_body = axum::body::to_bytes(metrics.into_body(), usize::MAX)
        .await
        .expect("metrics body");
    assert!(!metrics_body.is_empty());
    assert!(probe_started.elapsed() < PROBE_BOUND);

    for hold in holds {
        hold.release();
    }
    for task in admitted {
        let response = task.await.expect("admitted join");
        assert_eq!(response.status(), StatusCode::OK);
    }

    let follow_up = send(app, route_json_request(&fixture.inference.credential)).await;
    assert_eq!(follow_up.status(), StatusCode::OK);
    assert_eq!(simulator.generation_count(), CONCURRENCY_N as usize + 1);

    fixture.drop_rows().await;
}

#[tokio::test]
#[serial]
async fn permit_covers_retry_backoff_and_fallback() {
    isolate_provider_environment();
    let fixture = LimitFixture::provision().await;
    let simulator = OpenAiSimulator::start().await;

    let retry_app = limits_router(&simulator, fixture.auth_service(), |settings| {
        settings.limits.max_concurrent_generations = 1;
        settings.limits.retries_per_target = 1;
        settings.limits.max_total_attempts = 3;
    });
    let (retry_held, retry_hold) = ScriptedResponse::chat_ok().hold();
    simulator.enqueue_chat(ScriptedResponse::rate_limited("1"));
    simulator.enqueue_chat(retry_held);
    let retry_task =
        spawn_route(retry_app.clone(), fixture.inference.credential.clone()).await;
    wait_generation_at_least(&simulator, 1).await;
    let retry_extra = tokio::time::timeout(
        OVERLOAD_PROMPT_BOUND,
        send(retry_app, route_json_request(&fixture.inference.credential)),
    )
    .await
    .expect("retry backoff must keep the generation permit");
    let retry_headers = retry_extra.headers().clone();
    let retry_status = retry_extra.status();
    let retry_body = json_of(retry_extra).await;
    assert_overloaded(retry_status, &retry_headers, &retry_body);
    assert_eq!(simulator.generation_count(), 1);
    retry_task.abort();
    let _ = retry_task.await;
    retry_hold.release();
    simulator.clear_chat_script();

    let fallback_app = limits_router(&simulator, fixture.auth_service(), |settings| {
        settings.limits.max_concurrent_generations = 1;
        settings.limits.retries_per_target = 0;
        settings.limits.max_total_attempts = 3;
        settings
            .catalog
            .targets
            .get_mut("secondary")
            .expect("secondary target")
            .max_output_tokens = 512;
        settings
            .catalog
            .routes
            .get_mut("default")
            .expect("default route")
            .fallbacks
            .push("secondary".to_string());
    });
    let (fallback_held, fallback_hold) = ScriptedResponse::chat_ok().hold();
    simulator.enqueue_chat(ScriptedResponse::json_status(
        500,
        serde_json::json!({"error":{"message":"transient"}}),
    ));
    simulator.enqueue_chat(fallback_held);
    let before_fallback = simulator.generation_count();
    let fallback_task =
        spawn_route(fallback_app.clone(), fixture.inference.credential.clone()).await;
    wait_generation_at_least(&simulator, before_fallback + 2).await;
    let fallback_extra = tokio::time::timeout(
        OVERLOAD_PROMPT_BOUND,
        send(
            fallback_app,
            route_json_request(&fixture.inference.credential),
        ),
    )
    .await
    .expect("fallback must keep the generation permit");
    let fallback_headers = fallback_extra.headers().clone();
    let fallback_status = fallback_extra.status();
    let fallback_body = json_of(fallback_extra).await;
    assert_overloaded(fallback_status, &fallback_headers, &fallback_body);
    assert_eq!(simulator.generation_count(), before_fallback + 2);
    fallback_hold.release();
    let fallback_response = fallback_task.await.expect("fallback join");
    assert_eq!(fallback_response.status(), StatusCode::OK);

    fixture.drop_rows().await;
}

#[tokio::test]
#[serial]
async fn each_terminal_path_releases_capacity_for_a_later_request() {
    isolate_provider_environment();
    let fixture = LimitFixture::provision().await;
    let simulator = OpenAiSimulator::start().await;
    let bound = min_padded_chat_completion_len() + 32;
    let app = limits_router(&simulator, fixture.auth_service(), |settings| {
        settings.limits.max_concurrent_generations = 1;
        settings.limits.retries_per_target = 0;
        settings.limits.max_total_attempts = 1;
        settings.limits.max_upstream_response_bytes = bound as u64;
        settings.limits.protected_request_deadline = Duration::from_millis(300);
        settings.limits.max_attempt_timeout = Duration::from_millis(300);
    });

    simulator.enqueue_chat(ScriptedResponse::raw_json_bytes(
        padded_chat_completion_bytes(bound + 1),
    ));
    let oversized = send(
        app.clone(),
        route_json_request(&fixture.inference.credential),
    )
    .await;
    assert_eq!(oversized.status(), StatusCode::BAD_GATEWAY);
    let oversized_body = json_of(oversized).await;
    assert_eq!(oversized_body["error"]["code"], "UPSTREAM_PROTOCOL");
    let after_failure = send(
        app.clone(),
        route_json_request(&fixture.inference.credential),
    )
    .await;
    assert_eq!(after_failure.status(), StatusCode::OK);

    simulator
        .enqueue_chat(ScriptedResponse::chat_ok().delay_headers(Duration::from_secs(2)));
    let expired = send(
        app.clone(),
        route_json_request(&fixture.inference.credential),
    )
    .await;
    assert_eq!(expired.status(), StatusCode::GATEWAY_TIMEOUT);
    let expired_body = json_of(expired).await;
    assert_eq!(expired_body["error"]["code"], "DEADLINE_EXCEEDED");
    let after_deadline = send(
        app.clone(),
        route_json_request(&fixture.inference.credential),
    )
    .await;
    assert_eq!(after_deadline.status(), StatusCode::OK);

    let expected_after_hold = simulator.generation_count() + 1;
    let (held, hold) = ScriptedResponse::chat_ok().hold();
    simulator.enqueue_chat(held);
    let cancelled = spawn_route(app.clone(), fixture.inference.credential.clone()).await;
    wait_generation_at_least(&simulator, expected_after_hold).await;
    cancelled.abort();
    let _ = cancelled.await;
    hold.release();
    let after_cancel = send(app, route_json_request(&fixture.inference.credential)).await;
    assert_eq!(after_cancel.status(), StatusCode::OK);

    fixture.drop_rows().await;
}

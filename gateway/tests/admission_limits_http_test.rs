//! Raw-body and metadata admission limits through the production router.
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
use support::{
    cleanup_clients, isolate_provider_environment, production_router_with_settings,
    settings_for_simulator_with, test_pool, OpenAiSimulator,
};
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
    production_router_with_settings(
        settings_for_simulator_with(simulator, mutate),
        auth_service,
        MetricsConfig::disabled(),
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

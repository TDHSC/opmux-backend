//! Protected-request deadline and cancellation through the production router
//! and the real OpenAI adapter.

mod support;

use async_trait::async_trait;
use axum::{
    body::{Body, Bytes},
    http::{Request, StatusCode},
};
use chrono::{DateTime, Utc};
use gateway::{
    core::{deadline::RequestDeadline, metrics::MetricsConfig},
    features::{
        auth::{
            ApiKeyKind, ApiKeyRecord, AuthService, AuthStore, AuthStoreError,
            ClientRecord, KeyDigest, NewApiKey, NewClient, PostgresAuthStore,
            RevokeOutcome,
        },
        executor::{config::ExecutorConfig, service::ExecutorService},
    },
};
use serde_json::json;
use serial_test::serial;
use std::sync::Arc;
use std::time::{Duration, Instant};
use support::{
    cleanup_clients, isolate_provider_environment, production_router_with_settings,
    provision_inference_key, settings_for_simulator_with, test_pool, OpenAiSimulator,
    ScriptedResponse, SIMULATED_CONTENT,
};
use tower::ServiceExt;
use uuid::Uuid;

const ROUTE_JSON: &str = r#"{"prompt":"deadline-fixture","metadata":{}}"#;
const HTTP_BOUND: Duration = Duration::from_millis(1_500);

struct DelayedAuthStore {
    inner: PostgresAuthStore,
    delay: Duration,
}

#[async_trait]
impl AuthStore for DelayedAuthStore {
    async fn provision_client_with_key(
        &self,
        client: NewClient,
        key: NewApiKey,
    ) -> Result<(ClientRecord, ApiKeyRecord), AuthStoreError> {
        self.inner.provision_client_with_key(client, key).await
    }

    async fn insert_key(&self, key: NewApiKey) -> Result<ApiKeyRecord, AuthStoreError> {
        self.inner.insert_key(key).await
    }

    async fn find_key_by_digest(
        &self,
        digest: &KeyDigest,
    ) -> Result<Option<ApiKeyRecord>, AuthStoreError> {
        self.inner.find_key_by_digest(digest).await
    }

    async fn authenticate_digest(
        &self,
        digest: &KeyDigest,
        used_at: DateTime<Utc>,
    ) -> Result<Option<ApiKeyRecord>, AuthStoreError> {
        sqlx::query("SELECT pg_sleep($1)")
            .bind(self.delay.as_secs_f64())
            .execute(&self.inner.pool())
            .await
            .map_err(AuthStoreError::from_sqlx)?;
        self.inner.authenticate_digest(digest, used_at).await
    }

    async fn touch_last_used(
        &self,
        client_id: Uuid,
        key_id: Uuid,
        used_at: DateTime<Utc>,
    ) -> Result<bool, AuthStoreError> {
        self.inner.touch_last_used(client_id, key_id, used_at).await
    }

    async fn list_keys_for_client(
        &self,
        client_id: Uuid,
        limit: i64,
        offset: i64,
        kind: Option<ApiKeyKind>,
    ) -> Result<Vec<ApiKeyRecord>, AuthStoreError> {
        self.inner
            .list_keys_for_client(client_id, limit, offset, kind)
            .await
    }

    async fn revoke_key(
        &self,
        client_id: Uuid,
        key_id: Uuid,
        revoked_at: DateTime<Utc>,
    ) -> Result<RevokeOutcome, AuthStoreError> {
        self.inner.revoke_key(client_id, key_id, revoked_at).await
    }
}

fn delayed_auth_service(pool: sqlx::PgPool, delay: Duration) -> Arc<AuthService> {
    Arc::new(AuthService::new(Arc::new(DelayedAuthStore {
        inner: PostgresAuthStore::new(pool),
        delay,
    })))
}

async fn body_json(response: axum::http::Response<Body>) -> serde_json::Value {
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("body");
    serde_json::from_slice(&bytes).expect("json")
}

fn assert_deadline_envelope(
    status: StatusCode,
    body: &serde_json::Value,
    request_id: &str,
) {
    assert_eq!(status, StatusCode::GATEWAY_TIMEOUT);
    assert_eq!(body["error"]["code"], "DEADLINE_EXCEEDED");
    assert_eq!(
        body["error"]["message"],
        "The request deadline was exceeded"
    );
    assert_eq!(body["error"]["request_id"], request_id);
    assert!(body.get("response").is_none());
}

fn slow_json_body(stall: Duration) -> Body {
    const PREFIX: &[u8] = br#"{"prompt":"slow-body","metadata":{"k":""#;
    const SUFFIX: &[u8] = br#""}}"#;
    Body::from_stream(futures_util::stream::unfold(0u8, move |step| async move {
        match step {
            0 => Some((Ok::<_, std::io::Error>(Bytes::from_static(PREFIX)), 1)),
            1 => {
                tokio::time::sleep(stall).await;
                Some((Ok(Bytes::from_static(SUFFIX)), 2))
            }
            _ => None,
        }
    }))
}

fn generation_plan() -> gateway::core::contracts::RoutePlan {
    gateway::core::contracts::RoutePlan {
        vendor_id: "openai".to_string(),
        target_id: "primary".to_string(),
        model_id: "example-chat-model".to_string(),
        fallback_plans: Vec::new(),
    }
}

fn generation_payload() -> serde_json::Value {
    json!({
        "messages": [{ "role": "user", "content": "cancel-execution" }]
    })
}

fn executor_for_simulator(
    simulator: &OpenAiSimulator,
    retries: u32,
    attempt_timeout: Duration,
) -> ExecutorService {
    let mut settings = gateway::core::config::Settings::for_tests_with_provider(
        simulator.base_url(),
        simulator.credential(),
    );
    settings.limits.retries_per_target = retries;
    settings.limits.max_attempt_timeout = attempt_timeout;
    ExecutorService::from_config(ExecutorConfig::from_settings(&settings))
        .expect("executor from simulator settings")
}

async fn wait_for_generation(simulator: &OpenAiSimulator, count: usize) {
    let started = Instant::now();
    loop {
        if simulator.generation_count() >= count {
            return;
        }
        if started.elapsed() > Duration::from_secs(2) {
            panic!(
                "timed out waiting for {count} generation calls, observed {}",
                simulator.generation_count()
            );
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

#[tokio::test]
#[serial]
async fn slow_body_expires_before_execution_with_zero_provider_calls() {
    isolate_provider_environment();
    let pool = test_pool().await;
    let issued = provision_inference_key(&pool).await;
    let simulator = OpenAiSimulator::start().await;
    let settings = settings_for_simulator_with(&simulator, |settings| {
        settings.limits.protected_request_deadline = Duration::from_millis(250);
    });
    let app = production_router_with_settings(
        settings,
        Arc::new(AuthService::new(Arc::new(PostgresAuthStore::new(
            pool.clone(),
        )))),
        MetricsConfig::disabled(),
    );

    let started = Instant::now();
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/route")
                .header("content-type", "application/json")
                .header("x-api-key", &issued.credential)
                .body(slow_json_body(Duration::from_millis(800)))
                .unwrap(),
        )
        .await
        .unwrap();
    let elapsed = started.elapsed();
    let request_id = response
        .headers()
        .get("X-Request-ID")
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_string();
    let status = response.status();
    let body = body_json(response).await;
    assert_deadline_envelope(status, &body, &request_id);
    assert!(
        elapsed < HTTP_BOUND,
        "slow-body deadline took {elapsed:?}, bound {HTTP_BOUND:?}"
    );
    assert_eq!(simulator.generation_count(), 0);
    cleanup_clients(&pool, &[issued.client_id]).await;
}

#[tokio::test]
#[serial]
async fn delayed_database_auth_expires_with_zero_provider_calls() {
    isolate_provider_environment();
    let pool = test_pool().await;
    let issued = provision_inference_key(&pool).await;
    let simulator = OpenAiSimulator::start().await;
    let settings = settings_for_simulator_with(&simulator, |settings| {
        settings.limits.protected_request_deadline = Duration::from_millis(200);
    });
    let app = production_router_with_settings(
        settings,
        delayed_auth_service(pool.clone(), Duration::from_millis(600)),
        MetricsConfig::disabled(),
    );

    let started = Instant::now();
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/route")
                .header("content-type", "application/json")
                .header("x-api-key", &issued.credential)
                .body(Body::from(ROUTE_JSON))
                .unwrap(),
        )
        .await
        .unwrap();
    let elapsed = started.elapsed();
    let request_id = response
        .headers()
        .get("X-Request-ID")
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_string();
    let status = response.status();
    let body = body_json(response).await;
    assert_deadline_envelope(status, &body, &request_id);
    assert!(
        elapsed < HTTP_BOUND,
        "delayed-auth deadline took {elapsed:?}, bound {HTTP_BOUND:?}"
    );
    assert_eq!(simulator.generation_count(), 0);
    cleanup_clients(&pool, &[issued.client_id]).await;
}

#[tokio::test]
#[serial]
async fn execution_does_not_receive_a_fresh_deadline_after_auth() {
    isolate_provider_environment();
    let pool = test_pool().await;
    let issued = provision_inference_key(&pool).await;
    let simulator = OpenAiSimulator::start().await;
    simulator.enqueue_chat(
        ScriptedResponse::chat_ok().delay_headers(Duration::from_millis(400)),
    );
    let settings = settings_for_simulator_with(&simulator, |settings| {
        settings.limits.protected_request_deadline = Duration::from_millis(500);
        settings.limits.max_attempt_timeout = Duration::from_secs(10);
    });
    let app = production_router_with_settings(
        settings,
        delayed_auth_service(pool.clone(), Duration::from_millis(350)),
        MetricsConfig::disabled(),
    );

    let started = Instant::now();
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/route")
                .header("content-type", "application/json")
                .header("x-api-key", &issued.credential)
                .body(Body::from(ROUTE_JSON))
                .unwrap(),
        )
        .await
        .unwrap();
    let elapsed = started.elapsed();
    let request_id = response
        .headers()
        .get("X-Request-ID")
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_string();
    let status = response.status();
    let body = body_json(response).await;
    assert_deadline_envelope(status, &body, &request_id);
    assert!(
        elapsed < Duration::from_millis(1_200),
        "shared deadline took {elapsed:?}"
    );
    assert!(
        simulator.generation_count() <= 1,
        "deadline must not start additional attempts"
    );
    cleanup_clients(&pool, &[issued.client_id]).await;
}

#[tokio::test]
#[serial]
async fn delayed_upstream_headers_return_504_and_stop_retries() {
    isolate_provider_environment();
    let pool = test_pool().await;
    let issued = provision_inference_key(&pool).await;
    let simulator = OpenAiSimulator::start().await;
    simulator
        .enqueue_chat(ScriptedResponse::chat_ok().delay_headers(Duration::from_secs(2)));
    simulator.enqueue_chat(ScriptedResponse::chat_ok());
    let settings = settings_for_simulator_with(&simulator, |settings| {
        settings.limits.protected_request_deadline = Duration::from_millis(300);
        settings.limits.max_attempt_timeout = Duration::from_secs(10);
        settings.limits.retries_per_target = 1;
    });
    let app = production_router_with_settings(
        settings,
        Arc::new(AuthService::new(Arc::new(PostgresAuthStore::new(
            pool.clone(),
        )))),
        MetricsConfig::disabled(),
    );

    let started = Instant::now();
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/route")
                .header("content-type", "application/json")
                .header("x-api-key", &issued.credential)
                .body(Body::from(ROUTE_JSON))
                .unwrap(),
        )
        .await
        .unwrap();
    let elapsed = started.elapsed();
    let request_id = response
        .headers()
        .get("X-Request-ID")
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_string();
    let status = response.status();
    let body = body_json(response).await;
    assert_deadline_envelope(status, &body, &request_id);
    assert!(elapsed < HTTP_BOUND, "upstream stall took {elapsed:?}");
    tokio::time::sleep(Duration::from_millis(400)).await;
    assert_eq!(simulator.generation_count(), 1);
    cleanup_clients(&pool, &[issued.client_id]).await;
}

#[tokio::test]
#[serial]
async fn delayed_upstream_body_returns_504() {
    isolate_provider_environment();
    let pool = test_pool().await;
    let issued = provision_inference_key(&pool).await;
    let simulator = OpenAiSimulator::start().await;
    simulator
        .enqueue_chat(ScriptedResponse::chat_ok().delay_body(Duration::from_secs(2)));
    let settings = settings_for_simulator_with(&simulator, |settings| {
        settings.limits.protected_request_deadline = Duration::from_millis(300);
        settings.limits.max_attempt_timeout = Duration::from_secs(10);
        settings.limits.retries_per_target = 0;
    });
    let app = production_router_with_settings(
        settings,
        Arc::new(AuthService::new(Arc::new(PostgresAuthStore::new(
            pool.clone(),
        )))),
        MetricsConfig::disabled(),
    );

    let started = Instant::now();
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/route")
                .header("content-type", "application/json")
                .header("x-api-key", &issued.credential)
                .body(Body::from(ROUTE_JSON))
                .unwrap(),
        )
        .await
        .unwrap();
    let elapsed = started.elapsed();
    assert_eq!(response.status(), StatusCode::GATEWAY_TIMEOUT);
    assert!(elapsed < HTTP_BOUND, "body stall took {elapsed:?}");
    cleanup_clients(&pool, &[issued.client_id]).await;
}

#[tokio::test]
#[serial]
async fn short_deadline_still_allows_fast_success() {
    isolate_provider_environment();
    let pool = test_pool().await;
    let issued = provision_inference_key(&pool).await;
    let simulator = OpenAiSimulator::start().await;
    let settings = settings_for_simulator_with(&simulator, |settings| {
        settings.limits.protected_request_deadline = Duration::from_secs(2);
    });
    let app = production_router_with_settings(
        settings,
        Arc::new(AuthService::new(Arc::new(PostgresAuthStore::new(
            pool.clone(),
        )))),
        MetricsConfig::disabled(),
    );

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/route")
                .header("content-type", "application/json")
                .header("x-api-key", &issued.credential)
                .body(Body::from(ROUTE_JSON))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = body_json(response).await;
    assert_eq!(body["response"]["content"], SIMULATED_CONTENT);
    assert_eq!(simulator.generation_count(), 1);
    cleanup_clients(&pool, &[issued.client_id]).await;
}

#[tokio::test]
#[serial]
async fn health_and_metrics_are_outside_the_protected_deadline() {
    isolate_provider_environment();
    let pool = test_pool().await;
    let simulator = OpenAiSimulator::start().await;
    let settings = settings_for_simulator_with(&simulator, |settings| {
        settings.limits.protected_request_deadline = Duration::from_millis(50);
    });
    let app = production_router_with_settings(
        settings,
        Arc::new(AuthService::new(Arc::new(PostgresAuthStore::new(pool)))),
        MetricsConfig::production(),
    );

    let started = Instant::now();
    let health = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/health")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(health.status(), StatusCode::OK);
    let metrics = app
        .oneshot(
            Request::builder()
                .uri("/metrics")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(metrics.status(), StatusCode::OK);
    assert!(started.elapsed() < Duration::from_millis(500));
}

#[tokio::test]
#[serial]
async fn dropping_inflight_adapter_call_starts_no_later_attempt() {
    isolate_provider_environment();
    let simulator = OpenAiSimulator::start().await;
    simulator
        .enqueue_chat(ScriptedResponse::chat_ok().delay_headers(Duration::from_secs(2)));
    simulator.enqueue_chat(ScriptedResponse::chat_ok());
    let service = Arc::new(executor_for_simulator(
        &simulator,
        1,
        Duration::from_secs(5),
    ));
    let deadline = RequestDeadline::from_timeout(Duration::from_secs(30));
    let handle = {
        let service = service.clone();
        tokio::spawn(async move {
            service
                .execute(&generation_plan(), &generation_payload(), deadline)
                .await
        })
    };
    wait_for_generation(&simulator, 1).await;
    handle.abort();
    let _ = handle.await;
    tokio::time::sleep(Duration::from_secs(3)).await;
    assert_eq!(simulator.generation_count(), 1);

    let independent = service
        .execute(
            &generation_plan(),
            &generation_payload(),
            RequestDeadline::from_timeout(Duration::from_secs(30)),
        )
        .await
        .expect("independent request should succeed");
    assert_eq!(independent.content, SIMULATED_CONTENT);
    assert_eq!(simulator.generation_count(), 2);
}

#[tokio::test]
#[serial]
async fn dropping_during_retry_backoff_starts_no_later_attempt() {
    isolate_provider_environment();
    let simulator = OpenAiSimulator::start().await;
    simulator.enqueue_chat(ScriptedResponse::Json {
        status: 429,
        body: json!({"error":{"message":"rate"}}),
        retry_after: Some("2".to_string()),
    });
    simulator.enqueue_chat(ScriptedResponse::chat_ok());
    let service = executor_for_simulator(&simulator, 1, Duration::from_secs(5));
    let deadline = RequestDeadline::from_timeout(Duration::from_secs(30));
    let handle = tokio::spawn(async move {
        service
            .execute(&generation_plan(), &generation_payload(), deadline)
            .await
    });
    wait_for_generation(&simulator, 1).await;
    handle.abort();
    let _ = handle.await;
    tokio::time::sleep(Duration::from_secs(3)).await;
    assert_eq!(simulator.generation_count(), 1);
}

#[tokio::test]
#[serial]
async fn cancelled_deadline_work_releases_capacity_for_later_http() {
    isolate_provider_environment();
    let pool = test_pool().await;
    let issued = provision_inference_key(&pool).await;
    let simulator = OpenAiSimulator::start().await;
    simulator
        .enqueue_chat(ScriptedResponse::chat_ok().delay_headers(Duration::from_secs(2)));
    let settings = settings_for_simulator_with(&simulator, |settings| {
        settings.limits.protected_request_deadline = Duration::from_millis(250);
        settings.limits.retries_per_target = 1;
    });
    let app = production_router_with_settings(
        settings,
        Arc::new(AuthService::new(Arc::new(PostgresAuthStore::new(
            pool.clone(),
        )))),
        MetricsConfig::disabled(),
    );

    let first = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/route")
                .header("content-type", "application/json")
                .header("x-api-key", &issued.credential)
                .body(Body::from(ROUTE_JSON))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(first.status(), StatusCode::GATEWAY_TIMEOUT);

    let second = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/route")
                .header("content-type", "application/json")
                .header("x-api-key", &issued.credential)
                .body(Body::from(ROUTE_JSON))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(second.status(), StatusCode::OK);
    cleanup_clients(&pool, &[issued.client_id]).await;
}

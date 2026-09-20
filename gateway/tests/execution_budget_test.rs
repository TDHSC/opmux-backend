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
    core::{db::DatabasePoolConfig, deadline::RequestDeadline, metrics::MetricsConfig},
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
    provision_inference_key, required_database_url, settings_for_simulator_with,
    test_pool, OpenAiSimulator, ScriptedResponse, SIMULATED_CONTENT,
};
use tower::ServiceExt;
use uuid::Uuid;

const ROUTE_JSON: &str = r#"{"prompt":"deadline-fixture","metadata":{}}"#;
const HTTP_BOUND: Duration = Duration::from_millis(1_500);
/// Capacity must return well before the pool's 5s statement timeout.
const POOL_RELEASE_BOUND: Duration = Duration::from_millis(1_500);

fn route_request(credential: &str) -> Request<Body> {
    Request::builder()
        .method("POST")
        .uri("/api/v1/route")
        .header("content-type", "application/json")
        .header("x-api-key", credential)
        .body(Body::from(ROUTE_JSON))
        .unwrap()
}

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

    async fn probe_authentication_access(&self) -> Result<(), AuthStoreError> {
        self.inner.probe_authentication_access().await
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
        max_output_tokens: 512,
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
    executor_for_simulator_with(simulator, retries, attempt_timeout, |_| {})
}

fn executor_for_simulator_with(
    simulator: &OpenAiSimulator,
    retries: u32,
    attempt_timeout: Duration,
    mutate: impl FnOnce(&mut gateway::core::config::Settings),
) -> ExecutorService {
    let mut settings = gateway::core::config::Settings::for_tests_with_provider(
        simulator.base_url(),
        simulator.credential(),
    );
    settings.limits.retries_per_target = retries;
    settings.limits.max_attempt_timeout = attempt_timeout;
    mutate(&mut settings);
    ExecutorService::from_config(ExecutorConfig::from_settings(&settings))
        .expect("executor from simulator settings")
}

fn transient_unavailable() -> ScriptedResponse {
    ScriptedResponse::json_status(500, json!({"error":{"message":"temp"}}))
}

fn rate_limited(retry_after: &str) -> ScriptedResponse {
    ScriptedResponse::Json {
        status: 429,
        body: json!({"error":{"message":"rate"}}),
        retry_after: Some(retry_after.to_string()),
    }
}

fn enable_default_fallback(settings: &mut gateway::core::config::Settings) {
    settings
        .catalog
        .routes
        .get_mut("default")
        .expect("default route")
        .fallbacks = vec!["secondary".to_string()];
}

fn generation_bodies(simulator: &OpenAiSimulator) -> Vec<serde_json::Value> {
    simulator
        .captured()
        .into_iter()
        .filter(|capture| capture.is_generation())
        .filter_map(|capture| capture.body)
        .collect()
}

fn generation_models(simulator: &OpenAiSimulator) -> Vec<String> {
    generation_bodies(simulator)
        .into_iter()
        .map(|body| body["model"].as_str().unwrap_or_default().to_string())
        .collect()
}

fn assert_same_generation_bodies(simulator: &OpenAiSimulator, expected_calls: usize) {
    let bodies = generation_bodies(simulator);
    assert_eq!(bodies.len(), expected_calls);
    let first = bodies[0].clone();
    assert_eq!(first["model"], "example-chat-model");
    assert_eq!(first["messages"][0]["role"], "user");
    for body in &bodies {
        assert_eq!(body, &first);
    }
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
async fn timed_out_row_lock_auth_releases_pool_for_unrelated_key() {
    isolate_provider_environment();
    let admin_pool = test_pool().await;
    let key_a = provision_inference_key(&admin_pool).await;
    let key_b = provision_inference_key(&admin_pool).await;
    let runtime_pool = DatabasePoolConfig::new(required_database_url())
        .expect("DATABASE_URL must parse")
        .with_max_connections(1)
        .expect("one runtime slot")
        .with_acquire_timeout(Duration::from_secs(2))
        .expect("runtime acquire timeout")
        .connect_with_role("opmux_runtime")
        .await
        .unwrap_or_else(|_| {
            panic!(
                "failed to connect a one-slot runtime pool; persisted authentication tests require the owned local Supabase and do not skip"
            )
        });
    sqlx::query_scalar::<_, i32>("SELECT 1")
        .fetch_one(&runtime_pool)
        .await
        .expect("warm the one runtime pool slot");

    let simulator = OpenAiSimulator::start().await;
    let settings = settings_for_simulator_with(&simulator, |settings| {
        settings.limits.protected_request_deadline = Duration::from_millis(500);
    });
    let app = production_router_with_settings(
        settings,
        Arc::new(AuthService::new(Arc::new(PostgresAuthStore::new(
            runtime_pool.clone(),
        )))),
        MetricsConfig::disabled(),
    );

    let mut blocker = admin_pool.begin().await.expect("blocker transaction");
    sqlx::query("SELECT 1 FROM opmux_private.api_keys WHERE id = $1 FOR UPDATE")
        .bind(key_a.key_id)
        .fetch_one(&mut *blocker)
        .await
        .expect("hold key A row lock");

    let started = Instant::now();
    let response_a = app
        .clone()
        .oneshot(route_request(&key_a.credential))
        .await
        .unwrap();
    let a_elapsed = started.elapsed();
    let request_id = response_a
        .headers()
        .get("X-Request-ID")
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_string();
    let status_a = response_a.status();
    let body_a = body_json(response_a).await;
    assert_deadline_envelope(status_a, &body_a, &request_id);
    assert!(
        a_elapsed < HTTP_BOUND,
        "blocked-auth deadline took {a_elapsed:?}, bound {HTTP_BOUND:?}"
    );
    assert_eq!(simulator.generation_count(), 0);

    let b_started = Instant::now();
    let response_b = app.oneshot(route_request(&key_b.credential)).await.unwrap();
    let b_elapsed = b_started.elapsed();
    assert_eq!(response_b.status(), StatusCode::OK);
    let body_b = body_json(response_b).await;
    assert_eq!(body_b["response"]["content"], SIMULATED_CONTENT);
    assert_eq!(simulator.generation_count(), 1);
    assert!(
        b_elapsed < POOL_RELEASE_BOUND,
        "unrelated key B must reuse the one-slot pool before statement_timeout; took {b_elapsed:?}, bound {POOL_RELEASE_BOUND:?}"
    );

    blocker.rollback().await.expect("release blocker");
    runtime_pool.close().await;
    cleanup_clients(&admin_pool, &[key_a.client_id, key_b.client_id]).await;
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
    simulator.enqueue_chat(rate_limited("2"));
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

#[tokio::test]
#[serial]
async fn default_policy_stops_transient_attempts_at_two_calls() {
    isolate_provider_environment();
    let pool = test_pool().await;
    let issued = provision_inference_key(&pool).await;
    let simulator = OpenAiSimulator::start().await;
    simulator.enqueue_chat(transient_unavailable());
    simulator.enqueue_chat(transient_unavailable());
    simulator.enqueue_chat(ScriptedResponse::chat_ok());
    let settings = settings_for_simulator_with(&simulator, |settings| {
        settings.limits.backoff_cap = Duration::from_millis(1);
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
    let status = response.status();
    let body = body_json(response).await;
    assert_eq!(status, StatusCode::BAD_GATEWAY);
    assert_eq!(body["error"]["code"], "UPSTREAM_ERROR");
    assert_eq!(simulator.generation_count(), 2);
    assert_same_generation_bodies(&simulator, 2);
    cleanup_clients(&pool, &[issued.client_id]).await;
}

#[tokio::test]
#[serial]
async fn overridden_retry_allowance_stops_at_three_total_attempts() {
    isolate_provider_environment();
    let pool = test_pool().await;
    let issued = provision_inference_key(&pool).await;
    let simulator = OpenAiSimulator::start().await;
    for _ in 0..4 {
        simulator.enqueue_chat(transient_unavailable());
    }
    let settings = settings_for_simulator_with(&simulator, |settings| {
        settings.limits.retries_per_target = 5;
        settings.limits.max_total_attempts = 3;
        settings.limits.backoff_cap = Duration::from_millis(1);
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
    assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
    assert_eq!(simulator.generation_count(), 3);
    assert_same_generation_bodies(&simulator, 3);
    cleanup_clients(&pool, &[issued.client_id]).await;
}

#[tokio::test]
#[serial]
async fn transient_retry_then_success_reuses_the_same_request_body() {
    isolate_provider_environment();
    let pool = test_pool().await;
    let issued = provision_inference_key(&pool).await;
    let simulator = OpenAiSimulator::start().await;
    simulator.enqueue_chat(transient_unavailable());
    simulator.enqueue_chat(ScriptedResponse::chat_ok());
    let settings = settings_for_simulator_with(&simulator, |settings| {
        settings.limits.backoff_cap = Duration::from_millis(1);
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
    assert_eq!(simulator.generation_count(), 2);
    assert_same_generation_bodies(&simulator, 2);
    cleanup_clients(&pool, &[issued.client_id]).await;
}

#[tokio::test]
#[serial]
async fn attempt_timeout_retries_without_claiming_deadline_expiry() {
    isolate_provider_environment();
    let pool = test_pool().await;
    let issued = provision_inference_key(&pool).await;
    let simulator = OpenAiSimulator::start().await;
    simulator.enqueue_chat(
        ScriptedResponse::chat_ok().delay_headers(Duration::from_millis(400)),
    );
    simulator.enqueue_chat(ScriptedResponse::chat_ok());
    let settings = settings_for_simulator_with(&simulator, |settings| {
        settings.limits.protected_request_deadline = Duration::from_secs(2);
        settings.limits.max_attempt_timeout = Duration::from_millis(150);
        settings.limits.retries_per_target = 1;
        settings.limits.backoff_cap = Duration::from_millis(1);
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
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(simulator.generation_count(), 2);
    assert!(
        elapsed < Duration::from_millis(1_200),
        "attempt-timeout retry took {elapsed:?}"
    );
    cleanup_clients(&pool, &[issued.client_id]).await;
}

#[tokio::test]
#[serial]
async fn retry_after_delta_seconds_is_honored_before_the_next_call() {
    isolate_provider_environment();
    let simulator = OpenAiSimulator::start().await;
    simulator.enqueue_chat(rate_limited("1"));
    simulator.enqueue_chat(ScriptedResponse::chat_ok());
    let service =
        executor_for_simulator_with(&simulator, 1, Duration::from_secs(5), |settings| {
            settings.limits.protected_request_deadline = Duration::from_secs(5);
            settings.limits.backoff_cap = Duration::from_millis(2_000);
        });
    let started = Instant::now();
    let result = service
        .execute(
            &generation_plan(),
            &generation_payload(),
            RequestDeadline::from_timeout(Duration::from_secs(5)),
        )
        .await
        .expect("retry after provider delay should succeed");
    let elapsed = started.elapsed();
    assert_eq!(result.content, SIMULATED_CONTENT);
    assert_eq!(simulator.generation_count(), 2);
    assert_same_generation_bodies(&simulator, 2);
    assert!(
        elapsed >= Duration::from_millis(900),
        "second call started after {elapsed:?}, before the 1s Retry-After"
    );
    assert!(
        elapsed < Duration::from_millis(1_800),
        "Retry-After wait took {elapsed:?}"
    );
}

#[tokio::test]
#[serial]
async fn retry_after_that_cannot_fit_returns_429_not_504() {
    isolate_provider_environment();
    let pool = test_pool().await;
    let issued = provision_inference_key(&pool).await;
    let simulator = OpenAiSimulator::start().await;
    simulator.enqueue_chat(rate_limited("30"));
    simulator.enqueue_chat(ScriptedResponse::chat_ok());
    let settings = settings_for_simulator_with(&simulator, |settings| {
        settings.limits.protected_request_deadline = Duration::from_millis(400);
        settings.limits.retries_per_target = 1;
        settings.limits.backoff_cap = Duration::from_millis(2_000);
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
    let status = response.status();
    let body = body_json(response).await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(body["error"]["code"], "UPSTREAM_RATE_LIMIT");
    assert_ne!(body["error"]["code"], "DEADLINE_EXCEEDED");
    assert!(body.get("response").is_none());
    assert_eq!(simulator.generation_count(), 1);
    assert!(
        elapsed < HTTP_BOUND,
        "cannot-fit Retry-After took {elapsed:?}"
    );
    cleanup_clients(&pool, &[issued.client_id]).await;
}

#[tokio::test]
#[serial]
async fn http_date_retry_after_that_cannot_fit_is_throttling() {
    isolate_provider_environment();
    let future =
        DateTime::<Utc>::from(std::time::SystemTime::now() + Duration::from_secs(60))
            .format("%a, %d %b %Y %H:%M:%S GMT")
            .to_string();
    let simulator = OpenAiSimulator::start().await;
    simulator.enqueue_chat(ScriptedResponse::Json {
        status: 429,
        body: json!({"error":{"message":"rate"}}),
        retry_after: Some(future),
    });
    simulator.enqueue_chat(ScriptedResponse::chat_ok());
    let service =
        executor_for_simulator_with(&simulator, 1, Duration::from_secs(5), |settings| {
            settings.limits.protected_request_deadline = Duration::from_millis(400);
        });
    let started = Instant::now();
    match service
        .execute(
            &generation_plan(),
            &generation_payload(),
            RequestDeadline::from_timeout(Duration::from_millis(400)),
        )
        .await
    {
        Err(gateway::features::executor::error::ExecutorError::RateLimitExceeded {
            ..
        }) => {}
        other => panic!("expected RateLimitExceeded, got {other:?}"),
    }
    assert!(started.elapsed() < HTTP_BOUND);
    assert_eq!(simulator.generation_count(), 1);
}

#[tokio::test]
#[serial]
async fn malformed_retry_after_uses_capped_jitter_not_an_unbounded_sleep() {
    isolate_provider_environment();
    let simulator = OpenAiSimulator::start().await;
    simulator.enqueue_chat(rate_limited("not-a-delay"));
    simulator.enqueue_chat(ScriptedResponse::chat_ok());
    let service =
        executor_for_simulator_with(&simulator, 1, Duration::from_secs(5), |settings| {
            settings.limits.backoff_cap = Duration::from_millis(1);
            settings.limits.protected_request_deadline = Duration::from_secs(2);
        });
    let started = Instant::now();
    let result = service
        .execute(
            &generation_plan(),
            &generation_payload(),
            RequestDeadline::from_timeout(Duration::from_secs(2)),
        )
        .await
        .expect("malformed Retry-After should fall back to capped jitter");
    assert_eq!(result.content, SIMULATED_CONTENT);
    assert_eq!(simulator.generation_count(), 2);
    assert!(started.elapsed() < HTTP_BOUND);
}

#[tokio::test]
#[serial]
async fn stalled_429_body_preserves_retry_after_instead_of_timeout_retry() {
    isolate_provider_environment();
    let simulator = OpenAiSimulator::start().await;
    simulator.enqueue_chat(rate_limited("1").delay_body(Duration::from_secs(2)));
    simulator.enqueue_chat(ScriptedResponse::chat_ok());
    let service = executor_for_simulator_with(
        &simulator,
        1,
        Duration::from_millis(150),
        |settings| {
            settings.limits.protected_request_deadline = Duration::from_secs(5);
            settings.limits.backoff_cap = Duration::from_millis(1);
        },
    );
    let started = Instant::now();
    let result = service
        .execute(
            &generation_plan(),
            &generation_payload(),
            RequestDeadline::from_timeout(Duration::from_secs(5)),
        )
        .await
        .expect(
            "typed 429 Retry-After must be honored instead of an attempt-timeout retry",
        );
    let elapsed = started.elapsed();
    assert_eq!(result.content, SIMULATED_CONTENT);
    assert_eq!(simulator.generation_count(), 2);
    assert_same_generation_bodies(&simulator, 2);
    assert!(
        elapsed >= Duration::from_millis(900),
        "stalled 429 body must not convert Retry-After into an early timeout retry, elapsed {elapsed:?}"
    );
    assert!(
        elapsed < Duration::from_millis(1_800),
        "Retry-After wait took {elapsed:?}"
    );
}

#[tokio::test]
#[serial]
async fn stalled_429_body_that_cannot_fit_returns_429_not_504() {
    isolate_provider_environment();
    let pool = test_pool().await;
    let issued = provision_inference_key(&pool).await;
    let simulator = OpenAiSimulator::start().await;
    simulator.enqueue_chat(rate_limited("30").delay_body(Duration::from_secs(2)));
    simulator.enqueue_chat(ScriptedResponse::chat_ok());
    let settings = settings_for_simulator_with(&simulator, |settings| {
        settings.limits.protected_request_deadline = Duration::from_millis(400);
        settings.limits.max_attempt_timeout = Duration::from_secs(10);
        settings.limits.retries_per_target = 1;
        settings.limits.backoff_cap = Duration::from_millis(2_000);
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
    let status = response.status();
    let body = body_json(response).await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(body["error"]["code"], "UPSTREAM_RATE_LIMIT");
    assert_ne!(body["error"]["code"], "DEADLINE_EXCEEDED");
    assert!(body.get("response").is_none());
    assert_eq!(simulator.generation_count(), 1);
    assert!(
        elapsed < Duration::from_millis(400),
        "cannot-fit 429 must return before the 400ms deadline, elapsed {elapsed:?}"
    );
    cleanup_clients(&pool, &[issued.client_id]).await;
}

#[tokio::test]
#[serial]
async fn stalled_fitting_429_then_held_retry_expires_504_without_fallback() {
    isolate_provider_environment();
    let pool = test_pool().await;
    let issued = provision_inference_key(&pool).await;
    let simulator = OpenAiSimulator::start().await;
    let (held, hold) = ScriptedResponse::chat_ok().hold();
    simulator.enqueue_chat(rate_limited("1").delay_body(Duration::from_secs(5)));
    simulator.enqueue_chat(held);
    simulator.enqueue_chat(ScriptedResponse::chat_ok());
    let settings = settings_for_simulator_with(&simulator, |settings| {
        enable_default_fallback(settings);
        settings.limits.protected_request_deadline = Duration::from_secs(2);
        settings.limits.max_attempt_timeout = Duration::from_secs(10);
        settings.limits.retries_per_target = 1;
        settings.limits.max_total_attempts = 3;
        settings.limits.backoff_cap = Duration::from_millis(2_000);
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
    assert!(
        elapsed >= Duration::from_millis(1_500),
        "held retry must consume the overall deadline, elapsed {elapsed:?}"
    );
    assert!(
        elapsed < Duration::from_millis(3_000),
        "held retry 504 took {elapsed:?}"
    );
    let models = generation_models(&simulator);
    assert!(
        models.len() <= 2,
        "at most two primary calls, got {models:?}"
    );
    assert!(
        models.iter().all(|model| model == "example-chat-model"),
        "held retry must not call fallback, got {models:?}"
    );
    tokio::time::sleep(Duration::from_millis(400)).await;
    assert!(
        simulator.generation_count() <= 2,
        "deadline must not start late calls, got {}",
        simulator.generation_count()
    );
    assert_eq!(
        generation_models(&simulator)
            .iter()
            .filter(|model| *model == "example-chat-model-mini")
            .count(),
        0
    );
    hold.release();
    cleanup_clients(&pool, &[issued.client_id]).await;
}

#[tokio::test]
#[serial]
async fn retry_after_that_cannot_fit_does_not_call_configured_fallback() {
    isolate_provider_environment();
    let pool = test_pool().await;
    let issued = provision_inference_key(&pool).await;
    let simulator = OpenAiSimulator::start().await;
    simulator.enqueue_chat(rate_limited("30"));
    simulator.enqueue_chat(ScriptedResponse::chat_ok());
    simulator.enqueue_chat(ScriptedResponse::chat_ok());
    let settings = settings_for_simulator_with(&simulator, |settings| {
        enable_default_fallback(settings);
        settings.limits.protected_request_deadline = Duration::from_millis(400);
        settings.limits.retries_per_target = 0;
        settings.limits.max_total_attempts = 3;
        settings.limits.backoff_cap = Duration::from_millis(2_000);
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
    let status = response.status();
    let body = body_json(response).await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(body["error"]["code"], "UPSTREAM_RATE_LIMIT");
    assert_ne!(body["error"]["code"], "DEADLINE_EXCEEDED");
    assert!(body.get("response").is_none());
    assert_eq!(simulator.generation_count(), 1);
    assert_eq!(generation_models(&simulator), vec!["example-chat-model"]);
    assert!(
        elapsed < HTTP_BOUND,
        "cannot-fit with configured fallback took {elapsed:?}"
    );
    cleanup_clients(&pool, &[issued.client_id]).await;
}

#[tokio::test]
#[serial]
async fn near_attempt_cutoff_429_headers_preserve_retry_after() {
    isolate_provider_environment();
    let simulator = OpenAiSimulator::start().await;
    simulator.enqueue_chat(
        rate_limited("1").delay(Duration::from_millis(80), Duration::from_secs(2)),
    );
    simulator.enqueue_chat(ScriptedResponse::chat_ok());
    let service = executor_for_simulator_with(
        &simulator,
        1,
        Duration::from_millis(150),
        |settings| {
            settings.limits.protected_request_deadline = Duration::from_secs(5);
            settings.limits.backoff_cap = Duration::from_millis(1);
        },
    );
    let started = Instant::now();
    let result = service
        .execute(
            &generation_plan(),
            &generation_payload(),
            RequestDeadline::from_timeout(Duration::from_secs(5)),
        )
        .await
        .expect("header-classified 429 near attempt cutoff must keep Retry-After");
    let elapsed = started.elapsed();
    assert_eq!(result.content, SIMULATED_CONTENT);
    assert_eq!(simulator.generation_count(), 2);
    assert_same_generation_bodies(&simulator, 2);
    assert!(
        elapsed >= Duration::from_millis(900),
        "near-cutoff 429 headers must honor Retry-After instead of an attempt-timeout retry, elapsed {elapsed:?}"
    );
    assert!(
        elapsed < Duration::from_millis(2_000),
        "near-cutoff Retry-After wait took {elapsed:?}"
    );
}

#[tokio::test]
#[serial]
async fn malformed_and_oversized_429_bodies_keep_throttling() {
    isolate_provider_environment();
    let bound = 1_024_u64;
    let simulator = OpenAiSimulator::start().await;
    simulator.enqueue_chat(ScriptedResponse::malformed_429(
        br#"{"error": not-json"#,
        Some("1"),
    ));
    simulator.enqueue_chat(ScriptedResponse::chat_ok());
    let service =
        executor_for_simulator_with(&simulator, 1, Duration::from_secs(2), |settings| {
            settings.limits.max_upstream_response_bytes = bound;
            settings.limits.protected_request_deadline = Duration::from_secs(5);
            settings.limits.backoff_cap = Duration::from_millis(1);
        });
    let started = Instant::now();
    let result = service
        .execute(
            &generation_plan(),
            &generation_payload(),
            RequestDeadline::from_timeout(Duration::from_secs(5)),
        )
        .await
        .expect("malformed 429 body must keep Retry-After throttling");
    let elapsed = started.elapsed();
    assert_eq!(result.content, SIMULATED_CONTENT);
    assert_eq!(simulator.generation_count(), 2);
    assert!(
        elapsed >= Duration::from_millis(900),
        "malformed 429 must honor Retry-After, elapsed {elapsed:?}"
    );
    assert!(elapsed < Duration::from_millis(1_800));

    simulator.enqueue_chat(ScriptedResponse::chunked_429(
        br#"{"error":{"code":"insufficient_quota"}}"#,
        (bound as usize) + 32,
        Some("1"),
    ));
    simulator.enqueue_chat(ScriptedResponse::chat_ok());
    let oversized =
        executor_for_simulator_with(&simulator, 1, Duration::from_secs(2), |settings| {
            settings.limits.max_upstream_response_bytes = bound;
            settings.limits.protected_request_deadline = Duration::from_secs(5);
            settings.limits.backoff_cap = Duration::from_millis(1);
        });
    let started = Instant::now();
    let result = oversized
        .execute(
            &generation_plan(),
            &generation_payload(),
            RequestDeadline::from_timeout(Duration::from_secs(5)),
        )
        .await
        .expect("oversized 429 body must keep throttling, not fabricate quota");
    assert_eq!(result.content, SIMULATED_CONTENT);
    assert!(started.elapsed() >= Duration::from_millis(900));
    assert!(started.elapsed() < Duration::from_millis(1_800));
}

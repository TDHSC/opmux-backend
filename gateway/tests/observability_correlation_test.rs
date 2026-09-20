//! Fresh debug-log correlation, secret-safety, and authentication timing.
//!
//! Captures use dummy sentinels. Assertions check presence/absence without
//! printing credentials, digests, prompts, metadata, SQL, or provider bodies.

mod support;

use async_trait::async_trait;
use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use chrono::{DateTime, Utc};
use gateway::{
    core::metrics::MetricsConfig,
    features::auth::{
        hash_credential, ApiKeyKind, ApiKeyRecord, AuthService, AuthStore,
        AuthStoreError, ClientRecord, KeyDigest, NewApiKey, NewClient, PostgresAuthStore,
        ProvisioningService, RevokeOutcome, UnavailableAuthStore,
    },
};
use serde_json::{json, Value};
use serial_test::serial;
use std::io::Write;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use support::{
    cleanup_clients, isolate_provider_environment, production_router_with_auth,
    production_router_with_settings, settings_for_simulator_with, test_pool,
    OpenAiSimulator, ScriptedResponse,
};
use tokio::time::sleep;
use tower::ServiceExt;
use uuid::Uuid;

const PROMPT_SENTINEL: &str = "OBS001_PROMPT_SENTINEL";
const METADATA_SENTINEL: &str = "OBS001_METADATA_SENTINEL";
const SQL_SENTINEL: &str = "SELECT OBS001_SQL_SENTINEL FROM secrets";
const CONNECTION_SENTINEL: &str = "postgres://obs001:obs001pass@127.0.0.1:55432/postgres";
const INVALID_KEY_SENTINEL: &str = "opmx_v1_OBS001_INVALID_KEY_SENTINEL";
const UPSTREAM_BODY_SENTINEL: &str = "OBS001_UPSTREAM_BODY_SENTINEL";
const AUTHORIZATION_SENTINEL: &str = "Bearer OBS001_AUTHORIZATION_SENTINEL";
const CORRELATION_PREFIX: &str = "obs-corr-";
const SIMULATOR_DELAY: Duration = Duration::from_millis(400);
const AUTH_STORE_DELAY: Duration = Duration::from_millis(250);
const AUTH_DELAY_TOLERANCE: Duration = Duration::from_millis(100);
const GENERATION_WAIT_BOUND: Duration = Duration::from_secs(2);

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
            .json()
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

struct Issued {
    client_id: Uuid,
    credential: String,
}

struct ObsFixture {
    pool: sqlx::PgPool,
    inference: Issued,
    management: Issued,
}

impl ObsFixture {
    async fn provision() -> Self {
        let pool = test_pool().await;
        let service =
            ProvisioningService::new(Arc::new(PostgresAuthStore::new(pool.clone())));
        let created = service
            .create_tenant(&format!("obs-{}", Uuid::new_v4().simple()))
            .await
            .expect("tenant");
        let inference = service
            .issue_key(
                created.key.client_id,
                ApiKeyKind::Inference,
                "obs-inference",
            )
            .await
            .expect("inference key");
        Self {
            pool,
            inference: Issued {
                client_id: inference.client_id,
                credential: inference.credential().to_string(),
            },
            management: Issued {
                client_id: created.key.client_id,
                credential: created.key.credential().to_string(),
            },
        }
    }

    fn auth_service(&self) -> Arc<AuthService> {
        Arc::new(AuthService::new(Arc::new(PostgresAuthStore::new(
            self.pool.clone(),
        ))))
    }

    async fn drop_rows(self) {
        cleanup_clients(&self.pool, &[self.inference.client_id]).await;
    }
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
        sleep(self.delay).await;
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

fn sentinel_body() -> String {
    json!({
        "prompt": PROMPT_SENTINEL,
        "metadata": {
            "note": METADATA_SENTINEL,
            "sql": SQL_SENTINEL,
            "db": CONNECTION_SENTINEL
        }
    })
    .to_string()
}

fn json_events(capture: &str) -> Vec<Value> {
    capture
        .lines()
        .filter_map(|line| {
            let line = line.trim();
            if line.starts_with('{') {
                serde_json::from_str(line).ok()
            } else {
                None
            }
        })
        .collect()
}

fn value_contains(value: &Value, needle: &str) -> bool {
    match value {
        Value::String(text) => text == needle || text.contains(needle),
        Value::Array(items) => items.iter().any(|item| value_contains(item, needle)),
        Value::Object(map) => map.values().any(|item| value_contains(item, needle)),
        _ => false,
    }
}

fn gateway_events(events: &[Value]) -> Vec<&Value> {
    events
        .iter()
        .filter(|event| {
            event["target"]
                .as_str()
                .unwrap_or_default()
                .starts_with("gateway")
        })
        .collect()
}

fn is_request_scoped(event: &Value) -> bool {
    if event["span"]["name"].as_str() == Some("http_request") {
        return true;
    }
    event["spans"]
        .as_array()
        .map(|spans| {
            spans
                .iter()
                .any(|span| span["name"].as_str() == Some("http_request"))
        })
        .unwrap_or(false)
}

fn auth_duration_ms(events: &[Value]) -> Option<u64> {
    for event in events {
        let fields = &event["fields"];
        if fields["message"].as_str() != Some("authentication finished") {
            continue;
        }
        if let Some(ms) = fields["auth_duration_ms"].as_u64() {
            return Some(ms);
        }
        if let Some(ms) = fields["auth_duration_ms"].as_i64() {
            return Some(ms.max(0) as u64);
        }
    }
    None
}

fn digest_hex(digest: &[u8; 32]) -> String {
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn assert_omits_protected(capture: &str, extra_credentials: &[&str]) {
    for sentinel in [
        PROMPT_SENTINEL,
        METADATA_SENTINEL,
        SQL_SENTINEL,
        CONNECTION_SENTINEL,
        INVALID_KEY_SENTINEL,
        UPSTREAM_BODY_SENTINEL,
        AUTHORIZATION_SENTINEL,
        "obs001pass",
    ] {
        assert!(
            !capture.contains(sentinel),
            "fresh debug capture must omit protected sentinels"
        );
    }
    for credential in extra_credentials {
        assert!(
            !capture.contains(credential),
            "fresh debug capture must omit fixture credentials"
        );
        let digest = hash_credential(credential);
        let hex = digest_hex(digest.as_bytes());
        assert!(
            !capture.contains(&hex),
            "fresh debug capture must omit digest hex"
        );
        assert!(
            !capture.contains(&hex.to_uppercase()),
            "fresh debug capture must omit digest hex"
        );
        assert!(
            !capture.contains(&format!("{:?}", digest.as_bytes())),
            "fresh debug capture must omit digest bytes"
        );
    }
}

fn assert_correlated(
    capture: &str,
    request_id: &str,
    correlation_id: Option<&str>,
    error_request_id: Option<&str>,
) {
    assert!(!request_id.is_empty(), "response must include X-Request-ID");
    if let Some(error_id) = error_request_id {
        assert_eq!(error_id, request_id);
    }
    let events = json_events(capture);
    let scoped: Vec<&Value> = gateway_events(&events)
        .into_iter()
        .filter(|event| is_request_scoped(event))
        .collect();
    assert!(
        !scoped.is_empty(),
        "request-scoped debug capture must be nonempty"
    );
    for event in scoped {
        assert!(
            value_contains(event, request_id),
            "request-scoped logs must include the response request id"
        );
        if let Some(correlation_id) = correlation_id {
            assert!(
                value_contains(event, correlation_id),
                "valid client correlation must appear in request-scoped logs"
            );
        }
    }
}

fn assert_no_execution_logs(capture: &str) {
    assert!(
        !capture.contains("Calling LLM API"),
        "early failure must not fabricate execution logs"
    );
    assert!(
        !capture.contains("Executing LLM call"),
        "early failure must not fabricate execution logs"
    );
    assert!(
        !capture.contains("Retrying execution"),
        "early failure must not fabricate retry logs"
    );
}

fn route_request(
    api_key: Option<&str>,
    correlation_id: &str,
    body: String,
) -> Request<Body> {
    let mut builder = Request::builder()
        .method("POST")
        .uri("/api/v1/route")
        .header("content-type", "application/json")
        .header("x-correlation-id", correlation_id)
        .header("authorization", AUTHORIZATION_SENTINEL);
    if let Some(key) = api_key {
        builder = builder.header("x-api-key", key);
    }
    builder.body(Body::from(body)).unwrap()
}

async fn send(app: axum::Router, request: Request<Body>) -> axum::http::Response<Body> {
    app.oneshot(request).await.expect("router response")
}

async fn json_of(response: axum::http::Response<Body>) -> Value {
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("body");
    serde_json::from_slice(&bytes).unwrap_or(Value::Null)
}

struct Observed {
    status: StatusCode,
    request_id: String,
    correlation: Option<String>,
    body: Value,
    capture: String,
}

async fn observe(
    capture: &DebugCapture,
    app: axum::Router,
    request: Request<Body>,
) -> Observed {
    let response = send(app, request).await;
    let status = response.status();
    let request_id = response
        .headers()
        .get("x-request-id")
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_string();
    let correlation = response
        .headers()
        .get("x-correlation-id")
        .and_then(|value| value.to_str().ok())
        .map(ToOwned::to_owned);
    let body = json_of(response).await;
    Observed {
        status,
        request_id,
        correlation,
        body,
        capture: capture.take(),
    }
}

fn error_request_id(body: &Value) -> Option<&str> {
    body["error"]["request_id"].as_str()
}

#[tokio::test]
#[serial]
async fn success_and_failure_paths_share_root_correlation_without_leaking_sentinels() {
    isolate_provider_environment();
    let fixture = ObsFixture::provision().await;
    let simulator = OpenAiSimulator::start().await;
    let (capture, _guard) = DebugCapture::install();
    let app = production_router_with_auth(
        &simulator,
        fixture.auth_service(),
        MetricsConfig::disabled(),
    );
    let credentials = [
        fixture.inference.credential.as_str(),
        fixture.management.credential.as_str(),
    ];

    simulator.enqueue_chat(ScriptedResponse::chat_ok());
    let success = observe(
        &capture,
        app.clone(),
        route_request(
            Some(&fixture.inference.credential),
            &format!("{CORRELATION_PREFIX}success"),
            sentinel_body(),
        ),
    )
    .await;
    assert_eq!(success.status, StatusCode::OK);
    assert_eq!(success.correlation.as_deref(), Some("obs-corr-success"));
    assert_correlated(
        &success.capture,
        &success.request_id,
        Some("obs-corr-success"),
        None,
    );
    assert_omits_protected(&success.capture, &credentials);
    assert!(simulator.generation_count() >= 1);

    let missing = observe(
        &capture,
        app.clone(),
        route_request(
            None,
            &format!("{CORRELATION_PREFIX}missing"),
            sentinel_body(),
        ),
    )
    .await;
    assert_eq!(missing.status, StatusCode::UNAUTHORIZED);
    assert_eq!(missing.body["error"]["code"], "UNAUTHORIZED");
    assert_eq!(missing.correlation.as_deref(), Some("obs-corr-missing"));
    assert_correlated(
        &missing.capture,
        &missing.request_id,
        Some("obs-corr-missing"),
        error_request_id(&missing.body),
    );
    assert_omits_protected(&missing.capture, &credentials);
    assert_no_execution_logs(&missing.capture);
    assert!(auth_duration_ms(&json_events(&missing.capture)).is_some());

    let invalid = observe(
        &capture,
        app.clone(),
        route_request(
            Some(INVALID_KEY_SENTINEL),
            &format!("{CORRELATION_PREFIX}invalid"),
            sentinel_body(),
        ),
    )
    .await;
    assert_eq!(invalid.status, StatusCode::UNAUTHORIZED);
    assert_eq!(invalid.body["error"]["code"], "UNAUTHORIZED");
    assert_correlated(
        &invalid.capture,
        &invalid.request_id,
        Some("obs-corr-invalid"),
        error_request_id(&invalid.body),
    );
    assert_omits_protected(&invalid.capture, &credentials);
    assert_no_execution_logs(&invalid.capture);
    assert!(auth_duration_ms(&json_events(&invalid.capture)).is_some());

    let forbidden = observe(
        &capture,
        app.clone(),
        route_request(
            Some(&fixture.management.credential),
            &format!("{CORRELATION_PREFIX}forbidden"),
            sentinel_body(),
        ),
    )
    .await;
    assert_eq!(forbidden.status, StatusCode::FORBIDDEN);
    assert_eq!(forbidden.body["error"]["code"], "FORBIDDEN");
    assert_correlated(
        &forbidden.capture,
        &forbidden.request_id,
        Some("obs-corr-forbidden"),
        error_request_id(&forbidden.body),
    );
    assert_omits_protected(&forbidden.capture, &credentials);
    assert_no_execution_logs(&forbidden.capture);

    let malformed = observe(
        &capture,
        app.clone(),
        route_request(
            Some(&fixture.inference.credential),
            &format!("{CORRELATION_PREFIX}malformed"),
            "{".to_string(),
        ),
    )
    .await;
    assert_eq!(malformed.status, StatusCode::BAD_REQUEST);
    assert_eq!(malformed.body["error"]["code"], "INVALID_JSON");
    assert_correlated(
        &malformed.capture,
        &malformed.request_id,
        Some("obs-corr-malformed"),
        error_request_id(&malformed.body),
    );
    assert_omits_protected(&malformed.capture, &credentials);
    assert_no_execution_logs(&malformed.capture);

    simulator.enqueue_chat(ScriptedResponse::json_status(
        401,
        json!({"error":{"message": UPSTREAM_BODY_SENTINEL}}),
    ));
    let generations_before_upstream = simulator.generation_count();
    let upstream = observe(
        &capture,
        app.clone(),
        route_request(
            Some(&fixture.inference.credential),
            &format!("{CORRELATION_PREFIX}upstream"),
            sentinel_body(),
        ),
    )
    .await;
    assert_eq!(upstream.status, StatusCode::BAD_GATEWAY);
    assert_eq!(upstream.body["error"]["code"], "UPSTREAM_AUTHENTICATION");
    assert_correlated(
        &upstream.capture,
        &upstream.request_id,
        Some("obs-corr-upstream"),
        error_request_id(&upstream.body),
    );
    assert_omits_protected(&upstream.capture, &credentials);
    assert!(simulator.generation_count() > generations_before_upstream);

    let down_app = production_router_with_auth(
        &simulator,
        Arc::new(AuthService::new(Arc::new(UnavailableAuthStore))),
        MetricsConfig::disabled(),
    );
    let generations_before_down = simulator.generation_count();
    let down = observe(
        &capture,
        down_app,
        route_request(
            Some(&fixture.inference.credential),
            &format!("{CORRELATION_PREFIX}store"),
            sentinel_body(),
        ),
    )
    .await;
    assert_eq!(down.status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(down.body["error"]["code"], "AUTH_DEPENDENCY_UNAVAILABLE");
    assert_correlated(
        &down.capture,
        &down.request_id,
        Some("obs-corr-store"),
        error_request_id(&down.body),
    );
    assert_omits_protected(&down.capture, &credentials);
    assert_no_execution_logs(&down.capture);
    assert!(auth_duration_ms(&json_events(&down.capture)).is_some());
    assert_eq!(simulator.generation_count(), generations_before_down);

    fixture.drop_rows().await;
}

#[tokio::test]
#[serial]
async fn local_overload_keeps_root_correlation_without_provider_attempts() {
    isolate_provider_environment();
    let fixture = ObsFixture::provision().await;
    let simulator = OpenAiSimulator::start().await;
    let (capture, _guard) = DebugCapture::install();
    let app = production_router_with_settings(
        settings_for_simulator_with(&simulator, |settings| {
            settings.limits.max_concurrent_generations = 1;
            settings.limits.retries_per_target = 0;
        }),
        fixture.auth_service(),
        MetricsConfig::disabled(),
    );

    let (held, hold) = ScriptedResponse::chat_ok().hold();
    simulator.enqueue_chat(held);
    let admitted = {
        let app = app.clone();
        let credential = fixture.inference.credential.clone();
        tokio::spawn(async move {
            send(
                app,
                route_request(
                    Some(&credential),
                    &format!("{CORRELATION_PREFIX}held"),
                    sentinel_body(),
                ),
            )
            .await
        })
    };
    let started = Instant::now();
    while simulator.generation_count() < 1 {
        assert!(
            started.elapsed() < GENERATION_WAIT_BOUND,
            "timed out waiting for the held generation"
        );
        sleep(Duration::from_millis(5)).await;
    }
    let _ = capture.take();

    let overloaded = observe(
        &capture,
        app,
        route_request(
            Some(&fixture.inference.credential),
            &format!("{CORRELATION_PREFIX}overload"),
            sentinel_body(),
        ),
    )
    .await;
    assert_eq!(overloaded.status, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(overloaded.body["error"]["code"], "OVERLOADED");
    assert_eq!(overloaded.correlation.as_deref(), Some("obs-corr-overload"));
    assert_correlated(
        &overloaded.capture,
        &overloaded.request_id,
        Some("obs-corr-overload"),
        error_request_id(&overloaded.body),
    );
    assert_omits_protected(
        &overloaded.capture,
        &[fixture.inference.credential.as_str()],
    );
    assert_no_execution_logs(&overloaded.capture);
    assert_eq!(simulator.generation_count(), 1);

    hold.release();
    let admitted = admitted.await.expect("held join");
    assert_eq!(admitted.status(), StatusCode::OK);
    fixture.drop_rows().await;
}

#[tokio::test]
#[serial]
async fn auth_duration_excludes_simulator_delay_and_includes_store_delay() {
    isolate_provider_environment();
    let fixture = ObsFixture::provision().await;
    let simulator = OpenAiSimulator::start().await;
    let (capture, _guard) = DebugCapture::install();
    let app = production_router_with_auth(
        &simulator,
        fixture.auth_service(),
        MetricsConfig::disabled(),
    );

    simulator.enqueue_chat(ScriptedResponse::chat_ok());
    let fast_started = Instant::now();
    let fast = observe(
        &capture,
        app.clone(),
        route_request(
            Some(&fixture.inference.credential),
            &format!("{CORRELATION_PREFIX}fast"),
            sentinel_body(),
        ),
    )
    .await;
    let fast_e2e = fast_started.elapsed();
    assert_eq!(fast.status, StatusCode::OK);
    let fast_auth = auth_duration_ms(&json_events(&fast.capture))
        .expect("fast path must record authentication duration");
    let fast_processing = fast.body["processing_time_ms"].as_u64().unwrap_or(0);
    assert_omits_protected(&fast.capture, &[fixture.inference.credential.as_str()]);

    simulator.enqueue_chat(ScriptedResponse::chat_ok().delay_headers(SIMULATOR_DELAY));
    let slow_started = Instant::now();
    let slow = observe(
        &capture,
        app,
        route_request(
            Some(&fixture.inference.credential),
            &format!("{CORRELATION_PREFIX}slow"),
            sentinel_body(),
        ),
    )
    .await;
    let slow_e2e = slow_started.elapsed();
    assert_eq!(slow.status, StatusCode::OK);
    let slow_auth = auth_duration_ms(&json_events(&slow.capture))
        .expect("slow path must record authentication duration");
    let slow_processing = slow.body["processing_time_ms"].as_u64().unwrap_or(0);

    let delay_ms = SIMULATOR_DELAY.as_millis() as u64;
    let tolerance_ms = AUTH_DELAY_TOLERANCE.as_millis() as u64;
    assert!(
        slow_e2e >= fast_e2e + SIMULATOR_DELAY - AUTH_DELAY_TOLERANCE,
        "slow simulator must increase end-to-end time"
    );
    assert!(
        slow_processing >= fast_processing.saturating_add(delay_ms.saturating_sub(50)),
        "slow simulator must increase execution time"
    );
    let auth_delta = slow_auth.abs_diff(fast_auth);
    assert!(
        auth_delta < delay_ms.saturating_sub(tolerance_ms),
        "authentication duration must not include simulator delay"
    );

    let delayed_app = production_router_with_auth(
        &simulator,
        Arc::new(AuthService::new(Arc::new(DelayedAuthStore {
            inner: PostgresAuthStore::new(fixture.pool.clone()),
            delay: AUTH_STORE_DELAY,
        }))),
        MetricsConfig::disabled(),
    );
    simulator.enqueue_chat(ScriptedResponse::chat_ok());
    let delayed = observe(
        &capture,
        delayed_app,
        route_request(
            Some(&fixture.inference.credential),
            &format!("{CORRELATION_PREFIX}authdelay"),
            sentinel_body(),
        ),
    )
    .await;
    assert_eq!(delayed.status, StatusCode::OK);
    let delayed_auth = auth_duration_ms(&json_events(&delayed.capture))
        .expect("delayed auth must record authentication duration");
    assert!(
        delayed_auth >= AUTH_STORE_DELAY.as_millis() as u64 - 50,
        "authentication duration must include the store delay"
    );
    assert!(
        delayed_auth + 50 >= fast_auth + AUTH_STORE_DELAY.as_millis() as u64 - 50,
        "store delay must increase authentication duration"
    );
    assert_omits_protected(&delayed.capture, &[fixture.inference.credential.as_str()]);

    fixture.drop_rows().await;
}

#[tokio::test]
#[serial]
async fn missing_key_and_store_failure_report_auth_duration_without_execution() {
    isolate_provider_environment();
    let fixture = ObsFixture::provision().await;
    let simulator = OpenAiSimulator::start().await;
    let (capture, _guard) = DebugCapture::install();
    let app = production_router_with_auth(
        &simulator,
        fixture.auth_service(),
        MetricsConfig::disabled(),
    );

    let missing = observe(
        &capture,
        app,
        route_request(
            None,
            &format!("{CORRELATION_PREFIX}noauth"),
            sentinel_body(),
        ),
    )
    .await;
    assert_eq!(missing.status, StatusCode::UNAUTHORIZED);
    let missing_auth = auth_duration_ms(&json_events(&missing.capture))
        .expect("missing key must record bounded authentication duration");
    assert!(
        missing_auth < 1_000,
        "missing-key authentication must stay bounded"
    );
    assert_no_execution_logs(&missing.capture);
    assert_eq!(simulator.generation_count(), 0);

    let down_app = production_router_with_auth(
        &simulator,
        Arc::new(AuthService::new(Arc::new(UnavailableAuthStore))),
        MetricsConfig::disabled(),
    );
    let down = observe(
        &capture,
        down_app,
        route_request(
            Some(&fixture.inference.credential),
            &format!("{CORRELATION_PREFIX}nodb"),
            sentinel_body(),
        ),
    )
    .await;
    assert_eq!(down.status, StatusCode::SERVICE_UNAVAILABLE);
    let down_auth = auth_duration_ms(&json_events(&down.capture))
        .expect("store failure must record bounded authentication duration");
    assert!(
        down_auth < 1_000,
        "store-failure authentication must stay bounded"
    );
    assert_no_execution_logs(&down.capture);
    assert_eq!(simulator.generation_count(), 0);

    fixture.drop_rows().await;
}

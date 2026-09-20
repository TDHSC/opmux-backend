//! Bounded-cardinality execution metrics through the production router.
//!
//! Scrapes `/metrics` before and after isolated simulator scenarios. Evidence
//! omits credentials, prompts, metadata, and raw provider bodies.

mod support;

use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use gateway::{
    core::config::{Route, Settings},
    core::metrics::{
        MetricsConfig, CIRCUIT_STATE, CIRCUIT_TRANSITIONS_TOTAL, DEADLINE_EXCEEDED_TOTAL,
        EXECUTION_ATTEMPTS_TOTAL, EXECUTION_FALLBACKS_TOTAL, EXECUTION_RETRIES_TOTAL,
        OVERLOAD_REJECTED_TOTAL, SUCCESSFUL_COMPLETION_TOKENS_TOTAL,
        SUCCESSFUL_PROMPT_TOKENS_TOTAL,
    },
    features::auth::{ApiKeyKind, PostgresAuthStore, ProvisioningService},
};
use serial_test::serial;
use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, Instant};
use support::{
    cleanup_clients, isolate_provider_environment, production_router_with_settings,
    provision_inference_key, settings_for_simulator_with, test_pool, IssuedInference,
    OpenAiSimulator, ScriptedResponse,
};
use tokio::time::sleep;
use tower::ServiceExt;
use uuid::Uuid;

const PROMPT_TOKENS: i64 = 120;
const COMPLETION_TOKENS: i64 = 30;
const UNKNOWN_ROUTE: &str = "obs002-unknown-route";
const REPORTED_MODEL: &str = "obs002-reported-model";
const CORRELATION: &str = "obs002-corr-unique";
const PROMPT_SENTINEL: &str = "OBS002_PROMPT_SENTINEL";

struct Fixture {
    pool: sqlx::PgPool,
    issued: IssuedInference,
    simulator: OpenAiSimulator,
    app: axum::Router,
}

impl Fixture {
    async fn new(mutate: impl FnOnce(&mut Settings)) -> Self {
        isolate_provider_environment();
        let pool = test_pool().await;
        let issued = provision_inference_key(&pool).await;
        let simulator = OpenAiSimulator::start().await;
        let settings = settings_for_simulator_with(&simulator, |settings| {
            settings.limits.backoff_cap = Duration::from_millis(1);
            mutate(settings);
        });
        let app = production_router_with_settings(
            settings,
            support::auth_service_from_pool(pool.clone()),
            MetricsConfig::production(),
        );
        Self {
            pool,
            issued,
            simulator,
            app,
        }
    }

    async fn drop_rows(&self) {
        cleanup_clients(&self.pool, &[self.issued.client_id]).await;
    }
}

async fn scrape(app: axum::Router) -> String {
    scrape_with_correlation(app, "metrics-scrape").await.1
}

async fn scrape_with_correlation(
    app: axum::Router,
    correlation: &str,
) -> (axum::http::HeaderMap, String) {
    let response = app
        .oneshot(
            Request::builder()
                .uri("/metrics")
                .header("X-Correlation-ID", correlation)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert!(response.headers().contains_key("X-Request-ID"));
    assert_eq!(
        response.headers().get("X-Correlation-ID").unwrap(),
        correlation
    );
    let headers = response.headers().clone();
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("metrics body");
    (
        headers,
        String::from_utf8(body.to_vec()).expect("metrics utf8"),
    )
}

async fn post_route(
    app: axum::Router,
    credential: Option<&str>,
    body: serde_json::Value,
    correlation: &str,
) -> axum::http::Response<Body> {
    let mut builder = Request::builder()
        .method("POST")
        .uri("/api/v1/route")
        .header("content-type", "application/json")
        .header("X-Correlation-ID", correlation);
    if let Some(credential) = credential {
        builder = builder.header("x-api-key", credential);
    }
    app.oneshot(builder.body(Body::from(body.to_string())).unwrap())
        .await
        .unwrap()
}

fn route_body() -> serde_json::Value {
    serde_json::json!({ "prompt": "metrics-ok", "metadata": {} })
}

fn sample(text: &str, name: &str, labels: &[(&str, &str)]) -> f64 {
    for line in text.lines() {
        if line.starts_with('#') || line.is_empty() {
            continue;
        }
        let (metric, value) = split_sample(line);
        if metric_name(metric) != name {
            continue;
        }
        if labels_match(metric, labels) {
            return value;
        }
    }
    0.0
}

fn delta(before: &str, after: &str, name: &str, labels: &[(&str, &str)]) -> f64 {
    sample(after, name, labels) - sample(before, name, labels)
}

fn split_sample(line: &str) -> (&str, f64) {
    let line = line.trim();
    let Some(space) = line.rfind(' ') else {
        panic!("metric line without value: {line}");
    };
    let metric = &line[..space];
    let rest = line[space + 1..].trim();
    let value_token = rest.split_whitespace().next().unwrap_or(rest);
    let value: f64 = value_token.parse().unwrap_or(0.0);
    (metric, value)
}

fn metric_name(metric: &str) -> &str {
    metric.split('{').next().unwrap_or(metric)
}

fn parse_labels(metric: &str) -> BTreeMap<String, String> {
    let Some(start) = metric.find('{') else {
        return BTreeMap::new();
    };
    let Some(end) = metric.rfind('}') else {
        return BTreeMap::new();
    };
    let mut labels = BTreeMap::new();
    for part in metric[start + 1..end].split(',') {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        let Some((key, value)) = part.split_once('=') else {
            continue;
        };
        let value = value.trim().trim_matches('"');
        labels.insert(key.trim().to_string(), value.to_string());
    }
    labels
}

fn labels_match(metric: &str, expected: &[(&str, &str)]) -> bool {
    let labels = parse_labels(metric);
    expected
        .iter()
        .all(|(key, value)| labels.get(*key).map(String::as_str) == Some(*value))
}

fn emitted_label_values(text: &str) -> Vec<(String, String, String)> {
    let mut out = Vec::new();
    for line in text.lines() {
        if line.starts_with('#') || line.is_empty() {
            continue;
        }
        let (metric, _) = split_sample(line);
        let name = metric_name(metric).to_string();
        for (key, value) in parse_labels(metric) {
            out.push((name.clone(), key, value));
        }
    }
    out
}

fn assert_no_identifying(text: &str, sentinels: &[&str]) {
    let encoded = text.to_string();
    for sentinel in sentinels {
        assert!(
            !encoded.contains(sentinel),
            "metrics contained identifying sentinel"
        );
    }
}

fn http_total(status: &'static str) -> [(&'static str, &'static str); 3] {
    [
        ("endpoint", "/api/v1/route"),
        ("method", "POST"),
        ("status", status),
    ]
}

fn upstream_500() -> ScriptedResponse {
    ScriptedResponse::json_status(
        500,
        serde_json::json!({"error":{"message":"simulated transient"}}),
    )
}

fn chat_usage() -> ScriptedResponse {
    ScriptedResponse::chat_ok_with(None, PROMPT_TOKENS, COMPLETION_TOKENS)
}

#[tokio::test]
#[serial]
async fn success_records_attempt_usage_and_http_counts() {
    let fixture = Fixture::new(|_| {}).await;
    fixture.simulator.enqueue_chat(chat_usage());
    let before = scrape(fixture.app.clone()).await;
    let generations = fixture.simulator.generation_count();

    let response = post_route(
        fixture.app.clone(),
        Some(&fixture.issued.credential),
        route_body(),
        "obs002-success",
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(fixture.simulator.generation_count(), generations + 1);

    let after = scrape(fixture.app.clone()).await;
    assert_eq!(
        delta(
            &before,
            &after,
            EXECUTION_ATTEMPTS_TOTAL,
            &[("outcome", "success"), ("target", "primary")]
        ),
        1.0
    );
    assert_eq!(
        delta(
            &before,
            &after,
            EXECUTION_RETRIES_TOTAL,
            &[("target", "primary")]
        ),
        0.0
    );
    assert_eq!(
        delta(
            &before,
            &after,
            SUCCESSFUL_PROMPT_TOKENS_TOTAL,
            &[("target", "primary")]
        ),
        PROMPT_TOKENS as f64
    );
    assert_eq!(
        delta(
            &before,
            &after,
            SUCCESSFUL_COMPLETION_TOKENS_TOTAL,
            &[("target", "primary")]
        ),
        COMPLETION_TOKENS as f64
    );
    assert!(
        delta(
            &before,
            &after,
            "gateway_http_requests_total",
            &http_total("200")
        ) >= 1.0
    );
    assert!(
        delta(
            &before,
            &after,
            "gateway_http_requests_duration_seconds_count",
            &http_total("200")
        ) >= 1.0
    );
    fixture.drop_rows().await;
}

#[tokio::test]
#[serial]
async fn retry_then_success_is_distinguishable_from_fallback() {
    let fixture = Fixture::new(|settings| {
        settings.limits.retries_per_target = 1;
        settings.limits.max_total_attempts = 3;
    })
    .await;
    fixture.simulator.enqueue_chat(upstream_500());
    fixture.simulator.enqueue_chat(chat_usage());
    let before = scrape(fixture.app.clone()).await;
    let generations = fixture.simulator.generation_count();

    let response = post_route(
        fixture.app.clone(),
        Some(&fixture.issued.credential),
        route_body(),
        "obs002-retry",
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(fixture.simulator.generation_count(), generations + 2);

    let after = scrape(fixture.app.clone()).await;
    assert_eq!(
        delta(
            &before,
            &after,
            EXECUTION_ATTEMPTS_TOTAL,
            &[("outcome", "retryable"), ("target", "primary")]
        ),
        1.0
    );
    assert_eq!(
        delta(
            &before,
            &after,
            EXECUTION_ATTEMPTS_TOTAL,
            &[("outcome", "success"), ("target", "primary")]
        ),
        1.0
    );
    assert_eq!(
        delta(
            &before,
            &after,
            EXECUTION_RETRIES_TOTAL,
            &[("target", "primary")]
        ),
        1.0
    );
    assert_eq!(
        delta(
            &before,
            &after,
            EXECUTION_FALLBACKS_TOTAL,
            &[("from_target", "primary"), ("to_target", "secondary")]
        ),
        0.0
    );
    assert_eq!(
        delta(
            &before,
            &after,
            SUCCESSFUL_PROMPT_TOKENS_TOTAL,
            &[("target", "primary")]
        ),
        PROMPT_TOKENS as f64
    );
    fixture.drop_rows().await;
}

#[tokio::test]
#[serial]
async fn fallback_success_counts_primary_failure_and_later_target() {
    let fixture = Fixture::new(|settings| {
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
            .fallbacks = vec!["secondary".to_string()];
    })
    .await;
    fixture.simulator.enqueue_chat(upstream_500());
    fixture.simulator.enqueue_chat(chat_usage());
    let before = scrape(fixture.app.clone()).await;
    let generations = fixture.simulator.generation_count();

    let response = post_route(
        fixture.app.clone(),
        Some(&fixture.issued.credential),
        route_body(),
        "obs002-fallback",
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(fixture.simulator.generation_count(), generations + 2);

    let after = scrape(fixture.app.clone()).await;
    assert_eq!(
        delta(
            &before,
            &after,
            EXECUTION_ATTEMPTS_TOTAL,
            &[("outcome", "retryable"), ("target", "primary")]
        ),
        1.0
    );
    assert_eq!(
        delta(
            &before,
            &after,
            EXECUTION_ATTEMPTS_TOTAL,
            &[("outcome", "success"), ("target", "secondary")]
        ),
        1.0
    );
    assert_eq!(
        delta(
            &before,
            &after,
            EXECUTION_FALLBACKS_TOTAL,
            &[("from_target", "primary"), ("to_target", "secondary")]
        ),
        1.0
    );
    assert_eq!(
        delta(
            &before,
            &after,
            EXECUTION_RETRIES_TOTAL,
            &[("target", "primary")]
        ),
        0.0
    );
    assert_eq!(
        delta(
            &before,
            &after,
            SUCCESSFUL_PROMPT_TOKENS_TOTAL,
            &[("target", "secondary")]
        ),
        PROMPT_TOKENS as f64
    );
    assert_eq!(
        delta(
            &before,
            &after,
            SUCCESSFUL_PROMPT_TOKENS_TOTAL,
            &[("target", "primary")]
        ),
        0.0
    );
    fixture.drop_rows().await;
}

#[tokio::test]
#[serial]
async fn circuit_open_and_recovery_are_target_scoped() {
    let fixture = Fixture::new(|settings| {
        settings.limits.retries_per_target = 0;
        settings.limits.max_total_attempts = 1;
        settings.limits.circuit_failure_threshold = 1;
        settings.limits.circuit_cooldown = Duration::from_millis(80);
        settings.limits.max_fallback_targets = 0;
        settings.catalog.routes.insert(
            "default".to_string(),
            Route {
                primary: "primary".to_string(),
                fallbacks: Vec::new(),
            },
        );
    })
    .await;
    fixture.simulator.enqueue_chat(upstream_500());
    let before = scrape(fixture.app.clone()).await;
    let generations = fixture.simulator.generation_count();

    let opened = post_route(
        fixture.app.clone(),
        Some(&fixture.issued.credential),
        route_body(),
        "obs002-circuit-open",
    )
    .await;
    assert_eq!(opened.status(), StatusCode::BAD_GATEWAY);
    assert_eq!(fixture.simulator.generation_count(), generations + 1);

    let skipped = post_route(
        fixture.app.clone(),
        Some(&fixture.issued.credential),
        route_body(),
        "obs002-circuit-skip",
    )
    .await;
    assert_eq!(skipped.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(fixture.simulator.generation_count(), generations + 1);

    let after_open = scrape(fixture.app.clone()).await;
    assert_eq!(
        delta(
            &before,
            &after_open,
            CIRCUIT_TRANSITIONS_TOTAL,
            &[("target", "primary"), ("to_state", "open")]
        ),
        1.0
    );
    assert_eq!(
        sample(&after_open, CIRCUIT_STATE, &[("target", "primary")]),
        2.0
    );
    assert_eq!(
        delta(
            &before,
            &after_open,
            EXECUTION_ATTEMPTS_TOTAL,
            &[("outcome", "retryable"), ("target", "primary")]
        ),
        1.0
    );

    sleep(Duration::from_millis(120)).await;
    fixture.simulator.enqueue_chat(chat_usage());
    let recovered = post_route(
        fixture.app.clone(),
        Some(&fixture.issued.credential),
        route_body(),
        "obs002-circuit-recover",
    )
    .await;
    assert_eq!(recovered.status(), StatusCode::OK);
    assert_eq!(fixture.simulator.generation_count(), generations + 2);

    let after = scrape(fixture.app.clone()).await;
    assert_eq!(
        delta(
            &after_open,
            &after,
            CIRCUIT_TRANSITIONS_TOTAL,
            &[("target", "primary"), ("to_state", "half_open")]
        ),
        1.0
    );
    assert_eq!(
        delta(
            &after_open,
            &after,
            CIRCUIT_TRANSITIONS_TOTAL,
            &[("target", "primary"), ("to_state", "closed")]
        ),
        1.0
    );
    assert_eq!(sample(&after, CIRCUIT_STATE, &[("target", "primary")]), 0.0);
    assert_eq!(
        delta(
            &after_open,
            &after,
            SUCCESSFUL_COMPLETION_TOKENS_TOTAL,
            &[("target", "primary")]
        ),
        COMPLETION_TOKENS as f64
    );
    fixture.drop_rows().await;
}

#[tokio::test]
#[serial]
async fn overall_deadline_records_without_counting_usage() {
    let fixture = Fixture::new(|settings| {
        settings.limits.retries_per_target = 0;
        settings.limits.max_total_attempts = 1;
        settings.limits.protected_request_deadline = Duration::from_millis(200);
        settings.limits.max_attempt_timeout = Duration::from_millis(200);
    })
    .await;
    fixture
        .simulator
        .enqueue_chat(chat_usage().delay_headers(Duration::from_secs(2)));
    let before = scrape(fixture.app.clone()).await;
    let generations = fixture.simulator.generation_count();
    let started = Instant::now();

    let response = post_route(
        fixture.app.clone(),
        Some(&fixture.issued.credential),
        route_body(),
        "obs002-deadline",
    )
    .await;
    assert_eq!(response.status(), StatusCode::GATEWAY_TIMEOUT);
    assert!(started.elapsed() < Duration::from_secs(2));
    assert!(fixture.simulator.generation_count() <= generations + 1);

    let after = scrape(fixture.app.clone()).await;
    assert!(delta(&before, &after, DEADLINE_EXCEEDED_TOTAL, &[]) >= 1.0);
    assert_eq!(
        delta(
            &before,
            &after,
            SUCCESSFUL_PROMPT_TOKENS_TOTAL,
            &[("target", "primary")]
        ),
        0.0
    );
    fixture.drop_rows().await;
}

#[tokio::test]
#[serial]
async fn local_overload_is_not_an_attempt_or_provider_rate_limit() {
    let fixture = Fixture::new(|settings| {
        settings.limits.max_concurrent_generations = 1;
        settings.limits.retries_per_target = 0;
    })
    .await;
    let (held, hold) = ScriptedResponse::chat_ok().hold();
    fixture.simulator.enqueue_chat(held);
    let admitted = tokio::spawn({
        let app = fixture.app.clone();
        let credential = fixture.issued.credential.clone();
        async move { post_route(app, Some(&credential), route_body(), "obs002-held").await }
    });
    let wait_started = Instant::now();
    while fixture.simulator.generation_count() < 1 {
        assert!(wait_started.elapsed() < Duration::from_secs(2));
        sleep(Duration::from_millis(5)).await;
    }
    let before = scrape(fixture.app.clone()).await;
    let generations = fixture.simulator.generation_count();

    let extra = post_route(
        fixture.app.clone(),
        Some(&fixture.issued.credential),
        route_body(),
        "obs002-overload",
    )
    .await;
    assert_eq!(extra.status(), StatusCode::TOO_MANY_REQUESTS);
    let extra_bytes = axum::body::to_bytes(extra.into_body(), usize::MAX)
        .await
        .expect("overload body");
    let extra_json: serde_json::Value =
        serde_json::from_slice(&extra_bytes).expect("json");
    assert_eq!(extra_json["error"]["code"], "OVERLOADED");
    assert_ne!(extra_json["error"]["code"], "UPSTREAM_RATE_LIMIT");
    assert_eq!(fixture.simulator.generation_count(), generations);

    let after = scrape(fixture.app.clone()).await;
    assert_eq!(delta(&before, &after, OVERLOAD_REJECTED_TOTAL, &[]), 1.0);
    assert_eq!(
        delta(
            &before,
            &after,
            EXECUTION_ATTEMPTS_TOTAL,
            &[("outcome", "rate_limit"), ("target", "primary")]
        ),
        0.0
    );
    assert_eq!(
        delta(
            &before,
            &after,
            EXECUTION_ATTEMPTS_TOTAL,
            &[("outcome", "success"), ("target", "primary")]
        ),
        0.0
    );

    hold.release();
    let held_response = admitted.await.expect("held join");
    assert_eq!(held_response.status(), StatusCode::OK);

    fixture
        .simulator
        .enqueue_chat(ScriptedResponse::rate_limited("1"));
    let throttled_before = scrape(fixture.app.clone()).await;
    let throttled = post_route(
        fixture.app.clone(),
        Some(&fixture.issued.credential),
        route_body(),
        "obs002-upstream-429",
    )
    .await;
    assert_eq!(throttled.status(), StatusCode::TOO_MANY_REQUESTS);
    let throttled_bytes = axum::body::to_bytes(throttled.into_body(), usize::MAX)
        .await
        .expect("throttled body");
    let throttled_json: serde_json::Value =
        serde_json::from_slice(&throttled_bytes).expect("json");
    assert_eq!(throttled_json["error"]["code"], "UPSTREAM_RATE_LIMIT");
    let throttled_after = scrape(fixture.app.clone()).await;
    assert_eq!(
        delta(
            &throttled_before,
            &throttled_after,
            EXECUTION_ATTEMPTS_TOTAL,
            &[("outcome", "rate_limit"), ("target", "primary")]
        ),
        1.0
    );
    assert_eq!(
        delta(
            &throttled_before,
            &throttled_after,
            OVERLOAD_REJECTED_TOTAL,
            &[]
        ),
        0.0
    );
    fixture.drop_rows().await;
}

#[tokio::test]
#[serial]
async fn identifying_values_never_become_metric_labels() {
    isolate_provider_environment();
    let pool = test_pool().await;
    let service =
        ProvisioningService::new(Arc::new(PostgresAuthStore::new(pool.clone())));
    let created = service
        .create_tenant(&format!("obs002-{}", Uuid::new_v4().simple()))
        .await
        .expect("tenant");
    let inference = service
        .issue_key(
            created.key.client_id,
            ApiKeyKind::Inference,
            "obs002-inference",
        )
        .await
        .expect("inference");
    let management_id = created.key.key_id;
    let management_secret = created.key.credential().to_string();
    let inference_secret = inference.credential().to_string();
    let simulator = OpenAiSimulator::start().await;
    let settings = settings_for_simulator_with(&simulator, |settings| {
        settings.limits.backoff_cap = Duration::from_millis(1);
    });
    let app = production_router_with_settings(
        settings,
        support::auth_service_from_pool(pool.clone()),
        MetricsConfig::production(),
    );

    simulator.enqueue_chat(ScriptedResponse::chat_ok_with(
        Some(REPORTED_MODEL),
        PROMPT_TOKENS,
        COMPLETION_TOKENS,
    ));
    let _ = post_route(
        app.clone(),
        Some(&inference_secret),
        serde_json::json!({
            "prompt": PROMPT_SENTINEL,
            "metadata": { "note": "obs002-meta" }
        }),
        CORRELATION,
    )
    .await;
    let missing = post_route(app.clone(), None, route_body(), "obs002-missing-key").await;
    assert_eq!(missing.status(), StatusCode::UNAUTHORIZED);
    let unknown = post_route(
        app.clone(),
        Some(&inference_secret),
        serde_json::json!({
            "prompt": "ok",
            "metadata": {},
            "route": UNKNOWN_ROUTE
        }),
        "obs002-unknown",
    )
    .await;
    assert_eq!(unknown.status(), StatusCode::BAD_REQUEST);
    let revoke = app
        .clone()
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri(format!("/api/v1/auth/keys/{management_id}"))
                .header("x-api-key", &management_secret)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert!(
        revoke.status() == StatusCode::NO_CONTENT
            || revoke.status() == StatusCode::NOT_FOUND
    );

    let body = scrape(app.clone()).await;
    assert_no_identifying(
        &body,
        &[
            CORRELATION,
            UNKNOWN_ROUTE,
            REPORTED_MODEL,
            PROMPT_SENTINEL,
            &management_id.to_string(),
            &inference.key_id.to_string(),
            &created.key.client_id.to_string(),
        ],
    );
    assert!(!body.contains(&inference_secret));
    assert!(!body.contains(&management_secret));

    let allowed_endpoints = [
        "/",
        "/health",
        "/ready",
        "/metrics",
        "/api/v1/route",
        "/api/v1/auth/keys",
        "/api/v1/auth/keys/{id}",
        "unmatched",
    ];
    let allowed_outcomes = [
        "success",
        "retryable",
        "timeout",
        "rate_limit",
        "quota",
        "upstream_auth",
        "protocol",
        "rejected",
        "deadline",
        "circuit_open",
        "internal",
    ];
    let allowed_states = ["closed", "half_open", "open"];
    let allowed_targets = ["primary", "secondary", "unknown"];
    for (metric, key, value) in emitted_label_values(&body) {
        match key.as_str() {
            "endpoint" => assert!(
                allowed_endpoints.contains(&value.as_str()),
                "unbounded endpoint label"
            ),
            "method" => assert!(
                [
                    "GET", "POST", "PUT", "DELETE", "HEAD", "OPTIONS", "PATCH", "TRACE",
                    "CONNECT", ""
                ]
                .contains(&value.as_str()),
                "unbounded method label"
            ),
            "status" => {
                let parsed: u16 = value.parse().expect("status");
                assert!((100..600).contains(&parsed));
            }
            "outcome" => assert!(
                allowed_outcomes.contains(&value.as_str()),
                "unbounded outcome label"
            ),
            "target" | "from_target" | "to_target" => assert!(
                allowed_targets.contains(&value.as_str()),
                "unbounded target label"
            ),
            "to_state" => assert!(
                allowed_states.contains(&value.as_str()),
                "unbounded circuit state label"
            ),
            "le" => {
                assert!(
                    value == "+Inf" || value.parse::<f64>().is_ok(),
                    "unbounded histogram bucket"
                );
            }
            other => panic!("unexpected label key {other} on {metric}"),
        }
    }
    assert!(body.contains("gateway_http_requests_total"));
    assert!(
        delta(
            "",
            &body,
            "gateway_http_requests_total",
            &[
                ("endpoint", "/api/v1/route"),
                ("method", "POST"),
                ("status", "401")
            ]
        ) >= 1.0
    );

    cleanup_clients(&pool, &[created.key.client_id]).await;
}

#[tokio::test]
#[serial]
async fn metrics_endpoint_preserves_correlation() {
    let fixture = Fixture::new(|_| {}).await;
    let health = fixture
        .app
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
    let (headers, body) =
        scrape_with_correlation(fixture.app.clone(), "obs002-metrics-corr").await;
    assert!(headers.contains_key("X-Request-ID"));
    assert_eq!(
        headers.get("X-Correlation-ID").unwrap(),
        "obs002-metrics-corr"
    );
    assert!(body.contains("gateway_http_requests_total"));
    fixture.drop_rows().await;
}

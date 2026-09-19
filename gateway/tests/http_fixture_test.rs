//! Production-router HTTP fixtures against an owned loopback OpenAI simulator.

mod support;

use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use gateway::core::metrics::MetricsConfig;
use serial_test::serial;
use std::time::Duration;
use support::{
    isolate_provider_environment, production_router_for_simulator, OpenAiSimulator,
    MOCK_GATEWAY_API_KEY, SIMULATED_CONTENT,
};
use tower::ServiceExt;

async fn body_string(response: axum::http::Response<Body>) -> String {
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("response body");
    String::from_utf8(bytes.to_vec()).expect("utf8 body")
}

async fn run_health_and_generation_fixture() {
    isolate_provider_environment();
    let simulator = OpenAiSimulator::start().await;
    assert!(simulator.addr().ip().is_loopback());
    assert!(!simulator.addr().ip().is_unspecified());

    let app = production_router_for_simulator(&simulator, MetricsConfig::disabled());

    let health = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/health")
                .header("X-Correlation-ID", "fixture-health")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(health.status(), StatusCode::OK);
    assert!(health.headers().contains_key("X-Request-ID"));
    assert_eq!(
        health.headers().get("X-Correlation-ID").unwrap(),
        "fixture-health"
    );
    let health_body = body_string(health).await;
    assert!(health_body.contains("\"status\":\"healthy\""));

    let ready = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/ready")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(ready.status(), StatusCode::OK);
    let ready_body = body_string(ready).await;
    assert!(ready_body.contains("\"status\":\"ready\""));

    let generation = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/route")
                .header("content-type", "application/json")
                .header("x-api-key", MOCK_GATEWAY_API_KEY)
                .header("X-Correlation-ID", "fixture-generate")
                .body(Body::from(
                    r#"{"prompt":"fixture generation","metadata":{}}"#,
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(generation.status(), StatusCode::OK);
    assert!(generation.headers().contains_key("X-Request-ID"));
    assert_eq!(
        generation.headers().get("X-Correlation-ID").unwrap(),
        "fixture-generate"
    );
    let generation_body = body_string(generation).await;
    assert!(generation_body.contains(SIMULATED_CONTENT));
    assert!(generation_body.contains("\"model_used\":\"gpt-4\""));

    assert!(simulator.generation_count() >= 1);
    assert!(simulator.models_probe_count() >= 1);
    let generation_capture = simulator
        .captured()
        .into_iter()
        .find(|capture| capture.is_generation())
        .expect("chat completion capture");
    assert!(generation_capture.authorization_matches_fixture);
    assert_eq!(
        generation_capture.content_type.as_deref(),
        Some("application/json")
    );
    let model = generation_capture
        .body
        .as_ref()
        .and_then(|body| body.get("model"))
        .and_then(|value| value.as_str());
    assert_eq!(model, Some("gpt-4"));
}

#[tokio::test]
#[serial]
async fn production_router_health_and_generation_against_owned_simulator() {
    run_health_and_generation_fixture().await;
}

#[tokio::test]
#[serial]
async fn production_router_fixture_repeats_without_shared_process_state() {
    run_health_and_generation_fixture().await;
}

#[tokio::test]
#[serial]
async fn production_router_metrics_and_correlation_use_shared_builder() {
    isolate_provider_environment();
    let simulator = OpenAiSimulator::start().await;
    let app = production_router_for_simulator(&simulator, MetricsConfig::production());

    let _ = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/health")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    let metrics = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/metrics")
                .header("X-Correlation-ID", "fixture-metrics")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(metrics.status(), StatusCode::OK);
    assert!(metrics.headers().contains_key("X-Request-ID"));
    assert_eq!(
        metrics.headers().get("X-Correlation-ID").unwrap(),
        "fixture-metrics"
    );
    let metrics_body = body_string(metrics).await;
    assert!(metrics_body.contains("gateway_http_requests_total"));
}

#[tokio::test]
#[serial]
async fn simulator_binds_loopback_and_releases_on_drop() {
    isolate_provider_environment();
    let addr;
    {
        let simulator = OpenAiSimulator::start().await;
        addr = simulator.addr();
        assert!(addr.ip().is_loopback());
        assert_eq!(addr.ip().to_string(), "127.0.0.1");
    }
    let mut released = false;
    for _ in 0..20 {
        if tokio::net::TcpStream::connect(addr).await.is_err() {
            released = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    assert!(released, "owned simulator task must release its listener");
}

#[test]
fn binary_and_http_harness_share_production_router_builder() {
    let app = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/src/app.rs"));
    let main = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/src/main.rs"));
    let observability = include_str!("observability_integration_test.rs");
    let fixture = include_str!("http_fixture_test.rs");

    assert!(app.contains("pub fn build_production_router"));
    assert!(main.contains("Application::from_settings"));
    assert!(main.contains("into_router"));
    assert!(!main.contains("Router::new()"));
    let support = include_str!("support/mod.rs");
    assert!(observability.contains("build_production_router"));
    assert!(fixture.contains("production_router_for_simulator"));
    assert!(support.contains("Application::from_settings"));
    assert!(support.contains("into_router"));
}

#[test]
fn environment_wrapper_source_strips_provider_and_proxy_overrides() {
    let env_src = include_str!("support/env.rs");
    for name in [
        "OPENAI_API_KEY",
        "ANTHROPIC_API_KEY",
        "OPENAI_BASE_URL",
        "HTTP_PROXY",
        "HTTPS_PROXY",
        "ALL_PROXY",
        "http_proxy",
        "https_proxy",
        "all_proxy",
    ] {
        assert!(env_src.contains(name), "wrapper must clear {name}");
    }
    assert!(env_src.contains("NO_PROXY"));
    assert!(env_src.contains("AUTH_DEVELOPMENT_MODE"));
}

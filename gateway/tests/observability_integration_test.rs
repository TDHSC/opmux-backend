mod support;

use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use gateway::{
    app::build_production_router,
    core::{config::Settings, metrics::MetricsConfig},
    features::{
        auth::{AuthService, UnavailableAuthStore},
        executor::{config::ExecutorConfig, service::ExecutorService},
        health, ingress,
    },
    AppState,
};
use serde_json::json;
use serial_test::serial;
use std::sync::Arc;
use support::{
    auth_service_from_pool, cleanup_clients, isolate_provider_environment,
    production_router_with_auth, provision_inference_key, test_pool, OpenAiSimulator,
};
use tower::ServiceExt;

fn create_executor_service() -> Arc<ExecutorService> {
    isolate_provider_environment();
    let settings = Settings::for_tests();
    Arc::new(
        ExecutorService::from_config(ExecutorConfig::from_settings(&settings))
            .expect("executor should initialize with dummy vendor config"),
    )
}

fn build_test_app(
    health_service: Arc<health::HealthService>,
    include_metrics: bool,
) -> axum::Router {
    isolate_provider_environment();
    let settings = Arc::new(Settings::for_tests());
    let executor_service = Arc::new(
        ExecutorService::from_config(ExecutorConfig::from_settings(&settings))
            .expect("executor should initialize with dummy vendor config"),
    );
    let ingress_service = Arc::new(ingress::service::IngressService::new(
        executor_service.clone(),
        settings.clone(),
    ));

    let app_state = AppState {
        settings,
        ingress_service,
        executor_service,
        health_service,
        auth_service: Arc::new(AuthService::new(Arc::new(UnavailableAuthStore))),
    };

    let metrics = if include_metrics {
        MetricsConfig::production()
    } else {
        MetricsConfig::disabled()
    };
    build_production_router(app_state, metrics)
}

#[tokio::test]
#[serial]
async fn test_correlation_id_generation_and_preservation() {
    let app = build_test_app(Arc::new(health::HealthService::new()), false);

    let request = Request::builder()
        .uri("/health")
        .header("X-Correlation-ID", "integration-corr-123")
        .body(Body::empty())
        .unwrap();

    let response = app.oneshot(request).await.unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    assert!(response.headers().contains_key("X-Request-ID"));
    assert_eq!(
        response.headers().get("X-Correlation-ID").unwrap(),
        "integration-corr-123"
    );
}

#[tokio::test]
#[serial]
async fn test_metrics_endpoint_accessibility() {
    let app = build_test_app(Arc::new(health::HealthService::new()), true);

    let health_request = Request::builder()
        .uri("/health")
        .body(Body::empty())
        .unwrap();
    let _ = app.clone().oneshot(health_request).await.unwrap();

    let metrics_request = Request::builder()
        .uri("/metrics")
        .header("X-Correlation-ID", "metrics-corr-123")
        .body(Body::empty())
        .unwrap();
    let response = app.clone().oneshot(metrics_request).await.unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    assert!(response.headers().contains_key("X-Request-ID"));
    assert_eq!(
        response.headers().get("X-Correlation-ID").unwrap(),
        "metrics-corr-123"
    );
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let body_str = String::from_utf8(body.to_vec()).unwrap();
    assert!(body_str.contains("gateway_http_requests_total"));
    // Reuse the app because the metrics recorder can only be initialized once.
    let request = Request::builder()
        .method("POST")
        .uri("/api/v1/route")
        .header("X-Correlation-ID", "auth-corr-123")
        .header("Content-Type", "application/json")
        .body(Body::from(r#"{"prompt":"hello","metadata":{}}"#))
        .unwrap();

    let response = app.oneshot(request).await.unwrap();

    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    assert!(response.headers().contains_key("X-Request-ID"));
    assert_eq!(
        response.headers().get("X-Correlation-ID").unwrap(),
        "auth-corr-123"
    );
}

#[tokio::test]
#[serial]
async fn test_health_endpoint_response_format() {
    let app = build_test_app(Arc::new(health::HealthService::new()), false);

    let request = Request::builder()
        .uri("/health")
        .body(Body::empty())
        .unwrap();
    let response = app.oneshot(request).await.unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let body_str = String::from_utf8(body.to_vec()).unwrap();
    assert!(body_str.contains("\"status\":\"healthy\""));
    assert!(body_str.contains("\"version\""));
    assert!(body_str.contains("\"uptime_seconds\""));
}

#[tokio::test]
#[serial]
async fn test_ready_endpoint_with_healthy_dependencies() {
    isolate_provider_environment();
    let pool = test_pool().await;
    let simulator = OpenAiSimulator::start().await;
    let app = production_router_with_auth(
        &simulator,
        auth_service_from_pool(pool.clone()),
        MetricsConfig::disabled(),
    );

    let request = Request::builder()
        .uri("/ready")
        .body(Body::empty())
        .unwrap();
    let response = app.oneshot(request).await.unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let body_str = String::from_utf8(body.to_vec()).unwrap();
    assert!(body_str.contains("\"status\":\"ready\""));
    assert!(body_str.contains("\"database\""));
    assert!(body_str.contains("\"upstream\""));
    assert!(body_str.contains("\"default_route\""));
    let _ = pool;
}

#[tokio::test]
#[serial]
async fn test_ready_endpoint_with_unhealthy_dependencies() {
    let unhealthy_health_service = Arc::new(health::HealthService::with_executor(
        create_executor_service(),
    ));
    let app = build_test_app(unhealthy_health_service, false);

    let request = Request::builder()
        .uri("/ready")
        .body(Body::empty())
        .unwrap();
    let response = app.oneshot(request).await.unwrap();

    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let body_str = String::from_utf8(body.to_vec()).unwrap();
    assert!(body_str.contains("\"status\":\"not_ready\""));
    assert!(body_str.contains("\"status\":\"unhealthy\""));
}

#[tokio::test]
#[serial]
async fn test_repeated_ingress_calls_transition_to_circuit_open() {
    isolate_provider_environment();
    let pool = test_pool().await;
    let issued = provision_inference_key(&pool).await;
    let settings = Arc::new(Settings::for_tests());
    let executor_service = Arc::new(
        ExecutorService::from_config(ExecutorConfig::from_settings(&settings))
            .expect("executor should initialize with dummy vendor config"),
    );
    let ingress_service = Arc::new(ingress::service::IngressService::new(
        executor_service.clone(),
        settings.clone(),
    ));
    let app_state = AppState {
        settings,
        ingress_service,
        executor_service,
        health_service: Arc::new(health::HealthService::new()),
        auth_service: auth_service_from_pool(pool.clone()),
    };
    let app = build_production_router(app_state, MetricsConfig::disabled());

    let request_body =
        json!({ "prompt": "load", "metadata": {}, "allow_fallback": false }).to_string();

    for _ in 0..3 {
        let request = Request::builder()
            .method("POST")
            .uri("/api/v1/route")
            .header("content-type", "application/json")
            .header("x-api-key", &issued.credential)
            .body(Body::from(request_body.clone()))
            .unwrap();

        let response = app.clone().oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
    }

    let request = Request::builder()
        .method("POST")
        .uri("/api/v1/route")
        .header("content-type", "application/json")
        .header("x-api-key", &issued.credential)
        .body(Body::from(request_body))
        .unwrap();

    let response = app.oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);

    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let body_str = String::from_utf8(body.to_vec()).unwrap();
    assert!(body_str.contains("\"code\":\"CIRCUIT_OPEN\""));
    cleanup_clients(&pool, &[issued.client_id]).await;
}

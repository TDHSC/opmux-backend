//! OpenAI Chat Completions wire protocol and successful-result mapping.
//!
//! Exercises the shared production router and real Reqwest adapter against an
//! owned loopback simulator. Evidence omits credentials, prompts, metadata,
//! and raw provider bodies.

mod support;

use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use gateway::{
    app::Application,
    core::{
        config::{Route, Settings, Target, TargetPricing, VendorKind},
        metrics::MetricsConfig,
    },
    features::auth::AuthService,
};
use serial_test::serial;
use std::sync::Arc;
use support::{
    auth_service_from_pool, cleanup_clients, isolate_provider_environment,
    min_padded_chat_completion_len, padded_chat_completion_bytes,
    provision_inference_key, test_pool, CapturedRequest, OpenAiSimulator,
    ScriptedResponse, SIMULATED_CONTENT,
};
use tower::ServiceExt;

const DEFAULT_MODEL: &str = "example-chat-model";
const FAST_MODEL: &str = "example-chat-model-mini";
const REPORTED_SNAPSHOT_MODEL: &str = "reported-snapshot-model";
const WIRE_PROMPT: &str = "openai-results-wire-prompt";
const ILLUSTRATIVE_PROMPT_TOKENS: i64 = 120;
const ILLUSTRATIVE_COMPLETION_TOKENS: i64 = 30;
const ILLUSTRATIVE_PRIMARY_COST: f64 = 0.00018;
const ILLUSTRATIVE_SECONDARY_COST: f64 = 0.000045;
const SAME_MODEL_ALT_TARGET: &str = "same-model-alt";
const SAME_MODEL_ALT_ROUTE: &str = "same-model-alt";
const SAME_MODEL_CHAIN_ROUTE: &str = "same-model-chain";
const ILLUSTRATIVE_SAME_MODEL_ALT_COST: f64 = 0.0018;

async fn body_json(response: axum::http::Response<Body>) -> serde_json::Value {
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("response body");
    serde_json::from_slice(&bytes).expect("json body")
}

fn generation_body(capture: &CapturedRequest) -> &serde_json::Value {
    capture
        .body
        .as_ref()
        .expect("generation capture must include JSON")
}

fn results_router(
    simulator: &OpenAiSimulator,
    auth_service: Arc<AuthService>,
) -> axum::Router {
    let settings =
        Settings::for_tests_with_provider(simulator.base_url(), simulator.credential());
    Application::from_settings(Arc::new(settings), auth_service)
        .expect("application should build from openai-results fixture settings")
        .into_router(MetricsConfig::disabled())
}

fn settings_with_same_requested_model_targets(simulator: &OpenAiSimulator) -> Settings {
    let mut settings =
        Settings::for_tests_with_provider(simulator.base_url(), simulator.credential());
    let shared_model = settings
        .catalog
        .targets
        .get("primary")
        .expect("primary target")
        .model
        .clone();
    settings.catalog.targets.insert(
        SAME_MODEL_ALT_TARGET.to_string(),
        Target {
            vendor: VendorKind::Openai,
            model: shared_model,
            max_output_tokens: 512,
            pricing: TargetPricing {
                input_per_million: 10.0,
                output_per_million: 20.0,
            },
        },
    );
    settings.catalog.routes.insert(
        SAME_MODEL_ALT_ROUTE.to_string(),
        Route {
            primary: SAME_MODEL_ALT_TARGET.to_string(),
            fallbacks: Vec::new(),
        },
    );
    settings.catalog.routes.insert(
        SAME_MODEL_CHAIN_ROUTE.to_string(),
        Route {
            primary: "primary".to_string(),
            fallbacks: vec![SAME_MODEL_ALT_TARGET.to_string()],
        },
    );
    settings.limits.retries_per_target = 0;
    settings
}

fn results_router_with_same_requested_model_targets(
    simulator: &OpenAiSimulator,
    auth_service: Arc<AuthService>,
) -> axum::Router {
    let settings = settings_with_same_requested_model_targets(simulator);
    Application::from_settings(Arc::new(settings), auth_service)
        .expect("application should build from same-model fixture settings")
        .into_router(MetricsConfig::disabled())
}

async fn post_route(
    app: axum::Router,
    credential: &str,
    body: serde_json::Value,
) -> axum::http::Response<Body> {
    app.oneshot(
        Request::builder()
            .method("POST")
            .uri("/api/v1/route")
            .header("content-type", "application/json")
            .header("x-api-key", credential)
            .body(Body::from(body.to_string()))
            .unwrap(),
    )
    .await
    .unwrap()
}

fn enqueue_reported(simulator: &OpenAiSimulator, finish_reason: &str) {
    simulator.enqueue_chat(ScriptedResponse::chat_reported(
        REPORTED_SNAPSHOT_MODEL,
        ILLUSTRATIVE_PROMPT_TOKENS,
        ILLUSTRATIVE_COMPLETION_TOKENS,
        finish_reason,
    ));
}

fn assert_cost_eq(actual: f64, expected: f64) {
    assert!(
        (actual - expected).abs() < 1e-12,
        "cost {actual} differed from expected {expected}"
    );
    assert!(actual >= 0.0, "estimated cost must be nonnegative");
}

#[tokio::test]
#[serial]
async fn production_router_posts_documented_chat_completions_protocol() {
    isolate_provider_environment();
    let pool = test_pool().await;
    let issued = provision_inference_key(&pool).await;
    let simulator = OpenAiSimulator::start().await;
    enqueue_reported(&simulator, "stop");
    let app = results_router(&simulator, auth_service_from_pool(pool.clone()));

    let response = post_route(
        app,
        &issued.credential,
        serde_json::json!({
            "prompt": WIRE_PROMPT,
            "metadata": {},
            "parameters": {
                "temperature": 0.2,
                "top_p": 0.9,
                "max_tokens": 32
            }
        }),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = body_json(response).await;
    assert_eq!(body["response"]["content"], SIMULATED_CONTENT);
    assert_eq!(body["response"]["role"], "assistant");
    assert_eq!(body["response"]["finish_reason"], "stop");
    assert_eq!(body["model_used"], REPORTED_SNAPSHOT_MODEL);
    assert_ne!(body["model_used"], DEFAULT_MODEL);
    assert_cost_eq(
        body["cost"].as_f64().expect("cost number"),
        ILLUSTRATIVE_PRIMARY_COST,
    );
    let processing_time_ms = body["processing_time_ms"]
        .as_u64()
        .expect("processing_time_ms");
    assert!(processing_time_ms < 30_000);
    assert_eq!(body["usage"]["prompt_tokens"], ILLUSTRATIVE_PROMPT_TOKENS);
    assert_eq!(
        body["usage"]["completion_tokens"],
        ILLUSTRATIVE_COMPLETION_TOKENS
    );

    assert_eq!(simulator.generation_count(), 1);
    assert_eq!(simulator.models_probe_count(), 0);
    let capture = simulator
        .captured()
        .into_iter()
        .find(|capture| capture.is_generation())
        .expect("chat completion capture");
    assert_eq!(capture.method, "POST");
    assert_eq!(capture.path, "/v1/chat/completions");
    assert!(capture.authorization_matches_fixture);
    assert_eq!(capture.content_type.as_deref(), Some("application/json"));
    let wire = generation_body(&capture);
    assert_eq!(wire["model"], DEFAULT_MODEL);
    assert_eq!(wire["messages"][0]["role"], "user");
    assert_eq!(wire["messages"][0]["content"], WIRE_PROMPT);
    assert_eq!(wire["temperature"], 0.2);
    assert_eq!(wire["top_p"], 0.9);
    assert_eq!(wire["max_tokens"], 32);
    assert!(wire.get("stream").is_none());
    cleanup_clients(&pool, &[issued.client_id]).await;
}

#[tokio::test]
#[serial]
async fn named_targets_use_own_prices_when_provider_reports_the_same_model() {
    isolate_provider_environment();
    let pool = test_pool().await;
    let issued = provision_inference_key(&pool).await;
    let simulator = OpenAiSimulator::start().await;
    enqueue_reported(&simulator, "length");
    enqueue_reported(&simulator, "length");
    let app = results_router(&simulator, auth_service_from_pool(pool.clone()));

    let default_response = post_route(
        app.clone(),
        &issued.credential,
        serde_json::json!({
            "prompt": WIRE_PROMPT,
            "metadata": {}
        }),
    )
    .await;
    assert_eq!(default_response.status(), StatusCode::OK);
    let default_body = body_json(default_response).await;
    assert_eq!(default_body["response"]["content"], SIMULATED_CONTENT);
    assert_eq!(default_body["response"]["role"], "assistant");
    assert_eq!(default_body["response"]["finish_reason"], "length");
    assert_eq!(default_body["model_used"], REPORTED_SNAPSHOT_MODEL);
    assert_cost_eq(
        default_body["cost"].as_f64().expect("default cost"),
        ILLUSTRATIVE_PRIMARY_COST,
    );
    assert_eq!(
        default_body["usage"]["prompt_tokens"],
        ILLUSTRATIVE_PROMPT_TOKENS
    );
    assert_eq!(
        default_body["usage"]["completion_tokens"],
        ILLUSTRATIVE_COMPLETION_TOKENS
    );

    let named_response = post_route(
        app,
        &issued.credential,
        serde_json::json!({
            "prompt": WIRE_PROMPT,
            "metadata": {},
            "route": "fast"
        }),
    )
    .await;
    assert_eq!(named_response.status(), StatusCode::OK);
    let named_body = body_json(named_response).await;
    assert_eq!(named_body["model_used"], REPORTED_SNAPSHOT_MODEL);
    assert_ne!(named_body["model_used"], FAST_MODEL);
    assert_cost_eq(
        named_body["cost"].as_f64().expect("named cost"),
        ILLUSTRATIVE_SECONDARY_COST,
    );
    assert_ne!(
        named_body["cost"].as_f64().expect("named cost"),
        default_body["cost"].as_f64().expect("default cost")
    );

    let captures: Vec<_> = simulator
        .captured()
        .into_iter()
        .filter(|capture| capture.is_generation())
        .collect();
    assert_eq!(captures.len(), 2);
    assert_eq!(generation_body(&captures[0])["model"], DEFAULT_MODEL);
    assert_eq!(generation_body(&captures[1])["model"], FAST_MODEL);
    assert_eq!(simulator.models_probe_count(), 0);
    cleanup_clients(&pool, &[issued.client_id]).await;
}

#[tokio::test]
#[serial]
async fn same_requested_model_targets_keep_configured_prices_and_reported_model() {
    isolate_provider_environment();
    let pool = test_pool().await;
    let issued = provision_inference_key(&pool).await;
    let simulator = OpenAiSimulator::start().await;
    enqueue_reported(&simulator, "stop");
    enqueue_reported(&simulator, "stop");
    let app = results_router_with_same_requested_model_targets(
        &simulator,
        auth_service_from_pool(pool.clone()),
    );

    let default_response = post_route(
        app.clone(),
        &issued.credential,
        serde_json::json!({
            "prompt": WIRE_PROMPT,
            "metadata": {}
        }),
    )
    .await;
    assert_eq!(default_response.status(), StatusCode::OK);
    let default_body = body_json(default_response).await;
    assert_eq!(default_body["response"]["content"], SIMULATED_CONTENT);
    assert_eq!(default_body["response"]["role"], "assistant");
    assert_eq!(default_body["model_used"], REPORTED_SNAPSHOT_MODEL);
    assert_ne!(default_body["model_used"], DEFAULT_MODEL);
    assert_cost_eq(
        default_body["cost"]
            .as_f64()
            .expect("primary same-model cost"),
        ILLUSTRATIVE_PRIMARY_COST,
    );

    let alt_response = post_route(
        app,
        &issued.credential,
        serde_json::json!({
            "prompt": WIRE_PROMPT,
            "metadata": {},
            "route": SAME_MODEL_ALT_ROUTE
        }),
    )
    .await;
    assert_eq!(alt_response.status(), StatusCode::OK);
    let alt_body = body_json(alt_response).await;
    assert_eq!(alt_body["response"]["role"], "assistant");
    assert_eq!(alt_body["model_used"], REPORTED_SNAPSHOT_MODEL);
    assert_cost_eq(
        alt_body["cost"].as_f64().expect("alt same-model cost"),
        ILLUSTRATIVE_SAME_MODEL_ALT_COST,
    );
    assert_ne!(
        alt_body["cost"].as_f64().expect("alt same-model cost"),
        default_body["cost"]
            .as_f64()
            .expect("primary same-model cost")
    );

    let captures: Vec<_> = simulator
        .captured()
        .into_iter()
        .filter(|capture| capture.is_generation())
        .collect();
    assert_eq!(captures.len(), 2);
    assert_eq!(generation_body(&captures[0])["model"], DEFAULT_MODEL);
    assert_eq!(generation_body(&captures[1])["model"], DEFAULT_MODEL);
    assert_eq!(simulator.models_probe_count(), 0);
    cleanup_clients(&pool, &[issued.client_id]).await;
}

#[tokio::test]
#[serial]
async fn fallback_target_owns_successful_response_cost_for_shared_requested_model() {
    isolate_provider_environment();
    let pool = test_pool().await;
    let issued = provision_inference_key(&pool).await;
    let simulator = OpenAiSimulator::start().await;
    simulator.enqueue_chat(ScriptedResponse::json_status(
        500,
        serde_json::json!({"error":{"message":"simulated primary fault"}}),
    ));
    enqueue_reported(&simulator, "stop");
    let app = results_router_with_same_requested_model_targets(
        &simulator,
        auth_service_from_pool(pool.clone()),
    );

    let response = post_route(
        app,
        &issued.credential,
        serde_json::json!({
            "prompt": WIRE_PROMPT,
            "metadata": {},
            "route": SAME_MODEL_CHAIN_ROUTE
        }),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = body_json(response).await;
    assert_eq!(body["response"]["content"], SIMULATED_CONTENT);
    assert_eq!(body["response"]["role"], "assistant");
    assert_eq!(body["model_used"], REPORTED_SNAPSHOT_MODEL);
    assert_cost_eq(
        body["cost"].as_f64().expect("fallback same-model cost"),
        ILLUSTRATIVE_SAME_MODEL_ALT_COST,
    );

    let captures: Vec<_> = simulator
        .captured()
        .into_iter()
        .filter(|capture| capture.is_generation())
        .collect();
    assert_eq!(captures.len(), 2);
    assert_eq!(generation_body(&captures[0])["model"], DEFAULT_MODEL);
    assert_eq!(generation_body(&captures[1])["model"], DEFAULT_MODEL);
    assert_eq!(simulator.generation_count(), 2);
    assert_eq!(simulator.models_probe_count(), 0);
    cleanup_clients(&pool, &[issued.client_id]).await;
}

const RAW_UPSTREAM_SENTINEL: &str = "RAW_UPSTREAM_SENTINEL";

fn results_router_with_limit(
    simulator: &OpenAiSimulator,
    auth_service: Arc<AuthService>,
    max_upstream_response_bytes: u64,
) -> axum::Router {
    let mut settings =
        Settings::for_tests_with_provider(simulator.base_url(), simulator.credential());
    settings.limits.max_upstream_response_bytes = max_upstream_response_bytes;
    Application::from_settings(Arc::new(settings), auth_service)
        .expect("application should build from openai-results fixture settings")
        .into_router(MetricsConfig::disabled())
}

fn assert_protocol_http_error(status: StatusCode, body: &serde_json::Value, raw: &str) {
    assert_ne!(status, StatusCode::PAYLOAD_TOO_LARGE);
    assert_eq!(status, StatusCode::BAD_GATEWAY);
    assert!(body.get("response").is_none());
    assert!(body.get("model_used").is_none());
    assert!(body.get("cost").is_none());
    assert!(body.get("usage").is_none());
    let encoded = body.to_string();
    assert!(!encoded.contains(raw));
    assert!(!encoded.contains(SIMULATED_CONTENT));
    assert_eq!(body["error"]["code"], "UPSTREAM_PROTOCOL");
    assert!(!body["error"]["request_id"]
        .as_str()
        .unwrap_or("")
        .is_empty());
}

async fn route_json(
    app: axum::Router,
    credential: &str,
) -> (StatusCode, serde_json::Value) {
    let response = post_route(
        app,
        credential,
        serde_json::json!({
            "prompt": WIRE_PROMPT,
            "metadata": {}
        }),
    )
    .await;
    let status = response.status();
    (status, body_json(response).await)
}

#[tokio::test]
#[serial]
async fn production_router_rejects_malformed_success_payloads_without_retry() {
    isolate_provider_environment();
    let pool = test_pool().await;
    let issued = provision_inference_key(&pool).await;
    let simulator = OpenAiSimulator::start().await;
    let empty_choices = serde_json::json!({
        "id": "chatcmpl-local-fixture",
        "object": "chat.completion",
        "created": 0,
        "model": DEFAULT_MODEL,
        "choices": [],
        "usage": {
            "prompt_tokens": 10,
            "completion_tokens": 5,
            "total_tokens": 15
        }
    });
    simulator.enqueue_chat(ScriptedResponse::raw_json_bytes(
        format!("{{not-json {RAW_UPSTREAM_SENTINEL}").into_bytes(),
    ));
    simulator.enqueue_chat(ScriptedResponse::raw_json_bytes(
        empty_choices.to_string().into_bytes(),
    ));
    simulator.enqueue_chat(ScriptedResponse::chat_ok());
    let app = results_router(&simulator, auth_service_from_pool(pool.clone()));

    let (malformed_status, malformed_body) =
        route_json(app.clone(), &issued.credential).await;
    assert_protocol_http_error(malformed_status, &malformed_body, RAW_UPSTREAM_SENTINEL);
    assert_eq!(simulator.generation_count(), 1);

    let (empty_status, empty_body) = route_json(app.clone(), &issued.credential).await;
    assert_protocol_http_error(empty_status, &empty_body, RAW_UPSTREAM_SENTINEL);
    assert_eq!(simulator.generation_count(), 2);

    let (ok_status, ok_body) = route_json(app, &issued.credential).await;
    assert_eq!(ok_status, StatusCode::OK);
    assert_eq!(ok_body["response"]["content"], SIMULATED_CONTENT);
    assert_eq!(simulator.generation_count(), 3);
    cleanup_clients(&pool, &[issued.client_id]).await;
}

#[tokio::test]
#[serial]
async fn production_router_rejects_non_assistant_success_roles() {
    isolate_provider_environment();
    let pool = test_pool().await;
    let issued = provision_inference_key(&pool).await;
    let simulator = OpenAiSimulator::start().await;
    for role in ["user", "system", " assistant "] {
        let body = serde_json::json!({
            "id": "chatcmpl-local-fixture",
            "object": "chat.completion",
            "created": 0,
            "model": REPORTED_SNAPSHOT_MODEL,
            "choices": [{
                "index": 0,
                "message": {"role": role, "content": SIMULATED_CONTENT},
                "finish_reason": "stop"
            }],
            "usage": {
                "prompt_tokens": ILLUSTRATIVE_PROMPT_TOKENS,
                "completion_tokens": ILLUSTRATIVE_COMPLETION_TOKENS,
                "total_tokens": ILLUSTRATIVE_PROMPT_TOKENS + ILLUSTRATIVE_COMPLETION_TOKENS
            }
        });
        simulator.enqueue_chat(ScriptedResponse::raw_json_bytes(
            body.to_string().into_bytes(),
        ));
    }
    enqueue_reported(&simulator, "stop");
    let app = results_router(&simulator, auth_service_from_pool(pool.clone()));

    for _ in 0..3 {
        let (status, body) = route_json(app.clone(), &issued.credential).await;
        assert_protocol_http_error(status, &body, SIMULATED_CONTENT);
    }
    let (ok_status, ok_body) = route_json(app, &issued.credential).await;
    assert_eq!(ok_status, StatusCode::OK);
    assert_eq!(ok_body["response"]["role"], "assistant");
    assert_eq!(ok_body["model_used"], REPORTED_SNAPSHOT_MODEL);
    assert_eq!(simulator.generation_count(), 4);
    cleanup_clients(&pool, &[issued.client_id]).await;
}

#[tokio::test]
#[serial]
async fn production_router_bounds_upstream_bodies_across_transfer_modes() {
    isolate_provider_environment();
    let pool = test_pool().await;
    let issued = provision_inference_key(&pool).await;
    let bound = min_padded_chat_completion_len() + 32;
    let exact = padded_chat_completion_bytes(bound);
    let over = padded_chat_completion_bytes(bound + 1);
    let simulator = OpenAiSimulator::start().await;
    simulator.enqueue_chat(ScriptedResponse::raw_json_bytes(exact));
    simulator.enqueue_chat(ScriptedResponse::raw_json_bytes(over.clone()));
    simulator.enqueue_chat(ScriptedResponse::advertised_length(
        over.clone(),
        (bound as u64) + 1,
    ));
    simulator.enqueue_chat(ScriptedResponse::chunked(over, 256_000));
    let app = results_router_with_limit(
        &simulator,
        auth_service_from_pool(pool.clone()),
        bound as u64,
    );

    let (ok_status, ok_body) = route_json(app.clone(), &issued.credential).await;
    assert_eq!(ok_status, StatusCode::OK);
    assert_eq!(ok_body["response"]["content"], SIMULATED_CONTENT);

    for _ in 0..3 {
        let (status, body) = route_json(app.clone(), &issued.credential).await;
        assert_protocol_http_error(status, &body, "aaaaaaaa");
        assert_ne!(status, StatusCode::PAYLOAD_TOO_LARGE);
    }
    assert_eq!(simulator.generation_count(), 4);
    cleanup_clients(&pool, &[issued.client_id]).await;
}

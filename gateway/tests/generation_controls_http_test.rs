//! Typed generation controls and prompt/control validation through the production router.
//!
//! Evidence omits credentials, prompts, metadata, and raw provider bodies.

mod support;

use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use gateway::{
    app::Application,
    core::{config::Settings, metrics::MetricsConfig},
    features::auth::AuthService,
};
use serial_test::serial;
use std::sync::Arc;
use support::{
    auth_service_from_pool, cleanup_clients, isolate_provider_environment,
    provision_inference_key, test_pool, CapturedRequest, OpenAiSimulator,
};
use tower::ServiceExt;

const DEFAULT_MODEL: &str = "example-chat-model";
const FAST_MODEL: &str = "example-chat-model-mini";
const DEFAULT_MAX_TOKENS: u64 = 512;
const FAST_MAX_TOKENS: u64 = 256;
const SMALL_PROMPT_BOUND: u64 = 8;

async fn body_string(response: axum::http::Response<Body>) -> String {
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("response body");
    String::from_utf8(bytes.to_vec()).expect("utf8 body")
}

fn generation_body(capture: &CapturedRequest) -> &serde_json::Value {
    capture
        .body
        .as_ref()
        .expect("generation capture must include JSON")
}

fn generation_model(capture: &CapturedRequest) -> Option<&str> {
    generation_body(capture)
        .get("model")
        .and_then(|value| value.as_str())
}

fn user_contents(capture: &CapturedRequest) -> Vec<&str> {
    generation_body(capture)
        .get("messages")
        .and_then(|value| value.as_array())
        .map(|messages| {
            messages
                .iter()
                .filter(|message| {
                    message.get("role").and_then(|role| role.as_str()) == Some("user")
                })
                .filter_map(|message| {
                    message.get("content").and_then(|content| content.as_str())
                })
                .collect()
        })
        .unwrap_or_default()
}

fn controls_router(
    simulator: &OpenAiSimulator,
    auth_service: Arc<AuthService>,
    max_prompt_chars: Option<u64>,
) -> axum::Router {
    let mut settings =
        Settings::for_tests_with_provider(simulator.base_url(), simulator.credential());
    if let Some(bound) = max_prompt_chars {
        settings.limits.max_prompt_chars = bound;
    }
    Application::from_settings(Arc::new(settings), auth_service)
        .expect("application should build from generation-control fixture settings")
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

async fn post_route_raw(
    app: axum::Router,
    credential: &str,
    body: &str,
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

fn accepted_capture(captures: &[CapturedRequest], index: usize) -> &CapturedRequest {
    captures.get(index).expect("expected generation capture")
}

#[tokio::test]
#[serial]
async fn accepted_generation_options_match_outgoing_json_and_target_bounds() {
    isolate_provider_environment();
    let pool = test_pool().await;
    let issued = provision_inference_key(&pool).await;
    let simulator = OpenAiSimulator::start().await;
    let app = controls_router(&simulator, auth_service_from_pool(pool.clone()), None);

    let omitted = post_route(
        app.clone(),
        &issued.credential,
        serde_json::json!({
            "prompt": "documented defaults",
            "metadata": {}
        }),
    )
    .await;
    assert_eq!(omitted.status(), StatusCode::OK);
    let omitted_body = body_string(omitted).await;
    assert!(omitted_body.contains(&format!("\"model_used\":\"{DEFAULT_MODEL}\"")));

    let endpoints = post_route(
        app.clone(),
        &issued.credential,
        serde_json::json!({
            "prompt": "numeric endpoints",
            "metadata": {},
            "parameters": {
                "temperature": 0.0,
                "top_p": 1.0,
                "max_tokens": 32
            }
        }),
    )
    .await;
    assert_eq!(endpoints.status(), StatusCode::OK);

    let upper = post_route(
        app.clone(),
        &issued.credential,
        serde_json::json!({
            "prompt": "upper numeric endpoints",
            "metadata": {},
            "parameters": {
                "temperature": 2.0,
                "top_p": 0.0,
                "max_tokens": DEFAULT_MAX_TOKENS
            }
        }),
    )
    .await;
    assert_eq!(upper.status(), StatusCode::OK);

    let named = post_route(
        app,
        &issued.credential,
        serde_json::json!({
            "prompt": "named route default cap",
            "metadata": {},
            "route": "fast"
        }),
    )
    .await;
    assert_eq!(named.status(), StatusCode::OK);

    let captures: Vec<_> = simulator
        .captured()
        .into_iter()
        .filter(|capture| capture.is_generation())
        .collect();
    assert_eq!(captures.len(), 4);

    let omitted_json = generation_body(accepted_capture(&captures, 0));
    assert_eq!(generation_model(&captures[0]), Some(DEFAULT_MODEL));
    assert!(omitted_json.get("temperature").is_none());
    assert!(omitted_json.get("top_p").is_none());
    assert_eq!(
        omitted_json.get("max_tokens"),
        Some(&serde_json::json!(DEFAULT_MAX_TOKENS))
    );
    assert!(omitted_json.get("stream").is_none());
    assert_eq!(
        omitted_json["max_tokens"].as_u64(),
        Some(DEFAULT_MAX_TOKENS)
    );

    let endpoint_json = generation_body(accepted_capture(&captures, 1));
    assert_eq!(
        endpoint_json.get("temperature"),
        Some(&serde_json::json!(0.0))
    );
    assert_eq!(endpoint_json.get("top_p"), Some(&serde_json::json!(1.0)));
    assert_eq!(
        endpoint_json.get("max_tokens"),
        Some(&serde_json::json!(32))
    );
    assert_eq!(endpoint_json["max_tokens"].as_u64(), Some(32));
    assert!(endpoint_json["max_tokens"].as_f64().is_some());
    assert_eq!(endpoint_json["temperature"].as_f64(), Some(0.0));
    assert_eq!(endpoint_json["top_p"].as_f64(), Some(1.0));

    let upper_json = generation_body(accepted_capture(&captures, 2));
    assert_eq!(upper_json.get("temperature"), Some(&serde_json::json!(2.0)));
    assert_eq!(upper_json.get("top_p"), Some(&serde_json::json!(0.0)));
    assert_eq!(
        upper_json.get("max_tokens"),
        Some(&serde_json::json!(DEFAULT_MAX_TOKENS))
    );

    let named_json = generation_body(accepted_capture(&captures, 3));
    assert_eq!(generation_model(&captures[3]), Some(FAST_MODEL));
    assert!(named_json.get("temperature").is_none());
    assert!(named_json.get("top_p").is_none());
    assert_eq!(
        named_json.get("max_tokens"),
        Some(&serde_json::json!(FAST_MAX_TOKENS))
    );

    cleanup_clients(&pool, &[issued.client_id]).await;
}

#[tokio::test]
#[serial]
async fn invalid_generation_controls_fail_before_upstream() {
    isolate_provider_environment();
    let pool = test_pool().await;
    let issued = provision_inference_key(&pool).await;
    let simulator = OpenAiSimulator::start().await;
    let app = controls_router(&simulator, auth_service_from_pool(pool.clone()), None);

    let cases: Vec<(&str, serde_json::Value)> = vec![
        (
            "temperature below range",
            serde_json::json!({
                "prompt": "invalid controls",
                "metadata": {},
                "parameters": { "temperature": -0.1 }
            }),
        ),
        (
            "temperature above range",
            serde_json::json!({
                "prompt": "invalid controls",
                "metadata": {},
                "parameters": { "temperature": 2.1 }
            }),
        ),
        (
            "top_p below range",
            serde_json::json!({
                "prompt": "invalid controls",
                "metadata": {},
                "parameters": { "top_p": -0.1 }
            }),
        ),
        (
            "top_p above range",
            serde_json::json!({
                "prompt": "invalid controls",
                "metadata": {},
                "parameters": { "top_p": 1.1 }
            }),
        ),
        (
            "string temperature",
            serde_json::json!({
                "prompt": "invalid controls",
                "metadata": {},
                "parameters": { "temperature": "0.5" }
            }),
        ),
        (
            "boolean top_p",
            serde_json::json!({
                "prompt": "invalid controls",
                "metadata": {},
                "parameters": { "top_p": true }
            }),
        ),
        (
            "zero max_tokens",
            serde_json::json!({
                "prompt": "invalid controls",
                "metadata": {},
                "parameters": { "max_tokens": 0 }
            }),
        ),
        (
            "primary cap plus one",
            serde_json::json!({
                "prompt": "invalid controls",
                "metadata": {},
                "parameters": { "max_tokens": DEFAULT_MAX_TOKENS + 1 }
            }),
        ),
        (
            "named route cap plus one",
            serde_json::json!({
                "prompt": "invalid controls",
                "metadata": {},
                "route": "fast",
                "parameters": { "max_tokens": FAST_MAX_TOKENS + 1 }
            }),
        ),
        (
            "unknown parameter name",
            serde_json::json!({
                "prompt": "invalid controls",
                "metadata": {},
                "parameters": { "n": 1 }
            }),
        ),
        (
            "unknown top-level model",
            serde_json::json!({
                "prompt": "invalid controls",
                "metadata": {},
                "model": "unconfigured-injected-model"
            }),
        ),
        (
            "unknown top-level url",
            serde_json::json!({
                "prompt": "invalid controls",
                "metadata": {},
                "url": "https://injected.example/v1"
            }),
        ),
        (
            "explicit stream true",
            serde_json::json!({
                "prompt": "invalid controls",
                "metadata": {},
                "stream": true
            }),
        ),
        (
            "explicit rewrite true",
            serde_json::json!({
                "prompt": "invalid controls",
                "metadata": {},
                "rewrite": true
            }),
        ),
        ("missing prompt", serde_json::json!({ "metadata": {} })),
        (
            "missing metadata",
            serde_json::json!({ "prompt": "invalid controls" }),
        ),
        (
            "non-string prompt",
            serde_json::json!({ "prompt": 12, "metadata": {} }),
        ),
        (
            "whitespace-only prompt",
            serde_json::json!({ "prompt": " \n\t ", "metadata": {} }),
        ),
        (
            "wrong type route",
            serde_json::json!({
                "prompt": "invalid controls",
                "metadata": {},
                "route": 1
            }),
        ),
        (
            "wrong type allow_fallback",
            serde_json::json!({
                "prompt": "invalid controls",
                "metadata": {},
                "allow_fallback": "false"
            }),
        ),
    ];

    for (name, body) in cases {
        let before = simulator.generation_count();
        let response = post_route(app.clone(), &issued.credential, body).await;
        assert_eq!(
            response.status(),
            StatusCode::BAD_REQUEST,
            "{name} must return 400"
        );
        assert_eq!(
            simulator.generation_count(),
            before,
            "{name} must not start a generation call"
        );
    }

    let fractional = post_route_raw(
        app,
        &issued.credential,
        r#"{"prompt":"invalid controls","metadata":{},"parameters":{"max_tokens":1.5}}"#,
    )
    .await;
    assert_eq!(fractional.status(), StatusCode::BAD_REQUEST);
    assert_eq!(simulator.generation_count(), 0);

    cleanup_clients(&pool, &[issued.client_id]).await;
}

#[tokio::test]
#[serial]
async fn prompt_bounds_use_original_characters_and_bytes() {
    isolate_provider_environment();
    let pool = test_pool().await;
    let issued = provision_inference_key(&pool).await;
    let simulator = OpenAiSimulator::start().await;
    let app = controls_router(
        &simulator,
        auth_service_from_pool(pool.clone()),
        Some(SMALL_PROMPT_BOUND),
    );

    let exact_chars = "abcdefgh";
    let exact_char_response = post_route(
        app.clone(),
        &issued.credential,
        serde_json::json!({
            "prompt": exact_chars,
            "metadata": {}
        }),
    )
    .await;
    assert_eq!(exact_char_response.status(), StatusCode::OK);
    assert_eq!(exact_chars.chars().count() as u64, SMALL_PROMPT_BOUND);
    assert_eq!(exact_chars.len() as u64, SMALL_PROMPT_BOUND);

    let over_chars = "abcdefghi";
    let over_char_response = post_route(
        app.clone(),
        &issued.credential,
        serde_json::json!({
            "prompt": over_chars,
            "metadata": {}
        }),
    )
    .await;
    assert_eq!(over_char_response.status(), StatusCode::BAD_REQUEST);
    assert!(over_chars.chars().count() as u64 > SMALL_PROMPT_BOUND);

    let padded = format!("{exact_chars} ");
    let padded_response = post_route(
        app.clone(),
        &issued.credential,
        serde_json::json!({
            "prompt": padded,
            "metadata": {}
        }),
    )
    .await;
    assert_eq!(padded_response.status(), StatusCode::BAD_REQUEST);
    assert_eq!(padded.trim().chars().count() as u64, SMALL_PROMPT_BOUND);
    assert!(padded.chars().count() as u64 > SMALL_PROMPT_BOUND);

    let exact_bytes = "éééé";
    assert_eq!(exact_bytes.len() as u64, SMALL_PROMPT_BOUND);
    assert!(exact_bytes.chars().count() as u64 <= SMALL_PROMPT_BOUND);
    let exact_byte_response = post_route(
        app.clone(),
        &issued.credential,
        serde_json::json!({
            "prompt": exact_bytes,
            "metadata": {}
        }),
    )
    .await;
    assert_eq!(exact_byte_response.status(), StatusCode::OK);

    let over_bytes = "ééééé";
    assert!(over_bytes.len() as u64 > SMALL_PROMPT_BOUND);
    let over_byte_response = post_route(
        app.clone(),
        &issued.credential,
        serde_json::json!({
            "prompt": over_bytes,
            "metadata": {}
        }),
    )
    .await;
    assert_eq!(over_byte_response.status(), StatusCode::BAD_REQUEST);

    let preserved = " ab cd ";
    assert!(preserved.chars().count() as u64 <= SMALL_PROMPT_BOUND);
    assert!(!preserved.trim().is_empty());
    let preserved_response = post_route(
        app,
        &issued.credential,
        serde_json::json!({
            "prompt": preserved,
            "metadata": {}
        }),
    )
    .await;
    assert_eq!(preserved_response.status(), StatusCode::OK);

    let captures: Vec<_> = simulator
        .captured()
        .into_iter()
        .filter(|capture| capture.is_generation())
        .collect();
    assert_eq!(captures.len(), 3);

    let first_unchanged = user_contents(&captures[0]) == [exact_chars];
    let second_unchanged = user_contents(&captures[1]) == [exact_bytes];
    let third_unchanged = user_contents(&captures[2]) == [preserved];
    assert!(first_unchanged);
    assert!(second_unchanged);
    assert!(third_unchanged);

    cleanup_clients(&pool, &[issued.client_id]).await;
}

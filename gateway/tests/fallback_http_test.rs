//! Eligible flat fallback through the production router and real adapter.
//!
//! Evidence omits credentials, prompts, metadata, and raw provider bodies.

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
use std::time::{Duration, Instant};
use support::{
    auth_service_from_pool, cleanup_clients, isolate_provider_environment,
    min_padded_chat_completion_len, padded_chat_completion_bytes,
    provision_inference_key, test_pool, CapturedRequest, OpenAiSimulator,
    ScriptedResponse,
};
use tokio::time::sleep;
use tower::ServiceExt;

const MODEL_A: &str = "fallback-model-a";
const MODEL_B: &str = "fallback-model-b";
const MODEL_C: &str = "fallback-model-c";
const REPORTED_A_OK: &str = "reported-fallback-a";
const REPORTED_B: &str = "reported-fallback-b";
const REPORTED_C: &str = "reported-fallback-c";
const CONTENT_A_OK: &str = "FALLBACK_A_RECOVERED";
const CONTENT_B: &str = "FALLBACK_B_OK";
const CONTENT_C: &str = "FALLBACK_C_OK";
const PROMPT_TOKENS: i64 = 120;
const COMPLETION_TOKENS: i64 = 30;
const COST_B: f64 = 0.00072;
const WIRE_PROMPT: &str = "fallback-wire-prompt";
const RAW_SENTINEL: &str = "RAW_UPSTREAM_SENTINEL";

fn target(model: &str, max_output_tokens: u32, input: f64, output: f64) -> Target {
    Target {
        vendor: VendorKind::Openai,
        model: model.to_string(),
        max_output_tokens,
        pricing: TargetPricing {
            input_per_million: input,
            output_per_million: output,
        },
    }
}

fn chain_settings(
    simulator: &OpenAiSimulator,
    mutate: impl FnOnce(&mut Settings),
) -> Settings {
    let mut settings =
        Settings::for_tests_with_provider(simulator.base_url(), simulator.credential());
    settings
        .catalog
        .targets
        .insert("alpha".to_string(), target(MODEL_A, 512, 1.0, 2.0));
    settings
        .catalog
        .targets
        .insert("beta".to_string(), target(MODEL_B, 512, 4.0, 8.0));
    settings
        .catalog
        .targets
        .insert("gamma".to_string(), target(MODEL_C, 512, 10.0, 20.0));
    settings.catalog.routes.insert(
        "chain".to_string(),
        Route {
            primary: "alpha".to_string(),
            fallbacks: vec!["beta".to_string(), "gamma".to_string()],
        },
    );
    settings.catalog.routes.insert(
        "mixed".to_string(),
        Route {
            primary: "alpha".to_string(),
            fallbacks: vec!["beta-small".to_string(), "gamma".to_string()],
        },
    );
    settings
        .catalog
        .targets
        .insert("beta-small".to_string(), target(MODEL_B, 64, 4.0, 8.0));
    settings.limits.retries_per_target = 1;
    settings.limits.max_total_attempts = 8;
    settings.limits.backoff_cap = Duration::from_millis(1);
    settings.limits.max_attempt_timeout = Duration::from_millis(2_000);
    mutate(&mut settings);
    settings
}

fn router(settings: Settings, auth_service: Arc<AuthService>) -> axum::Router {
    Application::from_settings(Arc::new(settings), auth_service)
        .expect("application should build from fallback fixture settings")
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

fn generation_models(simulator: &OpenAiSimulator) -> Vec<String> {
    simulator
        .captured()
        .into_iter()
        .filter(|capture| capture.is_generation())
        .filter_map(|capture| {
            generation_body(&capture)
                .get("model")
                .and_then(|value| value.as_str())
                .map(ToOwned::to_owned)
        })
        .collect()
}

fn assert_models(actual: &[String], expected: &[&str]) {
    let expected: Vec<String> =
        expected.iter().map(|model| (*model).to_string()).collect();
    assert_eq!(actual, expected.as_slice());
}

fn assert_unchanged_params(capture: &CapturedRequest, max_tokens: i64) {
    let wire = generation_body(capture);
    assert_eq!(wire["temperature"], 0.2);
    assert_eq!(wire["top_p"], 0.9);
    assert_eq!(wire["max_tokens"], max_tokens);
    assert_eq!(wire["messages"][0]["role"], "user");
    assert_eq!(wire["messages"][0]["content"], WIRE_PROMPT);
    assert!(wire.get("stream").is_none());
}

fn chat_reported(model: &str, content: &str) -> ScriptedResponse {
    ScriptedResponse::ChatSuccess {
        content: content.to_string(),
        model: Some(model.to_string()),
        prompt_tokens: PROMPT_TOKENS,
        completion_tokens: COMPLETION_TOKENS,
        finish_reason: "stop".to_string(),
        role: "assistant".to_string(),
    }
}

fn upstream_500() -> ScriptedResponse {
    ScriptedResponse::json_status(
        500,
        serde_json::json!({"error":{"message":"simulated transient"}}),
    )
}

fn route_body(route: &str, allow_fallback: Option<bool>) -> serde_json::Value {
    let mut body = serde_json::json!({
        "prompt": WIRE_PROMPT,
        "metadata": {},
        "route": route,
        "parameters": {
            "temperature": 0.2,
            "top_p": 0.9,
            "max_tokens": 128
        }
    });
    if let Some(allow_fallback) = allow_fallback {
        body["allow_fallback"] = serde_json::Value::Bool(allow_fallback);
    }
    body
}

fn assert_cost_eq(actual: f64, expected: f64) {
    assert!(
        (actual - expected).abs() < 1e-12,
        "cost {actual} differed from expected {expected}"
    );
}

fn assert_envelope(status: StatusCode, body: &serde_json::Value, code: &str) {
    assert_eq!(body["error"]["code"], code);
    assert!(!body["error"]["request_id"]
        .as_str()
        .unwrap_or("")
        .is_empty());
    assert!(body.get("response").is_none());
    assert_ne!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
#[serial]
async fn equal_cap_chain_obeys_order_budget_and_opt_out() {
    isolate_provider_environment();
    let pool = test_pool().await;
    let issued = provision_inference_key(&pool).await;
    let simulator = OpenAiSimulator::start().await;

    simulator.enqueue_chat(upstream_500());
    simulator.enqueue_chat(upstream_500());
    simulator.enqueue_chat(chat_reported(REPORTED_B, CONTENT_B));
    let app = router(
        chain_settings(&simulator, |_| {}),
        auth_service_from_pool(pool.clone()),
    );
    let response = post_route(app, &issued.credential, route_body("chain", None)).await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = body_json(response).await;
    assert_eq!(body["response"]["content"], CONTENT_B);
    assert_eq!(body["response"]["role"], "assistant");
    assert_eq!(body["model_used"], REPORTED_B);
    assert_ne!(body["model_used"], MODEL_A);
    assert_cost_eq(body["cost"].as_f64().expect("fallback cost"), COST_B);
    assert_models(&generation_models(&simulator), &[MODEL_A, MODEL_A, MODEL_B]);
    for capture in simulator
        .captured()
        .iter()
        .filter(|capture| capture.is_generation())
    {
        assert_unchanged_params(capture, 128);
    }

    simulator.enqueue_chat(upstream_500());
    simulator.enqueue_chat(upstream_500());
    simulator.enqueue_chat(upstream_500());
    simulator.enqueue_chat(upstream_500());
    simulator.enqueue_chat(chat_reported(REPORTED_C, CONTENT_C));
    let app = router(
        chain_settings(&simulator, |_| {}),
        auth_service_from_pool(pool.clone()),
    );
    let before = simulator.generation_count();
    let response =
        post_route(app, &issued.credential, route_body("chain", Some(true))).await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = body_json(response).await;
    assert_eq!(body["response"]["content"], CONTENT_C);
    assert_eq!(body["model_used"], REPORTED_C);
    assert_models(
        &generation_models(&simulator)[before..],
        &[MODEL_A, MODEL_A, MODEL_B, MODEL_B, MODEL_C],
    );

    simulator.enqueue_chat(upstream_500());
    simulator.enqueue_chat(upstream_500());
    simulator.enqueue_chat(upstream_500());
    let app = router(
        chain_settings(&simulator, |settings| {
            settings.limits.max_total_attempts = 3;
        }),
        auth_service_from_pool(pool.clone()),
    );
    let before = simulator.generation_count();
    let response = post_route(app, &issued.credential, route_body("chain", None)).await;
    let status = response.status();
    let body = body_json(response).await;
    assert_eq!(status, StatusCode::BAD_GATEWAY);
    assert_envelope(status, &body, "UPSTREAM_ERROR");
    assert_models(
        &generation_models(&simulator)[before..],
        &[MODEL_A, MODEL_A, MODEL_B],
    );

    simulator.enqueue_chat(upstream_500());
    simulator.enqueue_chat(upstream_500());
    let app = router(
        chain_settings(&simulator, |_| {}),
        auth_service_from_pool(pool.clone()),
    );
    let before = simulator.generation_count();
    let response =
        post_route(app, &issued.credential, route_body("chain", Some(false))).await;
    assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
    assert_models(
        &generation_models(&simulator)[before..],
        &[MODEL_A, MODEL_A],
    );

    simulator.enqueue_chat(upstream_500());
    simulator.enqueue_chat(upstream_500());
    let app = router(
        chain_settings(&simulator, |_| {}),
        auth_service_from_pool(pool.clone()),
    );
    let before = simulator.generation_count();
    let response =
        post_route(app, &issued.credential, route_body("fast", Some(true))).await;
    assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
    assert_models(
        &generation_models(&simulator)[before..],
        &["example-chat-model-mini", "example-chat-model-mini"],
    );
    assert_eq!(simulator.models_probe_count(), 0);
    cleanup_clients(&pool, &[issued.client_id]).await;
}

#[tokio::test]
#[serial]
async fn mixed_cap_skips_incompatible_target_and_keeps_params() {
    isolate_provider_environment();
    let pool = test_pool().await;
    let issued = provision_inference_key(&pool).await;
    let simulator = OpenAiSimulator::start().await;
    simulator.enqueue_chat(upstream_500());
    simulator.enqueue_chat(upstream_500());
    simulator.enqueue_chat(chat_reported(REPORTED_C, CONTENT_C));
    let app = router(
        chain_settings(&simulator, |_| {}),
        auth_service_from_pool(pool.clone()),
    );
    let response = post_route(app, &issued.credential, route_body("mixed", None)).await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = body_json(response).await;
    assert_eq!(body["response"]["content"], CONTENT_C);
    assert_eq!(body["model_used"], REPORTED_C);
    assert_models(&generation_models(&simulator), &[MODEL_A, MODEL_A, MODEL_C]);
    for capture in simulator
        .captured()
        .iter()
        .filter(|capture| capture.is_generation())
    {
        assert_unchanged_params(capture, 128);
        assert_ne!(
            generation_body(capture)
                .get("max_tokens")
                .and_then(|value| value.as_i64()),
            Some(64),
            "incompatible cap must not clamp max_tokens"
        );
    }

    simulator.enqueue_chat(upstream_500());
    simulator.enqueue_chat(upstream_500());
    let app = router(
        chain_settings(&simulator, |settings| {
            settings.catalog.routes.insert(
                "mixed-only".to_string(),
                Route {
                    primary: "alpha".to_string(),
                    fallbacks: vec!["beta-small".to_string()],
                },
            );
        }),
        auth_service_from_pool(pool.clone()),
    );
    let before = simulator.generation_count();
    let response = post_route(
        app,
        &issued.credential,
        serde_json::json!({
            "prompt": WIRE_PROMPT,
            "metadata": {},
            "route": "mixed-only",
            "parameters": { "max_tokens": 128 }
        }),
    )
    .await;
    let status = response.status();
    let body = body_json(response).await;
    assert_eq!(status, StatusCode::BAD_GATEWAY);
    assert_envelope(status, &body, "UPSTREAM_ERROR");
    assert_models(
        &generation_models(&simulator)[before..],
        &[MODEL_A, MODEL_A],
    );
    cleanup_clients(&pool, &[issued.client_id]).await;
}

#[tokio::test]
#[serial]
async fn eligibility_matrix_prevents_futile_same_provider_switching() {
    isolate_provider_environment();
    let pool = test_pool().await;
    let issued = provision_inference_key(&pool).await;
    let simulator = OpenAiSimulator::start().await;
    let app = router(
        chain_settings(&simulator, |_| {}),
        auth_service_from_pool(pool.clone()),
    );

    let missing = post_route(
        app.clone(),
        &issued.credential,
        serde_json::json!({ "metadata": {}, "route": "chain" }),
    )
    .await;
    assert_eq!(missing.status(), StatusCode::BAD_REQUEST);
    assert_eq!(simulator.generation_count(), 0);

    let cases: [(&str, ScriptedResponse, StatusCode, &str); 6] = [
        (
            "malformed",
            ScriptedResponse::raw_json_bytes(
                format!("{{not-json {RAW_SENTINEL}").into_bytes(),
            ),
            StatusCode::BAD_GATEWAY,
            "UPSTREAM_PROTOCOL",
        ),
        (
            "permanent",
            ScriptedResponse::json_status(
                400,
                serde_json::json!({"error":{"message":"permanent rejection"}}),
            ),
            StatusCode::BAD_GATEWAY,
            "UPSTREAM_ERROR",
        ),
        (
            "credential-401",
            ScriptedResponse::json_status(
                401,
                serde_json::json!({"error":{"message":"provider key"}}),
            ),
            StatusCode::BAD_GATEWAY,
            "UPSTREAM_AUTHENTICATION",
        ),
        (
            "credential-403",
            ScriptedResponse::json_status(
                403,
                serde_json::json!({"error":{"message":"provider forbidden"}}),
            ),
            StatusCode::BAD_GATEWAY,
            "UPSTREAM_AUTHENTICATION",
        ),
        (
            "quota",
            ScriptedResponse::json_status(
                403,
                serde_json::json!({
                    "error": {
                        "code": "insufficient_quota",
                        "type": "insufficient_quota",
                        "message": "quota"
                    }
                }),
            ),
            StatusCode::BAD_GATEWAY,
            "UPSTREAM_ERROR",
        ),
        (
            "throttle",
            ScriptedResponse::Json {
                status: 429,
                body: serde_json::json!({"error":{"message":"rate"}}),
                retry_after: Some("0".to_string()),
            },
            StatusCode::TOO_MANY_REQUESTS,
            "UPSTREAM_RATE_LIMIT",
        ),
    ];

    for (label, script, status, code) in cases {
        let before = simulator.generation_count();
        simulator.enqueue_chat(script.clone());
        if label == "throttle" {
            simulator.enqueue_chat(script);
        }
        let response =
            post_route(app.clone(), &issued.credential, route_body("chain", None)).await;
        let actual_status = response.status();
        let body = body_json(response).await;
        assert_eq!(actual_status, status, "{label}");
        assert_envelope(actual_status, &body, code);
        if label == "quota" {
            assert_eq!(
                body["error"]["message"],
                "Upstream provider quota was exhausted"
            );
        }
        let models = generation_models(&simulator);
        let models = &models[before..];
        if label == "throttle" {
            assert_models(models, &[MODEL_A, MODEL_A]);
        } else {
            assert_models(models, &[MODEL_A]);
        }
        assert!(
            !models
                .iter()
                .any(|model| model == MODEL_B || model == MODEL_C),
            "{label} must not switch models"
        );
        let encoded = body.to_string();
        assert!(!encoded.contains(RAW_SENTINEL));
        assert!(!encoded.contains("insufficient_quota"));
    }

    let bound = min_padded_chat_completion_len() + 8;
    let over = padded_chat_completion_bytes(bound + 1);
    simulator.enqueue_chat(ScriptedResponse::raw_json_bytes(over));
    let limited = router(
        chain_settings(&simulator, |settings| {
            settings.limits.max_upstream_response_bytes = bound as u64;
        }),
        auth_service_from_pool(pool.clone()),
    );
    let before = simulator.generation_count();
    let response =
        post_route(limited, &issued.credential, route_body("chain", None)).await;
    let status = response.status();
    let body = body_json(response).await;
    assert_eq!(status, StatusCode::BAD_GATEWAY);
    assert_envelope(status, &body, "UPSTREAM_PROTOCOL");
    assert_models(&generation_models(&simulator)[before..], &[MODEL_A]);

    simulator.enqueue_chat(upstream_500());
    simulator.enqueue_chat(upstream_500());
    simulator.enqueue_chat(upstream_500());
    simulator.enqueue_chat(upstream_500());
    let app = router(
        chain_settings(&simulator, |settings| {
            settings.catalog.routes.insert(
                "ab".to_string(),
                Route {
                    primary: "alpha".to_string(),
                    fallbacks: vec!["beta".to_string()],
                },
            );
        }),
        auth_service_from_pool(pool.clone()),
    );
    let before = simulator.generation_count();
    let response =
        post_route(app.clone(), &issued.credential, route_body("ab", None)).await;
    let status = response.status();
    let fallback_body = body_json(response).await;
    assert_eq!(status, StatusCode::BAD_GATEWAY);
    assert_envelope(status, &fallback_body, "UPSTREAM_ERROR");
    assert_models(
        &generation_models(&simulator)[before..],
        &[MODEL_A, MODEL_A, MODEL_B, MODEL_B],
    );

    simulator.enqueue_chat(upstream_500());
    simulator.enqueue_chat(upstream_500());
    let primary_only = router(
        chain_settings(&simulator, |settings| {
            settings.catalog.routes.insert(
                "primary-only".to_string(),
                Route {
                    primary: "alpha".to_string(),
                    fallbacks: Vec::new(),
                },
            );
        }),
        auth_service_from_pool(pool.clone()),
    );
    let before = simulator.generation_count();
    let response = post_route(
        primary_only,
        &issued.credential,
        serde_json::json!({
            "prompt": WIRE_PROMPT,
            "metadata": {},
            "route": "primary-only"
        }),
    )
    .await;
    let status = response.status();
    let primary_body = body_json(response).await;
    assert_eq!(status, StatusCode::BAD_GATEWAY);
    assert_eq!(
        primary_body["error"]["code"],
        fallback_body["error"]["code"]
    );
    assert_eq!(
        primary_body["error"]["message"],
        fallback_body["error"]["message"]
    );
    assert_models(
        &generation_models(&simulator)[before..],
        &[MODEL_A, MODEL_A],
    );

    simulator
        .enqueue_chat(ScriptedResponse::chat_ok().delay_headers(Duration::from_secs(2)));
    let short = router(
        chain_settings(&simulator, |settings| {
            settings.limits.protected_request_deadline = Duration::from_millis(80);
        }),
        auth_service_from_pool(pool.clone()),
    );
    let before = simulator.generation_count();
    let response = post_route(short, &issued.credential, route_body("chain", None)).await;
    let status = response.status();
    let body = body_json(response).await;
    assert_eq!(status, StatusCode::GATEWAY_TIMEOUT);
    assert_envelope(status, &body, "DEADLINE_EXCEEDED");
    let models = &generation_models(&simulator)[before..];
    assert!(
        models.iter().all(|model| model == MODEL_A),
        "deadline must not call fallback models, got {models:?}"
    );
    assert_eq!(simulator.models_probe_count(), 0);
    cleanup_clients(&pool, &[issued.client_id]).await;
}

fn circuit_settings(
    simulator: &OpenAiSimulator,
    cooldown: Duration,
    mutate: impl FnOnce(&mut Settings),
) -> Settings {
    chain_settings(simulator, |settings| {
        settings.limits.retries_per_target = 0;
        settings.limits.max_total_attempts = 8;
        settings.limits.circuit_failure_threshold = 1;
        settings.limits.circuit_cooldown = cooldown;
        settings.catalog.routes.insert(
            "beta-only".to_string(),
            Route {
                primary: "beta".to_string(),
                fallbacks: Vec::new(),
            },
        );
        mutate(settings);
    })
}

async fn wait_generation_at_least(simulator: &OpenAiSimulator, expected: usize) {
    let started = Instant::now();
    while simulator.generation_count() < expected {
        assert!(
            started.elapsed() < Duration::from_secs(1),
            "timed out waiting for {expected} generation calls, saw {}",
            simulator.generation_count()
        );
        sleep(Duration::from_millis(5)).await;
    }
}

#[tokio::test]
#[serial]
async fn open_primary_circuit_skips_a_and_keeps_same_provider_b() {
    isolate_provider_environment();
    let pool = test_pool().await;
    let issued = provision_inference_key(&pool).await;
    let simulator = OpenAiSimulator::start().await;
    let app = router(
        circuit_settings(&simulator, Duration::from_secs(30), |_| {}),
        auth_service_from_pool(pool.clone()),
    );

    simulator.enqueue_chat(upstream_500());
    simulator.enqueue_chat(chat_reported(REPORTED_B, CONTENT_B));
    let first =
        post_route(app.clone(), &issued.credential, route_body("chain", None)).await;
    assert_eq!(first.status(), StatusCode::OK);
    let first_body = body_json(first).await;
    assert_eq!(first_body["response"]["content"], CONTENT_B);
    assert_eq!(first_body["model_used"], REPORTED_B);
    assert_models(&generation_models(&simulator), &[MODEL_A, MODEL_B]);

    simulator.enqueue_chat(chat_reported(REPORTED_B, CONTENT_B));
    let second =
        post_route(app.clone(), &issued.credential, route_body("chain", None)).await;
    assert_eq!(second.status(), StatusCode::OK);
    assert_models(&generation_models(&simulator), &[MODEL_A, MODEL_B, MODEL_B]);

    simulator.enqueue_chat(chat_reported(REPORTED_B, "DIRECT_B_OK"));
    let direct = post_route(
        app,
        &issued.credential,
        route_body("beta-only", Some(false)),
    )
    .await;
    assert_eq!(direct.status(), StatusCode::OK);
    let direct_body = body_json(direct).await;
    assert_eq!(direct_body["response"]["content"], "DIRECT_B_OK");
    assert_models(
        &generation_models(&simulator),
        &[MODEL_A, MODEL_B, MODEL_B, MODEL_B],
    );
    assert_eq!(simulator.models_probe_count(), 0);
    cleanup_clients(&pool, &[issued.client_id]).await;
}

#[tokio::test]
#[serial]
async fn all_eligible_open_targets_return_circuit_open_without_generation() {
    isolate_provider_environment();
    let pool = test_pool().await;
    let issued = provision_inference_key(&pool).await;
    let simulator = OpenAiSimulator::start().await;
    let app = router(
        circuit_settings(&simulator, Duration::from_secs(30), |settings| {
            settings.catalog.routes.insert(
                "ab".to_string(),
                Route {
                    primary: "alpha".to_string(),
                    fallbacks: vec!["beta".to_string()],
                },
            );
        }),
        auth_service_from_pool(pool.clone()),
    );

    simulator.enqueue_chat(upstream_500());
    simulator.enqueue_chat(upstream_500());
    let first = post_route(app.clone(), &issued.credential, route_body("ab", None)).await;
    let first_status = first.status();
    let first_body = body_json(first).await;
    assert_eq!(first_status, StatusCode::BAD_GATEWAY);
    assert_envelope(first_status, &first_body, "UPSTREAM_ERROR");
    assert_models(&generation_models(&simulator), &[MODEL_A, MODEL_B]);

    let before = simulator.generation_count();
    let second = post_route(app, &issued.credential, route_body("ab", None)).await;
    let status = second.status();
    let body = body_json(second).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert_envelope(status, &body, "CIRCUIT_OPEN");
    assert_eq!(simulator.generation_count(), before);
    assert_eq!(simulator.models_probe_count(), 0);
    cleanup_clients(&pool, &[issued.client_id]).await;
}

#[tokio::test]
#[serial]
async fn half_open_probe_is_single_flight_and_success_closes() {
    isolate_provider_environment();
    let pool = test_pool().await;
    let issued = provision_inference_key(&pool).await;
    let simulator = OpenAiSimulator::start().await;
    let cooldown = Duration::from_millis(80);
    let app = router(
        circuit_settings(&simulator, cooldown, |_| {}),
        auth_service_from_pool(pool.clone()),
    );

    simulator.enqueue_chat(upstream_500());
    simulator.enqueue_chat(chat_reported(REPORTED_B, CONTENT_B));
    let opened =
        post_route(app.clone(), &issued.credential, route_body("chain", None)).await;
    assert_eq!(opened.status(), StatusCode::OK);
    sleep(cooldown + Duration::from_millis(40)).await;

    let (held_ok, hold) = chat_reported(REPORTED_A_OK, CONTENT_A_OK).hold();
    simulator.enqueue_chat(held_ok);
    simulator.enqueue_chat(chat_reported(REPORTED_B, CONTENT_B));
    simulator.enqueue_chat(chat_reported(MODEL_A, "A_RESTORED"));

    let probe = tokio::spawn({
        let app = app.clone();
        let credential = issued.credential.clone();
        async move { post_route(app, &credential, route_body("chain", None)).await }
    });
    wait_generation_at_least(&simulator, 3).await;
    assert_models(&generation_models(&simulator), &[MODEL_A, MODEL_B, MODEL_A]);

    let waiter =
        post_route(app.clone(), &issued.credential, route_body("chain", None)).await;
    assert_eq!(waiter.status(), StatusCode::OK);
    let waiter_body = body_json(waiter).await;
    assert_eq!(waiter_body["response"]["content"], CONTENT_B);
    assert_models(
        &generation_models(&simulator),
        &[MODEL_A, MODEL_B, MODEL_A, MODEL_B],
    );

    hold.release();
    let probe_response = probe.await.expect("join probe");
    assert_eq!(probe_response.status(), StatusCode::OK);
    let probe_body = body_json(probe_response).await;
    assert_eq!(probe_body["response"]["content"], CONTENT_A_OK);

    let restored = post_route(app, &issued.credential, route_body("chain", None)).await;
    assert_eq!(restored.status(), StatusCode::OK);
    let restored_body = body_json(restored).await;
    assert_eq!(restored_body["response"]["content"], "A_RESTORED");
    assert_models(
        &generation_models(&simulator),
        &[MODEL_A, MODEL_B, MODEL_A, MODEL_B, MODEL_A],
    );
    cleanup_clients(&pool, &[issued.client_id]).await;
}

#[tokio::test]
#[serial]
async fn failed_probe_reopens_target_circuit() {
    isolate_provider_environment();
    let pool = test_pool().await;
    let issued = provision_inference_key(&pool).await;
    let simulator = OpenAiSimulator::start().await;
    let cooldown = Duration::from_millis(80);
    let app = router(
        circuit_settings(&simulator, cooldown, |_| {}),
        auth_service_from_pool(pool.clone()),
    );

    simulator.enqueue_chat(upstream_500());
    simulator.enqueue_chat(chat_reported(REPORTED_B, CONTENT_B));
    let opened =
        post_route(app.clone(), &issued.credential, route_body("chain", None)).await;
    assert_eq!(opened.status(), StatusCode::OK);
    sleep(cooldown + Duration::from_millis(40)).await;

    simulator.enqueue_chat(upstream_500());
    simulator.enqueue_chat(chat_reported(REPORTED_B, CONTENT_B));
    let failed_probe =
        post_route(app.clone(), &issued.credential, route_body("chain", None)).await;
    assert_eq!(failed_probe.status(), StatusCode::OK);
    assert_models(
        &generation_models(&simulator),
        &[MODEL_A, MODEL_B, MODEL_A, MODEL_B],
    );

    simulator.enqueue_chat(chat_reported(REPORTED_B, CONTENT_B));
    let skipped = post_route(app, &issued.credential, route_body("chain", None)).await;
    assert_eq!(skipped.status(), StatusCode::OK);
    assert_models(
        &generation_models(&simulator),
        &[MODEL_A, MODEL_B, MODEL_A, MODEL_B, MODEL_B],
    );
    cleanup_clients(&pool, &[issued.client_id]).await;
}

//! Real Reqwest OpenAI adapter against an owned loopback simulator.

mod support;

use gateway::features::executor::{
    error::ExecutorError,
    models::{ExecutionParams, Message},
    vendors::{openai::OpenAIVendor, LLMVendor},
};
use serde_json::json;
use serial_test::serial;
use std::time::{Duration, Instant};
use support::{
    isolate_provider_environment, min_padded_chat_completion_len,
    openai_config_for_simulator, padded_chat_completion_bytes, OpenAiSimulator,
    ScriptedResponse, SIMULATED_CONTENT,
};

const REPORTED_SNAPSHOT_MODEL: &str = "reported-snapshot-model";
const ILLUSTRATIVE_PROMPT_TOKENS: i64 = 120;
const ILLUSTRATIVE_COMPLETION_TOKENS: i64 = 30;
const ILLUSTRATIVE_PRIMARY_COST: f64 = 0.00018;

fn user_params(content: &str) -> ExecutionParams {
    ExecutionParams {
        messages: vec![Message {
            role: "user".to_string(),
            content: content.to_string(),
        }],
        temperature: Some(0.2),
        max_tokens: Some(32),
        top_p: Some(0.9),
        stream: false,
    }
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
async fn real_adapter_posts_chat_completions_to_owned_simulator() {
    isolate_provider_environment();
    let simulator = OpenAiSimulator::start().await;
    let vendor = OpenAIVendor::new(openai_config_for_simulator(&simulator))
        .expect("adapter should construct with dummy local config");

    let result = vendor
        .execute("gpt-4", "gpt-4", user_params("adapter success"))
        .await
        .expect("simulator success should parse");

    assert_eq!(result.content, SIMULATED_CONTENT);
    assert_eq!(result.finish_reason, "stop");
    assert_eq!(result.prompt_tokens, 10);
    assert_eq!(result.completion_tokens, 5);
    assert_eq!(simulator.generation_count(), 1);
    assert_eq!(simulator.models_probe_count(), 0);

    let capture = simulator.captured().into_iter().next().unwrap();
    assert!(capture.is_generation());
    assert_eq!(capture.method, "POST");
    assert_eq!(capture.path, "/v1/chat/completions");
    assert!(capture.authorization_matches_fixture);
    assert_eq!(capture.content_type.as_deref(), Some("application/json"));
    let body = capture.body.expect("json body");
    assert_eq!(body["model"], "gpt-4");
    assert_eq!(body["messages"][0]["role"], "user");
    assert_eq!(body["messages"][0]["content"], "adapter success");
    assert_eq!(body["temperature"], 0.2);
    assert_eq!(body["top_p"], 0.9);
    assert_eq!(body["max_tokens"], 32);
    assert!(body.get("stream").is_none());
}

#[tokio::test]
#[serial]
async fn real_adapter_uses_provider_reported_model_and_selected_target_prices() {
    isolate_provider_environment();
    let simulator = OpenAiSimulator::start().await;
    simulator.enqueue_chat(ScriptedResponse::chat_reported(
        REPORTED_SNAPSHOT_MODEL,
        ILLUSTRATIVE_PROMPT_TOKENS,
        ILLUSTRATIVE_COMPLETION_TOKENS,
        "length",
    ));
    let vendor = OpenAIVendor::new(openai_config_for_simulator(&simulator))
        .expect("adapter should construct with dummy local config");

    let result = vendor
        .execute("gpt-4", "gpt-4", user_params("reported model"))
        .await
        .expect("simulator success should parse");

    assert_eq!(result.content, SIMULATED_CONTENT);
    assert_eq!(result.role, "assistant");
    assert_eq!(result.finish_reason, "length");
    assert_eq!(result.model_used, REPORTED_SNAPSHOT_MODEL);
    assert_ne!(result.model_used, "gpt-4");
    assert_eq!(result.prompt_tokens, ILLUSTRATIVE_PROMPT_TOKENS);
    assert_eq!(result.completion_tokens, ILLUSTRATIVE_COMPLETION_TOKENS);
    assert_cost_eq(result.total_cost, ILLUSTRATIVE_PRIMARY_COST);
    assert_eq!(simulator.generation_count(), 1);
    assert_eq!(simulator.models_probe_count(), 0);
}

#[tokio::test]
#[serial]
async fn missing_selected_target_pricing_does_not_silently_yield_zero() {
    isolate_provider_environment();
    let simulator = OpenAiSimulator::start().await;
    let mut config = openai_config_for_simulator(&simulator);
    config
        .supported_models
        .push("unpriced-target-model".to_string());
    config.pricing.remove("unpriced-target-model");
    let vendor = OpenAIVendor::new(config)
        .expect("adapter should construct with dummy local config");

    let error = vendor
        .execute(
            "unpriced-target-model",
            "unpriced-target-model",
            user_params("unpriced"),
        )
        .await
        .expect_err("missing pricing must not become a successful zero cost");

    let message = error.to_string();
    assert!(
        message.to_lowercase().contains("pricing")
            || message.to_lowercase().contains("price"),
        "missing pricing error should mention pricing: {message}"
    );
    assert!(!message.contains(simulator.credential()));
    assert_eq!(simulator.generation_count(), 1);
}

#[tokio::test]
#[serial]
async fn real_adapter_surfaces_scripted_provider_failure() {
    isolate_provider_environment();
    let simulator = OpenAiSimulator::start().await;
    simulator.enqueue_chat(ScriptedResponse::json_status(
        500,
        serde_json::json!({"error":{"message":"simulated upstream fault"}}),
    ));
    let vendor = OpenAIVendor::new(openai_config_for_simulator(&simulator))
        .expect("adapter should construct with dummy local config");

    let error = vendor
        .execute("gpt-4", "gpt-4", user_params("adapter failure"))
        .await
        .expect_err("scripted 500 should fail");

    let message = error.to_string();
    assert!(message.contains("API call failed"));
    assert!(!message.contains(simulator.credential()));
    assert_eq!(simulator.generation_count(), 1);
}

#[tokio::test]
#[serial]
async fn inherited_provider_env_cannot_redirect_adapter_to_external_url() {
    isolate_provider_environment();
    std::env::set_var("OPENAI_API_KEY", "sk-inherited-must-not-be-used");
    std::env::set_var("OPENAI_BASE_URL", "https://api.openai.com/v1");
    std::env::set_var("HTTP_PROXY", "http://127.0.0.1:9");
    std::env::set_var("HTTPS_PROXY", "http://127.0.0.1:9");

    isolate_provider_environment();
    let simulator = OpenAiSimulator::start().await;
    let vendor = OpenAIVendor::new(openai_config_for_simulator(&simulator))
        .expect("adapter should construct with dummy local config");
    vendor
        .execute("gpt-4", "gpt-4", user_params("ignore inherited env"))
        .await
        .expect("request must hit the owned loopback simulator");

    assert_eq!(simulator.generation_count(), 1);
    let capture = simulator.captured().into_iter().next().unwrap();
    assert!(capture.authorization_matches_fixture);
    assert_ne!(simulator.credential(), "sk-inherited-must-not-be-used");
}

const RAW_UPSTREAM_SENTINEL: &str = "RAW_UPSTREAM_SENTINEL";

fn valid_chat_json() -> serde_json::Value {
    serde_json::json!({
        "id": "chatcmpl-local-fixture",
        "object": "chat.completion",
        "created": 0,
        "model": "gpt-4",
        "choices": [{
            "index": 0,
            "message": {"role": "assistant", "content": SIMULATED_CONTENT},
            "finish_reason": "stop"
        }],
        "usage": {
            "prompt_tokens": 10,
            "completion_tokens": 5,
            "total_tokens": 15
        }
    })
}

fn assert_protocol_error(error: ExecutorError, expect_json: bool) {
    let text = error.to_string();
    assert!(
        !text.contains(RAW_UPSTREAM_SENTINEL),
        "protocol error leaked raw upstream content"
    );
    assert!(!text.contains(SIMULATED_CONTENT));
    if expect_json {
        match error {
            ExecutorError::JsonError(_) => {}
            other => panic!("expected JsonError, got {other:?}"),
        }
    } else {
        match error {
            ExecutorError::InvalidUpstreamResult => {}
            other => panic!("expected InvalidUpstreamResult, got {other:?}"),
        }
    }
}

#[tokio::test]
#[serial]
async fn real_adapter_rejects_malformed_success_payloads_without_fabricating_results() {
    isolate_provider_environment();
    let mut empty_choices = valid_chat_json();
    empty_choices["choices"] = serde_json::json!([]);
    let mut missing_model = valid_chat_json();
    missing_model.as_object_mut().unwrap().remove("model");
    let mut missing_content = valid_chat_json();
    missing_content["choices"][0]["message"]
        .as_object_mut()
        .unwrap()
        .remove("content");
    let mut missing_role = valid_chat_json();
    missing_role["choices"][0]["message"]
        .as_object_mut()
        .unwrap()
        .remove("role");
    let mut missing_finish = valid_chat_json();
    missing_finish["choices"][0]
        .as_object_mut()
        .unwrap()
        .remove("finish_reason");
    let mut missing_usage = valid_chat_json();
    missing_usage.as_object_mut().unwrap().remove("usage");
    let mut non_text = valid_chat_json();
    non_text["choices"][0]["message"]["content"] = serde_json::json!([
        {"type": "text", "text": SIMULATED_CONTENT}
    ]);
    let mut negative = valid_chat_json();
    negative["usage"]["prompt_tokens"] = serde_json::json!(-1);
    let mut fractional = valid_chat_json();
    fractional["usage"]["completion_tokens"] = serde_json::json!(1.5);
    let mut inconsistent = valid_chat_json();
    inconsistent["usage"]["total_tokens"] = serde_json::json!(99);

    let cases: Vec<(&str, Vec<u8>, bool)> = vec![
        (
            "malformed_json",
            format!("{{not-json {RAW_UPSTREAM_SENTINEL}").into_bytes(),
            true,
        ),
        (
            "empty_choices",
            empty_choices.to_string().into_bytes(),
            false,
        ),
        (
            "missing_model",
            missing_model.to_string().into_bytes(),
            false,
        ),
        (
            "missing_content",
            missing_content.to_string().into_bytes(),
            false,
        ),
        ("missing_role", missing_role.to_string().into_bytes(), false),
        (
            "missing_finish_reason",
            missing_finish.to_string().into_bytes(),
            false,
        ),
        (
            "missing_usage",
            missing_usage.to_string().into_bytes(),
            false,
        ),
        ("non_text_content", non_text.to_string().into_bytes(), false),
        ("negative_tokens", negative.to_string().into_bytes(), false),
        (
            "fractional_tokens",
            fractional.to_string().into_bytes(),
            false,
        ),
        (
            "inconsistent_total",
            inconsistent.to_string().into_bytes(),
            false,
        ),
        (
            "user_role",
            {
                let mut body = valid_chat_json();
                body["choices"][0]["message"]["role"] = serde_json::json!("user");
                body.to_string().into_bytes()
            },
            false,
        ),
        (
            "system_role",
            {
                let mut body = valid_chat_json();
                body["choices"][0]["message"]["role"] = serde_json::json!("system");
                body.to_string().into_bytes()
            },
            false,
        ),
        (
            "whitespace_role",
            {
                let mut body = valid_chat_json();
                body["choices"][0]["message"]["role"] = serde_json::json!(" assistant ");
                body.to_string().into_bytes()
            },
            false,
        ),
    ];

    for (name, body, expect_json) in cases {
        let simulator = OpenAiSimulator::start().await;
        simulator.enqueue_chat(ScriptedResponse::raw_json_bytes(body));
        let vendor = OpenAIVendor::new(openai_config_for_simulator(&simulator))
            .expect("adapter should construct with dummy local config");
        let error = vendor
            .execute("gpt-4", "gpt-4", user_params(name))
            .await
            .expect_err(name);
        assert_protocol_error(error, expect_json);
        assert_eq!(simulator.generation_count(), 1, "{name} must not retry");
    }
}

#[tokio::test]
#[serial]
async fn real_adapter_rejects_non_assistant_roles_then_parses_valid_control() {
    isolate_provider_environment();
    let simulator = OpenAiSimulator::start().await;
    for role in ["user", "system", " ", " assistant "] {
        let mut body = valid_chat_json();
        body["choices"][0]["message"]["role"] = serde_json::json!(role);
        simulator.enqueue_chat(ScriptedResponse::raw_json_bytes(
            body.to_string().into_bytes(),
        ));
    }
    simulator.enqueue_chat(ScriptedResponse::chat_ok());
    let vendor = OpenAIVendor::new(openai_config_for_simulator(&simulator))
        .expect("adapter should construct with dummy local config");

    for name in ["user", "system", "whitespace", "padded-assistant"] {
        let error = vendor
            .execute("gpt-4", "gpt-4", user_params(name))
            .await
            .expect_err(name);
        assert_protocol_error(error, false);
    }

    let result = vendor
        .execute("gpt-4", "gpt-4", user_params("valid assistant control"))
        .await
        .expect("exact assistant role must still parse");
    assert_eq!(result.content, SIMULATED_CONTENT);
    assert_eq!(result.role, "assistant");
    assert_eq!(result.model_used, "gpt-4");
    assert_eq!(simulator.generation_count(), 5);
}

#[tokio::test]
#[serial]
async fn real_adapter_uses_target_prices_when_requested_model_is_shared() {
    isolate_provider_environment();
    let simulator = OpenAiSimulator::start().await;
    simulator.enqueue_chat(ScriptedResponse::chat_reported(
        REPORTED_SNAPSHOT_MODEL,
        ILLUSTRATIVE_PROMPT_TOKENS,
        ILLUSTRATIVE_COMPLETION_TOKENS,
        "stop",
    ));
    simulator.enqueue_chat(ScriptedResponse::chat_reported(
        REPORTED_SNAPSHOT_MODEL,
        ILLUSTRATIVE_PROMPT_TOKENS,
        ILLUSTRATIVE_COMPLETION_TOKENS,
        "stop",
    ));
    let mut config = openai_config_for_simulator(&simulator);
    config
        .supported_models
        .push("example-chat-model".to_string());
    config.pricing.insert(
        "same-model-primary".to_string(),
        gateway::features::executor::config::ModelPricing::new(1.0, 2.0),
    );
    config.pricing.insert(
        "same-model-alt".to_string(),
        gateway::features::executor::config::ModelPricing::new(10.0, 20.0),
    );
    let vendor = OpenAIVendor::new(config)
        .expect("adapter should construct with dummy local config");

    let primary = vendor
        .execute(
            "example-chat-model",
            "same-model-primary",
            user_params("same-model primary"),
        )
        .await
        .expect("primary same-model target should parse");
    let alt = vendor
        .execute(
            "example-chat-model",
            "same-model-alt",
            user_params("same-model alt"),
        )
        .await
        .expect("alt same-model target should parse");

    assert_eq!(primary.model_used, REPORTED_SNAPSHOT_MODEL);
    assert_eq!(alt.model_used, REPORTED_SNAPSHOT_MODEL);
    assert_cost_eq(primary.total_cost, ILLUSTRATIVE_PRIMARY_COST);
    assert_cost_eq(alt.total_cost, 0.0018);
    assert_ne!(primary.total_cost, alt.total_cost);

    let captures: Vec<_> = simulator
        .captured()
        .into_iter()
        .filter(|capture| capture.is_generation())
        .collect();
    assert_eq!(captures.len(), 2);
    assert_eq!(
        captures[0].body.as_ref().expect("primary wire")["model"],
        "example-chat-model"
    );
    assert_eq!(
        captures[1].body.as_ref().expect("alt wire")["model"],
        "example-chat-model"
    );
}

#[tokio::test]
#[serial]
async fn real_adapter_parses_a_valid_response_after_a_protocol_error() {
    isolate_provider_environment();
    let simulator = OpenAiSimulator::start().await;
    simulator.enqueue_chat(ScriptedResponse::raw_json_bytes(
        format!("{{not-json {RAW_UPSTREAM_SENTINEL}").into_bytes(),
    ));
    simulator.enqueue_chat(ScriptedResponse::chat_ok());
    let vendor = OpenAIVendor::new(openai_config_for_simulator(&simulator))
        .expect("adapter should construct with dummy local config");

    let error = vendor
        .execute("gpt-4", "gpt-4", user_params("first malformed"))
        .await
        .expect_err("malformed payload must fail");
    assert_protocol_error(error, true);

    let result = vendor
        .execute("gpt-4", "gpt-4", user_params("second valid"))
        .await
        .expect("later valid payload must parse");
    assert_eq!(result.content, SIMULATED_CONTENT);
    assert_eq!(result.model_used, "gpt-4");
    assert_eq!(result.prompt_tokens, 10);
    assert_eq!(result.completion_tokens, 5);
    assert_eq!(simulator.generation_count(), 2);
}

#[tokio::test]
#[serial]
async fn real_adapter_enforces_response_size_while_reading() {
    isolate_provider_environment();
    let bound = min_padded_chat_completion_len() + 32;
    let exact = padded_chat_completion_bytes(bound);
    let over = padded_chat_completion_bytes(bound + 1);
    assert_eq!(exact.len(), bound);
    assert_eq!(over.len(), bound + 1);

    let simulator = OpenAiSimulator::start().await;
    simulator.enqueue_chat(ScriptedResponse::raw_json_bytes(exact));
    simulator.enqueue_chat(ScriptedResponse::raw_json_bytes(over.clone()));
    simulator.enqueue_chat(ScriptedResponse::advertised_length(
        over.clone(),
        (bound as u64) + 1,
    ));
    simulator.enqueue_chat(ScriptedResponse::chunked(over, 256_000));
    let mut config = openai_config_for_simulator(&simulator);
    config.max_response_bytes = bound as u64;
    let vendor = OpenAIVendor::new(config)
        .expect("adapter should construct with dummy local config");

    let ok = vendor
        .execute("gpt-4", "gpt-4", user_params("exact bound"))
        .await
        .expect("exact-bound JSON must succeed");
    assert_eq!(ok.content, SIMULATED_CONTENT);
    assert_eq!(ok.prompt_tokens, 10);
    assert_eq!(ok.completion_tokens, 5);

    for name in ["one over", "advertised length", "chunked"] {
        let error = vendor
            .execute("gpt-4", "gpt-4", user_params(name))
            .await
            .expect_err(name);
        match &error {
            ExecutorError::InvalidUpstreamResult => {}
            other => panic!("{name} expected InvalidUpstreamResult, got {other:?}"),
        }
        let text = error.to_string();
        assert!(!text.contains(&"a".repeat(32)));
        assert!(!text.contains("XXXX"));
    }
    assert_eq!(simulator.generation_count(), 4);
}

#[tokio::test]
#[serial]
async fn real_adapter_preserves_429_retry_after_without_waiting_for_stalled_body() {
    isolate_provider_environment();
    let simulator = OpenAiSimulator::start().await;
    simulator.enqueue_chat(
        ScriptedResponse::Json {
            status: 429,
            body: json!({"error":{"message":"rate"}}),
            retry_after: Some("30".to_string()),
        }
        .delay_body(Duration::from_secs(2)),
    );
    let mut config = openai_config_for_simulator(&simulator);
    config.timeout_ms = 200;
    let vendor = OpenAIVendor::new(config)
        .expect("adapter should construct with dummy local config");

    let started = Instant::now();
    let error = vendor
        .execute("gpt-4", "gpt-4", user_params("stalled 429"))
        .await
        .expect_err("429 headers must fail without a success body");
    let elapsed = started.elapsed();
    match error {
        ExecutorError::RateLimitExceeded {
            vendor,
            retry_after_ms,
        } => {
            assert_eq!(vendor, "openai");
            assert_eq!(retry_after_ms, Some(30_000));
        }
        other => panic!("expected RateLimitExceeded, got {other:?}"),
    }
    assert!(
        elapsed < Duration::from_millis(500),
        "stalled 429 body must not delay typed throttling, took {elapsed:?}"
    );
    assert_eq!(simulator.generation_count(), 1);
}

#[tokio::test]
#[serial]
async fn real_adapter_classifies_complete_429_quota_from_code_or_type() {
    isolate_provider_environment();
    let cases = [("code-only", true, false), ("type-only", false, true)];
    for (label, code, error_type) in cases {
        let simulator = OpenAiSimulator::start().await;
        simulator.enqueue_chat(ScriptedResponse::quota_429(code, error_type));
        simulator.enqueue_chat(ScriptedResponse::chat_ok());
        let vendor = OpenAIVendor::new(openai_config_for_simulator(&simulator))
            .expect("adapter should construct with dummy local config");
        let error = vendor
            .execute("gpt-4", "gpt-4", user_params(label))
            .await
            .expect_err(label);
        match error {
            ExecutorError::QuotaExceeded => {}
            other => panic!("{label} expected QuotaExceeded, got {other:?}"),
        }
        assert_eq!(
            simulator.generation_count(),
            1,
            "{label} must not retry a complete quota 429"
        );
    }
}

#[tokio::test]
#[serial]
async fn real_adapter_keeps_throttling_for_malformed_and_oversized_429_bodies() {
    isolate_provider_environment();
    let bound = 64_u64;
    let simulator = OpenAiSimulator::start().await;
    simulator.enqueue_chat(ScriptedResponse::malformed_429(
        br#"{"error":{"code":"not-json"#,
        Some("12"),
    ));
    simulator.enqueue_chat(ScriptedResponse::advertised_429(
        br#"{"error":{"code":"insufficient_quota","type":"insufficient_quota"}}"#,
        bound + 1,
        Some("15"),
    ));
    simulator.enqueue_chat(ScriptedResponse::chat_ok());
    let mut config = openai_config_for_simulator(&simulator);
    config.max_response_bytes = bound;
    let vendor = OpenAIVendor::new(config)
        .expect("adapter should construct with dummy local config");

    let malformed = vendor
        .execute("gpt-4", "gpt-4", user_params("malformed 429"))
        .await
        .expect_err("malformed 429 must stay throttling");
    match malformed {
        ExecutorError::RateLimitExceeded {
            vendor,
            retry_after_ms,
        } => {
            assert_eq!(vendor, "openai");
            assert_eq!(retry_after_ms, Some(12_000));
        }
        other => panic!("expected RateLimitExceeded for malformed 429, got {other:?}"),
    }

    let oversized = vendor
        .execute("gpt-4", "gpt-4", user_params("oversized 429"))
        .await
        .expect_err("oversized 429 must stay throttling");
    match oversized {
        ExecutorError::RateLimitExceeded {
            vendor,
            retry_after_ms,
        } => {
            assert_eq!(vendor, "openai");
            assert_eq!(retry_after_ms, Some(15_000));
        }
        other => panic!("expected RateLimitExceeded for oversized 429, got {other:?}"),
    }
    assert_eq!(simulator.generation_count(), 2);
}

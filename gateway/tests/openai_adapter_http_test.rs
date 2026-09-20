//! Real Reqwest OpenAI adapter against an owned loopback simulator.

mod support;

use gateway::features::executor::{
    models::{ExecutionParams, Message},
    vendors::{openai::OpenAIVendor, LLMVendor},
};
use serial_test::serial;
use support::{
    isolate_provider_environment, openai_config_for_simulator, OpenAiSimulator,
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
        .execute("gpt-4", user_params("adapter success"))
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
        .execute("gpt-4", user_params("reported model"))
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
        .execute("unpriced-target-model", user_params("unpriced"))
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
        .execute("gpt-4", user_params("adapter failure"))
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
        .execute("gpt-4", user_params("ignore inherited env"))
        .await
        .expect("request must hit the owned loopback simulator");

    assert_eq!(simulator.generation_count(), 1);
    let capture = simulator.captured().into_iter().next().unwrap();
    assert!(capture.authorization_matches_fixture);
    assert_ne!(simulator.credential(), "sk-inherited-must-not-be-used");
}

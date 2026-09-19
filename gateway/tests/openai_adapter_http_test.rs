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

fn user_params(content: &str) -> ExecutionParams {
    ExecutionParams {
        messages: vec![Message {
            role: "user".to_string(),
            content: content.to_string(),
        }],
        temperature: Some(0.2),
        max_tokens: Some(32),
        top_p: None,
        stream: false,
    }
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
    assert!(capture.authorization_matches_fixture);
    assert_eq!(capture.content_type.as_deref(), Some("application/json"));
    let body = capture.body.expect("json body");
    assert_eq!(body["model"], "gpt-4");
    assert_eq!(body["messages"][0]["role"], "user");
    assert_eq!(body["temperature"], 0.2);
    assert_eq!(body["max_tokens"], 32);
    assert!(body.get("stream").is_none());
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

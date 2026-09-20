//! Deferred live-provider verification.
//!
//! These tests are **ignored by default** and must not run in routine CI or
//! local `cargo test`. Inherited `OPENAI_API_KEY` presence does not activate
//! them. Live OpenAI verification is unrun for this mission.
//!
//! Explicit opt-in only, when separately requested:
//! ```bash
//! OPMUX_LIVE_PROVIDER_TESTS=1 cargo test -p gateway --test executor_integration_test -- --ignored --nocapture
//! ```

use gateway::core::contracts::RoutePlan;
use gateway::core::deadline::RequestDeadline;
use gateway::features::executor::{config::ExecutorConfig, service::ExecutorService};
use serde_json::json;
use std::time::Duration;

const LIVE_OPT_IN: &str = "OPMUX_LIVE_PROVIDER_TESTS";

/// Live tests require an explicit opt-in. Credential presence is not enough.
fn live_provider_tests_enabled() -> bool {
    matches!(std::env::var(LIVE_OPT_IN).as_deref(), Ok("1") | Ok("true"))
}

fn require_live_opt_in() {
    assert!(
        live_provider_tests_enabled(),
        "live provider tests are deferred; set {LIVE_OPT_IN}=1 only when explicitly requested"
    );
}

fn create_test_route_plan(vendor_id: &str, model_id: &str) -> RoutePlan {
    RoutePlan {
        vendor_id: vendor_id.to_string(),
        target_id: model_id.to_string(),
        model_id: model_id.to_string(),
        fallback_plans: vec![],
    }
}

fn create_test_payload(content: &str) -> serde_json::Value {
    json!({
        "messages": [
            {"role": "user", "content": content}
        ],
        "temperature": 0.7,
        "max_tokens": 50
    })
}

#[tokio::test]
#[ignore = "live provider verification is deferred and unrun; opt in with OPMUX_LIVE_PROVIDER_TESTS=1 and --ignored"]
async fn test_openai_api_basic_execution() {
    require_live_opt_in();

    let config = ExecutorConfig::from_env();
    config.validate();

    let service = ExecutorService::from_config(config)
        .expect("Failed to create ExecutorService from config");

    let plan = create_test_route_plan("openai", "gpt-3.5-turbo");
    let payload = create_test_payload("Say 'Hello, World!' and nothing else.");
    let result = service
        .execute(
            &plan,
            &payload,
            RequestDeadline::from_timeout(Duration::from_secs(60)),
        )
        .await;

    match result {
        Ok(execution_result) => {
            assert_eq!(execution_result.model_used, "gpt-3.5-turbo");
            assert!(
                !execution_result.content.is_empty(),
                "Response should not be empty"
            );
            assert!(
                execution_result.prompt_tokens > 0,
                "Should have prompt tokens"
            );
            assert!(
                execution_result.completion_tokens > 0,
                "Should have completion tokens"
            );
            assert!(
                execution_result.total_cost >= 0.0,
                "Cost should be non-negative"
            );
            assert_eq!(execution_result.finish_reason, "stop");
        }
        Err(error) => {
            panic!("live API call failed: {error:?}");
        }
    }
}

#[tokio::test]
#[ignore = "live provider verification is deferred and unrun; opt in with OPMUX_LIVE_PROVIDER_TESTS=1 and --ignored"]
async fn test_openai_api_with_different_models() {
    require_live_opt_in();

    let config = ExecutorConfig::from_env();
    let service =
        ExecutorService::from_config(config).expect("Failed to create ExecutorService");

    let plan = create_test_route_plan("openai", "gpt-3.5-turbo");
    let payload = json!({
        "messages": [
            {"role": "user", "content": "Say 'Hello' and nothing else."}
        ],
        "temperature": 0.5,
        "max_tokens": 30
    });

    let result = service
        .execute(
            &plan,
            &payload,
            RequestDeadline::from_timeout(Duration::from_secs(60)),
        )
        .await;
    assert!(result.is_ok(), "gpt-3.5-turbo should succeed");
}

#[tokio::test]
#[ignore = "live provider verification is deferred and unrun; opt in with OPMUX_LIVE_PROVIDER_TESTS=1 and --ignored"]
async fn test_openai_api_parameter_extraction() {
    require_live_opt_in();

    let config = ExecutorConfig::from_env();
    let service =
        ExecutorService::from_config(config).expect("Failed to create ExecutorService");

    let plan = create_test_route_plan("openai", "gpt-3.5-turbo");
    let payload = json!({
        "messages": [
            {"role": "user", "content": "What is 2+2?"}
        ],
        "temperature": 0.3,
        "max_tokens": 20
    });

    let result = service
        .execute(
            &plan,
            &payload,
            RequestDeadline::from_timeout(Duration::from_secs(60)),
        )
        .await;
    match result {
        Ok(execution_result) => {
            assert!(!execution_result.content.is_empty());
        }
        Err(error) => {
            panic!("live parameter extraction failed: {error:?}");
        }
    }
}

#[tokio::test]
#[ignore = "live provider verification is deferred and unrun; opt in with OPMUX_LIVE_PROVIDER_TESTS=1 and --ignored"]
async fn test_openai_api_retry_logic() {
    require_live_opt_in();

    let config = ExecutorConfig::from_env();
    let service =
        ExecutorService::from_config(config).expect("Failed to create ExecutorService");

    let plan = create_test_route_plan("openai", "gpt-3.5-turbo");
    let payload = create_test_payload("Say 'Test' and nothing else.");
    let result = service
        .execute(
            &plan,
            &payload,
            RequestDeadline::from_timeout(Duration::from_secs(60)),
        )
        .await;
    assert!(
        result.is_ok(),
        "Retry logic should handle transient failures"
    );
}

#[tokio::test]
#[ignore = "live provider verification is deferred and unrun; opt in with OPMUX_LIVE_PROVIDER_TESTS=1 and --ignored"]
async fn test_openai_api_unsupported_model() {
    require_live_opt_in();

    let config = ExecutorConfig::from_env();
    let service =
        ExecutorService::from_config(config).expect("Failed to create ExecutorService");

    let plan = create_test_route_plan("openai", "gpt-5-ultra");
    let payload = create_test_payload("Test");
    let result = service
        .execute(
            &plan,
            &payload,
            RequestDeadline::from_timeout(Duration::from_secs(60)),
        )
        .await;
    assert!(result.is_err(), "Should fail with unsupported model");
}

#[tokio::test]
#[ignore = "live provider verification is deferred and unrun; opt in with OPMUX_LIVE_PROVIDER_TESTS=1 and --ignored"]
async fn test_openai_api_cost_calculation() {
    require_live_opt_in();

    let config = ExecutorConfig::from_env();
    let service =
        ExecutorService::from_config(config).expect("Failed to create ExecutorService");

    let plan = create_test_route_plan("openai", "gpt-3.5-turbo");
    let payload = json!({
        "messages": [
            {"role": "user", "content": "Count from 1 to 5."}
        ],
        "temperature": 0.5,
        "max_tokens": 50
    });

    let result = service
        .execute(
            &plan,
            &payload,
            RequestDeadline::from_timeout(Duration::from_secs(60)),
        )
        .await
        .expect("API call should succeed");

    assert!(
        result.total_cost > 0.0,
        "Cost should be calculated and positive"
    );
    assert!(
        result.total_cost < 0.01,
        "Cost should be reasonable for small request"
    );
}

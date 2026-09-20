#[cfg(test)]
mod tests {
    use crate::core::config::Settings;
    use crate::core::deadline::RequestDeadline;
    use crate::features::executor::{
        config::ExecutorConfig,
        error::ExecutorError,
        models::{ExecutionParams, ExecutionResult},
        repository::ExecutorRepository,
        service::ExecutorService,
        vendors::LLMVendor,
    };
    use crate::features::ingress::{
        error::IngressError,
        service::{GenerationParameters, IngressRequest, IngressService},
    };
    use async_trait::async_trait;
    use serde_json::json;
    use std::collections::HashMap;
    use std::sync::Arc;
    use std::time::Duration;

    #[derive(Clone)]
    struct MockVendor {
        vendor_id: String,
        supported_models: Vec<String>,
    }

    impl MockVendor {
        fn new(vendor_id: &str, models: Vec<&str>) -> Self {
            Self {
                vendor_id: vendor_id.to_string(),
                supported_models: models.into_iter().map(|m| m.to_string()).collect(),
            }
        }
    }

    #[async_trait]
    impl LLMVendor for MockVendor {
        async fn execute(
            &self,
            model: &str,
            _target_id: &str,
            _params: ExecutionParams,
        ) -> Result<ExecutionResult, ExecutorError> {
            Ok(ExecutionResult {
                content: "Mock ingress response".to_string(),
                role: "assistant".to_string(),
                model_used: model.to_string(),
                prompt_tokens: 15,
                completion_tokens: 25,
                total_cost: 0.001,
                finish_reason: "stop".to_string(),
            })
        }

        fn vendor_id(&self) -> &str {
            &self.vendor_id
        }

        fn supports_model(&self, model: &str) -> bool {
            self.supported_models.contains(&model.to_string())
        }

        fn calculate_cost(
            &self,
            _prompt_tokens: i64,
            _completion_tokens: i64,
            _target_id: &str,
        ) -> Result<f64, ExecutorError> {
            Ok(0.001)
        }

        async fn health_check(&self, _timeout_secs: u64) -> Result<(), ExecutorError> {
            Ok(())
        }
    }

    fn create_mock_executor_service(models: Vec<&str>) -> Arc<ExecutorService> {
        let vendor = MockVendor::new("openai", models);

        let mut vendors = HashMap::new();
        vendors.insert("openai".to_string(), Arc::new(vendor) as Arc<dyn LLMVendor>);

        let repository = ExecutorRepository { vendors };

        Arc::new(ExecutorService::from_repository(
            repository,
            ExecutorConfig::mock_policy(3, 30_000),
        ))
    }

    fn generous_deadline() -> RequestDeadline {
        RequestDeadline::from_timeout(Duration::from_secs(60))
    }

    fn service_with_models(models: Vec<&str>) -> IngressService {
        IngressService::new(
            create_mock_executor_service(models),
            Arc::new(Settings::for_tests()),
        )
    }

    #[tokio::test]
    async fn test_process_request_success_returns_expected_response_shape() {
        let service =
            service_with_models(vec!["example-chat-model", "example-chat-model-mini"]);

        let result = service
            .process_request(
                IngressRequest {
                    prompt: "Hello ingress".to_string(),
                    metadata: json!({ "rewrite": false, "conversation_id": "ignored" }),
                    route: None,
                    allow_fallback: None,
                    parameters: GenerationParameters::default(),
                },
                generous_deadline(),
            )
            .await
            .expect("ingress request should succeed");

        assert_eq!(result.response.role, "assistant");
        assert_eq!(result.response.content, "Mock ingress response");
        assert_eq!(result.response.finish_reason, Some("stop".to_string()));
        assert_eq!(result.model_used, "example-chat-model");
        assert_eq!(result.cost, 0.001);
        assert_eq!(result.usage.prompt_tokens, 15);
        assert_eq!(result.usage.completion_tokens, 25);
        assert!(result.processing_time_ms < 5_000);
    }

    #[tokio::test]
    async fn named_route_uses_configured_primary_model() {
        let service =
            service_with_models(vec!["example-chat-model", "example-chat-model-mini"]);

        let result = service
            .process_request(
                IngressRequest {
                    prompt: "Named route".to_string(),
                    metadata: json!({}),
                    route: Some("fast".to_string()),
                    allow_fallback: Some(false),
                    parameters: GenerationParameters::default(),
                },
                generous_deadline(),
            )
            .await
            .expect("named route should succeed");

        assert_eq!(result.model_used, "example-chat-model-mini");
    }

    #[tokio::test]
    async fn unknown_route_fails_before_execution() {
        let service = service_with_models(vec!["example-chat-model"]);

        let result = service
            .process_request(
                IngressRequest {
                    prompt: "Unknown route".to_string(),
                    metadata: json!({}),
                    route: Some("missing".to_string()),
                    allow_fallback: None,
                    parameters: GenerationParameters::default(),
                },
                generous_deadline(),
            )
            .await;

        match result {
            Err(IngressError::InvalidRequest(message)) => {
                assert_eq!(message, "Unknown route");
            }
            _ => panic!("expected unknown route"),
        }
    }

    #[tokio::test]
    async fn test_process_request_returns_execution_failed_for_unsupported_model() {
        let service = service_with_models(vec!["gpt-3.5-turbo"]);

        let result = service
            .process_request(
                IngressRequest {
                    prompt: "Trigger unsupported model".to_string(),
                    metadata: json!({}),
                    route: None,
                    allow_fallback: Some(false),
                    parameters: GenerationParameters::default(),
                },
                generous_deadline(),
            )
            .await;

        match result {
            Err(IngressError::ExecutionFailed(ExecutorError::UnsupportedModel(
                vendor,
                model,
            ))) => {
                assert_eq!(vendor, "openai");
                assert_eq!(model, "example-chat-model");
            }
            _ => panic!(
                "Expected UnsupportedModel wrapped by IngressError::ExecutionFailed"
            ),
        }
    }

    #[tokio::test]
    async fn max_tokens_above_primary_cap_fails_before_execution() {
        let service =
            service_with_models(vec!["example-chat-model", "example-chat-model-mini"]);

        let result = service
            .process_request(
                IngressRequest {
                    prompt: "too many tokens".to_string(),
                    metadata: json!({}),
                    route: None,
                    allow_fallback: Some(false),
                    parameters: GenerationParameters {
                        max_tokens: Some(513),
                        ..GenerationParameters::default()
                    },
                },
                generous_deadline(),
            )
            .await;

        match result {
            Err(IngressError::InvalidRequest(message)) => {
                assert!(message.contains("max_output_tokens"));
            }
            other => panic!(
                "expected primary cap rejection, got success={}",
                other.is_ok()
            ),
        }
    }
}

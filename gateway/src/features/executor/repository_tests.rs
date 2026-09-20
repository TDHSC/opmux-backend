// Tests for ExecutorRepository

#[cfg(test)]
mod tests {
    use crate::core::contracts::RoutePlan;
    use crate::features::executor::{
        config::{ExecutorConfig, OpenAIConfig},
        error::ExecutorError,
        models::{ExecutionParams, ExecutionResult, Message},
        repository::ExecutorRepository,
        vendors::LLMVendor,
    };
    use async_trait::async_trait;
    use std::collections::HashMap;
    use std::sync::Arc;

    /// Mock vendor for testing repository layer.
    #[derive(Clone)]
    struct MockVendor {
        vendor_id: String,
        supported_models: Vec<String>,
    }

    impl MockVendor {
        fn new(vendor_id: &str, models: Vec<&str>) -> Self {
            Self {
                vendor_id: vendor_id.to_string(),
                supported_models: models.iter().map(|s| s.to_string()).collect(),
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
                content: format!("Mock response from {} using {}", self.vendor_id, model),
                role: "assistant".to_string(),
                model_used: model.to_string(),
                prompt_tokens: 10,
                completion_tokens: 20,
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
            // Mock vendor always returns healthy
            Ok(())
        }
    }

    fn test_plan(vendor_id: &str, model_id: &str) -> RoutePlan {
        RoutePlan {
            vendor_id: vendor_id.to_string(),
            target_id: model_id.to_string(),
            model_id: model_id.to_string(),
            fallback_plans: vec![],
        }
    }

    // Helper function to create a test ExecutorRepository with OpenAI vendor
    fn create_test_repository() -> ExecutorRepository {
        let config = ExecutorConfig {
            openai: Some(OpenAIConfig::for_tests()),
            ..ExecutorConfig::mock_policy(3, 30_000)
        };
        ExecutorRepository::from_config(config).expect("Failed to create test repository")
    }

    // Helper function to create ExecutorRepository with mock vendors
    fn create_mock_repository(
        vendors: Vec<(String, Arc<dyn LLMVendor>)>,
    ) -> ExecutorRepository {
        let mut vendor_map = HashMap::new();
        for (id, vendor) in vendors {
            vendor_map.insert(id, vendor);
        }

        ExecutorRepository {
            vendors: vendor_map,
        }
    }

    #[test]
    fn test_from_config_success() {
        let config = ExecutorConfig {
            openai: Some(OpenAIConfig::for_tests()),
            ..ExecutorConfig::mock_policy(3, 30_000)
        };

        let result = ExecutorRepository::from_config(config);
        assert!(result.is_ok());

        let repo = result.unwrap();
        assert_eq!(repo.vendor_count(), 1);
    }

    #[test]
    fn test_from_config_no_vendors() {
        let config = ExecutorConfig::mock_policy(3, 30_000);

        let result = ExecutorRepository::from_config(config);
        assert!(result.is_err());

        match result {
            Err(ExecutorError::NoVendorsConfigured) => {}
            _ => panic!("Expected NoVendorsConfigured error"),
        }
    }

    #[test]
    fn test_get_vendor_success() {
        let repo = create_test_repository();

        let result = repo.get_vendor("openai");
        assert!(result.is_ok());

        let vendor = result.unwrap();
        assert_eq!(vendor.vendor_id(), "openai");
    }

    #[test]
    fn test_get_vendor_not_found() {
        let repo = create_test_repository();

        let result = repo.get_vendor("unknown_vendor");
        assert!(result.is_err());

        match result {
            Err(ExecutorError::UnsupportedVendor(vendor_id)) => {
                assert_eq!(vendor_id, "unknown_vendor");
            }
            _ => panic!("Expected UnsupportedVendor error"),
        }
    }

    #[test]
    fn test_get_vendor_case_sensitive() {
        let repo = create_test_repository();

        // Vendor IDs are case-sensitive
        let result = repo.get_vendor("OpenAI"); // Wrong case
        assert!(result.is_err());

        match result {
            Err(ExecutorError::UnsupportedVendor(vendor_id)) => {
                assert_eq!(vendor_id, "OpenAI");
            }
            _ => panic!("Expected UnsupportedVendor error"),
        }
    }

    #[test]
    fn test_vendor_count() {
        let repo = create_test_repository();

        // Should have 1 vendor (OpenAI) if OPENAI_API_KEY is set
        assert_eq!(repo.vendor_count(), 1);
    }

    #[tokio::test]
    async fn test_call_llm_success() {
        let mock_vendor = MockVendor::new("mock_vendor", vec!["gpt-4", "gpt-3.5-turbo"]);
        let repo = create_mock_repository(vec![(
            "mock_vendor".to_string(),
            Arc::new(mock_vendor),
        )]);

        let params = ExecutionParams {
            messages: vec![Message {
                role: "user".to_string(),
                content: "Hello".to_string(),
            }],
            temperature: Some(0.7),
            max_tokens: Some(100),
            top_p: None,
            stream: false,
        };

        let result = repo
            .call_llm(&test_plan("mock_vendor", "gpt-4"), &params)
            .await;
        assert!(result.is_ok());

        let execution_result = result.unwrap();
        assert_eq!(execution_result.model_used, "gpt-4");
        assert!(execution_result.content.contains("Mock response"));
    }

    #[tokio::test]
    async fn test_call_llm_vendor_not_found() {
        let repo = create_mock_repository(vec![]);

        let params = ExecutionParams {
            messages: vec![Message {
                role: "user".to_string(),
                content: "Hello".to_string(),
            }],
            temperature: None,
            max_tokens: None,
            top_p: None,
            stream: false,
        };

        let result = repo
            .call_llm(&test_plan("unknown_vendor", "gpt-4"), &params)
            .await;
        assert!(result.is_err());

        match result {
            Err(ExecutorError::UnsupportedVendor(vendor_id)) => {
                assert_eq!(vendor_id, "unknown_vendor");
            }
            _ => panic!("Expected UnsupportedVendor error"),
        }
    }

    #[tokio::test]
    async fn test_call_llm_model_not_supported() {
        let mock_vendor = MockVendor::new("mock_vendor", vec!["gpt-4"]); // Only supports gpt-4
        let repo = create_mock_repository(vec![(
            "mock_vendor".to_string(),
            Arc::new(mock_vendor),
        )]);

        let params = ExecutionParams {
            messages: vec![Message {
                role: "user".to_string(),
                content: "Hello".to_string(),
            }],
            temperature: None,
            max_tokens: None,
            top_p: None,
            stream: false,
        };

        let result = repo
            .call_llm(&test_plan("mock_vendor", "unsupported-model"), &params)
            .await;
        assert!(result.is_err());

        match result {
            Err(ExecutorError::UnsupportedModel(vendor_id, model_id)) => {
                assert_eq!(vendor_id, "mock_vendor");
                assert_eq!(model_id, "unsupported-model");
            }
            _ => panic!("Expected UnsupportedModel error"),
        }
    }
}

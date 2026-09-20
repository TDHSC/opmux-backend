// Tests for ExecutorService

#[cfg(test)]
mod tests {
    use crate::core::contracts::RoutePlan;
    use crate::core::deadline::RequestDeadline;
    use crate::features::executor::circuit::TargetCircuitRegistry;
    use crate::features::executor::{
        config::{ExecutorConfig, OpenAIConfig},
        error::ExecutorError,
        models::{ExecutionParams, ExecutionResult, Message},
        repository::ExecutorRepository,
        service::ExecutorService,
        vendors::LLMVendor,
    };
    use async_trait::async_trait;
    use serde_json::json;
    use std::collections::HashMap;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;
    use std::time::Duration;

    /// Configurable mock vendor for testing.
    ///
    /// Supports various failure scenarios:
    /// - Fail N times then succeed
    /// - Always fail with specific error
    /// - Always succeed
    /// - Model support configuration
    #[derive(Clone)]
    struct MockVendor {
        vendor_id: String,
        supported_models: Vec<String>,
        /// Number of times to fail before succeeding (None = always succeed)
        fail_count: Option<Arc<AtomicUsize>>,
        /// Error to return on failure
        failure_error: Option<ExecutorError>,
    }

    impl MockVendor {
        /// Creates a mock vendor that always succeeds.
        fn new_success(vendor_id: &str, models: Vec<&str>) -> Self {
            Self {
                vendor_id: vendor_id.to_string(),
                supported_models: models.iter().map(|s| s.to_string()).collect(),
                fail_count: None,
                failure_error: None,
            }
        }

        /// Creates a mock vendor that fails N times then succeeds.
        fn new_fail_then_succeed(
            vendor_id: &str,
            models: Vec<&str>,
            fail_times: usize,
            error: ExecutorError,
        ) -> Self {
            Self {
                vendor_id: vendor_id.to_string(),
                supported_models: models.iter().map(|s| s.to_string()).collect(),
                fail_count: Some(Arc::new(AtomicUsize::new(fail_times))),
                failure_error: Some(error),
            }
        }

        /// Creates a mock vendor that always fails.
        fn new_always_fail(
            vendor_id: &str,
            models: Vec<&str>,
            error: ExecutorError,
        ) -> Self {
            Self {
                vendor_id: vendor_id.to_string(),
                supported_models: models.iter().map(|s| s.to_string()).collect(),
                fail_count: Some(Arc::new(AtomicUsize::new(usize::MAX))),
                failure_error: Some(error),
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
            // Check if we should fail
            if let Some(ref counter) = self.fail_count {
                let remaining = counter.load(Ordering::SeqCst);
                if remaining > 0 {
                    counter.fetch_sub(1, Ordering::SeqCst);
                    return Err(self.failure_error.clone().unwrap());
                }
            }

            // Success case
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
            // Check if we should fail (same logic as execute)
            if let Some(ref counter) = self.fail_count {
                let remaining = counter.load(Ordering::SeqCst);
                if remaining > 0 {
                    return Err(self.failure_error.clone().unwrap());
                }
            }
            // Success case
            Ok(())
        }
    }

    fn generous_deadline() -> RequestDeadline {
        RequestDeadline::from_timeout(Duration::from_secs(60))
    }

    fn test_plan(vendor_id: &str, model_id: &str) -> RoutePlan {
        RoutePlan {
            vendor_id: vendor_id.to_string(),
            target_id: model_id.to_string(),
            model_id: model_id.to_string(),
            max_output_tokens: 4_096,
            fallback_plans: vec![],
        }
    }

    // Helper function to create a test ExecutorService with OpenAI vendor
    fn create_test_service() -> ExecutorService {
        let config = ExecutorConfig {
            openai: Some(OpenAIConfig::for_tests()),
            ..ExecutorConfig::mock_policy(3, 30_000)
        };
        ExecutorService::from_config(config).expect("Failed to create test service")
    }

    // Helper function to create ExecutorService with mock vendors
    fn create_mock_service(
        vendors: Vec<(String, Arc<dyn LLMVendor>)>,
    ) -> ExecutorService {
        let mut vendor_map = HashMap::new();
        for (id, vendor) in vendors {
            vendor_map.insert(id, vendor);
        }

        let config = ExecutorConfig::mock_policy(3, 30_000);
        ExecutorService::from_repository(
            ExecutorRepository {
                vendors: vendor_map,
            },
            config,
        )
    }

    #[test]
    fn test_from_config_success() {
        let config = ExecutorConfig {
            openai: Some(OpenAIConfig::for_tests()),
            ..ExecutorConfig::mock_policy(3, 30_000)
        };

        let result = ExecutorService::from_config(config);
        assert!(result.is_ok());

        let service = result.unwrap();
        assert_eq!(service.vendor_count(), 1);
    }

    #[test]
    fn test_from_config_no_vendors() {
        let config = ExecutorConfig::mock_policy(3, 30_000);

        let result = ExecutorService::from_config(config);
        assert!(result.is_err());

        match result {
            Err(ExecutorError::NoVendorsConfigured) => {}
            _ => panic!("Expected NoVendorsConfigured error"),
        }
    }

    #[test]
    fn test_vendor_count() {
        let service = create_test_service();
        assert_eq!(service.vendor_count(), 1);
    }

    #[test]
    fn test_is_retryable_error_network_error() {
        let error = ExecutorError::NetworkError("Connection failed".to_string());
        assert!(ExecutorService::is_retryable_error(&error));
    }

    #[test]
    fn test_is_retryable_error_timeout() {
        let error = ExecutorError::TimeoutError(30000);
        assert!(ExecutorService::is_retryable_error(&error));
    }

    #[test]
    fn test_is_retryable_error_rate_limit() {
        let error = ExecutorError::RateLimitExceeded {
            vendor: "openai".to_string(),
            retry_after_ms: None,
        };
        assert!(ExecutorService::is_retryable_error(&error));
        assert!(!ExecutorService::is_circuit_failure(&error));
    }

    #[test]
    fn test_is_retryable_error_api_call_failed() {
        let error = ExecutorError::ApiCallFailed("500 Internal Server Error".to_string());
        assert!(ExecutorService::is_retryable_error(&error));
    }

    #[test]
    fn test_is_not_retryable_error_authentication() {
        let error = ExecutorError::AuthenticationFailed("openai".to_string());
        assert!(!ExecutorService::is_retryable_error(&error));
    }

    #[test]
    fn test_is_not_retryable_error_invalid_payload() {
        let error = ExecutorError::InvalidPayload("Missing messages".to_string());
        assert!(!ExecutorService::is_retryable_error(&error));
    }

    #[test]
    fn test_is_not_retryable_error_unsupported_vendor() {
        let error = ExecutorError::UnsupportedVendor("unknown".to_string());
        assert!(!ExecutorService::is_retryable_error(&error));
    }

    #[test]
    fn test_is_not_retryable_error_unsupported_model() {
        let error =
            ExecutorError::UnsupportedModel("openai".to_string(), "gpt-5".to_string());
        assert!(!ExecutorService::is_retryable_error(&error));
    }

    #[test]
    fn test_is_not_retryable_error_json() {
        let error = ExecutorError::JsonError("malformed upstream JSON".to_string());
        assert!(!ExecutorService::is_retryable_error(&error));
    }

    #[test]
    fn test_is_not_retryable_error_invalid_upstream_result() {
        let error = ExecutorError::InvalidUpstreamResult;
        assert!(!ExecutorService::is_retryable_error(&error));
    }

    #[test]
    fn test_is_not_retryable_error_missing_pricing() {
        let error = ExecutorError::MissingPricing;
        assert!(!ExecutorService::is_retryable_error(&error));
    }

    #[test]
    fn test_is_not_retryable_error_deadline_exceeded() {
        let error = ExecutorError::DeadlineExceeded;
        assert!(!ExecutorService::is_retryable_error(&error));
    }

    #[test]
    fn test_is_not_retryable_error_quota_exceeded() {
        let error = ExecutorError::QuotaExceeded;
        assert!(!ExecutorService::is_retryable_error(&error));
        assert!(!ExecutorService::is_circuit_failure(&error));
    }

    #[test]
    fn transient_failures_count_for_circuits_permanent_ones_do_not() {
        assert!(ExecutorService::is_circuit_failure(
            &ExecutorError::NetworkError("transient".into())
        ));
        assert!(ExecutorService::is_circuit_failure(
            &ExecutorError::TimeoutError(50)
        ));
        assert!(ExecutorService::is_circuit_failure(
            &ExecutorError::ApiCallFailed("upstream http error".into())
        ));
        assert!(!ExecutorService::is_circuit_failure(
            &ExecutorError::AuthenticationFailed("openai".into())
        ));
        assert!(!ExecutorService::is_circuit_failure(
            &ExecutorError::JsonError("malformed upstream JSON".into())
        ));
        assert!(!ExecutorService::is_circuit_failure(
            &ExecutorError::InvalidUpstreamResult
        ));
        assert!(!ExecutorService::is_circuit_failure(
            &ExecutorError::UpstreamRejected
        ));
        assert!(!ExecutorService::is_circuit_failure(
            &ExecutorError::DeadlineExceeded
        ));
    }

    #[tokio::test]
    async fn test_execute_fallbacks_empty_list() {
        let service = create_test_service();
        let params = ExecutionParams {
            messages: vec![],
            temperature: None,
            max_tokens: None,
            top_p: None,
            stream: false,
        };
        let primary_error = ExecutorError::ApiCallFailed("Primary failed".to_string());

        let mut budget = crate::features::executor::budget::AttemptBudget::new(3);
        let result = service
            .execute_fallbacks(
                &[],
                &params,
                primary_error.clone(),
                generous_deadline(),
                &mut budget,
                "primary",
            )
            .await;

        assert!(result.is_err());
        // Should return the primary error when no fallbacks exist
        match result {
            Err(ExecutorError::ApiCallFailed(msg)) => {
                assert_eq!(msg, "Primary failed");
            }
            _ => panic!("Expected ApiCallFailed error"),
        }
    }

    #[test]
    fn test_extract_params_with_all_fields() {
        let payload = json!({
            "messages": [
                {"role": "system", "content": "You are a helpful assistant."},
                {"role": "user", "content": "Hello!"}
            ],
            "temperature": 0.7,
            "max_tokens": 100,
            "top_p": 0.9,
            "stream": true
        });

        let result = ExecutorService::extract_params(&payload);
        assert!(result.is_ok());

        let params = result.unwrap();
        assert_eq!(params.messages.len(), 2);
        assert_eq!(params.messages[0].role, "system");
        assert_eq!(params.messages[0].content, "You are a helpful assistant.");
        assert_eq!(params.messages[1].role, "user");
        assert_eq!(params.messages[1].content, "Hello!");
        assert_eq!(params.temperature, Some(0.7));
        assert_eq!(params.max_tokens, Some(100));
        assert_eq!(params.top_p, Some(0.9));
        assert!(params.stream);
    }

    #[test]
    fn test_extract_params_with_required_only() {
        let payload = json!({
            "messages": [
                {"role": "user", "content": "Hello!"}
            ]
        });

        let result = ExecutorService::extract_params(&payload);
        assert!(result.is_ok());

        let params = result.unwrap();
        assert_eq!(params.messages.len(), 1);
        assert_eq!(params.messages[0].role, "user");
        assert_eq!(params.messages[0].content, "Hello!");
        assert_eq!(params.temperature, None);
        assert_eq!(params.max_tokens, None);
        assert_eq!(params.top_p, None);
        assert!(!params.stream); // Default value
    }

    #[test]
    fn test_extract_params_missing_messages() {
        let payload = json!({
            "temperature": 0.7,
            "max_tokens": 100
        });

        let result = ExecutorService::extract_params(&payload);
        assert!(result.is_err());

        match result {
            Err(ExecutorError::InvalidPayload(msg)) => {
                assert!(msg.contains("messages"));
            }
            _ => panic!("Expected InvalidPayload error"),
        }
    }

    // ========================================
    // Comprehensive Tests for execute_with_retry()
    // Using Tokio time simulation for instant tests
    // ========================================

    #[tokio::test(start_paused = true)]
    async fn test_execute_with_retry_success_first_attempt() {
        let mock_vendor = MockVendor::new_success("mock", vec!["model-1"]);
        let service =
            create_mock_service(vec![("mock".to_string(), Arc::new(mock_vendor))]);

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

        let result = service
            .execute_with_retry(
                &test_plan("mock", "model-1"),
                &params,
                generous_deadline(),
            )
            .await;

        assert!(result.is_ok());
        let execution_result = result.unwrap();
        assert_eq!(execution_result.model_used, "model-1");
        assert!(execution_result.content.contains("Mock response"));
    }

    #[tokio::test(start_paused = true)]
    async fn test_execute_with_retry_succeeds_after_retries() {
        // Fail 2 times, then succeed (within max_retries=3)
        let mock_vendor = MockVendor::new_fail_then_succeed(
            "mock",
            vec!["model-1"],
            2,
            ExecutorError::NetworkError("Temporary network issue".to_string()),
        );
        let service =
            create_mock_service(vec![("mock".to_string(), Arc::new(mock_vendor))]);

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

        // Spawn task to advance time
        let handle = tokio::spawn(async move {
            service
                .execute_with_retry(
                    &test_plan("mock", "model-1"),
                    &params,
                    generous_deadline(),
                )
                .await
        });

        // Advance time for retries (1s + 2s = 3s total)
        tokio::time::advance(tokio::time::Duration::from_secs(3)).await;

        let result = handle.await.unwrap();
        assert!(result.is_ok());
        let execution_result = result.unwrap();
        assert_eq!(execution_result.model_used, "model-1");
    }

    #[tokio::test(start_paused = true)]
    async fn test_execute_with_retry_max_retries_exceeded() {
        // Always fail (exceeds max_retries=3)
        let mock_vendor = MockVendor::new_always_fail(
            "mock",
            vec!["model-1"],
            ExecutorError::NetworkError("Persistent network issue".to_string()),
        );
        let service =
            create_mock_service(vec![("mock".to_string(), Arc::new(mock_vendor))]);

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

        // Spawn task to advance time
        let handle = tokio::spawn(async move {
            service
                .execute_with_retry(
                    &test_plan("mock", "model-1"),
                    &params,
                    generous_deadline(),
                )
                .await
        });

        // Advance time for all retries (1s + 2s + 4s = 7s total)
        tokio::time::advance(tokio::time::Duration::from_secs(7)).await;

        let result = handle.await.unwrap();
        assert!(result.is_err());

        match result {
            Err(ExecutorError::NetworkError(msg)) => {
                assert_eq!(msg, "Persistent network issue");
            }
            _ => panic!("Expected NetworkError"),
        }
    }

    #[tokio::test(start_paused = true)]
    async fn test_execute_with_retry_non_retryable_error() {
        // Non-retryable error should fail immediately
        let mock_vendor = MockVendor::new_always_fail(
            "mock",
            vec!["model-1"],
            ExecutorError::AuthenticationFailed("Invalid API key".to_string()),
        );
        let service =
            create_mock_service(vec![("mock".to_string(), Arc::new(mock_vendor))]);

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

        let result = service
            .execute_with_retry(
                &test_plan("mock", "model-1"),
                &params,
                generous_deadline(),
            )
            .await;

        assert!(result.is_err());

        match result {
            Err(ExecutorError::AuthenticationFailed(msg)) => {
                assert_eq!(msg, "Invalid API key");
            }
            _ => panic!("Expected AuthenticationFailed error"),
        }
    }

    #[tokio::test]
    async fn protocol_json_error_is_not_retried() {
        let mock_vendor = MockVendor::new_fail_then_succeed(
            "mock",
            vec!["model-1"],
            10,
            ExecutorError::JsonError("malformed upstream JSON".to_string()),
        );
        let remaining = mock_vendor.fail_count.clone().expect("fail counter");
        let before = remaining.load(Ordering::SeqCst);
        let service =
            create_mock_service(vec![("mock".to_string(), Arc::new(mock_vendor))]);
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

        match service
            .execute_with_retry(
                &test_plan("mock", "model-1"),
                &params,
                generous_deadline(),
            )
            .await
        {
            Err(ExecutorError::JsonError(_)) => {}
            other => panic!("expected JsonError, got {other:?}"),
        }
        assert_eq!(before.saturating_sub(remaining.load(Ordering::SeqCst)), 1);
    }

    #[tokio::test]
    async fn protocol_invalid_upstream_result_is_not_retried() {
        let mock_vendor = MockVendor::new_fail_then_succeed(
            "mock",
            vec!["model-1"],
            10,
            ExecutorError::InvalidUpstreamResult,
        );
        let remaining = mock_vendor.fail_count.clone().expect("fail counter");
        let before = remaining.load(Ordering::SeqCst);
        let service =
            create_mock_service(vec![("mock".to_string(), Arc::new(mock_vendor))]);
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

        match service
            .execute_with_retry(
                &test_plan("mock", "model-1"),
                &params,
                generous_deadline(),
            )
            .await
        {
            Err(ExecutorError::InvalidUpstreamResult) => {}
            other => panic!("expected InvalidUpstreamResult, got {other:?}"),
        }
        assert_eq!(before.saturating_sub(remaining.load(Ordering::SeqCst)), 1);
    }

    #[tokio::test]
    async fn test_check_all_vendors_health_handles_panic() {
        // Create a mock vendor that panics during health check
        #[derive(Clone)]
        struct PanicVendor {
            vendor_id: String,
        }

        #[async_trait]
        impl LLMVendor for PanicVendor {
            async fn execute(
                &self,
                _model: &str,
                _target_id: &str,
                _params: ExecutionParams,
            ) -> Result<ExecutionResult, ExecutorError> {
                panic!("Mock panic in execute");
            }

            fn vendor_id(&self) -> &str {
                &self.vendor_id
            }

            fn supports_model(&self, _model: &str) -> bool {
                true
            }

            fn calculate_cost(
                &self,
                _prompt_tokens: i64,
                _completion_tokens: i64,
                _target_id: &str,
            ) -> Result<f64, ExecutorError> {
                Ok(0.0)
            }

            async fn health_check(
                &self,
                _timeout_secs: u64,
            ) -> Result<(), ExecutorError> {
                panic!("Mock panic in health check");
            }
        }

        // Create repository with panic vendor and one healthy vendor
        let mut vendors: HashMap<String, Arc<dyn LLMVendor>> = HashMap::new();
        vendors.insert(
            "panic-vendor".to_string(),
            Arc::new(PanicVendor {
                vendor_id: "panic-vendor".to_string(),
            }),
        );
        vendors.insert(
            "healthy-vendor".to_string(),
            Arc::new(MockVendor::new_success("healthy-vendor", vec!["model-1"])),
        );

        let repository = ExecutorRepository { vendors };
        let service = ExecutorService::from_repository(
            repository,
            ExecutorConfig::mock_policy(3, 30_000),
        );

        // Call check_all_vendors_health
        // Should succeed because at least one vendor (healthy-vendor) is healthy
        // The panic vendor should be logged as an error but not cause the whole check to fail
        let result = service.check_all_vendors_health(2).await;

        // Should succeed because we have at least one healthy vendor
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_check_all_vendors_health_all_panic() {
        // Create a mock vendor that panics during health check
        #[derive(Clone)]
        struct PanicVendor {
            vendor_id: String,
        }

        #[async_trait]
        impl LLMVendor for PanicVendor {
            async fn execute(
                &self,
                _model: &str,
                _target_id: &str,
                _params: ExecutionParams,
            ) -> Result<ExecutionResult, ExecutorError> {
                panic!("Mock panic in execute");
            }

            fn vendor_id(&self) -> &str {
                &self.vendor_id
            }

            fn supports_model(&self, _model: &str) -> bool {
                true
            }

            fn calculate_cost(
                &self,
                _prompt_tokens: i64,
                _completion_tokens: i64,
                _target_id: &str,
            ) -> Result<f64, ExecutorError> {
                Ok(0.0)
            }

            async fn health_check(
                &self,
                _timeout_secs: u64,
            ) -> Result<(), ExecutorError> {
                panic!("Mock panic in health check");
            }
        }

        // Create repository with only panic vendors
        let mut vendors: HashMap<String, Arc<dyn LLMVendor>> = HashMap::new();
        vendors.insert(
            "panic-vendor-1".to_string(),
            Arc::new(PanicVendor {
                vendor_id: "panic-vendor-1".to_string(),
            }),
        );
        vendors.insert(
            "panic-vendor-2".to_string(),
            Arc::new(PanicVendor {
                vendor_id: "panic-vendor-2".to_string(),
            }),
        );

        let repository = ExecutorRepository { vendors };
        let service = ExecutorService::from_repository(
            repository,
            ExecutorConfig::mock_policy(3, 30_000),
        );

        // Call check_all_vendors_health
        // Should fail because all vendors panic
        let result = service.check_all_vendors_health(2).await;

        // Should fail with NetworkError (from the JoinError handling)
        assert!(result.is_err());
        match result {
            Err(ExecutorError::NetworkError(msg)) => {
                assert!(msg.contains("Health check task failed"));
            }
            _ => panic!("Expected NetworkError from panic handling"),
        }
    }

    #[tokio::test]
    async fn test_circuit_breaker_opens_after_consecutive_primary_failures() {
        let failing_vendor = Arc::new(MockVendor::new_always_fail(
            "openai",
            vec!["gpt-4"],
            ExecutorError::NetworkError("simulated network failure".to_string()),
        ));
        let mut service =
            create_mock_service(vec![("openai".to_string(), failing_vendor)]);
        service.config.max_retries = 0;
        service.circuits =
            TargetCircuitRegistry::with_system_clock(2, Duration::from_secs(60));

        let payload = json!({
            "messages": [
                {"role": "user", "content": "hello"}
            ]
        });
        let plan = RoutePlan {
            vendor_id: "openai".to_string(),
            target_id: "gpt-4".to_string(),
            model_id: "gpt-4".to_string(),
            max_output_tokens: 4_096,
            fallback_plans: vec![],
        };

        let first = service.execute(&plan, &payload, generous_deadline()).await;
        assert!(matches!(first, Err(ExecutorError::NetworkError(_))));

        let second = service.execute(&plan, &payload, generous_deadline()).await;
        assert!(matches!(second, Err(ExecutorError::NetworkError(_))));

        let third = service.execute(&plan, &payload, generous_deadline()).await;
        match third {
            Err(ExecutorError::CircuitOpen {
                vendor,
                retry_after_ms,
            }) => {
                assert_eq!(vendor, "openai");
                assert!(retry_after_ms > 0);
            }
            _ => panic!("Expected CircuitOpen after threshold reached"),
        }
    }

    #[tokio::test]
    async fn test_circuit_open_primary_degrades_to_fallback() {
        let failing_primary = Arc::new(MockVendor::new_always_fail(
            "openai",
            vec!["gpt-4"],
            ExecutorError::NetworkError("simulated network failure".to_string()),
        ));
        let healthy_fallback =
            Arc::new(MockVendor::new_success("backup", vec!["gpt-4-turbo"]));

        let mut service = create_mock_service(vec![
            ("openai".to_string(), failing_primary),
            ("backup".to_string(), healthy_fallback),
        ]);
        service.config.max_retries = 0;
        service.circuits =
            TargetCircuitRegistry::with_system_clock(1, Duration::from_secs(60));

        let payload = json!({
            "messages": [
                {"role": "user", "content": "hello"}
            ]
        });
        let plan = RoutePlan {
            vendor_id: "openai".to_string(),
            target_id: "gpt-4".to_string(),
            model_id: "gpt-4".to_string(),
            max_output_tokens: 4_096,
            fallback_plans: vec![RoutePlan {
                vendor_id: "backup".to_string(),
                target_id: "gpt-4-turbo".to_string(),
                model_id: "gpt-4-turbo".to_string(),
                max_output_tokens: 4_096,
                fallback_plans: vec![],
            }],
        };

        let first = service.execute(&plan, &payload, generous_deadline()).await;
        assert!(first.is_ok());

        let second = service.execute(&plan, &payload, generous_deadline()).await;
        assert!(second.is_ok());
    }

    #[tokio::test]
    async fn test_non_retryable_failures_do_not_open_circuit() {
        let auth_failing_vendor = Arc::new(MockVendor::new_always_fail(
            "openai",
            vec!["gpt-4"],
            ExecutorError::AuthenticationFailed("openai".to_string()),
        ));
        let mut service =
            create_mock_service(vec![("openai".to_string(), auth_failing_vendor)]);
        service.config.max_retries = 0;
        service.circuits =
            TargetCircuitRegistry::with_system_clock(1, Duration::from_secs(60));

        let payload = json!({
            "messages": [
                {"role": "user", "content": "hello"}
            ]
        });
        let plan = RoutePlan {
            vendor_id: "openai".to_string(),
            target_id: "gpt-4".to_string(),
            model_id: "gpt-4".to_string(),
            max_output_tokens: 4_096,
            fallback_plans: vec![],
        };

        let first = service.execute(&plan, &payload, generous_deadline()).await;
        assert!(matches!(first, Err(ExecutorError::AuthenticationFailed(_))));

        let second = service.execute(&plan, &payload, generous_deadline()).await;
        assert!(matches!(
            second,
            Err(ExecutorError::AuthenticationFailed(_))
        ));
    }
}

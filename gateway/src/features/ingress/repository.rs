// Repository Layer - executor execution boundary

use super::error::IngressError;
use crate::core::contracts::RoutePlan;
use crate::core::deadline::RequestDeadline;
use crate::features::executor::{models::ExecutionResult, service::ExecutorService};
use std::sync::Arc;

/// Repository for external LLM execution.
///
/// The executor is the only external system contacted from ingress. Route
/// selection stays in the service layer.
pub struct IngressRepository {
    /// Executor service for LLM execution with retry and fallback logic
    executor_service: Arc<ExecutorService>,
}

impl IngressRepository {
    /// Creates a new repository instance with ExecutorService dependency.
    ///
    /// # Parameters
    /// - `executor_service` - Shared ExecutorService instance for LLM execution
    pub fn new(executor_service: Arc<ExecutorService>) -> Self {
        Self { executor_service }
    }

    /// Executes an LLM call for a configured route plan.
    ///
    /// This is a CHILD SPAN. It automatically inherits `request_id` from parent.
    ///
    /// Delegates to ExecutorService which handles retry logic, fallback
    /// execution, and vendor-specific API calls.
    ///
    /// # Parameters
    /// - `plan` - Flat routing plan selected from the operator catalog
    /// - `payload` - Request payload containing only the original user prompt
    /// - `deadline` - Shared protected-request deadline
    ///
    /// # Returns
    /// ExecutionResult with response content, token counts, and cost metrics
    ///
    /// # Errors
    /// Returns ExecutionFailed if LLM execution fails
    #[tracing::instrument(
        skip(self, payload, deadline),
        fields(
            vendor_id = %plan.vendor_id,
            target_id = %plan.target_id,
            model_id = %plan.model_id,
        )
    )]
    pub async fn execute_llm_call(
        &self,
        plan: &RoutePlan,
        payload: &serde_json::Value,
        deadline: RequestDeadline,
    ) -> Result<ExecutionResult, IngressError> {
        tracing::debug!("Executing LLM call via ExecutorService");
        let result = self
            .executor_service
            .execute(plan, payload, deadline)
            .await
            .map_err(IngressError::from)?;

        tracing::debug!(
            prompt_tokens = result.prompt_tokens,
            completion_tokens = result.completion_tokens,
            total_cost = result.total_cost,
            "LLM call completed successfully"
        );

        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::contracts::RoutePlan;
    use crate::core::deadline::RequestDeadline;
    use crate::features::executor::{
        config::ExecutorConfig,
        error::ExecutorError,
        models::{ExecutionParams, ExecutionResult},
        repository::ExecutorRepository,
        service::ExecutorService,
        vendors::LLMVendor,
    };
    use async_trait::async_trait;
    use serde_json::json;
    use std::collections::HashMap;
    use std::sync::Arc;
    use std::time::Duration;
    use tokio::sync::RwLock;

    #[derive(Clone)]
    struct MockVendor {
        vendor_id: String,
    }

    #[async_trait]
    impl LLMVendor for MockVendor {
        async fn execute(
            &self,
            model: &str,
            _target_id: &str,
            params: ExecutionParams,
        ) -> Result<ExecutionResult, ExecutorError> {
            Ok(ExecutionResult {
                content: params
                    .messages
                    .first()
                    .map(|message| message.content.clone())
                    .unwrap_or_default(),
                role: "assistant".to_string(),
                model_used: model.to_string(),
                prompt_tokens: 1,
                completion_tokens: 1,
                total_cost: 0.0,
                finish_reason: "stop".to_string(),
            })
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

        async fn health_check(&self, _timeout_secs: u64) -> Result<(), ExecutorError> {
            Ok(())
        }
    }

    fn create_repository() -> IngressRepository {
        let mut vendors: HashMap<String, Arc<dyn LLMVendor>> = HashMap::new();
        vendors.insert(
            "openai".to_string(),
            Arc::new(MockVendor {
                vendor_id: "openai".to_string(),
            }),
        );

        let executor = Arc::new(ExecutorService {
            repository: Arc::new(ExecutorRepository { vendors }),
            config: ExecutorConfig {
                openai: None,
                anthropic_api_key: None,
                timeout_ms: 30000,
                max_retries: 0,
            },
            circuit_breakers: Arc::new(RwLock::new(HashMap::new())),
            circuit_breaker_failure_threshold: 3,
            circuit_breaker_open_duration: Duration::from_secs(30),
        });

        IngressRepository::new(executor)
    }

    #[tokio::test]
    async fn execute_llm_call_forwards_original_prompt_on_configured_plan() {
        let repository = create_repository();
        let plan = RoutePlan {
            vendor_id: "openai".to_string(),
            target_id: "primary".to_string(),
            model_id: "example-chat-model".to_string(),
            fallback_plans: Vec::new(),
        };
        let payload = json!({
            "messages": [{ "role": "user", "content": "only this prompt" }]
        });

        let result = repository
            .execute_llm_call(
                &plan,
                &payload,
                RequestDeadline::from_timeout(Duration::from_secs(60)),
            )
            .await
            .expect("executor call should succeed");

        assert_eq!(result.content, "only this prompt");
        assert_eq!(result.model_used, "example-chat-model");
    }
}

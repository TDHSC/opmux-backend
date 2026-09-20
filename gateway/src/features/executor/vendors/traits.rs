//! Common traits for LLM vendor implementations.

use crate::features::executor::{
    error::ExecutorError, models::ExecutionParams, models::ExecutionResult,
};
use async_trait::async_trait;

/// Trait for LLM vendor implementations.
///
/// Each vendor (OpenAI, Anthropic, etc.) implements this trait to provide
/// a unified interface for LLM execution.
#[async_trait]
pub trait LLMVendor: Send + Sync {
    /// Executes LLM API call with given parameters.
    ///
    /// # Parameters
    /// - `model` - Model identifier (e.g., "gpt-4", "claude-3-opus")
    /// - `params` - Execution parameters (messages, temperature, etc.)
    ///
    /// # Returns
    /// Execution result with AI response and metrics
    async fn execute(
        &self,
        model: &str,
        params: ExecutionParams,
    ) -> Result<ExecutionResult, ExecutorError>;

    /// Returns the vendor identifier.
    fn vendor_id(&self) -> &str;

    /// Checks if the vendor supports the given model.
    fn supports_model(&self, model: &str) -> bool;

    /// Estimates successful-response cost from configured target prices.
    ///
    /// Look up prices by the selected target model that was requested, not by
    /// the provider-reported model string.
    ///
    /// # Parameters
    /// - `prompt_tokens` - Number of tokens in the prompt
    /// - `completion_tokens` - Number of tokens in the completion
    /// - `model` - Selected target model identifier
    ///
    /// # Returns
    /// Nonnegative USD estimate for the successful response
    ///
    /// # Errors
    /// Returns an error when configured prices are missing rather than zero.
    fn calculate_cost(
        &self,
        prompt_tokens: i64,
        completion_tokens: i64,
        model: &str,
    ) -> Result<f64, ExecutorError>;

    /// Performs a health check to verify vendor connectivity and credentials.
    ///
    /// This method makes a lightweight API call to verify:
    /// - API credentials are valid
    /// - Upstream service is reachable
    /// - Network connectivity is working
    ///
    /// # Parameters
    /// - `timeout_secs` - Timeout in seconds for the health check request
    ///
    /// # Implementation Guidelines
    /// - Use a lightweight endpoint (e.g., GET /models for OpenAI)
    /// - Respect the timeout parameter
    /// - Don't retry on failure (health checks should be fast)
    ///
    /// # Returns
    /// - `Ok(())` if the vendor is healthy and accessible
    /// - `Err(ExecutorError)` if the vendor is unhealthy, unreachable, or credentials are invalid
    ///
    /// # Errors
    /// - `ExecutorError::AuthenticationFailed` - Invalid API key
    /// - `ExecutorError::TimeoutError` - Request timed out
    /// - `ExecutorError::NetworkError` - Network connectivity issues
    /// - `ExecutorError::ApiCallFailed` - Other API errors
    async fn health_check(&self, timeout_secs: u64) -> Result<(), ExecutorError>;
}

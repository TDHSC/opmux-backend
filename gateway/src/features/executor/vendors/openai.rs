//! OpenAI Chat Completions adapter.
//!
//! Sends one `POST {base_url}/chat/completions` request per call. `model_used`
//! is the provider-reported model. Cost uses the selected target's configured
//! prices, not a hardcoded model-price table. Success bodies are accumulated
//! up to `max_response_bytes` before deserialization.

use crate::features::executor::{
    bounded_body::read_bounded_response_body,
    config::OpenAIConfig,
    error::ExecutorError,
    models::{ExecutionParams, ExecutionResult, Message},
    openai_response::parse_successful_chat_completion,
    pricing::estimate_successful_response_cost,
    vendors::traits::LLMVendor,
};
use async_trait::async_trait;
use reqwest::{header, Client, StatusCode};
use serde::{Deserialize, Serialize};
use std::time::Duration;

/// OpenAI Chat Completions API request.
#[derive(Debug, Deserialize, Serialize)]
struct ChatCompletionRequest {
    model: String,
    messages: Vec<Message>,
    #[serde(skip_serializing_if = "Option::is_none")]
    temperature: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    max_tokens: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    top_p: Option<f64>,
}

/// OpenAI vendor implementation.
pub struct OpenAIVendor {
    config: OpenAIConfig,
    client: Client,
}

impl OpenAIVendor {
    /// Creates a new OpenAI vendor instance from configuration.
    ///
    /// # Parameters
    /// - `config` - OpenAI configuration with API key, base URL, and pricing
    ///
    /// # Errors
    /// Returns `InvalidConfiguration` when the credential is blank or the
    /// bounded HTTP client cannot be constructed. Never falls back to an
    /// unbounded default client.
    pub fn new(config: OpenAIConfig) -> Result<Self, ExecutorError> {
        if config.api_key.trim().is_empty() {
            return Err(ExecutorError::InvalidConfiguration);
        }
        let timeout = Duration::from_millis(config.timeout_ms);
        let client = crate::core::config::build_bounded_http_client_for_base_url(
            timeout,
            &config.base_url,
        )
        .map_err(|_| ExecutorError::InvalidConfiguration)?;
        Ok(Self { config, client })
    }
}

#[async_trait]
impl LLMVendor for OpenAIVendor {
    fn vendor_id(&self) -> &str {
        "openai"
    }

    fn supports_model(&self, model: &str) -> bool {
        // Business logic: check if model is in the supported list
        self.config.supported_models.contains(&model.to_string())
    }

    fn calculate_cost(
        &self,
        prompt_tokens: i64,
        completion_tokens: i64,
        model: &str,
    ) -> Result<f64, ExecutorError> {
        estimate_successful_response_cost(
            prompt_tokens,
            completion_tokens,
            self.config.pricing.get(model),
        )
    }

    async fn execute(
        &self,
        model: &str,
        params: ExecutionParams,
    ) -> Result<ExecutionResult, ExecutorError> {
        if !self.supports_model(model) {
            return Err(ExecutorError::UnsupportedModel(
                model.to_string(),
                self.vendor_id().to_string(),
            ));
        }
        if params.stream {
            return Err(ExecutorError::InvalidPayload(
                "Streaming is not supported for OpenAI requests".to_string(),
            ));
        }

        let request = ChatCompletionRequest {
            messages: params.messages,
            temperature: params.temperature,
            max_tokens: params.max_tokens,
            top_p: params.top_p,
            model: model.to_string(),
        };

        let response = self
            .client
            .post(format!("{}/chat/completions", self.config.base_url))
            .header("Authorization", format!("Bearer {}", self.config.api_key))
            .header("Content-Type", "application/json")
            .json(&request)
            .send()
            .await?;

        if !response.status().is_success() {
            let status = response.status();
            let retry_after_ms = if status == StatusCode::TOO_MANY_REQUESTS {
                response
                    .headers()
                    .get(header::RETRY_AFTER)
                    .and_then(|value| value.to_str().ok())
                    .and_then(|value| value.parse::<u64>().ok())
                    .map(|seconds| seconds.saturating_mul(1000))
            } else {
                None
            };
            let _ = read_bounded_response_body(response, self.config.max_response_bytes)
                .await;
            return Err(match status {
                StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN => {
                    ExecutorError::AuthenticationFailed("openai".to_string())
                }
                StatusCode::TOO_MANY_REQUESTS => ExecutorError::RateLimitExceeded {
                    vendor: "openai".to_string(),
                    retry_after_ms,
                },
                status if status.is_client_error() => {
                    ExecutorError::InvalidPayload(format!("OpenAI API error {status}"))
                }
                status if status.is_server_error() => {
                    ExecutorError::ApiCallFailed(format!("OpenAI API error {status}"))
                }
                _ => ExecutorError::ApiCallFailed(format!("OpenAI API error {status}")),
            });
        }

        let body =
            read_bounded_response_body(response, self.config.max_response_bytes).await?;
        parse_successful_chat_completion(&body, self.config.pricing.get(model))
    }

    async fn health_check(&self, timeout_secs: u64) -> Result<(), ExecutorError> {
        // Use GET /models endpoint for health check
        // This is a lightweight endpoint that:
        // - Requires valid API key (verifies authentication)
        // - Returns quickly (typically < 500ms)
        // - Doesn't consume tokens
        let url = format!("{}/models", self.config.base_url);

        // Reuse self.client for connection pooling and TLS session reuse
        // Use tokio::time::timeout for per-request timeout control
        let request_future = self
            .client
            .get(&url)
            .header("Authorization", format!("Bearer {}", self.config.api_key))
            .send();

        // Apply per-request timeout using tokio::time::timeout
        let response = tokio::time::timeout(
            std::time::Duration::from_secs(timeout_secs),
            request_future,
        )
        .await
        .map_err(|_| ExecutorError::TimeoutError(timeout_secs * 1000))? // Timeout elapsed
        .map_err(|e| {
            // Request failed (not timeout)
            if e.is_connect() {
                ExecutorError::NetworkError(format!(
                    "Failed to connect to OpenAI API: {}",
                    e
                ))
            } else {
                ExecutorError::NetworkError(e.to_string())
            }
        })?;

        // Check response status
        match response.status() {
            reqwest::StatusCode::OK => {
                tracing::debug!("OpenAI health check passed");
                Ok(())
            }
            reqwest::StatusCode::UNAUTHORIZED | reqwest::StatusCode::FORBIDDEN => {
                Err(ExecutorError::AuthenticationFailed("openai".to_string()))
            }
            reqwest::StatusCode::TOO_MANY_REQUESTS => {
                Err(ExecutorError::RateLimitExceeded {
                    vendor: "openai".to_string(),
                    retry_after_ms: None,
                })
            }
            status => Err(ExecutorError::ApiCallFailed(format!(
                "Health check failed with status: {}",
                status
            ))),
        }
    }
}

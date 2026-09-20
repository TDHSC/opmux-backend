// Service Layer - Business logic and orchestration

use super::{
    constants::SLOW_REQUEST_THRESHOLD_MS, error::IngressError,
    repository::IngressRepository, routing::resolve_route,
};
use crate::core::config::Settings;
use crate::features::executor::service::ExecutorService;
use serde::Serialize;
use serde_json::{json, Number, Value};
use std::sync::Arc;

/// Typed generation options accepted by ingress.
///
/// Omitted `temperature` and `top_p` are left off the provider JSON so the
/// provider default applies. Omitted `max_tokens` uses the selected primary
/// target's `max_output_tokens`.
#[derive(Clone, Debug, Default)]
pub struct GenerationParameters {
    /// Sampling temperature in `[0.0, 2.0]`.
    pub temperature: Option<Number>,
    /// Nucleus sampling in `[0.0, 1.0]`.
    pub top_p: Option<Number>,
    /// Positive integer token cap, at most the selected target's output cap.
    pub max_tokens: Option<u32>,
}

/// Incoming AI routing request from clients.
pub struct IngressRequest {
    /// Original user prompt. Forwarded unchanged after validation.
    pub prompt: String,
    /// Bounded opaque metadata. Never forwarded, logged, or persisted.
    pub metadata: Value,
    /// Optional configured route name. Omitted selects the catalog default.
    pub route: Option<String>,
    /// Optional fallback opt-out. Omitted or `true` follows the configured
    /// chain; `false` limits execution to the primary without disabling retries.
    pub allow_fallback: Option<bool>,
    /// Optional typed generation controls.
    pub parameters: GenerationParameters,
}

/// AI assistant response structure.
#[derive(Serialize)]
pub struct AIResponse {
    /// AI-generated response content.
    pub content: String,
    /// Response role. Successful Chat Completions results are exactly `assistant`.
    pub role: String,
    /// Reason for response completion ("stop", "length", etc.).
    pub finish_reason: Option<String>,
}

/// Validated prompt and completion usage from the successful provider response.
#[derive(Serialize)]
pub struct TokenUsage {
    /// Prompt / input token count reported by the provider.
    pub prompt_tokens: i64,
    /// Completion / output token count reported by the provider.
    pub completion_tokens: i64,
}

/// Complete ingress response with AI content and metadata.
#[derive(Serialize)]
pub struct IngressResponse {
    /// AI assistant response.
    pub response: AIResponse,
    /// Provider-reported model. This may differ from the selected target alias.
    pub model_used: String,
    /// Estimated USD cost of the successful response from configured target prices.
    ///
    /// This is not current provider billing and does not total retries or
    /// abandoned work.
    pub cost: f64,
    /// Total processing time in milliseconds.
    pub processing_time_ms: u64,
    /// Validated usage copied from the successful provider response.
    pub usage: TokenUsage,
}

/// Service for stateless ingress routing and execution.
///
/// Selects an operator-configured route and forwards only the original user
/// prompt to the executor boundary.
pub struct IngressService {
    repository: IngressRepository,
    settings: Arc<Settings>,
    slow_request_threshold_ms: u64,
}

impl IngressService {
    /// Creates a new ingress service with catalog settings and executor access.
    ///
    /// # Parameters
    /// - `executor_service` - Shared ExecutorService instance for LLM execution
    /// - `settings` - Injected operator catalog and limits
    pub fn new(executor_service: Arc<ExecutorService>, settings: Arc<Settings>) -> Self {
        let slow_request_threshold_ms =
            std::env::var("INGRESS_SLOW_REQUEST_THRESHOLD_MS")
                .ok()
                .and_then(|v| v.parse::<u64>().ok())
                .unwrap_or(SLOW_REQUEST_THRESHOLD_MS);

        Self {
            repository: IngressRepository::new(executor_service),
            settings,
            slow_request_threshold_ms,
        }
    }

    /// Processes a stateless AI routing request.
    ///
    /// This is a CHILD SPAN. It automatically inherits `request_id` and
    /// `client_correlation_id` from the parent Handler span.
    ///
    /// # Flow
    /// 1. Selects the configured default or named route
    /// 2. Builds a flat target plan, honoring fallback opt-out
    /// 3. Rejects `max_tokens` above the selected primary cap
    /// 4. Forwards the original prompt and accepted generation options
    /// 5. Returns the execution result
    ///
    /// # Parameters
    /// - `request` - AI routing request with prompt, opaque metadata, and optional route controls
    ///
    /// # Returns
    /// Complete AI response with metadata (cost, model, processing time)
    ///
    /// # Errors
    /// Returns `InvalidRequest` for an unknown route or a token cap the selected
    /// primary cannot satisfy, before provider access.
    #[tracing::instrument(
        skip(self, request),
        fields(prompt_length = request.prompt.len())
    )]
    pub async fn process_request(
        &self,
        request: IngressRequest,
    ) -> Result<IngressResponse, IngressError> {
        tracing::debug!("Starting request processing");
        let start_time = std::time::Instant::now();

        let resolved = resolve_route(
            &self.settings.catalog,
            request.route.as_deref(),
            request.allow_fallback,
        )?;
        tracing::debug!(
            route_id = %resolved.route_id,
            target_id = %resolved.target_id,
            model_id = %resolved.plan.model_id,
            fallback_count = resolved.plan.fallback_plans.len(),
            "Selected configured route"
        );

        if let Some(max_tokens) = request.parameters.max_tokens {
            if max_tokens > resolved.max_output_tokens {
                return Err(IngressError::InvalidRequest(
                    "max_tokens exceeds the selected target's max_output_tokens"
                        .to_string(),
                ));
            }
        }

        let max_tokens = request
            .parameters
            .max_tokens
            .unwrap_or(resolved.max_output_tokens);
        let mut payload = json!({
            "messages": [
                {
                    "role": "user",
                    "content": request.prompt,
                }
            ],
            "max_tokens": max_tokens,
        });
        if let Some(temperature) = request.parameters.temperature {
            payload["temperature"] = Value::Number(temperature);
        }
        if let Some(top_p) = request.parameters.top_p {
            payload["top_p"] = Value::Number(top_p);
        }

        tracing::debug!("Executing LLM call via ExecutorService");
        let llm_result = self
            .repository
            .execute_llm_call(&resolved.plan, &payload)
            .await?;
        tracing::debug!(
            tokens = llm_result.prompt_tokens + llm_result.completion_tokens,
            cost = llm_result.total_cost,
            "LLM execution completed"
        );

        let processing_time_ms = start_time.elapsed().as_millis() as u64;
        if processing_time_ms >= self.slow_request_threshold_ms {
            tracing::warn!(
                processing_time_ms = processing_time_ms,
                threshold_ms = self.slow_request_threshold_ms,
                "Slow ingress request detected"
            );
        }
        tracing::debug!(
            processing_time_ms = processing_time_ms,
            "Request processing completed"
        );

        Ok(IngressResponse {
            response: AIResponse {
                content: llm_result.content,
                role: llm_result.role,
                finish_reason: Some(llm_result.finish_reason),
            },
            model_used: llm_result.model_used,
            cost: llm_result.total_cost,
            processing_time_ms,
            usage: TokenUsage {
                prompt_tokens: llm_result.prompt_tokens,
                completion_tokens: llm_result.completion_tokens,
            },
        })
    }
}

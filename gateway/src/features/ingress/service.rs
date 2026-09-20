// Service Layer - Business logic and orchestration

use super::{
    constants::{AI_RESPONSE_ROLE, SLOW_REQUEST_THRESHOLD_MS},
    error::IngressError,
    repository::IngressRepository,
    routing::resolve_route,
};
use crate::core::config::Settings;
use crate::features::executor::service::ExecutorService;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

/// Incoming AI routing request from clients.
#[derive(Deserialize)]
pub struct IngressRequest {
    /// User's prompt/message for AI processing.
    pub prompt: String,
    /// Bounded opaque metadata. Never forwarded, logged, or persisted.
    pub metadata: serde_json::Value,
    /// Optional configured route name. Omitted selects the catalog default.
    #[serde(default)]
    pub route: Option<String>,
    /// Optional fallback opt-out. Omitted or `true` follows the configured
    /// chain; `false` limits execution to the primary without disabling retries.
    #[serde(default)]
    pub allow_fallback: Option<bool>,
}

/// AI assistant response structure.
#[derive(Serialize)]
pub struct AIResponse {
    /// AI-generated response content.
    pub content: String,
    /// Response role (always "assistant").
    pub role: String,
    /// Reason for response completion ("stop", "length", etc.).
    pub finish_reason: Option<String>,
}

/// Complete ingress response with AI content and metadata.
#[derive(Serialize)]
pub struct IngressResponse {
    /// AI assistant response.
    pub response: AIResponse,
    /// AI model used for generation.
    pub model_used: String,
    /// Request cost in USD.
    pub cost: f64,
    /// Total processing time in milliseconds.
    pub processing_time_ms: u64,
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
    /// 3. Sends only the original user prompt to the executor
    /// 4. Returns the execution result
    ///
    /// # Parameters
    /// - `request` - AI routing request with prompt, opaque metadata, and optional route controls
    ///
    /// # Returns
    /// Complete AI response with metadata (cost, model, processing time)
    ///
    /// # Errors
    /// Returns `InvalidRequest` for an unknown route before provider access.
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

        let payload = serde_json::json!({
            "messages": [
                {
                    "role": "user",
                    "content": request.prompt,
                }
            ],
        });

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
                role: AI_RESPONSE_ROLE.to_string(),
                finish_reason: Some(llm_result.finish_reason),
            },
            model_used: llm_result.model_used,
            cost: llm_result.total_cost,
            processing_time_ms,
        })
    }
}

//! Data models for Executor Layer.

use crate::core::contracts::RoutePlan;
use serde::{Deserialize, Serialize};

/// Complete execution request containing routing plan and payload.
#[derive(Debug, Clone)]
pub struct ExecutionRequest {
    /// Routing plan from Router Service
    pub plan: RoutePlan,
    /// Original request payload from client
    pub payload: serde_json::Value,
}

/// Result of LLM execution with response content and metrics.
#[derive(Debug, Clone)]
pub struct ExecutionResult {
    /// Generated AI response content
    pub content: String,
    /// Provider-reported message role. Successful Chat Completions results are exactly `assistant`.
    pub role: String,
    /// Provider-reported model identifier, which may differ from the requested alias
    pub model_used: String,
    /// Number of tokens in the prompt
    pub prompt_tokens: i64,
    /// Number of tokens in the completion
    pub completion_tokens: i64,
    /// Estimated successful-response cost in USD from configured target prices
    pub total_cost: f64,
    /// Reason for completion ("stop", "length", "content_filter", etc.)
    pub finish_reason: String,
}

/// Execution parameters extracted from request payload.
#[derive(Debug, Clone)]
pub struct ExecutionParams {
    /// Conversation messages
    pub messages: Vec<Message>,
    /// Sampling temperature (0.0 to 2.0)
    pub temperature: Option<f64>,
    /// Maximum tokens to generate
    pub max_tokens: Option<i64>,
    /// Nucleus sampling parameter
    pub top_p: Option<f64>,
    /// Whether to stream the response
    pub stream: bool,
}

/// Chat message in conversation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    /// Message role ("system", "user", "assistant")
    pub role: String,
    /// Message content
    pub content: String,
}

/// Vendor configuration for API access.
#[derive(Debug, Clone)]
pub struct VendorConfig {
    /// Vendor identifier (e.g., "openai", "anthropic")
    pub vendor_id: String,
    /// API key for authentication
    pub api_key: String,
    /// Base URL for API endpoint
    pub base_url: String,
    /// Request timeout in milliseconds
    pub timeout_ms: u64,
    /// Maximum number of retries
    pub max_retries: u32,
}

impl Default for VendorConfig {
    fn default() -> Self {
        Self {
            vendor_id: String::new(),
            api_key: String::new(),
            base_url: String::new(),
            timeout_ms: 30000, // 30 seconds
            max_retries: 3,
        }
    }
}

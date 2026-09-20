//! Error types for Executor Layer.

use axum::{
    http::StatusCode,
    response::{IntoResponse, Json, Response},
};
use serde_json::json;

/// Errors specific to Executor Layer operations.
///
/// Each variant represents a specific failure scenario during LLM execution,
/// providing clear context for debugging and monitoring.
#[derive(Debug, Clone, thiserror::Error)]
pub enum ExecutorError {
    /// Vendor is not supported or not configured.
    #[error("Unsupported vendor: {0}")]
    UnsupportedVendor(String),

    /// Model is not supported by the vendor.
    #[error("Unsupported model '{0}' for vendor '{1}'")]
    UnsupportedModel(String, String),

    /// Request payload format is invalid.
    #[error("Invalid payload format: {0}")]
    InvalidPayload(String),

    /// LLM API call failed.
    #[error("API call failed: {0}")]
    ApiCallFailed(String),

    /// Rate limit exceeded by vendor API.
    #[error("Rate limit exceeded for vendor '{vendor}'")]
    RateLimitExceeded {
        /// Vendor identifier (e.g., "openai")
        vendor: String,
        /// Optional retry-after hint in milliseconds
        retry_after_ms: Option<u64>,
    },

    /// Authentication failed (invalid API key).
    #[error("Authentication failed for vendor '{0}'")]
    AuthenticationFailed(String),

    /// Request timeout.
    #[error("Request timeout after {0}ms")]
    TimeoutError(u64),

    /// Network error during API call.
    #[error("Network error: {0}")]
    NetworkError(String),

    /// JSON parsing error.
    #[error("JSON parsing error: {0}")]
    JsonError(String),

    /// No vendors configured in ExecutorConfig.
    #[error("No LLM vendors configured")]
    NoVendorsConfigured,

    /// Vendor or HTTP client configuration is invalid.
    #[error("Invalid executor configuration")]
    InvalidConfiguration,

    /// Configured pricing is missing for the selected target.
    #[error("Missing pricing for the selected target")]
    MissingPricing,

    /// Provider success payload cannot be used as a valid result.
    #[error("Invalid upstream result")]
    InvalidUpstreamResult,

    #[error("Circuit breaker open for vendor '{vendor}'")]
    CircuitOpen { vendor: String, retry_after_ms: u64 },
}

impl IntoResponse for ExecutorError {
    /// Converts errors into HTTP JSON responses with appropriate status codes.
    fn into_response(self) -> Response {
        let (status, error_code, message) = match &self {
            // Client errors (4xx)
            Self::UnsupportedVendor(vendor) => (
                StatusCode::BAD_REQUEST,
                "unsupported_vendor",
                format!("Vendor '{}' is not supported", vendor),
            ),
            Self::UnsupportedModel(model, vendor) => (
                StatusCode::BAD_REQUEST,
                "unsupported_model",
                format!("Model '{}' is not supported by vendor '{}'", model, vendor),
            ),
            Self::InvalidPayload(msg) => (
                StatusCode::BAD_REQUEST,
                "invalid_payload",
                format!("Invalid request payload: {}", msg),
            ),
            Self::AuthenticationFailed(vendor) => (
                StatusCode::UNAUTHORIZED,
                "authentication_failed",
                format!("Authentication failed for vendor '{}'", vendor),
            ),
            Self::RateLimitExceeded { vendor, .. } => (
                StatusCode::TOO_MANY_REQUESTS,
                "rate_limit_exceeded",
                format!("Rate limit exceeded for vendor '{}'", vendor),
            ),
            Self::CircuitOpen {
                vendor,
                retry_after_ms,
            } => (
                StatusCode::SERVICE_UNAVAILABLE,
                "circuit_open",
                format!(
                    "Vendor '{}' temporarily unavailable, retry after {}ms",
                    vendor, retry_after_ms
                ),
            ),

            // Server errors (5xx) - return generic message for security
            Self::ApiCallFailed(_) | Self::NetworkError(_) | Self::TimeoutError(_) => (
                StatusCode::INTERNAL_SERVER_ERROR,
                "execution_failed",
                "Failed to execute LLM request".to_string(),
            ),
            Self::JsonError(_) => (
                StatusCode::INTERNAL_SERVER_ERROR,
                "internal_error",
                "An internal error occurred".to_string(),
            ),
            Self::NoVendorsConfigured => (
                StatusCode::INTERNAL_SERVER_ERROR,
                "no_vendors_configured",
                "No LLM vendors are configured".to_string(),
            ),
            Self::InvalidConfiguration => (
                StatusCode::INTERNAL_SERVER_ERROR,
                "invalid_configuration",
                "Executor configuration is invalid".to_string(),
            ),
            Self::MissingPricing => (
                StatusCode::INTERNAL_SERVER_ERROR,
                "missing_pricing",
                "Configured pricing is missing for the selected target".to_string(),
            ),
            Self::InvalidUpstreamResult => (
                StatusCode::INTERNAL_SERVER_ERROR,
                "invalid_upstream_result",
                "Upstream result could not be used".to_string(),
            ),
        };

        // Log the actual error for debugging (server-side only)
        tracing::error!(
            error = ?self,
            error_code = error_code,
            "Executor error occurred"
        );

        let body = Json(json!({
            "error": {
                "code": error_code,
                "message": message,
            }
        }));

        (status, body).into_response()
    }
}

// Implement From conversions for common error types
impl From<reqwest::Error> for ExecutorError {
    fn from(err: reqwest::Error) -> Self {
        if err.is_timeout() {
            Self::TimeoutError(0) // Actual timeout value should be tracked separately
        } else if err.is_decode() {
            Self::JsonError("malformed upstream JSON".to_string())
        } else if err.is_connect() || err.is_request() {
            Self::NetworkError("upstream transport error".to_string())
        } else if err.is_status() {
            if let Some(status) = err.status() {
                // Use u16 comparison instead of StatusCode
                if status.as_u16() == 401 {
                    Self::AuthenticationFailed("unknown".to_string())
                } else if status.as_u16() == 429 {
                    Self::RateLimitExceeded {
                        vendor: "unknown".to_string(),
                        retry_after_ms: None,
                    }
                } else {
                    Self::ApiCallFailed(format!("HTTP {status}"))
                }
            } else {
                Self::ApiCallFailed("HTTP status error".to_string())
            }
        } else {
            Self::NetworkError("upstream transport error".to_string())
        }
    }
}

impl From<serde_json::Error> for ExecutorError {
    fn from(_err: serde_json::Error) -> Self {
        Self::JsonError("malformed upstream JSON".to_string())
    }
}

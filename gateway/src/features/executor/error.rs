//! Error types for Executor Layer.

use axum::response::{IntoResponse, Response};

use crate::core::http_error::{error_response, ErrorCode};

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

    /// Upstream returned a permanent non-success response.
    #[error("upstream rejected the request")]
    UpstreamRejected,

    /// Overall protected-request deadline elapsed during execution.
    #[error("request deadline exceeded")]
    DeadlineExceeded,

    #[error("Circuit breaker open for vendor '{vendor}'")]
    CircuitOpen { vendor: String, retry_after_ms: u64 },
}

impl ExecutorError {
    fn http_mapping(&self) -> (ErrorCode, &'static str, Option<u64>) {
        match self {
            Self::UnsupportedVendor(_)
            | Self::UnsupportedModel(_, _)
            | Self::InvalidPayload(_) => (
                ErrorCode::InvalidRequest,
                "The request payload is invalid",
                None,
            ),
            Self::AuthenticationFailed(_) => (
                ErrorCode::UpstreamAuthentication,
                "Upstream provider rejected the credentials",
                None,
            ),
            Self::RateLimitExceeded { retry_after_ms, .. } => (
                ErrorCode::UpstreamRateLimit,
                "Upstream provider rate-limited the request",
                retry_after_secs(*retry_after_ms),
            ),
            Self::ApiCallFailed(_)
            | Self::NetworkError(_)
            | Self::TimeoutError(_)
            | Self::UpstreamRejected => (
                ErrorCode::UpstreamError,
                "Upstream provider request failed",
                None,
            ),
            Self::JsonError(_) | Self::InvalidUpstreamResult => (
                ErrorCode::UpstreamProtocol,
                "Upstream provider returned an unusable result",
                None,
            ),
            Self::NoVendorsConfigured
            | Self::InvalidConfiguration
            | Self::MissingPricing => {
                (ErrorCode::InternalError, "An internal error occurred", None)
            }
            Self::DeadlineExceeded => (
                ErrorCode::DeadlineExceeded,
                "The request deadline was exceeded",
                None,
            ),
            Self::CircuitOpen { retry_after_ms, .. } => (
                ErrorCode::CircuitOpen,
                "No eligible upstream target is currently available",
                retry_after_secs(Some(*retry_after_ms)),
            ),
        }
    }
}

fn retry_after_secs(retry_after_ms: Option<u64>) -> Option<u64> {
    retry_after_ms
        .filter(|ms| *ms > 0)
        .map(|ms| ms.div_ceil(1000).max(1))
}

impl IntoResponse for ExecutorError {
    /// Converts executor failures into the canonical protected-API envelope.
    fn into_response(self) -> Response {
        let (code, message, retry_after_secs) = self.http_mapping();
        error_response(code, message, retry_after_secs)
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

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body;
    use axum::http::StatusCode;
    use axum::response::IntoResponse;

    async fn response_json(error: ExecutorError) -> (StatusCode, serde_json::Value) {
        let response = error.into_response();
        let status = response.status();
        let bytes = body::to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("body");
        (
            status,
            serde_json::from_slice(&bytes).expect("json envelope"),
        )
    }

    #[tokio::test]
    async fn upstream_authentication_is_bad_gateway_not_gateway_unauthorized() {
        let (status, body) =
            response_json(ExecutorError::AuthenticationFailed("openai".into())).await;
        assert_eq!(status, StatusCode::BAD_GATEWAY);
        assert_eq!(body["error"]["code"], "UPSTREAM_AUTHENTICATION");
        assert_eq!(
            body["error"]["message"],
            "Upstream provider rejected the credentials"
        );
        assert!(body["error"]["request_id"].is_string());
        let encoded = body.to_string();
        assert!(!encoded.contains("openai"));
        assert!(!encoded.contains("Bearer"));
        assert_ne!(status, StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn provider_protocol_and_rate_limit_use_distinct_sanitized_codes() {
        let (json_status, json_body) =
            response_json(ExecutorError::JsonError("malformed upstream JSON".into()))
                .await;
        assert_eq!(json_status, StatusCode::BAD_GATEWAY);
        assert_eq!(json_body["error"]["code"], "UPSTREAM_PROTOCOL");
        assert!(!json_body.to_string().contains("malformed upstream JSON"));

        let (oversize_status, oversize_body) =
            response_json(ExecutorError::InvalidUpstreamResult).await;
        assert_eq!(oversize_status, StatusCode::BAD_GATEWAY);
        assert_eq!(oversize_body["error"]["code"], "UPSTREAM_PROTOCOL");
        assert_ne!(oversize_status, StatusCode::PAYLOAD_TOO_LARGE);

        let (limit_status, limit_body) =
            response_json(ExecutorError::RateLimitExceeded {
                vendor: "openai".into(),
                retry_after_ms: Some(2_000),
            })
            .await;
        assert_eq!(limit_status, StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(limit_body["error"]["code"], "UPSTREAM_RATE_LIMIT");
        assert!(!limit_body.to_string().contains("openai"));
        assert_ne!(limit_body["error"]["code"], "UNAUTHORIZED");
    }

    #[tokio::test]
    async fn unexpected_internal_and_future_policy_variants_map_deterministically() {
        let (internal_status, internal_body) =
            response_json(ExecutorError::MissingPricing).await;
        assert_eq!(internal_status, StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(internal_body["error"]["code"], "INTERNAL_ERROR");
        assert_eq!(
            internal_body["error"]["message"],
            "An internal error occurred"
        );
        assert!(internal_body.get("response").is_none());

        let (circuit_status, circuit_body) = response_json(ExecutorError::CircuitOpen {
            vendor: "openai".into(),
            retry_after_ms: 5_000,
        })
        .await;
        assert_eq!(circuit_status, StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(circuit_body["error"]["code"], "CIRCUIT_OPEN");
        assert!(!circuit_body.to_string().contains("openai"));

        let (deadline_status, deadline_body) =
            response_json(ExecutorError::DeadlineExceeded).await;
        assert_eq!(deadline_status, StatusCode::GATEWAY_TIMEOUT);
        assert_eq!(deadline_body["error"]["code"], "DEADLINE_EXCEEDED");
    }
}

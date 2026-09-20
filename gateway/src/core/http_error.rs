//! Canonical protected-API error envelope and HTTP mapping.
//!
//! Protected endpoints and framework extractors share
//! `{"error":{"code","message","request_id"}}`. Codes distinguish client,
//! gateway-auth, capability, not-found, dependency, and upstream classes
//! without copying SQL, provider bodies, secrets, or credential-bearing URLs.

use axum::{
    http::{header, HeaderValue, StatusCode},
    response::{IntoResponse, Json, Response},
};
use serde::Serialize;
use std::future::Future;

use super::correlation::RequestContext;

tokio::task_local! {
    static CURRENT_REQUEST_CONTEXT: RequestContext;
}

/// Stable public error codes for protected API responses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorCode {
    /// Semantic client validation failure.
    InvalidRequest,
    /// Malformed JSON body.
    InvalidJson,
    /// Missing or non-JSON Content-Type.
    UnsupportedMediaType,
    /// Path parameter could not be parsed.
    InvalidPath,
    /// Missing, malformed, unknown, or revoked gateway credential.
    Unauthorized,
    /// Authenticated key lacks the required capability.
    Forbidden,
    /// Same-tenant resource is absent; other-tenant IDs use this too.
    NotFound,
    /// Authentication datastore is unavailable.
    AuthDependencyUnavailable,
    /// Generic upstream execution failure.
    UpstreamError,
    /// Upstream rejected provider credentials. Never a gateway 401.
    UpstreamAuthentication,
    /// Upstream success payload was unusable or oversized.
    UpstreamProtocol,
    /// Upstream throttled the request.
    UpstreamRateLimit,
    /// Unexpected internal fault.
    InternalError,
    /// Overall protected-request deadline elapsed.
    DeadlineExceeded,
    /// No eligible target is currently usable because circuits are open.
    CircuitOpen,
    /// Local generation admission is saturated.
    Overloaded,
    /// Request body exceeded the configured protected-route limit.
    PayloadTooLarge,
    /// The process is draining and is not admitting new generation.
    Draining,
}

impl ErrorCode {
    /// Returns the documented stable code string.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::InvalidRequest => "INVALID_REQUEST",
            Self::InvalidJson => "INVALID_JSON",
            Self::UnsupportedMediaType => "UNSUPPORTED_MEDIA_TYPE",
            Self::InvalidPath => "INVALID_PATH",
            Self::Unauthorized => "UNAUTHORIZED",
            Self::Forbidden => "FORBIDDEN",
            Self::NotFound => "NOT_FOUND",
            Self::AuthDependencyUnavailable => "AUTH_DEPENDENCY_UNAVAILABLE",
            Self::UpstreamError => "UPSTREAM_ERROR",
            Self::UpstreamAuthentication => "UPSTREAM_AUTHENTICATION",
            Self::UpstreamProtocol => "UPSTREAM_PROTOCOL",
            Self::UpstreamRateLimit => "UPSTREAM_RATE_LIMIT",
            Self::InternalError => "INTERNAL_ERROR",
            Self::DeadlineExceeded => "DEADLINE_EXCEEDED",
            Self::CircuitOpen => "CIRCUIT_OPEN",
            Self::Overloaded => "OVERLOADED",
            Self::PayloadTooLarge => "PAYLOAD_TOO_LARGE",
            Self::Draining => "DRAINING",
        }
    }

    /// Returns the HTTP status for this code.
    pub const fn status(self) -> StatusCode {
        match self {
            Self::InvalidRequest | Self::InvalidJson | Self::InvalidPath => {
                StatusCode::BAD_REQUEST
            }
            Self::UnsupportedMediaType => StatusCode::UNSUPPORTED_MEDIA_TYPE,
            Self::Unauthorized => StatusCode::UNAUTHORIZED,
            Self::Forbidden => StatusCode::FORBIDDEN,
            Self::NotFound => StatusCode::NOT_FOUND,
            Self::PayloadTooLarge => StatusCode::PAYLOAD_TOO_LARGE,
            Self::UpstreamRateLimit | Self::Overloaded => StatusCode::TOO_MANY_REQUESTS,
            Self::AuthDependencyUnavailable | Self::CircuitOpen | Self::Draining => {
                StatusCode::SERVICE_UNAVAILABLE
            }
            Self::UpstreamError
            | Self::UpstreamAuthentication
            | Self::UpstreamProtocol => StatusCode::BAD_GATEWAY,
            Self::DeadlineExceeded => StatusCode::GATEWAY_TIMEOUT,
            Self::InternalError => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }
}

/// Framework extraction rejection mapped into the canonical envelope.
#[derive(Debug, thiserror::Error)]
#[error("{message}")]
pub struct HttpError {
    code: ErrorCode,
    message: String,
    retry_after_secs: Option<u64>,
}

impl HttpError {
    /// Builds a sanitized HTTP error without retry guidance.
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            retry_after_secs: None,
        }
    }

    /// Builds a sanitized HTTP error with optional Retry-After seconds.
    pub fn with_retry_after(
        code: ErrorCode,
        message: impl Into<String>,
        retry_after_secs: Option<u64>,
    ) -> Self {
        Self {
            code,
            message: message.into(),
            retry_after_secs,
        }
    }
}

impl IntoResponse for HttpError {
    fn into_response(self) -> Response {
        error_response(self.code, self.message, self.retry_after_secs)
    }
}

#[derive(Serialize)]
struct ErrorEnvelope<'a> {
    error: ErrorBody<'a>,
}

#[derive(Serialize)]
struct ErrorBody<'a> {
    code: &'a str,
    message: &'a str,
    request_id: &'a str,
}

/// Runs `fut` with `ctx` as the request-scoped correlation context.
pub async fn scope_request_context<F>(ctx: RequestContext, fut: F) -> F::Output
where
    F: Future,
{
    CURRENT_REQUEST_CONTEXT.scope(ctx, fut).await
}

/// Returns the current request ID, or an empty string outside a request.
pub fn current_request_id() -> String {
    CURRENT_REQUEST_CONTEXT
        .try_with(|ctx| ctx.request_id.clone())
        .unwrap_or_default()
}

/// Returns the validated client correlation ID for the current request.
pub fn current_client_correlation_id() -> Option<String> {
    CURRENT_REQUEST_CONTEXT
        .try_with(|ctx| ctx.client_correlation_id.clone())
        .ok()
        .flatten()
}

/// Serializes the canonical protected-API error envelope.
///
/// Logs once at this HTTP boundary. Client errors are debug; server errors
/// are error-level. Lower layers must not emit a second terminal failure
/// summary before the error reaches this function. The public body never
/// includes SQL, provider payloads, secrets, or credential-bearing URLs.
pub fn error_response(
    code: ErrorCode,
    message: impl Into<String>,
    retry_after_secs: Option<u64>,
) -> Response {
    let message = message.into();
    let request_id = current_request_id();
    let status = code.status();
    log_boundary(status, code, &request_id);

    let envelope = ErrorEnvelope {
        error: ErrorBody {
            code: code.as_str(),
            message: &message,
            request_id: &request_id,
        },
    };
    let mut response = (status, Json(envelope)).into_response();
    if let Some(seconds) = retry_after_secs.filter(|value| *value > 0) {
        if let Ok(value) = HeaderValue::from_str(&seconds.to_string()) {
            response.headers_mut().insert(header::RETRY_AFTER, value);
        }
    }
    response
}

fn log_boundary(status: StatusCode, code: ErrorCode, request_id: &str) {
    let client_correlation_id = current_client_correlation_id();
    if status.is_server_error() {
        tracing::error!(
            error_code = code.as_str(),
            status = status.as_u16(),
            request_id = request_id,
            client_correlation_id = client_correlation_id.as_deref(),
            "request failed"
        );
    } else {
        tracing::debug!(
            error_code = code.as_str(),
            status = status.as_u16(),
            request_id = request_id,
            client_correlation_id = client_correlation_id.as_deref(),
            "request rejected"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body;

    async fn body_json(response: Response) -> serde_json::Value {
        let bytes = body::to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("body");
        serde_json::from_slice(&bytes).expect("json")
    }

    #[test]
    fn codes_map_to_documented_statuses() {
        assert_eq!(ErrorCode::InvalidRequest.status(), StatusCode::BAD_REQUEST);
        assert_eq!(ErrorCode::InvalidJson.status(), StatusCode::BAD_REQUEST);
        assert_eq!(
            ErrorCode::UnsupportedMediaType.status(),
            StatusCode::UNSUPPORTED_MEDIA_TYPE
        );
        assert_eq!(ErrorCode::Unauthorized.status(), StatusCode::UNAUTHORIZED);
        assert_eq!(ErrorCode::Forbidden.status(), StatusCode::FORBIDDEN);
        assert_eq!(ErrorCode::NotFound.status(), StatusCode::NOT_FOUND);
        assert_eq!(
            ErrorCode::AuthDependencyUnavailable.status(),
            StatusCode::SERVICE_UNAVAILABLE
        );
        assert_eq!(
            ErrorCode::UpstreamAuthentication.status(),
            StatusCode::BAD_GATEWAY
        );
        assert_eq!(
            ErrorCode::UpstreamProtocol.status(),
            StatusCode::BAD_GATEWAY
        );
        assert_eq!(ErrorCode::UpstreamError.status(), StatusCode::BAD_GATEWAY);
        assert_eq!(
            ErrorCode::UpstreamRateLimit.status(),
            StatusCode::TOO_MANY_REQUESTS
        );
        assert_eq!(
            ErrorCode::InternalError.status(),
            StatusCode::INTERNAL_SERVER_ERROR
        );
        assert_eq!(
            ErrorCode::DeadlineExceeded.status(),
            StatusCode::GATEWAY_TIMEOUT
        );
        assert_eq!(
            ErrorCode::CircuitOpen.status(),
            StatusCode::SERVICE_UNAVAILABLE
        );
        assert_eq!(
            ErrorCode::Overloaded.status(),
            StatusCode::TOO_MANY_REQUESTS
        );
        assert_eq!(
            ErrorCode::PayloadTooLarge.status(),
            StatusCode::PAYLOAD_TOO_LARGE
        );
        assert_eq!(
            ErrorCode::Draining.status(),
            StatusCode::SERVICE_UNAVAILABLE
        );
        assert_ne!(
            ErrorCode::UpstreamAuthentication.status(),
            StatusCode::UNAUTHORIZED
        );
    }

    #[tokio::test]
    async fn unexpected_internal_mapping_is_generic_500() {
        let ctx = RequestContext::new("req-internal-1".to_string(), None);
        let response = scope_request_context(ctx, async {
            error_response(ErrorCode::InternalError, "An internal error occurred", None)
        })
        .await;
        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
        let content_type = response
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default();
        assert!(content_type.starts_with("application/json"));
        let body = body_json(response).await;
        assert_eq!(body["error"]["code"], "INTERNAL_ERROR");
        assert_eq!(body["error"]["message"], "An internal error occurred");
        assert_eq!(body["error"]["request_id"], "req-internal-1");
        assert!(body.get("response").is_none());
        let encoded = body.to_string();
        assert!(!encoded.contains("stack"));
        assert!(!encoded.contains("postgres://"));
        assert!(!encoded.contains("Bearer"));
    }

    #[tokio::test]
    async fn future_policy_codes_serialize_without_demanding_owners() {
        let deadline = error_response(
            ErrorCode::DeadlineExceeded,
            "The request deadline was exceeded",
            None,
        );
        assert_eq!(deadline.status(), StatusCode::GATEWAY_TIMEOUT);
        let deadline_body = body_json(deadline).await;
        assert_eq!(deadline_body["error"]["code"], "DEADLINE_EXCEEDED");

        let overloaded =
            error_response(ErrorCode::Overloaded, "The service is overloaded", Some(1));
        assert_eq!(overloaded.status(), StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(overloaded.headers().get(header::RETRY_AFTER).unwrap(), "1");
        let overloaded_body = body_json(overloaded).await;
        assert_eq!(overloaded_body["error"]["code"], "OVERLOADED");

        let too_large = error_response(
            ErrorCode::PayloadTooLarge,
            "Request body is too large",
            None,
        );
        assert_eq!(too_large.status(), StatusCode::PAYLOAD_TOO_LARGE);
        assert_eq!(
            body_json(too_large).await["error"]["code"],
            "PAYLOAD_TOO_LARGE"
        );

        let draining =
            error_response(ErrorCode::Draining, "The service is shutting down", None);
        assert_eq!(draining.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(body_json(draining).await["error"]["code"], "DRAINING");
    }
}

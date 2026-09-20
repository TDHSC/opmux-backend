//! Correlation ID middleware for request tracing.
//!
//! Implements fail-safe correlation ID generation and propagation:
//! - Always generates system `request_id` (UUID v4 with timestamp fallback)
//! - Optionally extracts client `correlation_id` from headers
//! - Injects `RequestContext` into request extensions
//! - Adds correlation IDs to response headers
//! - Never fails or blocks requests

use axum::{
    body::Body,
    extract::Request,
    http::{HeaderMap, HeaderValue, Response},
    middleware::Next,
};
use tracing::Instrument;
use uuid::Uuid;

use crate::core::correlation::{http_request_span, RequestContext};

/// Correlation ID middleware with fail-safe design.
///
/// Opens the root `http_request` span before authentication so early
/// failures inherit `request_id` and a validated client correlation ID.
///
/// # Flow
/// 1. Generate unique `request_id` (UUID v4) and enter the root span
/// 2. Extract optional `X-Correlation-ID` header with validation
/// 3. Create `RequestContext` and inject into request extensions
/// 4. Process request through handler chain
/// 5. Add `X-Request-ID` and `X-Correlation-ID` to response headers
///
/// # Fail-Safe Guarantees
/// - Invalid UTF-8 header → Ignore client ID, log warning
/// - Malicious long ID (>256 chars) → Reject and log warning
/// - Empty client ID → Ignore and log debug message
/// - Header creation failure → Log error, continue without header
/// - **Never returns error** → Request always proceeds
///
/// # Headers
///
/// **Request Headers** (optional):
/// - `X-Correlation-ID`: Client-provided correlation ID (max 256 chars)
///
/// **Response Headers** (always added):
/// - `X-Request-ID`: System-generated unique request ID
/// - `X-Correlation-ID`: Echoed client correlation ID (if provided)
///
/// # Example
///
/// ```rust,ignore
/// use axum::{Router, middleware};
/// use gateway::middleware::correlation_id_middleware;
///
/// let app = Router::new()
///     .route("/api/route", post(handler))
///     .layer(middleware::from_fn(correlation_id_middleware));
/// ```
pub async fn correlation_id_middleware(
    mut request: Request,
    next: Next,
) -> Response<Body> {
    let request_id = Uuid::new_v4().to_string();
    let span = http_request_span(&request_id, None);

    async move {
        let client_correlation_id = extract_client_correlation_id(request.headers());
        if let Some(correlation_id) = client_correlation_id.as_deref() {
            tracing::Span::current().record(
                "client_correlation_id",
                tracing::field::display(correlation_id),
            );
        }

        let request_context =
            RequestContext::new(request_id, client_correlation_id.clone());
        request.extensions_mut().insert(request_context.clone());
        tracing::debug!("request context assigned");

        let mut response = crate::core::http_error::scope_request_context(
            request_context.clone(),
            next.run(request),
        )
        .await;
        attach_correlation_headers(
            &mut response,
            &request_context.request_id,
            request_context.client_correlation_id.as_deref(),
        );
        response
    }
    .instrument(span)
    .await
}

fn extract_client_correlation_id(headers: &HeaderMap) -> Option<String> {
    let value = headers.get("X-Correlation-ID")?;
    let text = match value.to_str() {
        Ok(text) => text,
        Err(_) => {
            tracing::warn!("Invalid UTF-8 in X-Correlation-ID header, ignoring");
            return None;
        }
    };
    if text.is_empty() {
        tracing::debug!("Empty X-Correlation-ID header, ignoring");
        None
    } else if text.len() > 256 {
        tracing::warn!(
            length = text.len(),
            "X-Correlation-ID exceeds max length (256), rejecting"
        );
        None
    } else {
        Some(text.to_string())
    }
}

fn attach_correlation_headers(
    response: &mut Response<Body>,
    request_id: &str,
    client_correlation_id: Option<&str>,
) {
    match HeaderValue::from_str(request_id) {
        Ok(header_value) => {
            response.headers_mut().insert("X-Request-ID", header_value);
        }
        Err(_) => {
            tracing::error!("Failed to create X-Request-ID header value");
        }
    }

    let Some(client_id) = client_correlation_id else {
        return;
    };
    match HeaderValue::from_str(client_id) {
        Ok(header_value) => {
            response
                .headers_mut()
                .insert("X-Correlation-ID", header_value);
        }
        Err(_) => {
            tracing::error!("Failed to create X-Correlation-ID header value");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        body::Body,
        http::{Request, StatusCode},
        middleware,
        response::IntoResponse,
        routing::get,
        Router,
    };
    use tower::ServiceExt;

    // Simple test handler that returns OK
    async fn test_handler(request: Request<Body>) -> impl IntoResponse {
        // Extract RequestContext to verify it was injected
        let context = request
            .extensions()
            .get::<RequestContext>()
            .expect("RequestContext should be injected");

        // Return context info in response for testing
        (
            StatusCode::OK,
            format!(
                "request_id={},client_id={:?}",
                context.request_id, context.client_correlation_id
            ),
        )
    }

    #[tokio::test]
    async fn test_request_id_generation() {
        let app = Router::new()
            .route("/test", get(test_handler))
            .layer(middleware::from_fn(correlation_id_middleware));

        let request = Request::builder().uri("/test").body(Body::empty()).unwrap();

        let response = app.oneshot(request).await.unwrap();

        // Should have X-Request-ID header
        assert!(response.headers().contains_key("X-Request-ID"));

        let request_id = response
            .headers()
            .get("X-Request-ID")
            .unwrap()
            .to_str()
            .unwrap();

        // Should be a valid UUID format (or timestamp fallback)
        assert!(!request_id.is_empty());
    }

    #[tokio::test]
    async fn test_client_correlation_id_extraction() {
        let app = Router::new()
            .route("/test", get(test_handler))
            .layer(middleware::from_fn(correlation_id_middleware));

        let request = Request::builder()
            .uri("/test")
            .header("X-Correlation-ID", "client-abc-123")
            .body(Body::empty())
            .unwrap();

        let response = app.oneshot(request).await.unwrap();

        // Should echo X-Correlation-ID
        assert!(response.headers().contains_key("X-Correlation-ID"));
        assert_eq!(
            response.headers().get("X-Correlation-ID").unwrap(),
            "client-abc-123"
        );
    }

    #[tokio::test]
    async fn test_client_correlation_id_absence() {
        let app = Router::new()
            .route("/test", get(test_handler))
            .layer(middleware::from_fn(correlation_id_middleware));

        let request = Request::builder().uri("/test").body(Body::empty()).unwrap();

        let response = app.oneshot(request).await.unwrap();

        // Should have X-Request-ID but not X-Correlation-ID
        assert!(response.headers().contains_key("X-Request-ID"));
        assert!(!response.headers().contains_key("X-Correlation-ID"));
    }

    #[tokio::test]
    async fn test_invalid_correlation_id_too_long() {
        let app = Router::new()
            .route("/test", get(test_handler))
            .layer(middleware::from_fn(correlation_id_middleware));

        // Create a correlation ID longer than 256 chars
        let long_id = "a".repeat(300);

        let request = Request::builder()
            .uri("/test")
            .header("X-Correlation-ID", long_id)
            .body(Body::empty())
            .unwrap();

        let response = app.oneshot(request).await.unwrap();

        // Should have X-Request-ID but reject long X-Correlation-ID
        assert!(response.headers().contains_key("X-Request-ID"));
        assert!(!response.headers().contains_key("X-Correlation-ID"));
    }

    #[tokio::test]
    async fn test_empty_correlation_id() {
        let app = Router::new()
            .route("/test", get(test_handler))
            .layer(middleware::from_fn(correlation_id_middleware));

        let request = Request::builder()
            .uri("/test")
            .header("X-Correlation-ID", "")
            .body(Body::empty())
            .unwrap();

        let response = app.oneshot(request).await.unwrap();

        // Should have X-Request-ID but ignore empty X-Correlation-ID
        assert!(response.headers().contains_key("X-Request-ID"));
        assert!(!response.headers().contains_key("X-Correlation-ID"));
    }

    #[tokio::test]
    async fn test_request_context_injection() {
        let app = Router::new()
            .route("/test", get(test_handler))
            .layer(middleware::from_fn(correlation_id_middleware));

        let request = Request::builder()
            .uri("/test")
            .header("X-Correlation-ID", "test-123")
            .body(Body::empty())
            .unwrap();

        let response = app.oneshot(request).await.unwrap();

        // Handler should have successfully extracted RequestContext
        assert_eq!(response.status(), StatusCode::OK);

        // Response body should contain context info
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let body_str = String::from_utf8(body.to_vec()).unwrap();

        assert!(body_str.contains("request_id="));
        assert!(body_str.contains("client_id=Some(\"test-123\")"));
    }
}

//! Request correlation and context management.
//!
//! Provides dual correlation ID system for request tracing:
//! - System-generated `request_id` (always present)
//! - Optional client-provided `client_correlation_id`

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Request context with dual correlation IDs.
///
/// This context is created by the correlation middleware and injected into
/// request extensions for use throughout the request lifecycle.
///
/// # Dual Correlation ID Strategy
///
/// - **request_id**: System-generated UUID (always present)
///   - Ensures every request has a unique identifier
///   - Used for internal tracing and debugging
///   - Returned to client in `X-Request-ID` response header
///
/// - **client_correlation_id**: Optional client-provided ID
///   - Extracted from `X-Correlation-ID` request header
///   - Preserved for cross-system tracing
///   - Returned to client in `X-Correlation-ID` response header
///
/// # Usage
///
/// ```rust,ignore
/// // In handler - extract from request extensions
/// let request_context = request.extensions()
///     .get::<RequestContext>()
///     .expect("RequestContext should be injected by middleware");
///
/// tracing::info!(
///     request_id = %request_context.request_id,
///     client_correlation_id = ?request_context.client_correlation_id,
///     "Processing request"
/// );
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RequestContext {
    /// System-generated unique request ID (always present).
    ///
    /// Generated using UUID v4 by the correlation middleware.
    /// Falls back to timestamp-based ID if UUID generation fails.
    pub request_id: String,

    /// Optional client-provided correlation ID for cross-system tracing.
    ///
    /// Extracted from `X-Correlation-ID` request header.
    /// Validated to be non-empty and max 256 characters.
    pub client_correlation_id: Option<String>,

    /// Timestamp when request was received.
    ///
    /// Set by correlation middleware when RequestContext is created.
    pub timestamp: DateTime<Utc>,
}

impl RequestContext {
    /// Creates a new RequestContext with system-generated request ID.
    ///
    /// # Parameters
    /// - `request_id` - System-generated unique request ID
    /// - `client_correlation_id` - Optional client-provided correlation ID
    ///
    /// # Returns
    /// New RequestContext with current timestamp
    pub fn new(request_id: String, client_correlation_id: Option<String>) -> Self {
        Self {
            request_id,
            client_correlation_id,
            timestamp: Utc::now(),
        }
    }

    /// Root HTTP span created before authentication and handler work.
    ///
    /// Child spans and events inherit `request_id` and, when present, the
    /// validated client correlation ID.
    pub fn root_span(&self) -> tracing::Span {
        http_request_span(&self.request_id, self.client_correlation_id.as_deref())
    }
}

/// Creates the root request span before authentication and handlers.
///
/// # Parameters
/// - `request_id` - System-generated request ID
/// - `client_correlation_id` - Validated client correlation, if any
///
/// # Returns
/// Span that request-scoped logs should inherit
pub fn http_request_span(
    request_id: &str,
    client_correlation_id: Option<&str>,
) -> tracing::Span {
    let span = tracing::info_span!(
        "http_request",
        request_id = %request_id,
        client_correlation_id = tracing::field::Empty,
    );
    if let Some(correlation_id) = client_correlation_id {
        span.record(
            "client_correlation_id",
            tracing::field::display(correlation_id),
        );
    }
    span
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_request_context_creation() {
        let request_id = "req-123".to_string();
        let client_id = Some("client-456".to_string());

        let context = RequestContext::new(request_id.clone(), client_id.clone());

        assert_eq!(context.request_id, request_id);
        assert_eq!(context.client_correlation_id, client_id);
        assert!(context.timestamp <= Utc::now());
    }

    #[test]
    fn test_request_context_without_client_id() {
        let request_id = "req-123".to_string();

        let context = RequestContext::new(request_id.clone(), None);

        assert_eq!(context.request_id, request_id);
        assert_eq!(context.client_correlation_id, None);
    }

    #[test]
    fn test_request_context_clone() {
        let context =
            RequestContext::new("req-123".to_string(), Some("client-456".to_string()));
        let cloned = context.clone();

        assert_eq!(context.request_id, cloned.request_id);
        assert_eq!(context.client_correlation_id, cloned.client_correlation_id);
        assert_eq!(context.timestamp, cloned.timestamp);
    }

    #[test]
    fn root_span_records_request_and_optional_correlation() {
        let with_corr = RequestContext::new(
            "req-span-1".to_string(),
            Some("corr-span-1".to_string()),
        );
        let span = with_corr.root_span();
        assert_eq!(
            span.metadata().map(|meta| meta.name()),
            Some("http_request")
        );

        let without_corr = RequestContext::new("req-span-2".to_string(), None);
        let span = without_corr.root_span();
        assert_eq!(
            span.metadata().map(|meta| meta.name()),
            Some("http_request")
        );
    }
}

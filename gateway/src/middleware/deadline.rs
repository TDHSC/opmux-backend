//! Protected-request deadline middleware.
//!
//! Starts one monotonic deadline for protected API work, including
//! authentication and body extraction. Health and metrics stay outside this
//! layer.

use crate::core::deadline::RequestDeadline;
use crate::core::http_error::{error_response, ErrorCode};
use crate::AppState;
use axum::{
    extract::{Request, State},
    middleware::Next,
    response::Response,
};

/// Injects the protected-request deadline and enforces it around inner work.
///
/// # Flow
/// 1. Record one deadline from `protected_request_deadline`
/// 2. Insert it into request extensions for later layers
/// 3. Run authentication, extraction, and handlers until that instant
/// 4. On expiry, return sanitized `504 DEADLINE_EXCEEDED` when a response
///    can still be delivered and drop owned inner work
///
/// # Parameters
/// - `state` - Application state with validated policy limits
/// - `request` - Incoming protected request
/// - `next` - Remaining middleware and handler
///
/// # Returns
/// Inner response, or a canonical deadline-exceeded envelope
pub async fn deadline_middleware(
    State(state): State<AppState>,
    mut request: Request,
    next: Next,
) -> Response {
    let deadline =
        RequestDeadline::from_timeout(state.settings.limits.protected_request_deadline);
    request.extensions_mut().insert(deadline);

    match tokio::time::timeout_at(deadline.as_instant(), next.run(request)).await {
        Ok(response) => response,
        Err(_) => error_response(
            ErrorCode::DeadlineExceeded,
            "The request deadline was exceeded",
            None,
        ),
    }
}

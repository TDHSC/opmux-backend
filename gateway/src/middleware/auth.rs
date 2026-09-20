//! Authentication middleware - protects endpoints with persisted API key validation.

use crate::core::deadline::RequestDeadline;
use crate::features::auth::{AuthError, AuthenticateError};
use crate::AppState;
use axum::{
    extract::{Request, State},
    http::HeaderMap,
    middleware::Next,
    response::Response,
};
use std::time::Instant;

/// Authentication middleware function.
///
/// This is a child of the root `http_request` span. Authentication timing
/// covers header validation, digest lookup, and last-used update, then ends
/// before downstream inference or management handling.
///
/// Validates a single `X-API-Key` header through [`crate::features::auth::AuthService`]
/// and injects [`AuthContext`]. Legacy development bypass settings are ignored.
pub async fn auth_middleware(
    State(state): State<AppState>,
    mut request: Request,
    next: Next,
) -> Result<Response, AuthError> {
    authenticate_request(&state, &mut request).await?;
    Ok(next.run(request).await)
}

#[tracing::instrument(
    level = "debug",
    skip(state, request),
    fields(
        auth_method = "api_key",
        auth_duration_ms = tracing::field::Empty,
        outcome = tracing::field::Empty,
    )
)]
async fn authenticate_request(
    state: &AppState,
    request: &mut Request,
) -> Result<(), AuthError> {
    let started = Instant::now();
    let result = match presented_api_key(request.headers()) {
        PresentedKey::Missing | PresentedKey::Invalid => {
            Err((AuthError::InvalidCredentials, "invalid_credentials"))
        }
        PresentedKey::Value(presented) => {
            let deadline = request.extensions().get::<RequestDeadline>().copied();
            let auth_result = match deadline {
                Some(deadline) => {
                    state
                        .auth_service
                        .authenticate_with_deadline(&presented, deadline)
                        .await
                }
                None => state.auth_service.authenticate(&presented).await,
            };
            match auth_result {
                Ok(context) => {
                    request.extensions_mut().insert(context);
                    Ok("authenticated")
                }
                Err(AuthenticateError::InvalidCredentials) => {
                    Err((AuthError::InvalidCredentials, "invalid_credentials"))
                }
                Err(AuthenticateError::StoreUnavailable) => {
                    Err((AuthError::StoreUnavailable, "store_unavailable"))
                }
                Err(AuthenticateError::DeadlineExceeded) => {
                    Err((AuthError::DeadlineExceeded, "deadline_exceeded"))
                }
            }
        }
    };

    let auth_duration_ms = started.elapsed().as_millis() as u64;
    let span = tracing::Span::current();
    span.record("auth_duration_ms", auth_duration_ms);
    let (outcome, result) = match result {
        Ok(outcome) => (outcome, Ok(())),
        Err((error, outcome)) => (outcome, Err(error)),
    };
    span.record("outcome", outcome);
    tracing::debug!(auth_duration_ms, outcome, "authentication finished");
    result
}

pub(crate) enum PresentedKey {
    Missing,
    Invalid,
    Value(String),
}

/// Extracts a single unambiguous API key from `X-API-Key`.
///
/// Missing, empty, non-UTF8, whitespace-padded, comma-joined, and duplicate
/// headers are rejected. The raw value is never logged.
pub(crate) fn presented_api_key(headers: &HeaderMap) -> PresentedKey {
    let mut values = headers.get_all("x-api-key").iter();
    let Some(first) = values.next() else {
        return PresentedKey::Missing;
    };
    if values.next().is_some() {
        return PresentedKey::Invalid;
    }
    let Ok(text) = first.to_str() else {
        return PresentedKey::Invalid;
    };
    if text.is_empty()
        || text.trim() != text
        || text.trim().is_empty()
        || text.contains(',')
    {
        return PresentedKey::Invalid;
    }
    PresentedKey::Value(text.to_string())
}

impl std::fmt::Debug for PresentedKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Missing => f.write_str("Missing"),
            Self::Invalid => f.write_str("Invalid"),
            Self::Value(_) => f.write_str("Value([redacted])"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderValue;

    #[test]
    fn extract_api_key_missing_when_absent() {
        let headers = HeaderMap::new();
        assert!(matches!(presented_api_key(&headers), PresentedKey::Missing));
    }

    #[test]
    fn extract_api_key_invalid_for_empty_duplicate_and_comma() {
        let mut headers = HeaderMap::new();
        headers.insert("x-api-key", HeaderValue::from_static(" "));
        assert!(matches!(presented_api_key(&headers), PresentedKey::Invalid));

        let mut headers = HeaderMap::new();
        headers.append("x-api-key", HeaderValue::from_static("abc"));
        headers.append("x-api-key", HeaderValue::from_static("abc"));
        assert!(matches!(presented_api_key(&headers), PresentedKey::Invalid));

        let mut headers = HeaderMap::new();
        headers.insert("x-api-key", HeaderValue::from_static("one,two"));
        assert!(matches!(presented_api_key(&headers), PresentedKey::Invalid));

        let mut headers = HeaderMap::new();
        headers.insert(
            "x-api-key",
            HeaderValue::from_bytes(&[0xff]).expect("opaque"),
        );
        assert!(matches!(presented_api_key(&headers), PresentedKey::Invalid));
    }

    #[test]
    fn extract_api_key_value_when_single_token() {
        let mut headers = HeaderMap::new();
        headers.insert("x-api-key", HeaderValue::from_static("opmx_v1_token"));
        match presented_api_key(&headers) {
            PresentedKey::Value(value) => assert_eq!(value, "opmx_v1_token"),
            other => panic!("expected value, got header class {other:?}"),
        }
    }
}

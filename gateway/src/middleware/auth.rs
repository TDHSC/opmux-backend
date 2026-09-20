//! Authentication middleware - protects endpoints with persisted API key validation.

use crate::features::auth::{AuthError, AuthenticateError};
use crate::AppState;
use axum::{
    extract::{Request, State},
    http::HeaderMap,
    middleware::Next,
    response::Response,
};

/// Authentication middleware function.
///
/// This is a CHILD SPAN. It automatically inherits `request_id` and
/// `client_correlation_id` from the correlation_id_middleware (root span).
///
/// Validates a single `X-API-Key` header through [`crate::features::auth::AuthService`]
/// and injects [`AuthContext`]. Legacy development bypass settings are ignored.
#[tracing::instrument(
    level = "debug",
    skip(state, request, next),
    fields(auth_method = "api_key")
)]
pub async fn auth_middleware(
    State(state): State<AppState>,
    mut request: Request,
    next: Next,
) -> Result<Response, AuthError> {
    let presented = match presented_api_key(request.headers()) {
        PresentedKey::Value(value) => value,
        PresentedKey::Missing | PresentedKey::Invalid => {
            tracing::debug!(
                success = false,
                reason = "invalid_header",
                "Authentication failed"
            );
            return Err(AuthError::InvalidCredentials);
        }
    };

    let auth_context = match state.auth_service.authenticate(&presented).await {
        Ok(context) => context,
        Err(AuthenticateError::InvalidCredentials) => {
            tracing::debug!(
                success = false,
                reason = "invalid_credentials",
                "Authentication failed"
            );
            return Err(AuthError::InvalidCredentials);
        }
        Err(AuthenticateError::StoreUnavailable) => {
            tracing::debug!(
                success = false,
                reason = "store_unavailable",
                "Authentication failed"
            );
            return Err(AuthError::StoreUnavailable);
        }
    };

    request.extensions_mut().insert(auth_context);
    Ok(next.run(request).await)
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

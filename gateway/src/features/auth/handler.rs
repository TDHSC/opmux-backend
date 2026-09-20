//! Handler Layer - HTTP API key management.
//!
//! Management credentials may create and list keys for the authenticated
//! tenant. Inference credentials receive 403. Ownership and kind come from
//! `AuthContext`, never from request fields.

use super::{
    persist::ApiKeyKind, AuthContext, AuthError, AuthService, IssuedKey, KeyInventory,
};
use crate::AppState;
use axum::{
    extract::{Json, State},
    http::{header, HeaderValue, StatusCode},
    response::{IntoResponse, Json as ResponseJson, Response},
};
use serde_json::Value;

/// Creates a management or inference key in the authenticated tenant.
///
/// # Flow
/// 1. Requires a management credential
/// 2. Rejects ownership fields and unsupported kinds
/// 3. Issues a key through the shared provisioning service
/// 4. Returns 201 with one-time credential and `Cache-Control: no-store`
///
/// # Parameters
/// - `state` - Injected application state
/// - `auth` - Authenticated tenant, key, and kind
/// - `body` - JSON object with `name` and `kind`
///
/// # Returns
/// Safe metadata plus the newly generated credential
///
/// # Errors
/// - `401` from authentication middleware for missing/unknown credentials
/// - `403` when the caller is not a management key
/// - `400` for ownership override, invalid kind, or invalid name
#[tracing::instrument(
    skip(state, auth, body),
    fields(
        endpoint = "/api/v1/auth/keys",
        client_id = %auth.client_id,
        key_id = %auth.key_id,
    )
)]
pub async fn create_api_key(
    State(state): State<AppState>,
    auth: AuthContext,
    Json(body): Json<Value>,
) -> Result<Response, AuthError> {
    AuthService::require_management(&auth)?;
    let (name, kind) = parse_create_key_request(&body)?;
    let issued = state.auth_service.create_key(&auth, &name, kind).await?;
    Ok(created_key_response(issued))
}

/// Lists safe key metadata for the authenticated tenant.
///
/// Inventory is scoped to `auth.client_id`. Query selectors cannot choose
/// another tenant. Responses omit credentials and digests.
///
/// # Parameters
/// - `state` - Injected application state
/// - `auth` - Authenticated tenant, key, and kind
///
/// # Returns
/// Bounded same-tenant inventory
///
/// # Errors
/// - `401` from authentication middleware for missing/unknown credentials
/// - `403` when the caller is not a management key
#[tracing::instrument(
    skip(state, auth),
    fields(
        endpoint = "/api/v1/auth/keys",
        client_id = %auth.client_id,
        key_id = %auth.key_id,
    )
)]
pub async fn list_api_keys(
    State(state): State<AppState>,
    auth: AuthContext,
) -> Result<ResponseJson<KeyInventory>, AuthError> {
    Ok(ResponseJson(state.auth_service.list_keys(&auth).await?))
}

fn created_key_response(issued: IssuedKey) -> Response {
    (
        StatusCode::CREATED,
        [(header::CACHE_CONTROL, HeaderValue::from_static("no-store"))],
        ResponseJson(issued),
    )
        .into_response()
}

/// Parses a create-key JSON object.
///
/// Ownership fields (`client_id`, `tenant_id`) and unknown properties are
/// rejected. `kind` must be `management` or `inference`.
fn parse_create_key_request(body: &Value) -> Result<(String, ApiKeyKind), AuthError> {
    let object = body.as_object().ok_or_else(|| {
        AuthError::InvalidInput("request body must be a JSON object".to_string())
    })?;
    if object.contains_key("client_id") || object.contains_key("tenant_id") {
        return Err(AuthError::InvalidInput(
            "key ownership cannot be set in the request".to_string(),
        ));
    }
    for key in object.keys() {
        if key != "name" && key != "kind" {
            return Err(AuthError::InvalidInput(
                "unknown field is not allowed".to_string(),
            ));
        }
    }
    let name = object
        .get("name")
        .and_then(Value::as_str)
        .ok_or_else(|| AuthError::InvalidInput("name is required".to_string()))?;
    let kind = match object.get("kind").and_then(Value::as_str) {
        Some("management") => ApiKeyKind::Management,
        Some("inference") => ApiKeyKind::Inference,
        _ => {
            return Err(AuthError::InvalidInput(
                "kind must be management or inference".to_string(),
            ))
        }
    };
    Ok((name.to_string(), kind))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parse_accepts_name_and_supported_kind() {
        let (name, kind) = parse_create_key_request(&json!({
            "name": "route-key",
            "kind": "inference"
        }))
        .expect("valid");
        assert_eq!(name, "route-key");
        assert_eq!(kind, ApiKeyKind::Inference);
    }

    #[test]
    fn parse_rejects_ownership_fields_and_admin_kind() {
        let client = parse_create_key_request(&json!({
            "name": "stolen",
            "kind": "inference",
            "client_id": "11111111-1111-1111-1111-111111111111"
        }))
        .expect_err("client_id");
        assert!(matches!(client, AuthError::InvalidInput(_)));

        let tenant = parse_create_key_request(&json!({
            "name": "stolen",
            "kind": "inference",
            "tenant_id": "11111111-1111-1111-1111-111111111111"
        }))
        .expect_err("tenant_id");
        assert!(matches!(tenant, AuthError::InvalidInput(_)));

        let admin = parse_create_key_request(&json!({
            "name": "admin-kind",
            "kind": "admin"
        }))
        .expect_err("admin");
        assert!(matches!(admin, AuthError::InvalidInput(_)));
    }
}

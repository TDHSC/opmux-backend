//! Handler Layer - HTTP API key management.
//!
//! Management credentials may create, list, and revoke keys for the
//! authenticated tenant. Inference credentials receive 403. Ownership and kind
//! come from `AuthContext`, never from request body, query, or path-tenant
//! fields. Inventory paging uses documented `limit`/`offset`/`kind` query
//! parameters. Revocation is idempotent for a same-tenant key.

use super::{
    persist::{ApiKeyKind, MAX_KEY_LIST_LIMIT},
    AuthContext, AuthError, AuthService, IssuedKey, KeyInventory, KeyListOptions,
};
use crate::{
    core::extract::{ApiJson, ApiPath},
    AppState,
};
use axum::{
    extract::{RawQuery, State},
    http::{header, HeaderValue, StatusCode},
    response::{IntoResponse, Json as ResponseJson, Response},
};
use serde_json::Value;
use std::collections::HashSet;
use uuid::Uuid;

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
    ApiJson(body): ApiJson<Value>,
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
/// - `query` - Optional `limit`, `offset`, and `kind`; ownership selectors
///   are rejected
///
/// # Returns
/// Bounded same-tenant inventory with continuation flag
///
/// # Errors
/// - `401` from authentication middleware for missing/unknown credentials
/// - `403` when the caller is not a management key
/// - `400` for ownership selectors, unknown parameters, or invalid paging
#[tracing::instrument(
    skip(state, auth, query),
    fields(
        endpoint = "/api/v1/auth/keys",
        client_id = %auth.client_id,
        key_id = %auth.key_id,
    )
)]
pub async fn list_api_keys(
    State(state): State<AppState>,
    auth: AuthContext,
    RawQuery(query): RawQuery,
) -> Result<ResponseJson<KeyInventory>, AuthError> {
    AuthService::require_management(&auth)?;
    let options = parse_list_keys_query(query.as_deref())?;
    Ok(ResponseJson(
        state.auth_service.list_keys(&auth, options).await?,
    ))
}

/// Revokes a same-tenant key without deleting its inventory row.
///
/// First revocation commits `revoked_at` and returns 204. Repeating DELETE
/// for the same tenant returns 204 without changing that timestamp.
/// Other-tenant and unknown identifiers are indistinguishable 404. There is
/// no last-manager prohibition; operator CLI recovery issues a replacement.
/// Subsequent authentication is denied after commit. Already admitted work
/// may finish.
///
/// # Parameters
/// - `state` - Injected application state
/// - `auth` - Authenticated tenant, key, and kind
/// - `key_id` - Target key identifier from the path
///
/// # Returns
/// Empty 204 response after commit
///
/// # Errors
/// - `401` from authentication middleware for missing/unknown credentials
/// - `403` when the caller is not a management key
/// - `404` when the key is missing or owned by another tenant
#[tracing::instrument(
    skip(state, auth),
    fields(
        endpoint = "/api/v1/auth/keys/{id}",
        client_id = %auth.client_id,
        key_id = %auth.key_id,
        target_key_id = %key_id,
    )
)]
pub async fn revoke_api_key(
    State(state): State<AppState>,
    auth: AuthContext,
    ApiPath(key_id): ApiPath<Uuid>,
) -> Result<StatusCode, AuthError> {
    AuthService::require_management(&auth)?;
    state.auth_service.revoke_key(&auth, key_id).await?;
    Ok(StatusCode::NO_CONTENT)
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

/// Parses inventory query parameters.
///
/// Ownership fields (`client_id`, `tenant_id`), unknown names, duplicates,
/// and invalid paging/kind values are rejected. Tenant scope is not read
/// from the query string.
fn parse_list_keys_query(raw: Option<&str>) -> Result<KeyListOptions, AuthError> {
    let mut options = KeyListOptions::default();
    let Some(raw) = raw.filter(|value| !value.is_empty()) else {
        return Ok(options);
    };
    let mut seen = HashSet::new();
    for pair in raw.split('&') {
        if pair.is_empty() {
            continue;
        }
        let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
        if !seen.insert(key) {
            return Err(AuthError::InvalidInput(
                "duplicate query parameter is not allowed".to_string(),
            ));
        }
        match key {
            "client_id" | "tenant_id" => {
                return Err(AuthError::InvalidInput(
                    "key ownership cannot be set in the query".to_string(),
                ));
            }
            "limit" => {
                let limit = parse_i64_query(value, "limit")?;
                if !(1..=MAX_KEY_LIST_LIMIT).contains(&limit) {
                    return Err(AuthError::InvalidInput(format!(
                        "limit must be between 1 and {MAX_KEY_LIST_LIMIT}"
                    )));
                }
                options.limit = limit;
            }
            "offset" => {
                let offset = parse_i64_query(value, "offset")?;
                if offset < 0 {
                    return Err(AuthError::InvalidInput(
                        "offset must be greater than or equal to 0".to_string(),
                    ));
                }
                options.offset = offset;
            }
            "kind" => {
                options.kind = Some(parse_kind_query(value)?);
            }
            _ => {
                return Err(AuthError::InvalidInput(
                    "unknown query parameter is not allowed".to_string(),
                ));
            }
        }
    }
    Ok(options)
}

fn parse_i64_query(raw: &str, field: &str) -> Result<i64, AuthError> {
    raw.parse::<i64>()
        .map_err(|_| AuthError::InvalidInput(format!("{field} must be an integer")))
}

fn parse_kind_query(raw: &str) -> Result<ApiKeyKind, AuthError> {
    match raw {
        "management" => Ok(ApiKeyKind::Management),
        "inference" => Ok(ApiKeyKind::Inference),
        _ => Err(AuthError::InvalidInput(
            "kind must be management or inference".to_string(),
        )),
    }
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

    #[test]
    fn parse_list_query_defaults_and_accepts_paging() {
        let defaults = parse_list_keys_query(None).expect("empty");
        assert_eq!(defaults.limit, MAX_KEY_LIST_LIMIT);
        assert_eq!(defaults.offset, 0);
        assert_eq!(defaults.kind, None);

        let page =
            parse_list_keys_query(Some("limit=2&offset=4&kind=inference")).expect("page");
        assert_eq!(page.limit, 2);
        assert_eq!(page.offset, 4);
        assert_eq!(page.kind, Some(ApiKeyKind::Inference));
    }

    #[test]
    fn parse_list_query_rejects_ownership_and_malformed_paging() {
        for raw in [
            "client_id=11111111-1111-1111-1111-111111111111",
            "tenant_id=11111111-1111-1111-1111-111111111111",
            "kind=admin",
            "limit=0",
            "limit=-1",
            "limit=101",
            "limit=abc",
            "offset=-1",
            "offset=nope",
            "unknown=1",
            "limit=1&limit=2",
        ] {
            let err = parse_list_keys_query(Some(raw)).expect_err(raw);
            assert!(matches!(err, AuthError::InvalidInput(_)), "{raw}");
        }
    }
}

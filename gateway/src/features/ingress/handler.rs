// Handler Layer - HTTP request/response processing

use super::{
    error::IngressError, service::IngressResponse, validate::parse_ingress_request,
};
use crate::{
    core::{correlation::RequestContext, deadline::RequestDeadline, extract::ApiJson},
    features::auth::{ApiKeyKind, AuthContext},
    AppState,
};
use axum::{
    extract::{Extension, State},
    response::Json as ResponseJson,
};
use serde_json::Value;

/// HTTP handler for AI routing ingress endpoint.
///
/// This is the ROOT SPAN for request tracing. All child spans automatically
/// inherit `request_id` and `client_correlation_id` from this span.
///
/// # Flow
/// 1. Validates inference capability
/// 2. Parses the canonical request contract and prompt/parameter bounds
/// 3. Acquires a generation admission permit without waiting
/// 4. Processes the request through stateless configured routing
/// 5. Returns JSON response or error. The permit is released on every exit.
///
/// # Parameters
/// - `state` - Application state with shared services (injected via Axum state)
/// - `request_context` - Request correlation context (injected by correlation_id_middleware)
/// - `auth_context` - Authentication context (injected by auth middleware)
/// - `deadline` - Shared protected-request deadline (injected by deadline middleware)
/// - `body` - JSON AI routing request
///
/// # Returns
/// JSON response with AI content, model info, cost, and timing
#[tracing::instrument(
    skip(state, request_context, auth_context, deadline, body),
    fields(
        request_id = %request_context.request_id,
        client_correlation_id = ?request_context.client_correlation_id,
        user_id = %auth_context.client_id,
        endpoint = "/api/v1/route",
        prompt_length = tracing::field::Empty,
    )
)]
pub async fn ingress_handler(
    State(state): State<AppState>,
    Extension(request_context): Extension<RequestContext>,
    Extension(deadline): Extension<RequestDeadline>,
    auth_context: AuthContext,
    ApiJson(body): ApiJson<Value>,
) -> Result<ResponseJson<IngressResponse>, IngressError> {
    tracing::info!("Incoming AI routing request");

    if auth_context.kind != ApiKeyKind::Inference {
        tracing::debug!(
            reason = "capability_denied",
            "Generation requires an inference key"
        );
        return Err(IngressError::AuthorizationFailed);
    }

    let request = parse_ingress_request(&body, &state.settings.limits)?;
    tracing::Span::current().record("prompt_length", request.prompt.len());
    tracing::debug!("Request validation passed");

    let _permit = match state.admission.try_acquire() {
        Some(permit) => permit,
        None => {
            tracing::debug!(
                reason = "generation_saturated",
                "Generation admission is saturated"
            );
            return Err(IngressError::Overloaded);
        }
    };

    match state
        .ingress_service
        .process_request(request, deadline)
        .await
    {
        Ok(response) => {
            tracing::info!(
                model = %response.model_used,
                cost = response.cost,
                processing_time_ms = response.processing_time_ms,
                "Request processing completed successfully"
            );
            Ok(ResponseJson(response))
        }
        Err(error) => Err(error),
    }
}

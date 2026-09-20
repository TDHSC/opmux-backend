// Handler Layer - HTTP request/response processing

use super::{
    constants::{MAX_METADATA_SIZE, MAX_PROMPT_LENGTH, MIN_PROMPT_LENGTH},
    error::IngressError,
    service::{IngressRequest, IngressResponse},
};
use crate::{
    core::correlation::RequestContext,
    features::auth::{ApiKeyKind, AuthContext},
    AppState,
};
use axum::{
    extract::{Extension, Json, State},
    response::Json as ResponseJson,
};

/// HTTP handler for AI routing ingress endpoint.
///
/// This is the ROOT SPAN for request tracing. All child spans automatically
/// inherit `request_id` and `client_correlation_id` from this span.
///
/// # Flow
/// 1. Validates request (non-empty prompt)
/// 2. Extracts user_id from authentication context
/// 3. Processes request through service layer
/// 4. Returns JSON response or error
///
/// # Parameters
/// - `state` - Application state with shared services (injected via Axum state)
/// - `request_context` - Request correlation context (injected by correlation_id_middleware)
/// - `auth_context` - Authentication context (injected by auth middleware)
/// - `request` - JSON AI routing request with prompt and metadata
///
/// # Returns
/// JSON response with AI content, model info, cost, and timing
#[tracing::instrument(
    skip(state, request_context, auth_context, request),
    fields(
        request_id = %request_context.request_id,
        client_correlation_id = ?request_context.client_correlation_id,
        user_id = %auth_context.client_id,
        endpoint = "/api/v1/route",
        prompt_length = request.prompt.len(),
    )
)]
pub async fn ingress_handler(
    State(state): State<AppState>,
    Extension(request_context): Extension<RequestContext>,
    auth_context: AuthContext,
    Json(request): Json<IngressRequest>,
) -> Result<ResponseJson<IngressResponse>, IngressError> {
    tracing::info!("Incoming AI routing request");

    if auth_context.kind != ApiKeyKind::Inference {
        tracing::debug!(
            reason = "capability_denied",
            "Generation requires an inference key"
        );
        return Err(IngressError::AuthorizationFailed);
    }

    let prompt_len = request.prompt.trim().chars().count();

    if prompt_len < MIN_PROMPT_LENGTH {
        tracing::warn!("Request validation failed: empty prompt");
        return Err(IngressError::InvalidRequest(
            "Prompt cannot be empty".to_string(),
        ));
    }

    if prompt_len > MAX_PROMPT_LENGTH {
        tracing::warn!(
            prompt_len = prompt_len,
            "Request validation failed: prompt too long"
        );
        return Err(IngressError::InvalidRequest(format!(
            "Prompt exceeds maximum length of {} characters",
            MAX_PROMPT_LENGTH
        )));
    }

    let metadata_size = serde_json::to_vec(&request.metadata)
        .map(|bytes| bytes.len())
        .unwrap_or(usize::MAX);
    if metadata_size > MAX_METADATA_SIZE {
        tracing::warn!(
            metadata_size = metadata_size,
            "Request validation failed: metadata too large"
        );
        return Err(IngressError::InvalidRequest(format!(
            "Metadata exceeds maximum size of {} bytes",
            MAX_METADATA_SIZE
        )));
    }

    tracing::debug!("Request validation passed");

    // Use client_id from authentication context. Metadata cannot replace it.
    let user_id = auth_context.client_id.to_string();

    // Process the request through service layer
    // Pass request_context for gRPC metadata construction
    match state
        .ingress_service
        .process_request(request, user_id, &request_context)
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
        Err(error) => {
            tracing::error!(error = ?error, "Request processing failed");
            Err(error)
        }
    }
}

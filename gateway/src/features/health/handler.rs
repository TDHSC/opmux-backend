// Handler Layer - HTTP request/response processing

use super::error::HealthError;
use crate::AppState;
use axum::{extract::State, http::StatusCode, response::Json};

/// HTTP handler for health check endpoints.
///
/// This handler provides a RESTful health check endpoint that returns the current
/// system health status in JSON format. It follows the handler layer pattern by
/// focusing solely on HTTP request/response processing while delegating business
/// logic to the service layer.
///
/// # HTTP Response
///
/// ## Success Response (200 OK)
/// ```json
/// {
///   "status": "healthy",
///   "timestamp": "2025-09-01T16:53:30.625665+00:00"
/// }
/// ```
///
/// ## Error Response (503 Service Unavailable)
/// ```json
/// {
///   "error": "Health check failed - service temporarily unavailable."
/// }
/// ```
///
/// # Usage
///
/// This handler is typically mounted at `/health` and used by:
/// - Load balancers for health checks
/// - Monitoring systems for service availability
/// - Kubernetes liveness/readiness probes
/// - Manual health verification
///
/// # Examples
///
/// ```bash
/// curl http://localhost:3000/health
/// ```
pub async fn health_handler(
    State(app_state): State<AppState>,
) -> Result<Json<super::service::HealthResponse>, HealthError> {
    match app_state.health_service.check_health().await {
        Ok(health_status) => Ok(Json(health_status)),
        Err(error) => {
            // Error logging is now handled by AppError in core/error.rs
            Err(error)
        }
    }
}

/// HTTP handler for readiness check endpoints.
///
/// This handler provides a Kubernetes-compatible readiness probe endpoint that
/// checks if the service is ready to accept traffic by verifying dependencies.
///
/// # HTTP Response
///
/// ## Success Response (200 OK)
/// ```json
/// {
///   "status": "ready",
///   "timestamp": "2025-09-01T16:53:30.625665+00:00",
///   "dependencies": {
///     "database": { "status": "healthy", "latency_ms": 2 },
///     "upstream": { "status": "healthy", "latency_ms": 12 },
///     "default_route": { "status": "healthy" }
///   }
/// }
/// ```
///
/// ## Not Ready Response (503 Service Unavailable)
/// ```json
/// {
///   "status": "not_ready",
///   "timestamp": "2025-09-01T16:53:30.625665+00:00",
///   "dependencies": {
///     "database": { "status": "unhealthy", "error": "Authentication database unavailable" },
///     "upstream": { "status": "healthy" },
///     "default_route": { "status": "healthy" }
///   }
/// }
/// ```
///
/// # Usage
///
/// This handler is typically mounted at `/ready` and used by:
/// - Kubernetes readiness probes
/// - Load balancers for traffic routing decisions
/// - Monitoring systems for dependency health
///
/// # Examples
///
/// ```bash
/// curl http://localhost:3000/ready
/// ```
pub async fn ready_handler(
    State(app_state): State<AppState>,
) -> Result<(StatusCode, Json<super::service::ReadinessResponse>), HealthError> {
    match app_state.health_service.check_readiness().await {
        Ok(readiness) => {
            let status_code = if readiness.status == "ready" {
                StatusCode::OK
            } else {
                StatusCode::SERVICE_UNAVAILABLE
            };
            Ok((status_code, Json(readiness)))
        }
        Err(error) => Err(error),
    }
}

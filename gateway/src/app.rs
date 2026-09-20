//! Shared production application and router composition.
//!
//! The binary and HTTP integration tests build the same route and middleware
//! graph from injected `AppState`. Middleware order is behavior-critical.

use crate::{
    core::{
        config::Settings,
        metrics::{create_metrics, MetricsConfig},
    },
    features::{
        auth::{self, AuthService},
        executor::{
            config::ExecutorConfig, error::ExecutorError, service::ExecutorService,
        },
        health, ingress,
    },
    middleware::correlation_id,
    AppState,
};
use axum::{
    middleware,
    response::Html,
    routing::{get, post},
    Router,
};
use std::sync::Arc;

/// Process services constructed from validated settings.
#[derive(Clone)]
pub struct Application {
    /// Injected application state used by the production router.
    pub state: AppState,
}

impl Application {
    /// Builds executor, health, ingress, and auth services from injected settings.
    ///
    /// # Parameters
    /// - `settings` - Validated operator configuration
    /// - `auth_service` - Persisted API-key authenticator
    ///
    /// # Returns
    /// Application holding shared `AppState`
    ///
    /// # Errors
    /// Returns `ExecutorError` when the vendor client cannot be constructed.
    pub fn from_settings(
        settings: Arc<Settings>,
        auth_service: Arc<AuthService>,
    ) -> Result<Self, ExecutorError> {
        let executor_config = ExecutorConfig::from_settings(&settings);
        let executor_service = Arc::new(ExecutorService::from_config(executor_config)?);
        let health_service = Arc::new(health::HealthService::with_executor(
            executor_service.clone(),
        ));
        let ingress_service = Arc::new(ingress::service::IngressService::new(
            executor_service.clone(),
        ));

        Ok(Self {
            state: AppState {
                settings,
                executor_service,
                ingress_service,
                health_service,
                auth_service,
            },
        })
    }

    /// Builds the production router from these services.
    ///
    /// # Parameters
    /// - `metrics` - Metrics enablement and endpoint path
    ///
    /// # Returns
    /// Router with the production middleware order
    pub fn into_router(self, metrics: MetricsConfig) -> Router {
        build_production_router(self.state, metrics)
    }
}

/// Builds the production route and middleware graph.
///
/// Middleware runs from the last layer added to the first:
/// 1. Correlation ID - generates request_id for every route, including metrics
/// 2. Metrics (when enabled) - records HTTP metrics, including auth failures
/// 3. Auth (protected routes only) - validates authentication
///
/// # Parameters
/// - `state` - Injected application state
/// - `metrics` - Metrics enablement and endpoint path
///
/// # Returns
/// Router used by the binary and HTTP fixtures
pub fn build_production_router(state: AppState, metrics: MetricsConfig) -> Router {
    let protected_routes = Router::new()
        .route("/api/v1/route", post(ingress::ingress_handler))
        .route(
            "/api/v1/auth/keys",
            post(auth::create_api_key).get(auth::list_api_keys),
        )
        .layer(middleware::from_fn_with_state(
            state.clone(),
            crate::middleware::auth::auth_middleware,
        ))
        .with_state(state.clone());

    let public_routes = Router::new()
        .route("/", get(hello_world))
        .route("/health", get(health::health_handler))
        .route("/ready", get(health::ready_handler))
        .with_state(state);

    let mut app = Router::new().merge(protected_routes).merge(public_routes);

    if let Some((metric_layer, prometheus_handle)) = create_metrics(metrics.clone()) {
        app = app
            .route(
                &metrics.endpoint_path,
                get(move || async move { prometheus_handle.render() }),
            )
            .layer(metric_layer);
    }

    app.layer(middleware::from_fn(
        correlation_id::correlation_id_middleware,
    ))
}

async fn hello_world() -> Html<&'static str> {
    Html("<h1>Gateway Service</h1><p>Minimal working server</p>")
}

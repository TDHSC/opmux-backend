use axum::{
    middleware,
    response::Html,
    routing::{get, post},
    Router,
};
use gateway::{
    core::{
        config::Settings,
        metrics::{create_metrics, MetricsConfig},
        tracing::{init_tracing, TracingConfig},
    },
    features::{
        executor::config::ExecutorConfig, executor::service::ExecutorService, health,
        ingress,
    },
    middleware::{auth, correlation_id},
    AppState,
};
use std::sync::Arc;

#[tokio::main]
async fn main() {
    // Initialize tracing for structured logging
    init_tracing(TracingConfig::from_env());
    tracing::info!(version = env!("CARGO_PKG_VERSION"), "Starting gateway");

    let settings = match Settings::load() {
        Ok(settings) => settings,
        Err(error) => {
            tracing::error!(category = error.category(), "{error}");
            std::process::exit(1);
        }
    };
    settings.log_safe_summary();
    let settings = Arc::new(settings);

    // Initialize Prometheus metrics
    tracing::info!("Initializing Prometheus metrics...");
    let metrics_config = MetricsConfig::from_env();
    let metrics_setup = create_metrics(metrics_config.clone());

    if metrics_setup.is_some() {
        tracing::info!(
            endpoint = %metrics_config.endpoint_path,
            "Prometheus metrics enabled"
        );
    } else {
        tracing::warn!("Prometheus metrics disabled by configuration");
    }

    // Initialize ExecutorService for LLM execution
    tracing::info!("Initializing ExecutorService...");
    let executor_config = ExecutorConfig::from_settings(&settings);
    let executor_service = match ExecutorService::from_config(executor_config.clone()) {
        Ok(service) => Arc::new(service),
        Err(e) => {
            tracing::error!("Fatal: Failed to initialize ExecutorService: {}", e);
            std::process::exit(1);
        }
    };
    tracing::info!(
        "ExecutorService initialized with {} vendors",
        executor_service.vendor_count()
    );

    // Initialize HealthService with ExecutorService dependency
    let health_service = Arc::new(health::HealthService::with_executor(
        executor_service.clone(),
    ));
    tracing::info!("HealthService initialized");

    let ingress_service = Arc::new(ingress::service::IngressService::new(
        executor_service.clone(),
    ));
    tracing::info!("IngressService initialized");

    // Create application state with all shared services
    let app_state = AppState {
        settings: settings.clone(),
        executor_service,
        ingress_service,
        health_service,
    };

    // Create protected routes that require authentication
    let protected_routes = Router::new()
        .route("/api/v1/route", post(ingress::ingress_handler))
        .layer(middleware::from_fn(auth::auth_middleware))
        .with_state(app_state.clone());

    // Create public routes that don't require authentication
    let public_routes = Router::new()
        .route("/", get(hello_world))
        .route("/health", get(health::health_handler))
        .route("/ready", get(health::ready_handler))
        .with_state(app_state.clone());

    // Register all routes before applying the outer correlation ID layer.
    let mut app = Router::new().merge(protected_routes).merge(public_routes);

    // Add metrics layer if enabled
    if let Some((metric_layer, prometheus_handle)) = metrics_setup {
        tracing::info!("Adding Prometheus metrics middleware to router");

        // Add metrics endpoint (public, no auth required)
        // ⚠️ SECURITY: In production, restrict /metrics access via network policies
        app = app
            .route(
                &metrics_config.endpoint_path,
                get(|| async move { prometheus_handle.render() }),
            )
            .layer(metric_layer);
    }

    // Middleware runs from the last layer added to the first:
    // 1. Correlation ID - generates request_id for every route, including metrics
    // 2. Metrics (when enabled) - records HTTP metrics, including auth failures
    // 3. Auth (protected routes only) - validates authentication
    let app = app.layer(middleware::from_fn(
        correlation_id::correlation_id_middleware,
    ));

    // Start the server
    let listener = tokio::net::TcpListener::bind(settings.server.bind_address)
        .await
        .unwrap();

    tracing::info!(
        "🚀 Gateway server running on http://{}",
        settings.server.bind_address
    );
    tracing::info!("");
    tracing::info!("📍 Available endpoints:");
    tracing::info!(
        "   - Health check: http://{}/health",
        settings.server.bind_address
    );
    tracing::info!(
        "   - Readiness check: http://{}/ready",
        settings.server.bind_address
    );
    tracing::info!(
        "   - Ingress API: http://{}/api/v1/route (protected)",
        settings.server.bind_address
    );

    if metrics_config.enabled {
        tracing::info!(
            "   - Metrics: http://{}{}",
            settings.server.bind_address,
            metrics_config.endpoint_path
        );
    }

    tracing::info!("");

    if settings.auth.development_mode {
        tracing::info!("🚨 Development mode: Authentication is BYPASSED");
        tracing::info!("🚨 No API key required for testing");
    } else {
        tracing::info!("🔒 Authentication required: X-API-Key header");
    }

    axum::serve(listener, app).await.unwrap();
}

async fn hello_world() -> Html<&'static str> {
    Html("<h1>Gateway Service</h1><p>Minimal working server</p>")
}

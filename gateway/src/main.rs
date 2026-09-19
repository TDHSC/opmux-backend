use gateway::{
    app::Application,
    core::{
        config::Settings,
        metrics::MetricsConfig,
        tracing::{init_tracing, TracingConfig},
    },
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

    let metrics_config = MetricsConfig::from_env();
    if metrics_config.enabled {
        tracing::info!(
            endpoint = %metrics_config.endpoint_path,
            "Prometheus metrics enabled"
        );
    } else {
        tracing::warn!("Prometheus metrics disabled by configuration");
    }

    tracing::info!("Initializing application services...");
    let application = match Application::from_settings(settings) {
        Ok(application) => application,
        Err(error) => {
            tracing::error!("Fatal: Failed to initialize application: {error}");
            std::process::exit(1);
        }
    };
    tracing::info!(
        "ExecutorService initialized with {} vendors",
        application.state.executor_service.vendor_count()
    );
    tracing::info!("HealthService initialized");
    tracing::info!("IngressService initialized");

    let bind_address = application.state.settings.server.bind_address;
    let development_mode = application.state.settings.auth.development_mode;
    let metrics_enabled = metrics_config.enabled;
    let metrics_path = metrics_config.endpoint_path.clone();
    let app = application.into_router(metrics_config);

    let listener = tokio::net::TcpListener::bind(bind_address).await.unwrap();

    tracing::info!("🚀 Gateway server running on http://{bind_address}");
    tracing::info!("");
    tracing::info!("📍 Available endpoints:");
    tracing::info!("   - Health check: http://{bind_address}/health");
    tracing::info!("   - Readiness check: http://{bind_address}/ready");
    tracing::info!("   - Ingress API: http://{bind_address}/api/v1/route (protected)");

    if metrics_enabled {
        tracing::info!("   - Metrics: http://{bind_address}{metrics_path}");
    }

    tracing::info!("");

    if development_mode {
        tracing::info!("🚨 Development mode: Authentication is BYPASSED");
        tracing::info!("🚨 No API key required for testing");
    } else {
        tracing::info!("🔒 Authentication required: X-API-Key header");
    }

    axum::serve(listener, app).await.unwrap();
}

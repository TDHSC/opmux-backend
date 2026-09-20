use gateway::{
    app::Application,
    core::{
        config::Settings,
        db::{runtime_role_from_env, DatabasePoolConfig},
        lifecycle::{bind_listener, serve_until_shutdown},
        metrics::MetricsConfig,
        tracing::{init_tracing, TracingConfig},
    },
    features::auth::{AuthService, PostgresAuthStore},
};
use std::sync::Arc;
use std::time::Duration;

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

    let database = match DatabasePoolConfig::from_env() {
        Ok(config) => config,
        Err(error) => {
            tracing::error!(category = error.as_str(), "{error}");
            std::process::exit(1);
        }
    };
    let runtime_role = match runtime_role_from_env() {
        Ok(role) => role,
        Err(error) => {
            tracing::error!(category = error.as_str(), "{error}");
            std::process::exit(1);
        }
    };
    let pool = match database.connect_lazy_with_role(&runtime_role) {
        Ok(pool) => pool,
        Err(_) => {
            tracing::error!(
                category = "database_unavailable",
                "failed to configure the authentication database pool"
            );
            std::process::exit(1);
        }
    };
    let auth_service = Arc::new(AuthService::new(Arc::new(PostgresAuthStore::new(pool))));

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
    let application = match Application::from_settings_and_metrics(
        settings,
        auth_service,
        metrics_config.clone(),
    ) {
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
    tracing::info!("AuthService initialized");

    let bind_address = application.state.settings.server.bind_address;
    let shutdown = application.state.shutdown.clone();
    let shutdown_grace =
        Duration::from_secs(application.state.settings.server.shutdown_timeout_secs);
    let metrics_enabled = metrics_config.enabled;
    let metrics_path = metrics_config.endpoint_path.clone();
    let app = application.into_router(metrics_config);

    let listener = match bind_listener(bind_address).await {
        Ok(listener) => listener,
        Err(error) => {
            tracing::error!(category = error.category(), "{error}");
            std::process::exit(1);
        }
    };

    tracing::info!("gateway listening on http://{bind_address}");
    tracing::info!("health check: http://{bind_address}/health");
    tracing::info!("readiness check: http://{bind_address}/ready");
    tracing::info!("ingress API: http://{bind_address}/api/v1/route (protected)");
    if metrics_enabled {
        tracing::info!("metrics: http://{bind_address}{metrics_path}");
    }
    tracing::info!(
        shutdown_timeout_secs = shutdown_grace.as_secs(),
        "SIGTERM and SIGINT drain new generation and bound in-flight work"
    );

    if let Err(error) =
        serve_until_shutdown(listener, app, shutdown, shutdown_grace).await
    {
        tracing::error!(category = "server_failed", error = %error, "server stopped");
        std::process::exit(1);
    }
}

//! Prometheus metrics configuration.
//!
//! Provides metrics collection and export for monitoring and observability.

pub use axum_prometheus::metrics_exporter_prometheus::PrometheusHandle;
pub use axum_prometheus::PrometheusMetricLayer;
use axum_prometheus::PrometheusMetricLayerBuilder;

/// Metrics configuration.
#[derive(Debug, Clone)]
pub struct MetricsConfig {
    /// Enable/disable metrics collection.
    pub enabled: bool,

    /// Metrics endpoint path (default: "/metrics").
    pub endpoint_path: String,
}

impl MetricsConfig {
    /// Creates MetricsConfig from environment variables.
    ///
    /// # Environment Variables
    /// - `METRICS_ENABLED`: "true" or "false" (default: "true")
    /// - `METRICS_PATH`: Metrics endpoint path (default: "/metrics")
    ///
    /// # Returns
    /// MetricsConfig with settings from environment
    pub fn from_env() -> Self {
        let enabled = std::env::var("METRICS_ENABLED")
            .map(|v| v == "true" || v == "1")
            .unwrap_or(true);

        let endpoint_path =
            std::env::var("METRICS_PATH").unwrap_or_else(|_| "/metrics".to_string());

        Self {
            enabled,
            endpoint_path,
        }
    }

    /// Creates production configuration.
    ///
    /// - Metrics enabled
    /// - Endpoint: /metrics
    pub fn production() -> Self {
        Self {
            enabled: true,
            endpoint_path: "/metrics".to_string(),
        }
    }

    /// Creates development configuration.
    ///
    /// - Metrics enabled
    /// - Endpoint: /metrics
    pub fn development() -> Self {
        Self {
            enabled: true,
            endpoint_path: "/metrics".to_string(),
        }
    }

    /// Metrics collection disabled. Used by HTTP fixtures that do not scrape.
    pub fn disabled() -> Self {
        Self {
            enabled: false,
            endpoint_path: "/metrics".to_string(),
        }
    }
}

/// Creates Prometheus metric layer and handle if metrics are enabled.
///
/// # Parameters
/// - `config` - Metrics configuration
///
/// # Returns
/// - `Some((layer, handle))` if metrics are enabled (`config.enabled = true`)
/// - `None` if metrics are disabled (`config.enabled = false`)
///
/// # Example
///
/// ```rust,ignore
/// use gateway::core::metrics::{MetricsConfig, create_metrics};
///
/// let config = MetricsConfig::from_env();
///
/// let app = Router::new()
///     .route("/health", get(health_handler));
///
/// // Only add metrics if enabled
/// let app = if let Some((metric_layer, prometheus_handle)) = create_metrics(config) {
///     app.route("/metrics", get(|| async move { prometheus_handle.render() }))
///        .layer(metric_layer)
/// } else {
///     app  // Metrics disabled
/// };
/// ```
pub fn create_metrics(
    config: MetricsConfig,
) -> Option<(PrometheusMetricLayer<'static>, PrometheusHandle)> {
    if !config.enabled {
        tracing::info!("Metrics collection is disabled by configuration");
        return None;
    }

    tracing::info!(
        endpoint = %config.endpoint_path,
        "Initializing Prometheus metrics"
    );

    let (layer, handle) = PrometheusMetricLayerBuilder::new()
        .with_prefix("gateway")
        .with_default_metrics()
        .build_pair();

    Some((layer, handle))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_metrics_config_production() {
        let config = MetricsConfig::production();

        assert!(config.enabled);
        assert_eq!(config.endpoint_path, "/metrics");
    }

    #[test]
    fn test_metrics_config_development() {
        let config = MetricsConfig::development();

        assert!(config.enabled);
        assert_eq!(config.endpoint_path, "/metrics");
    }

    #[test]
    fn test_metrics_config_disabled() {
        let config = MetricsConfig::disabled();

        assert!(!config.enabled);
        assert_eq!(config.endpoint_path, "/metrics");
    }

    #[tokio::test]
    async fn test_create_metrics_enabled() {
        let config = MetricsConfig {
            enabled: true,
            endpoint_path: "/metrics".to_string(),
        };

        let result = create_metrics(config);
        assert!(result.is_some());

        let (_layer, _handle) = result.unwrap();
        // Just verify it compiles and creates the layer/handle
    }

    #[tokio::test]
    async fn test_create_metrics_disabled() {
        let config = MetricsConfig {
            enabled: false,
            endpoint_path: "/metrics".to_string(),
        };

        let result = create_metrics(config);
        assert!(result.is_none());
    }
}

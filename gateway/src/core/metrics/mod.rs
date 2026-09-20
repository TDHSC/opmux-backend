//! Prometheus metrics configuration and bounded execution observations.
//!
//! HTTP metrics use matched route templates. Execution metrics use configured
//! target identifiers and finite outcome classes. `/metrics` is an internal
//! scrape surface: local deployment is loopback-bound, and production must
//! restrict it at the network layer rather than adding metrics authentication.

mod execution;
mod labels;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::OnceLock;
use std::time::Duration;

use axum_prometheus::metrics_exporter_prometheus::{Matcher, PrometheusBuilder};
use axum_prometheus::{EndpointLabel, PrometheusMetricLayerBuilder};

pub use axum_prometheus::metrics_exporter_prometheus::PrometheusHandle;
pub use axum_prometheus::PrometheusMetricLayer;

pub use execution::{
    execution_metrics_sink, ExecutionMetrics, NoopExecutionMetrics,
    PrometheusExecutionMetrics, CIRCUIT_STATE, CIRCUIT_TRANSITIONS_TOTAL,
    DEADLINE_EXCEEDED_TOTAL, EXECUTION_ATTEMPTS_TOTAL, EXECUTION_FALLBACKS_TOTAL,
    EXECUTION_RETRIES_TOTAL, OVERLOAD_REJECTED_TOTAL, SUCCESSFUL_COMPLETION_TOKENS_TOTAL,
    SUCCESSFUL_PROMPT_TOKENS_TOTAL,
};
#[cfg(test)]
pub use execution::{MetricEvent, RecordingExecutionMetrics};
pub use labels::{bound_catalog_id, AttemptOutcome, CircuitStateLabel};

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

static PROMETHEUS_HANDLE: OnceLock<PrometheusHandle> = OnceLock::new();
static PREFIX_SET: AtomicBool = AtomicBool::new(false);

/// Creates Prometheus metric layer and handle if metrics are enabled.
///
/// The process recorder is installed once so tests can build multiple
/// metrics-enabled routers and scrape deltas. Endpoint labels use matched
/// route templates and collapse unmatched paths to `unmatched`.
///
/// # Parameters
/// - `config` - Metrics configuration
///
/// # Returns
/// - `Some((layer, handle))` if metrics are enabled (`config.enabled = true`)
/// - `None` if metrics are disabled (`config.enabled = false`)
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

    let mut builder = PrometheusMetricLayerBuilder::new()
        .with_ignore_pattern("/metrics")
        .with_endpoint_label_type(EndpointLabel::MatchedPathWithFallbackFn(
            bounded_http_endpoint,
        ));
    if !PREFIX_SET.swap(true, Ordering::SeqCst) {
        builder = builder.with_prefix("gateway");
    }

    let (layer, handle) = builder
        .with_metrics_from_fn(shared_prometheus_handle)
        .build_pair();

    Some((layer, handle))
}

fn bounded_http_endpoint(path: &str) -> String {
    match path {
        "/" | "/health" | "/ready" | "/metrics" | "/api/v1/route"
        | "/api/v1/auth/keys" => path.to_string(),
        other if is_auth_key_resource_path(other) => "/api/v1/auth/keys/{id}".to_string(),
        _ => "unmatched".to_string(),
    }
}

fn is_auth_key_resource_path(path: &str) -> bool {
    path.strip_prefix("/api/v1/auth/keys/")
        .is_some_and(|id| !id.is_empty() && !id.contains('/'))
}

fn shared_prometheus_handle() -> PrometheusHandle {
    PROMETHEUS_HANDLE
        .get_or_init(|| {
            execution::describe_execution_metrics();
            let duration_name =
                axum_prometheus::utils::requests_duration_name().to_string();
            PrometheusBuilder::new()
                .upkeep_timeout(Duration::from_secs(5))
                .set_buckets_for_metric(
                    Matcher::Full(duration_name),
                    axum_prometheus::utils::SECONDS_DURATION_BUCKETS,
                )
                .expect("HTTP duration buckets")
                .install_recorder()
                .expect("prometheus recorder")
        })
        .clone()
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
        let second = create_metrics(MetricsConfig::production());
        assert!(second.is_some());
    }

    #[test]
    fn unknown_and_key_paths_collapse_to_bounded_endpoints() {
        assert_eq!(bounded_http_endpoint("/api/v1/route"), "/api/v1/route");
        assert_eq!(
            bounded_http_endpoint("/api/v1/auth/keys"),
            "/api/v1/auth/keys"
        );
        assert_eq!(
            bounded_http_endpoint(
                "/api/v1/auth/keys/11111111-2222-3333-4444-555555555555"
            ),
            "/api/v1/auth/keys/{id}"
        );
        assert_eq!(bounded_http_endpoint("/not-a-route"), "unmatched");
        assert_eq!(
            bounded_http_endpoint("/api/v1/auth/keys/id/extra"),
            "unmatched"
        );
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

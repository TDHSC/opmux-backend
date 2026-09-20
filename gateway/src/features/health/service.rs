// Service Layer - Business logic abstraction

use super::{error::HealthError, repository::HealthRepository};
use crate::core::config::Settings;
use crate::core::lifecycle::ShutdownState;
use crate::features::auth::AuthService;
use crate::features::executor::service::ExecutorService;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::RwLock;

const DATABASE_UNAVAILABLE: &str = "Authentication database unavailable";
const UPSTREAM_UNAVAILABLE: &str = "Upstream provider unreachable";
const NO_USABLE_TARGET: &str = "No usable default-route target";

/// Response structure for health check endpoints.
///
/// Liveness is process-only. Database and upstream failures do not change
/// this response.
#[derive(Serialize, Deserialize)]
pub struct HealthResponse {
    /// Current health status of the process (`"healthy"` while the process
    /// can answer).
    pub status: String,

    /// ISO 8601 timestamp of when the health check was performed.
    pub timestamp: String,

    /// Application version from Cargo.toml
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,

    /// Uptime in seconds
    #[serde(skip_serializing_if = "Option::is_none")]
    pub uptime_seconds: Option<u64>,
}

/// Response structure for readiness check endpoints.
///
/// Used by Kubernetes readiness probes and load balancers to determine
/// if the service is ready to accept traffic.
#[derive(Serialize, Deserialize)]
pub struct ReadinessResponse {
    /// Overall readiness status (`"ready"` or `"not_ready"`).
    pub status: String,

    /// ISO 8601 timestamp
    pub timestamp: String,

    /// Bounded safe statuses for required dependencies.
    pub dependencies: ReadinessDependencies,

    /// True when the process is draining and must not receive new generation.
    pub draining: bool,
}

/// Named dependency statuses reported by `/ready`.
///
/// `/models` reachability is `upstream` only. It is not proof of generation
/// and cannot override `default_route` when no configured target is usable.
#[derive(Serialize, Deserialize, Clone)]
pub struct ReadinessDependencies {
    /// Authentication schema, selected-column, locking, and last_used_at UPDATE access.
    pub database: DependencyStatus,
    /// Provider `/models` reachability and credentials.
    pub upstream: DependencyStatus,
    /// Closed targets on the configured default route.
    pub default_route: DependencyStatus,
}

/// Status of one readiness dependency.
#[derive(Serialize, Deserialize, Clone)]
pub struct DependencyStatus {
    /// Status: `"healthy"` or `"unhealthy"`.
    pub status: String,

    /// Probe latency in milliseconds when a live check ran.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub latency_ms: Option<u64>,

    /// Sanitized error when unhealthy. Never includes SQL, URLs, or secrets.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl DependencyStatus {
    fn healthy(latency_ms: Option<u64>) -> Self {
        Self {
            status: "healthy".to_string(),
            latency_ms,
            error: None,
        }
    }

    fn unhealthy(message: &'static str, latency_ms: Option<u64>) -> Self {
        Self {
            status: "unhealthy".to_string(),
            latency_ms,
            error: Some(message.to_string()),
        }
    }

    fn is_healthy(&self) -> bool {
        self.status == "healthy"
    }
}

/// Health check configuration.
pub struct HealthConfig {
    /// Timeout for dependency probes in seconds.
    pub timeout: u64,
    /// Success-cache TTL in seconds. Failures are never cached.
    pub cache_ttl_secs: u64,
}

impl HealthConfig {
    /// Creates configuration with an explicit timeout and success-cache TTL.
    pub fn new(timeout: u64, cache_ttl_secs: u64) -> Self {
        Self {
            timeout,
            cache_ttl_secs,
        }
    }

    /// Creates configuration from environment variables.
    ///
    /// `HEALTH_CHECK_TIMEOUT` defaults to 2 seconds.
    /// `HEALTH_CHECK_CACHE_TTL_SECS` defaults to 5 seconds. Zero disables
    /// the success cache so every probe rechecks immediately.
    pub fn from_env() -> Self {
        let timeout = std::env::var("HEALTH_CHECK_TIMEOUT")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(2);

        let cache_ttl_secs = std::env::var("HEALTH_CHECK_CACHE_TTL_SECS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(5);

        Self {
            timeout,
            cache_ttl_secs,
        }
    }

    fn probe_timeout(&self) -> Duration {
        Duration::from_secs(self.timeout.max(1))
    }

    fn cache_ttl(&self) -> Duration {
        Duration::from_secs(self.cache_ttl_secs)
    }
}

/// Service for health check business logic and orchestration.
///
/// Liveness is shallow. Readiness requires authentication-database schema,
/// selected-column, locking, and last_used_at UPDATE access, upstream
/// `/models` reachability, and at least one usable default-route target.
/// Successful probes may be cached for the configured TTL; failures
/// are never cached. Draining overrides cached dependency success and
/// reports not ready, including when drain starts during an in-flight
/// probe.
pub struct HealthService {
    /// Repository for process liveness.
    repository: HealthRepository,
    /// Executor used for `/models` probes and default-route circuit inspection.
    executor_service: Option<Arc<ExecutorService>>,
    /// Authenticator used to probe schema and authentication privileges.
    auth_service: Option<Arc<AuthService>>,
    /// Catalog used to identify default-route targets.
    settings: Option<Arc<Settings>>,
    /// Probe timeout and success-cache TTL.
    config: HealthConfig,
    /// Application start time for uptime calculation.
    start_time: Instant,
    /// Last successful database probe. Failures are not stored.
    database_success_at: Arc<RwLock<Option<Instant>>>,
    /// Last successful upstream `/models` probe. Failures are not stored.
    upstream_success_at: Arc<RwLock<Option<Instant>>>,
    /// Process drain flag. When set, readiness is unready regardless of cache.
    shutdown: ShutdownState,
}

impl HealthService {
    /// Creates a liveness-only service with no readiness dependencies.
    ///
    /// `/health` stays healthy. `/ready` is not ready until dependencies are
    /// injected.
    pub fn new() -> Self {
        Self {
            repository: HealthRepository::new(),
            executor_service: None,
            auth_service: None,
            settings: None,
            config: HealthConfig::from_env(),
            start_time: Instant::now(),
            database_success_at: Arc::new(RwLock::new(None)),
            upstream_success_at: Arc::new(RwLock::new(None)),
            shutdown: ShutdownState::new(),
        }
    }

    /// Creates a service that probes upstream vendors only.
    ///
    /// Database and default-route checks remain unready without injected
    /// auth and catalog settings.
    pub fn with_executor(executor_service: Arc<ExecutorService>) -> Self {
        Self {
            repository: HealthRepository::new(),
            executor_service: Some(executor_service),
            auth_service: None,
            settings: None,
            config: HealthConfig::from_env(),
            start_time: Instant::now(),
            database_success_at: Arc::new(RwLock::new(None)),
            upstream_success_at: Arc::new(RwLock::new(None)),
            shutdown: ShutdownState::new(),
        }
    }

    /// Creates a readiness service with database, upstream, and catalog checks.
    pub fn with_dependencies(
        executor_service: Arc<ExecutorService>,
        auth_service: Arc<AuthService>,
        settings: Arc<Settings>,
        config: HealthConfig,
    ) -> Self {
        Self {
            repository: HealthRepository::new(),
            executor_service: Some(executor_service),
            auth_service: Some(auth_service),
            settings: Some(settings),
            config,
            start_time: Instant::now(),
            database_success_at: Arc::new(RwLock::new(None)),
            upstream_success_at: Arc::new(RwLock::new(None)),
            shutdown: ShutdownState::new(),
        }
    }

    /// Replaces the drain flag with a shared process-level state.
    ///
    /// # Parameters
    /// - `shutdown` - Drain flag wired to the same generation limiter
    ///
    /// # Returns
    /// Service that reports not ready after `shutdown` is marked draining
    pub fn with_shutdown_state(mut self, shutdown: ShutdownState) -> Self {
        self.shutdown = shutdown;
        self
    }

    /// Returns the shared drain flag.
    pub fn shutdown_state(&self) -> ShutdownState {
        self.shutdown.clone()
    }

    /// Performs a process liveness check.
    ///
    /// This does not probe the authentication database or upstream provider.
    pub async fn check_health(&self) -> Result<HealthResponse, HealthError> {
        let system_status = self.repository.get_system_status().await?;
        let status = if system_status.is_healthy {
            "healthy".to_string()
        } else {
            "unhealthy".to_string()
        };

        Ok(HealthResponse {
            status,
            timestamp: chrono::Utc::now().to_rfc3339(),
            version: Some(env!("CARGO_PKG_VERSION").to_string()),
            uptime_seconds: Some(self.start_time.elapsed().as_secs()),
        })
    }

    fn cache_is_fresh(&self, cached_at: Instant) -> bool {
        let ttl = self.config.cache_ttl();
        !ttl.is_zero() && cached_at.elapsed() < ttl
    }

    async fn probe_database(&self) -> DependencyStatus {
        {
            let cache = self.database_success_at.read().await;
            if let Some(cached_at) = *cache {
                if self.cache_is_fresh(cached_at) {
                    return DependencyStatus::healthy(None);
                }
            }
        }

        let Some(auth) = &self.auth_service else {
            return DependencyStatus::unhealthy(DATABASE_UNAVAILABLE, None);
        };

        let start = Instant::now();
        let result = tokio::time::timeout(
            self.config.probe_timeout(),
            auth.probe_authentication_access(),
        )
        .await;
        let latency_ms = Some(start.elapsed().as_millis() as u64);
        match result {
            Ok(Ok(())) => {
                *self.database_success_at.write().await = Some(Instant::now());
                DependencyStatus::healthy(latency_ms)
            }
            Ok(Err(_)) | Err(_) => {
                tracing::warn!(dependency = "database", "Readiness probe failed");
                DependencyStatus::unhealthy(DATABASE_UNAVAILABLE, latency_ms)
            }
        }
    }

    async fn probe_upstream(&self) -> DependencyStatus {
        {
            let cache = self.upstream_success_at.read().await;
            if let Some(cached_at) = *cache {
                if self.cache_is_fresh(cached_at) {
                    return DependencyStatus::healthy(None);
                }
            }
        }

        let Some(executor) = &self.executor_service else {
            return DependencyStatus::unhealthy(UPSTREAM_UNAVAILABLE, None);
        };

        let start = Instant::now();
        let result = executor
            .check_all_vendors_health(self.config.timeout.max(1))
            .await;
        let latency_ms = Some(start.elapsed().as_millis() as u64);
        match result {
            Ok(()) => {
                *self.upstream_success_at.write().await = Some(Instant::now());
                DependencyStatus::healthy(latency_ms)
            }
            Err(_) => {
                tracing::warn!(dependency = "upstream", "Readiness probe failed");
                DependencyStatus::unhealthy(UPSTREAM_UNAVAILABLE, latency_ms)
            }
        }
    }

    fn probe_default_route(&self) -> DependencyStatus {
        let (Some(executor), Some(settings)) = (&self.executor_service, &self.settings)
        else {
            return DependencyStatus::unhealthy(NO_USABLE_TARGET, None);
        };
        if executor.default_route_has_usable_target(&settings.catalog) {
            DependencyStatus::healthy(None)
        } else {
            DependencyStatus::unhealthy(NO_USABLE_TARGET, None)
        }
    }

    /// Checks readiness of the authentication database, upstream, and route.
    ///
    /// Returns 200-equivalent `"ready"` only when all three are healthy and
    /// the process is not draining.
    /// Successful database and upstream probes may be reused until TTL
    /// expires. Failures are rechecked immediately. Circuit state is never
    /// success-cached. `/models` success cannot override an unusable default
    /// route. An initial drain snapshot skips new probes. After awaited
    /// probes and route evaluation, one final drain snapshot decides
    /// `draining` and `ready` together. This method does not call generation
    /// endpoints.
    pub async fn check_readiness(&self) -> Result<ReadinessResponse, HealthError> {
        let timestamp = chrono::Utc::now().to_rfc3339();
        if self.shutdown.is_draining() {
            return Ok(ReadinessResponse {
                status: "not_ready".to_string(),
                timestamp,
                draining: true,
                dependencies: ReadinessDependencies {
                    database: self.cached_database_status().await,
                    upstream: self.cached_upstream_status().await,
                    default_route: self.probe_default_route(),
                },
            });
        }

        let (database, upstream) =
            tokio::join!(self.probe_database(), self.probe_upstream());
        let default_route = self.probe_default_route();
        let draining = self.shutdown.is_draining();
        let ready = !draining
            && database.is_healthy()
            && upstream.is_healthy()
            && default_route.is_healthy();

        Ok(ReadinessResponse {
            status: if ready {
                "ready".to_string()
            } else {
                "not_ready".to_string()
            },
            timestamp,
            draining,
            dependencies: ReadinessDependencies {
                database,
                upstream,
                default_route,
            },
        })
    }

    async fn cached_database_status(&self) -> DependencyStatus {
        let cache = self.database_success_at.read().await;
        match *cache {
            Some(_) => DependencyStatus::healthy(None),
            None => DependencyStatus::unhealthy(DATABASE_UNAVAILABLE, None),
        }
    }

    async fn cached_upstream_status(&self) -> DependencyStatus {
        let cache = self.upstream_success_at.read().await;
        match *cache {
            Some(_) => DependencyStatus::healthy(None),
            None => DependencyStatus::unhealthy(UPSTREAM_UNAVAILABLE, None),
        }
    }
}

impl Default for HealthService {
    fn default() -> Self {
        Self::new()
    }
}

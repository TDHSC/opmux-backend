//! Process-environment settings retained for compatible startup.

use std::env;
use std::net::SocketAddr;
use std::sync::OnceLock;

use super::env::EnvSource;
use super::error::ConfigError;
use super::limits::{check_bound, parse_u64, SHUTDOWN_TIMEOUT_SECS};
use crate::core::tracing::{LogFormat, TracingConfig};

/// Global application configuration loaded from process environment.
#[derive(Debug, Clone, Default)]
pub struct Config {
    /// Server configuration
    pub server: ServerConfig,

    /// Authentication configuration
    pub auth: AuthConfig,

    /// Logging configuration
    pub logging: LoggingConfig,

    /// Service URLs for gRPC clients (future)
    pub services: ServiceConfig,
}

/// Server configuration
#[derive(Debug, Clone)]
pub struct ServerConfig {
    /// Server bind address (host:port)
    pub bind_address: SocketAddr,

    /// Graceful shutdown timeout in seconds
    pub shutdown_timeout_secs: u64,
}

/// Authentication configuration
#[derive(Debug, Clone)]
pub struct AuthConfig {
    /// Enable development mode bypass (NEVER enable in production)
    pub development_mode: bool,

    /// Mock client ID for development mode
    pub dev_client_id: String,

    /// Slow operation threshold in milliseconds
    pub slow_operation_threshold_ms: u64,
}

impl AuthConfig {
    /// Check if development mode is enabled
    pub fn is_development_mode(&self) -> bool {
        self.development_mode
    }

    /// Get development client ID
    pub fn get_dev_client_id(&self) -> &str {
        &self.dev_client_id
    }

    /// Get slow operation threshold in milliseconds
    pub fn get_slow_threshold_ms(&self) -> u64 {
        self.slow_operation_threshold_ms
    }
}

/// Logging configuration
#[derive(Debug, Clone)]
pub struct LoggingConfig {
    /// Log level (trace, debug, info, warn, error)
    pub level: String,

    /// Enable JSON structured logging
    pub json_format: bool,
}

/// External service configuration
#[derive(Debug, Clone)]
pub struct ServiceConfig {
    /// Router service gRPC URL
    pub router_url: String,

    /// Memory service gRPC URL
    pub memory_url: String,

    /// Rewrite service gRPC URL
    pub rewrite_url: String,

    /// Validation service gRPC URL
    pub validation_url: String,

    /// gRPC connection timeout in milliseconds
    pub connection_timeout_ms: u64,

    /// gRPC request timeout in milliseconds
    pub request_timeout_ms: u64,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            bind_address: "0.0.0.0:3000".parse().unwrap(),
            shutdown_timeout_secs: 30,
        }
    }
}

impl Default for AuthConfig {
    fn default() -> Self {
        Self {
            development_mode: false,
            dev_client_id: "dev-client-123".to_string(),
            slow_operation_threshold_ms: 10,
        }
    }
}

impl Default for LoggingConfig {
    fn default() -> Self {
        Self {
            level: "info".to_string(),
            json_format: true,
        }
    }
}

impl Default for ServiceConfig {
    fn default() -> Self {
        Self {
            router_url: "http://localhost:50051".to_string(),
            memory_url: "http://localhost:50052".to_string(),
            rewrite_url: "http://localhost:50053".to_string(),
            validation_url: "http://localhost:50054".to_string(),
            connection_timeout_ms: 5000,
            request_timeout_ms: 30000,
        }
    }
}

impl Config {
    /// Load configuration from environment variables
    pub fn from_env() -> Self {
        Self {
            server: ServerConfig::from_env(),
            auth: AuthConfig::from_env(),
            logging: LoggingConfig::from_env(),
            services: ServiceConfig::from_env(),
        }
    }

    /// Validate configuration and log startup information
    pub fn validate(&self) {
        tracing::info!("Server will bind to: {}", self.server.bind_address);
        tracing::info!("Shutdown timeout: {}s", self.server.shutdown_timeout_secs);

        if self.auth.development_mode {
            tracing::warn!("🚨 AUTH_DEVELOPMENT_MODE is ENABLED");
            tracing::warn!("🚨 Authentication is BYPASSED for development");
            tracing::warn!("🚨 This should NEVER be enabled in production");
            tracing::warn!("🚨 Mock client ID: {}", self.auth.dev_client_id);
        } else {
            tracing::info!("✅ Authentication is ENABLED (production mode)");
        }

        tracing::info!("Log level: {}", self.logging.level);
        tracing::info!("JSON format: {}", self.logging.json_format);

        tracing::info!("Router service: {}", self.services.router_url);
        tracing::info!("Memory service: {}", self.services.memory_url);
        tracing::info!("Rewrite service: {}", self.services.rewrite_url);
        tracing::info!("Validation service: {}", self.services.validation_url);
    }
}

impl ServerConfig {
    fn from_env() -> Self {
        let host = env::var("SERVER_HOST").unwrap_or_else(|_| "0.0.0.0".to_string());
        let port = env::var("SERVER_PORT")
            .unwrap_or_else(|_| "3000".to_string())
            .parse::<u16>()
            .unwrap_or(3000);

        let bind_address = format!("{}:{}", host, port)
            .parse()
            .expect("Invalid SERVER_HOST or SERVER_PORT");

        let shutdown_timeout_secs = env::var("SERVER_SHUTDOWN_TIMEOUT")
            .unwrap_or_else(|_| "30".to_string())
            .parse::<u64>()
            .unwrap_or(30);

        Self {
            bind_address,
            shutdown_timeout_secs,
        }
    }

    pub(crate) fn try_from_source<E: EnvSource>(env: &E) -> Result<Self, ConfigError> {
        let host = env
            .get("SERVER_HOST")
            .unwrap_or_else(|| "0.0.0.0".to_string());
        if host.trim().is_empty() {
            return Err(ConfigError::invalid_bind_address());
        }
        let port_raw = env.get("SERVER_PORT").unwrap_or_else(|| "3000".to_string());
        let port = parse_u64("SERVER_PORT", &port_raw)?;
        if port > u64::from(u16::MAX) {
            return Err(ConfigError::invalid_bind_address());
        }
        let bind_address = format!("{host}:{port}")
            .parse()
            .map_err(|_| ConfigError::invalid_bind_address())?;

        let shutdown_timeout_secs = match env.get("SERVER_SHUTDOWN_TIMEOUT") {
            Some(raw) => {
                let value = parse_u64("SERVER_SHUTDOWN_TIMEOUT", &raw)?;
                check_bound(SHUTDOWN_TIMEOUT_SECS, value)?
            }
            None => SHUTDOWN_TIMEOUT_SECS.default,
        };

        Ok(Self {
            bind_address,
            shutdown_timeout_secs,
        })
    }
}

impl AuthConfig {
    fn from_env() -> Self {
        let development_mode = env::var("AUTH_DEVELOPMENT_MODE")
            .unwrap_or_else(|_| "false".to_string())
            .parse::<bool>()
            .unwrap_or(false);

        let dev_client_id = env::var("AUTH_DEV_CLIENT_ID")
            .unwrap_or_else(|_| "dev-client-123".to_string());

        let slow_operation_threshold_ms = env::var("AUTH_SLOW_THRESHOLD_MS")
            .unwrap_or_else(|_| "10".to_string())
            .parse::<u64>()
            .unwrap_or(10);

        Self {
            development_mode,
            dev_client_id,
            slow_operation_threshold_ms,
        }
    }

    pub(crate) fn from_source<E: EnvSource>(env: &E) -> Self {
        let development_mode = env
            .get("AUTH_DEVELOPMENT_MODE")
            .unwrap_or_else(|| "false".to_string())
            .parse::<bool>()
            .unwrap_or(false);

        let dev_client_id = env
            .get("AUTH_DEV_CLIENT_ID")
            .unwrap_or_else(|| "dev-client-123".to_string());

        let slow_operation_threshold_ms = env
            .get("AUTH_SLOW_THRESHOLD_MS")
            .and_then(|value| value.parse().ok())
            .unwrap_or(10);

        Self {
            development_mode,
            dev_client_id,
            slow_operation_threshold_ms,
        }
    }
}

impl LoggingConfig {
    fn from_env() -> Self {
        let tracing = TracingConfig::from_env();

        Self {
            level: tracing.env_filter,
            json_format: tracing.format == LogFormat::Json,
        }
    }
}

impl ServiceConfig {
    fn from_env() -> Self {
        let router_url = env::var("ROUTER_SERVICE_URL")
            .unwrap_or_else(|_| "http://localhost:50051".to_string());

        let memory_url = env::var("MEMORY_SERVICE_URL")
            .unwrap_or_else(|_| "http://localhost:50052".to_string());

        let rewrite_url = env::var("REWRITE_SERVICE_URL")
            .unwrap_or_else(|_| "http://localhost:50053".to_string());

        let validation_url = env::var("VALIDATION_SERVICE_URL")
            .unwrap_or_else(|_| "http://localhost:50054".to_string());

        let connection_timeout_ms = env::var("GRPC_CONNECTION_TIMEOUT_MS")
            .unwrap_or_else(|_| "5000".to_string())
            .parse::<u64>()
            .unwrap_or(5000);

        let request_timeout_ms = env::var("GRPC_REQUEST_TIMEOUT_MS")
            .unwrap_or_else(|_| "30000".to_string())
            .parse::<u64>()
            .unwrap_or(30000);

        Self {
            router_url,
            memory_url,
            rewrite_url,
            validation_url,
            connection_timeout_ms,
            request_timeout_ms,
        }
    }
}

/// Global configuration instance
static CONFIG: OnceLock<Config> = OnceLock::new();

/// Get global configuration
/// Initializes from environment variables on first call
pub fn get_config() -> &'static Config {
    CONFIG.get_or_init(|| {
        let config = Config::from_env();
        config.validate();
        config
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_config() {
        let config = Config::default();
        assert_eq!(config.server.bind_address.to_string(), "0.0.0.0:3000");
        assert!(!config.auth.development_mode);
        assert_eq!(config.logging.level, "info");
        assert!(config.logging.json_format);
    }

    #[test]
    fn test_server_config_defaults() {
        let config = ServerConfig::default();
        assert_eq!(config.bind_address.to_string(), "0.0.0.0:3000");
        assert_eq!(config.shutdown_timeout_secs, 30);
    }

    #[test]
    fn test_auth_config_defaults() {
        let config = AuthConfig::default();
        assert!(!config.development_mode);
        assert_eq!(config.dev_client_id, "dev-client-123");
        assert_eq!(config.slow_operation_threshold_ms, 10);
    }

    #[test]
    fn test_logging_config_defaults() {
        let config = LoggingConfig::default();
        assert_eq!(config.level, "info");
        assert!(config.json_format);
    }

    #[test]
    fn test_service_config_defaults() {
        let config = ServiceConfig::default();
        assert_eq!(config.router_url, "http://localhost:50051");
        assert_eq!(config.memory_url, "http://localhost:50052");
        assert_eq!(config.rewrite_url, "http://localhost:50053");
        assert_eq!(config.validation_url, "http://localhost:50054");
        assert_eq!(config.connection_timeout_ms, 5000);
        assert_eq!(config.request_timeout_ms, 30000);
    }
}

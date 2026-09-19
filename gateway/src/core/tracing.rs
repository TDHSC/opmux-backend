//! Tracing and logging configuration.
//!
//! Provides configurable structured logging with environment-based settings
//! for production and development environments.

use tracing_subscriber::{fmt, layer::SubscriberExt, util::SubscriberInitExt, EnvFilter};

/// Log output format.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogFormat {
    /// JSON format for production (log aggregation).
    Json,
    /// Pretty format for development (human readability).
    Pretty,
}

impl LogFormat {
    /// Parses log format from environment variable.
    ///
    /// # Parameters
    /// - `value` - Environment variable value ("json" or "pretty")
    ///
    /// # Returns
    /// LogFormat enum value, defaults to Json if invalid
    pub fn from_env(value: &str) -> Self {
        match value.to_lowercase().as_str() {
            "pretty" => Self::Pretty,
            "json" => Self::Json,
            _ => {
                tracing::warn!(
                    value = value,
                    "Invalid LOG_FORMAT value, defaulting to 'json'"
                );
                Self::Json
            }
        }
    }
}

/// Tracing configuration with environment-based settings.
///
/// # Environment Variables
///
/// - `RUST_LOG`: Log level filter (default: "info")
///   - Examples: "info", "debug", "trace", "gateway=debug"
///   - Falls back to `LOG_LEVEL` when unset
///
/// - `LOG_FORMAT`: Output format (default: "json")
///   - "json": JSON format for production
///   - "pretty": Pretty format for development
///   - Falls back to `LOG_JSON` when unset (`true` for JSON, `false` for pretty)
///
/// - `LOG_VERBOSE_DEBUG`: Enable expensive logging options (default: "false")
///   - "true" or "1": Enable line numbers, thread IDs
///   - "false" or "0": Disable expensive options
///
/// # Performance Impact
///
/// | Option | Production | Development | Overhead |
/// |--------|-----------|-------------|----------|
/// | `with_line_number` | ❌ | ✅ | ~10-20% |
/// | `with_thread_ids` | ❌ | ✅ | ~5-10% |
/// | JSON format | ✅ | ❌ | ~2-5% |
/// | Pretty format | ❌ | ✅ | ~5-10% |
#[derive(Debug, Clone)]
pub struct TracingConfig {
    /// Environment filter for log levels.
    pub env_filter: String,

    /// Log output format.
    pub format: LogFormat,

    /// Include line numbers in logs (expensive in production).
    pub with_line_number: bool,

    /// Include thread IDs in logs (expensive in production).
    pub with_thread_ids: bool,

    /// Include target module path in logs.
    pub with_target: bool,
}

impl TracingConfig {
    /// Creates TracingConfig from environment variables.
    ///
    /// # Environment Variables
    /// - `RUST_LOG`: Log level, falling back to `LOG_LEVEL` (default: "info")
    /// - `LOG_FORMAT`: "json" or "pretty", falling back to `LOG_JSON` (default: "json")
    /// - `LOG_VERBOSE_DEBUG`: "true" or "false" (default: "false")
    ///
    /// # Returns
    /// TracingConfig with settings from environment
    pub fn from_env() -> Self {
        let env_filter = std::env::var("RUST_LOG")
            .or_else(|_| std::env::var("LOG_LEVEL"))
            .unwrap_or_else(|_| "info".to_string());

        let format = std::env::var("LOG_FORMAT")
            .map(|v| LogFormat::from_env(&v))
            .unwrap_or_else(|_| match std::env::var("LOG_JSON").as_deref() {
                Ok("true") | Err(_) => LogFormat::Json,
                Ok(_) => LogFormat::Pretty,
            });

        let verbose_debug = std::env::var("LOG_VERBOSE_DEBUG")
            .map(|v| v == "true" || v == "1")
            .unwrap_or(false);

        Self {
            env_filter,
            format,
            with_line_number: verbose_debug,
            with_thread_ids: verbose_debug,
            with_target: true,
        }
    }

    /// Creates production configuration.
    ///
    /// - Log level: info
    /// - Format: JSON
    /// - Verbose debug: disabled
    pub fn production() -> Self {
        Self {
            env_filter: "info".to_string(),
            format: LogFormat::Json,
            with_line_number: false,
            with_thread_ids: false,
            with_target: true,
        }
    }

    /// Creates development configuration.
    ///
    /// - Log level: debug
    /// - Format: Pretty
    /// - Verbose debug: enabled
    pub fn development() -> Self {
        Self {
            env_filter: "debug,gateway=trace".to_string(),
            format: LogFormat::Pretty,
            with_line_number: true,
            with_thread_ids: true,
            with_target: true,
        }
    }
}

/// Initializes tracing subscriber with configuration.
///
/// This should be called once at application startup before any logging occurs.
///
/// # Parameters
/// - `config` - Tracing configuration
///
/// # Example
///
/// ```rust,ignore
/// use gateway::core::tracing::{TracingConfig, init_tracing};
///
/// #[tokio::main]
/// async fn main() {
///     let config = TracingConfig::from_env();
///     init_tracing(config);
///
///     tracing::info!("Application started");
/// }
/// ```
pub fn init_tracing(config: TracingConfig) {
    let env_filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new(&config.env_filter));

    match config.format {
        LogFormat::Json => {
            let fmt_layer = fmt::layer()
                .json()
                .with_target(config.with_target)
                .with_line_number(config.with_line_number)
                .with_thread_ids(config.with_thread_ids);

            tracing_subscriber::registry()
                .with(env_filter)
                .with(fmt_layer)
                .init();
        }
        LogFormat::Pretty => {
            let fmt_layer = fmt::layer()
                .pretty()
                .with_target(config.with_target)
                .with_line_number(config.with_line_number)
                .with_thread_ids(config.with_thread_ids);

            tracing_subscriber::registry()
                .with(env_filter)
                .with(fmt_layer)
                .init();
        }
    }

    tracing::info!(
        log_level = %config.env_filter,
        format = ?config.format,
        verbose_debug = config.with_line_number,
        "Tracing initialized"
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_log_format_from_env() {
        assert_eq!(LogFormat::from_env("json"), LogFormat::Json);
        assert_eq!(LogFormat::from_env("JSON"), LogFormat::Json);
        assert_eq!(LogFormat::from_env("pretty"), LogFormat::Pretty);
        assert_eq!(LogFormat::from_env("PRETTY"), LogFormat::Pretty);
        assert_eq!(LogFormat::from_env("invalid"), LogFormat::Json); // Default
    }

    #[test]
    fn test_tracing_config_production() {
        let config = TracingConfig::production();

        assert_eq!(config.env_filter, "info");
        assert_eq!(config.format, LogFormat::Json);
        assert!(!config.with_line_number);
        assert!(!config.with_thread_ids);
        assert!(config.with_target);
    }

    #[test]
    fn test_tracing_config_development() {
        let config = TracingConfig::development();

        assert_eq!(config.env_filter, "debug,gateway=trace");
        assert_eq!(config.format, LogFormat::Pretty);
        assert!(config.with_line_number);
        assert!(config.with_thread_ids);
        assert!(config.with_target);
    }
}

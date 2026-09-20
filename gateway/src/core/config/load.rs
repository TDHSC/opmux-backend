//! Canonical settings loader: catalog file plus process environment.

use std::collections::HashMap;
use std::fmt;
use std::path::{Path, PathBuf};

use super::catalog::{parse_catalog, Catalog};
use super::env::{EnvSource, ProcessEnv};
use super::error::{ConfigCategory, ConfigError};
use super::http_client::build_bounded_http_client_for_base_url;
use super::limits::PolicyLimits;
use super::process::{AuthConfig, ServerConfig};
use super::provider::ProviderSettings;

/// Injected operator configuration used by the binary and tests.
///
/// `Debug` is opaque and never dumps credentials, URLs, or catalog contents.
pub struct Settings {
    /// Bind address and shutdown grace.
    pub server: ServerConfig,
    /// Authentication process settings.
    pub auth: AuthConfig,
    /// Validated route/target/pricing catalog.
    pub catalog: Catalog,
    /// Validated policy limits for later execution and admission features.
    pub limits: PolicyLimits,
    /// Environment-only provider credential and base URL.
    pub provider: ProviderSettings,
    catalog_path: PathBuf,
}

impl fmt::Debug for Settings {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Settings")
    }
}

impl Settings {
    /// Loads settings from the process environment and `OPMUX_CONFIG_FILE`.
    pub fn load() -> Result<Self, ConfigError> {
        Self::load_with(&ProcessEnv)
    }

    /// Loads settings from an explicit catalog path and environment map.
    ///
    /// The environment map is the complete source; process environment is not
    /// inherited. Tests must set dummy credentials and loopback URLs here.
    pub fn load_from(
        catalog_path: impl AsRef<Path>,
        env: &HashMap<String, String>,
    ) -> Result<Self, ConfigError> {
        Self::from_catalog_path(catalog_path.as_ref(), env)
    }

    /// Local dummy settings aimed at an explicit loopback provider.
    ///
    /// # Parameters
    /// - `base_url` - Loopback OpenAI-compatible base URL
    /// - `api_key` - Dummy provider credential
    ///
    /// # Returns
    /// Test settings with the provided provider endpoint
    pub fn for_tests_with_provider(
        base_url: impl Into<String>,
        api_key: impl Into<String>,
    ) -> Self {
        let mut settings = Self::for_tests();
        let parsed = super::provider::validate_provider_url(&base_url.into())
            .expect("test provider URL must be a valid http(s) URL");
        settings.provider = super::provider::ProviderSettings {
            api_key: super::provider::SecretString::new(api_key),
            base_url: parsed.as_str().trim_end_matches('/').to_string(),
        };
        settings
    }

    /// Local dummy settings for tests that need `AppState` but not catalog I/O.
    ///
    /// Includes two named routes with distinct primary models so ingress
    /// routing tests can select `default` or `fast`.
    pub fn for_tests() -> Self {
        let mut targets = HashMap::new();
        targets.insert(
            "primary".to_string(),
            super::catalog::Target {
                vendor: super::catalog::VendorKind::Openai,
                model: "example-chat-model".to_string(),
                max_output_tokens: 512,
                pricing: super::catalog::TargetPricing {
                    input_per_million: 1.0,
                    output_per_million: 2.0,
                },
            },
        );
        targets.insert(
            "secondary".to_string(),
            super::catalog::Target {
                vendor: super::catalog::VendorKind::Openai,
                model: "example-chat-model-mini".to_string(),
                max_output_tokens: 256,
                pricing: super::catalog::TargetPricing {
                    input_per_million: 0.25,
                    output_per_million: 0.5,
                },
            },
        );
        let mut routes = HashMap::new();
        routes.insert(
            "default".to_string(),
            super::catalog::Route {
                primary: "primary".to_string(),
                fallbacks: Vec::new(),
            },
        );
        routes.insert(
            "fast".to_string(),
            super::catalog::Route {
                primary: "secondary".to_string(),
                fallbacks: Vec::new(),
            },
        );
        Self {
            server: ServerConfig {
                bind_address: "127.0.0.1:3000".parse().expect("static test bind address"),
                shutdown_timeout_secs: 30,
            },
            auth: AuthConfig::default(),
            catalog: Catalog {
                version: 1,
                default_route: "default".to_string(),
                targets,
                routes,
            },
            limits: PolicyLimits::documented_defaults(),
            provider: ProviderSettings::dummy_local(),
            catalog_path: PathBuf::from("test://catalog"),
        }
    }

    fn load_with<E: EnvSource>(env: &E) -> Result<Self, ConfigError> {
        let path = match env.get("OPMUX_CONFIG_FILE") {
            Some(value) if !value.trim().is_empty() => PathBuf::from(value),
            _ => return Err(ConfigError::missing_config_file()),
        };
        Self::from_catalog_path(&path, env)
    }

    fn from_catalog_path<E: EnvSource>(
        catalog_path: &Path,
        env: &E,
    ) -> Result<Self, ConfigError> {
        let json = std::fs::read_to_string(catalog_path)
            .map_err(|_| ConfigError::catalog_unreadable())?;

        let parsed = parse_catalog(&json)?;
        let limits = PolicyLimits::from_raw(parsed.raw_limits.as_ref())?
            .apply_env_overrides(env)?;
        let catalog = apply_limit_to_routes(parsed.catalog, limits.max_fallback_targets)?;
        let provider = ProviderSettings::from_env(env)?;
        let server = ServerConfig::try_from_source(env)?;
        let auth = AuthConfig::from_source(env);

        let settings = Self {
            server,
            auth,
            catalog,
            limits,
            provider,
            catalog_path: catalog_path.to_path_buf(),
        };
        // Fail closed if the bounded client cannot be built. Do not start
        // with an unbounded default client.
        let _ = settings.build_provider_http_client()?;
        Ok(settings)
    }

    /// Path of the catalog file that was loaded.
    pub fn catalog_path(&self) -> &Path {
        &self.catalog_path
    }

    /// Constructs the bounded provider HTTP client from validated limits.
    pub fn build_provider_http_client(&self) -> Result<reqwest::Client, ConfigError> {
        build_bounded_http_client_for_base_url(
            self.limits.max_attempt_timeout,
            &self.provider.base_url,
        )
    }

    /// Logs a secret-free summary of validated settings.
    pub fn log_safe_summary(&self) {
        tracing::info!(
            bind_address = %self.server.bind_address,
            default_route = %self.catalog.default_route,
            target_count = self.catalog.targets.len(),
            route_count = self.catalog.routes.len(),
            protected_request_deadline_ms = self.limits.protected_request_deadline_ms(),
            max_attempt_timeout_ms = self.limits.max_attempt_timeout_ms(),
            retries_per_target = self.limits.retries_per_target,
            max_total_attempts = self.limits.max_total_attempts,
            max_fallback_targets = self.limits.max_fallback_targets,
            backoff_cap_ms = self.limits.backoff_cap_ms(),
            "Loaded operator configuration"
        );
        tracing::info!(
            "Policy limits are validated at startup; protected-request deadline is enforced, while fallback, circuit, and admission enforcement land in later milestones"
        );
        if self.auth.development_mode {
            tracing::info!(
                "AUTH_DEVELOPMENT_MODE has no effect; persisted authentication is required"
            );
        }
        tracing::info!("Persisted authentication is required");
    }
}

fn apply_limit_to_routes(
    catalog: Catalog,
    max_fallback_targets: u32,
) -> Result<Catalog, ConfigError> {
    for (name, route) in &catalog.routes {
        if route.fallbacks.len() > max_fallback_targets as usize {
            return Err(ConfigError::new(
                ConfigCategory::CatalogFallbackLimit,
                format!("route {name} fallback list exceeds max_fallback_targets"),
            ));
        }
    }
    Ok(catalog)
}

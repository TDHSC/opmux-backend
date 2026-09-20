//! Configuration for Executor Layer.

use std::collections::HashMap;
use std::env;
use std::fmt;

use crate::core::config::{Settings, MAX_UPSTREAM_RESPONSE_BYTES};

/// Model pricing information for a selected target.
///
/// Values are USD per million tokens. Catalog example prices are illustrative
/// samples, not current provider billing facts.
#[derive(Debug, Clone)]
pub struct ModelPricing {
    /// Prompt / input price per million tokens in USD
    pub input_per_million: f64,
    /// Completion / output price per million tokens in USD
    pub output_per_million: f64,
}

impl ModelPricing {
    /// Creates target pricing in USD per million tokens.
    pub fn new(input_per_million: f64, output_per_million: f64) -> Self {
        Self {
            input_per_million,
            output_per_million,
        }
    }
}

/// OpenAI vendor configuration.
#[derive(Clone)]
pub struct OpenAIConfig {
    /// API key for authentication
    pub api_key: String,
    /// Base URL for API endpoint
    pub base_url: String,
    /// Request timeout in milliseconds
    pub timeout_ms: u64,
    /// List of supported model IDs
    pub supported_models: Vec<String>,
    /// Pricing keyed by catalog target identity, not provider model string.
    pub pricing: HashMap<String, ModelPricing>,
    /// Maximum upstream success body size in bytes
    pub max_response_bytes: u64,
}

impl fmt::Debug for OpenAIConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("OpenAIConfig")
            .field("api_key", &"[redacted]")
            .field("base_url", &"[omitted]")
            .field("timeout_ms", &self.timeout_ms)
            .field("supported_models", &self.supported_models)
            .field("max_response_bytes", &self.max_response_bytes)
            .finish_non_exhaustive()
    }
}

impl OpenAIConfig {
    /// Loads OpenAI configuration from environment variables.
    ///
    /// # Environment Variables
    /// - `OPENAI_API_KEY` - API key (required)
    /// - `OPENAI_BASE_URL` - Base URL (default: https://api.openai.com/v1)
    /// - `OPENAI_TIMEOUT_MS` - Request timeout (default: 30000)
    pub fn from_env() -> Self {
        let api_key = env::var("OPENAI_API_KEY").unwrap_or_default();
        let base_url = env::var("OPENAI_BASE_URL")
            .unwrap_or_else(|_| "https://api.openai.com/v1".to_string());
        let timeout_ms = env::var("OPENAI_TIMEOUT_MS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(30000);

        // Sample prices only. Production uses catalog target prices via
        // `from_settings`; these are not current provider billing facts.
        let mut pricing = HashMap::new();
        pricing.insert("gpt-4".to_string(), ModelPricing::new(1.0, 2.0));
        pricing.insert("gpt-4-turbo".to_string(), ModelPricing::new(1.0, 2.0));
        pricing.insert("gpt-3.5-turbo".to_string(), ModelPricing::new(1.0, 2.0));

        Self {
            api_key,
            base_url,
            timeout_ms,
            supported_models: vec![
                "gpt-4".to_string(),
                "gpt-4-turbo".to_string(),
                "gpt-3.5-turbo".to_string(),
            ],
            pricing,
            max_response_bytes: MAX_UPSTREAM_RESPONSE_BYTES.default,
        }
    }

    /// Dummy local-only configuration for tests.
    pub fn for_tests() -> Self {
        let mut pricing = HashMap::new();
        pricing.insert("primary".to_string(), ModelPricing::new(1.0, 2.0));
        pricing.insert("secondary".to_string(), ModelPricing::new(0.25, 0.5));
        pricing.insert(
            "example-chat-model".to_string(),
            ModelPricing::new(1.0, 2.0),
        );
        pricing.insert(
            "example-chat-model-mini".to_string(),
            ModelPricing::new(0.25, 0.5),
        );
        pricing.insert("gpt-4".to_string(), ModelPricing::new(1.0, 2.0));
        pricing.insert("gpt-4-turbo".to_string(), ModelPricing::new(1.0, 2.0));
        pricing.insert("gpt-3.5-turbo".to_string(), ModelPricing::new(1.0, 2.0));

        Self {
            api_key: "test-dummy-openai-key".to_string(),
            base_url: "http://127.0.0.1:9/v1".to_string(),
            timeout_ms: 200,
            supported_models: vec![
                "example-chat-model".to_string(),
                "example-chat-model-mini".to_string(),
                "gpt-4".to_string(),
                "gpt-4-turbo".to_string(),
                "gpt-3.5-turbo".to_string(),
            ],
            pricing,
            max_response_bytes: MAX_UPSTREAM_RESPONSE_BYTES.default,
        }
    }

    /// Validates the configuration.
    pub fn validate(&self) -> Result<(), String> {
        if self.api_key.is_empty() {
            return Err("OPENAI_API_KEY is not set or empty".to_string());
        }
        if self.supported_models.is_empty() {
            return Err("No supported models configured".to_string());
        }
        Ok(())
    }
}

/// Configuration for Executor Layer.
#[derive(Clone)]
pub struct ExecutorConfig {
    /// OpenAI vendor configuration
    pub openai: Option<OpenAIConfig>,
    /// Anthropic API key (future)
    pub anthropic_api_key: Option<String>,
    /// Per-attempt timeout in milliseconds, capped by remaining deadline
    pub timeout_ms: u64,
    /// Maximum retries after the first attempt on one target
    pub max_retries: u32,
    /// Maximum actual provider calls across primary and fallback hops
    pub max_total_attempts: u32,
    /// Exponential full-jitter cap in milliseconds
    pub backoff_cap_ms: u64,
}

impl fmt::Debug for ExecutorConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ExecutorConfig")
            .field("openai", &self.openai)
            .field(
                "anthropic_api_key",
                &self.anthropic_api_key.as_ref().map(|_| "[redacted]"),
            )
            .field("timeout_ms", &self.timeout_ms)
            .field("max_retries", &self.max_retries)
            .field("max_total_attempts", &self.max_total_attempts)
            .field("backoff_cap_ms", &self.backoff_cap_ms)
            .finish()
    }
}

impl ExecutorConfig {
    /// Builds executor configuration from validated injected settings.
    pub fn from_settings(settings: &Settings) -> Self {
        let mut pricing = HashMap::new();
        let mut supported_models = Vec::new();
        for (target_id, target) in &settings.catalog.targets {
            if !supported_models.contains(&target.model) {
                supported_models.push(target.model.clone());
            }
            pricing.insert(
                target_id.clone(),
                ModelPricing::new(
                    target.pricing.input_per_million,
                    target.pricing.output_per_million,
                ),
            );
        }

        Self {
            openai: Some(OpenAIConfig {
                api_key: settings.provider.api_key.expose().to_string(),
                base_url: settings.provider.base_url.clone(),
                timeout_ms: settings.limits.max_attempt_timeout_ms(),
                supported_models,
                pricing,
                max_response_bytes: settings.limits.max_upstream_response_bytes,
            }),
            anthropic_api_key: None,
            timeout_ms: settings.limits.max_attempt_timeout_ms(),
            max_retries: settings.limits.retries_per_target,
            max_total_attempts: settings.limits.max_total_attempts,
            backoff_cap_ms: settings.limits.backoff_cap_ms(),
        }
    }

    /// Policy used by mock executor services in unit tests.
    ///
    /// `max_total_attempts` allows the configured per-target retries so those
    /// tests are not silently capped by the production global budget.
    pub(crate) fn mock_policy(max_retries: u32, timeout_ms: u64) -> Self {
        Self {
            openai: None,
            anthropic_api_key: None,
            timeout_ms,
            max_retries,
            max_total_attempts: max_retries.saturating_add(1).max(1),
            backoff_cap_ms: 2_000,
        }
    }

    /// Loads configuration from environment variables.
    pub fn from_env() -> Self {
        // Load OpenAI configuration if API key is set
        let openai = if env::var("OPENAI_API_KEY").is_ok() {
            Some(OpenAIConfig::from_env())
        } else {
            None
        };

        let anthropic_api_key = env::var("ANTHROPIC_API_KEY").ok();

        let timeout_ms = env::var("EXECUTOR_TIMEOUT_MS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(30000); // Default: 30 seconds

        let max_retries = env::var("EXECUTOR_MAX_RETRIES")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(3); // Default: 3 retries

        let max_total_attempts = env::var("OPMUX_MAX_TOTAL_ATTEMPTS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(3);

        let backoff_cap_ms = env::var("OPMUX_BACKOFF_CAP_MS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(2_000);

        Self {
            openai,
            anthropic_api_key,
            timeout_ms,
            max_retries,
            max_total_attempts,
            backoff_cap_ms,
        }
    }

    /// Validates configuration.
    pub fn validate(&self) {
        if self.openai.is_none() && self.anthropic_api_key.is_none() {
            tracing::warn!(
                "No LLM vendor API keys configured. Set OPENAI_API_KEY or ANTHROPIC_API_KEY."
            );
        }

        if let Some(ref config) = self.openai {
            if let Err(e) = config.validate() {
                tracing::warn!("OpenAI configuration invalid: {}", e);
            } else {
                tracing::info!(
                    "OpenAI vendor configured with {} models",
                    config.supported_models.len()
                );
            }
        }

        if let Some(ref key) = self.anthropic_api_key {
            if key.is_empty() {
                tracing::warn!("ANTHROPIC_API_KEY is empty");
            } else {
                tracing::info!("Anthropic vendor configured");
            }
        }

        tracing::info!("Executor timeout: {}ms", self.timeout_ms);
        tracing::info!("Executor max retries: {}", self.max_retries);
        tracing::info!("Executor max total attempts: {}", self.max_total_attempts);
        tracing::info!("Executor backoff cap: {}ms", self.backoff_cap_ms);
    }
}

impl Default for ExecutorConfig {
    fn default() -> Self {
        Self::mock_policy(3, 30_000)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::config::Settings;

    #[test]
    fn from_settings_uses_catalog_models_only() {
        let settings = Settings::for_tests();
        let config = ExecutorConfig::from_settings(&settings);
        let openai = config.openai.expect("openai config");
        assert!(openai
            .supported_models
            .contains(&"example-chat-model".to_string()));
        assert!(openai
            .supported_models
            .contains(&"example-chat-model-mini".to_string()));
        assert!(!openai.supported_models.contains(&"gpt-4".to_string()));
        assert_eq!(openai.base_url, "http://127.0.0.1:9/v1");
        assert_eq!(openai.api_key, "test-dummy-openai-key");
        let primary = openai
            .pricing
            .get("primary")
            .expect("primary target prices");
        assert_eq!(primary.input_per_million, 1.0);
        assert_eq!(primary.output_per_million, 2.0);
        let secondary = openai
            .pricing
            .get("secondary")
            .expect("secondary target prices");
        assert_eq!(secondary.input_per_million, 0.25);
        assert_eq!(secondary.output_per_million, 0.5);
        assert!(!openai.pricing.contains_key("example-chat-model"));
        assert!(!openai.pricing.contains_key("gpt-4"));
        assert_eq!(
            openai.max_response_bytes,
            settings.limits.max_upstream_response_bytes
        );
        assert_eq!(config.timeout_ms, 10_000);
        assert_eq!(config.max_retries, 1);
        assert_eq!(config.max_total_attempts, 3);
        assert_eq!(config.backoff_cap_ms, 2_000);
    }

    #[test]
    fn from_settings_keeps_distinct_prices_for_same_requested_model() {
        let mut settings = Settings::for_tests();
        let shared_model = settings
            .catalog
            .targets
            .get("primary")
            .expect("primary target")
            .model
            .clone();
        settings.catalog.targets.insert(
            "same-model-alt".to_string(),
            crate::core::config::Target {
                vendor: crate::core::config::VendorKind::Openai,
                model: shared_model.clone(),
                max_output_tokens: 512,
                pricing: crate::core::config::TargetPricing {
                    input_per_million: 10.0,
                    output_per_million: 20.0,
                },
            },
        );

        let openai = ExecutorConfig::from_settings(&settings)
            .openai
            .expect("openai config");
        assert_eq!(
            openai
                .supported_models
                .iter()
                .filter(|model| *model == &shared_model)
                .count(),
            1
        );
        let primary = openai
            .pricing
            .get("primary")
            .expect("primary target prices");
        assert_eq!(primary.input_per_million, 1.0);
        assert_eq!(primary.output_per_million, 2.0);
        let alt = openai
            .pricing
            .get("same-model-alt")
            .expect("same-model alt target prices");
        assert_eq!(alt.input_per_million, 10.0);
        assert_eq!(alt.output_per_million, 20.0);
        assert_ne!(
            (primary.input_per_million, primary.output_per_million),
            (alt.input_per_million, alt.output_per_million)
        );
    }
}

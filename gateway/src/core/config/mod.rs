//! Canonical injected configuration.
//!
//! Loads a non-secret JSON catalog from `OPMUX_CONFIG_FILE` and merges
//! environment credentials, bind settings, and optional limit overrides.
//! Copying `.env` is not process configuration. Example catalog prices and
//! model names are illustrative, not current billing or availability facts.
//!
//! Database URLs are not required in this milestone.

mod catalog;
mod env;
mod error;
mod http_client;
mod limits;
mod load;
mod process;
mod provider;

#[cfg(test)]
mod tests;

pub use catalog::{Catalog, Route, Target, TargetPricing, VendorKind};
pub use error::{ConfigCategory, ConfigError};
pub use http_client::{
    build_bounded_http_client, build_bounded_http_client_for_base_url,
    client_from_build_result,
};
pub use limits::{
    LimitBound, PolicyLimits, BACKOFF_CAP_MS, CIRCUIT_COOLDOWN_MS,
    CIRCUIT_FAILURE_THRESHOLD, MAX_ATTEMPT_TIMEOUT_MS, MAX_CONCURRENT_GENERATIONS,
    MAX_FALLBACK_TARGETS, MAX_METADATA_BYTES, MAX_OUTPUT_TOKENS, MAX_PROMPT_CHARS,
    MAX_REQUEST_BODY_BYTES, MAX_TOTAL_ATTEMPTS, MAX_UPSTREAM_RESPONSE_BYTES,
    PROTECTED_REQUEST_DEADLINE_MS, RETRIES_PER_TARGET,
};
pub use load::Settings;
pub use process::{
    get_config, AuthConfig, Config, LoggingConfig, ServerConfig, ServiceConfig,
};
pub use provider::{ProviderSettings, SecretString};

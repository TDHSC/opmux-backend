//! Environment-only provider credentials and base URL.

use super::env::EnvSource;
use super::error::ConfigError;
use std::fmt;

/// Secret value that never prints its contents.
#[derive(Clone)]
pub struct SecretString(String);

impl SecretString {
    /// Wraps a secret string.
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    /// Returns the secret for authorized runtime use.
    pub fn expose(&self) -> &str {
        &self.0
    }

    /// Returns true when the secret is missing or whitespace.
    pub fn is_blank(&self) -> bool {
        self.0.trim().is_empty()
    }
}

impl fmt::Debug for SecretString {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("[redacted]")
    }
}

impl fmt::Display for SecretString {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("[redacted]")
    }
}

/// Validated OpenAI provider settings from process environment.
#[derive(Clone)]
pub struct ProviderSettings {
    /// Provider credential. Environment-only; never logged.
    pub api_key: SecretString,
    /// Provider base URL without embedded credentials.
    pub base_url: String,
}

impl ProviderSettings {
    /// Local dummy credential and loopback URL for tests.
    pub fn dummy_local() -> Self {
        Self {
            api_key: SecretString::new("test-dummy-openai-key"),
            base_url: "http://127.0.0.1:9/v1".to_string(),
        }
    }

    pub(crate) fn from_env<E: EnvSource>(env: &E) -> Result<Self, ConfigError> {
        let api_key = match env.get("OPENAI_API_KEY") {
            Some(value) => SecretString::new(value),
            None => return Err(ConfigError::missing_credential()),
        };
        if api_key.is_blank() {
            return Err(ConfigError::missing_credential());
        }

        let base_url = env
            .get("OPENAI_BASE_URL")
            .unwrap_or_else(|| "https://api.openai.com/v1".to_string());
        let parsed = validate_provider_url(&base_url)?;

        Ok(Self {
            api_key,
            base_url: parsed.as_str().trim_end_matches('/').to_string(),
        })
    }
}

pub(crate) fn validate_provider_url(raw: &str) -> Result<reqwest::Url, ConfigError> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(ConfigError::invalid_provider_url());
    }
    let url =
        reqwest::Url::parse(trimmed).map_err(|_| ConfigError::invalid_provider_url())?;
    if url.scheme() != "http" && url.scheme() != "https" {
        return Err(ConfigError::invalid_provider_url());
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err(ConfigError::invalid_provider_url());
    }
    if url.host_str().is_none() {
        return Err(ConfigError::invalid_provider_url());
    }
    Ok(url)
}

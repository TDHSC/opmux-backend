//! Sanitized configuration error categories.

use std::fmt;

/// Stable configuration failure category.
///
/// These strings are safe to log and assert in tests. They never include
/// credentials, credential-bearing URLs, or a full configuration dump.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfigCategory {
    /// `OPMUX_CONFIG_FILE` is missing or blank.
    MissingConfigFile,
    /// Catalog path cannot be read.
    CatalogUnreadable,
    /// Catalog bytes are not valid JSON.
    CatalogMalformedJson,
    /// Catalog `version` is missing or not `1`.
    CatalogUnsupportedVersion,
    /// Catalog contains a field outside the canonical schema.
    CatalogUnknownField,
    /// Raw JSON defines the same target identifier more than once.
    CatalogDuplicateTarget,
    /// Raw JSON defines the same route identifier more than once.
    CatalogDuplicateRoute,
    /// `default_route` is missing from `routes`.
    CatalogMissingDefaultRoute,
    /// A route primary or fallback does not name a configured target.
    CatalogUnknownTargetReference,
    /// A required identifier, primary, or model is empty.
    CatalogEmptyIdentifier,
    /// Route chain is nested or recursive instead of a flat target list.
    CatalogNestedChain,
    /// Primary or fallback list repeats a target.
    CatalogDuplicateChainTarget,
    /// Fallback list exceeds the configured maximum.
    CatalogFallbackLimit,
    /// Target vendor is missing, empty, or not `openai`.
    CatalogUnsupportedVendor,
    /// Pricing object is missing a required field.
    CatalogIncompletePricing,
    /// A price is negative, non-finite, or above the documented bound.
    CatalogInvalidPrice,
    /// A catalog field has the wrong JSON type.
    CatalogInvalidType,
    /// A numeric limit is missing, zero where forbidden, fractional, or out of range.
    InvalidLimit,
    /// Required provider credential is missing or blank.
    MissingCredential,
    /// Provider base URL is missing, unusable, or credential-bearing.
    InvalidProviderUrl,
    /// Bounded HTTP client construction failed.
    HttpClientConstruction,
    /// Server bind host/port cannot be used.
    InvalidBindAddress,
}

impl ConfigCategory {
    /// Returns the stable diagnostic token.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::MissingConfigFile => "missing_config_file",
            Self::CatalogUnreadable => "catalog_unreadable",
            Self::CatalogMalformedJson => "catalog_malformed_json",
            Self::CatalogUnsupportedVersion => "catalog_unsupported_version",
            Self::CatalogUnknownField => "catalog_unknown_field",
            Self::CatalogDuplicateTarget => "catalog_duplicate_target",
            Self::CatalogDuplicateRoute => "catalog_duplicate_route",
            Self::CatalogMissingDefaultRoute => "catalog_missing_default_route",
            Self::CatalogUnknownTargetReference => "catalog_unknown_target_reference",
            Self::CatalogEmptyIdentifier => "catalog_empty_identifier",
            Self::CatalogNestedChain => "catalog_nested_chain",
            Self::CatalogDuplicateChainTarget => "catalog_duplicate_chain_target",
            Self::CatalogFallbackLimit => "catalog_fallback_limit",
            Self::CatalogUnsupportedVendor => "catalog_unsupported_vendor",
            Self::CatalogIncompletePricing => "catalog_incomplete_pricing",
            Self::CatalogInvalidPrice => "catalog_invalid_price",
            Self::CatalogInvalidType => "catalog_invalid_type",
            Self::InvalidLimit => "invalid_limit",
            Self::MissingCredential => "missing_credential",
            Self::InvalidProviderUrl => "invalid_provider_url",
            Self::HttpClientConstruction => "http_client_construction",
            Self::InvalidBindAddress => "invalid_bind_address",
        }
    }
}

/// Configuration failure that is safe to print.
#[derive(Debug)]
pub struct ConfigError {
    category: ConfigCategory,
    message: String,
}

impl ConfigError {
    /// Builds a sanitized configuration error.
    pub fn new(category: ConfigCategory, message: impl Into<String>) -> Self {
        Self {
            category,
            message: message.into(),
        }
    }

    /// Returns the stable diagnostic category.
    pub fn category(&self) -> &'static str {
        self.category.as_str()
    }

    /// Returns the typed category.
    pub fn kind(&self) -> ConfigCategory {
        self.category
    }

    pub(crate) fn missing_config_file() -> Self {
        Self::new(
            ConfigCategory::MissingConfigFile,
            "OPMUX_CONFIG_FILE must point to a non-secret JSON catalog",
        )
    }

    pub(crate) fn catalog_unreadable() -> Self {
        Self::new(
            ConfigCategory::CatalogUnreadable,
            "operator catalog file cannot be read",
        )
    }

    pub(crate) fn invalid_limit(name: &str) -> Self {
        Self::new(
            ConfigCategory::InvalidLimit,
            format!("invalid numeric limit for {name}"),
        )
    }

    pub(crate) fn missing_credential() -> Self {
        Self::new(
            ConfigCategory::MissingCredential,
            "OPENAI_API_KEY is required and must be nonempty",
        )
    }

    pub(crate) fn invalid_provider_url() -> Self {
        Self::new(
            ConfigCategory::InvalidProviderUrl,
            "OPENAI_BASE_URL must be an http or https URL without embedded credentials",
        )
    }

    pub(crate) fn http_client_construction() -> Self {
        Self::new(
            ConfigCategory::HttpClientConstruction,
            "failed to construct the bounded provider HTTP client",
        )
    }

    pub(crate) fn invalid_bind_address() -> Self {
        Self::new(
            ConfigCategory::InvalidBindAddress,
            "SERVER_HOST and SERVER_PORT do not form a valid bind address",
        )
    }
}

impl fmt::Display for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.category.as_str(), self.message)
    }
}

impl std::error::Error for ConfigError {}

//! Version-1 operator catalog: targets, routes, and sample pricing.

use serde::de::{self, Deserializer, MapAccess, Visitor};
use serde::Deserialize;
use std::collections::HashMap;
use std::fmt;
use std::marker::PhantomData;

use super::error::{ConfigCategory, ConfigError};
use super::limits::{check_bound, RawLimits, MAX_OUTPUT_TOKENS, MAX_PRICE_PER_MILLION};

/// Allowed vendor identifier. OpenAI is the only supported vendor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VendorKind {
    /// OpenAI Chat Completions compatible provider.
    Openai,
}

impl VendorKind {
    /// Returns the canonical vendor identifier.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Openai => "openai",
        }
    }
}

/// Configured price for one target.
///
/// Values are USD per million tokens. Example catalog prices are illustrative
/// and are not current provider billing facts.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TargetPricing {
    /// Prompt / input price per million tokens.
    pub input_per_million: f64,
    /// Completion / output price per million tokens.
    pub output_per_million: f64,
}

/// Model-specific execution target sharing the operator provider credential.
#[derive(Debug, Clone, PartialEq)]
pub struct Target {
    /// Vendor used by this target.
    pub vendor: VendorKind,
    /// Provider model identifier.
    pub model: String,
    /// Inclusive output-token cap for this target.
    pub max_output_tokens: u32,
    /// Configured estimated prices for a successful response.
    pub pricing: TargetPricing,
}

/// Flat ordered route: one primary target and optional fallback targets.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Route {
    /// Primary target identifier.
    pub primary: String,
    /// Ordered fallback target identifiers.
    pub fallbacks: Vec<String>,
}

/// Validated operator catalog selected by `OPMUX_CONFIG_FILE`.
#[derive(Debug, Clone, PartialEq)]
pub struct Catalog {
    /// Schema version. Currently only `1` is accepted.
    pub version: u32,
    /// Route used when a client omits a route name.
    pub default_route: String,
    /// Target identifier to execution settings.
    pub targets: HashMap<String, Target>,
    /// Route identifier to flat target chain.
    pub routes: HashMap<String, Route>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawCatalog {
    version: u32,
    default_route: String,
    #[serde(deserialize_with = "deserialize_unique_targets")]
    targets: HashMap<String, RawTarget>,
    #[serde(deserialize_with = "deserialize_unique_routes")]
    routes: HashMap<String, RawRoute>,
    #[serde(default)]
    limits: Option<RawLimits>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawTarget {
    #[serde(default)]
    vendor: Option<String>,
    model: String,
    max_output_tokens: u32,
    pricing: RawPricing,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawPricing {
    input_per_million: f64,
    output_per_million: f64,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawRoute {
    primary: String,
    #[serde(default, deserialize_with = "deserialize_fallbacks")]
    fallbacks: Vec<String>,
}

pub(crate) struct ParsedCatalog {
    pub catalog: Catalog,
    pub raw_limits: Option<RawLimits>,
}

pub(crate) fn parse_catalog(json: &str) -> Result<ParsedCatalog, ConfigError> {
    let raw: RawCatalog = serde_json::from_str(json).map_err(map_serde_error)?;
    if raw.version != 1 {
        return Err(ConfigError::new(
            ConfigCategory::CatalogUnsupportedVersion,
            "catalog version must be 1",
        ));
    }

    let mut targets = HashMap::new();
    for (id, target) in raw.targets {
        let id = require_identifier("target", &id)?;
        let vendor = parse_vendor(target.vendor.as_deref())?;
        let model = require_identifier("model", &target.model)?;
        let max_output_tokens = u32::try_from(check_bound(
            MAX_OUTPUT_TOKENS,
            u64::from(target.max_output_tokens),
        )?)
        .map_err(|_| ConfigError::invalid_limit(MAX_OUTPUT_TOKENS.name))?;
        let pricing = validate_pricing(&target.pricing)?;
        targets.insert(
            id,
            Target {
                vendor,
                model,
                max_output_tokens,
                pricing,
            },
        );
    }

    if targets.is_empty() {
        return Err(ConfigError::new(
            ConfigCategory::CatalogEmptyIdentifier,
            "catalog must define at least one target",
        ));
    }

    let mut routes = HashMap::new();
    for (id, route) in raw.routes {
        let id = require_identifier("route", &id)?;
        routes.insert(id, validate_route(&targets, route)?);
    }

    if routes.is_empty() {
        return Err(ConfigError::new(
            ConfigCategory::CatalogMissingDefaultRoute,
            "catalog must define at least one route",
        ));
    }

    let default_route = require_identifier("default_route", &raw.default_route)?;
    if !routes.contains_key(&default_route) {
        return Err(ConfigError::new(
            ConfigCategory::CatalogMissingDefaultRoute,
            "default_route must reference a configured route",
        ));
    }

    Ok(ParsedCatalog {
        catalog: Catalog {
            version: raw.version,
            default_route,
            targets,
            routes,
        },
        raw_limits: raw.limits,
    })
}

fn validate_route(
    targets: &HashMap<String, Target>,
    route: RawRoute,
) -> Result<Route, ConfigError> {
    let primary = require_identifier("primary", &route.primary)?;
    if !targets.contains_key(&primary) {
        return Err(ConfigError::new(
            ConfigCategory::CatalogUnknownTargetReference,
            "route primary must reference a configured target",
        ));
    }

    let mut seen = vec![primary.clone()];
    let mut fallbacks = Vec::with_capacity(route.fallbacks.len());
    for fallback in route.fallbacks {
        let fallback = require_identifier("fallback", &fallback)?;
        if !targets.contains_key(&fallback) {
            return Err(ConfigError::new(
                ConfigCategory::CatalogUnknownTargetReference,
                "route fallback must reference a configured target",
            ));
        }
        if seen.iter().any(|existing| existing == &fallback) {
            return Err(ConfigError::new(
                ConfigCategory::CatalogDuplicateChainTarget,
                "route chain cannot repeat a target",
            ));
        }
        seen.push(fallback.clone());
        fallbacks.push(fallback);
    }

    Ok(Route { primary, fallbacks })
}

fn parse_vendor(raw: Option<&str>) -> Result<VendorKind, ConfigError> {
    match raw {
        None => Ok(VendorKind::Openai),
        Some("openai") => Ok(VendorKind::Openai),
        Some(value) if value.trim().is_empty() => Err(ConfigError::new(
            ConfigCategory::CatalogUnsupportedVendor,
            "target vendor cannot be empty",
        )),
        Some(_) => Err(ConfigError::new(
            ConfigCategory::CatalogUnsupportedVendor,
            "only the openai vendor is supported",
        )),
    }
}

fn validate_pricing(raw: &RawPricing) -> Result<TargetPricing, ConfigError> {
    Ok(TargetPricing {
        input_per_million: validate_price(raw.input_per_million)?,
        output_per_million: validate_price(raw.output_per_million)?,
    })
}

fn validate_price(value: f64) -> Result<f64, ConfigError> {
    if !value.is_finite() || !(0.0..=MAX_PRICE_PER_MILLION).contains(&value) {
        return Err(ConfigError::new(
            ConfigCategory::CatalogInvalidPrice,
            "prices must be finite, nonnegative, and within the documented bound",
        ));
    }
    Ok(value)
}

fn require_identifier(kind: &str, value: &str) -> Result<String, ConfigError> {
    if value.is_empty() || value.trim() != value {
        return Err(ConfigError::new(
            ConfigCategory::CatalogEmptyIdentifier,
            format!("{kind} identifier cannot be empty"),
        ));
    }
    Ok(value.to_string())
}

fn deserialize_unique_targets<'de, D>(
    deserializer: D,
) -> Result<HashMap<String, RawTarget>, D::Error>
where
    D: Deserializer<'de>,
{
    deserialize_unique_map(deserializer, "duplicate target identifier")
}

fn deserialize_unique_routes<'de, D>(
    deserializer: D,
) -> Result<HashMap<String, RawRoute>, D::Error>
where
    D: Deserializer<'de>,
{
    deserialize_unique_map(deserializer, "duplicate route identifier")
}

fn deserialize_unique_map<'de, D, T>(
    deserializer: D,
    duplicate_message: &'static str,
) -> Result<HashMap<String, T>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    struct UniqueMapVisitor<T> {
        duplicate_message: &'static str,
        marker: PhantomData<T>,
    }

    impl<'de, T> Visitor<'de> for UniqueMapVisitor<T>
    where
        T: Deserialize<'de>,
    {
        type Value = HashMap<String, T>;

        fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
            formatter.write_str("a map with unique identifiers")
        }

        fn visit_map<M>(self, mut access: M) -> Result<Self::Value, M::Error>
        where
            M: MapAccess<'de>,
        {
            let mut map = HashMap::with_capacity(access.size_hint().unwrap_or(0));
            while let Some((key, value)) = access.next_entry::<String, T>()? {
                if map.contains_key(&key) {
                    return Err(de::Error::custom(self.duplicate_message));
                }
                map.insert(key, value);
            }
            Ok(map)
        }
    }

    deserializer.deserialize_map(UniqueMapVisitor {
        duplicate_message,
        marker: PhantomData,
    })
}

fn deserialize_fallbacks<'de, D>(deserializer: D) -> Result<Vec<String>, D::Error>
where
    D: Deserializer<'de>,
{
    let value = serde_json::Value::deserialize(deserializer)?;
    match value {
        serde_json::Value::Array(items) => {
            let mut fallbacks = Vec::with_capacity(items.len());
            for item in items {
                match item {
                    serde_json::Value::String(id) => fallbacks.push(id),
                    serde_json::Value::Object(_) | serde_json::Value::Array(_) => {
                        return Err(de::Error::custom("nested chain"));
                    }
                    _ => {
                        return Err(de::Error::custom(
                            "invalid type: fallback identifiers must be strings",
                        ));
                    }
                }
            }
            Ok(fallbacks)
        }
        serde_json::Value::Object(_) => Err(de::Error::custom("nested chain")),
        _ => Err(de::Error::custom(
            "invalid type: fallbacks must be an array of target identifiers",
        )),
    }
}

fn map_serde_error(err: serde_json::Error) -> ConfigError {
    let message = err.to_string();
    if message.contains("duplicate target identifier") {
        return ConfigError::new(
            ConfigCategory::CatalogDuplicateTarget,
            "catalog defines duplicate target identifiers",
        );
    }
    if message.contains("duplicate route identifier") {
        return ConfigError::new(
            ConfigCategory::CatalogDuplicateRoute,
            "catalog defines duplicate route identifiers",
        );
    }
    if message.contains("nested chain") {
        return ConfigError::new(
            ConfigCategory::CatalogNestedChain,
            "route chains must be flat lists of target identifiers",
        );
    }
    if message.contains("unknown field") {
        return ConfigError::new(
            ConfigCategory::CatalogUnknownField,
            "catalog contains an unknown field",
        );
    }
    if message.contains("missing field `version`")
        || (message.contains("invalid type") && message.contains("version"))
    {
        return ConfigError::new(
            ConfigCategory::CatalogUnsupportedVersion,
            "catalog version must be 1",
        );
    }
    if message.contains("missing field `pricing`")
        || message.contains("missing field `input_per_million`")
        || message.contains("missing field `output_per_million`")
    {
        return ConfigError::new(
            ConfigCategory::CatalogIncompletePricing,
            "target pricing must include input_per_million and output_per_million",
        );
    }
    if message.contains("number out of range") {
        return ConfigError::new(
            ConfigCategory::CatalogInvalidPrice,
            "prices must be finite, nonnegative, and within the documented bound",
        );
    }
    if err.is_syntax() || err.is_eof() {
        return ConfigError::new(
            ConfigCategory::CatalogMalformedJson,
            "catalog JSON is malformed",
        );
    }
    if message.contains("invalid type")
        || message.contains("invalid value")
        || message.contains("missing field")
    {
        return ConfigError::new(
            ConfigCategory::CatalogInvalidType,
            "catalog field has the wrong type",
        );
    }
    ConfigError::new(
        ConfigCategory::CatalogMalformedJson,
        "catalog JSON is malformed",
    )
}

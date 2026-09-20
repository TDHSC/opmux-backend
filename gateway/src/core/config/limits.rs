//! Validated execution and admission policy limits.

use serde::Deserialize;
use std::time::Duration;

use super::env::EnvSource;
use super::error::ConfigError;

/// Documented bound for one numeric policy setting.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LimitBound {
    /// Canonical JSON/environment field name.
    pub name: &'static str,
    /// Unit used by the integer value.
    pub unit: &'static str,
    /// Documented default when the field is omitted.
    pub default: u64,
    /// Inclusive minimum. Zero is allowed only when this is `0`.
    pub min: u64,
    /// Inclusive maximum.
    pub max: u64,
}

/// Protected request deadline. Unit: milliseconds.
pub const PROTECTED_REQUEST_DEADLINE_MS: LimitBound = LimitBound {
    name: "protected_request_deadline_ms",
    unit: "milliseconds",
    default: 30_000,
    min: 1,
    max: 300_000,
};

/// Maximum duration of one provider attempt. Unit: milliseconds.
pub const MAX_ATTEMPT_TIMEOUT_MS: LimitBound = LimitBound {
    name: "max_attempt_timeout_ms",
    unit: "milliseconds",
    default: 10_000,
    min: 1,
    max: 120_000,
};

/// Retry count per target after the first attempt. Unit: count.
///
/// Zero retries is valid and still allows the initial attempt.
pub const RETRIES_PER_TARGET: LimitBound = LimitBound {
    name: "retries_per_target",
    unit: "count",
    default: 1,
    min: 0,
    max: 8,
};

/// Maximum actual provider attempts across the whole request. Unit: count.
pub const MAX_TOTAL_ATTEMPTS: LimitBound = LimitBound {
    name: "max_total_attempts",
    unit: "count",
    default: 3,
    min: 1,
    max: 16,
};

/// Maximum fallback targets in a route chain. Unit: count.
pub const MAX_FALLBACK_TARGETS: LimitBound = LimitBound {
    name: "max_fallback_targets",
    unit: "count",
    default: 2,
    min: 0,
    max: 8,
};

/// Exponential backoff cap. Unit: milliseconds.
pub const BACKOFF_CAP_MS: LimitBound = LimitBound {
    name: "backoff_cap_ms",
    unit: "milliseconds",
    default: 2_000,
    min: 1,
    max: 60_000,
};

/// Consecutive transient failures before a target circuit opens. Unit: count.
pub const CIRCUIT_FAILURE_THRESHOLD: LimitBound = LimitBound {
    name: "circuit_failure_threshold",
    unit: "count",
    default: 3,
    min: 1,
    max: 100,
};

/// Circuit cooldown / open duration. Unit: milliseconds.
pub const CIRCUIT_COOLDOWN_MS: LimitBound = LimitBound {
    name: "circuit_cooldown_ms",
    unit: "milliseconds",
    default: 30_000,
    min: 1,
    max: 600_000,
};

/// Concurrent generation admission slots. Unit: count.
pub const MAX_CONCURRENT_GENERATIONS: LimitBound = LimitBound {
    name: "max_concurrent_generations",
    unit: "count",
    default: 32,
    min: 1,
    max: 1_024,
};

/// Raw HTTP body limit. Unit: bytes.
pub const MAX_REQUEST_BODY_BYTES: LimitBound = LimitBound {
    name: "max_request_body_bytes",
    unit: "bytes",
    default: 1_048_576,
    min: 1,
    max: 16_777_216,
};

/// Metadata JSON size limit. Unit: bytes.
pub const MAX_METADATA_BYTES: LimitBound = LimitBound {
    name: "max_metadata_bytes",
    unit: "bytes",
    default: 1_000,
    min: 1,
    max: 1_048_576,
};

/// Original prompt character limit. Unit: characters.
pub const MAX_PROMPT_CHARS: LimitBound = LimitBound {
    name: "max_prompt_chars",
    unit: "characters",
    default: 4_000,
    min: 1,
    max: 1_000_000,
};

/// Upstream response body limit. Unit: bytes.
pub const MAX_UPSTREAM_RESPONSE_BYTES: LimitBound = LimitBound {
    name: "max_upstream_response_bytes",
    unit: "bytes",
    default: 1_048_576,
    min: 1,
    max: 16_777_216,
};

/// Target output-token cap. Unit: tokens.
pub const MAX_OUTPUT_TOKENS: LimitBound = LimitBound {
    name: "max_output_tokens",
    unit: "tokens",
    default: 512,
    min: 1,
    max: 1_000_000,
};

/// Shutdown grace period. Unit: seconds.
pub const SHUTDOWN_TIMEOUT_SECS: LimitBound = LimitBound {
    name: "SERVER_SHUTDOWN_TIMEOUT",
    unit: "seconds",
    default: 30,
    min: 1,
    max: 600,
};

/// Maximum finite price per million tokens. Unit: USD.
pub const MAX_PRICE_PER_MILLION: f64 = 1_000_000.0;

/// Optional catalog `limits` object.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RawLimits {
    pub protected_request_deadline_ms: Option<u64>,
    pub max_attempt_timeout_ms: Option<u64>,
    pub retries_per_target: Option<u64>,
    pub max_total_attempts: Option<u64>,
    pub max_fallback_targets: Option<u64>,
    pub backoff_cap_ms: Option<u64>,
    pub circuit_failure_threshold: Option<u64>,
    pub circuit_cooldown_ms: Option<u64>,
    pub max_concurrent_generations: Option<u64>,
    pub max_request_body_bytes: Option<u64>,
    pub max_metadata_bytes: Option<u64>,
    pub max_prompt_chars: Option<u64>,
    pub max_upstream_response_bytes: Option<u64>,
}

/// Validated policy limits injected at startup.
///
/// Milestone 1 validates and injects these values. Upstream success bodies
/// are bounded by `max_upstream_response_bytes` while reading. Request-deadline,
/// fallback, circuit, concurrency, and inbound raw-size enforcement are later
/// features and must not be described as already active.
#[derive(Clone, PartialEq, Eq)]
pub struct PolicyLimits {
    /// Overall protected-request deadline.
    pub protected_request_deadline: Duration,
    /// Maximum time for one provider attempt.
    pub max_attempt_timeout: Duration,
    /// Retries after the first attempt on the same target.
    pub retries_per_target: u32,
    /// Maximum actual provider attempts for the whole request.
    pub max_total_attempts: u32,
    /// Maximum fallback targets allowed in a catalog route.
    pub max_fallback_targets: u32,
    /// Exponential backoff cap.
    pub backoff_cap: Duration,
    /// Failures before a target circuit opens.
    pub circuit_failure_threshold: u32,
    /// Circuit open / cooldown duration.
    pub circuit_cooldown: Duration,
    /// Concurrent generation slots.
    pub max_concurrent_generations: u32,
    /// Raw request body size.
    pub max_request_body_bytes: u64,
    /// Metadata JSON size.
    pub max_metadata_bytes: u64,
    /// Original prompt characters.
    pub max_prompt_chars: u64,
    /// Upstream response bytes.
    pub max_upstream_response_bytes: u64,
}

impl PolicyLimits {
    /// Returns documented defaults used when catalog and env omit a limit.
    pub fn documented_defaults() -> Self {
        Self::from_raw(None).expect("documented defaults are in range")
    }

    pub(crate) fn from_raw(raw: Option<&RawLimits>) -> Result<Self, ConfigError> {
        let raw = raw.cloned().unwrap_or_default();
        Ok(Self {
            protected_request_deadline: Duration::from_millis(check_bound(
                PROTECTED_REQUEST_DEADLINE_MS,
                raw.protected_request_deadline_ms
                    .unwrap_or(PROTECTED_REQUEST_DEADLINE_MS.default),
            )?),
            max_attempt_timeout: Duration::from_millis(check_bound(
                MAX_ATTEMPT_TIMEOUT_MS,
                raw.max_attempt_timeout_ms
                    .unwrap_or(MAX_ATTEMPT_TIMEOUT_MS.default),
            )?),
            retries_per_target: to_u32(check_bound(
                RETRIES_PER_TARGET,
                raw.retries_per_target.unwrap_or(RETRIES_PER_TARGET.default),
            )?)?,
            max_total_attempts: to_u32(check_bound(
                MAX_TOTAL_ATTEMPTS,
                raw.max_total_attempts.unwrap_or(MAX_TOTAL_ATTEMPTS.default),
            )?)?,
            max_fallback_targets: to_u32(check_bound(
                MAX_FALLBACK_TARGETS,
                raw.max_fallback_targets
                    .unwrap_or(MAX_FALLBACK_TARGETS.default),
            )?)?,
            backoff_cap: Duration::from_millis(check_bound(
                BACKOFF_CAP_MS,
                raw.backoff_cap_ms.unwrap_or(BACKOFF_CAP_MS.default),
            )?),
            circuit_failure_threshold: to_u32(check_bound(
                CIRCUIT_FAILURE_THRESHOLD,
                raw.circuit_failure_threshold
                    .unwrap_or(CIRCUIT_FAILURE_THRESHOLD.default),
            )?)?,
            circuit_cooldown: Duration::from_millis(check_bound(
                CIRCUIT_COOLDOWN_MS,
                raw.circuit_cooldown_ms
                    .unwrap_or(CIRCUIT_COOLDOWN_MS.default),
            )?),
            max_concurrent_generations: to_u32(check_bound(
                MAX_CONCURRENT_GENERATIONS,
                raw.max_concurrent_generations
                    .unwrap_or(MAX_CONCURRENT_GENERATIONS.default),
            )?)?,
            max_request_body_bytes: check_bound(
                MAX_REQUEST_BODY_BYTES,
                raw.max_request_body_bytes
                    .unwrap_or(MAX_REQUEST_BODY_BYTES.default),
            )?,
            max_metadata_bytes: check_bound(
                MAX_METADATA_BYTES,
                raw.max_metadata_bytes.unwrap_or(MAX_METADATA_BYTES.default),
            )?,
            max_prompt_chars: check_bound(
                MAX_PROMPT_CHARS,
                raw.max_prompt_chars.unwrap_or(MAX_PROMPT_CHARS.default),
            )?,
            max_upstream_response_bytes: check_bound(
                MAX_UPSTREAM_RESPONSE_BYTES,
                raw.max_upstream_response_bytes
                    .unwrap_or(MAX_UPSTREAM_RESPONSE_BYTES.default),
            )?,
        })
    }

    pub(crate) fn apply_env_overrides<E: EnvSource>(
        mut self,
        env: &E,
    ) -> Result<Self, ConfigError> {
        if let Some(value) = env_u64(
            env,
            &[
                "OPMUX_PROTECTED_REQUEST_DEADLINE_MS",
                PROTECTED_REQUEST_DEADLINE_MS.name,
            ],
        )? {
            self.protected_request_deadline =
                Duration::from_millis(check_bound(PROTECTED_REQUEST_DEADLINE_MS, value)?);
        }
        if let Some(value) = env_u64(
            env,
            &[
                "OPMUX_MAX_ATTEMPT_TIMEOUT_MS",
                MAX_ATTEMPT_TIMEOUT_MS.name,
                "OPENAI_TIMEOUT_MS",
                "EXECUTOR_TIMEOUT_MS",
            ],
        )? {
            self.max_attempt_timeout =
                Duration::from_millis(check_bound(MAX_ATTEMPT_TIMEOUT_MS, value)?);
        }
        if let Some(value) = env_u64(
            env,
            &[
                "OPMUX_RETRIES_PER_TARGET",
                RETRIES_PER_TARGET.name,
                "EXECUTOR_MAX_RETRIES",
            ],
        )? {
            self.retries_per_target = to_u32(check_bound(RETRIES_PER_TARGET, value)?)?;
        }
        if let Some(value) =
            env_u64(env, &["OPMUX_MAX_TOTAL_ATTEMPTS", MAX_TOTAL_ATTEMPTS.name])?
        {
            self.max_total_attempts = to_u32(check_bound(MAX_TOTAL_ATTEMPTS, value)?)?;
        }
        if let Some(value) = env_u64(
            env,
            &["OPMUX_MAX_FALLBACK_TARGETS", MAX_FALLBACK_TARGETS.name],
        )? {
            self.max_fallback_targets =
                to_u32(check_bound(MAX_FALLBACK_TARGETS, value)?)?;
        }
        if let Some(value) = env_u64(env, &["OPMUX_BACKOFF_CAP_MS", BACKOFF_CAP_MS.name])?
        {
            self.backoff_cap = Duration::from_millis(check_bound(BACKOFF_CAP_MS, value)?);
        }
        if let Some(value) = env_u64(
            env,
            &[
                "OPMUX_CIRCUIT_FAILURE_THRESHOLD",
                CIRCUIT_FAILURE_THRESHOLD.name,
            ],
        )? {
            self.circuit_failure_threshold =
                to_u32(check_bound(CIRCUIT_FAILURE_THRESHOLD, value)?)?;
        }
        if let Some(value) = env_u64(
            env,
            &["OPMUX_CIRCUIT_COOLDOWN_MS", CIRCUIT_COOLDOWN_MS.name],
        )? {
            self.circuit_cooldown =
                Duration::from_millis(check_bound(CIRCUIT_COOLDOWN_MS, value)?);
        }
        if let Some(value) = env_u64(
            env,
            &[
                "OPMUX_MAX_CONCURRENT_GENERATIONS",
                MAX_CONCURRENT_GENERATIONS.name,
            ],
        )? {
            self.max_concurrent_generations =
                to_u32(check_bound(MAX_CONCURRENT_GENERATIONS, value)?)?;
        }
        if let Some(value) = env_u64(
            env,
            &["OPMUX_MAX_REQUEST_BODY_BYTES", MAX_REQUEST_BODY_BYTES.name],
        )? {
            self.max_request_body_bytes = check_bound(MAX_REQUEST_BODY_BYTES, value)?;
        }
        if let Some(value) =
            env_u64(env, &["OPMUX_MAX_METADATA_BYTES", MAX_METADATA_BYTES.name])?
        {
            self.max_metadata_bytes = check_bound(MAX_METADATA_BYTES, value)?;
        }
        if let Some(value) =
            env_u64(env, &["OPMUX_MAX_PROMPT_CHARS", MAX_PROMPT_CHARS.name])?
        {
            self.max_prompt_chars = check_bound(MAX_PROMPT_CHARS, value)?;
        }
        if let Some(value) = env_u64(
            env,
            &[
                "OPMUX_MAX_UPSTREAM_RESPONSE_BYTES",
                MAX_UPSTREAM_RESPONSE_BYTES.name,
            ],
        )? {
            self.max_upstream_response_bytes =
                check_bound(MAX_UPSTREAM_RESPONSE_BYTES, value)?;
        }
        Ok(self)
    }

    /// Deadline in milliseconds.
    pub fn protected_request_deadline_ms(&self) -> u64 {
        duration_ms(self.protected_request_deadline)
    }

    /// Attempt timeout in milliseconds.
    pub fn max_attempt_timeout_ms(&self) -> u64 {
        duration_ms(self.max_attempt_timeout)
    }

    /// Backoff cap in milliseconds.
    pub fn backoff_cap_ms(&self) -> u64 {
        duration_ms(self.backoff_cap)
    }

    /// Circuit cooldown in milliseconds.
    pub fn circuit_cooldown_ms(&self) -> u64 {
        duration_ms(self.circuit_cooldown)
    }
}

fn duration_ms(duration: Duration) -> u64 {
    duration.as_millis() as u64
}

pub(crate) fn check_bound(bound: LimitBound, value: u64) -> Result<u64, ConfigError> {
    if value < bound.min || value > bound.max {
        return Err(ConfigError::invalid_limit(bound.name));
    }
    Ok(value)
}

pub(crate) fn parse_u64(name: &str, raw: &str) -> Result<u64, ConfigError> {
    let trimmed = raw.trim();
    if trimmed.is_empty() || !trimmed.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(ConfigError::invalid_limit(name));
    }
    trimmed
        .parse::<u64>()
        .map_err(|_| ConfigError::invalid_limit(name))
}

fn env_u64<E: EnvSource>(env: &E, names: &[&str]) -> Result<Option<u64>, ConfigError> {
    for name in names {
        if let Some(raw) = env.get(name) {
            return Ok(Some(parse_u64(name, &raw)?));
        }
    }
    Ok(None)
}

fn to_u32(value: u64) -> Result<u32, ConfigError> {
    u32::try_from(value).map_err(|_| ConfigError::invalid_limit("numeric_limit"))
}

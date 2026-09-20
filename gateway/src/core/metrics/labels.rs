//! Bounded metric label sources.
//!
//! Execution labels are operator-configured catalog identifiers and a finite
//! set of protocol/outcome classes. They are never credentials, prompts,
//! metadata, tenant/key/request/correlation IDs, raw paths, URLs, provider
//! model strings, or raw error bodies.

/// Replacement for a value that is not a safe catalog identifier.
pub const UNKNOWN_ID: &str = "unknown";

/// Inclusive maximum length of a configured target or route identifier label.
pub const MAX_CATALOG_ID_LEN: usize = 64;

/// Outcome class for one started provider attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AttemptOutcome {
    /// Attempt returned a validated successful result.
    Success,
    /// Transient transport or provider 5xx failure.
    Retryable,
    /// Per-attempt timeout while overall time remained.
    Timeout,
    /// Provider throttling (`UPSTREAM_RATE_LIMIT`).
    RateLimit,
    /// Shared-account quota exhaustion.
    Quota,
    /// Upstream credential rejection.
    UpstreamAuth,
    /// Malformed or unusable success payload.
    Protocol,
    /// Permanent upstream request rejection.
    Rejected,
    /// Overall protected-request deadline elapsed.
    Deadline,
    /// Target circuit skipped or exhausted the route.
    CircuitOpen,
    /// Unexpected internal execution fault.
    Internal,
}

impl AttemptOutcome {
    /// Stable metric label value for this class.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Success => "success",
            Self::Retryable => "retryable",
            Self::Timeout => "timeout",
            Self::RateLimit => "rate_limit",
            Self::Quota => "quota",
            Self::UpstreamAuth => "upstream_auth",
            Self::Protocol => "protocol",
            Self::Rejected => "rejected",
            Self::Deadline => "deadline",
            Self::CircuitOpen => "circuit_open",
            Self::Internal => "internal",
        }
    }
}

/// Target-scoped circuit phase exported as a bounded label and gauge value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CircuitStateLabel {
    /// Target accepts normal hops.
    Closed,
    /// Single recovery probe is admitted.
    HalfOpen,
    /// Target is skipped until cooldown.
    Open,
}

impl CircuitStateLabel {
    /// Stable metric label value for this phase.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Closed => "closed",
            Self::HalfOpen => "half_open",
            Self::Open => "open",
        }
    }

    /// Gauge value: 0 closed, 1 half-open, 2 open.
    pub const fn as_gauge(self) -> f64 {
        match self {
            Self::Closed => 0.0,
            Self::HalfOpen => 1.0,
            Self::Open => 2.0,
        }
    }
}

/// Returns a catalog identifier when it is a bounded configured ID.
///
/// Other values collapse to [`UNKNOWN_ID`] so URLs, request IDs, and raw
/// paths cannot become series.
pub fn bound_catalog_id(value: &str) -> &str {
    if is_configured_id(value) {
        value
    } else {
        UNKNOWN_ID
    }
}

fn is_configured_id(value: &str) -> bool {
    let len = value.len();
    (1..=MAX_CATALOG_ID_LEN).contains(&len)
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalog_ids_stay_verbatim_and_unsafe_values_collapse() {
        assert_eq!(bound_catalog_id("primary"), "primary");
        assert_eq!(bound_catalog_id("alpha-1"), "alpha-1");
        assert_eq!(bound_catalog_id("Secondary_2"), "Secondary_2");
        assert_eq!(
            bound_catalog_id("http://127.0.0.1/v1/chat/completions"),
            UNKNOWN_ID
        );
        assert_eq!(bound_catalog_id("/api/v1/route?x=1"), UNKNOWN_ID);
        assert_eq!(bound_catalog_id("req/obs002"), UNKNOWN_ID);
        assert_eq!(bound_catalog_id(""), UNKNOWN_ID);
        assert_eq!(
            bound_catalog_id(&"a".repeat(MAX_CATALOG_ID_LEN + 1)),
            UNKNOWN_ID
        );
        assert_eq!(bound_catalog_id("reported model"), UNKNOWN_ID);
        assert_ne!(AttemptOutcome::RateLimit.as_str(), "overloaded");
        assert_ne!(AttemptOutcome::RateLimit.as_str(), "OVERLOADED");
    }
}

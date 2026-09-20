//! Bounded metric label sources.
//!
//! Execution labels are operator-configured catalog identifiers and a finite
//! set of protocol/outcome classes. They are never credentials, prompts,
//! metadata, tenant/key/request/correlation IDs, raw paths, URLs, provider
//! model strings, or raw error bodies. Cardinality is bounded by catalog
//! membership: accepted target IDs are exported verbatim and escaped by the
//! Prometheus exporter.

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
    /// Started attempt dropped before completion while the deadline remained.
    Cancelled,
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
            Self::Cancelled => "cancelled",
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

/// Returns an accepted configured catalog identifier verbatim.
///
/// Callers must pass operator-configured target IDs from the catalog, never
/// unvalidated request IDs, provider model strings, or URLs. The Prometheus
/// exporter escapes label values; this function does not truncate, hash, or
/// collapse identifiers to `unknown`.
pub fn bound_catalog_id(value: &str) -> &str {
    value
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepted_catalog_ids_stay_verbatim() {
        assert_eq!(bound_catalog_id("primary"), "primary");
        assert_eq!(bound_catalog_id("alpha-1"), "alpha-1");
        assert_eq!(bound_catalog_id("Secondary_2"), "Secondary_2");
        assert_eq!(bound_catalog_id("alpha.primary"), "alpha.primary");
        let long = format!("configured-long-target-{}", "c".repeat(50));
        assert!(long.len() > 64);
        assert_eq!(bound_catalog_id(&long), long);
        assert_eq!(AttemptOutcome::Cancelled.as_str(), "cancelled");
        assert_ne!(AttemptOutcome::RateLimit.as_str(), "overloaded");
        assert_ne!(AttemptOutcome::RateLimit.as_str(), "OVERLOADED");
    }
}

//! Per-attempt cutoff and observed upstream classification.
//!
//! Header-derived throttling is stored before any body refinement so an
//! outer attempt timeout cannot erase Retry-After or typed 429 facts.

use super::error::ExecutorError;
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// Shared per-attempt cutoff and the last observed classified error.
#[derive(Clone)]
pub struct AttemptContext {
    cutoff: tokio::time::Instant,
    observed: Arc<Mutex<Option<ExecutorError>>>,
}

impl AttemptContext {
    /// Creates a context that expires at an absolute Tokio instant.
    ///
    /// # Parameters
    /// - `cutoff` - Outer attempt timeout instant
    pub fn new(cutoff: tokio::time::Instant) -> Self {
        Self {
            cutoff,
            observed: Arc::new(Mutex::new(None)),
        }
    }

    /// Creates a context that expires after `timeout` on the current Tokio clock.
    ///
    /// # Parameters
    /// - `timeout` - Remaining time allowed for this attempt
    pub fn from_timeout(timeout: Duration) -> Self {
        Self::new(tokio::time::Instant::now() + timeout)
    }

    /// Absolute attempt cutoff.
    pub fn cutoff(&self) -> tokio::time::Instant {
        self.cutoff
    }

    /// Remaining time until the attempt cutoff, or zero when elapsed.
    pub fn remaining(&self) -> Duration {
        self.cutoff
            .checked_duration_since(tokio::time::Instant::now())
            .unwrap_or(Duration::ZERO)
    }

    /// Stores a classified error observed during this attempt.
    ///
    /// Call this as soon as 429 headers are classified, before any body
    /// refinement await.
    ///
    /// # Parameters
    /// - `error` - Header throttling or a later quota refinement
    pub fn observe(&self, error: ExecutorError) {
        *self
            .observed
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(error);
    }

    /// Returns the last observed classified error, if any.
    pub fn observed(&self) -> Option<ExecutorError> {
        self.observed
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[tokio::test]
    async fn remaining_is_zero_after_cutoff() {
        let attempt = AttemptContext::from_timeout(Duration::from_millis(1));
        tokio::time::sleep(Duration::from_millis(5)).await;
        assert!(attempt.remaining().is_zero());
        assert!(attempt.observed().is_none());
    }

    #[test]
    fn observe_replaces_throttling_with_quota() {
        let attempt = AttemptContext::from_timeout(Duration::from_secs(1));
        attempt.observe(ExecutorError::RateLimitExceeded {
            vendor: "openai".into(),
            retry_after_ms: Some(1_000),
        });
        attempt.observe(ExecutorError::QuotaExceeded);
        assert!(matches!(
            attempt.observed(),
            Some(ExecutorError::QuotaExceeded)
        ));
    }
}

//! Monotonic protected-request deadline.
//!
//! One deadline covers authentication, body extraction, and execution.
//! Layers read remaining time from this value instead of starting a fresh
//! budget.

use std::time::Duration;

/// Instant at which protected request work must stop.
///
/// Constructed once per request by deadline middleware and injected through
/// extensions and service calls. `remaining` shrinks as time passes; it is
/// not reset when entering a later layer.
#[derive(Clone, Copy, Debug)]
pub struct RequestDeadline {
    deadline: tokio::time::Instant,
}

impl RequestDeadline {
    /// Builds a deadline `timeout` from now on the current Tokio clock.
    ///
    /// # Parameters
    /// - `timeout` - Overall protected-request budget
    ///
    /// # Returns
    /// Deadline used by auth, body extraction, and execution
    pub fn from_timeout(timeout: Duration) -> Self {
        Self {
            deadline: tokio::time::Instant::now() + timeout,
        }
    }

    /// Builds a deadline at an explicit Tokio instant.
    ///
    /// # Parameters
    /// - `deadline` - Absolute expiry instant
    ///
    /// # Returns
    /// Deadline that expires at `deadline`
    pub fn at(deadline: tokio::time::Instant) -> Self {
        Self { deadline }
    }

    /// Returns the absolute expiry instant.
    pub fn as_instant(self) -> tokio::time::Instant {
        self.deadline
    }

    /// Returns true when the current Tokio clock is at or past expiry.
    pub fn is_expired(self) -> bool {
        tokio::time::Instant::now() >= self.deadline
    }

    /// Returns remaining time, or zero when expired.
    pub fn remaining(self) -> Duration {
        self.deadline
            .checked_duration_since(tokio::time::Instant::now())
            .unwrap_or(Duration::ZERO)
    }

    /// Caps `max` by remaining time.
    ///
    /// # Parameters
    /// - `max` - Per-attempt or other local maximum
    ///
    /// # Returns
    /// `None` when the deadline has already elapsed, otherwise
    /// `min(remaining, max)`
    pub fn cap(self, max: Duration) -> Option<Duration> {
        let remaining = self.remaining();
        if remaining.is_zero() {
            None
        } else {
            Some(remaining.min(max))
        }
    }

    /// Absolute cutoff for one provider attempt.
    ///
    /// Uses this request deadline instant, not a later `now` plus a previously
    /// observed remaining duration.
    ///
    /// # Parameters
    /// - `max_attempt` - Configured per-attempt maximum
    ///
    /// # Returns
    /// `None` when the deadline has elapsed or the capped cutoff is not in the
    /// future, otherwise `min(now + max_attempt, deadline)`
    pub fn attempt_cutoff(self, max_attempt: Duration) -> Option<tokio::time::Instant> {
        let now = tokio::time::Instant::now();
        if now >= self.deadline {
            return None;
        }
        let max_cutoff = now + max_attempt;
        let cutoff = if self.deadline < max_cutoff {
            self.deadline
        } else {
            max_cutoff
        };
        if now >= cutoff {
            None
        } else {
            Some(cutoff)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test(start_paused = true)]
    async fn remaining_budget_shrinks_without_resetting_the_deadline() {
        let deadline = RequestDeadline::from_timeout(Duration::from_secs(5));
        let first = deadline.remaining();
        assert!(first <= Duration::from_secs(5));
        assert!(first >= Duration::from_millis(4_990));

        tokio::time::advance(Duration::from_secs(2)).await;
        let second = deadline.remaining();
        assert!(second <= Duration::from_millis(3_000));
        assert!(second >= Duration::from_millis(2_990));
        assert_eq!(deadline.cap(Duration::from_secs(30)), Some(second));
        assert_eq!(deadline.as_instant(), deadline.as_instant());
        assert!(!deadline.is_expired());
    }

    #[tokio::test(start_paused = true)]
    async fn cap_returns_none_after_expiry() {
        let deadline = RequestDeadline::from_timeout(Duration::from_millis(10));
        tokio::time::advance(Duration::from_millis(10)).await;
        assert!(deadline.is_expired());
        assert_eq!(deadline.remaining(), Duration::ZERO);
        assert_eq!(deadline.cap(Duration::from_secs(10)), None);
    }

    #[tokio::test(start_paused = true)]
    async fn cap_uses_remaining_when_shorter_than_attempt_timeout() {
        let deadline = RequestDeadline::from_timeout(Duration::from_millis(40));
        tokio::time::advance(Duration::from_millis(25)).await;
        let capped = deadline
            .cap(Duration::from_secs(10))
            .expect("deadline still open");
        assert!(capped <= Duration::from_millis(15));
        assert!(capped > Duration::ZERO);
    }

    #[tokio::test(start_paused = true)]
    async fn attempt_cutoff_uses_absolute_deadline_not_later_now_plus_remaining() {
        let deadline = RequestDeadline::from_timeout(Duration::from_millis(100));
        tokio::time::advance(Duration::from_millis(40)).await;
        let stale_remaining = deadline.remaining();
        tokio::time::advance(Duration::from_millis(10)).await;
        let cutoff = deadline
            .attempt_cutoff(Duration::from_secs(10))
            .expect("deadline still open");
        assert_eq!(cutoff, deadline.as_instant());
        assert!(cutoff < tokio::time::Instant::now() + stale_remaining);
        assert!(deadline.attempt_cutoff(Duration::from_millis(5)).is_some());
    }

    #[tokio::test(start_paused = true)]
    async fn attempt_cutoff_returns_none_after_expiry() {
        let deadline = RequestDeadline::from_timeout(Duration::from_millis(10));
        tokio::time::advance(Duration::from_millis(10)).await;
        assert!(deadline.attempt_cutoff(Duration::from_secs(10)).is_none());
    }
}

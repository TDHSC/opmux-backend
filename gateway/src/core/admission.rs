//! Bounded concurrent generation admission.
//!
//! `try_acquire` never waits. A held permit covers primary execution, retry
//! backoff, and fallback, and is released on every terminal path including
//! cancellation.

use std::sync::Arc;
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

/// Safe `Retry-After` seconds returned with local overload `429 OVERLOADED`.
pub const LOCAL_OVERLOAD_RETRY_AFTER_SECS: u64 = 1;

/// RAII permit for one admitted generation request.
#[derive(Debug)]
pub struct GenerationPermit {
    _permit: OwnedSemaphorePermit,
}

/// Non-blocking limiter for concurrent generation work.
#[derive(Clone)]
pub struct AdmissionLimiter {
    semaphore: Arc<Semaphore>,
}

impl AdmissionLimiter {
    /// Builds a limiter with `max_concurrent_generations` slots.
    ///
    /// # Parameters
    /// - `max_concurrent_generations` - Inclusive concurrent generation cap
    ///
    /// # Returns
    /// Limiter shared by the production router
    pub fn new(max_concurrent_generations: u32) -> Self {
        Self {
            semaphore: Arc::new(Semaphore::new(max_concurrent_generations as usize)),
        }
    }

    /// Tries to admit one generation without waiting.
    ///
    /// # Returns
    /// `Some(permit)` when a slot is free. `None` when saturated.
    pub fn try_acquire(&self) -> Option<GenerationPermit> {
        self.semaphore
            .clone()
            .try_acquire_owned()
            .ok()
            .map(|permit| GenerationPermit { _permit: permit })
    }

    /// Returns currently unused generation slots.
    #[cfg(test)]
    pub fn available_permits(&self) -> usize {
        self.semaphore.available_permits()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    #[test]
    fn try_acquire_rejects_immediately_when_saturated() {
        let limiter = AdmissionLimiter::new(1);
        let permit = limiter.try_acquire().expect("first slot");
        assert_eq!(limiter.available_permits(), 0);
        let started = Instant::now();
        assert!(limiter.try_acquire().is_none());
        assert!(started.elapsed() < Duration::from_millis(50));
        drop(permit);
        assert!(limiter.try_acquire().is_some());
    }

    #[test]
    fn configured_limit_is_the_concurrent_cap() {
        let limiter = AdmissionLimiter::new(2);
        let first = limiter.try_acquire().expect("first");
        let second = limiter.try_acquire().expect("second");
        assert!(limiter.try_acquire().is_none());
        drop(first);
        assert!(limiter.try_acquire().is_some());
        drop(second);
    }

    #[tokio::test]
    async fn cancelling_a_holder_releases_capacity() {
        let limiter = AdmissionLimiter::new(1);
        let held = limiter.clone();
        let handle = tokio::spawn(async move {
            let _permit = held.try_acquire().expect("task slot");
            std::future::pending::<()>().await;
        });
        let started = Instant::now();
        while limiter.available_permits() != 0 {
            assert!(started.elapsed() < Duration::from_secs(1));
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
        assert!(limiter.try_acquire().is_none());
        handle.abort();
        let _ = handle.await;
        assert!(limiter.try_acquire().is_some());
    }
}

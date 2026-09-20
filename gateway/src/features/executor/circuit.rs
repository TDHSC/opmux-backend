//! Target-scoped circuit breakers with single-flight half-open probes.
//!
//! Circuits are keyed by catalog target identity, not vendor. One model's
//! transient failure can open its own circuit without blocking a healthy
//! same-provider fallback. At most one recovery probe is in flight per
//! target; dropping a probe future releases ownership without wedging
//! half-open state.

use super::config::ExecutorConfig;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Source of `Instant` values for circuit transitions.
pub(crate) trait InstantClock: Send + Sync {
    /// Current time for cooldown and probe decisions.
    fn now(&self) -> Instant;
}

/// Production clock using `Instant::now`.
#[derive(Debug, Default)]
pub(crate) struct SystemClock;

impl InstantClock for SystemClock {
    fn now(&self) -> Instant {
        Instant::now()
    }
}

/// Test clock that advances independently of wall time.
#[cfg(test)]
#[derive(Debug)]
pub(crate) struct ManualClock {
    origin: Instant,
    offset: Mutex<Duration>,
}

#[cfg(test)]
impl ManualClock {
    /// Creates a clock starting at an arbitrary origin.
    pub(crate) fn new() -> Self {
        Self {
            origin: Instant::now(),
            offset: Mutex::new(Duration::ZERO),
        }
    }

    /// Advances the clock by `by` without sleeping.
    pub(crate) fn advance(&self, by: Duration) {
        let mut offset = self
            .offset
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        *offset = offset.saturating_add(by);
    }
}

#[cfg(test)]
impl InstantClock for ManualClock {
    fn now(&self) -> Instant {
        let offset = self
            .offset
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        self.origin + *offset
    }
}

#[derive(Clone, Copy)]
enum CircuitPhase {
    Closed { failures: u32 },
    Open { opened_until: Instant },
    HalfOpen { in_flight: bool },
}

impl Default for CircuitPhase {
    fn default() -> Self {
        Self::Closed { failures: 0 }
    }
}

/// Result of asking a target circuit whether a hop may start.
pub(crate) enum CircuitAdmission {
    /// Closed circuit; use the normal retry budget.
    Allow,
    /// Open, or a probe is already in flight. Skip without an attempt.
    Reject { retry_after_ms: u64 },
    /// This caller owns the single half-open probe.
    Probe(ProbeGuard),
}

enum ProbeOutcome {
    Success,
    TransientFailure,
    Permanent,
}

/// Releases probe ownership on drop unless the outcome was recorded.
pub(crate) struct ProbeGuard {
    registry: TargetCircuitRegistry,
    target_id: String,
    settled: bool,
}

impl ProbeGuard {
    /// Closes the circuit after a healthy probe.
    pub(crate) fn success(mut self) {
        self.settle(ProbeOutcome::Success);
    }

    /// Reopens the circuit for cooldown after a transient probe failure.
    pub(crate) fn transient_failure(mut self) {
        self.settle(ProbeOutcome::TransientFailure);
    }

    /// Drops half-open state without counting a permanent error as transient.
    pub(crate) fn ignore_permanent(mut self) {
        self.settle(ProbeOutcome::Permanent);
    }

    fn settle(&mut self, outcome: ProbeOutcome) {
        if self.settled {
            return;
        }
        self.settled = true;
        self.registry.finish_probe(&self.target_id, outcome);
    }
}

impl Drop for ProbeGuard {
    fn drop(&mut self) {
        if !self.settled {
            self.registry.release_probe(&self.target_id);
        }
    }
}

/// In-process circuit map keyed by catalog target ID.
#[derive(Clone)]
pub(crate) struct TargetCircuitRegistry {
    inner: Arc<Mutex<HashMap<String, CircuitPhase>>>,
    threshold: u32,
    cooldown: Duration,
    clock: Arc<dyn InstantClock>,
}

impl TargetCircuitRegistry {
    /// Creates a registry with an injected clock.
    pub(crate) fn new(
        threshold: u32,
        cooldown: Duration,
        clock: Arc<dyn InstantClock>,
    ) -> Self {
        Self {
            inner: Arc::new(Mutex::new(HashMap::new())),
            threshold: threshold.max(1),
            cooldown,
            clock,
        }
    }

    /// Creates a registry using the system clock.
    pub(crate) fn with_system_clock(threshold: u32, cooldown: Duration) -> Self {
        Self::new(threshold, cooldown, Arc::new(SystemClock))
    }

    /// Creates a registry from executor policy.
    pub(crate) fn from_config(config: &ExecutorConfig) -> Self {
        Self::with_system_clock(
            config.circuit_failure_threshold,
            Duration::from_millis(config.circuit_cooldown_ms),
        )
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<String, CircuitPhase>> {
        self.inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn retry_after_ms(&self, until: Instant) -> u64 {
        let now = self.clock.now();
        if until > now {
            u64::try_from((until - now).as_millis())
                .unwrap_or(u64::MAX)
                .max(1)
        } else {
            1
        }
    }

    fn cooldown_retry_after_ms(&self) -> u64 {
        u64::try_from(self.cooldown.as_millis())
            .unwrap_or(u64::MAX)
            .max(1)
    }

    /// Decides whether this target may start an upstream call.
    pub(crate) fn admit(&self, target_id: &str) -> CircuitAdmission {
        let mut map = self.lock();
        let now = self.clock.now();
        let entry = map.entry(target_id.to_string()).or_default();
        match *entry {
            CircuitPhase::Closed { .. } => CircuitAdmission::Allow,
            CircuitPhase::Open { opened_until } if now < opened_until => {
                CircuitAdmission::Reject {
                    retry_after_ms: self.retry_after_ms(opened_until),
                }
            }
            CircuitPhase::Open { .. } => {
                *entry = CircuitPhase::HalfOpen { in_flight: true };
                CircuitAdmission::Probe(ProbeGuard {
                    registry: self.clone(),
                    target_id: target_id.to_string(),
                    settled: false,
                })
            }
            CircuitPhase::HalfOpen { in_flight: true } => CircuitAdmission::Reject {
                retry_after_ms: self.cooldown_retry_after_ms(),
            },
            CircuitPhase::HalfOpen { in_flight: false } => {
                *entry = CircuitPhase::HalfOpen { in_flight: true };
                CircuitAdmission::Probe(ProbeGuard {
                    registry: self.clone(),
                    target_id: target_id.to_string(),
                    settled: false,
                })
            }
        }
    }

    /// Resets the target to closed after a successful hop.
    pub(crate) fn record_success(&self, target_id: &str) {
        let mut map = self.lock();
        map.insert(target_id.to_string(), CircuitPhase::Closed { failures: 0 });
    }

    /// Counts a completed hop's eligible transient failure.
    pub(crate) fn record_failure(&self, target_id: &str) {
        let mut map = self.lock();
        let now = self.clock.now();
        let entry = map.entry(target_id.to_string()).or_default();
        match *entry {
            CircuitPhase::Closed { failures } => {
                let next = failures.saturating_add(1);
                if next >= self.threshold {
                    tracing::warn!(
                        target_id = %target_id,
                        threshold = self.threshold,
                        open_duration_ms = self.cooldown_retry_after_ms(),
                        "Circuit breaker opened for target"
                    );
                    *entry = CircuitPhase::Open {
                        opened_until: now + self.cooldown,
                    };
                } else {
                    *entry = CircuitPhase::Closed { failures: next };
                }
            }
            CircuitPhase::HalfOpen { .. } => {
                *entry = CircuitPhase::Open {
                    opened_until: now + self.cooldown,
                };
            }
            CircuitPhase::Open { .. } => {}
        }
    }

    fn finish_probe(&self, target_id: &str, outcome: ProbeOutcome) {
        match outcome {
            ProbeOutcome::Success | ProbeOutcome::Permanent => {
                self.record_success(target_id);
            }
            ProbeOutcome::TransientFailure => self.record_failure(target_id),
        }
    }

    fn release_probe(&self, target_id: &str) {
        let mut map = self.lock();
        if let Some(CircuitPhase::HalfOpen { in_flight }) = map.get_mut(target_id) {
            *in_flight = false;
        }
    }
}

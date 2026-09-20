//! Target-scoped circuit breakers with generation-owned completions.
//!
//! Circuits are keyed by catalog target identity, not vendor. One model's
//! transient failure can open its own circuit without blocking a healthy
//! same-provider fallback. At most one recovery probe is in flight per
//! target.
//!
//! Every closed admission and half-open probe receives a generation/phase
//! token. Completions apply only while that ownership is still current.
//! Opening, probe admission, and recovery advance generation so a stale
//! success cannot close a newer open circuit, a stale failure cannot reopen
//! after recovery, and probe Drop releases only its matching half-open
//! ownership. Same-generation closed completions still count in completion
//! order. Request results remain deliverable even when the circuit write is
//! ignored.

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

struct CircuitState {
    generation: u64,
    phase: CircuitPhase,
}

impl Default for CircuitState {
    fn default() -> Self {
        Self {
            generation: 0,
            phase: CircuitPhase::Closed { failures: 0 },
        }
    }
}

/// Result of asking a target circuit whether a hop may start.
pub(crate) enum CircuitAdmission {
    /// Closed circuit; use the normal retry budget.
    Allow(CircuitPermit),
    /// Open, or a probe is already in flight. Skip without an attempt.
    Reject { retry_after_ms: u64 },
    /// This caller owns the single half-open probe.
    Probe(CircuitPermit),
}

#[derive(Clone, Copy)]
enum PermitKind {
    Closed,
    Probe,
}

#[derive(Clone, Copy)]
enum CompletionOutcome {
    Success,
    TransientFailure,
    Permanent,
}

/// Ownership token issued at admission.
///
/// Completions apply only while the target's generation and phase still
/// match this token. Dropping an unsettled probe permit releases only that
/// probe's half-open ownership.
pub(crate) struct CircuitPermit {
    registry: TargetCircuitRegistry,
    target_id: String,
    generation: u64,
    kind: PermitKind,
    settled: bool,
}

impl CircuitPermit {
    /// Records a healthy hop or probe against this admission.
    pub(crate) fn success(mut self) {
        self.settle(CompletionOutcome::Success);
    }

    /// Records an eligible transient failure against this admission.
    pub(crate) fn transient_failure(mut self) {
        self.settle(CompletionOutcome::TransientFailure);
    }

    /// Drops half-open ownership without counting a permanent error as
    /// transient.
    pub(crate) fn ignore_permanent(mut self) {
        self.settle(CompletionOutcome::Permanent);
    }

    fn settle(&mut self, outcome: CompletionOutcome) {
        if self.settled {
            return;
        }
        self.settled = true;
        self.registry
            .complete(&self.target_id, self.generation, self.kind, outcome);
    }
}

impl Drop for CircuitPermit {
    fn drop(&mut self) {
        if !self.settled && matches!(self.kind, PermitKind::Probe) {
            self.registry
                .release_probe(&self.target_id, self.generation);
        }
    }
}

/// In-process circuit map keyed by catalog target ID.
#[derive(Clone)]
pub(crate) struct TargetCircuitRegistry {
    inner: Arc<Mutex<HashMap<String, CircuitState>>>,
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

    /// True when the target is closed and can accept generation.
    ///
    /// Open and half-open states are not usable. Readiness must not call
    /// [`Self::admit`], which would start a recovery probe.
    pub(crate) fn target_is_usable(&self, target_id: &str) -> bool {
        let map = self.lock();
        match map.get(target_id).map(|state| state.phase) {
            None | Some(CircuitPhase::Closed { .. }) => true,
            Some(CircuitPhase::Open { .. } | CircuitPhase::HalfOpen { .. }) => false,
        }
    }

    /// Opens a target for tests without recording hop failures.
    #[cfg(test)]
    pub(crate) fn force_open(&self, target_id: &str) {
        let mut map = self.lock();
        let now = self.clock.now();
        let state = map.entry(target_id.to_string()).or_default();
        self.set_open(state, now);
    }

    /// Closes a target for tests without a recovery probe.
    #[cfg(test)]
    pub(crate) fn force_close(&self, target_id: &str) {
        let mut map = self.lock();
        let state = map.entry(target_id.to_string()).or_default();
        Self::close_circuit(state);
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<String, CircuitState>> {
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

    fn closed_permit(&self, target_id: &str, generation: u64) -> CircuitPermit {
        CircuitPermit {
            registry: self.clone(),
            target_id: target_id.to_string(),
            generation,
            kind: PermitKind::Closed,
            settled: false,
        }
    }

    fn probe_permit(&self, target_id: &str, generation: u64) -> CircuitPermit {
        CircuitPermit {
            registry: self.clone(),
            target_id: target_id.to_string(),
            generation,
            kind: PermitKind::Probe,
            settled: false,
        }
    }

    fn start_probe(state: &mut CircuitState) {
        state.generation = state.generation.wrapping_add(1);
        state.phase = CircuitPhase::HalfOpen { in_flight: true };
    }

    fn set_open(&self, state: &mut CircuitState, now: Instant) {
        state.generation = state.generation.wrapping_add(1);
        state.phase = CircuitPhase::Open {
            opened_until: now + self.cooldown,
        };
    }

    fn close_circuit(state: &mut CircuitState) {
        state.generation = state.generation.wrapping_add(1);
        state.phase = CircuitPhase::Closed { failures: 0 };
    }

    /// Decides whether this target may start an upstream call.
    pub(crate) fn admit(&self, target_id: &str) -> CircuitAdmission {
        let mut map = self.lock();
        let now = self.clock.now();
        let entry = map.entry(target_id.to_string()).or_default();
        match entry.phase {
            CircuitPhase::Closed { .. } => {
                CircuitAdmission::Allow(self.closed_permit(target_id, entry.generation))
            }
            CircuitPhase::Open { opened_until } if now < opened_until => {
                CircuitAdmission::Reject {
                    retry_after_ms: self.retry_after_ms(opened_until),
                }
            }
            CircuitPhase::Open { .. } => {
                Self::start_probe(entry);
                CircuitAdmission::Probe(self.probe_permit(target_id, entry.generation))
            }
            CircuitPhase::HalfOpen { in_flight: true } => CircuitAdmission::Reject {
                retry_after_ms: self.cooldown_retry_after_ms(),
            },
            CircuitPhase::HalfOpen { in_flight: false } => {
                Self::start_probe(entry);
                CircuitAdmission::Probe(self.probe_permit(target_id, entry.generation))
            }
        }
    }

    fn complete(
        &self,
        target_id: &str,
        generation: u64,
        kind: PermitKind,
        outcome: CompletionOutcome,
    ) {
        let mut map = self.lock();
        let now = self.clock.now();
        let Some(state) = map.get_mut(target_id) else {
            return;
        };
        if state.generation != generation {
            return;
        }
        match (kind, state.phase, outcome) {
            (
                PermitKind::Closed,
                CircuitPhase::Closed { .. },
                CompletionOutcome::Success,
            ) => {
                state.phase = CircuitPhase::Closed { failures: 0 };
            }
            (
                PermitKind::Closed,
                CircuitPhase::Closed { failures },
                CompletionOutcome::TransientFailure,
            ) => {
                let next = failures.saturating_add(1);
                if next >= self.threshold {
                    tracing::warn!(
                        target_id = %target_id,
                        threshold = self.threshold,
                        open_duration_ms = self.cooldown_retry_after_ms(),
                        "Circuit breaker opened for target"
                    );
                    self.set_open(state, now);
                } else {
                    state.phase = CircuitPhase::Closed { failures: next };
                }
            }
            (
                PermitKind::Probe,
                CircuitPhase::HalfOpen { .. },
                CompletionOutcome::Success | CompletionOutcome::Permanent,
            ) => {
                Self::close_circuit(state);
            }
            (
                PermitKind::Probe,
                CircuitPhase::HalfOpen { .. },
                CompletionOutcome::TransientFailure,
            ) => {
                self.set_open(state, now);
            }
            _ => {}
        }
    }

    fn release_probe(&self, target_id: &str, generation: u64) {
        let mut map = self.lock();
        let Some(state) = map.get_mut(target_id) else {
            return;
        };
        if state.generation != generation {
            return;
        }
        if let CircuitPhase::HalfOpen { in_flight } = &mut state.phase {
            *in_flight = false;
        }
    }
}

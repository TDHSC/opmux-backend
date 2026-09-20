//! Execution, circuit, deadline, overload, and usage metric recording.
//!
//! Production uses the process Prometheus recorder. Tests inject
//! [`NoopExecutionMetrics`] unless `/metrics` is enabled, so fixtures that
//! do not scrape cannot create user-specific series.

use super::labels::{bound_catalog_id, AttemptOutcome, CircuitStateLabel};
use metrics::{counter, describe_counter, describe_gauge, gauge};
use std::sync::Arc;

/// Provider attempts by target and bounded outcome class.
pub const EXECUTION_ATTEMPTS_TOTAL: &str = "gateway_execution_attempts_total";
/// Extra provider calls on the same target after the first attempt.
pub const EXECUTION_RETRIES_TOTAL: &str = "gateway_execution_retries_total";
/// Fallback hops that execution actually considered.
pub const EXECUTION_FALLBACKS_TOTAL: &str = "gateway_execution_fallbacks_total";
/// Circuit phase transitions by target and destination phase.
pub const CIRCUIT_TRANSITIONS_TOTAL: &str = "gateway_circuit_transitions_total";
/// Current circuit phase gauge by target (0 closed, 1 half-open, 2 open).
pub const CIRCUIT_STATE: &str = "gateway_circuit_state";
/// Overall protected-request deadline expirations.
pub const DEADLINE_EXCEEDED_TOTAL: &str = "gateway_deadline_exceeded_total";
/// Local generation admission rejections. Distinct from provider throttling.
pub const OVERLOAD_REJECTED_TOTAL: &str = "gateway_overload_rejected_total";
/// Validated prompt tokens from successful responses only.
pub const SUCCESSFUL_PROMPT_TOKENS_TOTAL: &str = "gateway_successful_prompt_tokens_total";
/// Validated completion tokens from successful responses only.
pub const SUCCESSFUL_COMPLETION_TOKENS_TOTAL: &str =
    "gateway_successful_completion_tokens_total";

/// Records bounded execution observations.
pub trait ExecutionMetrics: Send + Sync {
    /// Records one started provider attempt and its outcome class.
    fn record_attempt(&self, target_id: &str, outcome: AttemptOutcome);

    /// Records a retry call on the same target after a prior attempt.
    fn record_retry(&self, target_id: &str);

    /// Records that execution moved from a failed hop to a later target.
    fn record_fallback(&self, from_target: &str, to_target: &str);

    /// Records a circuit phase change.
    fn record_circuit_transition(&self, target_id: &str, to_state: CircuitStateLabel);

    /// Sets the current circuit phase gauge for a target.
    fn set_circuit_state(&self, target_id: &str, state: CircuitStateLabel);

    /// Records that the overall protected-request deadline elapsed.
    fn record_deadline_exceeded(&self);

    /// Records a local generation-admission rejection.
    fn record_overload_rejected(&self);

    /// Records validated successful-response usage once.
    fn record_successful_usage(
        &self,
        target_id: &str,
        prompt_tokens: i64,
        completion_tokens: i64,
    );
}

/// One recorded execution observation. Used by unit tests.
#[cfg(test)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MetricEvent {
    /// Started provider attempt with a bounded outcome.
    Attempt {
        /// Configured target identifier.
        target: String,
        /// Bounded outcome class.
        outcome: AttemptOutcome,
    },
    /// Retry on the same target after a prior attempt.
    Retry {
        /// Configured target identifier.
        target: String,
    },
    /// Evaluated fallback hop.
    Fallback {
        /// Last evaluated target.
        from: String,
        /// Next evaluated target.
        to: String,
    },
    /// Circuit phase change.
    CircuitTransition {
        /// Configured target identifier.
        target: String,
        /// Destination phase.
        to_state: CircuitStateLabel,
    },
    /// Circuit gauge update.
    CircuitState {
        /// Configured target identifier.
        target: String,
        /// Current phase.
        state: CircuitStateLabel,
    },
    /// Overall protected-request deadline expiry.
    DeadlineExceeded,
    /// Local generation admission rejection.
    OverloadRejected,
    /// Validated successful-response usage.
    Usage {
        /// Configured target identifier.
        target: String,
        /// Prompt tokens.
        prompt: i64,
        /// Completion tokens.
        completion: i64,
    },
}

/// In-memory execution metric sink for focused unit tests.
#[cfg(test)]
#[derive(Debug, Default, Clone)]
pub struct RecordingExecutionMetrics {
    events: std::sync::Arc<std::sync::Mutex<Vec<MetricEvent>>>,
}

#[cfg(test)]
impl RecordingExecutionMetrics {
    /// Returns a snapshot of recorded events in order.
    pub fn events(&self) -> Vec<MetricEvent> {
        self.events
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    /// Returns recorded attempts as `(target, outcome)`.
    pub fn attempts(&self) -> Vec<(String, AttemptOutcome)> {
        self.events()
            .into_iter()
            .filter_map(|event| match event {
                MetricEvent::Attempt { target, outcome } => Some((target, outcome)),
                _ => None,
            })
            .collect()
    }

    /// Returns recorded retry target identifiers.
    pub fn retries(&self) -> Vec<String> {
        self.events()
            .into_iter()
            .filter_map(|event| match event {
                MetricEvent::Retry { target } => Some(target),
                _ => None,
            })
            .collect()
    }

    /// Returns recorded fallback hops as `(from, to)`.
    pub fn fallbacks(&self) -> Vec<(String, String)> {
        self.events()
            .into_iter()
            .filter_map(|event| match event {
                MetricEvent::Fallback { from, to } => Some((from, to)),
                _ => None,
            })
            .collect()
    }

    /// Returns recorded circuit transitions as `(target, to_state)`.
    pub fn circuit_transitions(&self) -> Vec<(String, CircuitStateLabel)> {
        self.events()
            .into_iter()
            .filter_map(|event| match event {
                MetricEvent::CircuitTransition { target, to_state } => {
                    Some((target, to_state))
                }
                _ => None,
            })
            .collect()
    }

    /// Returns recorded successful-usage triples.
    pub fn usage(&self) -> Vec<(String, i64, i64)> {
        self.events()
            .into_iter()
            .filter_map(|event| match event {
                MetricEvent::Usage {
                    target,
                    prompt,
                    completion,
                } => Some((target, prompt, completion)),
                _ => None,
            })
            .collect()
    }

    /// Returns how many overall deadline events were recorded.
    pub fn deadline_count(&self) -> usize {
        self.events()
            .into_iter()
            .filter(|event| matches!(event, MetricEvent::DeadlineExceeded))
            .count()
    }

    fn push(&self, event: MetricEvent) {
        self.events
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .push(event);
    }
}

#[cfg(test)]
impl ExecutionMetrics for RecordingExecutionMetrics {
    fn record_attempt(&self, target_id: &str, outcome: AttemptOutcome) {
        self.push(MetricEvent::Attempt {
            target: bound_catalog_id(target_id).to_string(),
            outcome,
        });
    }

    fn record_retry(&self, target_id: &str) {
        self.push(MetricEvent::Retry {
            target: bound_catalog_id(target_id).to_string(),
        });
    }

    fn record_fallback(&self, from_target: &str, to_target: &str) {
        self.push(MetricEvent::Fallback {
            from: bound_catalog_id(from_target).to_string(),
            to: bound_catalog_id(to_target).to_string(),
        });
    }

    fn record_circuit_transition(&self, target_id: &str, to_state: CircuitStateLabel) {
        self.push(MetricEvent::CircuitTransition {
            target: bound_catalog_id(target_id).to_string(),
            to_state,
        });
        self.set_circuit_state(target_id, to_state);
    }

    fn set_circuit_state(&self, target_id: &str, state: CircuitStateLabel) {
        self.push(MetricEvent::CircuitState {
            target: bound_catalog_id(target_id).to_string(),
            state,
        });
    }

    fn record_deadline_exceeded(&self) {
        self.push(MetricEvent::DeadlineExceeded);
    }

    fn record_overload_rejected(&self) {
        self.push(MetricEvent::OverloadRejected);
    }

    fn record_successful_usage(
        &self,
        target_id: &str,
        prompt_tokens: i64,
        completion_tokens: i64,
    ) {
        self.push(MetricEvent::Usage {
            target: bound_catalog_id(target_id).to_string(),
            prompt: prompt_tokens,
            completion: completion_tokens,
        });
    }
}

/// Discards observations. Used when metrics export is disabled.
#[derive(Debug, Default, Clone, Copy)]
pub struct NoopExecutionMetrics;

impl ExecutionMetrics for NoopExecutionMetrics {
    fn record_attempt(&self, _target_id: &str, _outcome: AttemptOutcome) {}

    fn record_retry(&self, _target_id: &str) {}

    fn record_fallback(&self, _from_target: &str, _to_target: &str) {}

    fn record_circuit_transition(&self, _target_id: &str, _to_state: CircuitStateLabel) {}

    fn set_circuit_state(&self, _target_id: &str, _state: CircuitStateLabel) {}

    fn record_deadline_exceeded(&self) {}

    fn record_overload_rejected(&self) {}

    fn record_successful_usage(
        &self,
        _target_id: &str,
        _prompt_tokens: i64,
        _completion_tokens: i64,
    ) {
    }
}

/// Prometheus-backed recorder using the process-global exporter.
#[derive(Debug, Default, Clone, Copy)]
pub struct PrometheusExecutionMetrics;

impl ExecutionMetrics for PrometheusExecutionMetrics {
    fn record_attempt(&self, target_id: &str, outcome: AttemptOutcome) {
        let target = bound_catalog_id(target_id).to_string();
        let outcome = outcome.as_str().to_string();
        counter!(
            EXECUTION_ATTEMPTS_TOTAL,
            &[("outcome", outcome), ("target", target)]
        )
        .increment(1);
    }

    fn record_retry(&self, target_id: &str) {
        let target = bound_catalog_id(target_id).to_string();
        counter!(EXECUTION_RETRIES_TOTAL, &[("target", target)]).increment(1);
    }

    fn record_fallback(&self, from_target: &str, to_target: &str) {
        let from_target = bound_catalog_id(from_target).to_string();
        let to_target = bound_catalog_id(to_target).to_string();
        counter!(
            EXECUTION_FALLBACKS_TOTAL,
            &[("from_target", from_target), ("to_target", to_target)]
        )
        .increment(1);
    }

    fn record_circuit_transition(&self, target_id: &str, to_state: CircuitStateLabel) {
        let target = bound_catalog_id(target_id).to_string();
        let to_state_label = to_state.as_str().to_string();
        counter!(
            CIRCUIT_TRANSITIONS_TOTAL,
            &[("target", target), ("to_state", to_state_label)]
        )
        .increment(1);
        self.set_circuit_state(target_id, to_state);
    }

    fn set_circuit_state(&self, target_id: &str, state: CircuitStateLabel) {
        let target = bound_catalog_id(target_id).to_string();
        gauge!(CIRCUIT_STATE, &[("target", target)]).set(state.as_gauge());
    }

    fn record_deadline_exceeded(&self) {
        counter!(DEADLINE_EXCEEDED_TOTAL).increment(1);
    }

    fn record_overload_rejected(&self) {
        counter!(OVERLOAD_REJECTED_TOTAL).increment(1);
    }

    fn record_successful_usage(
        &self,
        target_id: &str,
        prompt_tokens: i64,
        completion_tokens: i64,
    ) {
        let target = bound_catalog_id(target_id).to_string();
        let prompt = u64::try_from(prompt_tokens.max(0)).unwrap_or(0);
        let completion = u64::try_from(completion_tokens.max(0)).unwrap_or(0);
        counter!(
            SUCCESSFUL_PROMPT_TOKENS_TOTAL,
            &[("target", target.clone())]
        )
        .increment(prompt);
        counter!(SUCCESSFUL_COMPLETION_TOKENS_TOTAL, &[("target", target)])
            .increment(completion);
    }
}

/// Returns a sink that records to Prometheus only when metrics are enabled.
pub fn execution_metrics_sink(enabled: bool) -> Arc<dyn ExecutionMetrics> {
    if enabled {
        Arc::new(PrometheusExecutionMetrics)
    } else {
        Arc::new(NoopExecutionMetrics)
    }
}

/// Registers HELP/TYPE metadata for execution metric families.
pub(super) fn describe_execution_metrics() {
    describe_counter!(
        EXECUTION_ATTEMPTS_TOTAL,
        "Started provider attempts by configured target and bounded outcome class."
    );
    describe_counter!(
        EXECUTION_RETRIES_TOTAL,
        "Retry provider calls after a prior attempt on the same target."
    );
    describe_counter!(
        EXECUTION_FALLBACKS_TOTAL,
        "Fallback hops from one configured target to a later configured target."
    );
    describe_counter!(
        CIRCUIT_TRANSITIONS_TOTAL,
        "Target circuit transitions by destination phase."
    );
    describe_gauge!(
        CIRCUIT_STATE,
        "Current target circuit phase (0 closed, 1 half-open, 2 open)."
    );
    describe_counter!(
        DEADLINE_EXCEEDED_TOTAL,
        "Protected-request overall deadline expirations."
    );
    describe_counter!(
        OVERLOAD_REJECTED_TOTAL,
        "Local generation admission rejections. Distinct from provider throttling."
    );
    describe_counter!(
        SUCCESSFUL_PROMPT_TOKENS_TOTAL,
        "Validated prompt tokens from successful responses only."
    );
    describe_counter!(
        SUCCESSFUL_COMPLETION_TOKENS_TOTAL,
        "Validated completion tokens from successful responses only."
    );
}

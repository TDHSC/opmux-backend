//! Direct controlled-clock and cancellation tests for target circuits.

use super::circuit::{
    CircuitAdmission, CircuitPermit, ManualClock, TargetCircuitRegistry,
};
use crate::core::contracts::RoutePlan;
use crate::core::deadline::RequestDeadline;
use crate::features::executor::{
    config::ExecutorConfig,
    error::ExecutorError,
    models::{ExecutionParams, ExecutionResult},
    repository::ExecutorRepository,
    service::ExecutorService,
    vendors::LLMVendor,
};
use async_trait::async_trait;
use serde_json::json;
use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::Notify;

const MODEL_A: &str = "circuit-model-a";
const MODEL_B: &str = "circuit-model-b";

/// Retained release: a flag plus `Notify`, so a wakeup is not lost if
/// `release` runs before the waiter starts `notified().await`.
struct ReleaseLatch {
    released: AtomicBool,
    notify: Notify,
}

impl ReleaseLatch {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            released: AtomicBool::new(false),
            notify: Notify::new(),
        })
    }

    fn release(&self) {
        self.released.store(true, Ordering::Release);
        self.notify.notify_waiters();
    }

    async fn wait(&self) {
        loop {
            if self.released.load(Ordering::Acquire) {
                return;
            }
            let notified = self.notify.notified();
            if self.released.load(Ordering::Acquire) {
                return;
            }
            notified.await;
        }
    }
}

struct ScriptedStep {
    outcome: Result<ExecutionResult, ExecutorError>,
    hold: Option<Arc<ReleaseLatch>>,
}

fn ready_step(outcome: Result<ExecutionResult, ExecutorError>) -> ScriptedStep {
    ScriptedStep {
        outcome,
        hold: None,
    }
}

fn held_step(
    latch: &Arc<ReleaseLatch>,
    outcome: Result<ExecutionResult, ExecutorError>,
) -> ScriptedStep {
    ScriptedStep {
        outcome,
        hold: Some(latch.clone()),
    }
}

struct ScriptedVendor {
    calls: Arc<Mutex<Vec<String>>>,
    scripts: Mutex<HashMap<String, VecDeque<ScriptedStep>>>,
    release: Arc<ReleaseLatch>,
    hold_calls: Arc<AtomicUsize>,
}

impl ScriptedVendor {
    fn new(
        scripts: HashMap<String, Vec<Result<ExecutionResult, ExecutorError>>>,
    ) -> Self {
        Self::with_hold(scripts, None)
    }

    fn with_hold(
        scripts: HashMap<String, Vec<Result<ExecutionResult, ExecutorError>>>,
        hold_model: Option<&str>,
    ) -> Self {
        let release = ReleaseLatch::new();
        let steps = scripts
            .into_iter()
            .map(|(model, outcomes)| {
                let hold = hold_model.is_some_and(|held| held == model);
                let latch = hold.then(|| release.clone());
                (
                    model,
                    outcomes
                        .into_iter()
                        .map(|outcome| ScriptedStep {
                            outcome,
                            hold: latch.clone(),
                        })
                        .collect(),
                )
            })
            .collect();
        Self::from_steps(steps, release)
    }

    fn from_steps(
        scripts: HashMap<String, Vec<ScriptedStep>>,
        release: Arc<ReleaseLatch>,
    ) -> Self {
        Self {
            calls: Arc::new(Mutex::new(Vec::new())),
            scripts: Mutex::new(
                scripts
                    .into_iter()
                    .map(|(model, outcomes)| (model, VecDeque::from(outcomes)))
                    .collect(),
            ),
            release,
            hold_calls: Arc::new(AtomicUsize::new(0)),
        }
    }
}

#[async_trait]
impl LLMVendor for ScriptedVendor {
    async fn execute(
        &self,
        model: &str,
        _target_id: &str,
        _params: ExecutionParams,
    ) -> Result<ExecutionResult, ExecutorError> {
        self.calls
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .push(model.to_string());
        let step = self
            .scripts
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get_mut(model)
            .and_then(VecDeque::pop_front)
            .unwrap_or_else(|| panic!("unexpected extra call for {model}"));
        if let Some(latch) = step.hold {
            self.hold_calls.fetch_add(1, Ordering::SeqCst);
            latch.wait().await;
        }
        step.outcome
    }

    fn vendor_id(&self) -> &str {
        "openai"
    }

    fn supports_model(&self, _model: &str) -> bool {
        true
    }

    fn calculate_cost(
        &self,
        _prompt_tokens: i64,
        _completion_tokens: i64,
        _target_id: &str,
    ) -> Result<f64, ExecutorError> {
        Ok(0.0)
    }

    async fn health_check(&self, _timeout_secs: u64) -> Result<(), ExecutorError> {
        Ok(())
    }
}

fn success(model: &str, content: &str) -> ExecutionResult {
    ExecutionResult {
        content: content.to_string(),
        role: "assistant".to_string(),
        model_used: model.to_string(),
        prompt_tokens: 1,
        completion_tokens: 1,
        total_cost: 0.0,
        finish_reason: "stop".to_string(),
    }
}

fn network() -> ExecutorError {
    ExecutorError::NetworkError("transient".to_string())
}

fn hop(target_id: &str, model_id: &str) -> RoutePlan {
    RoutePlan {
        vendor_id: "openai".to_string(),
        target_id: target_id.to_string(),
        model_id: model_id.to_string(),
        max_output_tokens: 512,
        fallback_plans: Vec::new(),
    }
}

fn chain_ab() -> RoutePlan {
    RoutePlan {
        fallback_plans: vec![hop("beta", MODEL_B)],
        ..hop("alpha", MODEL_A)
    }
}

fn payload() -> serde_json::Value {
    json!({ "messages": [{ "role": "user", "content": "circuit" }] })
}

fn generous_deadline() -> RequestDeadline {
    RequestDeadline::from_timeout(Duration::from_secs(30))
}

fn service_from(
    vendor: ScriptedVendor,
    clock: Arc<ManualClock>,
    cooldown: Duration,
) -> ExecutorService {
    let mut vendors = HashMap::new();
    vendors.insert("openai".to_string(), Arc::new(vendor) as Arc<dyn LLMVendor>);
    let mut config = ExecutorConfig::mock_policy(0, 10_000);
    config.max_total_attempts = 8;
    config.circuit_failure_threshold = 1;
    config.circuit_cooldown_ms = u64::try_from(cooldown.as_millis()).unwrap_or(u64::MAX);
    let mut service =
        ExecutorService::from_repository(ExecutorRepository { vendors }, config);
    service.circuits = TargetCircuitRegistry::new(1, cooldown, clock);
    service
}

async fn wait_holds(holds: &AtomicUsize, expected: usize) {
    let started = std::time::Instant::now();
    while holds.load(Ordering::SeqCst) < expected {
        if started.elapsed() > Duration::from_secs(1) {
            panic!(
                "timed out waiting for {expected} held probes, saw {}",
                holds.load(Ordering::SeqCst)
            );
        }
        tokio::task::yield_now().await;
        tokio::time::sleep(Duration::from_millis(1)).await;
    }
}

#[tokio::test]
async fn half_open_admits_only_one_probe_and_waiters_use_fallback() {
    let clock = Arc::new(ManualClock::new());
    let cooldown = Duration::from_millis(40);
    let opener = ScriptedVendor::new(HashMap::from([
        (MODEL_A.to_string(), vec![Err(network())]),
        (MODEL_B.to_string(), vec![Ok(success(MODEL_B, "b-open"))]),
    ]));
    let service = Arc::new(service_from(opener, clock.clone(), cooldown));
    service
        .execute(&chain_ab(), &payload(), generous_deadline())
        .await
        .expect("B remains usable while A opens");
    clock.advance(cooldown + Duration::from_millis(1));

    let probe_vendor = ScriptedVendor::with_hold(
        HashMap::from([
            (MODEL_A.to_string(), vec![Ok(success(MODEL_A, "probe-ok"))]),
            (
                MODEL_B.to_string(),
                vec![
                    Ok(success(MODEL_B, "b-waiter")),
                    Ok(success(MODEL_B, "b-waiter")),
                    Ok(success(MODEL_B, "b-waiter")),
                    Ok(success(MODEL_B, "b-waiter")),
                ],
            ),
        ]),
        Some(MODEL_A),
    );
    let holds = probe_vendor.hold_calls.clone();
    let release = probe_vendor.release.clone();
    let calls = probe_vendor.calls.clone();
    let mut probing = service_from(probe_vendor, clock.clone(), cooldown);
    probing.circuits = service.circuits.clone();
    let probing = Arc::new(probing);

    let probe = {
        let probing = probing.clone();
        tokio::spawn(async move {
            probing
                .execute(&chain_ab(), &payload(), generous_deadline())
                .await
        })
    };
    wait_holds(&holds, 1).await;

    let mut extras = Vec::new();
    for _ in 0..4 {
        let probing = probing.clone();
        extras.push(tokio::spawn(async move {
            probing
                .execute(&chain_ab(), &payload(), generous_deadline())
                .await
        }));
    }
    tokio::time::sleep(Duration::from_millis(30)).await;
    assert_eq!(holds.load(Ordering::SeqCst), 1);
    let during = calls
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clone();
    assert_eq!(during.iter().filter(|model| *model == MODEL_A).count(), 1);
    assert!(during.iter().any(|model| model == MODEL_B));

    release.release();
    let probe_result = probe.await.expect("join").expect("healthy probe closes A");
    assert_eq!(probe_result.content, "probe-ok");
    for extra in extras {
        extra
            .await
            .expect("join extra")
            .expect("healthy B remains usable during the probe");
    }
}

#[tokio::test]
async fn cancelled_probe_releases_ownership_for_later_probe() {
    let clock = Arc::new(ManualClock::new());
    let cooldown = Duration::from_millis(40);
    let opener =
        ScriptedVendor::new(HashMap::from([(MODEL_A.to_string(), vec![Err(network())])]));
    let service = service_from(opener, clock.clone(), cooldown);
    let _ = service
        .execute(&hop("alpha", MODEL_A), &payload(), generous_deadline())
        .await;
    clock.advance(cooldown + Duration::from_millis(1));

    let vendor = ScriptedVendor::with_hold(
        HashMap::from([(
            MODEL_A.to_string(),
            vec![
                Ok(success(MODEL_A, "cancelled-probe")),
                Ok(success(MODEL_A, "second-probe")),
            ],
        )]),
        Some(MODEL_A),
    );
    let holds = vendor.hold_calls.clone();
    let release = vendor.release.clone();
    let mut probing = service_from(vendor, clock.clone(), cooldown);
    probing.circuits = service.circuits.clone();
    let probing = Arc::new(probing);

    let first = {
        let probing = probing.clone();
        tokio::spawn(async move {
            probing
                .execute(&hop("alpha", MODEL_A), &payload(), generous_deadline())
                .await
        })
    };
    wait_holds(&holds, 1).await;
    first.abort();
    assert!(first.await.unwrap_err().is_cancelled());

    let second = {
        let probing = probing.clone();
        tokio::spawn(async move {
            probing
                .execute(&hop("alpha", MODEL_A), &payload(), generous_deadline())
                .await
        })
    };
    wait_holds(&holds, 2).await;
    release.release();
    let recovered = second.await.expect("join").expect("later probe proceeds");
    assert_eq!(recovered.content, "second-probe");
}

#[tokio::test]
async fn deadline_during_probe_releases_ownership() {
    let clock = Arc::new(ManualClock::new());
    let cooldown = Duration::from_millis(40);
    let opener =
        ScriptedVendor::new(HashMap::from([(MODEL_A.to_string(), vec![Err(network())])]));
    let service = service_from(opener, clock.clone(), cooldown);
    let _ = service
        .execute(&hop("alpha", MODEL_A), &payload(), generous_deadline())
        .await;
    clock.advance(cooldown + Duration::from_millis(1));

    let vendor = ScriptedVendor::with_hold(
        HashMap::from([(
            MODEL_A.to_string(),
            vec![
                Ok(success(MODEL_A, "deadline-probe")),
                Ok(success(MODEL_A, "after-deadline")),
            ],
        )]),
        Some(MODEL_A),
    );
    let holds = vendor.hold_calls.clone();
    let release = vendor.release.clone();
    let mut probing = service_from(vendor, clock.clone(), cooldown);
    probing.circuits = service.circuits.clone();
    let probing = Arc::new(probing);

    let short = RequestDeadline::from_timeout(Duration::from_millis(20));
    let first = {
        let probing = probing.clone();
        tokio::spawn(async move {
            probing
                .execute(&hop("alpha", MODEL_A), &payload(), short)
                .await
        })
    };
    wait_holds(&holds, 1).await;
    match first.await.expect("join") {
        Err(ExecutorError::DeadlineExceeded) | Err(ExecutorError::TimeoutError(_)) => {}
        other => panic!("expected deadline or attempt timeout, got {other:?}"),
    }
    clock.advance(cooldown + Duration::from_millis(1));

    let second = {
        let probing = probing.clone();
        tokio::spawn(async move {
            probing
                .execute(&hop("alpha", MODEL_A), &payload(), generous_deadline())
                .await
        })
    };
    wait_holds(&holds, 2).await;
    release.release();
    second
        .await
        .expect("join")
        .expect("probe ownership must be free after timeout");
}

#[tokio::test]
async fn failed_probe_reopens_and_healthy_probe_closes() {
    let clock = Arc::new(ManualClock::new());
    let cooldown = Duration::from_millis(25);
    let opener =
        ScriptedVendor::new(HashMap::from([(MODEL_A.to_string(), vec![Err(network())])]));
    let service = service_from(opener, clock.clone(), cooldown);
    let _ = service
        .execute(&hop("alpha", MODEL_A), &payload(), generous_deadline())
        .await;
    clock.advance(cooldown + Duration::from_millis(1));

    let failing_probe =
        ScriptedVendor::new(HashMap::from([(MODEL_A.to_string(), vec![Err(network())])]));
    let fail_calls = failing_probe.calls.clone();
    let mut failing = service_from(failing_probe, clock.clone(), cooldown);
    failing.circuits = service.circuits.clone();
    assert!(matches!(
        failing
            .execute(&hop("alpha", MODEL_A), &payload(), generous_deadline())
            .await,
        Err(ExecutorError::NetworkError(_))
    ));
    assert_eq!(
        fail_calls
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .len(),
        1
    );
    assert!(matches!(
        failing
            .execute(&hop("alpha", MODEL_A), &payload(), generous_deadline())
            .await,
        Err(ExecutorError::CircuitOpen { .. })
    ));
    assert_eq!(
        fail_calls
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .len(),
        1
    );

    clock.advance(cooldown + Duration::from_millis(1));
    let healthy = ScriptedVendor::new(HashMap::from([(
        MODEL_A.to_string(),
        vec![
            Ok(success(MODEL_A, "closed")),
            Ok(success(MODEL_A, "normal")),
        ],
    )]));
    let healthy_calls = healthy.calls.clone();
    let mut closing = service_from(healthy, clock, cooldown);
    closing.circuits = service.circuits.clone();
    closing
        .execute(&hop("alpha", MODEL_A), &payload(), generous_deadline())
        .await
        .expect("healthy probe closes");
    closing
        .execute(&hop("alpha", MODEL_A), &payload(), generous_deadline())
        .await
        .expect("closed circuit allows normal calls");
    assert_eq!(
        healthy_calls
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .len(),
        2
    );
}

fn throttle() -> ExecutorError {
    ExecutorError::RateLimitExceeded {
        vendor: "openai".into(),
        retry_after_ms: Some(1),
    }
}

#[tokio::test]
async fn throttled_probe_does_not_reopen_as_transient() {
    let clock = Arc::new(ManualClock::new());
    let cooldown = Duration::from_millis(25);
    let opener = ScriptedVendor::new(HashMap::from([
        (MODEL_A.to_string(), vec![Err(network())]),
        (MODEL_B.to_string(), vec![Ok(success(MODEL_B, "b-open"))]),
    ]));
    let service = service_from(opener, clock.clone(), cooldown);
    service
        .execute(&chain_ab(), &payload(), generous_deadline())
        .await
        .expect("B remains usable while A opens");
    clock.advance(cooldown + Duration::from_millis(1));

    let probe_vendor = ScriptedVendor::new(HashMap::from([
        (
            MODEL_A.to_string(),
            vec![Err(throttle()), Ok(success(MODEL_A, "closed"))],
        ),
        (
            MODEL_B.to_string(),
            vec![Ok(success(MODEL_B, "should-not-run"))],
        ),
    ]));
    let calls = probe_vendor.calls.clone();
    let mut probing = service_from(probe_vendor, clock, cooldown);
    probing.circuits = service.circuits.clone();

    let probe = probing
        .execute(&chain_ab(), &payload(), generous_deadline())
        .await
        .expect_err("throttled probe must not fall back");
    assert!(matches!(probe, ExecutorError::RateLimitExceeded { .. }));

    let restored = probing
        .execute(&chain_ab(), &payload(), generous_deadline())
        .await
        .expect("throttled probe must not reopen A as transient");
    assert_eq!(restored.content, "closed");
    assert_eq!(
        calls
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone(),
        vec![MODEL_A.to_string(), MODEL_A.to_string()]
    );
}

fn recorded_models(calls: &Arc<Mutex<Vec<String>>>) -> Vec<String> {
    calls
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clone()
}

fn model_counts(models: &[String]) -> (usize, usize) {
    (
        models.iter().filter(|model| *model == MODEL_A).count(),
        models.iter().filter(|model| *model == MODEL_B).count(),
    )
}

fn admit_closed(registry: &TargetCircuitRegistry, target: &str) -> CircuitPermit {
    match registry.admit(target) {
        CircuitAdmission::Allow(permit) => permit,
        CircuitAdmission::Reject { .. } => {
            panic!("expected closed admission, got reject")
        }
        CircuitAdmission::Probe(_) => panic!("expected closed admission, got probe"),
    }
}

fn assert_rejected(registry: &TargetCircuitRegistry, target: &str, why: &str) {
    assert!(
        matches!(registry.admit(target), CircuitAdmission::Reject { .. }),
        "{why}"
    );
}

fn assert_closed(registry: &TargetCircuitRegistry, target: &str) {
    // Drop the permit without completing so later assertions can admit again.
    drop(admit_closed(registry, target));
}

fn open_alpha(registry: &TargetCircuitRegistry) {
    admit_closed(registry, "alpha").transient_failure();
    assert_rejected(registry, "alpha", "circuit should be open");
}

#[test]
fn same_generation_closed_failures_count_until_threshold() {
    let clock = Arc::new(ManualClock::new());
    let cooldown = Duration::from_millis(40);
    let registry = TargetCircuitRegistry::new(2, cooldown, clock);
    let first = admit_closed(&registry, "alpha");
    let second = admit_closed(&registry, "alpha");
    first.transient_failure();
    assert_closed(&registry, "alpha");
    second.transient_failure();
    assert_rejected(
        &registry,
        "alpha",
        "same-generation closed failures must count in completion order",
    );
}

#[test]
fn same_generation_closed_success_then_failure_still_counts() {
    let clock = Arc::new(ManualClock::new());
    let cooldown = Duration::from_millis(40);
    let registry = TargetCircuitRegistry::new(1, cooldown, clock);
    let first = admit_closed(&registry, "alpha");
    let second = admit_closed(&registry, "alpha");
    first.success();
    second.transient_failure();
    assert_rejected(
        &registry,
        "alpha",
        "same-generation closed failure after success must still count",
    );
}

#[test]
fn stale_success_does_not_close_open_circuit_before_cooldown() {
    let clock = Arc::new(ManualClock::new());
    let cooldown = Duration::from_millis(40);
    let registry = TargetCircuitRegistry::new(1, cooldown, clock);
    let stale = admit_closed(&registry, "alpha");
    open_alpha(&registry);
    stale.success();
    assert_rejected(
        &registry,
        "alpha",
        "stale success must not close a newer open circuit before cooldown",
    );
}

#[test]
fn stale_failure_does_not_reopen_after_successful_recovery() {
    let clock = Arc::new(ManualClock::new());
    let cooldown = Duration::from_millis(40);
    let registry = TargetCircuitRegistry::new(1, cooldown, clock.clone());
    let stale = admit_closed(&registry, "alpha");
    open_alpha(&registry);
    clock.advance(cooldown + Duration::from_millis(1));
    match registry.admit("alpha") {
        CircuitAdmission::Probe(guard) => guard.success(),
        _ => panic!("expected a half-open probe after cooldown"),
    }
    assert_closed(&registry, "alpha");
    stale.transient_failure();
    assert_closed(&registry, "alpha");
}

#[test]
fn stale_closed_completion_while_probe_held_does_not_admit_another_call() {
    let clock = Arc::new(ManualClock::new());
    let cooldown = Duration::from_millis(40);
    let registry = TargetCircuitRegistry::new(1, cooldown, clock.clone());
    let stale = admit_closed(&registry, "alpha");
    open_alpha(&registry);
    clock.advance(cooldown + Duration::from_millis(1));
    let probe = match registry.admit("alpha") {
        CircuitAdmission::Probe(guard) => guard,
        _ => panic!("expected a half-open probe after cooldown"),
    };
    stale.success();
    assert_rejected(
        &registry,
        "alpha",
        "stale closed success must not admit another call while a probe is held",
    );
    probe.success();
    assert_closed(&registry, "alpha");
}

#[tokio::test]
async fn held_stale_success_cannot_close_newer_open_circuit_before_cooldown() {
    let clock = Arc::new(ManualClock::new());
    let cooldown = Duration::from_millis(40);
    let stale_ok = ReleaseLatch::new();
    let vendor = ScriptedVendor::from_steps(
        HashMap::from([
            (
                MODEL_A.to_string(),
                vec![
                    held_step(&stale_ok, Ok(success(MODEL_A, "stale-success"))),
                    ready_step(Err(network())),
                    ready_step(Ok(success(MODEL_A, "should-not-run-before-cooldown"))),
                ],
            ),
            (
                MODEL_B.to_string(),
                vec![
                    ready_step(Ok(success(MODEL_B, "b-open"))),
                    ready_step(Ok(success(MODEL_B, "b-still-open"))),
                    ready_step(Ok(success(MODEL_B, "b-after-stale"))),
                ],
            ),
        ]),
        stale_ok.clone(),
    );
    let holds = vendor.hold_calls.clone();
    let calls = vendor.calls.clone();
    let service = Arc::new(service_from(vendor, clock.clone(), cooldown));

    let stale = {
        let service = service.clone();
        tokio::spawn(async move {
            service
                .execute(&chain_ab(), &payload(), generous_deadline())
                .await
        })
    };
    wait_holds(&holds, 1).await;

    let opened = service
        .execute(&chain_ab(), &payload(), generous_deadline())
        .await
        .expect("B remains usable while A opens");
    assert_eq!(opened.content, "b-open");

    let skipped = service
        .execute(&chain_ab(), &payload(), generous_deadline())
        .await
        .expect("open A must skip to B before cooldown");
    assert_eq!(skipped.content, "b-still-open");

    stale_ok.release();
    let stale_result = stale.await.expect("join stale success");
    assert_eq!(
        stale_result
            .expect("stale success remains deliverable")
            .content,
        "stale-success"
    );

    let after_stale = service
        .execute(&chain_ab(), &payload(), generous_deadline())
        .await
        .expect("stale success must not close A before cooldown");
    assert_eq!(after_stale.content, "b-after-stale");
    let models = recorded_models(&calls);
    assert_eq!(model_counts(&models), (2, 3), "models={models:?}");
}

#[tokio::test]
async fn held_stale_failure_cannot_reopen_after_successful_recovery() {
    let clock = Arc::new(ManualClock::new());
    let cooldown = Duration::from_millis(40);
    let stale_fail = ReleaseLatch::new();
    let vendor = ScriptedVendor::from_steps(
        HashMap::from([
            (
                MODEL_A.to_string(),
                vec![
                    held_step(&stale_fail, Err(network())),
                    ready_step(Err(network())),
                    ready_step(Ok(success(MODEL_A, "probe-ok"))),
                    ready_step(Ok(success(MODEL_A, "still-closed"))),
                ],
            ),
            (
                MODEL_B.to_string(),
                vec![
                    ready_step(Ok(success(MODEL_B, "b-open"))),
                    ready_step(Ok(success(MODEL_B, "should-not-run"))),
                ],
            ),
        ]),
        stale_fail.clone(),
    );
    let holds = vendor.hold_calls.clone();
    let calls = vendor.calls.clone();
    let service = Arc::new(service_from(vendor, clock.clone(), cooldown));

    let stale = {
        let service = service.clone();
        tokio::spawn(async move {
            service
                .execute(&hop("alpha", MODEL_A), &payload(), generous_deadline())
                .await
        })
    };
    wait_holds(&holds, 1).await;

    let opened = service
        .execute(&chain_ab(), &payload(), generous_deadline())
        .await
        .expect("B remains usable while A opens");
    assert_eq!(opened.content, "b-open");
    clock.advance(cooldown + Duration::from_millis(1));

    let recovered = service
        .execute(&chain_ab(), &payload(), generous_deadline())
        .await
        .expect("healthy probe closes A");
    assert_eq!(recovered.content, "probe-ok");

    stale_fail.release();
    let stale_result = stale.await.expect("join stale failure");
    assert!(
        matches!(stale_result, Err(ExecutorError::NetworkError(_))),
        "stale failure remains deliverable"
    );

    let after_stale = service
        .execute(&chain_ab(), &payload(), generous_deadline())
        .await
        .expect("stale failure must not reopen A after recovery");
    assert_eq!(after_stale.content, "still-closed");
    assert_eq!(
        recorded_models(&calls),
        vec![
            MODEL_A.to_string(),
            MODEL_A.to_string(),
            MODEL_B.to_string(),
            MODEL_A.to_string(),
            MODEL_A.to_string(),
        ]
    );
}

#[tokio::test]
async fn stale_closed_completion_during_held_probe_does_not_admit_another_a() {
    let clock = Arc::new(ManualClock::new());
    let cooldown = Duration::from_millis(40);
    let stale_ok = ReleaseLatch::new();
    let probe_ok = ReleaseLatch::new();
    let vendor = ScriptedVendor::from_steps(
        HashMap::from([
            (
                MODEL_A.to_string(),
                vec![
                    held_step(&stale_ok, Ok(success(MODEL_A, "stale-closed"))),
                    ready_step(Err(network())),
                    held_step(&probe_ok, Ok(success(MODEL_A, "probe-ok"))),
                    ready_step(Ok(success(MODEL_A, "should-not-run"))),
                ],
            ),
            (
                MODEL_B.to_string(),
                vec![
                    ready_step(Ok(success(MODEL_B, "b-open"))),
                    ready_step(Ok(success(MODEL_B, "b-during-probe"))),
                    ready_step(Ok(success(MODEL_B, "b-after-stale"))),
                ],
            ),
        ]),
        stale_ok.clone(),
    );
    let holds = vendor.hold_calls.clone();
    let calls = vendor.calls.clone();
    let service = Arc::new(service_from(vendor, clock.clone(), cooldown));

    let stale = {
        let service = service.clone();
        tokio::spawn(async move {
            service
                .execute(&chain_ab(), &payload(), generous_deadline())
                .await
        })
    };
    wait_holds(&holds, 1).await;

    let opened = service
        .execute(&chain_ab(), &payload(), generous_deadline())
        .await
        .expect("B remains usable while A opens");
    assert_eq!(opened.content, "b-open");
    clock.advance(cooldown + Duration::from_millis(1));

    let probe = {
        let service = service.clone();
        tokio::spawn(async move {
            service
                .execute(&chain_ab(), &payload(), generous_deadline())
                .await
        })
    };
    wait_holds(&holds, 2).await;

    let during_probe = service
        .execute(&chain_ab(), &payload(), generous_deadline())
        .await
        .expect("healthy B remains usable during the probe");
    assert_eq!(during_probe.content, "b-during-probe");
    assert_eq!(holds.load(Ordering::SeqCst), 2);
    assert_eq!(model_counts(&recorded_models(&calls)), (3, 2));

    stale_ok.release();
    let stale_result = stale.await.expect("join stale closed success");
    assert_eq!(
        stale_result
            .expect("stale closed success remains deliverable")
            .content,
        "stale-closed"
    );

    let after_stale = service
        .execute(&chain_ab(), &payload(), generous_deadline())
        .await
        .expect("stale closed success must not admit another A while the probe is held");
    assert_eq!(after_stale.content, "b-after-stale");
    assert_eq!(holds.load(Ordering::SeqCst), 2);
    assert_eq!(
        model_counts(&recorded_models(&calls)),
        (3, 3),
        "no extra A probe or closed admission"
    );

    probe_ok.release();
    let probe_result = probe.await.expect("join probe");
    assert_eq!(
        probe_result
            .expect("matching probe success closes A")
            .content,
        "probe-ok"
    );
}

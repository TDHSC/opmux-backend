//! Direct controlled-clock and cancellation tests for target circuits.

use super::circuit::{ManualClock, TargetCircuitRegistry};
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
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::Notify;

const MODEL_A: &str = "circuit-model-a";
const MODEL_B: &str = "circuit-model-b";

struct ScriptedVendor {
    calls: Arc<Mutex<Vec<String>>>,
    scripts: Mutex<HashMap<String, VecDeque<Result<ExecutionResult, ExecutorError>>>>,
    hold_model: Option<String>,
    release: Arc<Notify>,
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
        Self {
            calls: Arc::new(Mutex::new(Vec::new())),
            scripts: Mutex::new(
                scripts
                    .into_iter()
                    .map(|(model, outcomes)| (model, VecDeque::from(outcomes)))
                    .collect(),
            ),
            hold_model: hold_model.map(ToOwned::to_owned),
            release: Arc::new(Notify::new()),
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
        if self.hold_model.as_deref() == Some(model) {
            self.hold_calls.fetch_add(1, Ordering::SeqCst);
            self.release.notified().await;
        }
        self.scripts
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get_mut(model)
            .and_then(VecDeque::pop_front)
            .unwrap_or_else(|| panic!("unexpected extra call for {model}"))
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

    release.notify_waiters();
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
            vec![Ok(success(MODEL_A, "second-probe"))],
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
    release.notify_waiters();
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
            vec![Ok(success(MODEL_A, "after-deadline"))],
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
    release.notify_waiters();
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

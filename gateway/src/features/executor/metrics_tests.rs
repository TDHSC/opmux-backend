//! Focused execution-metric recorder coverage for target identity, hops,
//! and cancellation-safe attempt accounting.

use crate::core::contracts::RoutePlan;
use crate::core::deadline::RequestDeadline;
use crate::core::metrics::{
    AttemptOutcome, CircuitStateLabel, RecordingExecutionMetrics,
};
use crate::features::executor::{
    config::ExecutorConfig,
    error::ExecutorError,
    models::{ExecutionParams, ExecutionResult, Message},
    repository::ExecutorRepository,
    service::ExecutorService,
    vendors::LLMVendor,
};
use async_trait::async_trait;
use serde_json::json;
use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

const DOTTED_A: &str = "alpha.primary";
const DOTTED_B: &str = "beta.fallback";
const MODEL_A: &str = "metric-model-a";
const MODEL_B: &str = "metric-model-b";
const MODEL_C: &str = "metric-model-c";

struct ScriptedVendor {
    vendor_id: String,
    models: Vec<String>,
    calls: Arc<AtomicUsize>,
    delay: Duration,
    error: Option<ExecutorError>,
    succeed_on: usize,
}

impl ScriptedVendor {
    fn fail_then_success(
        vendor_id: &str,
        model: &str,
        error: ExecutorError,
    ) -> (Self, Arc<AtomicUsize>) {
        let calls = Arc::new(AtomicUsize::new(0));
        (
            Self {
                vendor_id: vendor_id.to_string(),
                models: vec![model.to_string()],
                calls: calls.clone(),
                delay: Duration::ZERO,
                error: Some(error),
                succeed_on: 2,
            },
            calls,
        )
    }

    fn delayed_success(
        vendor_id: &str,
        model: &str,
        delay: Duration,
    ) -> (Self, Arc<AtomicUsize>) {
        let calls = Arc::new(AtomicUsize::new(0));
        (
            Self {
                vendor_id: vendor_id.to_string(),
                models: vec![model.to_string()],
                calls: calls.clone(),
                delay,
                error: None,
                succeed_on: 1,
            },
            calls,
        )
    }

    fn fail(
        vendor_id: &str,
        model: &str,
        error: ExecutorError,
        delay: Duration,
    ) -> (Self, Arc<AtomicUsize>) {
        let calls = Arc::new(AtomicUsize::new(0));
        (
            Self {
                vendor_id: vendor_id.to_string(),
                models: vec![model.to_string()],
                calls: calls.clone(),
                delay,
                error: Some(error),
                succeed_on: usize::MAX,
            },
            calls,
        )
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
        let n = self.calls.fetch_add(1, Ordering::SeqCst) + 1;
        if !self.delay.is_zero() {
            tokio::time::sleep(self.delay).await;
        }
        if n >= self.succeed_on {
            return Ok(success(model, "ok"));
        }
        Err(self
            .error
            .clone()
            .unwrap_or_else(|| ExecutorError::NetworkError("transient".into())))
    }

    fn vendor_id(&self) -> &str {
        &self.vendor_id
    }

    fn supports_model(&self, model: &str) -> bool {
        self.models.iter().any(|item| item == model)
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

struct MultiModelVendor {
    calls: Arc<std::sync::Mutex<Vec<String>>>,
    outcomes: HashMap<String, Vec<Result<ExecutionResult, ExecutorError>>>,
}

impl MultiModelVendor {
    fn new(
        outcomes: HashMap<String, Vec<Result<ExecutionResult, ExecutorError>>>,
    ) -> (Self, Arc<std::sync::Mutex<Vec<String>>>) {
        let calls = Arc::new(std::sync::Mutex::new(Vec::new()));
        (
            Self {
                calls: calls.clone(),
                outcomes,
            },
            calls,
        )
    }
}

#[async_trait]
impl LLMVendor for MultiModelVendor {
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
        let remaining = self.outcomes.get(model).cloned().unwrap_or_default();
        let idx = self
            .calls
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .iter()
            .filter(|item| item.as_str() == model)
            .count()
            .saturating_sub(1);
        remaining
            .get(idx)
            .cloned()
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
        prompt_tokens: 8,
        completion_tokens: 4,
        total_cost: 0.001,
        finish_reason: "stop".to_string(),
    }
}

fn hop(target_id: &str, model_id: &str, max_output_tokens: u32) -> RoutePlan {
    RoutePlan {
        vendor_id: "openai".to_string(),
        target_id: target_id.to_string(),
        model_id: model_id.to_string(),
        max_output_tokens,
        fallback_plans: Vec::new(),
    }
}

fn chain(primary: RoutePlan, fallbacks: Vec<RoutePlan>) -> RoutePlan {
    RoutePlan {
        fallback_plans: fallbacks,
        ..primary
    }
}

fn payload() -> serde_json::Value {
    json!({ "messages": [{ "role": "user", "content": "metrics" }] })
}

fn payload_with_tokens(max_tokens: i64) -> serde_json::Value {
    json!({
        "messages": [{ "role": "user", "content": "metrics" }],
        "max_tokens": max_tokens
    })
}

fn params() -> ExecutionParams {
    ExecutionParams {
        messages: vec![Message {
            role: "user".to_string(),
            content: "metrics".to_string(),
        }],
        temperature: None,
        max_tokens: None,
        top_p: None,
        stream: false,
    }
}

fn long_c() -> String {
    let id = format!("configured-long-target-{}", "c".repeat(50));
    assert!(id.len() > 64);
    id
}

fn service_with_metrics(
    vendors: Vec<(String, Arc<dyn LLMVendor>)>,
    max_retries: u32,
    max_total_attempts: u32,
    metrics: RecordingExecutionMetrics,
) -> ExecutorService {
    let mut vendor_map = HashMap::new();
    for (id, vendor) in vendors {
        vendor_map.insert(id, vendor);
    }
    let mut config = ExecutorConfig::mock_policy(max_retries, 10_000);
    config.max_total_attempts = max_total_attempts;
    config.backoff_cap_ms = 5_000;
    config.circuit_failure_threshold = 1;
    ExecutorService::from_repository_with_metrics(
        ExecutorRepository {
            vendors: vendor_map,
        },
        config,
        Arc::new(metrics),
    )
}

async fn wait_for_calls(calls: &AtomicUsize, expected: usize) {
    for _ in 0..50 {
        if calls.load(Ordering::SeqCst) >= expected {
            return;
        }
        tokio::task::yield_now().await;
    }
    panic!(
        "expected {expected} vendor calls, observed {}",
        calls.load(Ordering::SeqCst)
    );
}

fn network() -> ExecutorError {
    ExecutorError::NetworkError("transient".into())
}

#[tokio::test(start_paused = true)]
async fn retry_keeps_distinct_dotted_target_ids() {
    let metrics = RecordingExecutionMetrics::default();
    let (vendor, calls) = ScriptedVendor::fail_then_success(
        "openai",
        MODEL_A,
        ExecutorError::ApiCallFailed("upstream http error".into()),
    );
    let service = service_with_metrics(
        vec![("openai".to_string(), Arc::new(vendor))],
        1,
        3,
        metrics.clone(),
    );
    let result = service
        .execute(
            &hop(DOTTED_A, MODEL_A, 512),
            &payload(),
            RequestDeadline::from_timeout(Duration::from_secs(30)),
        )
        .await
        .expect("retry should succeed");
    assert_eq!(result.model_used, MODEL_A);
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    assert_eq!(
        metrics.attempts(),
        vec![
            (DOTTED_A.to_string(), AttemptOutcome::Retryable),
            (DOTTED_A.to_string(), AttemptOutcome::Success),
        ]
    );
    assert_eq!(metrics.retries(), vec![DOTTED_A.to_string()]);
    assert!(metrics.fallbacks().is_empty());
    assert_eq!(metrics.usage(), vec![(DOTTED_A.to_string(), 8, 4)]);
    assert!(!metrics
        .events()
        .iter()
        .any(|event| format!("{event:?}").contains("unknown")));
}

#[tokio::test(start_paused = true)]
async fn circuit_skip_keeps_target_ids_distinct_without_fabricated_attempts() {
    let metrics = RecordingExecutionMetrics::default();
    let (vendor, calls) = MultiModelVendor::new(HashMap::from([
        (MODEL_A.to_string(), vec![Err(network())]),
        (MODEL_B.to_string(), vec![Ok(success(MODEL_B, "b-ok"))]),
    ]));
    let service = service_with_metrics(
        vec![("openai".to_string(), Arc::new(vendor))],
        0,
        3,
        metrics.clone(),
    );
    service.force_open_target(DOTTED_A);
    let result = service
        .execute(
            &chain(
                hop(DOTTED_A, MODEL_A, 512),
                vec![hop(DOTTED_B, MODEL_B, 512)],
            ),
            &payload(),
            RequestDeadline::from_timeout(Duration::from_secs(30)),
        )
        .await
        .expect("B should succeed");
    assert_eq!(result.model_used, MODEL_B);
    assert_eq!(
        calls
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone(),
        vec![MODEL_B.to_string()]
    );
    assert_eq!(
        metrics.attempts(),
        vec![(DOTTED_B.to_string(), AttemptOutcome::Success)]
    );
    assert_eq!(
        metrics.fallbacks(),
        vec![(DOTTED_A.to_string(), DOTTED_B.to_string())]
    );
    assert!(metrics
        .circuit_transitions()
        .contains(&(DOTTED_A.to_string(), CircuitStateLabel::Open)));
    assert!(!metrics
        .attempts()
        .iter()
        .any(|(target, _)| target == DOTTED_A));
}

#[tokio::test(start_paused = true)]
async fn cap_skip_records_adjacent_a_to_c_without_b_attempt() {
    let metrics = RecordingExecutionMetrics::default();
    let long_c = long_c();
    let (vendor, calls) = MultiModelVendor::new(HashMap::from([
        (MODEL_A.to_string(), vec![Err(network())]),
        (
            MODEL_B.to_string(),
            vec![Ok(success(MODEL_B, "should-not-run"))],
        ),
        (MODEL_C.to_string(), vec![Ok(success(MODEL_C, "c-ok"))]),
    ]));
    let service = service_with_metrics(
        vec![("openai".to_string(), Arc::new(vendor))],
        0,
        3,
        metrics.clone(),
    );
    let result = service
        .execute(
            &chain(
                hop(DOTTED_A, MODEL_A, 512),
                vec![hop(DOTTED_B, MODEL_B, 64), hop(&long_c, MODEL_C, 512)],
            ),
            &payload_with_tokens(128),
            RequestDeadline::from_timeout(Duration::from_secs(30)),
        )
        .await
        .expect("C should succeed after skipping B");
    assert_eq!(result.model_used, MODEL_C);
    assert_eq!(
        calls
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone(),
        vec![MODEL_A.to_string(), MODEL_C.to_string()]
    );
    assert_eq!(
        metrics.attempts(),
        vec![
            (DOTTED_A.to_string(), AttemptOutcome::Retryable),
            (long_c.clone(), AttemptOutcome::Success),
        ]
    );
    assert_eq!(
        metrics.fallbacks(),
        vec![(DOTTED_A.to_string(), long_c.clone())]
    );
    assert_eq!(metrics.usage(), vec![(long_c, 8, 4)]);
}

#[tokio::test(start_paused = true)]
async fn adjacent_evaluated_hops_record_a_to_b_then_b_to_c() {
    let metrics = RecordingExecutionMetrics::default();
    let long_c = long_c();
    let (vendor, _calls) = MultiModelVendor::new(HashMap::from([
        (MODEL_A.to_string(), vec![Err(network())]),
        (MODEL_B.to_string(), vec![Err(network())]),
        (MODEL_C.to_string(), vec![Ok(success(MODEL_C, "c-ok"))]),
    ]));
    let service = service_with_metrics(
        vec![("openai".to_string(), Arc::new(vendor))],
        0,
        3,
        metrics.clone(),
    );
    service
        .execute(
            &chain(
                hop(DOTTED_A, MODEL_A, 512),
                vec![hop(DOTTED_B, MODEL_B, 512), hop(&long_c, MODEL_C, 512)],
            ),
            &payload(),
            RequestDeadline::from_timeout(Duration::from_secs(30)),
        )
        .await
        .expect("C should succeed");
    assert_eq!(
        metrics.fallbacks(),
        vec![
            (DOTTED_A.to_string(), DOTTED_B.to_string()),
            (DOTTED_B.to_string(), long_c.clone()),
        ]
    );
    assert_eq!(
        metrics.attempts(),
        vec![
            (DOTTED_A.to_string(), AttemptOutcome::Retryable),
            (DOTTED_B.to_string(), AttemptOutcome::Retryable),
            (long_c.clone(), AttemptOutcome::Success),
        ]
    );
    assert_eq!(metrics.usage(), vec![(long_c, 8, 4)]);
}

#[tokio::test(start_paused = true)]
async fn drop_after_expiry_before_executor_repoll_records_deadline_once() {
    let metrics = RecordingExecutionMetrics::default();
    let (vendor, calls) =
        ScriptedVendor::delayed_success("openai", MODEL_A, Duration::from_secs(5));
    let service = service_with_metrics(
        vec![("openai".to_string(), Arc::new(vendor))],
        0,
        1,
        metrics.clone(),
    );
    let deadline = RequestDeadline::from_timeout(Duration::from_millis(50));
    let handle = tokio::spawn(async move {
        service
            .execute_with_retry(&hop(DOTTED_A, MODEL_A, 512), &params(), deadline)
            .await
    });
    wait_for_calls(&calls, 1).await;
    tokio::time::advance(Duration::from_millis(50)).await;
    handle.abort();
    let _ = handle.await;
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_eq!(
        metrics.attempts(),
        vec![(DOTTED_A.to_string(), AttemptOutcome::Deadline)]
    );
    assert!(metrics.usage().is_empty());
    assert_eq!(metrics.deadline_count(), 0);
    assert!(metrics.retries().is_empty());
}

#[tokio::test(start_paused = true)]
async fn drop_during_attempt_before_expiry_records_cancelled_without_usage() {
    let metrics = RecordingExecutionMetrics::default();
    let (vendor, calls) =
        ScriptedVendor::delayed_success("openai", MODEL_A, Duration::from_secs(5));
    let service = service_with_metrics(
        vec![("openai".to_string(), Arc::new(vendor))],
        2,
        3,
        metrics.clone(),
    );
    let deadline = RequestDeadline::from_timeout(Duration::from_secs(30));
    let handle = tokio::spawn(async move {
        service
            .execute_with_retry(&hop(DOTTED_A, MODEL_A, 512), &params(), deadline)
            .await
    });
    wait_for_calls(&calls, 1).await;
    handle.abort();
    let _ = handle.await;
    tokio::time::advance(Duration::from_secs(10)).await;
    tokio::task::yield_now().await;
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_eq!(
        metrics.attempts(),
        vec![(DOTTED_A.to_string(), AttemptOutcome::Cancelled)]
    );
    assert!(metrics.usage().is_empty());
    assert_eq!(metrics.deadline_count(), 0);
    assert!(metrics.retries().is_empty());
}

#[tokio::test(start_paused = true)]
async fn drop_during_backoff_does_not_invent_later_attempts() {
    let metrics = RecordingExecutionMetrics::default();
    let (vendor, calls) = ScriptedVendor::fail(
        "openai",
        MODEL_A,
        ExecutorError::RateLimitExceeded {
            vendor: "openai".to_string(),
            retry_after_ms: Some(5_000),
        },
        Duration::ZERO,
    );
    let service = service_with_metrics(
        vec![("openai".to_string(), Arc::new(vendor))],
        3,
        4,
        metrics.clone(),
    );
    let deadline = RequestDeadline::from_timeout(Duration::from_secs(30));
    let handle = tokio::spawn(async move {
        service
            .execute_with_retry(&hop(DOTTED_A, MODEL_A, 512), &params(), deadline)
            .await
    });
    wait_for_calls(&calls, 1).await;
    handle.abort();
    let _ = handle.await;
    tokio::time::advance(Duration::from_secs(10)).await;
    tokio::task::yield_now().await;
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_eq!(
        metrics.attempts(),
        vec![(DOTTED_A.to_string(), AttemptOutcome::RateLimit)]
    );
    assert!(metrics.retries().is_empty());
    assert!(metrics.usage().is_empty());
    assert_eq!(metrics.deadline_count(), 0);
}

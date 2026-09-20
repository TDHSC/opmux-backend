//! Direct deadline and cancellation tests for ExecutorService.

use crate::core::contracts::RoutePlan;
use crate::core::deadline::RequestDeadline;
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

struct ScriptedVendor {
    vendor_id: String,
    models: Vec<String>,
    calls: Arc<AtomicUsize>,
    delay: Duration,
    delayed_calls: Arc<AtomicUsize>,
    error: Option<ExecutorError>,
}

impl ScriptedVendor {
    fn success(
        vendor_id: &str,
        model: &str,
        delay: Duration,
    ) -> (Self, Arc<AtomicUsize>) {
        Self::new(vendor_id, model, delay, usize::MAX, None)
    }

    fn success_delaying_once(
        vendor_id: &str,
        model: &str,
        delay: Duration,
    ) -> (Self, Arc<AtomicUsize>) {
        Self::new(vendor_id, model, delay, 1, None)
    }

    fn fail(
        vendor_id: &str,
        model: &str,
        error: ExecutorError,
        delay: Duration,
    ) -> (Self, Arc<AtomicUsize>) {
        Self::new(vendor_id, model, delay, usize::MAX, Some(error))
    }

    fn new(
        vendor_id: &str,
        model: &str,
        delay: Duration,
        delayed_calls: usize,
        error: Option<ExecutorError>,
    ) -> (Self, Arc<AtomicUsize>) {
        let calls = Arc::new(AtomicUsize::new(0));
        (
            Self {
                vendor_id: vendor_id.to_string(),
                models: vec![model.to_string()],
                calls: calls.clone(),
                delay,
                delayed_calls: Arc::new(AtomicUsize::new(delayed_calls)),
                error,
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
        self.calls.fetch_add(1, Ordering::SeqCst);
        if !self.delay.is_zero()
            && self
                .delayed_calls
                .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |left| {
                    left.checked_sub(1)
                })
                .is_ok()
        {
            tokio::time::sleep(self.delay).await;
        }
        if let Some(error) = self.error.clone() {
            return Err(error);
        }
        Ok(ExecutionResult {
            content: format!("ok from {}", self.vendor_id),
            role: "assistant".to_string(),
            model_used: model.to_string(),
            prompt_tokens: 1,
            completion_tokens: 1,
            total_cost: 0.0,
            finish_reason: "stop".to_string(),
        })
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

/// Vendor that parks until the shared overall deadline, then returns success.
///
/// Used to prove that a ready success at expiry is still `DeadlineExceeded`.
struct SuccessAtDeadlineVendor {
    vendor_id: String,
    models: Vec<String>,
    calls: Arc<AtomicUsize>,
    started: Arc<AtomicUsize>,
    deadline: tokio::time::Instant,
}

impl SuccessAtDeadlineVendor {
    fn new(
        vendor_id: &str,
        model: &str,
        deadline: tokio::time::Instant,
    ) -> (Self, Arc<AtomicUsize>, Arc<AtomicUsize>) {
        let calls = Arc::new(AtomicUsize::new(0));
        let started = Arc::new(AtomicUsize::new(0));
        (
            Self {
                vendor_id: vendor_id.to_string(),
                models: vec![model.to_string()],
                calls: calls.clone(),
                started: started.clone(),
                deadline,
            },
            calls,
            started,
        )
    }
}

#[async_trait]
impl LLMVendor for SuccessAtDeadlineVendor {
    async fn execute(
        &self,
        model: &str,
        _target_id: &str,
        _params: ExecutionParams,
    ) -> Result<ExecutionResult, ExecutorError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.started.fetch_add(1, Ordering::SeqCst);
        tokio::time::sleep_until(self.deadline).await;
        Ok(ExecutionResult {
            content: format!("ok from {}", self.vendor_id),
            role: "assistant".to_string(),
            model_used: model.to_string(),
            prompt_tokens: 1,
            completion_tokens: 1,
            total_cost: 0.0,
            finish_reason: "stop".to_string(),
        })
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

fn service_with(
    vendors: Vec<(String, Arc<dyn LLMVendor>)>,
    max_retries: u32,
    timeout_ms: u64,
) -> ExecutorService {
    let mut vendor_map = HashMap::new();
    for (id, vendor) in vendors {
        vendor_map.insert(id, vendor);
    }
    let mut config = ExecutorConfig::mock_policy(max_retries, timeout_ms);
    config.max_total_attempts = 16;
    ExecutorService::from_repository(
        ExecutorRepository {
            vendors: vendor_map,
        },
        config,
    )
}

fn plan(vendor_id: &str, model_id: &str, fallbacks: Vec<RoutePlan>) -> RoutePlan {
    RoutePlan {
        vendor_id: vendor_id.to_string(),
        target_id: model_id.to_string(),
        model_id: model_id.to_string(),
        max_output_tokens: 4_096,
        fallback_plans: fallbacks,
    }
}

fn params() -> ExecutionParams {
    ExecutionParams {
        messages: vec![Message {
            role: "user".to_string(),
            content: "deadline".to_string(),
        }],
        temperature: None,
        max_tokens: None,
        top_p: None,
        stream: false,
    }
}

fn payload() -> serde_json::Value {
    json!({ "messages": [{ "role": "user", "content": "deadline" }] })
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

#[tokio::test(start_paused = true)]
async fn expired_deadline_starts_zero_vendor_calls() {
    let (vendor, calls) = ScriptedVendor::success("mock", "model-1", Duration::ZERO);
    let service = service_with(vec![("mock".to_string(), Arc::new(vendor))], 1, 10_000);
    let deadline = RequestDeadline::from_timeout(Duration::from_millis(5));
    tokio::time::advance(Duration::from_millis(5)).await;

    match service
        .execute_with_retry(&plan("mock", "model-1", vec![]), &params(), deadline)
        .await
    {
        Err(ExecutorError::DeadlineExceeded) => {}
        other => panic!("expected DeadlineExceeded, got {other:?}"),
    }
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}

#[tokio::test(start_paused = true)]
async fn remaining_deadline_caps_attempt_instead_of_resetting() {
    let (vendor, calls) =
        ScriptedVendor::success("mock", "model-1", Duration::from_secs(1));
    let service = service_with(vec![("mock".to_string(), Arc::new(vendor))], 0, 10_000);
    let deadline = RequestDeadline::from_timeout(Duration::from_millis(80));

    let handle = tokio::spawn(async move {
        service
            .execute(&plan("mock", "model-1", vec![]), &payload(), deadline)
            .await
    });
    wait_for_calls(&calls, 1).await;
    tokio::time::advance(Duration::from_millis(80)).await;
    tokio::task::yield_now().await;
    assert!(
        handle.is_finished(),
        "execution should stop at remaining deadline, not a fresh attempt timeout"
    );
    match handle.await.expect("join") {
        Err(ExecutorError::DeadlineExceeded) => {}
        other => panic!("expected DeadlineExceeded, got {other:?}"),
    }
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

#[tokio::test(start_paused = true)]
async fn fallback_does_not_receive_a_fresh_deadline() {
    let (primary, primary_calls) = ScriptedVendor::fail(
        "openai",
        "gpt-4",
        ExecutorError::NetworkError("primary down".to_string()),
        Duration::ZERO,
    );
    let (fallback, fallback_calls) =
        ScriptedVendor::success("backup", "gpt-4-turbo", Duration::from_secs(1));
    let service = service_with(
        vec![
            ("openai".to_string(), Arc::new(primary)),
            ("backup".to_string(), Arc::new(fallback)),
        ],
        0,
        10_000,
    );
    let deadline = RequestDeadline::from_timeout(Duration::from_millis(80));
    let route = plan(
        "openai",
        "gpt-4",
        vec![plan("backup", "gpt-4-turbo", vec![])],
    );

    let handle =
        tokio::spawn(async move { service.execute(&route, &payload(), deadline).await });
    wait_for_calls(&fallback_calls, 1).await;
    tokio::time::advance(Duration::from_millis(80)).await;
    tokio::task::yield_now().await;
    assert!(
        handle.is_finished(),
        "fallback must use remaining budget, not a reset timeout"
    );
    match handle.await.expect("join") {
        Err(ExecutorError::DeadlineExceeded) => {}
        other => panic!("expected DeadlineExceeded, got {other:?}"),
    }
    assert_eq!(primary_calls.load(Ordering::SeqCst), 1);
    assert_eq!(fallback_calls.load(Ordering::SeqCst), 1);
}

#[tokio::test(start_paused = true)]
async fn dropping_during_backoff_starts_no_later_attempt() {
    let (vendor, calls) = ScriptedVendor::fail(
        "mock",
        "model-1",
        ExecutorError::RateLimitExceeded {
            vendor: "mock".to_string(),
            retry_after_ms: Some(5_000),
        },
        Duration::ZERO,
    );
    let service = service_with(vec![("mock".to_string(), Arc::new(vendor))], 3, 10_000);
    let deadline = RequestDeadline::from_timeout(Duration::from_secs(30));
    let handle = tokio::spawn(async move {
        service
            .execute_with_retry(&plan("mock", "model-1", vec![]), &params(), deadline)
            .await
    });
    wait_for_calls(&calls, 1).await;
    handle.abort();
    let _ = handle.await;
    tokio::time::advance(Duration::from_secs(10)).await;
    tokio::task::yield_now().await;
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

#[tokio::test(start_paused = true)]
async fn dropping_during_delayed_attempt_starts_no_later_retry() {
    let (vendor, calls) =
        ScriptedVendor::success("mock", "model-1", Duration::from_secs(5));
    let service = service_with(vec![("mock".to_string(), Arc::new(vendor))], 2, 10_000);
    let deadline = RequestDeadline::from_timeout(Duration::from_secs(30));
    let handle = tokio::spawn(async move {
        service
            .execute_with_retry(&plan("mock", "model-1", vec![]), &params(), deadline)
            .await
    });
    wait_for_calls(&calls, 1).await;
    handle.abort();
    let _ = handle.await;
    tokio::time::advance(Duration::from_secs(10)).await;
    tokio::task::yield_now().await;
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

#[tokio::test(start_paused = true)]
async fn independent_execution_succeeds_after_cancelled_work() {
    let (vendor, calls) =
        ScriptedVendor::success_delaying_once("mock", "model-1", Duration::from_secs(5));
    let service = Arc::new(service_with(
        vec![("mock".to_string(), Arc::new(vendor))],
        0,
        10_000,
    ));
    let deadline = RequestDeadline::from_timeout(Duration::from_secs(30));
    let cancelled = {
        let service = service.clone();
        tokio::spawn(async move {
            service
                .execute(&plan("mock", "model-1", vec![]), &payload(), deadline)
                .await
        })
    };
    wait_for_calls(&calls, 1).await;
    cancelled.abort();
    let _ = cancelled.await;

    let second = service
        .execute(
            &plan("mock", "model-1", vec![]),
            &payload(),
            RequestDeadline::from_timeout(Duration::from_secs(30)),
        )
        .await
        .expect("independent request should proceed");
    assert_eq!(second.model_used, "model-1");
    assert_eq!(calls.load(Ordering::SeqCst), 2);
}

#[tokio::test(start_paused = true)]
async fn ready_success_at_overall_deadline_is_deadline_exceeded() {
    let overall = Duration::from_millis(100);
    let deadline_at = tokio::time::Instant::now() + overall;
    let (primary, primary_calls, started) =
        SuccessAtDeadlineVendor::new("openai", "gpt-4", deadline_at);
    let (fallback, fallback_calls) =
        ScriptedVendor::success("backup", "gpt-4-turbo", Duration::ZERO);
    let service = service_with(
        vec![
            ("openai".to_string(), Arc::new(primary)),
            ("backup".to_string(), Arc::new(fallback)),
        ],
        0,
        1_000,
    );
    let route = plan(
        "openai",
        "gpt-4",
        vec![plan("backup", "gpt-4-turbo", vec![])],
    );
    let deadline = RequestDeadline::at(deadline_at);
    let handle =
        tokio::spawn(async move { service.execute(&route, &payload(), deadline).await });
    wait_for_calls(&started, 1).await;
    tokio::time::advance(overall).await;
    tokio::task::yield_now().await;
    match handle.await.expect("join") {
        Err(ExecutorError::DeadlineExceeded) => {}
        other => panic!("expected DeadlineExceeded, got {other:?}"),
    }
    assert_eq!(primary_calls.load(Ordering::SeqCst), 1);
    assert_eq!(fallback_calls.load(Ordering::SeqCst), 0);
}

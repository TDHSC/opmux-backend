//! Direct executor tests for shared attempt budgets and Retry-After policy.

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
use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

struct CountingVendor {
    vendor_id: String,
    models: Vec<String>,
    calls: Arc<AtomicUsize>,
    fail_times: usize,
    error: ExecutorError,
    delay: Duration,
}

struct SequenceVendor {
    vendor_id: String,
    models: Vec<String>,
    calls: Arc<AtomicUsize>,
    outcomes: Vec<Result<ExecutionResult, ExecutorError>>,
}

impl CountingVendor {
    fn always_fail(
        vendor_id: &str,
        model: &str,
        error: ExecutorError,
    ) -> (Self, Arc<AtomicUsize>) {
        Self::fail_then_succeed(vendor_id, model, usize::MAX, error, Duration::ZERO)
    }

    fn fail_then_succeed(
        vendor_id: &str,
        model: &str,
        fail_times: usize,
        error: ExecutorError,
        delay: Duration,
    ) -> (Self, Arc<AtomicUsize>) {
        let calls = Arc::new(AtomicUsize::new(0));
        (
            Self {
                vendor_id: vendor_id.to_string(),
                models: vec![model.to_string()],
                calls: calls.clone(),
                fail_times,
                error,
                delay,
            },
            calls,
        )
    }
}

#[async_trait]
impl LLMVendor for CountingVendor {
    async fn execute(
        &self,
        model: &str,
        _target_id: &str,
        _params: ExecutionParams,
    ) -> Result<ExecutionResult, ExecutorError> {
        let call = self.calls.fetch_add(1, Ordering::SeqCst);
        if !self.delay.is_zero() {
            tokio::time::sleep(self.delay).await;
        }
        if call < self.fail_times {
            return Err(self.error.clone());
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

impl SequenceVendor {
    fn new(
        vendor_id: &str,
        model: &str,
        outcomes: Vec<Result<ExecutionResult, ExecutorError>>,
    ) -> (Self, Arc<AtomicUsize>) {
        let calls = Arc::new(AtomicUsize::new(0));
        (
            Self {
                vendor_id: vendor_id.to_string(),
                models: vec![model.to_string()],
                calls: calls.clone(),
                outcomes,
            },
            calls,
        )
    }
}

#[async_trait]
impl LLMVendor for SequenceVendor {
    async fn execute(
        &self,
        _model: &str,
        _target_id: &str,
        _params: ExecutionParams,
    ) -> Result<ExecutionResult, ExecutorError> {
        let call = self.calls.fetch_add(1, Ordering::SeqCst);
        self.outcomes.get(call).cloned().unwrap_or_else(|| {
            panic!("unexpected extra call {call} on {}", self.vendor_id)
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

fn rate_limit(vendor: &str, retry_after_ms: u64) -> ExecutorError {
    ExecutorError::RateLimitExceeded {
        vendor: vendor.to_string(),
        retry_after_ms: Some(retry_after_ms),
    }
}

fn success_result(vendor_id: &str, model: &str) -> ExecutionResult {
    ExecutionResult {
        content: format!("ok from {vendor_id}"),
        role: "assistant".to_string(),
        model_used: model.to_string(),
        prompt_tokens: 1,
        completion_tokens: 1,
        total_cost: 0.0,
        finish_reason: "stop".to_string(),
    }
}

fn service_with_policy(
    vendors: Vec<(String, Arc<dyn LLMVendor>)>,
    max_retries: u32,
    max_total_attempts: u32,
    timeout_ms: u64,
    backoff_cap_ms: u64,
) -> ExecutorService {
    let mut vendor_map = HashMap::new();
    for (id, vendor) in vendors {
        vendor_map.insert(id, vendor);
    }
    let mut config = ExecutorConfig::mock_policy(max_retries, timeout_ms);
    config.max_total_attempts = max_total_attempts;
    config.backoff_cap_ms = backoff_cap_ms;
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

fn payload() -> serde_json::Value {
    json!({ "messages": [{ "role": "user", "content": "budget" }] })
}

fn network_error() -> ExecutorError {
    ExecutorError::NetworkError("transient".to_string())
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
async fn default_single_target_policy_permits_at_most_two_calls() {
    let (vendor, calls) = CountingVendor::always_fail("mock", "model-1", network_error());
    let service = service_with_policy(
        vec![("mock".to_string(), Arc::new(vendor))],
        1,
        3,
        10_000,
        1,
    );

    let handle = tokio::spawn(async move {
        service
            .execute(
                &plan("mock", "model-1", vec![]),
                &payload(),
                RequestDeadline::from_timeout(Duration::from_secs(30)),
            )
            .await
    });
    tokio::time::advance(Duration::from_millis(5)).await;
    tokio::task::yield_now().await;

    match handle.await.expect("join") {
        Err(ExecutorError::NetworkError(_)) => {}
        other => panic!("expected exhausted NetworkError, got {other:?}"),
    }
    assert_eq!(calls.load(Ordering::SeqCst), 2);
}

#[tokio::test(start_paused = true)]
async fn higher_retry_allowance_is_capped_by_global_attempt_budget() {
    let (vendor, calls) = CountingVendor::always_fail("mock", "model-1", network_error());
    let service = service_with_policy(
        vec![("mock".to_string(), Arc::new(vendor))],
        5,
        3,
        10_000,
        1,
    );

    let handle = tokio::spawn(async move {
        service
            .execute(
                &plan("mock", "model-1", vec![]),
                &payload(),
                RequestDeadline::from_timeout(Duration::from_secs(30)),
            )
            .await
    });
    tokio::time::advance(Duration::from_millis(5)).await;
    tokio::task::yield_now().await;

    match handle.await.expect("join") {
        Err(ExecutorError::NetworkError(_)) => {}
        other => panic!("expected exhausted NetworkError, got {other:?}"),
    }
    assert_eq!(calls.load(Ordering::SeqCst), 3);
}

#[tokio::test(start_paused = true)]
async fn fallback_does_not_reset_the_shared_attempt_budget() {
    let (primary, primary_calls) =
        CountingVendor::always_fail("openai", "gpt-4", network_error());
    let (fallback, fallback_calls) =
        CountingVendor::always_fail("backup", "gpt-4-turbo", network_error());
    let service = service_with_policy(
        vec![
            ("openai".to_string(), Arc::new(primary)),
            ("backup".to_string(), Arc::new(fallback)),
        ],
        1,
        3,
        10_000,
        1,
    );
    let route = plan(
        "openai",
        "gpt-4",
        vec![plan("backup", "gpt-4-turbo", vec![])],
    );

    let handle = tokio::spawn(async move {
        service
            .execute(
                &route,
                &payload(),
                RequestDeadline::from_timeout(Duration::from_secs(30)),
            )
            .await
    });
    tokio::time::advance(Duration::from_millis(5)).await;
    tokio::task::yield_now().await;
    handle.await.expect("join").expect_err("should exhaust");

    assert_eq!(primary_calls.load(Ordering::SeqCst), 2);
    assert_eq!(fallback_calls.load(Ordering::SeqCst), 1);
}

#[tokio::test(start_paused = true)]
async fn remaining_time_shorter_than_attempt_timeout_starts_no_later_call() {
    let (vendor, calls) = CountingVendor::fail_then_succeed(
        "mock",
        "model-1",
        usize::MAX,
        network_error(),
        Duration::from_secs(1),
    );
    let service = service_with_policy(
        vec![("mock".to_string(), Arc::new(vendor))],
        1,
        3,
        10_000,
        1,
    );
    let deadline = RequestDeadline::from_timeout(Duration::from_millis(80));
    let handle = tokio::spawn(async move {
        service
            .execute(&plan("mock", "model-1", vec![]), &payload(), deadline)
            .await
    });
    wait_for_calls(&calls, 1).await;
    tokio::time::advance(Duration::from_millis(80)).await;
    tokio::task::yield_now().await;
    match handle.await.expect("join") {
        Err(ExecutorError::DeadlineExceeded) => {}
        other => panic!("expected DeadlineExceeded, got {other:?}"),
    }
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

#[tokio::test(start_paused = true)]
async fn attempt_timeout_does_not_claim_overall_deadline_elapsed() {
    let (vendor, calls) = CountingVendor::fail_then_succeed(
        "mock",
        "model-1",
        usize::MAX,
        network_error(),
        Duration::from_secs(1),
    );
    let service =
        service_with_policy(vec![("mock".to_string(), Arc::new(vendor))], 0, 1, 50, 1);
    let deadline = RequestDeadline::from_timeout(Duration::from_secs(5));
    let handle = tokio::spawn(async move {
        service
            .execute(&plan("mock", "model-1", vec![]), &payload(), deadline)
            .await
    });
    wait_for_calls(&calls, 1).await;
    tokio::time::advance(Duration::from_millis(50)).await;
    tokio::task::yield_now().await;
    match handle.await.expect("join") {
        Err(ExecutorError::TimeoutError(ms)) => assert_eq!(ms, 50),
        other => panic!("expected TimeoutError, got {other:?}"),
    }
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

#[tokio::test(start_paused = true)]
async fn retry_after_that_cannot_fit_returns_provider_throttling() {
    let (vendor, calls) = CountingVendor::always_fail(
        "mock",
        "model-1",
        ExecutorError::RateLimitExceeded {
            vendor: "mock".to_string(),
            retry_after_ms: Some(5_000),
        },
    );
    let service = service_with_policy(
        vec![("mock".to_string(), Arc::new(vendor))],
        3,
        4,
        10_000,
        2_000,
    );
    let deadline = RequestDeadline::from_timeout(Duration::from_millis(200));
    let handle = tokio::spawn(async move {
        service
            .execute(&plan("mock", "model-1", vec![]), &payload(), deadline)
            .await
    });
    wait_for_calls(&calls, 1).await;
    tokio::task::yield_now().await;
    assert!(
        handle.is_finished(),
        "oversized Retry-After must not sleep until the deadline"
    );
    match handle.await.expect("join") {
        Err(ExecutorError::RateLimitExceeded {
            vendor,
            retry_after_ms,
        }) => {
            assert_eq!(vendor, "mock");
            assert_eq!(retry_after_ms, Some(5_000));
        }
        other => panic!("expected RateLimitExceeded, got {other:?}"),
    }
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

#[tokio::test(start_paused = true)]
async fn eligible_retry_does_not_start_before_provider_minimum() {
    let (vendor, calls) = CountingVendor::fail_then_succeed(
        "mock",
        "model-1",
        1,
        ExecutorError::RateLimitExceeded {
            vendor: "mock".to_string(),
            retry_after_ms: Some(100),
        },
        Duration::ZERO,
    );
    let service = service_with_policy(
        vec![("mock".to_string(), Arc::new(vendor))],
        1,
        3,
        10_000,
        2_000,
    );
    let handle = tokio::spawn(async move {
        service
            .execute(
                &plan("mock", "model-1", vec![]),
                &payload(),
                RequestDeadline::from_timeout(Duration::from_secs(5)),
            )
            .await
    });
    wait_for_calls(&calls, 1).await;
    tokio::time::advance(Duration::from_millis(99)).await;
    tokio::task::yield_now().await;
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert!(!handle.is_finished());

    tokio::time::advance(Duration::from_millis(1)).await;
    tokio::task::yield_now().await;
    let result = handle.await.expect("join").expect("retry should succeed");
    assert_eq!(result.model_used, "model-1");
    assert_eq!(calls.load(Ordering::SeqCst), 2);
}

#[tokio::test(start_paused = true)]
async fn zero_retries_cannot_fit_does_not_call_configured_fallback() {
    let (primary, primary_calls) =
        CountingVendor::always_fail("openai", "gpt-4", rate_limit("openai", 5_000));
    let (fallback, fallback_calls) = SequenceVendor::new(
        "backup",
        "gpt-4-turbo",
        vec![Ok(success_result("backup", "gpt-4-turbo"))],
    );
    let service = service_with_policy(
        vec![
            ("openai".to_string(), Arc::new(primary)),
            ("backup".to_string(), Arc::new(fallback)),
        ],
        0,
        3,
        10_000,
        2_000,
    );
    let route = plan(
        "openai",
        "gpt-4",
        vec![plan("backup", "gpt-4-turbo", vec![])],
    );
    let deadline = RequestDeadline::from_timeout(Duration::from_millis(200));
    let handle =
        tokio::spawn(async move { service.execute(&route, &payload(), deadline).await });
    wait_for_calls(&primary_calls, 1).await;
    tokio::task::yield_now().await;
    assert!(
        handle.is_finished(),
        "cannot-fit Retry-After must terminate before fallback"
    );
    match handle.await.expect("join") {
        Err(ExecutorError::RateLimitExceeded {
            vendor,
            retry_after_ms,
        }) => {
            assert_eq!(vendor, "openai");
            assert_eq!(retry_after_ms, Some(5_000));
        }
        other => panic!("expected RateLimitExceeded, got {other:?}"),
    }
    assert_eq!(primary_calls.load(Ordering::SeqCst), 1);
    assert_eq!(fallback_calls.load(Ordering::SeqCst), 0);
}

#[tokio::test(start_paused = true)]
async fn final_retry_cannot_fit_does_not_call_configured_fallback() {
    let (primary, primary_calls) = SequenceVendor::new(
        "openai",
        "gpt-4",
        vec![
            Err(rate_limit("openai", 10)),
            Err(rate_limit("openai", 5_000)),
        ],
    );
    let (fallback, fallback_calls) = SequenceVendor::new(
        "backup",
        "gpt-4-turbo",
        vec![Ok(success_result("backup", "gpt-4-turbo"))],
    );
    let service = service_with_policy(
        vec![
            ("openai".to_string(), Arc::new(primary)),
            ("backup".to_string(), Arc::new(fallback)),
        ],
        1,
        3,
        10_000,
        2_000,
    );
    let route = plan(
        "openai",
        "gpt-4",
        vec![plan("backup", "gpt-4-turbo", vec![])],
    );
    let deadline = RequestDeadline::from_timeout(Duration::from_millis(200));
    let handle =
        tokio::spawn(async move { service.execute(&route, &payload(), deadline).await });
    wait_for_calls(&primary_calls, 1).await;
    tokio::time::advance(Duration::from_millis(10)).await;
    tokio::task::yield_now().await;
    wait_for_calls(&primary_calls, 2).await;
    tokio::task::yield_now().await;
    assert!(
        handle.is_finished(),
        "final-retry cannot-fit must not start fallback"
    );
    match handle.await.expect("join") {
        Err(ExecutorError::RateLimitExceeded { retry_after_ms, .. }) => {
            assert_eq!(retry_after_ms, Some(5_000));
        }
        other => panic!("expected RateLimitExceeded, got {other:?}"),
    }
    assert_eq!(primary_calls.load(Ordering::SeqCst), 2);
    assert_eq!(fallback_calls.load(Ordering::SeqCst), 0);
}

#[tokio::test(start_paused = true)]
async fn intermediate_fallback_cannot_fit_stops_the_remaining_chain() {
    let (primary, primary_calls) =
        CountingVendor::always_fail("openai", "gpt-4", network_error());
    let (mid, mid_calls) =
        CountingVendor::always_fail("mid", "model-mid", rate_limit("mid", 5_000));
    let (tail, tail_calls) = SequenceVendor::new(
        "tail",
        "model-tail",
        vec![Ok(success_result("tail", "model-tail"))],
    );
    let service = service_with_policy(
        vec![
            ("openai".to_string(), Arc::new(primary)),
            ("mid".to_string(), Arc::new(mid)),
            ("tail".to_string(), Arc::new(tail)),
        ],
        0,
        3,
        10_000,
        2_000,
    );
    let route = plan(
        "openai",
        "gpt-4",
        vec![
            plan("mid", "model-mid", vec![]),
            plan("tail", "model-tail", vec![]),
        ],
    );
    let deadline = RequestDeadline::from_timeout(Duration::from_millis(200));
    let handle =
        tokio::spawn(async move { service.execute(&route, &payload(), deadline).await });
    wait_for_calls(&primary_calls, 1).await;
    wait_for_calls(&mid_calls, 1).await;
    tokio::task::yield_now().await;
    assert!(
        handle.is_finished(),
        "intermediate fallback cannot-fit must stop the chain"
    );
    match handle.await.expect("join") {
        Err(ExecutorError::RateLimitExceeded {
            vendor,
            retry_after_ms,
        }) => {
            assert_eq!(vendor, "mid");
            assert_eq!(retry_after_ms, Some(5_000));
        }
        other => panic!("expected RateLimitExceeded, got {other:?}"),
    }
    assert_eq!(primary_calls.load(Ordering::SeqCst), 1);
    assert_eq!(mid_calls.load(Ordering::SeqCst), 1);
    assert_eq!(tail_calls.load(Ordering::SeqCst), 0);
}

#[tokio::test(start_paused = true)]
async fn spare_global_attempt_slots_do_not_bypass_cannot_fit() {
    let (primary, primary_calls) =
        CountingVendor::always_fail("openai", "gpt-4", rate_limit("openai", 5_000));
    let (fallback, fallback_calls) = SequenceVendor::new(
        "backup",
        "gpt-4-turbo",
        vec![Ok(success_result("backup", "gpt-4-turbo"))],
    );
    let service = service_with_policy(
        vec![
            ("openai".to_string(), Arc::new(primary)),
            ("backup".to_string(), Arc::new(fallback)),
        ],
        1,
        4,
        10_000,
        2_000,
    );
    let route = plan(
        "openai",
        "gpt-4",
        vec![plan("backup", "gpt-4-turbo", vec![])],
    );
    let deadline = RequestDeadline::from_timeout(Duration::from_millis(200));
    let handle =
        tokio::spawn(async move { service.execute(&route, &payload(), deadline).await });
    wait_for_calls(&primary_calls, 1).await;
    tokio::task::yield_now().await;
    match handle.await.expect("join") {
        Err(ExecutorError::RateLimitExceeded { retry_after_ms, .. }) => {
            assert_eq!(retry_after_ms, Some(5_000));
        }
        other => panic!("expected RateLimitExceeded, got {other:?}"),
    }
    assert_eq!(primary_calls.load(Ordering::SeqCst), 1);
    assert_eq!(fallback_calls.load(Ordering::SeqCst), 0);
}

#[tokio::test(start_paused = true)]
async fn fitting_retry_after_still_retries_primary_when_fallback_exists() {
    let (primary, primary_calls) = SequenceVendor::new(
        "openai",
        "gpt-4",
        vec![
            Err(rate_limit("openai", 10)),
            Ok(success_result("openai", "gpt-4")),
        ],
    );
    let (fallback, fallback_calls) = SequenceVendor::new(
        "backup",
        "gpt-4-turbo",
        vec![Ok(success_result("backup", "gpt-4-turbo"))],
    );
    let service = service_with_policy(
        vec![
            ("openai".to_string(), Arc::new(primary)),
            ("backup".to_string(), Arc::new(fallback)),
        ],
        1,
        3,
        10_000,
        2_000,
    );
    let route = plan(
        "openai",
        "gpt-4",
        vec![plan("backup", "gpt-4-turbo", vec![])],
    );
    let handle = tokio::spawn(async move {
        service
            .execute(
                &route,
                &payload(),
                RequestDeadline::from_timeout(Duration::from_secs(5)),
            )
            .await
    });
    wait_for_calls(&primary_calls, 1).await;
    tokio::time::advance(Duration::from_millis(10)).await;
    tokio::task::yield_now().await;
    let result = handle
        .await
        .expect("join")
        .expect("fitting delay should retry");
    assert_eq!(result.model_used, "gpt-4");
    assert_eq!(primary_calls.load(Ordering::SeqCst), 2);
    assert_eq!(fallback_calls.load(Ordering::SeqCst), 0);
}

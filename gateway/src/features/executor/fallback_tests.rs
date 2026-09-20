//! Direct executor tests for eligible flat fallback order and skip rules.

use crate::core::contracts::RoutePlan;
use crate::core::deadline::RequestDeadline;
use crate::features::executor::circuit::TargetCircuitRegistry;
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
use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::Duration;

const MODEL_A: &str = "fallback-model-a";
const MODEL_B: &str = "fallback-model-b";
const MODEL_C: &str = "fallback-model-c";

type CallLog = Arc<Mutex<Vec<String>>>;
type ParamLog = Arc<Mutex<Vec<ExecutionParams>>>;

struct SharedProviderVendor {
    calls: CallLog,
    params: ParamLog,
    scripts: Mutex<HashMap<String, VecDeque<Result<ExecutionResult, ExecutorError>>>>,
    delay: Duration,
}

impl SharedProviderVendor {
    fn new(
        scripts: HashMap<String, Vec<Result<ExecutionResult, ExecutorError>>>,
    ) -> (Self, CallLog, ParamLog) {
        Self::with_delay(scripts, Duration::ZERO)
    }

    fn with_delay(
        scripts: HashMap<String, Vec<Result<ExecutionResult, ExecutorError>>>,
        delay: Duration,
    ) -> (Self, CallLog, ParamLog) {
        let calls = Arc::new(Mutex::new(Vec::new()));
        let params = Arc::new(Mutex::new(Vec::new()));
        let queued = scripts
            .into_iter()
            .map(|(model, outcomes)| (model, VecDeque::from(outcomes)))
            .collect();
        (
            Self {
                calls: calls.clone(),
                params: params.clone(),
                scripts: Mutex::new(queued),
                delay,
            },
            calls,
            params,
        )
    }
}

#[async_trait]
impl LLMVendor for SharedProviderVendor {
    async fn execute(
        &self,
        model: &str,
        _target_id: &str,
        params: ExecutionParams,
    ) -> Result<ExecutionResult, ExecutorError> {
        self.calls
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .push(model.to_string());
        self.params
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .push(params);
        if !self.delay.is_zero() {
            tokio::time::sleep(self.delay).await;
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

fn network() -> ExecutorError {
    ExecutorError::NetworkError("transient".to_string())
}

fn payload_with_tokens(max_tokens: i64) -> serde_json::Value {
    json!({
        "messages": [{ "role": "user", "content": "fallback" }],
        "max_tokens": max_tokens,
        "temperature": 0.2,
        "top_p": 0.9
    })
}

fn service_with(
    vendor: SharedProviderVendor,
    max_retries: u32,
    max_total_attempts: u32,
) -> ExecutorService {
    service_with_circuit(
        vendor,
        max_retries,
        max_total_attempts,
        3,
        Duration::from_secs(30),
    )
}

fn service_with_circuit(
    vendor: SharedProviderVendor,
    max_retries: u32,
    max_total_attempts: u32,
    threshold: u32,
    cooldown: Duration,
) -> ExecutorService {
    let mut vendors: HashMap<String, Arc<dyn LLMVendor>> = HashMap::new();
    vendors.insert("openai".to_string(), Arc::new(vendor));
    let mut config = ExecutorConfig::mock_policy(max_retries, 10_000);
    config.max_total_attempts = max_total_attempts;
    config.backoff_cap_ms = 1;
    config.circuit_failure_threshold = threshold;
    config.circuit_cooldown_ms = u64::try_from(cooldown.as_millis()).unwrap_or(u64::MAX);
    let mut service =
        ExecutorService::from_repository(ExecutorRepository { vendors }, config);
    service.circuits = TargetCircuitRegistry::with_system_clock(threshold, cooldown);
    service
}

fn equal_chain() -> RoutePlan {
    chain(
        hop("alpha", MODEL_A, 512),
        vec![hop("beta", MODEL_B, 512), hop("gamma", MODEL_C, 512)],
    )
}

async fn execute_paused(
    service: ExecutorService,
    plan: RoutePlan,
    payload: serde_json::Value,
    deadline: RequestDeadline,
) -> Result<ExecutionResult, ExecutorError> {
    let handle =
        tokio::spawn(async move { service.execute(&plan, &payload, deadline).await });
    tokio::time::advance(Duration::from_millis(20)).await;
    tokio::task::yield_now().await;
    handle.await.expect("join")
}

fn observed_models(calls: &Mutex<Vec<String>>) -> Vec<String> {
    calls
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clone()
}

#[test]
fn fallback_eligibility_matches_typed_error_classes() {
    assert!(ExecutorService::is_fallback_eligible(&network()));
    assert!(ExecutorService::is_fallback_eligible(
        &ExecutorError::TimeoutError(50)
    ));
    assert!(ExecutorService::is_fallback_eligible(
        &ExecutorError::ApiCallFailed("upstream http error".into())
    ));
    assert!(ExecutorService::is_fallback_eligible(
        &ExecutorError::CircuitOpen {
            vendor: "openai".into(),
            retry_after_ms: 1_000,
        }
    ));

    assert!(!ExecutorService::is_fallback_eligible(
        &ExecutorError::InvalidPayload("missing messages".into())
    ));
    assert!(!ExecutorService::is_fallback_eligible(
        &ExecutorError::AuthenticationFailed("openai".into())
    ));
    assert!(!ExecutorService::is_fallback_eligible(
        &ExecutorError::QuotaExceeded
    ));
    assert!(!ExecutorService::is_fallback_eligible(
        &ExecutorError::JsonError("malformed upstream JSON".into())
    ));
    assert!(!ExecutorService::is_fallback_eligible(
        &ExecutorError::InvalidUpstreamResult
    ));
    assert!(!ExecutorService::is_fallback_eligible(
        &ExecutorError::UpstreamRejected
    ));
    assert!(!ExecutorService::is_fallback_eligible(
        &ExecutorError::RateLimitExceeded {
            vendor: "openai".into(),
            retry_after_ms: Some(10),
        }
    ));
    assert!(!ExecutorService::is_fallback_eligible(
        &ExecutorError::DeadlineExceeded
    ));
}

#[test]
fn hop_is_skipped_only_when_requested_tokens_exceed_target_cap() {
    let plan = hop("beta", MODEL_B, 64);
    let compatible = ExecutionParams {
        messages: vec![Message {
            role: "user".to_string(),
            content: "fallback".to_string(),
        }],
        temperature: Some(0.2),
        max_tokens: Some(64),
        top_p: Some(0.9),
        stream: false,
    };
    let incompatible = ExecutionParams {
        max_tokens: Some(128),
        ..compatible.clone()
    };
    assert!(ExecutorService::hop_supports_params(&plan, &compatible));
    assert!(!ExecutorService::hop_supports_params(&plan, &incompatible));
    assert_eq!(incompatible.max_tokens, Some(128));
}

#[tokio::test(start_paused = true)]
async fn same_provider_a_retries_then_b_succeeds() {
    let (vendor, calls, _params) = SharedProviderVendor::new(HashMap::from([
        (MODEL_A.to_string(), vec![Err(network()), Err(network())]),
        (
            MODEL_B.to_string(),
            vec![Ok(success("reported-b", "FALLBACK_B_OK"))],
        ),
    ]));
    let service = service_with(vendor, 1, 8);
    let result = execute_paused(
        service,
        equal_chain(),
        payload_with_tokens(128),
        RequestDeadline::from_timeout(Duration::from_secs(30)),
    )
    .await
    .expect("B should succeed");
    assert_eq!(result.model_used, "reported-b");
    assert_eq!(result.content, "FALLBACK_B_OK");
    assert_eq!(result.total_cost, 0.001);
    assert_eq!(observed_models(&calls), vec![MODEL_A, MODEL_A, MODEL_B]);
}

#[tokio::test(start_paused = true)]
async fn large_budget_reaches_c_in_declared_order() {
    let (vendor, calls, _params) = SharedProviderVendor::new(HashMap::from([
        (MODEL_A.to_string(), vec![Err(network()), Err(network())]),
        (MODEL_B.to_string(), vec![Err(network()), Err(network())]),
        (
            MODEL_C.to_string(),
            vec![Ok(success("reported-c", "FALLBACK_C_OK"))],
        ),
    ]));
    let service = service_with(vendor, 1, 8);
    let result = execute_paused(
        service,
        equal_chain(),
        payload_with_tokens(128),
        RequestDeadline::from_timeout(Duration::from_secs(30)),
    )
    .await
    .expect("C should succeed");
    assert_eq!(result.model_used, "reported-c");
    assert_eq!(result.content, "FALLBACK_C_OK");
    assert_eq!(
        observed_models(&calls),
        vec![MODEL_A, MODEL_A, MODEL_B, MODEL_B, MODEL_C]
    );
}

#[tokio::test(start_paused = true)]
async fn default_budget_stops_after_two_a_and_first_b() {
    let (vendor, calls, _params) = SharedProviderVendor::new(HashMap::from([
        (MODEL_A.to_string(), vec![Err(network()), Err(network())]),
        (MODEL_B.to_string(), vec![Err(network()), Err(network())]),
        (
            MODEL_C.to_string(),
            vec![Ok(success("reported-c", "should-not-run"))],
        ),
    ]));
    let service = service_with(vendor, 1, 3);
    let err = execute_paused(
        service,
        equal_chain(),
        payload_with_tokens(128),
        RequestDeadline::from_timeout(Duration::from_secs(30)),
    )
    .await
    .expect_err("budget should exhaust");
    match err {
        ExecutorError::NetworkError(message) => assert_eq!(message, "transient"),
        other => panic!("expected preserved primary NetworkError, got {other:?}"),
    }
    assert_eq!(observed_models(&calls), vec![MODEL_A, MODEL_A, MODEL_B]);
}

#[tokio::test(start_paused = true)]
async fn mixed_cap_skips_b_without_changing_params() {
    let (vendor, calls, params) = SharedProviderVendor::new(HashMap::from([
        (MODEL_A.to_string(), vec![Err(network()), Err(network())]),
        (
            MODEL_B.to_string(),
            vec![Ok(success("reported-b", "should-not-run"))],
        ),
        (
            MODEL_C.to_string(),
            vec![Ok(success("reported-c", "FALLBACK_C_OK"))],
        ),
    ]));
    let service = service_with(vendor, 1, 8);
    let plan = chain(
        hop("alpha", MODEL_A, 512),
        vec![hop("beta", MODEL_B, 64), hop("gamma", MODEL_C, 512)],
    );
    let result = execute_paused(
        service,
        plan,
        payload_with_tokens(128),
        RequestDeadline::from_timeout(Duration::from_secs(30)),
    )
    .await
    .expect("C should succeed after skipping B");
    assert_eq!(result.model_used, "reported-c");
    assert_eq!(observed_models(&calls), vec![MODEL_A, MODEL_A, MODEL_C]);
    let captured = params
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clone();
    assert!(captured.iter().all(|item| item.max_tokens == Some(128)));
    assert!(captured.iter().all(|item| item.temperature == Some(0.2)));
    assert!(captured.iter().all(|item| item.top_p == Some(0.9)));
}

#[tokio::test(start_paused = true)]
async fn no_compatible_fallback_preserves_primary_error() {
    let (vendor, calls, _params) = SharedProviderVendor::new(HashMap::from([
        (MODEL_A.to_string(), vec![Err(network()), Err(network())]),
        (
            MODEL_B.to_string(),
            vec![Ok(success("reported-b", "should-not-run"))],
        ),
    ]));
    let service = service_with(vendor, 1, 8);
    let plan = chain(hop("alpha", MODEL_A, 512), vec![hop("beta", MODEL_B, 64)]);
    let err = execute_paused(
        service,
        plan,
        payload_with_tokens(128),
        RequestDeadline::from_timeout(Duration::from_secs(30)),
    )
    .await
    .expect_err("incompatible fallback must not replace primary");
    match err {
        ExecutorError::NetworkError(message) => assert_eq!(message, "transient"),
        other => panic!("expected primary NetworkError, got {other:?}"),
    }
    assert_eq!(observed_models(&calls), vec![MODEL_A, MODEL_A]);
}

#[tokio::test(start_paused = true)]
async fn credential_quota_protocol_and_rejection_do_not_switch() {
    let cases = [
        ExecutorError::AuthenticationFailed("openai".into()),
        ExecutorError::QuotaExceeded,
        ExecutorError::JsonError("malformed upstream JSON".into()),
        ExecutorError::InvalidUpstreamResult,
        ExecutorError::UpstreamRejected,
    ];
    for error in cases {
        let (vendor, calls, _params) = SharedProviderVendor::new(HashMap::from([
            (
                MODEL_A.to_string(),
                vec![Err(error.clone()), Err(error.clone())],
            ),
            (
                MODEL_B.to_string(),
                vec![Ok(success("reported-b", "should-not-run"))],
            ),
        ]));
        let service = service_with(vendor, 1, 8);
        let err = execute_paused(
            service,
            equal_chain(),
            payload_with_tokens(128),
            RequestDeadline::from_timeout(Duration::from_secs(30)),
        )
        .await
        .expect_err("ineligible error must not switch models");
        assert_eq!(
            std::mem::discriminant(&err),
            std::mem::discriminant(&error),
            "primary error class must be preserved, got {err:?}"
        );
        assert_eq!(
            observed_models(&calls),
            vec![MODEL_A.to_string()],
            "ineligible {error:?} must not retry or fall back"
        );
    }
}

#[tokio::test(start_paused = true)]
async fn same_account_throttling_retries_primary_only() {
    let throttle = ExecutorError::RateLimitExceeded {
        vendor: "openai".into(),
        retry_after_ms: Some(1),
    };
    let (vendor, calls, _params) = SharedProviderVendor::new(HashMap::from([
        (
            MODEL_A.to_string(),
            vec![Err(throttle.clone()), Err(throttle.clone())],
        ),
        (
            MODEL_B.to_string(),
            vec![Ok(success("reported-b", "should-not-run"))],
        ),
    ]));
    let service = service_with(vendor, 1, 8);
    let err = execute_paused(
        service,
        equal_chain(),
        payload_with_tokens(128),
        RequestDeadline::from_timeout(Duration::from_secs(30)),
    )
    .await
    .expect_err("throttling must not switch models");
    match err {
        ExecutorError::RateLimitExceeded {
            vendor,
            retry_after_ms,
        } => {
            assert_eq!(vendor, "openai");
            assert_eq!(retry_after_ms, Some(1));
        }
        other => panic!("expected RateLimitExceeded, got {other:?}"),
    }
    assert_eq!(observed_models(&calls), vec![MODEL_A, MODEL_A]);
}

#[tokio::test(start_paused = true)]
async fn exhausted_eligible_fallbacks_preserve_identical_primary_error() {
    let (vendor, calls, _params) = SharedProviderVendor::new(HashMap::from([
        (MODEL_A.to_string(), vec![Err(network()), Err(network())]),
        (MODEL_B.to_string(), vec![Err(network()), Err(network())]),
    ]));
    let service = service_with(vendor, 1, 8);
    let plan = chain(hop("alpha", MODEL_A, 512), vec![hop("beta", MODEL_B, 512)]);
    let err = execute_paused(
        service,
        plan,
        payload_with_tokens(128),
        RequestDeadline::from_timeout(Duration::from_secs(30)),
    )
    .await
    .expect_err("eligible fallbacks exhausted");
    match err {
        ExecutorError::NetworkError(message) => assert_eq!(message, "transient"),
        other => panic!("expected preserved primary error, got {other:?}"),
    }
    assert_eq!(
        observed_models(&calls),
        vec![MODEL_A, MODEL_A, MODEL_B, MODEL_B]
    );
}

#[tokio::test(start_paused = true)]
async fn deadline_during_fallback_is_authoritative() {
    let (vendor, calls, _params) = SharedProviderVendor::with_delay(
        HashMap::from([
            (MODEL_A.to_string(), vec![Err(network())]),
            (
                MODEL_B.to_string(),
                vec![Ok(success("reported-b", "should-not-run"))],
            ),
        ]),
        Duration::from_secs(1),
    );
    let service = service_with(vendor, 0, 8);
    let deadline = RequestDeadline::from_timeout(Duration::from_millis(5));
    let handle = tokio::spawn(async move {
        service
            .execute(&equal_chain(), &payload_with_tokens(128), deadline)
            .await
    });
    for _ in 0..50 {
        if !observed_models(&calls).is_empty() {
            break;
        }
        tokio::task::yield_now().await;
    }
    tokio::time::advance(Duration::from_millis(5)).await;
    tokio::task::yield_now().await;
    match handle.await.expect("join") {
        Err(ExecutorError::DeadlineExceeded) => {}
        other => panic!("expected DeadlineExceeded, got {other:?}"),
    }
    assert_eq!(observed_models(&calls), vec![MODEL_A]);
}

#[tokio::test]
async fn open_primary_target_does_not_block_same_provider_fallback() {
    let (vendor, calls, _params) = SharedProviderVendor::new(HashMap::from([
        (MODEL_A.to_string(), vec![Err(network())]),
        (
            MODEL_B.to_string(),
            vec![
                Ok(success(MODEL_B, "b-first")),
                Ok(success(MODEL_B, "b-second")),
                Ok(success(MODEL_B, "b-direct")),
            ],
        ),
    ]));
    let service = service_with_circuit(vendor, 0, 8, 1, Duration::from_secs(60));
    let plan = chain(hop("alpha", MODEL_A, 512), vec![hop("beta", MODEL_B, 512)]);
    let payload = payload_with_tokens(32);
    let deadline = RequestDeadline::from_timeout(Duration::from_secs(30));

    let first = service
        .execute(&plan, &payload, deadline)
        .await
        .expect("fallback B should succeed after A opens");
    assert_eq!(first.content, "b-first");

    let second = service
        .execute(&plan, &payload, deadline)
        .await
        .expect("open A must still reach healthy same-provider B");
    assert_eq!(second.content, "b-second");

    let direct_b = service
        .execute(&hop("beta", MODEL_B, 512), &payload, deadline)
        .await
        .expect("direct B must remain usable");
    assert_eq!(direct_b.content, "b-direct");

    assert_eq!(
        observed_models(&calls),
        vec![MODEL_A, MODEL_B, MODEL_B, MODEL_B]
    );
}

#[tokio::test]
async fn skipping_open_target_consumes_no_attempt_and_all_open_returns_circuit_open() {
    let (vendor, calls, _params) = SharedProviderVendor::new(HashMap::from([
        (MODEL_A.to_string(), vec![Err(network())]),
        (MODEL_B.to_string(), vec![Err(network())]),
    ]));
    let service = service_with_circuit(vendor, 0, 8, 1, Duration::from_secs(60));
    let plan = chain(hop("alpha", MODEL_A, 512), vec![hop("beta", MODEL_B, 512)]);
    let payload = payload_with_tokens(32);
    let deadline = RequestDeadline::from_timeout(Duration::from_secs(30));

    let first = service
        .execute(&plan, &payload, deadline)
        .await
        .expect_err("both hops fail");
    match first {
        ExecutorError::NetworkError(message) => assert_eq!(message, "transient"),
        other => panic!("expected primary NetworkError, got {other:?}"),
    }
    assert_eq!(observed_models(&calls), vec![MODEL_A, MODEL_B]);

    let second = service
        .execute(&plan, &payload, deadline)
        .await
        .expect_err("all eligible targets circuit-open");
    match second {
        ExecutorError::CircuitOpen { retry_after_ms, .. } => {
            assert!(retry_after_ms > 0);
        }
        other => panic!("expected CircuitOpen, got {other:?}"),
    }
    assert_eq!(
        observed_models(&calls),
        vec![MODEL_A, MODEL_B],
        "skipping open targets must not start provider calls"
    );
}

#[tokio::test]
async fn permanent_failures_do_not_open_target_circuit() {
    let (vendor, calls, _params) = SharedProviderVendor::new(HashMap::from([
        (
            MODEL_A.to_string(),
            vec![
                Err(ExecutorError::AuthenticationFailed("openai".into())),
                Err(ExecutorError::AuthenticationFailed("openai".into())),
            ],
        ),
        (
            MODEL_B.to_string(),
            vec![Ok(success(MODEL_B, "should-not-run"))],
        ),
    ]));
    let service = service_with_circuit(vendor, 0, 8, 1, Duration::from_secs(60));
    let plan = chain(hop("alpha", MODEL_A, 512), vec![hop("beta", MODEL_B, 512)]);
    let payload = payload_with_tokens(32);
    let deadline = RequestDeadline::from_timeout(Duration::from_secs(30));

    let first = service.execute(&plan, &payload, deadline).await;
    assert!(matches!(first, Err(ExecutorError::AuthenticationFailed(_))));
    let second = service.execute(&plan, &payload, deadline).await;
    assert!(matches!(
        second,
        Err(ExecutorError::AuthenticationFailed(_))
    ));
    assert_eq!(observed_models(&calls), vec![MODEL_A, MODEL_A]);
}

// Service Layer - Business logic for LLM execution (retry, fallback, parameter extraction)

use super::{
    budget::{
        evaluate_retry_delay, plan_retry_delay, provider_minimum_cannot_fit,
        AttemptBudget, DelayError, SystemJitter,
    },
    config::ExecutorConfig,
    error::ExecutorError,
    models::{ExecutionParams, ExecutionResult},
    repository::ExecutorRepository,
};
use crate::core::contracts::RoutePlan;
use crate::core::deadline::RequestDeadline;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::RwLock;

const DEFAULT_CIRCUIT_BREAKER_FAILURE_THRESHOLD: u32 = 3;
const DEFAULT_CIRCUIT_BREAKER_OPEN_DURATION_SECS: u64 = 30;

#[derive(Debug, Clone)]
pub(crate) struct CircuitBreakerState {
    consecutive_failures: u32,
    opened_until: Option<Instant>,
}

impl CircuitBreakerState {
    fn new() -> Self {
        Self {
            consecutive_failures: 0,
            opened_until: None,
        }
    }
}

/// Service for LLM execution with business logic.
///
/// Handles retry logic, fallback execution, and parameter extraction.
/// Delegates direct API calls to ExecutorRepository.
pub struct ExecutorService {
    /// Repository for vendor management and direct API calls
    pub(crate) repository: Arc<ExecutorRepository>,
    /// Executor configuration for retry logic and timeout settings
    pub(crate) config: ExecutorConfig,
    pub(crate) circuit_breakers: Arc<RwLock<HashMap<String, CircuitBreakerState>>>,
    pub(crate) circuit_breaker_failure_threshold: u32,
    pub(crate) circuit_breaker_open_duration: Duration,
}

impl ExecutorService {
    /// Creates ExecutorService from configuration.
    ///
    /// # Parameters
    /// - `config` - Executor configuration with vendor settings
    ///
    /// # Returns
    /// ExecutorService instance with initialized repository
    ///
    /// # Errors
    /// Returns `NoVendorsConfigured` if no vendors are configured
    pub fn from_config(config: ExecutorConfig) -> Result<Self, ExecutorError> {
        let repository = ExecutorRepository::from_config(config.clone())?;
        Ok(Self {
            repository: Arc::new(repository),
            config,
            circuit_breakers: Arc::new(RwLock::new(HashMap::new())),
            circuit_breaker_failure_threshold: DEFAULT_CIRCUIT_BREAKER_FAILURE_THRESHOLD,
            circuit_breaker_open_duration: Duration::from_secs(
                DEFAULT_CIRCUIT_BREAKER_OPEN_DURATION_SECS,
            ),
        })
    }

    async fn circuit_open_retry_after_ms(&self, vendor_id: &str) -> Option<u64> {
        let mut breakers = self.circuit_breakers.write().await;
        let state = breakers
            .entry(vendor_id.to_string())
            .or_insert_with(CircuitBreakerState::new);

        match state.opened_until {
            Some(until) if until > Instant::now() => {
                Some((until - Instant::now()).as_millis() as u64)
            }
            Some(_) => {
                state.opened_until = None;
                state.consecutive_failures = 0;
                None
            }
            None => None,
        }
    }

    async fn record_vendor_success(&self, vendor_id: &str) {
        let mut breakers = self.circuit_breakers.write().await;
        let state = breakers
            .entry(vendor_id.to_string())
            .or_insert_with(CircuitBreakerState::new);
        state.consecutive_failures = 0;
        state.opened_until = None;
    }

    async fn record_vendor_failure(&self, vendor_id: &str) {
        let mut breakers = self.circuit_breakers.write().await;
        let state = breakers
            .entry(vendor_id.to_string())
            .or_insert_with(CircuitBreakerState::new);
        state.consecutive_failures = state.consecutive_failures.saturating_add(1);

        if state.consecutive_failures >= self.circuit_breaker_failure_threshold {
            let opened_until = Instant::now() + self.circuit_breaker_open_duration;
            state.opened_until = Some(opened_until);
            state.consecutive_failures = 0;
            tracing::warn!(
                vendor_id = %vendor_id,
                open_duration_secs = self.circuit_breaker_open_duration.as_secs(),
                "Circuit breaker opened for vendor"
            );
        }
    }

    /// Returns the number of registered vendors.
    ///
    /// Useful for logging and monitoring vendor availability.
    pub fn vendor_count(&self) -> usize {
        self.repository.vendor_count()
    }

    /// Checks health of a specific vendor.
    ///
    /// Makes a lightweight API call to verify vendor connectivity and credentials.
    ///
    /// # Parameters
    /// - `vendor_name` - Name of the vendor to check (e.g., "openai")
    /// - `timeout_secs` - Timeout in seconds for the health check request
    ///
    /// # Returns
    /// - `Ok(())` if the vendor is healthy and accessible
    /// - `Err(ExecutorError)` if the vendor is unhealthy or not found
    ///
    /// # Errors
    /// - `ExecutorError::UnsupportedVendor` - Vendor not found in registry
    /// - `ExecutorError::AuthenticationFailed` - Invalid API key
    /// - `ExecutorError::TimeoutError` - Request timed out
    /// - `ExecutorError::NetworkError` - Network connectivity issues
    pub async fn check_vendor_health(
        &self,
        vendor_name: &str,
        timeout_secs: u64,
    ) -> Result<(), ExecutorError> {
        let vendor = self.repository.get_vendor(vendor_name)?;
        vendor.health_check(timeout_secs).await
    }

    /// Checks health of all configured vendors.
    ///
    /// Performs health checks on all vendors in parallel.
    ///
    /// # Parameters
    /// - `timeout_secs` - Timeout in seconds for each health check request
    ///
    /// # Returns
    /// - `Ok(())` if at least one vendor is healthy
    /// - `Err(ExecutorError)` if all vendors are unhealthy or no vendors configured
    ///
    /// # Errors
    /// - `ExecutorError::NoVendorsConfigured` - No vendors in registry
    /// - Other errors if all vendors fail health checks
    pub async fn check_all_vendors_health(
        &self,
        timeout_secs: u64,
    ) -> Result<(), ExecutorError> {
        let vendor_names = self.repository.list_vendor_names();

        if vendor_names.is_empty() {
            return Err(ExecutorError::NoVendorsConfigured);
        }

        // Check all vendors in parallel using tokio::spawn
        let mut handles = Vec::new();
        for vendor_name in vendor_names.clone() {
            let vendor = self.repository.get_vendor(&vendor_name)?;
            let handle = tokio::spawn(async move {
                let result = vendor.health_check(timeout_secs).await;
                (vendor_name, result)
            });
            handles.push(handle);
        }

        // Wait for all checks to complete
        let mut results = Vec::new();
        for handle in handles {
            match handle.await {
                Ok(result) => {
                    results.push(result);
                }
                Err(join_error) => {
                    // Task panicked or was cancelled - treat as a health check failure
                    tracing::error!(
                        error = %join_error,
                        "Health check task failed (panic or cancellation)"
                    );
                    // Create a synthetic failure result for this vendor
                    // We don't have the vendor name here, so we'll treat it as a generic failure
                    results.push((
                        "unknown".to_string(),
                        Err(ExecutorError::NetworkError(format!(
                            "Health check task failed: {}",
                            join_error
                        ))),
                    ));
                }
            }
        }

        // If at least one vendor is healthy, return Ok
        let healthy_count = results.iter().filter(|(_, result)| result.is_ok()).count();

        if healthy_count > 0 {
            tracing::debug!(
                healthy_count = healthy_count,
                total_count = vendor_names.len(),
                "Health check completed"
            );
            Ok(())
        } else {
            // All vendors failed, return the first error
            let first_error = results
                .into_iter()
                .find(|(_, result)| result.is_err())
                .map(|(_, result)| result.unwrap_err())
                .unwrap_or(ExecutorError::NoVendorsConfigured);

            tracing::warn!(
                error = ?first_error,
                "All vendors failed health check"
            );
            Err(first_error)
        }
    }

    #[cfg(test)]
    /// Executes one hop with retry logic, capped jitter, and a fresh attempt budget.
    ///
    /// Production `execute` shares one budget across primary and fallback hops.
    /// Tests use this helper when they only exercise a single hop.
    ///
    /// # Parameters
    /// - `plan` - Selected hop, including catalog target identity and wire model
    /// - `params` - Execution parameters shared across retries for this hop
    /// - `deadline` - Shared protected-request deadline; remaining time is
    ///   injected rather than reset per attempt
    ///
    /// # Returns
    /// Execution result with AI response and metrics
    ///
    /// # Errors
    /// Returns error if retries are exhausted, a non-retryable error occurs,
    /// the shared deadline elapsed, or provider Retry-After cannot fit.
    pub(crate) async fn execute_with_retry(
        &self,
        plan: &RoutePlan,
        params: &ExecutionParams,
        deadline: RequestDeadline,
    ) -> Result<ExecutionResult, ExecutorError> {
        let mut budget = AttemptBudget::new(self.config.max_total_attempts);
        self.execute_attempts(plan, params, deadline, &mut budget)
            .await
    }

    /// Runs bounded attempts for one hop against the shared request budget.
    #[tracing::instrument(
        skip(self, params, deadline, budget),
        fields(
            vendor_id = %plan.vendor_id,
            target_id = %plan.target_id,
            model_id = %plan.model_id,
            max_retries = self.config.max_retries,
        )
    )]
    async fn execute_attempts(
        &self,
        plan: &RoutePlan,
        params: &ExecutionParams,
        deadline: RequestDeadline,
        budget: &mut AttemptBudget,
    ) -> Result<ExecutionResult, ExecutorError> {
        let max_retries = self.config.max_retries;
        let mut last_error = None;
        let mut retry_after_ms: Option<u64> = None;
        let max_attempt = Duration::from_millis(self.config.timeout_ms);
        let backoff_cap = Duration::from_millis(self.config.backoff_cap_ms);

        for attempt in 0..=max_retries {
            if deadline.is_expired() {
                return Err(ExecutorError::DeadlineExceeded);
            }
            if budget.remaining() == 0 {
                break;
            }

            if attempt > 0 {
                let provider_delay = retry_after_ms.take().map(Duration::from_millis);
                let planned = plan_retry_delay(
                    attempt,
                    backoff_cap,
                    provider_delay,
                    &mut SystemJitter,
                );
                match evaluate_retry_delay(planned, deadline.remaining()) {
                    Ok(wait) if wait.is_zero() => {}
                    Ok(wait) => {
                        tracing::info!(
                            attempt,
                            max_retries,
                            vendor_id = %plan.vendor_id,
                            target_id = %plan.target_id,
                            model_id = %plan.model_id,
                            delay_ms = wait.as_millis() as u64,
                            "Retrying execution"
                        );
                        if tokio::time::timeout_at(
                            deadline.as_instant(),
                            tokio::time::sleep(wait),
                        )
                        .await
                        .is_err()
                        {
                            return Err(ExecutorError::DeadlineExceeded);
                        }
                    }
                    Err(DelayError::ProviderDelayCannotFit { retry_after }) => {
                        tracing::info!(
                            vendor_id = %plan.vendor_id,
                            target_id = %plan.target_id,
                            model_id = %plan.model_id,
                            retry_after_ms = retry_after.as_millis() as u64,
                            remaining_ms = deadline.remaining().as_millis() as u64,
                            "Provider retry delay cannot fit remaining deadline"
                        );
                        return Err(match last_error.take() {
                            Some(error @ ExecutorError::RateLimitExceeded { .. }) => {
                                error
                            }
                            _ => ExecutorError::RateLimitExceeded {
                                vendor: plan.vendor_id.clone(),
                                retry_after_ms: Some(
                                    u64::try_from(retry_after.as_millis())
                                        .unwrap_or(u64::MAX),
                                ),
                            },
                        });
                    }
                    Err(DelayError::DeadlineExceeded) => {
                        return Err(ExecutorError::DeadlineExceeded);
                    }
                }
                if deadline.is_expired() {
                    return Err(ExecutorError::DeadlineExceeded);
                }
            }

            if !budget.try_start() {
                break;
            }

            let Some(attempt_timeout) = deadline.cap(max_attempt) else {
                return Err(ExecutorError::DeadlineExceeded);
            };
            match tokio::time::timeout(
                attempt_timeout,
                self.repository.call_llm(plan, params),
            )
            .await
            {
                Ok(Ok(result)) => {
                    if attempt > 0 {
                        tracing::info!(
                            attempt,
                            vendor_id = %plan.vendor_id,
                            target_id = %plan.target_id,
                            model_id = %plan.model_id,
                            "Execution succeeded after retry"
                        );
                    }
                    return Ok(result);
                }
                Ok(Err(e)) => {
                    if Self::is_retryable_error(&e) {
                        retry_after_ms = match &e {
                            ExecutorError::RateLimitExceeded {
                                retry_after_ms: Some(ms),
                                ..
                            } => Some(*ms),
                            _ => None,
                        };
                        if let Some(terminal) =
                            Self::terminate_for_provider_minimum(&e, deadline)
                        {
                            tracing::info!(
                                vendor_id = %plan.vendor_id,
                                target_id = %plan.target_id,
                                model_id = %plan.model_id,
                                retry_after_ms = retry_after_ms.unwrap_or(0),
                                remaining_ms = deadline.remaining().as_millis() as u64,
                                "Provider retry delay cannot fit remaining deadline"
                            );
                            return Err(terminal);
                        }
                        tracing::warn!(
                            attempt,
                            max_retries,
                            vendor_id = %plan.vendor_id,
                            target_id = %plan.target_id,
                            model_id = %plan.model_id,
                            "Retryable error"
                        );
                        last_error = Some(e);
                        continue;
                    } else {
                        return Err(e);
                    }
                }
                Err(_elapsed) => {
                    if deadline.is_expired() {
                        return Err(ExecutorError::DeadlineExceeded);
                    }
                    let e =
                        ExecutorError::TimeoutError(attempt_timeout.as_millis() as u64);
                    tracing::warn!(
                        attempt,
                        max_retries,
                        vendor_id = %plan.vendor_id,
                        target_id = %plan.target_id,
                        model_id = %plan.model_id,
                        "Retryable error"
                    );
                    last_error = Some(e);
                    continue;
                }
            }
        }

        Err(last_error.unwrap_or_else(|| {
            ExecutorError::ApiCallFailed("Max retries exceeded".to_string())
        }))
    }

    /// Terminates the whole request when a valid Retry-After cannot fit.
    ///
    /// Actual deadline expiry is `DeadlineExceeded` (504). A provider minimum
    /// that cannot finish in remaining time is `RateLimitExceeded` (429) and
    /// must not start later retries or configured fallbacks.
    fn terminate_for_provider_minimum(
        error: &ExecutorError,
        deadline: RequestDeadline,
    ) -> Option<ExecutorError> {
        let ExecutorError::RateLimitExceeded {
            retry_after_ms: Some(ms),
            ..
        } = error
        else {
            return None;
        };
        if deadline.is_expired() {
            return Some(ExecutorError::DeadlineExceeded);
        }
        if provider_minimum_cannot_fit(
            Some(Duration::from_millis(*ms)),
            deadline.remaining(),
        ) {
            Some(error.clone())
        } else {
            None
        }
    }

    /// Determines if an error is retryable.
    ///
    /// # Retryable Errors
    /// - NetworkError - Network connectivity issues
    /// - TimeoutError - Request timeout
    /// - RateLimitExceeded - Vendor rate limit hit
    /// - ApiCallFailed - Generic API call failure
    ///
    /// # Non-Retryable Errors
    /// - AuthenticationFailed - Invalid API key (won't fix with retry)
    /// - InvalidPayload - Bad request format (won't fix with retry)
    /// - UpstreamRejected - Permanent upstream HTTP rejection
    /// - UnsupportedVendor - Vendor not configured (won't fix with retry)
    /// - UnsupportedModel - Model not supported (won't fix with retry)
    /// - JsonError - Malformed upstream JSON (protocol fault)
    /// - InvalidUpstreamResult - Empty choices, missing fields, or invalid usage
    /// - MissingPricing - Configured prices are absent
    /// - DeadlineExceeded - Overall request deadline elapsed
    pub(crate) fn is_retryable_error(error: &ExecutorError) -> bool {
        matches!(
            error,
            ExecutorError::NetworkError(_)
                | ExecutorError::TimeoutError(_)
                | ExecutorError::RateLimitExceeded { .. }
                | ExecutorError::ApiCallFailed(_)
        )
    }

    /// Executes fallback plans sequentially.
    ///
    /// # Flow
    /// 1. Check if fallback plans exist
    /// 2. Try each fallback plan sequentially
    /// 3. Each fallback gets full retry logic via execute_with_retry()
    /// 4. Return on first successful fallback
    /// 5. Return primary error if all fallbacks fail
    ///
    /// # Parameters
    /// - `fallback_plans` - List of fallback routing plans
    /// - `params` - Execution parameters (shared across all attempts)
    /// - `primary_error` - Error from primary execution attempt
    /// - `deadline` - Shared protected-request deadline
    /// - `budget` - Shared actual-attempt counter; fallback does not reset it
    ///
    /// # Returns
    /// Execution result from first successful fallback
    ///
    /// # Errors
    /// Returns primary error if no fallbacks exist or all fallbacks fail.
    /// A valid provider Retry-After that cannot fit remaining time terminates
    /// the whole request as rate-limited, including later configured fallbacks.
    /// Overall deadline expiry is authoritative over the primary error.
    pub(crate) async fn execute_fallbacks(
        &self,
        fallback_plans: &[RoutePlan],
        params: &ExecutionParams,
        primary_error: ExecutorError,
        deadline: RequestDeadline,
        budget: &mut AttemptBudget,
    ) -> Result<ExecutionResult, ExecutorError> {
        if deadline.is_expired() {
            return Err(ExecutorError::DeadlineExceeded);
        }
        if fallback_plans.is_empty() {
            return Err(primary_error);
        }

        tracing::info!(
            "Primary execution failed, attempting {} fallback plans",
            fallback_plans.len()
        );

        for (index, fallback) in fallback_plans.iter().enumerate() {
            if deadline.is_expired() {
                return Err(ExecutorError::DeadlineExceeded);
            }
            if let Some(retry_after_ms) =
                self.circuit_open_retry_after_ms(&fallback.vendor_id).await
            {
                tracing::warn!(
                    fallback_index = index + 1,
                    vendor_id = %fallback.vendor_id,
                    retry_after_ms = retry_after_ms,
                    "Skipping fallback due to open circuit"
                );
                continue;
            }

            tracing::info!(
                "Attempting fallback {}/{}: vendor={}, target={}, model={}",
                index + 1,
                fallback_plans.len(),
                fallback.vendor_id,
                fallback.target_id,
                fallback.model_id
            );

            match self
                .execute_attempts(fallback, params, deadline, budget)
                .await
            {
                Ok(result) => {
                    self.record_vendor_success(&fallback.vendor_id).await;
                    tracing::info!(
                        "Fallback {}/{} succeeded: vendor={}, target={}, model={}",
                        index + 1,
                        fallback_plans.len(),
                        fallback.vendor_id,
                        fallback.target_id,
                        fallback.model_id
                    );
                    return Ok(result);
                }
                Err(ExecutorError::DeadlineExceeded) => {
                    return Err(ExecutorError::DeadlineExceeded);
                }
                Err(e) => {
                    if Self::is_retryable_error(&e) {
                        self.record_vendor_failure(&fallback.vendor_id).await;
                    }
                    if let Some(terminal) =
                        Self::terminate_for_provider_minimum(&e, deadline)
                    {
                        return Err(terminal);
                    }
                    tracing::warn!(
                        fallback_index = index + 1,
                        fallback_count = fallback_plans.len(),
                        vendor_id = %fallback.vendor_id,
                        target_id = %fallback.target_id,
                        model_id = %fallback.model_id,
                        "Fallback attempt failed"
                    );
                    continue;
                }
            }
        }

        if deadline.is_expired() {
            return Err(ExecutorError::DeadlineExceeded);
        }
        Err(primary_error)
    }

    /// Extracts execution parameters from request payload.
    ///
    /// # Parameters
    /// - `payload` - JSON payload containing execution parameters
    ///
    /// # Returns
    /// Extracted execution parameters
    ///
    /// # Errors
    /// Returns `InvalidPayload` if:
    /// - `messages` field is missing or invalid
    /// - Required fields have wrong types
    pub(crate) fn extract_params(
        payload: &serde_json::Value,
    ) -> Result<ExecutionParams, ExecutorError> {
        // Extract messages (required field)
        let messages = payload
            .get("messages")
            .and_then(|v| serde_json::from_value(v.clone()).ok())
            .ok_or_else(|| {
                ExecutorError::InvalidPayload(
                    "Missing or invalid 'messages' field".to_string(),
                )
            })?;

        // Extract optional parameters with type validation
        let temperature = payload.get("temperature").and_then(|v| v.as_f64());
        let max_tokens = payload.get("max_tokens").and_then(|v| v.as_i64());
        let top_p = payload.get("top_p").and_then(|v| v.as_f64());
        let stream = payload
            .get("stream")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        Ok(ExecutionParams {
            messages,
            temperature,
            max_tokens,
            top_p,
            stream,
        })
    }

    /// Executes LLM call based on routing plan.
    ///
    /// This is a CHILD SPAN. It automatically inherits `request_id` from parent.
    ///
    /// # Flow
    /// 1. Extract parameters from payload
    /// 2. Try primary plan with retry logic
    /// 3. On failure, try fallback plans sequentially
    /// 4. Return result or error
    ///
    /// # Parameters
    /// - `plan` - Routing plan from Router Service
    /// - `payload` - Original request payload
    /// - `deadline` - Shared protected-request deadline covering earlier
    ///   authentication and body work
    ///
    /// # Returns
    /// Execution result with AI response and metrics
    ///
    /// # Errors
    /// Returns error if:
    /// - Payload is invalid
    /// - Primary execution fails and no fallbacks succeed
    /// - The shared deadline elapsed before or during execution
    #[tracing::instrument(
        skip(self, payload, deadline),
        fields(
            vendor_id = %plan.vendor_id,
            target_id = %plan.target_id,
            model_id = %plan.model_id,
        )
    )]
    pub async fn execute(
        &self,
        plan: &RoutePlan,
        payload: &serde_json::Value,
        deadline: RequestDeadline,
    ) -> Result<ExecutionResult, ExecutorError> {
        if deadline.is_expired() {
            return Err(ExecutorError::DeadlineExceeded);
        }

        let mut budget = AttemptBudget::new(self.config.max_total_attempts);

        if let Some(retry_after_ms) =
            self.circuit_open_retry_after_ms(&plan.vendor_id).await
        {
            tracing::warn!(
                vendor_id = %plan.vendor_id,
                retry_after_ms = retry_after_ms,
                "Primary vendor circuit is open, skipping primary execution"
            );

            let params = Self::extract_params(payload)?;
            let circuit_open_error = ExecutorError::CircuitOpen {
                vendor: plan.vendor_id.clone(),
                retry_after_ms,
            };
            return self
                .execute_fallbacks(
                    &plan.fallback_plans,
                    &params,
                    circuit_open_error,
                    deadline,
                    &mut budget,
                )
                .await;
        }

        let params = Self::extract_params(payload)?;

        tracing::info!(
            "Executing LLM call: vendor={}, target={}, model={}",
            plan.vendor_id,
            plan.target_id,
            plan.model_id
        );

        match self
            .execute_attempts(plan, &params, deadline, &mut budget)
            .await
        {
            Ok(result) => {
                self.record_vendor_success(&plan.vendor_id).await;
                tracing::info!(
                    "Primary execution succeeded: vendor={}, target={}, model={}, tokens={}, cost=${}",
                    plan.vendor_id,
                    plan.target_id,
                    plan.model_id,
                    result.prompt_tokens + result.completion_tokens,
                    result.total_cost
                );
                Ok(result)
            }
            Err(ExecutorError::DeadlineExceeded) => Err(ExecutorError::DeadlineExceeded),
            Err(primary_error) => {
                if Self::is_retryable_error(&primary_error) {
                    self.record_vendor_failure(&plan.vendor_id).await;
                }
                if let Some(terminal) =
                    Self::terminate_for_provider_minimum(&primary_error, deadline)
                {
                    return Err(terminal);
                }
                self.execute_fallbacks(
                    &plan.fallback_plans,
                    &params,
                    primary_error,
                    deadline,
                    &mut budget,
                )
                .await
            }
        }
    }
}

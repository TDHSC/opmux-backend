// Service Layer - Business logic for LLM execution (retry, fallback, parameter extraction)

use super::{
    attempt::AttemptContext,
    budget::{
        evaluate_retry_delay, plan_retry_delay, provider_minimum_cannot_fit,
        AttemptBudget, DelayError, SystemJitter,
    },
    circuit::{CircuitAdmission, TargetCircuitRegistry},
    config::ExecutorConfig,
    error::ExecutorError,
    models::{ExecutionParams, ExecutionResult},
    repository::ExecutorRepository,
};
use crate::core::contracts::RoutePlan;
use crate::core::deadline::RequestDeadline;
use std::sync::Arc;
use std::time::Duration;

/// Service for LLM execution with business logic.
///
/// Handles retry logic, fallback execution, and parameter extraction.
/// Delegates direct API calls to ExecutorRepository.
pub struct ExecutorService {
    /// Repository for vendor management and direct API calls
    pub(crate) repository: Arc<ExecutorRepository>,
    /// Executor configuration for retry logic and timeout settings
    pub(crate) config: ExecutorConfig,
    /// Target-scoped circuit breakers with single-flight half-open probes
    pub(crate) circuits: TargetCircuitRegistry,
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
        Ok(Self::from_repository(repository, config))
    }

    /// Builds a service around an already-constructed repository.
    pub(crate) fn from_repository(
        repository: ExecutorRepository,
        config: ExecutorConfig,
    ) -> Self {
        Self {
            circuits: TargetCircuitRegistry::from_config(&config),
            repository: Arc::new(repository),
            config,
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
        self.execute_attempts(
            plan,
            params,
            deadline,
            &mut budget,
            self.config.max_retries,
        )
        .await
    }

    /// Runs one hop through target-scoped circuit admission.
    async fn execute_hop(
        &self,
        plan: &RoutePlan,
        params: &ExecutionParams,
        deadline: RequestDeadline,
        budget: &mut AttemptBudget,
    ) -> Result<ExecutionResult, ExecutorError> {
        if deadline.is_expired() {
            return Err(ExecutorError::DeadlineExceeded);
        }
        if budget.remaining() == 0 {
            return Err(ExecutorError::ApiCallFailed(
                "Max retries exceeded".to_string(),
            ));
        }

        match self.circuits.admit(&plan.target_id) {
            CircuitAdmission::Reject { retry_after_ms } => {
                tracing::warn!(
                    vendor_id = %plan.vendor_id,
                    target_id = %plan.target_id,
                    model_id = %plan.model_id,
                    retry_after_ms,
                    "Skipping target due to open circuit"
                );
                Err(ExecutorError::CircuitOpen {
                    vendor: plan.vendor_id.clone(),
                    retry_after_ms,
                })
            }
            CircuitAdmission::Probe(guard) => {
                tracing::info!(
                    vendor_id = %plan.vendor_id,
                    target_id = %plan.target_id,
                    model_id = %plan.model_id,
                    "Admitting half-open recovery probe"
                );
                match self
                    .execute_attempts(plan, params, deadline, budget, 0)
                    .await
                {
                    Ok(result) => {
                        guard.success();
                        Ok(result)
                    }
                    Err(ExecutorError::DeadlineExceeded) => {
                        Err(ExecutorError::DeadlineExceeded)
                    }
                    Err(error) if Self::is_circuit_failure(&error) => {
                        guard.transient_failure();
                        Err(error)
                    }
                    Err(error) => {
                        guard.ignore_permanent();
                        Err(error)
                    }
                }
            }
            CircuitAdmission::Allow(permit) => {
                match self
                    .execute_attempts(
                        plan,
                        params,
                        deadline,
                        budget,
                        self.config.max_retries,
                    )
                    .await
                {
                    Ok(result) => {
                        permit.success();
                        Ok(result)
                    }
                    Err(ExecutorError::DeadlineExceeded) => {
                        Err(ExecutorError::DeadlineExceeded)
                    }
                    Err(error) => {
                        if Self::is_circuit_failure(&error) {
                            permit.transient_failure();
                        }
                        Err(error)
                    }
                }
            }
        }
    }

    /// Runs bounded attempts for one hop against the shared request budget.
    #[tracing::instrument(
        skip(self, params, deadline, budget),
        fields(
            vendor_id = %plan.vendor_id,
            target_id = %plan.target_id,
            model_id = %plan.model_id,
            max_retries,
        )
    )]
    async fn execute_attempts(
        &self,
        plan: &RoutePlan,
        params: &ExecutionParams,
        deadline: RequestDeadline,
        budget: &mut AttemptBudget,
        max_retries: u32,
    ) -> Result<ExecutionResult, ExecutorError> {
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
            let cutoff = tokio::time::Instant::now() + attempt_timeout;
            let attempt_ctx = AttemptContext::new(cutoff);
            let error = match tokio::time::timeout_at(
                cutoff,
                self.repository.call_llm(plan, params, &attempt_ctx),
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
                Ok(Err(error)) => error,
                Err(_elapsed) => match attempt_ctx.observed() {
                    Some(observed) => observed,
                    None if deadline.is_expired() => {
                        return Err(ExecutorError::DeadlineExceeded);
                    }
                    None => {
                        ExecutorError::TimeoutError(attempt_timeout.as_millis() as u64)
                    }
                },
            };
            if Self::is_retryable_error(&error) {
                retry_after_ms = match &error {
                    ExecutorError::RateLimitExceeded {
                        retry_after_ms: Some(ms),
                        ..
                    } => Some(*ms),
                    _ => None,
                };
                if let Some(terminal) =
                    Self::terminate_for_provider_minimum(&error, deadline)
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
                last_error = Some(error);
                continue;
            }
            return Err(error);
        }

        Err(last_error.unwrap_or_else(|| {
            ExecutorError::ApiCallFailed("Max retries exceeded".to_string())
        }))
    }

    /// Terminates the whole request when a valid Retry-After cannot fit.
    ///
    /// A provider minimum that cannot finish in remaining time is
    /// `RateLimitExceeded` (429), including when body refinement later
    /// exhausts the budget. Actual deadline expiry without a cannot-fit
    /// provider minimum is `DeadlineExceeded` (504). This must not start
    /// later retries or configured fallbacks.
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
        if provider_minimum_cannot_fit(
            Some(Duration::from_millis(*ms)),
            deadline.remaining(),
        ) {
            return Some(error.clone());
        }
        if deadline.is_expired() {
            Some(ExecutorError::DeadlineExceeded)
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

    /// True when a hop failure may open or reopen a transient-failure circuit.
    ///
    /// Transport, attempt-timeout, and transient provider failures count.
    /// Throttling, quota, credentials, and protocol errors do not, for both
    /// normal hops and half-open probes.
    pub(crate) fn is_circuit_failure(error: &ExecutorError) -> bool {
        matches!(
            error,
            ExecutorError::NetworkError(_)
                | ExecutorError::TimeoutError(_)
                | ExecutorError::ApiCallFailed(_)
        )
    }

    /// True when a failed hop may continue to a later configured target.
    ///
    /// Transient transport, attempt-timeout, and provider 5xx failures may
    /// fall back. Client, protocol, shared-credential, quota, and same-account
    /// throttling errors do not switch models. Open primary circuits may still
    /// degrade to a later target.
    pub(crate) fn is_fallback_eligible(error: &ExecutorError) -> bool {
        matches!(
            error,
            ExecutorError::NetworkError(_)
                | ExecutorError::TimeoutError(_)
                | ExecutorError::ApiCallFailed(_)
                | ExecutorError::CircuitOpen { .. }
        )
    }

    /// True when this hop can serve the already-validated generation params.
    ///
    /// A smaller output-token cap skips the hop. Requested `max_tokens` are
    /// never clamped or rewritten.
    pub(crate) fn hop_supports_params(
        plan: &RoutePlan,
        params: &ExecutionParams,
    ) -> bool {
        match params.max_tokens {
            Some(requested) if requested > 0 => u32::try_from(requested)
                .map(|requested| requested <= plan.max_output_tokens)
                .unwrap_or(false),
            _ => true,
        }
    }

    /// Executes eligible fallback hops sequentially under the shared budget.
    ///
    /// # Flow
    /// 1. Check if fallback plans exist
    /// 2. Skip hops whose output-token cap cannot satisfy the request
    /// 3. Try each remaining hop in catalog order with shared retries/budget
    /// 4. Stop switching after an ineligible error such as throttling
    /// 5. Return the first successful hop, else the original primary error
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
            if !Self::hop_supports_params(fallback, params) {
                tracing::info!(
                    fallback_index = index + 1,
                    target_id = %fallback.target_id,
                    model_id = %fallback.model_id,
                    requested_max_tokens = params.max_tokens,
                    target_max_output_tokens = fallback.max_output_tokens,
                    "Skipping fallback target that cannot satisfy the requested token cap"
                );
                continue;
            }

            tracing::info!(
                fallback_index = index + 1,
                fallback_count = fallback_plans.len(),
                vendor_id = %fallback.vendor_id,
                target_id = %fallback.target_id,
                model_id = %fallback.model_id,
                "Attempting configured fallback target"
            );

            match self.execute_hop(fallback, params, deadline, budget).await {
                Ok(result) => {
                    tracing::info!(
                        fallback_index = index + 1,
                        fallback_count = fallback_plans.len(),
                        vendor_id = %fallback.vendor_id,
                        target_id = %fallback.target_id,
                        model_id = %fallback.model_id,
                        "Fallback target succeeded"
                    );
                    return Ok(result);
                }
                Err(ExecutorError::DeadlineExceeded) => {
                    return Err(ExecutorError::DeadlineExceeded);
                }
                Err(e) => {
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
                    if !Self::is_fallback_eligible(&e) {
                        break;
                    }
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
        let params = Self::extract_params(payload)?;

        tracing::info!(
            "Executing LLM call: vendor={}, target={}, model={}",
            plan.vendor_id,
            plan.target_id,
            plan.model_id
        );

        match self.execute_hop(plan, &params, deadline, &mut budget).await {
            Ok(result) => {
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
                if let Some(terminal) =
                    Self::terminate_for_provider_minimum(&primary_error, deadline)
                {
                    return Err(terminal);
                }
                if !Self::is_fallback_eligible(&primary_error) {
                    return Err(primary_error);
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

# Executor Layer - Technical Design

> **Historical design.** Current executor lives in `gateway/src/features/executor/`. OpenAI is the
> only vendor. Pricing is **per million** tokens on catalog **target IDs**, not per 1000 tokens and
> not a hardcoded model table. Retries, fallback, and target-scoped circuits are implemented in
> `ExecutorService`; vendor clients do not retry. Streaming, Anthropic/Cohere, and RouterService as
> a microservice are deferred. Local verification is **SIMULATED ONLY**. See
> [README.md](../../README.md).

## Overview

The Executor Layer is an internal module within Gateway Service responsible for executing actual LLM
API calls based on routing decisions from RouterService. It is **not a separate microservice** but a
well-encapsulated module within the Gateway codebase.

## Responsibilities

### What Executor Layer Does ✅

- Execute LLM API calls based on RoutePlan (vendor_id, model_id)
- Extract execution parameters from original_payload (temperature, max_tokens, etc.)
- Integrate multiple LLM vendor SDKs (OpenAI, Anthropic, Cohere, etc.)
- Handle HTTP calls, retries, timeouts
- Calculate actual token usage and costs
- Support streaming responses (future: SSE/WebSocket)

### What Executor Layer Does NOT Do ❌

- Routing decisions (RouterService's responsibility)
- Prompt rewriting (RewriteService's responsibility)
- Context management (MemoryService's responsibility)
- Authentication (Gateway middleware's responsibility)

## Architecture

### Module Structure

```
gateway/src/executor/
├── mod.rs              # Module exports and re-exports
├── config.rs           # Configuration data structures (no business logic)
├── error.rs            # Executor-specific error types
├── models.rs           # Data models (ExecutionParams, ExecutionResult, etc.)
├── service.rs          # ExecutorService orchestration layer
└── vendors/            # Vendor-specific implementations
    ├── mod.rs          # Vendor module exports
    ├── traits.rs       # LLMVendor trait definition
    ├── openai.rs       # OpenAI Chat Completions API
    └── anthropic.rs    # Anthropic Messages API (future)
```

### Component Responsibilities

#### 1. Configuration Layer (`config.rs`)

**Responsibility**: Store and load configuration data ONLY

**Structures**:

- `ModelPricing`: Pricing per 1000 tokens (prompt + completion)
- `OpenAIConfig`: OpenAI vendor configuration
- `ExecutorConfig`: Top-level executor configuration

**Methods**:

- `from_env()`: Load configuration from environment variables
- `validate()`: Validate configuration completeness (may log for convenience)
- `Default`: Provide default values

**Design Principle**: Configuration layer does NOT contain business logic. No model support checks,
no cost calculations. Only data storage, loading, and validation.

#### 2. Vendor Trait (`vendors/traits.rs`)

**LLMVendor Trait**: Unified interface for all LLM vendors

```rust
#[async_trait]
pub trait LLMVendor: Send + Sync {
    /// Execute LLM API call
    async fn execute(&self, model: &str, params: ExecutionParams) -> Result<ExecutionResult, ExecutorError>;

    /// Return vendor identifier
    fn vendor_id(&self) -> &str;

    /// Check if vendor supports model (business logic)
    fn supports_model(&self, model: &str) -> bool;

    /// Calculate cost based on token usage (business logic)
    fn calculate_cost(&self, prompt_tokens: i64, completion_tokens: i64, model: &str) -> f64;
}
```

#### 3. Vendor Implementations (`vendors/*.rs`)

**Responsibility**: Business logic and API integration

**OpenAIVendor** (`vendors/openai.rs`):

- Integrates with OpenAI Chat Completions API
- Supported models: gpt-4, gpt-4-turbo, gpt-3.5-turbo
- Business logic:
  - `supports_model()`: Check if model is in config.supported_models
  - `calculate_cost()`: Calculate cost using config.pricing
- API integration:
  - Serialize request to OpenAI format
  - Handle authentication (Bearer token)
  - Parse response and extract tokens/cost
  - Error handling (rate limits, auth failures, etc.)

**Future Vendors**:

- `AnthropicVendor`: Claude models
- `CohereVendor`: Command models

#### 4. Service Layer (`service.rs`)

**ExecutorService**: Orchestrates LLM execution workflow

**Responsibilities**:

- Vendor registry and selection (by vendor_id)
- Parameter extraction from original_payload
- Error handling and retry logic
- Streaming response support (future)

**Status**: Placeholder implementation, not yet complete

#### 5. Error Handling (`error.rs`)

**ExecutorError**: Business operation focused errors

- `UnsupportedVendor(String)`: Vendor not configured
- `UnsupportedModel(String, String)`: Model not supported by vendor
- `InvalidPayload(String)`: Request format invalid
- `ApiCallFailed(String)`: LLM API call failed
- `RateLimitExceeded(String)`: Vendor rate limit hit
- `AuthenticationFailed(String)`: Invalid API key
- `TimeoutError(u64)`: Request timeout
- `NetworkError(String)`: Network connectivity issues

**HTTP Status Mapping**:

- 400: UnsupportedVendor, UnsupportedModel, InvalidPayload
- 401: AuthenticationFailed
- 429: RateLimitExceeded
- 500: ApiCallFailed, NetworkError, TimeoutError

## Configuration

### Environment Variables

**OpenAI Configuration**:

- `OPENAI_API_KEY`: API key (required)
- `OPENAI_BASE_URL`: Custom endpoint (optional, default: https://api.openai.com/v1)
- `OPENAI_TIMEOUT_MS`: Request timeout (optional, default: 30000ms)

**Executor Configuration**:

- `EXECUTOR_TIMEOUT_MS`: Global timeout (optional, default: 30000ms)
- `EXECUTOR_MAX_RETRIES`: Max retry attempts (optional, default: 3)

**Future Vendors**:

- `ANTHROPIC_API_KEY`: Anthropic API key
- `COHERE_API_KEY`: Cohere API key

### Pricing Configuration

Pricing is configured in code (default values) but can be overridden in the future:

**OpenAI Pricing** (as of 2024):

- GPT-4: $0.03/1K prompt tokens, $0.06/1K completion tokens
- GPT-4 Turbo: $0.01/1K prompt tokens, $0.03/1K completion tokens
- GPT-3.5 Turbo: $0.0005/1K prompt tokens, $0.0015/1K completion tokens

**Future Enhancement**: Load pricing from database or configuration file for dynamic updates.

## Integration Flow

### Request Flow

```
Client Request
    ↓
Ingress Handler (HTTP)
    ↓
Ingress Service (Business Logic)
    ↓
Repository.optimize_route() → RouterService (gRPC)
    ↓ (returns RoutePlan: { vendor_id: "openai", model_id: "gpt-4" })
Repository.execute_llm_call() → ExecutorService
    ↓
ExecutorService.execute(plan, payload)
    ↓
1. Select vendor by vendor_id → OpenAIVendor
2. Extract params from payload → ExecutionParams { messages, temperature, max_tokens, ... }
3. Validate model support → OpenAIVendor.supports_model("gpt-4")
    ↓
OpenAIVendor.execute("gpt-4", params)
    ↓
1. Build ChatCompletionRequest
2. POST to OpenAI API
3. Parse ChatCompletionResponse
4. Calculate cost using pricing config
    ↓
ExecutionResult { content, model_used, prompt_tokens, completion_tokens, total_cost, finish_reason }
    ↓
Return to Ingress Service
    ↓
Return to Client
```

### Error Handling Flow

```
OpenAI API Error
    ↓
Parse HTTP status code
    ↓
401/403 → ExecutorError::AuthenticationFailed
429 → ExecutorError::RateLimitExceeded (with Retry-After when present)
4xx → ExecutorError::InvalidPayload
5xx → ExecutorError::ApiCallFailed
Timeout → ExecutorError::TimeoutError
    ↓
Convert to HTTP Response (IntoResponse)
    ↓
Return error JSON to client
```

## Design Principles

### 1. Configuration vs Business Logic Separation

**Configuration Layer** (`config.rs`):

- ✅ Load from environment variables
- ✅ Store configuration data (API keys, URLs, pricing, models)
- ✅ Validate configuration completeness
- ✅ Provide default values
- ❌ NO business logic (model checks, cost calculations)

**Vendor Layer** (`vendors/*.rs`):

- ✅ Business logic (model support checks, cost calculations)
- ✅ API integration and error handling
- ✅ Access configuration data directly (e.g., `self.config.pricing.get(model)`)

**Rationale**: Configuration should be pure data. Business logic belongs in the layer that uses the
data.

### 2. Vendor Abstraction

All vendors implement the same `LLMVendor` trait, allowing:

- Easy addition of new vendors
- Vendor selection at runtime
- Consistent error handling
- Unified testing interface

### 3. Error Design

Errors are modeled by **business operation** (what failed), not technical cause:

- ✅ `UnsupportedModel`: Tells us the operation (model selection) failed
- ❌ `HttpError`: Only tells us the technical cause, loses business context

## ExecutorService Implementation Details (Task 8.6)

### Architecture Overview

ExecutorService acts as the **orchestration layer** that coordinates LLM execution workflow:

```
IngressRepository.execute_llm_call()
    ↓
ExecutorService.execute(plan, payload)
    ↓
1. Extract parameters from payload
2. Select vendor by vendor_id
3. Validate model support
4. Execute with retry logic
5. Handle fallback plans if primary fails
    ↓
ExecutionResult
```

### Component Design

**Implementation Approach**: **Modify** existing `gateway/src/executor/service.rs`, not rewrite

**Current State** (placeholder):

```rust
pub struct ExecutorService {
    // Vendor registry will be added in Task 2.1
}

impl ExecutorService {
    pub fn new() -> Self {
        Self {}
    }

    pub async fn execute(
        &self,
        _plan: &RoutePlan,
        _payload: &serde_json::Value,
    ) -> Result<ExecutionResult, ExecutorError> {
        todo!("ExecutorService::execute not yet implemented")
    }
}
```

#### 1. ExecutorService Structure (Modify existing struct)

**Changes to make**:

```rust
use std::collections::HashMap;
use std::sync::Arc;

pub struct ExecutorService {
    /// Vendor registry: vendor_id → vendor instance
    vendors: HashMap<String, Arc<dyn LLMVendor>>,  // ADD THIS
    /// Executor configuration
    config: ExecutorConfig,  // ADD THIS
}
```

**Design Decisions**:

- ✅ **HashMap for vendor registry** - O(1) lookup by vendor_id
- ✅ **Arc<dyn LLMVendor>** - Thread-safe shared ownership, vendors are stateless
- ✅ **Config stored in service** - Needed for retry logic and timeout settings
- ✅ **Keep existing method signatures** - Only replace `todo!()` with real implementation

#### 2. Initialization (Auto-create from config)

```rust
impl ExecutorService {
    /// Creates ExecutorService from configuration.
    /// Automatically initializes all configured vendors.
    pub fn from_config(config: ExecutorConfig) -> Result<Self, ExecutorError> {
        let mut vendors: HashMap<String, Arc<dyn LLMVendor>> = HashMap::new();

        // Initialize OpenAI vendor if configured
        if let Some(openai_config) = &config.openai {
            let vendor = OpenAIVendor::new(openai_config.clone())?;
            vendors.insert("openai".to_string(), Arc::new(vendor));
        }

        // Initialize Anthropic vendor if configured (future)
        // Initialize Grok vendor if configured (future)
        // Initialize Google AI vendor if configured (future)

        if vendors.is_empty() {
            return Err(ExecutorError::NoVendorsConfigured);
        }

        Ok(Self { vendors, config })
    }
}
```

**Benefits**:

- ✅ Simple initialization in main.rs: `ExecutorService::from_config(config)?`
- ✅ Encapsulates vendor creation logic
- ✅ Easy to add new vendors (just update this method)

#### 3. Parameter Extraction

```rust
impl ExecutorService {
    /// Extracts execution parameters from request payload.
    fn extract_params(payload: &serde_json::Value) -> Result<ExecutionParams, ExecutorError> {
        // Extract messages (required)
        let messages = payload
            .get("messages")
            .and_then(|v| serde_json::from_value(v.clone()).ok())
            .ok_or_else(|| ExecutorError::InvalidPayload(
                "Missing or invalid 'messages' field".to_string()
            ))?;

        // Extract optional parameters
        let temperature = payload.get("temperature").and_then(|v| v.as_f64());
        let max_tokens = payload.get("max_tokens").and_then(|v| v.as_i64());
        let top_p = payload.get("top_p").and_then(|v| v.as_f64());
        let stream = payload.get("stream").and_then(|v| v.as_bool()).unwrap_or(false);

        Ok(ExecutionParams {
            messages,
            temperature,
            max_tokens,
            top_p,
            stream,
        })
    }
}
```

**Supported Vendors** (MVP):

- ✅ OpenAI (messages, temperature, max_tokens, top_p; stream is currently rejected)
- ✅ Anthropic (same format, compatible)
- ✅ Grok (same format, compatible)
- ✅ Google AI (same format, compatible)

**Future Enhancement**: Vendor-specific parameter extraction if needed

#### 4. Vendor Selection

```rust
impl ExecutorService {
    /// Selects vendor by vendor_id.
    fn get_vendor(&self, vendor_id: &str) -> Result<Arc<dyn LLMVendor>, ExecutorError> {
        self.vendors
            .get(vendor_id)
            .cloned()
            .ok_or_else(|| ExecutorError::UnsupportedVendor(vendor_id.to_string()))
    }
}
```

**Error Handling**:

- ❌ Vendor not found → `ExecutorError::UnsupportedVendor`
- ❌ Model not supported → `ExecutorError::UnsupportedModel`

#### 5. Execute Method with Retry and Fallback

```rust
impl ExecutorService {
    /// Executes LLM call with retry and fallback logic.
    pub async fn execute(
        &self,
        plan: &RoutePlan,
        payload: &serde_json::Value,
    ) -> Result<ExecutionResult, ExecutorError> {
        // Extract parameters once (shared across retries and fallbacks)
        let params = Self::extract_params(payload)?;

        // Try primary plan with retry
        match self.execute_with_retry(&plan.vendor_id, &plan.model_id, &params).await {
            Ok(result) => Ok(result),
            Err(primary_error) => {
                tracing::warn!(
                    "Primary execution failed: vendor={}, model={}, error={:?}",
                    plan.vendor_id,
                    plan.model_id,
                    primary_error
                );

                // Try fallback plans
                self.execute_fallbacks(&plan.fallback_plans, &params, primary_error).await
            }
        }
    }
}
```

## Current Implementation Status

### ✅ Completed

- Configuration system (ModelPricing, OpenAIConfig, ExecutorConfig)
- Environment variable loading and validation
- LLMVendor trait definition
- OpenAIVendor structure and implementation
- Error handling (ExecutorError with HTTP mapping)
- Cost calculation based on configurable pricing
- Configuration/business logic separation

### 🔄 In Progress (Task 8.6)

- ExecutorService orchestration layer design
- Retry logic with exponential backoff
- Fallback plan execution
- Integration with ingress service
- Parameter extraction from payload

### ⏸️ Not Started

- Real API testing (not tested with actual OpenAI API key)
- Anthropic vendor implementation
- Grok vendor implementation
- Google AI vendor implementation
- Streaming response support
- Circuit breaker pattern

### Retry Logic Implementation

```rust
impl ExecutorService {
    /// Executes with retry logic.
    async fn execute_with_retry(
        &self,
        vendor_id: &str,
        model_id: &str,
        params: &ExecutionParams,
    ) -> Result<ExecutionResult, ExecutorError> {
        let vendor = self.get_vendor(vendor_id)?;

        // Validate model support before attempting
        if !vendor.supports_model(model_id) {
            return Err(ExecutorError::UnsupportedModel(
                vendor_id.to_string(),
                model_id.to_string(),
            ));
        }

        let max_retries = self.config.max_retries;
        let mut last_error = None;

        for attempt in 0..=max_retries {
            if attempt > 0 {
                tracing::info!(
                    "Retrying execution: attempt {}/{}, vendor={}, model={}",
                    attempt,
                    max_retries,
                    vendor_id,
                    model_id
                );

                // Exponential backoff: 1s, 2s, 4s, 8s, ...
                let backoff_ms = 1000 * (2_u64.pow(attempt - 1));
                tokio::time::sleep(tokio::time::Duration::from_millis(backoff_ms)).await;
            }

            match vendor.execute(model_id, params.clone()).await {
                Ok(result) => {
                    if attempt > 0 {
                        tracing::info!(
                            "Execution succeeded after {} retries",
                            attempt
                        );
                    }
                    return Ok(result);
                }
                Err(e) => {
                    // Determine if error is retryable
                    if Self::is_retryable_error(&e) {
                        tracing::warn!(
                            "Retryable error on attempt {}: {:?}",
                            attempt,
                            e
                        );
                        last_error = Some(e);
                        continue;
                    } else {
                        // Non-retryable error, fail immediately
                        tracing::error!(
                            "Non-retryable error: {:?}",
                            e
                        );
                        return Err(e);
                    }
                }
            }
        }

        // All retries exhausted
        Err(last_error.unwrap_or_else(|| {
            ExecutorError::ApiCallFailed("Max retries exceeded".to_string())
        }))
    }

    /// Determines if an error is retryable.
    fn is_retryable_error(error: &ExecutorError) -> bool {
        matches!(
            error,
            ExecutorError::NetworkError(_)
                | ExecutorError::TimeoutError(_)
                | ExecutorError::RateLimitExceeded(_)
                | ExecutorError::ApiCallFailed(_)
        )
    }
}
```

**Retry Strategy**:

- ✅ **Retryable errors**: NetworkError, TimeoutError, RateLimitExceeded, ApiCallFailed
- ❌ **Non-retryable errors**: AuthenticationFailed, InvalidPayload, UnsupportedVendor,
  UnsupportedModel
- ✅ **Exponential backoff**: 1s, 2s, 4s, 8s, ...
- ✅ **Max retries**: Configurable via `config.max_retries` (default: 3)
- ✅ **Logging**: Detailed logs for each retry attempt

### Fallback Plan Execution

```rust
impl ExecutorService {
    /// Executes fallback plans sequentially.
    async fn execute_fallbacks(
        &self,
        fallback_plans: &[RoutePlan],
        params: &ExecutionParams,
        primary_error: ExecutorError,
    ) -> Result<ExecutionResult, ExecutorError> {
        if fallback_plans.is_empty() {
            // No fallbacks, return primary error
            return Err(primary_error);
        }

        tracing::info!("Attempting {} fallback plans", fallback_plans.len());

        for (index, fallback) in fallback_plans.iter().enumerate() {
            tracing::info!(
                "Trying fallback {}/{}: vendor={}, model={}",
                index + 1,
                fallback_plans.len(),
                fallback.vendor_id,
                fallback.model_id
            );

            match self.execute_with_retry(
                &fallback.vendor_id,
                &fallback.model_id,
                params,
            ).await {
                Ok(result) => {
                    tracing::info!(
                        "Fallback {} succeeded: vendor={}, model={}",
                        index + 1,
                        fallback.vendor_id,
                        fallback.model_id
                    );
                    return Ok(result);
                }
                Err(e) => {
                    tracing::warn!(
                        "Fallback {} failed: vendor={}, model={}, error={:?}",
                        index + 1,
                        fallback.vendor_id,
                        fallback.model_id,
                        e
                    );
                    // Continue to next fallback
                    continue;
                }
            }
        }

        // All fallbacks failed, return primary error
        tracing::error!("All fallback plans exhausted");
        Err(primary_error)
    }
}
```

**Fallback Strategy**:

- ✅ **Sequential execution**: Try fallbacks one by one
- ✅ **Each fallback gets retry logic**: Full retry attempts for each fallback
- ✅ **Return primary error if all fail**: Preserve original error context
- ✅ **Logging**: Detailed logs for debugging and monitoring

### Integration with IngressRepository

#### Step 1: Update IngressError to wrap ExecutorError

```rust
// gateway/src/features/ingress/error.rs

use crate::executor::error::ExecutorError;

#[derive(Debug, thiserror::Error)]
pub enum IngressError {
    // ... existing variants ...

    /// LLM execution failed (wraps ExecutorError to preserve error context)
    #[error(transparent)]
    ExecutionFailed(#[from] ExecutorError),
}
```

**Benefits**:

- ✅ Preserves structured error information
- ✅ Allows automatic conversion with `?` operator
- ✅ Upper layers can access original ExecutorError type
- ✅ Correct HTTP status code mapping (e.g., 429 for RateLimitExceeded)

#### Step 2: Update IngressRepository

```rust
// gateway/src/features/ingress/repository.rs

use crate::executor::ExecutorService;
use std::sync::Arc;

pub struct IngressRepository {
    executor_service: Arc<ExecutorService>,
}

impl IngressRepository {
    pub fn new(executor_service: Arc<ExecutorService>) -> Self {
        Self { executor_service }
    }

    pub async fn execute_llm_call(
        &self,
        plan: &RoutePlan,
        payload: &serde_json::Value,
    ) -> Result<LLMExecutionResult, IngressError> {
        // Call real ExecutorService
        // The ? operator automatically converts ExecutorError → IngressError::ExecutionFailed
        let result = self.executor_service
            .execute(plan, payload)
            .await?;

        // Convert ExecutionResult → LLMExecutionResult
        Ok(LLMExecutionResult {
            content: result.content,
            model_used: result.model_used,
            prompt_tokens: result.prompt_tokens,
            completion_tokens: result.completion_tokens,
            total_cost: result.total_cost,
            finish_reason: result.finish_reason,
        })
    }
}
```

**Key Changes**:

- ❌ **Before**: `.map_err(|e| IngressError::ExecutionFailed(e.to_string()))?` - Loses error type
- ✅ **After**: `.await?` - Preserves ExecutorError via `#[from]` attribute

````

### Initialization in main.rs

```rust
// gateway/src/main.rs

use crate::executor::{ExecutorService, config::ExecutorConfig};
use std::sync::Arc;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // ... existing setup ...

    // Load and validate executor configuration
    let executor_config = ExecutorConfig::from_env();
    executor_config.validate();

    // Create ExecutorService (auto-initializes vendors)
    let executor_service = Arc::new(
        ExecutorService::from_config(executor_config)
            .expect("Failed to initialize ExecutorService")
    );

    tracing::info!(
        "ExecutorService initialized with {} vendors",
        executor_service.vendor_count()
    );

    // Create IngressRepository with ExecutorService
    let ingress_repo = Arc::new(IngressRepository::new(executor_service.clone()));

    // Create IngressService with repository
    let ingress_service = Arc::new(IngressService::new(ingress_repo));

    // ... rest of setup ...
}
````

**Helper Method**:

```rust
impl ExecutorService {
    /// Returns the number of registered vendors.
    pub fn vendor_count(&self) -> usize {
        self.vendors.len()
    }
}
```

### Error Handling Updates

Add new error variant to `executor/error.rs`:

```rust
#[derive(Debug, thiserror::Error)]
pub enum ExecutorError {
    // ... existing variants ...

    /// No vendors configured in ExecutorConfig
    #[error("No vendors configured")]
    NoVendorsConfigured,
}
```

**HTTP Status Mapping**:

```rust
impl IntoResponse for ExecutorError {
    fn into_response(self) -> Response {
        let (status, message) = match self {
            // ... existing mappings ...
            Self::NoVendorsConfigured => (
                StatusCode::INTERNAL_SERVER_ERROR,
                "No LLM vendors configured".to_string(),
            ),
        };

        let body = Json(json!({ "error": message }));
        (status, body).into_response()
    }
}
```

## Implementation Guide

### Files to Modify (Not Rewrite)

| File                                         | Modification Type | Description                                      |
| -------------------------------------------- | ----------------- | ------------------------------------------------ |
| `gateway/src/executor/service.rs`            | **Modify**        | Add fields, replace `todo!()` with real logic    |
| `gateway/src/executor/error.rs`              | **Modify**        | Add `NoVendorsConfigured` variant                |
| `gateway/src/features/ingress/error.rs`      | **Modify**        | Add `ExecutionFailed(#[from] ExecutorError)`     |
| `gateway/src/features/ingress/repository.rs` | **Modify**        | Add ExecutorService dependency, replace mock     |
| `gateway/src/main.rs`                        | **Modify**        | Initialize ExecutorService, pass to repositories |

### Key Principles

1. ✅ **Modify, don't rewrite** - Keep existing structure and method signatures
2. ✅ **Preserve error context** - Use `#[from]` to wrap ExecutorError in IngressError
3. ✅ **Use `?` operator** - Automatic error conversion instead of `.map_err()`
4. ✅ **Follow existing patterns** - Match style of other features (auth, health)

### Error Handling Best Practice

**❌ Bad** (loses error type information):

```rust
.map_err(|e| IngressError::ExecutionFailed(e.to_string()))?
```

**✅ Good** (preserves structured error):

```rust
// In IngressError enum:
#[error(transparent)]
ExecutionFailed(#[from] ExecutorError),

// In code:
let result = self.executor_service.execute(plan, payload).await?;
```

**Benefits**:

- Preserves HTTP status code mapping (e.g., 429 for RateLimitExceeded)
- Allows upper layers to inspect error type
- Enables proper error logging and monitoring

## Future Enhancements

### Short-term (Task 8.6 - Current)

1. ✅ Complete ExecutorService implementation (modify existing service.rs)
2. ✅ Integrate with ingress service (preserve error context)
3. ⏸️ Test with real OpenAI API (requires API key)
4. ⏸️ Add unit tests for retry and fallback logic

### Medium-term (Task 8.7)

### Medium-term

1. Load pricing from database (dynamic updates)
2. Support streaming responses (SSE/WebSocket)
3. Add more vendors (Cohere, Google, etc.)
4. Implement circuit breaker pattern
5. Add request/response caching

### Long-term

1. Multi-region vendor support
2. Automatic failover between vendors
3. Cost optimization strategies
4. A/B testing support
5. Custom model fine-tuning integration

## Testing Strategy

### Unit Tests

- Configuration loading and validation
- Model support checks
- Cost calculation accuracy
- Error handling and conversion

### Integration Tests

- Real API calls (with test API keys)
- Error scenarios (rate limits, auth failures)
- Timeout handling
- Retry logic

### Mock Testing

- Mock vendor implementations for testing
- Simulate API failures
- Test error propagation

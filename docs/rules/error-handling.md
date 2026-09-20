# Error Handling Rules

## 1. Core Philosophy: Model by Business Operation

Errors describe the **business operation** or **process step** that failed, not the low-level
technical cause. This gives actionable context when debugging.

- **CORRECT ✅**: `enum IngressError { ContextRetrievalFailed, ... }` — tells us **what** we were
  trying to do.
- **INCORRECT ❌**: `enum IngressError { MemoryServiceError, GrpcTimeout, ... }` — only tells us
  **which** dependency failed and loses the business context.

## 2. Two-Layer Structure

- **Feature-level errors**: each feature owns an enum in `features/<feature>/error.rs`. Variants map
  to the distinct operations the feature's service performs.
- **Top-level `AppError`**: `core/error.rs` aggregates every feature error with `#[from]` and
  `#[error(transparent)]`, and delegates `IntoResponse` to the feature error.

Cross-feature failures are wrapped, not flattened. Example:
`IngressError::ExecutionFailed(#[from] ExecutorError)` keeps the ingress operation name while
carrying the executor detail.

## 3. Implementation Workflow

### Step 1: Analyze the Feature's Business Logic

List the high-level operations the service performs (validate input, retrieve context, execute,
persist). Each becomes an error variant.

### Step 2: Create the Feature-Level Error File

```rust
use axum::{http::StatusCode, response::{IntoResponse, Response, Json}};
use serde_json::json;

/// Errors specific to "Your Feature".
/// Each variant corresponds to a failed business operation.
#[derive(Debug, thiserror::Error)]
pub enum YourFeatureError {
    #[error("Invalid input provided: {0}")]
    InvalidInput(String),

    #[error("Operation one failed.")]
    OperationOneFailed,

    #[error("Operation two failed.")]
    OperationTwoFailed,
}

impl IntoResponse for YourFeatureError {
    fn into_response(self) -> Response {
        let (code, message) = match self {
            Self::InvalidInput(msg) => (ErrorCode::InvalidRequest, msg),
            Self::OperationOneFailed | Self::OperationTwoFailed => (
                ErrorCode::InternalError,
                "An internal error occurred".to_string(),
            ),
        };
        error_response(code, message, None)
    }
}
```

### Step 3: Register in `AppError`

```rust
#[derive(Debug, thiserror::Error)]
pub enum AppError {
    #[error(transparent)]
    Auth(#[from] auth::error::AuthError),
    #[error(transparent)]
    Health(#[from] health::error::HealthError),
    #[error(transparent)]
    Ingress(#[from] ingress::error::IngressError),
    // Add new features here.
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        match self {
            AppError::Auth(e) => e.into_response(),
            AppError::Health(e) => e.into_response(),
            AppError::Ingress(e) => e.into_response(),
            AppError::Internal => error_response(
                ErrorCode::InternalError,
                "An internal error occurred",
                None,
            ),
        }
    }
}
```

## 4. Rules

- Protected API errors use `{"error":{"code","message","request_id"}}` via
  `core::http_error::error_response`. Literal codes live on `ErrorCode`.
- Never expose internal detail (stack traces, upstream bodies, SQL, secrets) in public responses.
- Upstream credential failures are `502` / `UPSTREAM_AUTHENTICATION`, never gateway `401`.
- Log once at the HTTP envelope boundary, not at every layer.
- Handlers return the feature error directly; conversion to `AppError` happens only where a single
  unified type is required. Cross-feature failures stay wrapped
  (`IngressError::ExecutionFailed(ExecutorError)`).

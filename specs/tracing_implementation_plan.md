# Tracing and Correlation ID Implementation Plan

> **⚠️ DEPRECATED**: This document has been superseded by the comprehensive Enhanced Observability
> specification.
>
> **Please refer to**: `specs/enhanced_observability/` for the current implementation.
>
> **Key differences from this plan**:
>
> - Uses **Dual-ID System**: System-generated `request_id` + optional client `client_correlation_id`
> - Implements **Hybrid Approach**: Explicit RequestContext parameter passing for business logic
>   (gRPC) + Automatic tracing span inheritance for logging
> - Includes **Fail-Safe Middleware**: Middleware never blocks requests, uses fallback strategies
> - Follows **Root Span Only Rule**: Add correlation IDs only in Handler layer (root span), child
>   spans inherit automatically
> - Comprehensive **Prometheus Metrics** and **Enhanced Health Checks**
>
> **This document is kept for historical reference only.**

---

## Original Plan (Deprecated)

This document outlines the phased approach to implementing comprehensive, end-to-end tracing in the
Gateway service using `tracing` and a `Correlation-ID` pattern.

---

## Phase 1: Foundation Setup - Correlation ID Middleware

This phase establishes the core mechanism for generating and propagating a unique ID for every
request. We will use a middleware approach to keep this logic decoupled from business handlers.

### 1.1. Middleware for Correlation ID

- **File**: `gateway/src/main.rs`
- **Logic**:
  1.  An Axum middleware will be created (`correlation_id_middleware`).
  2.  On every incoming request, it will inspect the `X-Correlation-ID` HTTP header.
  3.  **If the header exists**: The middleware will adopt its value. This supports end-to-end
      tracing initiated by a client or an upstream proxy.
  4.  **If the header does not exist**: The middleware will generate a new, unique UUID v4 value.
  5.  The resulting `CorrelationId` will be inserted into the request's extensions for downstream
      access.
  6.  A `tracing::span` will be created that covers the entire request lifecycle. This span will
      automatically include the `correlation_id`.

### 1.2. `CorrelationId` Type Definition

- **File**: `gateway/src/core/correlation.rs`
- **Details**:
  - A type-safe `CorrelationId(String)` newtype wrapper will be created to prevent misuse of plain
    strings.
  - It will implement `Display` for easy logging, `Default` and `new()` for generation, and
    `From<String>` for creation from a header value.

### 1.3. Router Integration

- **File**: `gateway/src/main.rs`
- **Details**: The `correlation_id_middleware` will be applied globally to the Axum `Router` using
  `.layer()`.

---

## Phase 2: Handler Layer Tracing

### 2.1. Request Lifecycle Logging

- **File**: `gateway/src/features/ingress/handler.rs`
- **Actions**:
  - The handler will extract the `CorrelationId` via the `Extension<CorrelationId>` extractor.
  - The `CorrelationId` will be passed down to the service layer in all subsequent calls.
  - Logging events will be added at key points in the handler's execution.
- **Log Events**:
  - `tracing::info!("Incoming request received");`
  - `tracing::debug!("Request validation passed");`
  - `tracing::info!("Request processing completed");`
  - `tracing::error!(error = %err, "Validation failed");`
- **Log Fields (Attached to Span)**:
  - `correlation_id`, `user_id`, `prompt_length`, `metadata_keys`

---

## Phase 3: Service Layer Tracing

### 3.1. Business Operation Logging

- **File**: `gateway/src/features/ingress/service.rs`
- **Actions**:
  - All public methods will be updated to accept the `CorrelationId`.
  - Spans or events will be used to log the duration and context of business operations.
- **Log Events**:
  - `tracing::info!("Starting business operation");`
  - `tracing::debug!("Executing step: context retrieval");`
  - `tracing::info!(duration_ms, "Business operation completed");`
  - `tracing::warn!("Business logic warning: e.g., fallback was used");`
- **Log Fields**:
  - `correlation_id`, `user_id`, `operation`, `duration_ms`

---

## Phase 4: Repository Layer Tracing

### 4.1. Downstream Service Call Logging

- **File**: `gateway/src/features/ingress/repository.rs`
- **Actions**:
  - All public methods will be updated to accept the `CorrelationId`.
  - Logging will be added around each interaction with a downstream dependency (even mocks).
- **Log Events**:
  - `tracing::debug!("Calling downstream gRPC service: {service_name}.{method}");`
  - `tracing::info!("Received successful response from {service_name}");`
  - `tracing::warn!(error = %err, "Downstream service call failed");`
  - `tracing::debug!("Using mock data for {service_name}");`
- **Log Fields**:
  - `correlation_id`, `service_name`, `operation`, `response_time_ms`

---

## Phase 5: Error Context Enhancement

### 5.1. Rich Error Logging

- **File**: `gateway/src/features/ingress/error.rs`, `gateway/src/core/error.rs`
- **Actions**:
  - The primary `AppError` enum and its `IntoResponse` implementation will be enhanced.
  - When an error is converted into an HTTP response, the associated `CorrelationId` will be
    explicitly logged alongside the error details.
  - This ensures that every error response sent to a client has a corresponding server log entry
    with a searchable ID.

---

## Phase 6: Integration & Testing

### 6.1. End-to-End Tracing Verification

- **Actions**:
  1.  **Manual Testing**: Execute `curl` requests, both with and without a provided
      `X-Correlation-ID` header.
  2.  **Log Verification**: Inspect the server's console output (configured for structured JSON) to
      confirm the `correlation_id` is present and propagates correctly through all log messages for
      a single request.
  3.  **Test Case**: Verify that the ID from the `curl` command is the one that appears in the logs.
  4.  **Performance**: A qualitative assessment of any performance impact will be made, though it is
      expected to be negligible.

---

## Implementation Details

### Correlation ID Structure

A newtype wrapper will be used for type safety: `pub struct CorrelationId(String);`

### Tracing Patterns

- **Spans**: A single top-level span will be used per request to provide context (`correlation_id`,
  `user_id`, etc.) to all child events. Additional spans may be used for long-running, complex
  operations within the request lifecycle.
- **Events**: `tracing::info!`, `debug!`, `warn!`, `error!` will be used to log specific,
  point-in-time events during the request's execution.

### Dependencies to Add

- **`gateway/Cargo.toml`**:

```toml
[dependencies]
# ...
uuid = { version = "1.8.0", features = ["v4", "serde"] }
```

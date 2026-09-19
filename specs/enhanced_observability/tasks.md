# Implementation Plan - Enhanced Observability (Phase 1)

## Overview

This implementation plan follows the **business-logic-first development principles** for adding
enhanced observability features to the Gateway Service.

**Related Requirement**: Requirement 5 - Monitoring and Observability

**Implementation Scope**: Phase 1 (MVP) - Production Readiness

**Estimated Time**: 2-3 days

## Task Breakdown

### Task 10.1: Add Dependencies and Core Infrastructure

**Objective**: Add required dependencies and create core observability infrastructure

**Subtasks**:

- [x] 10.1.1 Update Cargo.toml Dependencies
  - Add `tower-http = { version = "0.5", features = ["trace", "request-id"] }`
  - Add `uuid = { version = "1.0", features = ["v4", "serde"] }`
  - Add `axum-prometheus = "0.7"`
  - Add `chrono = { version = "0.4", features = ["serde"] }` (if not already present)
  - Verify all dependencies compile
  - _Requirement: Requirement 5 - Monitoring and Observability_

- [x] 10.1.2 Create Core Correlation Module
  - Create `gateway/src/core/correlation.rs`
  - Define `RequestContext` struct with `request_id`, `client_correlation_id`, `timestamp`
  - Implement `Debug`, `Clone` traits
  - Add helper methods for creating and extracting context
  - Export from `core/mod.rs`
  - _Requirement: Requirement 5 - Monitoring and Observability_

- [x] 10.1.3 Create Core Tracing Module
  - Create `gateway/src/core/tracing.rs`
  - Define `TracingConfig` struct with environment-based configuration
  - Define `LogFormat` enum (Json, Pretty)
  - Implement `TracingConfig::from_env()` with support for:
    - `RUST_LOG` - Log level filter
    - `LOG_FORMAT` - Log format (json, pretty)
    - `LOG_VERBOSE_DEBUG` - Enable expensive options (line numbers, thread IDs)
  - Implement `init_tracing()` function with configurable formatting
  - Add performance-optimized production defaults (disable line numbers, thread IDs)
  - Export from `core/mod.rs`
  - _Requirement: Requirement 5 - Monitoring and Observability_

- [x] 10.1.4 Create Core Metrics Module
  - Create `gateway/src/core/metrics.rs`
  - Define metrics registry structure (placeholder for Phase 2 custom metrics)
  - Add helper functions for metrics initialization
  - Export from `core/mod.rs`
  - _Requirement: Requirement 5 - Monitoring and Observability_

### Task 10.2: Implement Correlation ID Middleware

**Objective**: Implement middleware to generate and propagate correlation IDs

**Subtasks**:

- [x] 10.2.1 Create Correlation ID Middleware
  - Create `gateway/src/middleware/correlation_id.rs`
  - Implement `correlation_id_middleware` function with fail-safe design
  - Generate unique `request_id` using UUID v4 with timestamp fallback
  - Extract optional `X-Correlation-ID` header from request with validation:
    - Validate UTF-8 encoding (ignore invalid headers)
    - Validate length (max 256 chars, reject malicious long IDs)
    - Log warnings for invalid client IDs
  - Create `RequestContext` and inject into request extensions (always succeeds)
  - Add `X-Request-ID` to response headers (best effort, log errors)
  - Echo `X-Correlation-ID` to response headers (if provided by client)
  - Ensure middleware never returns error (fail-safe design)
  - _Requirement: Requirement 5 - Monitoring and Observability_

- [x] 10.2.2 Add Unit Tests for Correlation Middleware
  - Test request ID generation (always generates UUID)
  - Test UUID generation fallback (simulate failure, use timestamp)
  - Test client correlation ID extraction (when provided)
  - Test client correlation ID absence (when not provided)
  - Test invalid client correlation ID (non-UTF-8, too long)
  - Test response headers (X-Request-ID always present)
  - Test response headers (X-Correlation-ID echoed when provided)
  - Test RequestContext injection into extensions
  - Test middleware never fails (fail-safe design)
  - _Requirement: Requirement 5 - Monitoring and Observability_

- [x] 10.2.3 Export Middleware
  - Export `correlation_id_middleware` from `middleware/mod.rs`
  - Update module documentation
  - _Requirement: Requirement 5 - Monitoring and Observability_

### Task 10.3: Add Tracing Spans to All Layers ✅ COMPLETED

**Objective**: Instrument all feature layers with tracing spans

**Implementation Note**: Uses **Hybrid Approach** - Explicit RequestContext parameter passing for
business logic (gRPC calls) + Automatic tracing span inheritance for logging.

**Subtasks**:

- [x] 10.3.1 Add Tracing to Ingress Handler (Root Span) ✅
  - Add `#[tracing::instrument]` to `ingress_handler` function
  - Include `request_id`, `client_correlation_id`, `user_id`, `endpoint`, `prompt_length` in span
    fields (ROOT SPAN ONLY)
  - Skip sensitive fields (request body)
  - Add info logs for request start and completion
  - **Important**: This is the ONLY place to add request_id to span fields
  - Pass `&request_context` to service layer for business logic (gRPC calls)
  - _Requirement: Requirement 5 - Monitoring and Observability_

- [x] 10.3.2 Add Tracing to Ingress Service (Child Span) ✅
  - Add `#[tracing::instrument]` to `process_request` function
  - Add `request_context: &RequestContext` parameter for gRPC RequestMeta construction
  - Include `user_id`, `prompt_length` in span fields
  - **DO NOT** include `request_id` or `client_correlation_id` (inherited from parent)
  - Skip sensitive fields (prompt content)
  - Add debug logs for each processing step
  - Pass `request_context` to all repository methods
  - _Requirement: Requirement 5 - Monitoring and Observability_

- [x] 10.3.3 Add Tracing to Ingress Repository (Child Span) ✅
  - Add `#[tracing::instrument]` to `get_context`, `optimize_route`, `update_context` functions
  - Add `request_context: &RequestContext` parameter to all methods for future gRPC calls
  - Include `vendor_id`, `model_id` in span fields
  - **DO NOT** include `request_id` or `client_correlation_id` (inherited from root)
  - Add debug logs for external service calls
  - Add comments showing future gRPC RequestMeta construction
  - _Requirement: Requirement 5 - Monitoring and Observability_

- [x] 10.3.4 Add Tracing to Executor Service (Child Span) ✅
  - Add `#[tracing::instrument]` to `execute` function
  - Add `#[tracing::instrument]` to `execute_with_retry` function
  - Include `vendor_id`, `model_id`, `max_retries` in span fields
  - **DO NOT** include `request_id` (inherited from root span)
  - Add info logs for retry attempts and fallbacks
  - _Requirement: Requirement 5 - Monitoring and Observability_

- [x] 10.3.5 Add Tracing to Executor Repository (Child Span) ✅
  - Add `#[tracing::instrument]` to `call_llm` method
  - Include `vendor_id`, `model_id`, `max_tokens` in span fields
  - **DO NOT** include `request_id` (inherited from root span)
  - Add debug logs for API calls and completion
  - _Requirement: Requirement 5 - Monitoring and Observability_

- [x] 10.3.6 Review Auth Middleware Tracing (Child Span) ✅
  - Updated `#[tracing::instrument]` in `auth_middleware` function
  - Include `auth_method`, `user_id` in span fields (using dynamic recording)
  - **DO NOT** include `request_id` (inherited from correlation_id_middleware)
  - Skip sensitive fields (API keys)
  - Uses `tracing::Span::current().record()` for dynamic field values
  - _Requirement: Requirement 5 - Monitoring and Observability_

### Task 10.4: Integrate Prometheus Metrics ✅ COMPLETED

**Objective**: Add Prometheus metrics collection using axum-prometheus

**Implementation Notes**:

- Integrated Prometheus metrics using `axum-prometheus` crate
- Middleware order: Correlation ID → Metrics → Auth
- Metrics endpoint: `/metrics` (configurable via `METRICS_PATH`)
- Conditional enabling via `METRICS_ENABLED` environment variable
- All 66 tests passing
- Commit: `9da61de` - "feat(observability): Complete Task 10.4 - Prometheus Metrics Integration"

**Subtasks**:

- [x] 10.4.1 Initialize Prometheus Metrics
  - ✅ Created `MetricsConfig::from_env()` initialization in `main.rs`
  - ✅ Initialized `axum-prometheus` with default metrics using `create_metrics()`
  - ✅ Configured metric labels (method, endpoint, status)
  - ✅ Support conditional enabling/disabling via `METRICS_ENABLED`
  - _Requirement: Requirement 5 - Monitoring and Observability_

- [x] 10.4.2 Add Metrics Middleware to Router
  - ✅ Added `PrometheusMetricLayer` to middleware stack
  - ✅ Correct middleware order: Correlation ID (first) → Metrics (second) → Auth (third)
  - ✅ Ensures all requests are tracked (including auth failures)
  - _Requirement: Requirement 5 - Monitoring and Observability_

- [x] 10.4.3 Create Metrics Endpoint
  - ✅ Added `/metrics` route to router (configurable via `METRICS_PATH`)
  - ✅ Implemented handler using `prometheus_handle.render()`
  - ✅ Endpoint is accessible without authentication (public)
  - ✅ Added security warning comment for production deployment
  - _Requirement: Requirement 5 - Monitoring and Observability_

- [x] 10.4.4 Test Metrics Collection
  - ✅ Made test requests to various endpoints (/health, /api/v1/route, /)
  - ✅ Verified metrics are collected:
    - `gateway_http_requests_total` (counter)
    - `gateway_http_requests_pending` (gauge)
    - `gateway_http_requests_duration_seconds` (histogram)
  - ✅ Verified labels are correct (method, endpoint, status)
  - ✅ Verified `/metrics` endpoint returns Prometheus format
  - ✅ Tested correlation ID integration (X-Request-ID and X-Correlation-ID headers)
  - _Requirement: Requirement 5 - Monitoring and Observability_

**Metrics Collected**:

- `gateway_http_requests_total{method, status, endpoint}` - Total HTTP requests (counter)
- `gateway_http_requests_pending{method, endpoint}` - In-flight requests (gauge)
- `gateway_http_requests_duration_seconds{method, status, endpoint}` - Request duration (histogram)

**Additional Improvements**:

- Enhanced startup logs with emoji indicators (🚀, 📍, 🚨, 🔒)
- Formatted endpoint list for better readability
- Added metrics endpoint to startup log output

### Task 10.5: Enhance Health Check Endpoints ✅ COMPLETED

**Objective**: Enhance `/health` endpoint and create new `/ready` endpoint with real vendor health
checks

**Implementation Notes**:

- Implemented **Plan B: Real Health Checks** - actual vendor connectivity verification
- Added `LLMVendor::health_check()` trait method for vendor health verification
- Implemented OpenAI vendor health check (calls GET /models endpoint)
- Added caching mechanism with configurable TTL (default: 5 seconds)
- Parallel health check execution for all vendors (at-least-one-healthy strategy)
- Configurable timeout via `HEALTH_CHECK_TIMEOUT` environment variable
- Added version and uptime to /health response
- Created /ready endpoint with 200/503 status codes for Kubernetes readiness probes
- All 80 tests passing, no clippy warnings

**Subtasks**:

- [x] 10.5.1 Create Health Check Models ✅
  - Extended existing `HealthResponse` struct (added version, uptime_seconds)
  - Added `ReadinessResponse` struct (status, timestamp, dependencies)
  - Added `DependencyStatus` struct (status, vendor_count, latency_ms, error)
  - Added `HealthConfig` struct (timeout configuration)
  - All models in `features/health/service.rs` (minimal file changes)
  - _Requirement: Requirement 5 - Monitoring and Observability_

- [x] 10.5.2 Create Health Check Service (Real Health Check Approach) ✅
  - Extended existing `HealthService` struct
  - Added `executor_service` dependency injection via `with_executor()` constructor
  - Implemented `check_readiness()` method with real vendor health checks
  - Added caching mechanism with `RwLock<Option<HealthCheckCache>>`
  - Cache TTL configurable via `HEALTH_CHECK_CACHE_TTL_SECS` (default: 5 seconds)
  - Calls `ExecutorService::check_all_vendors_health()` for actual connectivity verification
  - _Requirement: Requirement 5 - Monitoring and Observability_

- [x] 10.5.3 Enhance /health Handler ✅
  - Updated `health_handler` to return enhanced `HealthResponse`
  - Added version from `CARGO_PKG_VERSION`
  - Added uptime calculation from service start_time
  - Existing tracing already in place
  - _Requirement: Requirement 5 - Monitoring and Observability_

- [x] 10.5.4 Create /ready Handler ✅
  - Added `ready_handler` to `features/health/handler.rs`
  - Calls `HealthService::check_readiness()`
  - Returns `ReadinessResponse` with dependency status
  - Returns 200 OK if ready, 503 Service Unavailable if not ready
  - Uses `State<Arc<HealthService>>` for dependency injection
  - _Requirement: Requirement 5 - Monitoring and Observability_

- [x] 10.5.5 Update Router with /ready Endpoint ✅
  - Added `/ready` route to public_routes in `main.rs`
  - Endpoint accessible without authentication
  - Passes `health_service` as state to ready_handler
  - _Requirement: Requirement 5 - Monitoring and Observability_

- [x] 10.5.6 Add Executor Health Check Method ✅
  - Added `ExecutorService::check_vendor_health(vendor_name, timeout_secs)` method
  - Added `ExecutorService::check_all_vendors_health(timeout_secs)` method
  - Parallel execution using `tokio::spawn` for concurrent health checks
  - At-least-one-healthy strategy: returns Ok if any vendor is healthy
  - Added `ExecutorRepository::list_vendor_names()` helper method
  - _Requirement: Requirement 5 - Monitoring and Observability_

- [x] 10.5.7 Add Vendor Health Check Methods ✅
  - Added `LLMVendor::health_check(timeout_secs)` trait method
  - Implemented `OpenAIVendor::health_check()` using GET /models endpoint
  - Configurable timeout parameter (respects `HEALTH_CHECK_TIMEOUT` config)
  - Detects authentication failures, network errors, timeouts
  - Returns appropriate `ExecutorError` variants for different failure modes
  - _Requirement: Requirement 5 - Monitoring and Observability_

- [x] 10.5.8 Add Unit Tests for Health Service ✅
  - Created `MockVendor` for testing without real HTTP calls
  - Added 17 health service tests (config, readiness, caching, HTTP handlers)
  - Added tests for healthy/unhealthy vendor scenarios
  - Added caching tests: cache hit, TTL expiry, failure not cached (with dynamic health status)
  - Added panic handling tests for executor service (2 tests)
  - Added HTTP handler tests using `Router::oneshot`
  - All 85 tests passing with fast execution (< 1.52s)
  - _Requirement: Requirement 5 - Monitoring and Observability_

**Configuration Options**:

The health check system supports the following environment variables:

```bash
# Health check timeout in seconds (default: 2)
HEALTH_CHECK_TIMEOUT=2

# Health check cache TTL in seconds (default: 5)
# Results are cached to avoid excessive API calls to vendor health endpoints
HEALTH_CHECK_CACHE_TTL_SECS=5
```

**Architecture Details**:

- **Caching Strategy**: Only successful health checks are cached (failures are not cached)
  - Uses `tokio::sync::RwLock` for async-safe locking (prevents blocking Tokio workers)
  - Fast recovery from transient failures
- **Parallel Execution**: All vendors are checked concurrently using `tokio::spawn`
  - JoinErrors (panics/cancellations) are logged and treated as failures
  - Prevents silent failures from vendor health check panics
- **At-Least-One-Healthy**: Service is ready if at least one vendor passes health check
- **Timeout Control**: Each vendor health check respects the configured timeout
- **Error Detection**: Detects authentication failures, network errors, timeouts, rate limits

**Health Check Flow**:

1. `/ready` endpoint receives request
2. `HealthService::check_readiness()` checks cache first
3. If cache miss or expired, calls `ExecutorService::check_all_vendors_health(timeout)`
4. Executor spawns parallel tasks for each vendor
5. Each vendor calls its health check endpoint (e.g., OpenAI GET /models)
6. Results are aggregated (at-least-one-healthy strategy)
7. Successful result is cached with TTL
8. Response returned with 200 OK (ready) or 503 Service Unavailable (not ready)

### Task 10.6: Update Main Application

**Objective**: Integrate all observability components in main.rs

**Subtasks**:

- [x] 10.6.1 Initialize Tracing in main.rs
  - Call `core::tracing::init_tracing()` at application startup
  - Add error handling for tracing initialization
  - Add startup log message with version and configuration
  - _Requirement: Requirement 5 - Monitoring and Observability_

- [x] 10.6.2 Update Middleware Stack
  - Add correlation ID middleware to stack
  - Add Prometheus metrics layer to stack
  - Add tower-http tracing layer to stack
  - Ensure correct middleware order:
    1. Correlation ID (outermost)
    2. Tracing
    3. Metrics
    4. Auth (innermost)
  - _Requirement: Requirement 5 - Monitoring and Observability_

- [x] 10.6.3 Update AppState for Health Service
  - Add `HealthService` to `AppState` struct
  - Initialize `HealthService` with `ExecutorService` dependency
  - Pass `AppState` to health handlers
  - _Requirement: Requirement 5 - Monitoring and Observability_

- [x] 10.6.4 Add Startup Logging
  - Log application version
  - Log enabled features (metrics, tracing)
  - Log server address and port
  - Log available endpoints
  - _Requirement: Requirement 5 - Monitoring and Observability_

### Task 10.7: Update Environment Configuration

**Objective**: Add observability configuration to environment files

**Subtasks**:

- [x] 10.7.1 Update .env.example
  - Add `RUST_LOG` configuration example (production vs development)
  - Add `LOG_FORMAT` configuration (json, pretty)
  - Add `LOG_VERBOSE_DEBUG` flag (enable expensive logging options)
  - Add `METRICS_ENABLED` flag
  - Add `HEALTH_CHECK_MODE` configuration (config, connectivity)
  - Add `HEALTH_CHECK_CACHE_TTL` configuration (default: 30 seconds)
  - Add `HEALTH_CHECK_TIMEOUT` configuration (default: 2 seconds)
  - Add documentation comments for each variable
  - Add performance impact notes for each option
  - Provide separate examples for production, development, and staging
  - _Requirement: Requirement 5 - Monitoring and Observability_

- [x] 10.7.2 Update Configuration Documentation
  - Document observability environment variables
  - Add examples for different log levels
  - Add examples for production vs development configuration
  - _Requirement: Requirement 5 - Monitoring and Observability_

### Task 10.8: Integration Testing

**Objective**: Create comprehensive integration tests for observability features

**Subtasks**:

- [x] 10.8.1 Create Observability Integration Tests
  - Create `gateway/tests/observability_integration_test.rs`
  - Test correlation ID generation and propagation
  - Test client correlation ID preservation
  - Test response headers (X-Request-ID, X-Correlation-ID)
  - Test metrics endpoint accessibility
  - Test health endpoint response format
  - Test ready endpoint with healthy dependencies
  - Test ready endpoint with unhealthy dependencies
  - _Requirement: Requirement 5 - Monitoring and Observability_

- [x] 10.8.2 Create Manual Testing Guide
  - Create `gateway/tests/OBSERVABILITY_TESTING.md`
  - Document curl commands for testing correlation IDs
  - Document how to verify metrics collection
  - Document how to test health check endpoints
  - Document how to verify log format
  - _Requirement: Requirement 5 - Monitoring and Observability_

### Task 10.9: Documentation

**Objective**: Document observability features for users and operators

**Subtasks**:

- [x] 10.9.1 Update README
  - Add observability features section
  - Document correlation ID usage
  - Document metrics endpoint
  - Document health check endpoints
  - _Requirement: Requirement 5 - Monitoring and Observability_

- [x] 10.9.2 Create Observability Guide
  - Create `docs/OBSERVABILITY.md`
  - Document correlation ID strategy (dual-ID system)
  - Document log format and structure
  - Document available metrics
  - Document health check endpoints
  - Add examples for common monitoring scenarios
  - _Requirement: Requirement 5 - Monitoring and Observability_

- [x] 10.9.3 Create Prometheus Integration Guide
  - Create `docs/PROMETHEUS.md`
  - Document how to scrape metrics
  - Provide example Prometheus configuration
  - Document available metrics and labels
  - Provide example alert rules
  - _Requirement: Requirement 5 - Monitoring and Observability_

### Task 10.10: Verification and Cleanup

**Objective**: Verify all features work correctly and clean up

**Subtasks**:

- [x] 10.10.1 Run All Tests
  - Run `cargo test` and verify all tests pass
  - Run integration tests and verify observability features
  - Run `cargo clippy` and fix any warnings
  - Run `cargo fmt` and format all code
  - _Requirement: Requirement 5 - Monitoring and Observability_

- [x] 10.10.2 Manual End-to-End Testing
  - Start the server and verify startup logs
  - Make requests with and without correlation IDs
  - Verify logs contain request_id and client_correlation_id
  - Verify metrics are collected and exposed
  - Verify health check endpoints return correct responses
  - _Requirement: Requirement 5 - Monitoring and Observability_

- [x] 10.10.3 Update Task Status
  - Mark all completed tasks in `specs/gateway_service/tasks.md`
  - Update Task 10 status to completed
  - Document any deferred items for Phase 2
  - _Requirement: Requirement 5 - Monitoring and Observability_

## Task Dependencies

```
10.1 (Dependencies & Core)
  ↓
10.2 (Correlation Middleware) ──┐
  ↓                              │
10.3 (Tracing Spans) ────────────┤
  ↓                              │
10.4 (Prometheus Metrics) ───────┤
  ↓                              │
10.5 (Health Checks) ────────────┤
  ↓                              │
10.6 (Main Integration) ←────────┘
  ↓
10.7 (Configuration)
  ↓
10.8 (Integration Tests)
  ↓
10.9 (Documentation)
  ↓
10.10 (Verification)
```

## Success Criteria

### Functional Requirements

- ✅ Every request generates a unique request_id
- ✅ Client-provided correlation IDs are preserved and echoed
- ✅ All logs are structured (JSON format)
- ✅ All logs include request_id and client_correlation_id (if present)
- ✅ Prometheus metrics are collected for all HTTP requests
- ✅ `/metrics` endpoint exposes metrics in Prometheus format
- ✅ `/health` endpoint returns enhanced health information
- ✅ `/ready` endpoint checks Executor dependency status

### Non-Functional Requirements

- ✅ Correlation ID generation adds < 1ms latency
- ✅ Metrics collection adds < 5ms latency
- ✅ All tests pass (unit + integration)
- ✅ Code passes clippy and fmt checks
- ✅ Documentation is complete and accurate

## Completion Notes (2026-03-12)

- Verified quality gates completed:
  - `cargo test -p gateway ingress`
  - `cargo test -p gateway executor`
  - `cargo test`
  - `cargo build`
  - `cargo fmt -- --check`
  - `cargo clippy --all-targets --all-features -- -D warnings`
  - `./scripts/check-security.sh`
- Manual E2E smoke test completed with live server run:
  - `/health` returned `200 OK`
  - `/ready` returned `503 Service Unavailable` with unhealthy dependency details under dummy vendor
    credentials
  - `/metrics` returned Prometheus metrics payload
- `/api/v1/route` returned `500 Internal Server Error` with `execution_failed` under dummy upstream
  config and preserved `X-Correlation-ID`
- Task status reconciled:
  - Gateway Task 10 marked complete in `specs/gateway_service/tasks.md`
- Documentation and integration artifacts for 10.8/10.9 are implemented in this milestone

## Incremental Development Approach

### Iteration 1: Core Infrastructure (Day 1)

- Complete Task 10.1 (Dependencies & Core)
- Complete Task 10.2 (Correlation Middleware)
- Verify correlation IDs work end-to-end

### Iteration 2: Tracing & Metrics (Day 2)

- Complete Task 10.3 (Tracing Spans)
- Complete Task 10.4 (Prometheus Metrics)
- Verify logs and metrics are collected

### Iteration 3: Health Checks & Integration (Day 3)

- Complete Task 10.5 (Health Checks)
- Complete Task 10.6 (Main Integration)
- Complete Task 10.7 (Configuration)
- Complete Task 10.8 (Integration Tests)

### Iteration 4: Documentation & Verification (Day 3)

- Complete Task 10.9 (Documentation)
- Complete Task 10.10 (Verification)
- Final testing and cleanup

## Phase 2 Enhancements (Future)

- ⏸️ Distributed tracing with OpenTelemetry
- ⏸️ Custom business metrics (LLM cost, tokens, cache hits)
- ⏸️ Grafana dashboard templates
- ⏸️ Alert rules for Prometheus
- ⏸️ Log aggregation integration (Loki, Elasticsearch)
- ⏸️ Health check for gRPC services (Router, Memory)
- ⏸️ Performance profiling and optimization

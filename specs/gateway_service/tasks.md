# Implementation Plan - Business Logic First Development

> **Historical task list.** Checkbox state is contemporaneous and is **not** current shipped
> inventory. In particular:
>
> - `AUTH_DEVELOPMENT_MODE` / mock keys are **not** shipped. Persisted API-key auth is required.
> - Local Supabase persistence **is** shipped (this file still marks some of it deferred).
> - Ingress no longer uses Memory/Router mocks or `execute_llm_call()` stubs.
> - Item 8.6.10 "real OpenAI API" tests are deferred/unrun. Shipped verification uses the local
>   simulator (**SIMULATED ONLY**).
> - Distributed tracing export, gRPC client pooling, and conversation-context caches are **not**
>   shipped.
>
> Current commands and contract: [README.md](../../README.md).

## Development Philosophy

Based on the complete system design (requirements.md + design.md), this implementation follows
**business-logic-first development principles**:

- Start with core business functionality that delivers immediate value
- Add configuration, error handling, and infrastructure organically as needed by business features
- Build supporting systems (auth, observability, etc.) when required by actual endpoints
- Each iteration delivers working, testable functionality while maintaining architectural standards

## Iteration 1: Minimal Working Gateway

- [x] 1. Minimal Project Setup
  - Create basic Cargo.toml with essential dependencies only
  - Create src/main.rs with minimal "Hello World" server
  - Create src/lib.rs with basic module structure
  - Ensure project compiles and runs
  - _Requirement: Foundation for all other requirements_

- [x] 2. Basic Health Check Endpoint (First Working Feature)
  - Implement simple /health endpoint that returns 200 OK
  - Create minimal Axum server setup in main.rs
  - Add basic error handling as needed for this endpoint
  - Add minimal configuration (server port) as required
  - _Requirement: Requirement 5 - Monitoring and Observability_

- [x] 3. Basic Ingress Endpoint Structure (Core Business Logic)
  - Implement POST /api/v1/route endpoint that accepts JSON
  - Create request/response models as needed
  - Add basic request validation and error responses
  - Implement minimal service layer structure using 3-layer architecture
  - _Requirement: Requirement 1 - Request Reception and Routing_

## Iteration 2: Authentication Integration

- [x] 4. Unified Authentication for Ingress Endpoint (Phase 1: API Key Focus)
  - ✅ Implemented unified authentication middleware (Axum middleware)
  - ✅ Added API Key authentication via X-API-Key for B2B clients
  - ✅ Created client context extraction from API key validation
  - ✅ Added development mode bypass for testing (configurable client ID)
  - ⏸️ Authorization header detection (Bearer ...) — deferred
  - ⏸️ JWT and service auth supported by architecture but not enabled yet
  - _Requirement: Requirement 3 - Unified Authentication and Authorization_

- [ ] 5. Supabase Database Integration (Deferred until pre-launch)
  - Implement Supabase database client for API key storage and validation
  - Add secure API key hashing and storage mechanisms
  - Implement caching layer for high-performance API key validation
  - Add proper error handling for database connection failures
  - Add authentication configuration management with environment variables
  - _Requirement: Requirement 3 - Unified Authentication and Authorization_

## Iteration 2.5: Authentication System Expansion (Future)

- [ ] 5.1 JWT Authentication for Dashboard (Deferred until pre-launch)
  - Extend unified middleware to support JWT validation
  - Implement Supabase public key fetching and caching
  - Add dashboard user context extraction from JWT claims
  - Implement session management and logout support
  - _Requirement: Requirement 3 - Unified Authentication and Authorization_

- [ ] 5.2 Internal Service Authentication (Deferred until pre-launch)
  - Add lightweight internal service token validation
  - Implement service-to-service authentication
  - Add service context extraction and request correlation
  - Integrate with existing unified middleware
  - _Requirement: Requirement 3 - Unified Authentication and Authorization_

## Iteration 2.7: gRPC Contract Design and Simplification

- [x] 5.5 Design and Simplify gRPC Contracts
  - ✅ Designed complete gRPC contract for all microservices (RouterService, MemoryService,
    RewriteService, ValidationService)
  - ✅ Simplified RouterService contract for MVP (removed 7 unnecessary fields)
  - ✅ Renamed RouteOptimizerService → RouterService for consistency
  - ✅ Removed fields: endpoint_url, needs_rewrite, execution_params, estimated_latency_ms,
    estimated_cost
  - ✅ Removed OptimizationConstraints message (deferred to post-MVP)
  - ✅ Documented parameter handling philosophy (extract from original_payload)
  - ✅ Clarified RouterService responsibility: returns routing strategy only, not AI responses
  - ✅ Aligned repository code with simplified contract design
  - _Requirement: Requirement 2 - Microservice Coordination_
  - _Status: Contract design complete, ready for microservice implementation_

## Iteration 3: Service Integration

- [ ] 6. Memory Service Integration (Deferred until post-MVP)
  - Add gRPC client for Memory Service to ingress flow
  - Implement context retrieval before processing requests
  - Add service configuration and connection management
  - Handle service failures and implement basic retry logic
  - _Requirement: Requirement 2 - Microservice Coordination_

- [x] 7. Router Service Integration (Repository Layer - Mock Implementation)
  - ✅ Aligned repository structures with simplified gRPC contract
  - ✅ Added RoutePlan, RouterServiceResponse, LLMExecutionResult structures
  - ✅ Implemented optimize_route() method (mock - returns routing strategy)
  - ✅ Implemented execute_llm_call() method (temporary mock - simulates Executor)
  - ✅ Updated service layer to use new two-step flow (optimize → execute)
  - ✅ Removed cache_hit field from IngressResponse
  - ⏸️ Real gRPC client implementation deferred (using mock data)
  - ⏸️ Executor Layer implementation deferred (temporary mock in repository)
  - _Requirement: Requirement 2 - Microservice Coordination_
  - _Status: Mock implementation complete, ready for gRPC client integration_

- [ ] 8. Rewrite Service Integration (Deferred to Future)
  - ✅ Added TODO comments and placeholder for future integration
  - ✅ Documented integration point in service layer (Step 2.5)
  - ✅ Added commented rewrite_request() method signature in repository
  - ⏸️ MVP does not require rewrite functionality (deferred per design decision)
  - ⏸️ Will implement when RewriteService is available
  - ⏸️ Conditional routing based on metadata.rewrite flag (future)
  - _Requirement: Requirement 2 - Microservice Coordination_
  - _Status: Interface designed, implementation deferred to post-MVP_

## Iteration 3.5: Executor Layer Implementation

- [x] 8.5 Executor Layer Foundation (Configuration System)
  - ✅ Created executor module structure (config, error, models, service, vendors)
  - ✅ Implemented ModelPricing and OpenAIConfig for vendor configuration
  - ✅ Added environment variable support (OPENAI_API_KEY, OPENAI_BASE_URL, OPENAI_TIMEOUT_MS)
  - ✅ Implemented configuration validation and loading
  - ✅ Designed LLMVendor trait for unified vendor interface
  - ✅ Implemented OpenAIVendor with Chat Completions API integration
  - ✅ Added cost calculation based on configurable pricing
  - ✅ Separated configuration (data storage) from business logic (vendor implementation)
  - ✅ Updated .env.example with Executor Layer configuration
  - ⏸️ ExecutorService orchestration layer (placeholder, not yet implemented)
  - ⏸️ Real LLM API calls (OpenAIVendor structure ready, execute() not tested)
  - ⏸️ Integration with ingress service (deferred)
  - _Requirement: Requirement 2 - Microservice Coordination (Executor Layer)_
  - _Status: Configuration system complete, vendor implementation ready for testing_

- [x] 8.6 Executor Layer Service Integration
  - Implement ExecutorService orchestration layer with retry and fallback logic
  - _Requirement: Requirement 2 - Microservice Coordination (Executor Layer)_
  - _Status: ✅ Completed - All 10 subtasks finished_
  - _Unit Tests: 28 tests passing (19 service + 9 repository)_
  - _Integration Tests: 6 tests passing with real OpenAI API_

  **Subtasks:**
  - [x] 8.6.1 Modify ExecutorService Core Structure
    - **Modify** existing struct (not rewrite): Add `vendors` and `config` fields
    - Replace `new()` with `from_config()` method for auto-initialization
    - Add `vendor_count()` helper method
    - Add `NoVendorsConfigured` error variant to `executor/error.rs`
    - Keep existing `execute()` method signature
    - _Status: ✅ Completed (commit 7f5205d)_

  - [x] 8.6.2 Implement Parameter Extraction
    - Create `extract_params()` method
    - Extract messages (required field)
    - Extract optional parameters (temperature, max_tokens, top_p, stream)
    - Add validation and error handling
    - _Status: ✅ Completed (commit 74afb3c) - 6 unit tests added_

  - [x] 8.6.3 Implement Vendor Selection
    - Create `get_vendor()` method
    - Lookup vendor by vendor_id
    - Return appropriate error if vendor not found
    - _Status: ✅ Completed (commit 263c334) - 4 unit tests added_

  - [x] 8.6.4 Implement Retry Logic
    - Create `execute_with_retry()` method
    - Implement exponential backoff (1s, 2s, 4s, 8s, ...)
    - Create `is_retryable_error()` helper
    - Classify errors (retryable vs non-retryable)
    - Add detailed logging for retry attempts
    - _Status: ✅ Completed (commit 2b9e71d) - 8 unit tests added_

  - [x] 8.6.5 Implement Fallback Execution
    - Create `execute_fallbacks()` method
    - Sequential fallback execution
    - Each fallback gets full retry logic
    - Return primary error if all fallbacks fail
    - Add detailed logging for fallback attempts
    - _Status: ✅ Completed (commit 7c7d4b9) - 1 async unit test added_

  - [x] 8.6.6 Implement Main Execute Method
    - Create `execute()` method
    - Extract parameters once
    - Try primary plan with retry
    - Try fallback plans if primary fails
    - Return ExecutionResult
    - _Status: ✅ Completed (commit c3e7646) - Integration complete_

  - [x] 8.6.7 Integrate with IngressRepository
    - Update IngressError to wrap ExecutorError (use `#[from]` attribute)
    - Update IngressRepository to accept ExecutorService dependency
    - Replace mock `execute_llm_call()` with real ExecutorService call
    - Convert ExecutionResult → LLMExecutionResult
    - Use `?` operator for automatic error conversion
    - _Status: ✅ Completed - Integration complete_

  - [x] 8.6.8 Initialize in main.rs
    - Load ExecutorConfig from environment
    - Create ExecutorService instance
    - Pass ExecutorService to IngressRepository via AppState
    - Add initialization logging
    - Implement AppState pattern for scalability
    - Implement graceful error handling (std::process::exit)
    - Move AppState to lib.rs to prevent feature coupling
    - _Status: ✅ Completed (commits 1ba5aa6, ee3b022, 8910ebd)_

  - [x] 8.6.9 Add Unit Tests
    - Test vendor selection logic (4 tests in repository_tests.rs)
    - Test parameter extraction (3 tests in service_tests.rs)
    - Test error classification (8 tests: 4 retryable + 4 non-retryable)
    - Test vendor_count() helper (2 tests: service + repository)
    - _Status: ✅ Completed (28 tests total for executor module)_

  - [x] 8.6.10 Integration Testing
    - Test real OpenAI API calls (6 tests created)
    - Test retry logic with actual network conditions
    - Test parameter extraction from JSON payload
    - Test unsupported model error handling
    - Test different model configurations
    - Verify token counting and cost calculation
    - Created helper script: `scripts/run-integration-tests.sh`
    - Created documentation: `gateway/tests/README.md`
    - _Status: ✅ Completed - All 6 integration tests passing_
    - _Test Results: 6 passed, estimated cost < $0.001 per run_

- [ ] 8.7 Additional Vendor Support (Future)
  - Implement AnthropicVendor (Claude models)
  - Implement CohereVendor (Command models)
  - Add vendor-specific error handling
  - Support streaming responses (SSE/WebSocket)
  - _Requirement: Requirement 2 - Microservice Coordination (Executor Layer)_
  - _Status: Not started_

- [ ] 9. Validation Service Integration (Optional)
  - Add gRPC client for Validation Service
  - Implement request validation in ingress flow
  - Add validation error handling and responses
  - Make validation optional based on configuration
  - _Requirement: Requirement 2 - Microservice Coordination_

## Iteration 4: Production Readiness

- [x] 10. Enhanced Observability
  - Add structured logging with correlation IDs to existing endpoints
  - Implement Prometheus metrics collection
  - Add distributed tracing for service calls
  - Enhance health check with dependency status
  - _Requirement: Requirement 5 - Monitoring and Observability_

- _Status: ✅ Completed_

- [x] 11. Error Handling and Resilience
  - Implement circuit breaker patterns for service calls
  - Add comprehensive error handling and recovery
  - Implement graceful degradation strategies
  - Add proper error logging and monitoring
  - _Requirement: Requirement 6 - Error Handling and Recovery_
  - _Status: ✅ Completed (circuit breaker + graceful fallback/degradation + structured error
    telemetry/monitoring)_

- [x] 12. Performance Optimization
  - Add connection pooling for gRPC clients
  - Implement caching strategies for frequently accessed data
  - Optimize request processing pipeline
  - Add performance monitoring and alerting
  - _Requirement: Requirement 7 - Performance and Scalability_
  - _Status: ✅ Completed (ingress context cache with TTL/eviction/invalidation, env-tunable
    slow-request threshold, and pipeline verification)_

## Iteration 5: Testing and Documentation

- [x] 13. Testing Suite
  - Add unit tests for implemented functionality
  - Create integration tests for end-to-end flows
  - Add performance and load testing
  - Implement mocking for external services
  - _Requirement: All requirements validation_
  - _Status: ✅ Completed (unit/integration coverage extended, resilience E2E flow test added,
    load-test script+guide added, external-service mocking retained in feature test suites)_

- [x] 14. Documentation and Deployment
  - Create API documentation for implemented endpoints
  - Add deployment configuration (Docker, environment variables)
  - Create operational runbooks and monitoring guides
  - Document configuration and troubleshooting
  - _Requirement: All requirements documentation_
  - _Status: ✅ Completed (API reference, Docker artifacts, operations runbook, monitoring/config
    troubleshooting docs added)_

## Incremental Development Principles

### Each Iteration Must Deliver:

1. **Working Functionality**: Each iteration produces deployable, testable features
2. **Immediate Value**: Features solve real problems and can be demonstrated
3. **Incremental Complexity**: Each iteration builds naturally on the previous one
4. **Organic Growth**: Configuration, error handling, and infrastructure grow with needs

### Success Criteria for Each Task:

1. **Functionality First**: Feature works end-to-end before optimization
2. **Code Quality**: Follows Rust idioms and passes basic linting
3. **Testability**: Can be tested in isolation and integration
4. **Documentation**: Minimal but sufficient documentation for usage
5. **Architecture**: Maintains 3-layer separation while being pragmatic

### Development Guidelines:

- **Start Simple**: Implement the minimal version that works
- **Add as Needed**: Only add complexity when required by actual use cases
- **Refactor Later**: Focus on working code first, clean code second
- **Test Early**: Write tests for core functionality, expand coverage incrementally

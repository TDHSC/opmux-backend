# Requirements Document

> **Historical requirements.** This file is original product intent, not shipped inventory.
> Implemented MVP: API-only gateway, persisted tenant-scoped API keys, configured stateless routing,
> OpenAI Chat Completions with retry/fallback/circuits, real local Supabase, operator CLI. See
> [README.md](../../README.md).
>
> **Not shipped:** Rewrite/Router/Memory/Validation microservices, gRPC coordination, JWT/dashboard
> auth, service tokens, `AUTH_DEVELOPMENT_MODE` bypass, hot reload, configuration-dump endpoints.
> OpenAI verification is **SIMULATED ONLY**. Live-provider, hosted deployment, and hosted
> Supabase/TLS checks are deferred and unrun.

## Introduction

Design and develop the Gateway Microservice for AI API Router, serving as the unified entry point
for the system. The service is responsible for receiving client requests (prompt + metadata),
coordinating other microservices, aggregating responses, and handling authentication via a unified
authentication system supporting multiple methods (API Keys for B2B clients, JWT tokens for
dashboard users, and internal service tokens). Supabase serves as the database layer for
authentication data and user management, while the Gateway focuses on request routing, service
coordination, and multi-method authorization. The service needs to support core MVP functionality
while reserving architectural space for future expansion.

## Requirements

### Requirement 1 - Request Reception and Routing

**User Story:** As an API user, I want to send requests containing prompt and metadata through a
unified Gateway endpoint, so that the system can correctly route to appropriate backend services.

#### Acceptance Criteria

1. When receiving a POST request to `/api/v1/route`, the Gateway Service shall accept JSON payload
   containing prompt and metadata fields. The service SHALL normalize prompt into a messages array
   before execution.
2. When metadata contains rewrite flag as true, the Gateway Service shall route request to Rewrite
   Service before Router Service.
3. When metadata contains rewrite flag as false, the Gateway Service shall route request directly to
   Router Service.
4. When request processing is complete, the Gateway Service shall return aggregated response in JSON
   format.
5. While processing requests, the Gateway Service shall maintain request tracing throughout the
   service chain.

### Requirement 2 - Microservice Coordination

**User Story:** As a system architect, I want the Gateway to coordinate multiple backend
microservices, ensuring requests are processed in the correct order and responses are aggregated.

#### Acceptance Criteria

1. When coordinating services, the Gateway Service shall communicate with Router Service, Memory
   Service, Rewrite Service, and Validation Service via gRPC.
2. When a service is unavailable, the Gateway Service shall implement retry logic with exponential
   backoff.
3. When service coordination fails, the Gateway Service shall return appropriate error responses
   with detailed error codes.
4. While coordinating services, the Gateway Service shall maintain service health checks and circuit
   breaker patterns.
5. When processing requests, the Gateway Service shall aggregate responses from multiple services
   into a unified response format.

### Requirement 3 - Unified Authentication and Authorization

**User Story:** As a system administrator, I want the Gateway to support multiple authentication
methods for different client types (B2B API clients, dashboard users, internal services) with
appropriate authorization controls for each.

#### Acceptance Criteria

1. When receiving requests with API Key header (X-API-Key), the Gateway Service shall validate the
   key against Supabase database and extract client context (client_id, organization_id,
   project_id).
2. When receiving requests with JWT token from Supabase (Authorization header), the Gateway Service
   shall validate token signature, expiration, and issuer for dashboard users.
3. When receiving requests with internal service tokens, the Gateway Service shall validate using
   lightweight internal authentication for microservice communication.
4. When authentication method detection fails, the Gateway Service shall return 400 Bad Request with
   clear error message.
5. When API key validation fails, the Gateway Service shall return 401 Unauthorized with minimal
   security information.
6. When JWT validation fails, the Gateway Service shall return 401 Unauthorized with appropriate
   error message.
7. While processing authenticated requests, the Gateway Service shall inject appropriate context
   (API client context, dashboard user context, or service context) for downstream handlers.
8. When authorization fails, the Gateway Service shall return 403 Forbidden with context-appropriate
   error message.
9. When validating Supabase JWT, the Gateway Service shall verify against Supabase public keys and
   handle key rotation gracefully.
10. When in development mode, the Gateway Service shall support authentication bypass with mock
    contexts for testing purposes.

### Requirement 4 - Configuration Management

**User Story:** As a DevOps engineer, I want the Gateway to support flexible configuration
management, including hot updates and environment variable injection.

#### Acceptance Criteria

1. When starting up, the Gateway Service shall load configuration from environment variables and
   config files.
2. When configuration changes are detected, the Gateway Service shall support hot reload without
   service restart.
3. When deployed in different environments, the Gateway Service shall support service address
   configuration via environment variables.
4. While running, the Gateway Service shall expose configuration validation endpoints for health
   checks.
5. When configuration is invalid, the Gateway Service shall log detailed error messages and fail
   gracefully.

### Requirement 5 - Monitoring and Observability

**User Story:** As an operations engineer, I want the Gateway to provide comprehensive monitoring
and logging capabilities for problem diagnosis and performance analysis.

#### Acceptance Criteria

1. When processing requests, the Gateway Service shall emit structured logs using tracing framework.
2. When handling requests, the Gateway Service shall collect metrics (latency, throughput, error
   rates) for Prometheus.
3. When errors occur, the Gateway Service shall log detailed error information with correlation IDs.
4. While running, the Gateway Service shall expose health check endpoints at `/health` and `/ready`.
5. When request tracing is enabled, the Gateway Service shall propagate trace context to downstream
   services.

### Requirement 6 - Error Handling and Recovery

**User Story:** As an API user, I want to receive clear error messages when services encounter
problems, and the system should handle exceptional situations gracefully.

#### Acceptance Criteria

1. When downstream services fail, the Gateway Service shall implement circuit breaker pattern to
   prevent cascade failures.
2. When temporary failures occur, the Gateway Service shall retry requests with exponential backoff
   and jitter.
3. When permanent failures occur, the Gateway Service shall return structured error responses with
   appropriate HTTP status codes.
4. While handling errors, the Gateway Service shall log error details without exposing sensitive
   information to clients.
5. When service degradation occurs, the Gateway Service shall implement graceful degradation
   strategies.

### Requirement 7 - Performance and Scalability

**User Story:** As a system architect, I want the Gateway to handle high concurrent requests and
support horizontal scaling.

#### Acceptance Criteria

1. When handling concurrent requests, the Gateway Service shall support async/await patterns for
   non-blocking I/O.
2. When load increases, the Gateway Service shall maintain response times under acceptable
   thresholds.
3. When deployed in multiple instances, the Gateway Service shall be stateless to support horizontal
   scaling.
4. While processing requests, the Gateway Service shall implement connection pooling for gRPC
   clients.
5. When memory usage exceeds thresholds, the Gateway Service shall implement backpressure
   mechanisms.

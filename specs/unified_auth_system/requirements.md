# Requirements Document

> **Historical requirements.** Shipped auth is persisted tenant-scoped API keys (management vs
> inference) with SHA-256 digests, no plaintext storage, no authentication cache, and no
> `AUTH_DEVELOPMENT_MODE` bypass. Former public mock keys return 401.
>
> **Not shipped:** JWT/dashboard login, organizations/projects, key expiration, service tokens, mock
> development bypass, and latency SLAs. See [README.md](../../README.md).

## Introduction

Design and implement a unified authentication system for the Gateway Microservice that supports
multiple authentication methods for different client types. The system shall provide secure access
control for API clients, dashboard users, and internal microservices while maintaining high
performance and following engineering best practices. The initial focus is on API key authentication
for B2B customers, with future expansion to JWT-based dashboard authentication and internal service
authentication.

## Requirements

### Requirement 1 - API Key Authentication (Phase 1 - Primary)

**User Story:** As a B2B API client, I need to authenticate my requests using a long-lived API key
so that I can securely access AI API Router services for my applications.

#### Acceptance Criteria

1. When a client sends a request with a valid API key in the X-API-Key header, the system shall
   validate the key against the database and allow access.
2. When an API key is invalid or expired, the system shall return a 401 Unauthorized response.
3. When no API key is provided for protected endpoints, the system shall return a 401 Unauthorized
   response.
4. When API key validation succeeds, the system shall extract client context (client_id,
   organization_id, project_id) and make it available to downstream handlers.
5. When validating API keys, the system shall complete validation within 5ms for cached keys and
   20ms for database lookups.

### Requirement 2 - Unified Authentication Middleware

**User Story:** As a system architect, I need a unified authentication middleware that can handle
different authentication methods so that the system can support multiple client types efficiently.

#### Acceptance Criteria

1. When receiving a request, the system shall detect the authentication method based on request
   headers (X-API-Key, Authorization, etc.).
2. When the authentication method is detected, the system shall route to the appropriate validator
   (API key, JWT, or service token).
3. When authentication succeeds, the system shall inject the appropriate context type into the
   request.
4. When authentication fails, the system shall return consistent error responses regardless of the
   authentication method used.

### Requirement 3 - Endpoint Protection Strategy

**User Story:** As a system administrator, I need different authentication requirements for
different endpoint types so that the system follows security best practices.

#### Acceptance Criteria

1. When accessing health check endpoints (/health), the system shall allow unauthenticated access.
2. When accessing business API endpoints (/api/v1/\*), the system shall require valid authentication
   (API key in Phase 1).
3. When accessing root endpoint (/), the system shall allow unauthenticated access for basic
   connectivity testing.
4. When new business endpoints are added, they shall be protected by default unless explicitly
   configured otherwise.

### Requirement 4 - Development Environment Support

**User Story:** As a developer, I need the ability to bypass authentication during development so
that I can test functionality without setting up complete authentication flows.

#### Acceptance Criteria

1. When the system is configured with development mode enabled, the system shall accept requests
   without authentication headers.
2. When in development mode, the system shall inject a mock client context for testing purposes.
3. When development mode is disabled, the system shall enforce full authentication validation.
4. When switching between modes, the system shall not require code changes, only configuration
   updates.

### Requirement 5 - Database Integration with Supabase

**User Story:** As a system integrator, I need the authentication system to use Supabase as the
database backend so that API key management is centralized and scalable.

#### Acceptance Criteria

1. When validating API keys, the system shall query the Supabase database for key information.
2. When API key information is retrieved, the system shall cache it with appropriate TTL to optimize
   performance.
3. When Supabase is unreachable, the system shall use cached key information if available or return
   service unavailable errors.
4. When storing API keys, the system shall use secure hashing (SHA-256) and never store plain text
   keys.

### Requirement 6 - High Performance Authentication

**User Story:** As a system operator, I need authentication to have minimal performance impact so
that the system can handle high request volumes from B2B clients.

#### Acceptance Criteria

1. When validating API keys, the system shall complete validation within 5ms for cached scenarios.
2. When under load, the system shall serve 2000+ requests per second without performance
   degradation.
3. When cache misses occur, the system shall fetch key information asynchronously without blocking
   other requests.
4. When multiple requests use the same API key, the system shall efficiently share cached validation
   results.

### Requirement 7 - Layered Error Handling

**User Story:** As a developer and operator, I need comprehensive error information for debugging
while providing secure error responses to clients.

#### Acceptance Criteria

1. When authentication fails, the system shall log detailed error information for debugging
   purposes.
2. When returning errors to clients, the system shall provide minimal information to prevent
   security leaks.
3. When different authentication operations fail, the system shall categorize errors by business
   operation (ApiKeyValidationFailed, ClientContextExtractionFailed, etc.).
4. When errors occur, the system shall follow the established two-layer error handling pattern
   (AuthError -> AppError).

### Requirement 8 - Future Extensibility

**User Story:** As a system architect, I need the authentication system to support future expansion
to JWT and service authentication so that the system can grow with business needs.

#### Acceptance Criteria

1. When the system architecture is designed, it shall support adding JWT authentication for
   dashboard users without major refactoring.
2. When the system architecture is designed, it shall support adding internal service authentication
   for microservice communication.
3. When new authentication methods are added, they shall integrate with the existing unified
   middleware.
4. When extending authentication, the system shall maintain backward compatibility with existing API
   key authentication.

### Requirement 9 - Configuration Management

**User Story:** As a deployment engineer, I need flexible configuration options so that the system
can be deployed across different environments with appropriate security settings.

#### Acceptance Criteria

1. When deploying to different environments, the system shall read authentication configuration from
   environment variables.
2. When configuration is invalid or missing, the system shall fail to start with clear error
   messages.
3. When in development mode, the system shall support configuration overrides for testing.
4. When configuration changes, the system shall support runtime configuration updates where
   possible.

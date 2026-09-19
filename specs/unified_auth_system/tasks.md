# Implementation Plan

Following incremental development principles: start with core business logic that delivers immediate
value, then add supporting infrastructure organically as needed.

## Iteration 1: Protect Existing Endpoint (Immediate Business Value)

### Task 1: Add Authentication to Ingress Endpoint ✅ COMPLETED

- [x] 1.1 Create minimal authentication middleware
  - ✅ Created `src/middleware/auth.rs` with basic auth function
  - ✅ Implemented X-API-Key header extraction and validation
  - ✅ Hardcoded test API key: "test-api-key-123"
  - ✅ Returns 401 Unauthorized for missing/invalid keys
  - _Focus: Protect real business functionality immediately_

- [x] 1.2 Apply middleware to existing ingress endpoint
  - ✅ Applied auth middleware to `/api/v1/route` endpoint in main.rs
  - ✅ Created basic AuthContext struct with client_id
  - ✅ Inject mock client context for valid API keys
  - _Deliverable: /api/v1/route endpoint now requires API key authentication_

- [x] 1.3 Test protected endpoint
  - ✅ Tested with valid API key: `curl -H "X-API-Key: test-api-key-123" /api/v1/route` → 200 OK
  - ✅ Tested without API key: returns 401 Unauthorized
  - ✅ Verified existing ingress functionality still works with valid key
  - _Requirement: Requirement 1 - API Key Authentication_

## Iteration 2: Implement 3-Layer Architecture (Keep Mock Data)

### Task 2: Create Auth Feature Module with 3-Layer Architecture ✅ COMPLETED

- [x] 2.1 Create auth feature module structure
  - ✅ Created `src/features/auth/` directory
  - ✅ Created `src/features/auth/mod.rs` with basic exports
  - ✅ Created `src/features/auth/handler.rs` for HTTP endpoints (future use)
  - ✅ Created `src/features/auth/service.rs` for business logic
  - ✅ Created `src/features/auth/repository.rs` for data access (mock data)
  - ✅ Created `src/features/auth/models.rs` for data structures
  - ✅ Created `src/features/auth/mockdata.rs` for mock API keys
  - _Build 3-layer architecture following existing pattern_

- [x] 2.2 Add data models
  - ✅ Created ApiKeyInfo struct with necessary fields
  - ✅ Moved AuthContext struct from middleware to models.rs
  - ✅ Added Axum FromRequestParts implementation
  - _Follow same pattern as ingress models_

- [x] 2.3 Create mock repository
  - ✅ Implemented MockAuthRepository with hardcoded API keys
  - ✅ Implemented find_api_key_by_hash method with mock data
  - ✅ Added AuthRepository trait with async methods
  - _Deliverable: Structured mock data access_

- [x] 2.4 Create service layer
  - ✅ Moved hardcoded validation logic from middleware to AuthService
  - ✅ Implemented AuthService with validate_api_key method
  - ✅ Uses repository for data access
  - _Deliverable: Same functionality, better architecture_

- [x] 2.5 Update middleware to use service
  - ✅ Refactored middleware to use AuthService for validation
  - ✅ Maintained same API behavior
  - ✅ Removed hardcoded logic from middleware
  - _Deliverable: Clean separation of concerns_

## Iteration 3: Add Development Mode and Error Handling (Production Readiness)

### Task 3: Add Development Support and Error Handling

- [x] 3.1 Add development mode bypass ✅ COMPLETED
  - ✅ Added AUTH_DEVELOPMENT_MODE environment variable
  - ✅ Allow bypassing auth in development (no API key required)
  - ✅ Inject mock context when in dev mode (configurable client ID)
  - ✅ Added clear warnings when dev mode is enabled (🚨 emojis for visibility)
  - ✅ Added AUTH_DEV_CLIENT_ID environment variable for custom dev client ID
  - ✅ Updated middleware to check development mode before authentication
  - ✅ Updated main.rs to show development mode status in startup logs
  - _Requirement: Requirement 4 - Development Environment Support_

- [x] 3.2 Add error handling as needed ✅ COMPLETED
  - ✅ Added `src/features/auth/error.rs` with AuthError enum
  - ✅ Implemented only error types that are actually needed (ApiKeyValidationFailed,
    ApiKeyInactive, RepositoryOperationFailed)
  - ✅ Added proper error responses for authentication failures
  - ✅ Integrated AuthError into AppError with proper HTTP response mapping
  - ✅ Replaced println! with structured tracing logs
  - ✅ Updated repository to use Result<(), AuthError> instead of Result<(), String>
  - _Principle: Add error handling when errors occur, not preemptively_
  - _Requirement: Requirement 7 - Layered Error Handling_

- [x] 3.3 Add basic configuration management ✅ COMPLETED
  - ✅ Created `src/features/auth/config.rs` (completed in Task 3.1)
  - ✅ Added environment variable support (AUTH_DEVELOPMENT_MODE, AUTH_DEV_CLIENT_ID)
  - ✅ Implemented thread-safe global configuration with OnceLock
  - ✅ Added configuration validation and security warnings
  - ✅ Following "add only what's needed" principle - no unnecessary config added
  - _Add only configuration that is actually needed by this point_

## Iteration 4: Production Optimizations (Performance and Monitoring)

### Task 4: Add Production Support Features

- [x] 4.1 Add basic performance monitoring
  - ✅ Added authentication timing metrics using tracing (middleware, service, repository)
  - ✅ Recorded API key validation duration and success/failure reasons
  - ✅ Added slow-operation warnings with configurable threshold via AUTH_SLOW_THRESHOLD_MS (default
    10ms)
  - ✅ Updated .env.example with AUTH_SLOW_THRESHOLD_MS
  - _Focus: Monitor current performance, no premature optimization_
  - _Skip: Service instance caching, moka dependency - not needed with current mock data_

- [x] 4.2 Add comprehensive testing
  - [x] Unit tests for service layer (AuthService::validate_api_key)
  - [x] Repository tests with mock data (MockAuthRepository)
  - [x] Middleware helper tests (extract_api_key, validate flow) - no extra deps
  - [x] Middleware end-to-end tests (Router::oneshot via tower::ServiceExt)
  - [x] Error handling tests (AuthError IntoResponse mappings)
  - _Test based on actual implementation, ensure reliability_

- [x] 4.3 Skip comprehensive error handling expansion
  - Current error handling is sufficient for current needs
  - AuthError enum covers all actual error scenarios
  - Following "add only when needed" principle
  - _Will revisit when new error scenarios are encountered in practice_

## Future Iterations (Deferred until pre-launch): Database Integration and Extended Authentication

### Iteration 5: Database Integration (Future - Replace Mock Data)

- [ ] 5.1 Add database dependencies
  - Add supabase client and sha2 for hashing
  - Add uuid for key generation
  - _Add dependencies when ready for real data_

- [ ] 5.2 Implement database repository
  - Replace MockAuthRepository with DatabaseAuthRepository
  - Implement Supabase connection for API key storage
  - Create api_keys table schema in Supabase
  - Store hashed API keys (SHA-256)
  - _Requirement: Requirement 5 - Database Integration_

### Iteration 6: API Key Management (Future - Client Self-Service)

- [ ] 6.1 Add API key management endpoints
  - Implement handler.rs for HTTP endpoints
  - Add POST `/api/v1/auth/keys` (create API key)
  - Add GET `/api/v1/auth/keys` (list API keys)
  - Add DELETE `/api/v1/auth/keys/{id}` (revoke API key)
  - _Enable client self-service API key management_

### Iteration 7: JWT Authentication (Phase 2)

- [ ] 7.1 Add JWT support for dashboard users
  - Extend middleware to detect JWT tokens
  - Add JWT validation service
  - Integrate with existing unified middleware
  - _Requirement: Requirement 8 - Future Extensibility_

### Iteration 8: Internal Service Authentication (Phase 3)

- [ ] 8.1 Add lightweight service-to-service authentication
  - Implement internal service tokens
  - Add service context extraction
  - _Requirement: Requirement 8 - Future Extensibility_

## Development Principles Applied

### Business Value First

- **Iteration 1**: Protect real business endpoint from day 1
- **Iteration 2**: Persistent data without losing functionality
- **Iteration 3**: Production-ready features
- **Iteration 4**: Performance and monitoring optimizations

### Organic Infrastructure Growth

- **Dependencies**: Added only when features require them
- **Error Handling**: Added when errors are encountered
- **Configuration**: Added when configuration is needed
- **Caching**: Added when performance becomes an issue

### Incremental Architecture

- **Start Simple**: Hardcoded validation in middleware
- **Add Layers**: Service layer, then repository layer
- **Maintain Functionality**: Each iteration builds on working foundation
- **Test Continuously**: Every iteration is testable and demonstrable

## Success Criteria for Each Iteration

### Iteration 1 Success

- `/api/v1/route` endpoint requires valid API key
- Returns 401 for missing/invalid keys
- Existing ingress functionality works with valid key

### Iteration 2 Success

- Same API functionality with database persistence
- Clean 3-layer architecture
- API keys stored securely (hashed)

### Iteration 3 Success

- Development mode bypass functional
- Proper error handling for all scenarios
- Configuration management in place

### Iteration 4 Success

- Production-ready performance and monitoring
- Comprehensive error handling and logging
- Full caching and optimization

## Key Differences from Traditional Approach

### ❌ Traditional (Infrastructure First)

```
1. Set up all dependencies
2. Create complete error handling
3. Build configuration system
4. Design all data models
5. Implement all layers
6. Finally protect endpoints
```

### ✅ Incremental (Business Value First)

```
1. Protect real endpoint (hardcoded auth)
2. Add persistence (same API)
3. Add production features
4. Add optimizations
5. Add management features (later)
```

### Benefits of This Approach

- **Immediate Security**: Business endpoint protected from day 1
- **Immediate Value**: Authentication working immediately
- **Reduced Risk**: Each iteration is small and testable
- **Better Design**: Infrastructure grows based on actual needs
- **Faster Time to Value**: Real security delivered early
- **Focused Development**: Skip non-essential features initially

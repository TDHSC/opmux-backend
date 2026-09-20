# Architecture and Module Rules

Normative rules for how code in `gateway/` is structured. For the refactoring-specific subset see
`refactoring.md`; for error types see `error-handling.md`.

## 3-Layer Pattern

Every feature module under `gateway/src/features/` is split into three layers. Layers may only call
downward (handler → service → repository); skipping a layer is not allowed.

```
handler.rs      # HTTP layer
service.rs      # Business layer
repository.rs   # Data / external-access layer
```

### Handler (HTTP)

- Parse and validate requests, call the service, map results to HTTP responses.
- Receives dependencies via `AppState` / injected structs, never constructs them.
- No business logic, no external calls, no direct database access.

### Service (Business)

- Implements workflows and business rules; orchestrates multiple repository calls.
- Owns business error handling, retry, and fallback logic.
- No HTTP types, no direct external calls.

### Repository (Data / External)

- The only layer that touches external systems: LLM vendors, databases, files, other services.
- Returns data shaped as the source returns it; no business logic.
- May ship mock implementations (`mockdata.rs`) that are swappable without touching handlers.

### Example

```rust
// handler.rs
pub async fn ingress_handler(State(state): State<AppState>, Json(req): Json<IngressRequest>)
    -> Result<Json<IngressResponse>, IngressError> {
    let result = state.ingress_service.process_request(req, user_id, &ctx).await?;
    Ok(Json(result))
}

// service.rs
impl IngressService {
    pub async fn process_request(&self, req: IngressRequest) -> Result<IngressResponse, IngressError> {
        let plan = resolve_route(&self.settings.catalog, req.route.as_deref(), req.allow_fallback)?;
        let response = self.repository.execute_llm_call(&plan, &payload).await?;
        Ok(response)
    }
}
```

## Feature Module Layout

```
features/<feature>/
├── mod.rs              # Exports and wiring
├── handler.rs          # Handler layer
├── service.rs          # Service layer
├── repository.rs       # Repository layer
├── models.rs           # Request/response structs and domain types
├── error.rs            # Feature error enum (see error-handling.md)
├── config.rs           # Feature config (optional; prefer routing through core config)
├── constants.rs        # Constants (optional)
├── mockdata.rs         # Mock repository data (optional)
└── *_tests.rs          # Unit tests colocated with the layer they cover
```

Health has no repository-level external state beyond the executor dependency; small features may
omit files they do not need, but must not merge layers into one file.

## Gateway Layout

```
gateway/src/
├── main.rs             # Process boot: load settings, construct Application, bind/serve
├── app.rs              # Shared production router and middleware order
├── lib.rs              # AppState and crate exports
├── core/               # Shared primitives: config, error, tracing, metrics, correlation, contracts
├── middleware/         # HTTP middleware: auth, correlation_id
└── features/
    ├── auth/           # API key authentication (repository + mock data)
    ├── executor/       # LLM execution: retry/fallback in service, vendor plugins in vendors/
    ├── health/         # /health and /ready with success-only dependency cache
    └── ingress/        # /api/v1/route orchestration; depends on executor
```

Cross-feature dependencies are injected as `Arc<Service>` at startup in `app.rs` (for example
`IngressService::new(executor_service)`), never constructed inside a feature. The binary and HTTP
tests call the same `build_production_router` graph.

## Module Development Rules

- Reuse first: prefer existing crates and existing modules in this repo over hand-written
  implementations. If nothing fits, propose an alternative and explain why.
- Follow the most similar existing feature when adding a new one.
- Configuration comes from environment variables or config files via `core/config.rs`; no hard-coded
  values.
- Routing is centralized in `app.rs`; handlers are plain functions registered there. The binary
  boots that router from `main.rs`.
- Split files by responsibility. Do not grow a single file into a service, repository, and model
  dump.
- Vendor clients (`executor/vendors/`) implement the `LLMVendor` trait and contain no retry logic;
  retry and fallback live in `ExecutorService`.
- Middleware ordering in `app.rs` is behavior-critical; do not reorder without updating the comment
  that documents it.

## When to Apply the Full 3-Layer Split

Always for features that talk to external systems, have multiple data sources, or carry business
rules shared across handlers. Trivial endpoints (like the root hello route) may live in `main.rs`.

# Agent Guide

## Scope

Rust workspace centered on the `gateway` Axum service, with a small shared `common` crate. See
[README.md](README.md) for current capabilities, setup, commands, and the workspace structure.

This is the only agent instruction entry point. Keep engineering rules in `docs/rules/` and link
them here; do not create tool-specific copies. Keep user-facing setup and usage in `README.md` and
feature requirements and designs in `specs/`.

## Rules (Read First)

Read the applicable engineering rules below before acting; do not rely on summaries here.

- [Architecture](docs/rules/architecture.md): changes in `gateway/src/`, layering, module layout,
  reuse.
- [Error handling](docs/rules/error-handling.md): adding or changing error types and `AppError`.
- [Documentation](docs/rules/documentation.md): writing Rust doc comments.
- [Refactoring](docs/rules/refactoring.md): moving or restructuring existing code.

## Execution and Optional Workflow

- Work autonomously within the task's scope and granted permissions when the goal and acceptance
  conditions are clear. Do not request routine approval between planning, implementation, and
  validation. Honor explicit planning-only requests or review checkpoints.
- [Workflow reference](docs/rules/workflow.md) offers optional planning, spec, and testing
  techniques. Consult it when useful; it is not required reading or a mandatory sequence, and it
  does not require spec files or stage confirmations.
- Resolve low-risk implementation details using existing conventions and reasonable defaults; record
  assumptions that affect the result. Do not guess unresolved core requirements or expand scope.
- For a genuine blocker, state the missing decision, information, or permission. Ask a focused
  question in an interactive session; in an unattended run, report the blocker and stop affected
  work rather than waiting indefinitely or claiming completion.

## Working with Specs

- Read the relevant requirements, design, and tasks in `specs/` before changing a feature or
  proposing new requirements. Check any recorded status and compare with the current implementation.
- Specs describe intended behavior, not necessarily shipped behavior. Treat historical or unmarked
  documents as context to verify, not authoritative descriptions of the current architecture.
- If confirmed requirements and code disagree, note the discrepancy. Proceed when the current task
  or other explicit requirements clearly resolve the intended behavior; otherwise treat unresolved
  core behavior as a blocker under the execution guidance above. Do not silently assume either
  source is correct.

## Task Navigation

- Startup: `gateway/src/main.rs`. Shared production router: `gateway/src/app.rs`
  (`build_production_router` / `Application`). Shared `AppState`: `gateway/src/lib.rs`.
- Configuration: `gateway/src/core/config/` (`Settings`, catalog, limits); executor settings in
  `gateway/src/features/executor/config.rs`; metrics in `gateway/src/core/metrics.rs`.
- HTTP middleware: `gateway/src/middleware/`; API key validation: `gateway/src/features/auth/`.
- Operator CLI: `gateway/src/bin/opmux-admin.rs`; shared key generation/provisioning:
  `gateway/src/features/auth/credentials.rs` and `provision.rs`. HTTP auth uses persisted keys via
  `AuthService` and `DATABASE_URL`.
- Request orchestration: `gateway/src/features/ingress/service.rs` (`IngressService`), with context
  and routing access in `gateway/src/features/ingress/repository.rs`.
- LLM retries, fallback, circuit breakers: `gateway/src/features/executor/service.rs`
  (`ExecutorService`).
- Vendor registry and dispatch: `gateway/src/features/executor/repository.rs`
  (`ExecutorRepository`); vendor interface and OpenAI implementation:
  `gateway/src/features/executor/vendors/`.
- Health/readiness and successful-result caching: `gateway/src/features/health/`.
- HTTP-facing features use `handler.rs`, `service.rs`, and `repository.rs`; read the nearest
  existing feature and its tests before adding a new pattern.
- Local HTTP fixtures and simulator: `gateway/tests/support/`, `gateway/tests/http_fixture_test.rs`,
  `gateway/tests/openai_adapter_http_test.rs`. Observability HTTP tests:
  `gateway/tests/observability_integration_test.rs`. Deferred live-provider tests:
  `gateway/tests/executor_integration_test.rs` (`#[ignore]`; not key-gated). See the test guidance
  below before running them.
- API, configuration, operations, and test guides: the documentation links in `README.md`.

## High-Risk Guardrails

- Autonomous development is not blanket authorization for production operations, destructive
  migrations, billable API calls, or pushing, merging, and deploying changes. Perform those actions
  only when explicitly authorized by the task or automation policy and allowed by the execution
  environment; do not bypass permission controls.
- Do not enable `AUTH_DEVELOPMENT_MODE` in committed code or CI.
- Keep retry/fallback orchestration in `ExecutorService`, not vendor clients.
- Health checks cache only successful dependency states; failures must remain uncached.
- Middleware order is behavior-critical. Verify actual router composition and affected behavior when
  changing it; do not treat reordering as cosmetic.

## Validation and External Calls

Completion reports must state the changes, actual check results, relevant assumptions, and any
remaining limitations or blockers. Do not report unrun, skipped, blocked, or failing checks as
passed. Choose checks for the changed surface and explain omissions:

- **Markdown only:** check formatting and referenced paths for the changed files. No Rust build or
  tests are needed unless the change also affects code or build behavior. Avoid repository-wide
  formatting that rewrites unrelated files.
- **Rust changes:** run affected tests, rustfmt, and strict Clippy; expand to workspace checks when
  shared behavior changes. Commands are listed in `README.md`; CI configuration is in
  `.github/workflows/`.
- **Startup/configuration changes:** check both the configuration loader and startup wiring, then
  update the affected usage instructions. Copying `.env` alone does not configure the Rust binary.

`executor_integration_test.rs` is ignored live-provider verification. Inherited `OPENAI_API_KEY`
does not activate it. Routine tests must use dummy credentials and owned loopback simulator URLs,
and should clear provider/proxy environment (`env -u OPENAI_API_KEY -u ANTHROPIC_API_KEY` plus proxy
variables, `NO_PROXY='*'`). Do not run `--ignored` live tests unless explicitly requested.

`http_fixture_test.rs` and `openai_adapter_http_test.rs` exercise the shared production router and
the real Reqwest adapter against an owned loopback simulator. `observability_integration_test.rs`
uses that same router with dummy configuration. Ignored live tests are unrun; they are not evidence
of upstream compatibility.

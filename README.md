# Opmux Backend

Opmux's Rust workspace contains an Axum gateway (`gateway`) and a small shared crate (`common`). The
gateway exposes an authenticated AI request endpoint, executes LLM calls through vendor adapters,
and provides health checks, correlation IDs, and Prometheus metrics.

## Current Capabilities

- `POST /api/v1/route`: request orchestration protected by API key authentication.
- LLM execution: an OpenAI vendor implementation, with retry, fallback, and circuit-breaker logic in
  the executor service.
- Observability: `X-Request-ID`, optional `X-Correlation-ID` echo, `/health`, `/ready`, and a
  configurable metrics endpoint (`/metrics` by default).

**Implementation boundary:** API key lookup currently uses a mock repository; conversation context
and routing optimization also use mock data. Real upstream LLM execution does not make the entire
pipeline production-ready. Planned Memory/Router/Rewrite/Validation microservices and additional
vendors should not be treated as implemented capabilities.

## Getting Started

### Prerequisites

- Rust stable and Cargo, with rustfmt and Clippy for development.
- Node.js and npm for non-Rust formatting.
- Docker with Compose, if using the container setup.

From the repository root:

```bash
cargo build
npm ci
```

### Local Startup Check (No Real LLM Calls)

The gateway requires a configured vendor at startup; without one it exits with
`NoVendorsConfigured`. Use a dummy key and an intentionally unavailable local upstream to check
startup and failure handling:

```bash
SERVER_HOST=127.0.0.1 SERVER_PORT=3000 \
AUTH_DEVELOPMENT_MODE=false METRICS_ENABLED=true METRICS_PATH=/metrics \
OPENAI_API_KEY=dummy-key OPENAI_BASE_URL=http://127.0.0.1:9/v1 OPENAI_TIMEOUT_MS=200 \
cargo run -p gateway
```

These environment overrides apply only to this command. In another terminal:

```bash
curl -i http://127.0.0.1:3000/health
curl -i http://127.0.0.1:3000/ready
curl -i http://127.0.0.1:3000/metrics
```

Expected with no upstream listening on port 9: `/health` returns `200`, `/ready` returns `503`, and
`/metrics` returns Prometheus output. This is a failure simulation, not a working LLM configuration.
A healthy process does not imply healthy upstream dependencies.

### Real Upstream Access

Replace the placeholder with a valid provider key in your local environment:

```bash
SERVER_HOST=127.0.0.1 SERVER_PORT=3000 AUTH_DEVELOPMENT_MODE=false \
OPENAI_API_KEY='<your-provider-key>' OPENAI_BASE_URL=https://api.openai.com/v1 \
cargo run -p gateway
```

`OPENAI_BASE_URL` defaults to `https://api.openai.com/v1` when unset; override it for a compatible
endpoint. Keep real keys out of version control. The provider key is separate from the gateway's
`X-API-Key` request header; see the [API reference](docs/API_REFERENCE.md) for request examples.
Authentication remains mock-backed even when using a real upstream.

The Rust binary reads **process environment variables** and does not automatically load `.env`.
Copying [.env.example](.env.example) to `.env` alone will not configure `cargo run`; export the
needed variables or explicitly load them with your local tooling. The template includes
planned-service settings, so its presence is not evidence that those integrations are implemented.

Logging defaults to JSON at `info` level. Set `LOG_FORMAT=pretty` for readable local logs and
`RUST_LOG=gateway=debug` for more detail. `RUST_LOG` overrides legacy `LOG_LEVEL`; `LOG_FORMAT`
overrides legacy `LOG_JSON`. Set `LOG_VERBOSE_DEBUG=true` to include line numbers and thread IDs.

### Docker Compose

```bash
docker compose up --build
```

The gateway is available at `http://127.0.0.1:3000`. Without overrides,
[docker-compose.yml](docker-compose.yml) uses a dummy key and `http://host.docker.internal:9/v1` as
the upstream. Expect readiness failure unless that endpoint actually serves a compatible API;
starting a container is not proof of LLM availability.

For real upstream access, set **both** values so the Compose dummy URL is not retained:

```bash
OPENAI_API_KEY='<your-provider-key>' OPENAI_BASE_URL=https://api.openai.com/v1 \
docker compose up --build
```

Unlike the Rust binary, Compose can read a root `.env` file for variable substitution. Only settings
passed through the service configuration become container environment variables. The Compose setup
publishes port 3000; treat it as a development setup, not a hardened production deployment.

## Development Checks

### Tests

Run the test suite without enabling the credential-gated executor integration tests:

```bash
env -u OPENAI_API_KEY cargo test
```

- `gateway/tests/executor_integration_test.rs` skips its test bodies when `OPENAI_API_KEY` is
  absent. When the variable is present, even if empty or a dummy value, the suite is enabled and can
  contact the configured provider. Real calls may incur charges; skipped bodies are not evidence of
  upstream compatibility.
- `gateway/tests/observability_integration_test.rs` uses dummy configuration and a local unavailable
  upstream to exercise HTTP behavior and failure scenarios. It does not depend on that skip guard.
- `gateway/tests/startup_integration_test.rs` launches the binary without vendor keys to verify
  logging defaults, environment-variable precedence, and the expected startup failure.

To run the same local-only binary smoke check used by the security workflow:

```bash
cargo build -p gateway
bash scripts/check-startup.sh "$PWD/target/debug/gateway"
```

The check binds to `127.0.0.1:3000` (override with `STARTUP_CHECK_PORT`), uses dummy credentials and
an unavailable local upstream, and cleans up its process and temporary logs. It checks liveness,
readiness failure, authentication, metrics, and correlation headers. Without a binary argument it
uses `target/release/gateway`.

To intentionally run the real-provider suite after configuring credentials:

```bash
cargo test -p gateway --test executor_integration_test -- --nocapture
```

See the [executor integration test guide](gateway/tests/README.md) for setup details.

### Formatting and Linting

```bash
cargo fmt --all -- --check
cargo clippy --all-targets --all-features -- -D warnings
npm run format:check
```

Use `cargo fmt` to format Rust. `npm run format` formats the repository's non-Rust files; for a
small Markdown change, prefer targeting the changed files, for example
`npx prettier --write README.md AGENTS.md`.

[CI](.github/workflows/ci.yml) configures tests on stable, beta, and nightly Rust, formatting
checks, strict Clippy, dependency auditing with `cargo audit`, and a release build uploaded from
`target/release/gateway`. CI explicitly removes `OPENAI_API_KEY` for routine tests; dummy vendor
configuration is scoped to the separate startup check. It does not currently configure a
code-coverage job.

## Project Structure

```text
opmux-backend/
├── common/             # Shared crate
├── gateway/            # Axum service crate
│   ├── src/            # Startup, core, middleware, and feature modules
│   └── tests/          # Provider integration and observability tests
├── docs/               # API and operations documentation
│   └── rules/          # Engineering rules
├── specs/              # Requirements, designs, and implementation tasks
├── scripts/            # Developer and CI utilities
├── .github/workflows/  # CI configuration
├── Cargo.toml          # Workspace members and shared dependencies
└── package.json        # Formatting tools
```

## Documentation and Contributing

- [AGENTS.md](AGENTS.md): agent instructions, task navigation, and the engineering-rule index.
- [Architecture rules](docs/rules/architecture.md): layering and module boundaries.
- [Suggested workflow](docs/rules/workflow.md): planning, specs, incremental delivery, and testing.
- [API reference](docs/API_REFERENCE.md) and
  [configuration troubleshooting](docs/CONFIGURATION_TROUBLESHOOTING.md).
- [Operations runbook](docs/OPERATIONS_RUNBOOK.md).
- [Observability](docs/OBSERVABILITY.md) and [Prometheus](docs/PROMETHEUS.md).
- [Observability testing](gateway/tests/OBSERVABILITY_TESTING.md) and
  [performance/load testing](gateway/tests/PERFORMANCE_LOAD_TESTING.md).

Read relevant requirements and designs in `specs/` before changing a feature, but verify their
status against the implementation: historical or unmarked specs are not an inventory of shipped
features. Keep usage instructions here, engineering rules in `docs/rules/`, and feature requirements
in `specs/`.

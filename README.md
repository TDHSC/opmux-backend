# Opmux Backend

Opmux's Rust workspace contains an Axum gateway (`gateway`) and a small shared crate (`common`). The
gateway exposes an authenticated AI request endpoint, executes LLM calls through vendor adapters,
and provides health checks, correlation IDs, and Prometheus metrics.

## Current Capabilities

- `POST /api/v1/route`: stateless configured routing protected by inference API-key authentication.
  Omitted `route` uses the catalog default; a named route selects that route's primary target.
  Unknown routes return `400` before any provider call. Omitted `allow_fallback` follows the
  configured chain; `true` cannot invent fallbacks a route does not have; `false` limits execution
  to the primary without disabling its retries. Transient transport, attempt-timeout, and provider
  5xx failures may continue to later equal-capability targets in catalog order under the shared
  attempt budget and deadline. Client, protocol, shared-credential, quota, and same-account
  throttling failures do not switch models. A fallback whose `max_output_tokens` cannot satisfy the
  already-validated request is skipped without clamping parameters. Successful fallbacks report the
  fallback target's `model_used` and cost. Exhausted eligible paths keep the original primary error
  unless the overall deadline expires (`504`). Transient hop failures open a target-scoped circuit
  after `circuit_failure_threshold` consecutive failures; an open primary is skipped without using
  an attempt and does not block a healthy same-provider fallback. After `circuit_cooldown_ms`, at
  most one half-open probe may run; success closes the circuit and failure reopens it. Completions
  apply only to the still-current admission generation and phase, so a late success cannot close a
  newer open circuit before cooldown, a late failure cannot reopen a recovered target, and a late
  closed completion cannot admit another probe while one is held. The late request's result is still
  returned. If every eligible target is open, the response is `503 CIRCUIT_OPEN` with no generation
  call. Permanent credential, quota, protocol, and rejection errors do not open circuits.
  Same-account throttling may retry the current hop under the shared budget, but it does not open or
  reopen circuits. A complete bounded HTTP 429 JSON body whose `error.code` or `error.type` is
  `insufficient_quota` is terminal quota (`502 UPSTREAM_ERROR`) with no retry or fallback. Optional
  `parameters.temperature` (`0.0`–`2.0`), `parameters.top_p` (`0.0`–`1.0`), and integral
  `parameters.max_tokens` are validated against the selected primary cap and forwarded as JSON
  numbers/integers. Omitted parameters use documented defaults (provider sampling defaults; target
  `max_output_tokens` for `max_tokens`). Unknown controls, `stream`, and `rewrite` return `400`.
  Prompt bounds use the original untrimmed character and UTF-8 byte lengths. Management credentials
  receive `403` and do not generate. Metadata stays opaque and is not forwarded, logged, or
  persisted. Successful responses preserve provider `content`, `role`, and `finish_reason`.
  Successful Chat Completions `message.role` must be exactly `assistant`; other roles fail as
  protocol errors. `model_used` is the provider-reported model, which may differ from the selected
  target alias sent as the request `model`. `cost` is a USD estimate for that successful response
  from the selected target's configured per-million prices (illustrative `1.0`/`2.0` with 120/30
  tokens is `0.00018`), rounded to 8 decimal places. Distinct target IDs keep their own prices even
  when they request the same provider model. Missing prices fail rather than becoming zero. The
  estimate is not current provider billing and does not total retries or abandoned work. Malformed
  successful payloads, empty choices, missing or invalid required fields, non-assistant roles, and
  impossible usage fail as upstream protocol errors without fabricating model, content, usage, role,
  or cost. Provider response bodies are capped by `max_upstream_response_bytes` while the bytes are
  read, including chunked transfer and advertised `Content-Length`. Those protocol faults are not
  retried as network failures. Concurrent generation is capped by `max_concurrent_generations`
  (default 32). Saturation returns `429 OVERLOADED` with `Retry-After: 1` immediately and does not
  queue; the permit covers primary work, retry backoff, and fallback, and is released on success,
  upstream failure, deadline, and cancellation. `/health`, `/ready`, and `/metrics` stay independent
  of generation slots. Protected endpoint errors, including JSON and path extraction rejections, use
  `{"error":{"code","message","request_id"}}` with `X-Request-ID`. Upstream credential, protocol,
  and oversized failures are sanitized `502` and are never a gateway `401`.
- `POST /api/v1/auth/keys` and `GET /api/v1/auth/keys`: management-only, same-tenant key creation
  and inventory. Creation returns the secret once with `Cache-Control: no-store`. That one-time
  output is not guaranteed delivery or exactly-once issuance. A timeout or disconnect during
  creation does not prove rollback; a committed key whose response was lost cannot reveal its
  plaintext again and may be revoked or replaced from inventory or `opmux-admin`. Inventory returns
  at most 100 safe metadata rows, newest first, with optional `limit`/`offset`/`kind` paging. Query
  ownership selectors and invalid paging are rejected. Protected-request deadlines still apply to
  these mutations.
- `DELETE /api/v1/auth/keys/{id}`: management-only same-tenant revocation. First and repeat DELETE
  return `204`. Other-tenant and unknown IDs return indistinguishable `404`. Self-revocation and
  final-manager revocation are allowed; recover with `opmux-admin key issue`. Subsequent auth is
  denied after commit on every gateway process sharing the database; already admitted requests may
  finish. A timeout during DELETE does not prove rollback; check inventory and repeat if needed.
  Inventory `last_used_at` is the last successful authentication, persisted monotonically before
  admission (including when later inference fails). Invalid credentials do not update it.
- LLM execution: an OpenAI vendor implementation, with retry, fallback, and circuit-breaker logic in
  the executor service.
- Observability: `X-Request-ID`, optional `X-Correlation-ID` echo, process liveness on `/health`,
  dependency readiness on `/ready`, and a configurable metrics endpoint (`/metrics` by default).
  `/metrics` is an internal scrape surface (loopback locally; network-restrict in production) and
  exposes HTTP counts/duration plus bounded execution attempts, retries, fallback, circuit,
  deadline, overload, and successful-usage series. Labels are route templates, configured target
  IDs, and finite outcome classes, never credentials, prompts, tenant/key/request IDs, raw URLs, or
  provider-returned model strings. `/ready` requires authentication-database schema,
  selected-column, locking, and `last_used_at` UPDATE access, upstream `/models` reachability, and
  at least one usable default-route target. `/models` is not a generation call and cannot override
  circuit-open default-route targets. Successful probes cache for `HEALTH_CHECK_CACHE_TTL_SECS`
  (default 5 seconds); failures are never cached. SIGTERM and SIGINT close generation admission,
  mark `/ready` unready even for in-flight probes, reject later generation with `503 DRAINING`
  rather than `429 OVERLOADED`, and bound already admitted work by `SERVER_SHUTDOWN_TIMEOUT`
  (default 30 seconds). An occupied listen address fails startup with a sanitized
  `bind_address_in_use` diagnostic and does not disturb the existing listener.

**Implementation boundary:** Request authentication uses persisted API keys in local Supabase.
Provision tenants with `opmux-admin`; former public mock keys are rejected. Ingress selects
operator-configured routes only; there is no Memory/Router service and no conversation history. The
OpenAI adapter speaks Chat Completions against the configured base URL; live-provider verification
remains deferred. Eligible configured fallback switching and target-scoped circuits are enforced.
Planned Rewrite/Validation microservices and additional vendors should not be treated as implemented
capabilities. Explicit `stream`/`rewrite` requests are rejected.

## Getting Started

### Prerequisites

- Rust stable and Cargo, with rustfmt and Clippy for development.
- Node.js and npm for non-Rust formatting.
- Docker with Compose, if using the documented local container stack.
- `npm ci` installs Prettier and the pinned Supabase CLI **2.117.0** used by
  `scripts/db-migrate.sh`. Do not run `supabase start` from this repository.

From the repository root:

```bash
cargo build
npm ci
```

### Local Startup Check (No Real LLM Calls)

The gateway requires `OPMUX_CONFIG_FILE` (non-secret JSON catalog), `OPENAI_API_KEY`, and
`DATABASE_URL`. Invalid catalogs, blank credentials, missing/invalid database configuration,
out-of-range limits, or HTTP client construction failures exit before the process binds. Copying
`.env` is not process configuration; export variables or load them with your local tooling.
`AUTH_DEVELOPMENT_MODE` does not bypass authentication.

Use a dummy provider key, the example catalog, owned local Supabase, and an intentionally
unavailable local upstream to check startup and failure handling:

```bash
bash scripts/with-owned-database.sh env \
SERVER_HOST=127.0.0.1 SERVER_PORT=3000 \
OPMUX_CONFIG_FILE="$PWD/config/opmux.example.json" \
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

Replace the placeholder with a valid provider key in your local environment, and keep `DATABASE_URL`
pointed at local Supabase:

```bash
bash scripts/with-owned-database.sh env \
SERVER_HOST=127.0.0.1 SERVER_PORT=3000 AUTH_DEVELOPMENT_MODE=false \
OPMUX_CONFIG_FILE="$PWD/config/opmux.example.json" \
OPENAI_API_KEY='<your-provider-key>' OPENAI_BASE_URL=https://api.openai.com/v1 \
cargo run -p gateway
```

`OPENAI_BASE_URL` defaults to `https://api.openai.com/v1` when unset; override it for a compatible
http or https path-prefix endpoint. Userinfo, query strings, and fragments (including empty `?` or
`#`) are rejected before the process binds; a trailing slash is stripped. Keep real keys out of
version control. The provider key is separate from the gateway's `X-API-Key` request header; see the
[API reference](docs/API_REFERENCE.md) for request examples. Gateway `X-API-Key` values must be
operator-provisioned credentials, not the former public mock keys.

The example catalog at [config/opmux.example.json](config/opmux.example.json) defines a default
route, a named `fast` route, targets, illustrative per-million prices, and a flat fallback list.
Those model names and prices are samples for configuration tests, not current provider billing or
availability. Successful-response cost uses those configured target prices and provider usage; it is
not a retry-total bill. See [configuration troubleshooting](docs/CONFIGURATION_TROUBLESHOOTING.md)
for the canonical schema, documented defaults, and numeric bounds.

Omitted optional limits default to a 30-second protected-request deadline, 10-second attempt
maximum, one retry per target, three total provider attempts, at most two fallback targets, and a
2-second backoff cap. Those values are validated and injected at startup.
`max_upstream_response_bytes` (default 1 MiB) is enforced while reading provider success bodies. The
protected-request deadline covers authentication, body extraction, and execution as one monotonic
budget; expiry returns sanitized `504 DEADLINE_EXCEEDED` and does not start later provider attempts.
Health and metrics stay outside that deadline. Each provider attempt uses
`min(max_attempt_timeout, remaining deadline)`. Default single-target policy allows one retry (two
actual calls); a higher per-target retry setting still cannot exceed `max_total_attempts` across
primary and fallback hops, and considering a fallback does not reset that counter. Backoff is
full-jitter exponential capped at `backoff_cap_ms` (default 2 seconds). A valid `Retry-After`
delta-seconds or HTTP-date is never shortened by that cap; if the provider minimum cannot finish
before the deadline, the whole request ends as sanitized `429 UPSTREAM_RATE_LIMIT` with no later
provider calls, including configured fallbacks, rather than a false `504 DEADLINE_EXCEEDED`. Actual
overall expiry is always `504`, including when a ready 429 or saved `Retry-After` is already
observed. Provider 429 responses save header throttling immediately, then refine a complete bounded
body inside a short window of the remaining attempt budget (`min(100ms, remaining / 2)`). Complete
`insufficient_quota` JSON is terminal quota; stalled, failed, malformed, or oversized bodies keep
the saved throttling. Malformed `Retry-After` uses the capped jitter instead of an unbounded sleep.
Eligible fallback switching and target-scoped circuits are enforced: an open primary does not block
a healthy same-provider fallback, skipped open targets consume no attempt, and recovery uses one
half-open probe per target. Concurrent generation admission and inbound raw-size limits are
enforced.

The Rust binary reads **process environment variables** and does not automatically load `.env`.
Copying [.env.example](.env.example) to `.env` alone will not configure `cargo run`; export the
needed variables or explicitly load them with your local tooling. The template includes
planned-service settings, so its presence is not evidence that those integrations are implemented.

### Local persistence (Supabase)

API-key persistence uses SQLx 0.8.6 against Postgres. Schema lives in
[supabase/migrations](supabase/migrations) and is tracked by Supabase CLI 2.117.x, not a SQLx
`_sqlx_migrations` ledger. Apply migrations as an explicit setup step; running replicas must not
migrate on startup.

```bash
bash scripts/with-owned-database.sh bash scripts/db-migrate.sh
bash scripts/with-owned-database.sh bash scripts/ci-setup-db.sh
```

`scripts/ci-setup-db.sh` prepares portable role stubs, applies the same history, and reapplies it as
a no-op. It requires `DATABASE_URL` and does not skip. CI uses that script against disposable
Postgres 17; local use wraps it with `scripts/with-owned-database.sh`. Do not reset retained mission
data to reapply migrations.

`scripts/with-owned-database.sh` always verifies the owned container, the exact `127.0.0.1:55432`
loopback binding, and readiness, then replaces any inherited `DATABASE_URL` privately. Use that
wrapper for local tests, smoke, and mission migrations. `scripts/db-migrate.sh` itself still applies
the same history to a chosen `DATABASE_URL` for operator/production Postgres; that path is not
restricted to localhost. Do not run `supabase start` from this repository (that would create a
second stack). Do not use hosted project linking. Persistence tests fail if the owned database is
missing; they do not skip.

Gateway startup requires `DATABASE_URL` after catalog load. Missing or invalid URLs fail before the
process serves protected work. Use a direct or session-mode pooler URL for hosted Postgres
(`sslmode=verify-full` plus a CA). Transaction-mode poolers are incompatible with SQLx prepared
statements. Platform `anon` / `service_role` keys are not Opmux API keys and cannot read
`opmux_private` digests. The gateway assumes `SET ROLE opmux_runtime` (override
`OPMUX_RUNTIME_DB_ROLE`). Transient database outages keep `/health` live, mark `/ready` not ready
after any success cache expires, and fail closed with 503 on protected routes. Restoring the
database does not revive revoked keys or introduce an authentication cache.

`opmux-admin` is the operator CLI, not an HTTP bootstrap. It uses `DATABASE_URL` and
`SET ROLE opmux_operator` (override with `OPMUX_DB_ROLE`). Schema migrations stay with the database
owner via `scripts/db-migrate.sh`; `opmux_runtime` cannot create tenants. Successful commands print
one JSON object to stdout, including the newly generated `opmx_v1_` credential **once**. Write that
stdout to a **fresh** private file created with `mktemp` (mode 0600 even under umask 022). Do not
redirect onto an existing path or symlink. Do not log the secret. There is no later retrieval; issue
a replacement key to recover.

```bash
keyfile=$(mktemp "${TMPDIR:-/tmp}/opmux-key.XXXXXX")
chmod 600 "$keyfile"
bash scripts/with-owned-database.sh cargo run -p gateway --bin opmux-admin -- \
  tenant create --name acme > "$keyfile"
inference_file=$(mktemp "${TMPDIR:-/tmp}/opmux-key.XXXXXX")
chmod 600 "$inference_file"
bash scripts/with-owned-database.sh cargo run -p gateway --bin opmux-admin -- \
  key issue --client-id "$CLIENT_ID" --kind inference --name route > "$inference_file"
```

HTTP generation authenticates persisted inference keys. Management keys may create additional
management or inference keys for their own tenant through `POST /api/v1/auth/keys`; they cannot
generate. `GET /api/v1/auth/keys` lists that tenant's safe metadata only (at most 100 keys, newest
first; optional `limit`, `offset`, and `kind`). Ownership selectors and invalid paging/kind values
return 400. `last_used_at` is null until the first successful authentication and then never moves
backward. Failed or revoked credentials do not update it; a request that authenticates and then
fails downstream still counts. `DELETE /api/v1/auth/keys/{id}` revokes a same-tenant key (204,
idempotent) and returns indistinguishable 404 for other-tenant or unknown IDs. There is no
authentication cache: another gateway process using the same database denies a revoked key as soon
as DELETE commits. Rotate a manager by creating a replacement, verifying it, then revoking the old
key. Final-manager self-revocation is allowed; recover with
`opmux-admin key issue --client-id UUID --kind management --name NAME` for the existing client.
Inference keys cannot create, list, or revoke keys. Do not seed `test-api-key-123` or
`dev-api-key-456` into `opmux_private`; those former public keys return 401.

Wrap workspace tests so they receive the owned database URL. The wrapper ignores inherited remote or
unrelated-local `DATABASE_URL` values. `scripts/with-safe-test-env.sh` then removes inherited
provider/proxy variables, disables live-provider opt-in, and forces a dummy loopback OpenAI URL:

```bash
bash scripts/with-owned-database.sh bash scripts/with-safe-test-env.sh \
  env CARGO_BUILD_JOBS=2 \
  cargo test --workspace --locked --all-features -j 2 -- --test-threads=2
```

Logging defaults to JSON at `info` level. Set `LOG_FORMAT=pretty` for readable local logs and
`RUST_LOG=gateway=debug` for more detail. `RUST_LOG` overrides legacy `LOG_LEVEL`; `LOG_FORMAT`
overrides legacy `LOG_JSON`. Set `LOG_VERBOSE_DEBUG=true` to include line numbers and thread IDs.

### Container image

The image in [gateway/Dockerfile](gateway/Dockerfile) builds with the verified Rust 1.89.0 Bookworm
toolchain and `cargo build --locked --release`. The runtime is Debian Bookworm slim with CA
certificates and OpenSSL (`libssl3`), and the process runs as UID `65532`. Building the image does
not require `DATABASE_URL` or a provider credential. The image does not bake connection strings, API
keys, or `AUTH_DEVELOPMENT_MODE`.

Certificate-chain and hostname verification stay enabled in the Reqwest adapter. Do not pass
insecure TLS flags or use `curl -k` against the gateway. Local simulators and local Postgres may use
plain HTTP and `sslmode=disable`; those choices are local-only and are not hosted or provider TLS
proof.

```bash
docker build --file gateway/Dockerfile --tag opmux-gateway:mvp .
bash scripts/check-container.sh
```

`scripts/check-container.sh` builds the image, publishes the API on `127.0.0.1:38086` only, joins
the owned local Supabase network, and uses owned loopback HTTP and untrusted-TLS simulators. It
checks the non-root UID, `/health` and `/ready`, persisted-auth generation through the real adapter,
invalid-key `401` with zero upstream calls, untrusted TLS `502`, and `docker stop` graceful
shutdown. `opmux-admin` is included in the image for operator provisioning.

### Local container stack

The documented local stack is the locked gateway image plus the in-repo OpenAI simulator, attached
to the **existing** database-only Supabase container `supabase_db_opmux-mvp-20260919`. It does not
start a second Postgres, Auth, Studio, REST, or other Supabase services. Host publishes are
loopback-only in the approved ranges: database `127.0.0.1:55432`, gateway `127.0.0.1:38080`
(`/health`, `/ready`, `/metrics`), simulator `127.0.0.1:38081`. Docker-internal listeners may use
`0.0.0.0`; that is not host publication. The default upstream is the local simulator with dummy
credentials, not `api.openai.com`.

Do not run `docker compose up` without the wrapper: Compose needs a private container-network
`DATABASE_URL` for the owned database. `scripts/local-stack.sh` verifies the loopback database
binding, injects that owned URL (it wins over inherited `OPMUX_LOCAL_DATABASE_URL`), and starts only
the gateway and simulator. Generated Compose env is private per invocation. The wrapper does not
load project `.env` and does not overwrite or delete `.env.opmux-local` just because that filename
exists. Process environment overrides the generated file, which overrides Compose defaults. Select
the gateway image with `CONTAINER_IMAGE` (default `opmux-gateway:mvp`); a missing selected image
fails clearly when `OPMUX_LOCAL_BUILD=0`.

```bash
bash scripts/local-stack.sh up
bash scripts/local-stack.sh status
bash scripts/local-stack.sh bindings
curl --noproxy '*' -i http://127.0.0.1:38080/health
curl --noproxy '*' -i http://127.0.0.1:38080/ready
curl --noproxy '*' -i http://127.0.0.1:38080/metrics
```

Provision through the image so the operator CLI uses the same private database URL:

```bash
keyfile=$(mktemp "${TMPDIR:-/tmp}/opmux-key.XXXXXX")
chmod 600 "$keyfile"
bash scripts/local-stack.sh admin tenant create --name acme > "$keyfile"
```

Stop/start of the application stack removes only owned `opmux-local` gateway and simulator
containers after checking Compose project, service, and repository labels. It does not use force
removal, does not stop the shared database/network/volume, and does not need the generated env file.
`docker stop --time 605` is a fixed ceiling for the 600-second application maximum; a normal exit
returns immediately. After `up` again, `/ready` returns 200 and persisted inference keys still
authenticate.

```bash
bash scripts/local-stack.sh down
bash scripts/local-stack.sh up
```

`scripts/check-local-stack.sh` is the acceptance check for those bindings, image selection, delayed
graceful stop, reuse, and restart behavior. It does not stop unrelated containers. `/metrics` stays
on the loopback gateway listener; production must restrict scrape access at the network layer rather
than adding metrics authentication.

Hosted Postgres is a connection-string/TLS documentation path only. Use a direct or session-mode
pooler URL with `sslmode=verify-full` and a CA. Do not link this repository to a hosted Supabase
project, do not run hosted mutations from local tooling, and do not treat local `sslmode=disable` as
hosted TLS proof.

For a paid compatible upstream, replace **both** the dummy key and simulator URL so the local
Compose default is not retained. That path is optional and is not the documented local stack.

## Development Checks

### Tests

Run the test suite. Routine tests isolate provider and proxy environment so inherited credentials
cannot contact a public provider:

```bash
bash scripts/with-owned-database.sh bash scripts/with-safe-test-env.sh \
  env CARGO_BUILD_JOBS=2 \
  cargo test --workspace --locked --all-features -j 2 -- --test-threads=2
```

- HTTP fixtures in `gateway/tests/http_fixture_test.rs` and
  `gateway/tests/openai_adapter_http_test.rs` start an owned loopback OpenAI simulator, build the
  same production router as the binary, and exercise `/health`, generation, metrics, and the real
  Reqwest adapter. They bind `127.0.0.1` only and abort the simulator task on drop.
- `gateway/tests/health_readiness_test.rs` exercises `/health` vs `/ready` against real Supabase and
  the owned simulator, including independent database and `/models` outages, success-only caching,
  revoked-key persistence after recovery, default-route circuit override of cached upstream health,
  and SELECT-only/wrong-column UPDATE runtime roles staying unready until `last_used_at` UPDATE is
  granted.
- `gateway/tests/observability_integration_test.rs` uses that shared router with dummy configuration
  and a local unavailable upstream for HTTP failure scenarios.
- `gateway/tests/startup_integration_test.rs` launches the binary without vendor keys to verify
  logging defaults, environment-variable precedence, and the expected startup failure.
- `gateway/tests/process_lifecycle_test.rs` launches owned gateway subprocesses for occupied-port
  bind failure, SIGTERM/SIGINT draining, grace-bounded cancellation, and same-port restart against
  the owned simulator and local Supabase.
- `gateway/tests/release_acceptance_test.rs` is the bounded milestone-15 CLI/API lifecycle
  acceptance flow: actual `opmux-admin` tenant provisioning, HTTP key issuance/rotation/revocation,
  default and named simulated-provider generation, tenant isolation with zero upstream calls on
  denials, gateway restart with retained database state, one fallback recovery, and one database
  outage recovery. It uses isolated fixture tenants and does not add production test-control routes.
  OpenAI results are **SIMULATED ONLY**.
- `gateway/tests/executor_integration_test.rs` is **ignored live-provider verification**. It does
  not run because a key is present. Live OpenAI checks are unrun unless you explicitly opt in.

To run the same local-only binary smoke check used by the security workflow:

```bash
cargo build -p gateway
bash scripts/with-owned-database.sh bash scripts/check-startup.sh "$PWD/target/debug/gateway"
```

The check binds to `127.0.0.1:3000` (override with `STARTUP_CHECK_PORT`), uses dummy credentials and
an unavailable local upstream, and cleans up its process and temporary logs. It checks liveness,
readiness failure, authentication, metrics, and correlation headers. Without a binary argument it
uses `target/release/gateway`.

Live-provider tests stay ignored. Do not treat ignored or unrun live tests as provider validation.
If live verification is explicitly requested later:

```bash
OPMUX_LIVE_PROVIDER_TESTS=1 cargo test -p gateway --test executor_integration_test -- --ignored --nocapture
```

See the [integration test guide](gateway/tests/README.md) for local simulator and deferred live
setup.

### Formatting and Linting

```bash
cargo fmt --all -- --check
cargo clippy --workspace --locked --all-targets --all-features -j 2 -- -D warnings
cargo check --workspace --locked --all-targets --all-features -j 2
npm run format:check
```

Use `cargo fmt` to format Rust. `npm run format` formats the repository's non-Rust files; for a
small Markdown change, prefer targeting the changed files, for example
`npx prettier --write README.md AGENTS.md`.

The local equivalent of [CI](.github/workflows/ci.yml) is:

```bash
bash scripts/ci-local.sh
```

That script applies canonical migrations to the **owned** loopback database, then runs the locked
workspace test/check/Clippy/fmt/Prettier gates, the startup smoke check, and the locked image
build/runtime acceptance. It fails clearly if the owned database is unavailable; it does not skip
persistence checks, push, or start a remote job.

CI pins **Rust 1.89.0** (`rust-toolchain.toml`) and runs the same gates with `-j 2` /
`--test-threads=2`. The test job provisions disposable Postgres 17.6 on `127.0.0.1:55432`, installs
the pinned Supabase CLI 2.117.x, and applies `supabase/migrations` through `scripts/ci-setup-db.sh`.
It does not depend on this workstation's container. Routine tests use
`scripts/with-safe-test-env.sh` so inherited provider keys, proxy variables, and
`OPMUX_LIVE_PROVIDER_TESTS` cannot contact a real provider. Ignored live-provider tests are not
invoked. The image job builds `gateway/Dockerfile` without baked credentials. `sslmode=disable` in
CI is local-only, not hosted TLS proof. Remote CI execution is not required for local acceptance.

## Project Structure

```text
opmux-backend/
├── common/             # Shared crate
├── gateway/            # Axum service crate
│   ├── src/            # Startup, core, middleware, and feature modules
│   └── tests/          # HTTP fixtures, persistence, observability, deferred live tests
├── supabase/           # Versioned SQL migrations (Supabase history, not SQLx)
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

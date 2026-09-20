# Gateway Operations Runbook

## Schema migrations

Application tables live in the private schema `opmux_private` (`clients`, `api_keys`). Digests are
SHA-256 `bytea` values; anonymous and Data-API roles cannot read them. Apply the Supabase migration
history explicitly:

```bash
bash scripts/with-owned-database.sh bash scripts/db-migrate.sh
bash scripts/with-owned-database.sh bash scripts/ci-setup-db.sh
```

Local tests and mission migrations must use `scripts/with-owned-database.sh`, which verifies the
owned loopback fixture and replaces inherited `DATABASE_URL` values. `scripts/ci-setup-db.sh`
prepares portable role stubs, applies the same history, and reapplies it as a no-op without
resetting retained rows. For operator/production Postgres, run `scripts/db-migrate.sh` with a chosen
`DATABASE_URL`; that path is not restricted to localhost. CI provisions its own disposable Postgres
17 and uses `scripts/ci-setup-db.sh` rather than this host. Do not enable SQLx migrators, hosted
project linking, or automatic migrate-on-boot for each replica. The local CI equivalent is
`bash scripts/ci-local.sh`.

Local privileges are separate:

- **Migration / database owner:** DDL through `scripts/db-migrate.sh`. This role is not the gateway
  runtime user.
- **Operator (`opmux_operator`):** DML on `clients` and `api_keys` (no DELETE). Used by
  `opmux-admin` via `SET ROLE` after connecting with `DATABASE_URL`. Override the role with
  `OPMUX_DB_ROLE` only when the connecting user is a member of that role.
- **Runtime (`opmux_runtime`):** `SELECT` clients; `SELECT`/`INSERT`/`UPDATE` keys. Cannot create
  tenants. Used by the gateway via `SET ROLE` after connecting with `DATABASE_URL`. Override with
  `OPMUX_RUNTIME_DB_ROLE` only when the connecting user is a member of that role.

## Operator provisioning

`opmux-admin` creates tenants and issues keys through the same generation/hashing service as
`POST /api/v1/auth/keys`. It is not an unauthenticated HTTP endpoint. Tenant creation inserts one
client and one management key atomically. Existing-client issuance adds one key of an explicit
`management` or `inference` kind. Authenticated managers may also issue keys for their own tenant
over HTTP; ownership cannot be taken from the request body. `GET /api/v1/auth/keys` lists only that
tenant's safe metadata (default/max 100, newest first). Continue with `offset` when `has_more` is
true. Query `client_id`/`tenant_id`, unknown parameters, and invalid `limit`/`offset`/`kind` return
400 and do not change inventory. `DELETE /api/v1/auth/keys/{id}` revokes a same-tenant key (204,
idempotent). Other-tenant and unknown IDs return indistinguishable 404 and do not change the target.

Successful commands print one JSON object to **stdout**, including the newly generated
`opmx_v1_<base64url>` credential exactly once. Write stdout to a fresh private file created with
`mktemp` (mode 0600 even under umask 022). Do not redirect onto an existing path or symlink. Do not
copy the secret into logs, tickets, or shell history. Failed commands print no credential and leave
no partial tenant/key row. Secrets cannot be retrieved later; issue a replacement key to recover.

HTTP key creation and revocation use the same protected-request deadline as inference. A `504` or
client disconnect during a management mutation does not prove rollback. If creation committed and
the one-time credential never reached the caller, the plaintext cannot be shown again. List the
tenant inventory; revoke an unexpected new key or issue a replacement with `opmux-admin`. Do not
expect commit/response reconciliation or recovery of the same secret.

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

Do not seed public mock keys (`test-api-key-123`, `dev-api-key-456`) into the database. HTTP request
authentication hashes the presented credential and looks up the digest in `opmux_private`. Last
successful authentication updates `last_used_at` before the request is admitted, including when
later inference fails. The timestamp is monotonic: concurrent authentications cannot move it
backward. Missing, unknown, and revoked credentials do not update it. Lookup and last-used share a
bounded database transaction with revocation so a commit cannot admit a key whose row is already
revoked. There is no authentication cache. Revocation is visible to subsequent authentication as
soon as DELETE commits, including other gateway processes that share the same database. Already
admitted work may finish; revocation does not promise cancellation of an in-flight provider call.

### Rotate a management key

Create the replacement, confirm it can list the tenant, then revoke the old key. Do not revoke
first.

```bash
replacement=$(mktemp "${TMPDIR:-/tmp}/opmux-key.XXXXXX")
chmod 600 "$replacement"
curl -sS -X POST http://127.0.0.1:3000/api/v1/auth/keys \
  -H "X-API-Key: $OLD_MANAGER" \
  -H "Content-Type: application/json" \
  -d '{"name":"replacement-manager","kind":"management"}' > "$replacement"
# Confirm the replacement can list keys, then revoke the old key with it:
curl -sS -X DELETE "http://127.0.0.1:3000/api/v1/auth/keys/$OLD_KEY_ID" \
  -H "X-API-Key: $REPLACEMENT_MANAGER"
```

Read the replacement credential from `$replacement` privately. Do not paste it into logs.

### Recover after last-manager revocation

Self-revocation and final-manager revocation are allowed. There is no last-manager prohibition. If
every management key for a client is revoked, HTTP management is unavailable until an operator
issues a replacement for that existing client. This does not create another tenant or revive revoked
keys.

```bash
recovered=$(mktemp "${TMPDIR:-/tmp}/opmux-key.XXXXXX")
chmod 600 "$recovered"
bash scripts/with-owned-database.sh cargo run -p gateway --bin opmux-admin -- \
  key issue --client-id "$CLIENT_ID" --kind management --name recovered-manager > "$recovered"
```

## Startup checks

1. Confirm required env vars are set (`OPMUX_CONFIG_FILE`, `OPENAI_API_KEY`, `DATABASE_URL`,
   `SERVER_PORT`). Copying `.env` is not process configuration. `AUTH_DEVELOPMENT_MODE` does not
   bypass authentication.
2. Start service and verify startup logs show initialized Executor/Health/Ingress services.
3. Validate endpoints:

```bash
curl -i http://127.0.0.1:3000/health
curl -i http://127.0.0.1:3000/ready
curl -i http://127.0.0.1:3000/metrics
```

An occupied `SERVER_HOST`/`SERVER_PORT` fails before serving. The process exits nonzero with
`bind_address_in_use` and does not replace or terminate the listener that already owns that address.

## Shutdown

SIGTERM and SIGINT (CTRL-C) start draining. `/ready` becomes `503` even if dependency success is
still cached. `/health` stays liveness-only. Newly handled `POST /api/v1/route` returns
`503 DRAINING` with zero provider calls. Already admitted generation may finish until
`SERVER_SHUTDOWN_TIMEOUT` (default 30 seconds, inclusive bounds 1–600). Grace expiry cancels owned
in-flight and retry-backoff work and starts no later attempt or fallback. The process then exits and
closes its listener. Cancellation does not reverse computation already started on a remote provider.

```bash
kill -TERM "$GATEWAY_PID"
```

Container runtimes should send SIGTERM on stop (`docker stop`). The image runs UID `65532` and
includes `gateway` and `opmux-admin`. Inject `DATABASE_URL`, `OPENAI_API_KEY`, and `OPENAI_BASE_URL`
at runtime; do not bake them into the image. `docker stop --time N` should be at least
`SERVER_SHUTDOWN_TIMEOUT`. The local stack wrapper uses a fixed 605-second Docker stop ceiling so it
cannot undercut the 1–600 second application maximum; a normal exit still returns immediately.
Publish host ports on loopback only.

```bash
docker build --file gateway/Dockerfile --tag opmux-gateway:mvp .
bash scripts/check-container.sh
CONTAINER_IMAGE=opmux-gateway:mvp bash scripts/local-stack.sh up
bash scripts/local-stack.sh bindings
bash scripts/check-local-stack.sh
bash scripts/local-stack.sh down
```

`scripts/check-container.sh` reuses the owned local Supabase, owned loopback simulators, and a
test-only untrusted TLS fixture. `scripts/local-stack.sh` is the documented Compose stack: gateway
`127.0.0.1:38080`, simulator `127.0.0.1:38081`, database `127.0.0.1:55432`. It does not start a
second database or publish all interfaces. Compose selects `CONTAINER_IMAGE` (default
`opmux-gateway:mvp`). Generated env is private per invocation and is not required for `down`;
existing `.env.opmux-local` files are left alone, project `.env` is not loaded, and the owned
database URL stays authoritative over inherited `OPMUX_LOCAL_DATABASE_URL`. Stop verifies
gateway/simulator Compose project, service, and repository labels, then removes only those owned
containers without `--force`. Application-stack stop/start keeps the owned database records. Plain
HTTP simulator URLs and `sslmode=disable` are local-only, not hosted or provider TLS proof.
Certificate and hostname verification stay enabled. Hosted guidance is a direct or session-mode URL
with `sslmode=verify-full`; do not mutate hosted projects from this repository.

`/metrics` is loopback-local on the gateway publish. Production must restrict scrape access at the
network layer; do not add a metrics authentication system.

## Incident triage

### Symptom: `/ready` returns `503`

- If `draining` is `true`, the process received SIGTERM or SIGINT. Stop sending generation; wait for
  the configured shutdown grace, then start a replacement process.
- `/health` staying `200` means the process is live; readiness is independent liveness.
- Inspect `dependencies.database`, `dependencies.upstream`, and `dependencies.default_route`.
  Messages are sanitized (`Authentication database unavailable`, `Upstream provider unreachable`,
  `No usable default-route target`) and omit SQL, URLs, and secrets.
- Database readiness requires `opmux_private.api_keys` schema, selected-column, locking, and
  `last_used_at` UPDATE access, not a socket ping or a write/trigger probe.
- Upstream readiness is `GET /models` reachability and credentials. It is not a generation call and
  does not prove a configured model can generate.
- Successful probes may stay cached for `HEALTH_CHECK_CACHE_TTL_SECS` (default 5 seconds). Failures
  are not cached; restoring a dependency is visible on the next completed probe.
- If `default_route` is unhealthy while `upstream` is healthy, every eligible default-route target
  is circuit-open. Cached `/models` success cannot override that. Recover through the normal
  generation path after `circuit_cooldown_ms`; readiness does not start generation probes.

### Symptom: `/api/v1/route` returns `504 DEADLINE_EXCEEDED`

- The protected-request deadline covers authentication, body receipt, and execution as one budget.
- Slow clients, delayed authentication, or a slow upstream can exhaust it; retries do not start
  after expiry.
- `/health` and `/metrics` are not gated by that deadline.
- Per-attempt timeout is the lesser of the configured attempt maximum and remaining time. The global
  actual-attempt budget is not reset on fallback. A `Retry-After` that cannot finish before the
  deadline returns `429 UPSTREAM_RATE_LIMIT` for the whole request, including configured fallbacks,
  not a false `504`, but only while the overall deadline has not actually elapsed. Actual expiry is
  always `504`, including when a ready 429 or saved `Retry-After` is already observed. Provider 429
  responses save header throttling and parsed `Retry-After` before a short bounded body refinement
  inside the remaining attempt budget. Complete `insufficient_quota` JSON is terminal quota;
  stalled, malformed, or oversized 429 bodies keep throttling. Throttling does not open circuits.

### Symptom: `/api/v1/route` returns `503 DRAINING`

- The process is shutting down and is not admitting new generation. Drain closes the generation
  limiter before `/ready` reports draining, so later acquires are `503 DRAINING`, not
  `429 OVERLOADED`.
- In-flight work that already holds a permit may still complete until `SERVER_SHUTDOWN_TIMEOUT`.
  Releasing those permits does not reopen admission.
- Send new generation to a replacement process after it becomes ready.

### Symptom: `/api/v1/route` returns `503 circuit_open`

- Consecutive eligible transient failures opened every usable target on the route.
- Circuits are target-scoped: one model can be open while another same-provider target still works.
- Wait for the configured cooldown, then a single half-open probe may recover that target. Late
  completions from earlier admissions do not close or reopen a newer circuit state.
- Investigate upstream network/timeout conditions for the failing target. Permanent credential,
  quota, and throttling failures do not open circuits.

### Symptom: `/api/v1/route` returns `429 OVERLOADED`

- Generation concurrency reached `max_concurrent_generations`. The extra request is rejected
  immediately; it is not queued behind in-flight retries or fallbacks.
- The response includes `Retry-After: 1`. This is local admission, not `UPSTREAM_RATE_LIMIT`.
- `/health`, `/ready`, and `/metrics` must remain usable while generation slots are occupied.
- Capacity returns when an admitted request finishes, fails, times out, or is cancelled.

### Symptom: increased latency

- Inspect metrics endpoint for request counters/latency trends.
- Lower load and run `scripts/run-load-tests.sh` to reproduce under controlled concurrency.

## Monitoring guide

- Use `/metrics` as the scrape endpoint. It has no application authentication. Keep it loopback-only
  locally and network-restricted in production.
- Track at minimum:
  - request volume and failure ratio (`gateway_http_requests_total`),
  - readiness status transitions,
  - latency and pending request trends,
  - execution attempts/outcomes, retries, and fallbacks,
  - `gateway_circuit_state` / `gateway_circuit_transitions_total`,
  - `gateway_deadline_exceeded_total` and `gateway_overload_rejected_total`,
  - successful prompt/completion token counts (successful responses only).
- Local overload (`OVERLOADED`) is `gateway_overload_rejected_total`. Provider throttling is
  `gateway_execution_attempts_total{outcome="rate_limit"}`.
- Metric labels are bounded. Do not expect tenant, key, request, or model-name series. See
  [PROMETHEUS.md](PROMETHEUS.md).

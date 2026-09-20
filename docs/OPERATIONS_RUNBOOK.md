# Gateway Operations Runbook

## Schema migrations

Application tables live in the private schema `opmux_private` (`clients`, `api_keys`). Digests are
SHA-256 `bytea` values; anonymous and Data-API roles cannot read them. Apply the Supabase migration
history explicitly:

```bash
bash scripts/with-owned-database.sh bash scripts/db-migrate.sh
```

Local tests and mission migrations must use `scripts/with-owned-database.sh`, which verifies the
owned loopback fixture and replaces inherited `DATABASE_URL` values. For operator/production
Postgres, run `scripts/db-migrate.sh` with a chosen `DATABASE_URL`; that path is not restricted to
localhost. Do not enable SQLx migrators, hosted project linking, or automatic migrate-on-boot for
each replica.

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

## Incident triage

### Symptom: `/ready` returns `503`

- Check dependency details in readiness response (`dependencies.error`, `healthy_vendors`).
- Validate vendor credentials and upstream endpoint connectivity.
- Confirm circuit breaker behavior via repeated `/api/v1/route` calls.

### Symptom: `/api/v1/route` returns `504 DEADLINE_EXCEEDED`

- The protected-request deadline covers authentication, body receipt, and execution as one budget.
- Slow clients, delayed authentication, or a slow upstream can exhaust it; retries do not start
  after expiry.
- `/health` and `/metrics` are not gated by that deadline.
- Per-attempt timeout is the lesser of the configured attempt maximum and remaining time. The global
  actual-attempt budget is not reset on fallback. A `Retry-After` that cannot finish before the
  deadline returns `429 UPSTREAM_RATE_LIMIT` for the whole request, including configured fallbacks,
  not a false `504`. Provider 429 responses are classified from headers without waiting for an
  unused error body.

### Symptom: `/api/v1/route` returns `503 circuit_open`

- Consecutive eligible transient failures opened every usable target on the route.
- Circuits are target-scoped: one model can be open while another same-provider target still works.
- Wait for the configured cooldown, then a single half-open probe may recover that target.
- Investigate upstream network/timeout conditions for the failing target. Permanent credential or
  quota failures do not open circuits.

### Symptom: increased latency

- Inspect metrics endpoint for request counters/latency trends.
- Lower load and run `scripts/run-load-tests.sh` to reproduce under controlled concurrency.

## Monitoring guide

- Use `/metrics` as scrape endpoint.
- Track at minimum:
  - request volume and failure ratio,
  - readiness status transitions,
  - latency and pending request trends,
  - circuit-open error frequency.

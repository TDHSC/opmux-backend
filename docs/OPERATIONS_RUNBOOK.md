# Gateway Operations Runbook

## Schema migrations

Application tables live in the private schema `opmux_private` (`clients`, `api_keys`). Digests are
SHA-256 `bytea` values; anonymous and Data-API roles cannot read them. Apply the Supabase migration
history explicitly:

```bash
bash scripts/db-migrate.sh
```

Use `DATABASE_URL` for a chosen Postgres, or omit it to target the owned local database at
`127.0.0.1:55432`. Do not enable SQLx migrators, hosted project linking, or automatic
migrate-on-boot for each replica.

Local privileges are separate:

- **Migration / database owner:** DDL through `scripts/db-migrate.sh`. This role is not the gateway
  runtime user.
- **Operator (`opmux_operator`):** DML on `clients` and `api_keys` (no DELETE). Used by
  `opmux-admin` via `SET ROLE` after connecting with `DATABASE_URL`. Override the role with
  `OPMUX_DB_ROLE` only when the connecting user is a member of that role.
- **Runtime (`opmux_runtime`):** `SELECT` clients; `SELECT`/`INSERT`/`UPDATE` keys. Cannot create
  tenants. Intended for the gateway process after persisted authentication is wired.

## Operator provisioning

`opmux-admin` creates tenants and issues keys through the same generation/hashing service the HTTP
API will use. It is not an unauthenticated HTTP endpoint. Tenant creation inserts one client and one
management key atomically. Existing-client issuance adds one key of an explicit `management` or
`inference` kind.

Successful commands print one JSON object to **stdout**, including the newly generated
`opmx_v1_<base64url>` credential exactly once. Redirect stdout to a mode-0600 file. Do not copy the
secret into logs, tickets, or shell history. Failed commands print no credential and leave no
partial tenant/key row. Secrets cannot be retrieved later; issue a replacement key to recover.

```bash
bash scripts/with-owned-database.sh cargo run -p gateway --bin opmux-admin -- \
  tenant create --name acme > /tmp/acme-management.json
bash scripts/with-owned-database.sh cargo run -p gateway --bin opmux-admin -- \
  key issue --client-id "$CLIENT_ID" --kind inference --name route > /tmp/acme-inference.json
```

Do not seed public mock keys (`test-api-key-123`, `dev-api-key-456`) into the database. HTTP request
authentication is still mock-backed until the next persistence wiring step.

## Startup checks

1. Confirm required env vars are set (`OPMUX_CONFIG_FILE`, `OPENAI_API_KEY`, `SERVER_PORT`, auth
   settings). Copying `.env` is not process configuration. `DATABASE_URL` is required for
   persistence tests, not yet for gateway bind.
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

### Symptom: `/api/v1/route` returns `503 circuit_open`

- This indicates repeated transient failures for a vendor.
- Wait for breaker cool-down window, then retry.
- Investigate upstream network/timeout/rate-limit conditions.

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

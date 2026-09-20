# Configuration and Troubleshooting

The gateway reads **process environment** plus a non-secret JSON catalog selected by
`OPMUX_CONFIG_FILE`. Copying `.env` is not configuration. Credentials and database URLs stay in the
environment; the catalog is not a secret store. `DATABASE_URL` is required for gateway bind after
catalog load, persistence tests, `opmux-admin`, and migration apply. `AUTH_DEVELOPMENT_MODE` does
not bypass authentication.

Example catalog prices and model names are illustrative samples. They are not current provider
billing or model-availability facts. Distinct target identifiers may request the same provider model
and still keep their own prices. Successful-response `cost` is estimated from the selected target's
configured `input_per_million` and `output_per_million` prices and the validated provider usage:
`(prompt_tokens * input_per_million + completion_tokens * output_per_million) / 1_000_000`, rounded
to 8 decimal places. Illustrative prices of `1.0` and `2.0` with 120 prompt and 30 completion tokens
yield `0.00018`. Missing prices fail rather than reporting zero. This estimate covers the successful
response only; it is not a bill and does not total retries or abandoned work.

## Canonical catalog schema

Version `1` only. Unknown fields, duplicate identifiers in the raw JSON, nested/recursive route
plans, and unsupported vendors are rejected before bind.

```json
{
  "version": 1,
  "default_route": "default",
  "targets": {
    "primary": {
      "vendor": "openai",
      "model": "example-chat-model",
      "max_output_tokens": 512,
      "pricing": { "input_per_million": 1.0, "output_per_million": 2.0 }
    }
  },
  "routes": {
    "default": { "primary": "primary", "fallbacks": [] }
  }
}
```

- `vendor` is optional and defaults to `openai`. No other vendor is accepted.
- `fallbacks` is a flat list of target identifiers. Nested route objects are invalid.
- Finite zero prices are valid. Negative, NaN/Inf, and overflowing prices are not.
- See [config/opmux.example.json](../config/opmux.example.json) for a default route with one
  fallback and a named `fast` route with a distinct primary model.
- Clients may send optional `route`, `allow_fallback`, and `parameters` on `POST /api/v1/route`.
  Omitted `route` uses `default_route`. Omitted `allow_fallback` follows the configured chain;
  `false` keeps the primary target and its retries. Clients cannot choose a vendor, model, or URL.
- `parameters.temperature` is a JSON number in `0.0`–`2.0`. `parameters.top_p` is a JSON number in
  `0.0`–`1.0`. `parameters.max_tokens` is a positive JSON integer no greater than the selected
  primary target's `max_output_tokens`. Omitted temperature/top_p are omitted upstream; omitted
  `max_tokens` is replaced by that target cap. Values are not clamped.
- Prompt length uses the original untrimmed Unicode scalar count and UTF-8 byte length. Both are
  capped by `max_prompt_chars` (default 4000). Whitespace-only prompts are rejected.

Optional `limits` may appear on the catalog object. Omitted fields use the defaults below.
Environment overrides are validated with the same bounds. Canonical `OPMUX_*` variables win over
compatible `OPENAI_TIMEOUT_MS` / `EXECUTOR_*` names.

These limits are validated and injected now. `max_prompt_chars` and per-target `max_output_tokens`
are enforced on `POST /api/v1/route`. `max_upstream_response_bytes` is enforced while accumulating
the provider response, including when `Content-Length` is missing or chunked.
`protected_request_deadline_ms` is one monotonic budget for authentication, body extraction, and
execution; later layers receive remaining time rather than a reset timeout. Expiry returns
`504 DEADLINE_EXCEEDED` and does not start additional provider attempts. `/health` and `/metrics`
are outside that deadline. Per-attempt timeout is `min(max_attempt_timeout_ms, remaining deadline)`.
`retries_per_target` bounds extra calls on one target after the first; `max_total_attempts` bounds
actual started provider calls across the whole request and does not replenish when execution moves
to a fallback. Exponential backoff uses full jitter (`0..=min(1000 * 2^(retry-1), backoff_cap_ms)`),
default cap 2000ms. Sleeps consume the overall deadline. A valid provider `Retry-After`
(delta-seconds or HTTP-date) is the minimum wait and is not shortened by the backoff cap; if that
wait cannot finish in the remaining time, the gateway returns `429 UPSTREAM_RATE_LIMIT` without a
later attempt or configured fallback call and without claiming deadline expiry. Provider 429
responses save header throttling and parsed `Retry-After` before a bounded body refinement. Complete
JSON whose `error.code` or `error.type` is `insufficient_quota` is terminal quota with no retry or
fallback. Stalled, failed, malformed, or oversized 429 bodies keep the saved throttling. Malformed
`Retry-After` values use the capped jitter, not an unbounded sleep. Eligible configured fallback
switching is enforced in catalog order under that shared budget: transient transport, attempt
timeout, and provider 5xx may continue to a later target; client, protocol, shared-credential,
quota, and same-account throttling errors do not switch models. Throttling may retry the current hop
but does not open or reopen circuits. A fallback whose output-token cap cannot satisfy the
already-validated request is skipped without rewriting parameters. Target-scoped circuits open after
`circuit_failure_threshold` consecutive eligible hop failures and skip that target for
`circuit_cooldown_ms` without consuming an attempt. A healthy same-provider fallback remains usable.
After cooldown, at most one half-open probe is admitted per target; success closes the circuit and
failure reopens it. Each closed admission and probe carries a generation/phase token; only the
current owner can change circuit state. A late success or failure is ignored for circuit accounting
but the request result is still returned. Permanent credential, quota, protocol, rejection, and
throttling errors do not open circuits. Concurrency admission and inbound raw-body enforcement are
later milestones.

| Setting                          | Type    | Unit                       | Default | Min | Max      |
| -------------------------------- | ------- | -------------------------- | ------- | --- | -------- |
| `protected_request_deadline_ms`  | integer | milliseconds               | 30000   | 1   | 300000   |
| `max_attempt_timeout_ms`         | integer | milliseconds               | 10000   | 1   | 120000   |
| `retries_per_target`             | integer | count                      | 1       | 0   | 8        |
| `max_total_attempts`             | integer | count                      | 3       | 1   | 16       |
| `max_fallback_targets`           | integer | count                      | 2       | 0   | 8        |
| `backoff_cap_ms`                 | integer | milliseconds               | 2000    | 1   | 60000    |
| `circuit_failure_threshold`      | integer | count                      | 3       | 1   | 100      |
| `circuit_cooldown_ms`            | integer | milliseconds               | 30000   | 1   | 600000   |
| `max_concurrent_generations`     | integer | count                      | 32      | 1   | 1024     |
| `max_request_body_bytes`         | integer | bytes                      | 1048576 | 1   | 16777216 |
| `max_metadata_bytes`             | integer | bytes                      | 1000    | 1   | 1048576  |
| `max_prompt_chars`               | integer | characters and UTF-8 bytes | 4000    | 1   | 1000000  |
| `max_upstream_response_bytes`    | integer | bytes                      | 1048576 | 1   | 16777216 |
| `max_output_tokens` (per target) | integer | tokens                     | n/a     | 1   | 1000000  |

Zero retries means no extra attempts after the first call; it is not the same as zero actual
attempts (`max_total_attempts` minimum is 1). Fractional integers are rejected.

## Core environment variables

Catalog:

- `OPMUX_CONFIG_FILE` (required) path to the version-1 JSON catalog

Server:

- `SERVER_HOST` (default `0.0.0.0`)
- `SERVER_PORT` (default `3000`)
- `SERVER_SHUTDOWN_TIMEOUT` (default `30` seconds)

Auth:

- `AUTH_DEVELOPMENT_MODE` (ignored; persisted authentication is always required)
- `AUTH_DEV_CLIENT_ID` (ignored; identity comes from the authenticated key)

Provider (environment-only):

- `OPENAI_API_KEY` (required, nonempty)
- `OPENAI_BASE_URL` (default `https://api.openai.com/v1`; http or https path-prefix URL with no
  userinfo, query, or fragment, including empty `?`/`#`; trailing slashes are stripped)
- `OPENAI_TIMEOUT_MS` (optional override of `max_attempt_timeout_ms`)

Compatible executor overrides:

- `EXECUTOR_MAX_RETRIES` (optional override of `retries_per_target`)
- `EXECUTOR_TIMEOUT_MS` (optional override of `max_attempt_timeout_ms`)

Canonical limit overrides use an `OPMUX_` prefix plus the catalog field name, for example
`OPMUX_PROTECTED_REQUEST_DEADLINE_MS`.

Observability/performance:

- `RUST_LOG` (default `info`; overrides legacy `LOG_LEVEL`)
- `LOG_FORMAT` (`json` by default, or `pretty`; overrides legacy `LOG_JSON`)
- `LOG_VERBOSE_DEBUG` (default `false`; adds line numbers and thread IDs)
- `METRICS_ENABLED` (default `true`)
- `METRICS_PATH` (default `/metrics`)
- `HEALTH_CHECK_TIMEOUT` (default `2`)
- `HEALTH_CHECK_CACHE_TTL_SECS` (default `5`)
- `INGRESS_SLOW_REQUEST_THRESHOLD_MS` (default `1000`)

TLS certificate and hostname verification stay enabled. There is no insecure-TLS setting.

Persistence (SQLx 0.8.6, required at gateway bind):

- `DATABASE_URL` (PostgreSQL URL; required for the gateway, `opmux-admin`, migrations, and
  persistence tests)
- `OPMUX_RUNTIME_DB_ROLE` (optional, default `opmux_runtime` for the gateway; connecting user must
  be a member of that role)
- `OPMUX_DB_ROLE` (optional, default `opmux_operator` for `opmux-admin`; connecting user must be a
  member of that role)
- `OPMUX_DB_MAX_CONNECTIONS` (optional, default 10, max 32)
- `OPMUX_DB_ACQUIRE_TIMEOUT_MS` (optional, default 3000, 100–60000)
- `OPMUX_DB_STATEMENT_TIMEOUT_MS` (optional, default 5000, 100–60000)

Local loopback may use `sslmode=disable`. Hosted connections should use a direct or session-mode
pooler URL with `sslmode=verify-full`. Transaction-mode poolers are not supported. Apply local
schema with `bash scripts/with-owned-database.sh bash scripts/db-migrate.sh`. Operator/production
migrations may set `DATABASE_URL` and run `scripts/db-migrate.sh` without the wrapper; that path is
not localhost-restricted. Do not migrate from every replica and do not `supabase start` a second
database from this repository.

## Troubleshooting quick reference

### Startup fails before the process listens

Cause: missing `OPMUX_CONFIG_FILE`, unreadable or invalid catalog, blank `OPENAI_API_KEY`, invalid
`OPENAI_BASE_URL`, missing/invalid `DATABASE_URL`, out-of-range limits, or HTTP client construction
failure. Diagnostics include a stable category such as `catalog_duplicate_target`,
`missing_credential`, or `missing_database_url` and omit keys, URLs with userinfo, query, or
fragment, connection strings, and full configuration dumps.

Action: point `OPMUX_CONFIG_FILE` at a valid version-1 catalog, export a nonempty provider key, and
use a loopback URL for local checks. The binary does not automatically load `.env`.

### Startup fails with vendor config error

Cause: no vendor is configured, usually because `OPENAI_API_KEY` is absent. Startup does not
validate upstream credentials or connectivity; `/ready` checks those after the process starts.

Action:

```bash
export OPMUX_CONFIG_FILE="$PWD/config/opmux.example.json"
export OPENAI_API_KEY=your-key
export OPENAI_BASE_URL=https://api.openai.com/v1
cargo run -p gateway
```

For a check without real credentials, use the local failure-simulation configuration in
[README.md](../README.md#local-startup-check-no-real-llm-calls). HTTP integration tests start an
owned loopback OpenAI simulator and the shared production router; they do not use inherited provider
keys. Live-provider tests are ignored and unrun unless explicitly opted in.

### `/api/v1/route` returns `401`

Cause: missing, empty, duplicate, comma-joined, unknown, or revoked `X-API-Key`. Former public mock
keys are also rejected. `AUTH_DEVELOPMENT_MODE` does not grant access.

Action: provision a tenant and inference key with `opmux-admin` and send that one-time credential in
`X-API-Key`.

### `/api/v1/route` returns `503` without a circuit-open body

Cause: authentication datastore timeout or unavailability. The process can stay live; protected
requests fail closed and do not call the provider.

Action: confirm `DATABASE_URL`, schema (`scripts/db-migrate.sh`), and that the runtime role can
select `opmux_private.api_keys`.

### `/api/v1/route` returns `500 execution_failed`

Cause: upstream execution failed.

Action: inspect upstream availability, timeout, and base URL settings.

### `/api/v1/route` returns `503 circuit_open`

Cause: every eligible target on the route is circuit-open after consecutive transient hop failures.

Action: wait `circuit_cooldown_ms`, then retry. A single half-open probe recovers a healthy target.
Another same-provider model can remain usable while one target is open. Permanent credential or
quota, or throttling failures do not open circuits.

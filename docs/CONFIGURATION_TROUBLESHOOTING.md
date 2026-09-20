# Configuration and Troubleshooting

The gateway reads **process environment** plus a non-secret JSON catalog selected by
`OPMUX_CONFIG_FILE`. Copying `.env` is not configuration. Credentials and database URLs stay in the
environment; the catalog is not a secret store. `DATABASE_URL` is required for persistence tests and
migration apply. Gateway startup still uses mock authentication and does not yet require a database
URL.

Example catalog prices and model names are illustrative samples. They are not current provider
billing or model-availability facts. Estimated cost for a successful response is calculated later
from these configured prices.

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
  fallback.

Optional `limits` may appear on the catalog object. Omitted fields use the defaults below.
Environment overrides are validated with the same bounds. Canonical `OPMUX_*` variables win over
compatible `OPENAI_TIMEOUT_MS` / `EXECUTOR_*` names.

These limits are validated and injected now. Protected-request deadline, fallback execution, target
circuits, concurrency admission, and raw-size enforcement are later milestones.

| Setting                          | Type    | Unit         | Default | Min | Max      |
| -------------------------------- | ------- | ------------ | ------- | --- | -------- |
| `protected_request_deadline_ms`  | integer | milliseconds | 30000   | 1   | 300000   |
| `max_attempt_timeout_ms`         | integer | milliseconds | 10000   | 1   | 120000   |
| `retries_per_target`             | integer | count        | 1       | 0   | 8        |
| `max_total_attempts`             | integer | count        | 3       | 1   | 16       |
| `max_fallback_targets`           | integer | count        | 2       | 0   | 8        |
| `backoff_cap_ms`                 | integer | milliseconds | 2000    | 1   | 60000    |
| `circuit_failure_threshold`      | integer | count        | 3       | 1   | 100      |
| `circuit_cooldown_ms`            | integer | milliseconds | 30000   | 1   | 600000   |
| `max_concurrent_generations`     | integer | count        | 32      | 1   | 1024     |
| `max_request_body_bytes`         | integer | bytes        | 1048576 | 1   | 16777216 |
| `max_metadata_bytes`             | integer | bytes        | 1000    | 1   | 1048576  |
| `max_prompt_chars`               | integer | characters   | 4000    | 1   | 1000000  |
| `max_upstream_response_bytes`    | integer | bytes        | 1048576 | 1   | 16777216 |
| `max_output_tokens` (per target) | integer | tokens       | n/a     | 1   | 1000000  |

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

- `AUTH_DEVELOPMENT_MODE` (default `false`)
- `AUTH_DEV_CLIENT_ID` (default `dev-client-123`)

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

Persistence (SQLx 0.8.6, not required at gateway bind yet):

- `DATABASE_URL` (PostgreSQL URL; persistence tests fail if unset or unreachable)
- `OPMUX_DB_MAX_CONNECTIONS` (optional, default 10, max 32)
- `OPMUX_DB_ACQUIRE_TIMEOUT_MS` (optional, default 3000, 100–60000)
- `OPMUX_DB_STATEMENT_TIMEOUT_MS` (optional, default 5000, 100–60000)

Local loopback may use `sslmode=disable`. Hosted connections should use a direct or session-mode
pooler URL with `sslmode=verify-full`. Transaction-mode poolers are not supported. Apply schema with
`bash scripts/db-migrate.sh`; do not migrate from every replica and do not `supabase start` a second
database from this repository.

## Troubleshooting quick reference

### Startup fails before the process listens

Cause: missing `OPMUX_CONFIG_FILE`, unreadable or invalid catalog, blank `OPENAI_API_KEY`, invalid
`OPENAI_BASE_URL`, out-of-range limits, or HTTP client construction failure. Diagnostics include a
stable category such as `catalog_duplicate_target` or `missing_credential` and omit keys, URLs with
userinfo, query, or fragment, and full configuration dumps.

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

Cause: missing or invalid `X-API-Key` in production mode.

Action: send `X-API-Key: test-api-key-123` for local default flow.

### `/api/v1/route` returns `500 execution_failed`

Cause: upstream execution failed.

Action: inspect upstream availability, timeout, and base URL settings.

### `/api/v1/route` returns `503 circuit_open`

Cause: consecutive transient failures tripped circuit breaker.

Action: wait cooldown window and recover upstream connectivity.

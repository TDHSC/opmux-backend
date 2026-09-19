# Configuration and Troubleshooting

## Core environment variables

Server:

- `SERVER_HOST` (default `0.0.0.0`)
- `SERVER_PORT` (default `3000`)

Auth:

- `AUTH_DEVELOPMENT_MODE` (default `false`)
- `AUTH_DEV_CLIENT_ID` (default `dev-client-123`)

Executor:

- `OPENAI_API_KEY` (required for real upstream calls)
- `OPENAI_BASE_URL` (default `https://api.openai.com/v1`)
- `OPENAI_TIMEOUT_MS` (default `30000`)
- `EXECUTOR_MAX_RETRIES` (default `3`)

Observability/performance:

- `RUST_LOG` (default `info`; overrides legacy `LOG_LEVEL`)
- `LOG_FORMAT` (`json` by default, or `pretty`; overrides legacy `LOG_JSON`)
- `LOG_VERBOSE_DEBUG` (default `false`; adds line numbers and thread IDs)
- `METRICS_ENABLED` (default `true`)
- `METRICS_PATH` (default `/metrics`)
- `HEALTH_CHECK_TIMEOUT` (default `2`)
- `HEALTH_CHECK_CACHE_TTL_SECS` (default `5`)
- `INGRESS_SLOW_REQUEST_THRESHOLD_MS` (default `1000`)

## Troubleshooting quick reference

### Startup fails with vendor config error

Cause: no vendor is configured, usually because `OPENAI_API_KEY` is absent. Startup does not
validate upstream credentials or connectivity; `/ready` checks those after the process starts.

Action:

```bash
export OPENAI_API_KEY=your-key
export OPENAI_BASE_URL=https://api.openai.com/v1
cargo run -p gateway
```

For a check without real credentials, use the local failure-simulation configuration in
[README.md](../README.md#local-startup-check-no-real-llm-calls). The binary does not automatically
load `.env`.

### `/api/v1/route` returns `401`

Cause: missing or invalid `X-API-Key` in production mode.

Action: send `X-API-Key: test-api-key-123` for local default flow.

### `/api/v1/route` returns `500 execution_failed`

Cause: upstream execution failed.

Action: inspect upstream availability, timeout, and base URL settings.

### `/api/v1/route` returns `503 circuit_open`

Cause: consecutive transient failures tripped circuit breaker.

Action: wait cooldown window and recover upstream connectivity.

# Observability Manual Testing Guide

This guide verifies correlation IDs, health/readiness, and Prometheus metrics against the shipped
API. OpenAI is **SIMULATED ONLY**. Live-provider and hosted checks are deferred and unrun.

## Prerequisites

- Run from repo root
- Owned local Supabase (`scripts/with-owned-database.sh`)
- A dummy provider key. Do not use former public mock gateway keys.

Two local setups:

1. **Startup/failure simulation** (no simulator): `OPENAI_BASE_URL=http://127.0.0.1:9/v1` yields
   `/health` 200 and `/ready` 503. Authenticated generation returns `502 UPSTREAM_ERROR`, not `500`.
2. **Working local stack** (recommended for generation/metrics deltas):
   `bash scripts/local-stack.sh up` publishes gateway `127.0.0.1:38080` and simulator
   `127.0.0.1:38081`. `/ready` is 200 when the database and simulator are healthy.

Copying `.env` is not process configuration. `AUTH_DEVELOPMENT_MODE` does not bypass authentication.

```bash
export OPMUX_CONFIG_FILE="$PWD/config/opmux.example.json"
export OPENAI_API_KEY=dummy-key
export OPENAI_BASE_URL=http://127.0.0.1:9/v1
export AUTH_DEVELOPMENT_MODE=false
bash scripts/with-owned-database.sh cargo run -p gateway
```

Native default listen address is `127.0.0.1:3000` only if you also export
`SERVER_HOST`/`SERVER_PORT` as in [README.md](../../README.md). Unset, the binary defaults to
`0.0.0.0:3000`. Documented local commands set `SERVER_HOST=127.0.0.1`.

## 1) Correlation ID propagation

```bash
curl --noproxy '*' -i "http://127.0.0.1:3000/health" \
  -H "X-Correlation-ID: manual-corr-001"
```

Expected:

- `X-Request-ID` exists in response headers
- `X-Correlation-ID: manual-corr-001` echoed in response headers
- With `RUST_LOG=gateway=debug LOG_FORMAT=json`, request-scoped lines include the same `request_id`
  (and the client correlation when provided). Auth, input, overload, and upstream failures keep that
  correlation. Fresh debug output must omit credentials, prompts, metadata, SQL, and provider
  bodies. Authentication `auth_duration_ms` must not grow by a delayed simulator response.

## 2) Health endpoint response

```bash
curl --noproxy '*' -i "http://127.0.0.1:3000/health"
```

Expected:

- `HTTP/1.1 200 OK`
- JSON fields: `status`, `timestamp`, `version`, `uptime_seconds`

There is no `HEALTH_CHECK_MODE`. `/health` is liveness only.

## 3) Readiness endpoint response

```bash
curl --noproxy '*' -i "http://127.0.0.1:3000/ready"
```

Expected with dummy unreachable upstream (`127.0.0.1:9`):

- `HTTP/1.1 503 Service Unavailable`
- JSON fields: `status: not_ready`, dependency details under `dependencies`

Expected with the local stack simulator healthy: `200` and `"status":"ready"`. Successful probes
cache for `HEALTH_CHECK_CACHE_TTL_SECS` (default **5** seconds). Failures are never cached.

## 4) Metrics endpoint accessibility

```bash
curl --noproxy '*' -i "http://127.0.0.1:3000/metrics"
```

Expected:

- `HTTP/1.1 200 OK`
- Prometheus payload includes `gateway_http_requests_total`
- Payload may include `gateway_execution_attempts_total`, retry/fallback/circuit/deadline/overload
  and successful-usage series after generation traffic
- Labels are route templates and accepted catalog target IDs exported verbatim, not request IDs, key
  UUIDs, URLs, or model names
- `X-Request-ID` exists in response headers
- Treat `/metrics` as internal: loopback locally, network-restrict in production

## 5) Protected ingress route behavior

Provision an inference key with `opmux-admin` first. Former public mock keys such as
`test-api-key-123` return `401` and do not reach the executor.

```bash
curl --noproxy '*' -i -X POST "http://127.0.0.1:3000/api/v1/route" \
  -H "Content-Type: application/json" \
  -H "X-API-Key: $INFERENCE_KEY" \
  -H "X-Correlation-ID: manual-corr-002" \
  -d '{"prompt":"hello","metadata":{}}'
```

Expected with dummy unreachable upstream and an operator-provisioned inference key:

- `HTTP/1.1 502 Bad Gateway`
- JSON envelope `{"error":{"code":"UPSTREAM_ERROR","message":"...","request_id":"..."}}`
- `X-Correlation-ID: manual-corr-002` preserved in response

Expected against the local simulator: `200` with `response.content`, provider-reported `model_used`,
and configured estimated `cost`. That success is simulated, not live OpenAI.

## 6) Startup logging configuration

Run with `LOG_FORMAT=json RUST_LOG=info` and confirm each log line is JSON. Restart with
`LOG_FORMAT=pretty` for readable text. `LOG_VERBOSE_DEBUG=true` adds line numbers and thread IDs.
The startup summary must agree with the effective log format and filter.

For the automated local-only smoke check, build the binary and run:

```bash
cargo build -p gateway
bash scripts/with-owned-database.sh bash scripts/check-startup.sh "$PWD/target/debug/gateway"
```

This starts and stops its own gateway; stop any instance already using port 3000 or set
`STARTUP_CHECK_PORT` to an unused port.

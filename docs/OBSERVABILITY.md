# Observability Guide

This service provides correlation IDs, structured logs, health/readiness checks, and Prometheus
metrics.

## Correlation IDs

- Incoming `X-Correlation-ID` is validated and echoed back when present. Empty, overlong (>256
  bytes), and non-UTF-8 values are dropped rather than reflected.
- Service always generates `X-Request-ID`. Protected error bodies copy that value to
  `error.request_id`.
- Correlation middleware opens a root `http_request` span **before authentication**. Early auth,
  input, overload, and later execution failures inherit `request_id` and a validated client
  correlation ID. A request without `X-Correlation-ID` still receives `X-Request-ID`.

## Logging

- `RUST_LOG` controls filtering, including module-level filters (default `info`).
- `LOG_FORMAT=json` enables structured JSON (the default); use `pretty` for local readability.
- `LOG_VERBOSE_DEBUG=true` adds source line numbers and thread IDs.
- Legacy `LOG_LEVEL` and `LOG_JSON` remain fallbacks. `RUST_LOG` takes precedence over `LOG_LEVEL`;
  `LOG_FORMAT` takes precedence over `LOG_JSON`. With only `LOG_JSON` set, `true` selects JSON and
  `false` selects pretty output.
- The startup configuration summary reports the effective logging settings.
- Request-scoped logs omit credentials, digests, prompts, metadata, connection strings,
  authorization values, raw SQL, provider bodies, and credential-bearing URLs. Public error traces
  keep `request_id` and a stable `error.code`. Terminal failures are logged once at the HTTP
  envelope; authentication records `auth_duration_ms` and then ends before downstream work.
- Authentication duration includes header validation, database lookup, and last-used update. A slow
  upstream response increases execution time, not authentication time.

## Endpoints

- `GET /health` - liveness/status payload
- `GET /ready` - readiness with dependency health
- `GET /metrics` - Prometheus metrics payload (when enabled). Internal scrape surface: no
  application auth; restrict it at the network layer in production. HTTP counts/duration include
  auth failures. Execution series cover attempts, retries, fallback, circuit state/transitions,
  deadlines, local overload, and successful usage. Labels are bounded route templates, configured
  target IDs, and finite outcome classes. See [PROMETHEUS.md](PROMETHEUS.md).

## Local verification

Use the manual guide in `gateway/tests/OBSERVABILITY_TESTING.md`.

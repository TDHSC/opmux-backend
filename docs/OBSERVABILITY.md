# Observability Guide

This service provides correlation IDs, structured logs, health/readiness checks, and Prometheus
metrics.

## Correlation IDs

- Incoming `X-Correlation-ID` is validated and echoed back when present.
- Service always generates `X-Request-ID`.
- Correlation context is injected by middleware and available to handlers/services.

## Logging

- `RUST_LOG` controls filtering, including module-level filters (default `info`).
- `LOG_FORMAT=json` enables structured JSON (the default); use `pretty` for local readability.
- `LOG_VERBOSE_DEBUG=true` adds source line numbers and thread IDs.
- Legacy `LOG_LEVEL` and `LOG_JSON` remain fallbacks. `RUST_LOG` takes precedence over `LOG_LEVEL`;
  `LOG_FORMAT` takes precedence over `LOG_JSON`. With only `LOG_JSON` set, `true` selects JSON and
  `false` selects pretty output.
- The startup configuration summary reports the effective logging settings.

## Endpoints

- `GET /health` - liveness/status payload
- `GET /ready` - readiness with dependency health
- `GET /metrics` - Prometheus metrics payload (when enabled)

## Local verification

Use the manual guide in `gateway/tests/OBSERVABILITY_TESTING.md`.

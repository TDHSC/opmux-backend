# Gateway API Reference

Base URL: `http://<host>:3000`

## GET /

Simple service banner endpoint.

- Auth: none
- Response: `200 OK`, HTML body

## GET /health

Liveness endpoint.

- Auth: none
- Response: `200 OK`
- Body fields: `status`, `timestamp`, `version`, `uptime_seconds`

## GET /ready

Readiness endpoint including dependency checks.

- Auth: none
- Response:
  - `200 OK` when dependencies are healthy
  - `503 Service Unavailable` when dependencies are unhealthy

Example response (`503`):

```json
{
  "status": "not_ready",
  "timestamp": "2026-03-13T00:00:00Z",
  "dependencies": {
    "status": "unhealthy",
    "vendor_count": 1,
    "healthy_vendors": 0,
    "latency_ms": 12,
    "error": "..."
  }
}
```

## GET /metrics

Prometheus metrics endpoint.

- Auth: none
- Controlled by `METRICS_ENABLED` and `METRICS_PATH`
- Response: `200 OK` when enabled

## POST /api/v1/route

Protected AI routing endpoint.

- Auth: required (`X-API-Key` header with an operator-provisioned credential)
- Headers:
  - `X-API-Key`: required; missing, empty, duplicate, comma-joined, unknown, revoked, and former
    public mock keys (`test-api-key-123`, `dev-api-key-456`) return `401`
  - `X-Correlation-ID`: optional, echoed in response when provided
- Request body:

```json
{
  "prompt": "hello",
  "metadata": {}
}
```

- Validation:
  - `prompt` must be non-empty and <= 4000 chars
  - serialized `metadata` must be <= 1000 bytes

- Response codes:
  - `200 OK` success
  - `400 Bad Request` invalid request
  - `401 Unauthorized` invalid/missing/ambiguous API key
  - `500 Internal Server Error` execution failed
  - `503 Service Unavailable` authentication datastore unavailable or circuit breaker open

Error payload format:

```json
{
  "error": {
    "code": "execution_failed",
    "message": "Failed to execute LLM request"
  }
}
```

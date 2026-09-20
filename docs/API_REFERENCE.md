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

## POST /api/v1/auth/keys

Create a management or inference key in the authenticated tenant.

- Auth: required management `X-API-Key`. Inference credentials receive `403`. Missing, unknown, and
  malformed credentials receive `401`.
- Tenant ownership comes from the authenticated key. Request fields `client_id` and `tenant_id` are
  rejected with `400` and do not create a key. `kind` must be `management` or `inference`.
- HTTP issuance uses the same generation and hashing service as `opmux-admin`. The plaintext
  credential is returned **once** and cannot be retrieved later.
- Headers:
  - `X-API-Key`: required management credential
  - Successful responses include `Cache-Control: no-store`

Request body:

```json
{
  "name": "route-key",
  "kind": "inference"
}
```

- `name`: 1–128 characters after trimming
- `kind`: `management` or `inference`

Response `201 Created`:

```json
{
  "client_id": "11111111-1111-1111-1111-111111111111",
  "key_id": "22222222-2222-2222-2222-222222222222",
  "display_id": "opk_...",
  "name": "route-key",
  "kind": "inference",
  "created_at": "2026-09-19T00:00:00Z",
  "credential": "opmx_v1_..."
}
```

The response is safe metadata plus the one-time credential. Digests and internal store records are
not serialized. Subsequent list responses omit `credential`.

Response codes:

- `201 Created` success
- `400 Bad Request` invalid name/kind or ownership override
- `401 Unauthorized` missing/invalid API key
- `403 Forbidden` authenticated inference key
- `503 Service Unavailable` authentication datastore unavailable

## GET /api/v1/auth/keys

List keys for the authenticated tenant.

- Auth: required management `X-API-Key`. Inference credentials receive `403`. Missing, unknown, and
  malformed credentials receive `401`.
- Inventory is always the authenticated tenant. Query selectors cannot choose another client.
- Responses include at most 100 keys, newest first, with safe metadata only: `client_id`, `key_id`,
  `display_id`, `name`, `kind`, `created_at`, `last_used_at`, and `revoked_at`. Credentials and
  digests are never included.

Response `200 OK`:

```json
{
  "keys": [
    {
      "client_id": "11111111-1111-1111-1111-111111111111",
      "key_id": "22222222-2222-2222-2222-222222222222",
      "display_id": "opk_...",
      "name": "route-key",
      "kind": "inference",
      "created_at": "2026-09-19T00:00:00Z",
      "last_used_at": null,
      "revoked_at": null
    }
  ]
}
```

Response codes:

- `200 OK` success
- `401 Unauthorized` missing/invalid API key
- `403 Forbidden` authenticated inference key
- `503 Service Unavailable` authentication datastore unavailable

## POST /api/v1/route

Protected AI routing endpoint.

- Auth: required inference `X-API-Key`. Management credentials receive `403` and do not start
  upstream generation. Missing, unknown, and malformed credentials receive `401`.
- Headers:
  - `X-API-Key`: required inference credential; missing, empty, duplicate, comma-joined, unknown,
    revoked, and former public mock keys (`test-api-key-123`, `dev-api-key-456`) return `401`
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
  - `403 Forbidden` authenticated management key (generation requires inference)
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

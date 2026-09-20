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
- Inventory is always the authenticated tenant. Query selectors `client_id` and `tenant_id` are
  rejected with `400` and cannot choose another client. Duplicate or unknown query names also return
  `400`.
- Optional query parameters:
  - `limit`: integer page size from 1 through 100. Default `100`.
  - `offset`: integer number of newest-first keys to skip. Default `0`. Must be `>= 0`.
  - `kind`: `management` or `inference`. Omit to include both kinds.
- Malformed `limit`, `offset`, or `kind` values return `400` and do not change inventory.
- Responses include at most `limit` keys, newest first, with safe metadata only: `client_id`,
  `key_id`, `display_id`, `name`, `kind`, `created_at`, `last_used_at`, and `revoked_at`.
  Credentials and digests are never included. `last_used_at` is null until the first successful
  authentication. It is written monotonically before the request is admitted, including when later
  inference fails. Missing, unknown, and revoked credentials do not update it. Listing a tenant
  authenticates the calling manager, so that manager's `last_used_at` advances.
- `has_more` is `true` when additional same-tenant keys exist after this page. Continue with the
  same `limit` and `kind`, setting `offset` to the previous `offset + limit`. When the page is
  shorter than `limit` or `has_more` is `false`, there is no further page. This MVP does not return
  a continuation token.

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
  ],
  "has_more": false
}
```

Response codes:

- `200 OK` success
- `400 Bad Request` ownership selector, unknown/duplicate query, or invalid paging/kind
- `401 Unauthorized` missing/invalid API key
- `403 Forbidden` authenticated inference key
- `503 Service Unavailable` authentication datastore unavailable

## DELETE /api/v1/auth/keys/{id}

Revoke a key in the authenticated tenant. The row is retained with a revocation timestamp.

- Auth: required management `X-API-Key`. Inference credentials receive `403`, including when
  targeting a key in their own tenant. Missing, unknown, and malformed credentials receive `401`.
- Tenant ownership comes from the authenticated key. The path `{id}` cannot select another tenant.
  Other-tenant and well-formed nonexistent UUIDs return indistinguishable `404` status and error
  text (correlation headers may differ).
- First revocation commits `revoked_at` and returns `204`. Repeating DELETE for the same tenant
  returns `204` without changing the original timestamp.
- Self-revocation and final-manager revocation are allowed. There is no last-manager lock. Recover
  with `opmux-admin key issue` for the existing client; that issues a replacement and does not
  revive revoked keys.
- Rotation is create a replacement manager, verify it, then revoke the old key. Overlap is required:
  the replacement must work before the old key is revoked.
- Revocation is effective for authentication after commit, including other gateway processes that
  share the database. There is no authentication cache and no TTL delay. Already admitted requests
  may finish; revocation does not cancel in-flight provider work.

Response codes:

- `204 No Content` success, including idempotent same-tenant repeats
- `401 Unauthorized` missing/invalid API key
- `403 Forbidden` authenticated inference key
- `404 Not Found` missing or other-tenant key
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

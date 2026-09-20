# Gateway API Reference

Base URL: `http://<host>:3000`

## GET /

Simple service banner endpoint.

- Auth: none
- Response: `200 OK`, HTML body

## GET /health

Process liveness endpoint. It does not probe the authentication database or upstream provider. A
live process can return `200` while `/ready` is `503`.

- Auth: none
- Response: `200 OK`
- Body fields: `status`, `timestamp`, `version`, `uptime_seconds`

## GET /ready

Readiness endpoint. Returns `200` only when the authentication database schema, selected-column,
locking, and `last_used_at` UPDATE access, upstream `/models` reachability, and at least one usable
default-route target are healthy. The database probe does not persist writes or fire UPDATE
triggers. `/models` is a reachability and credential probe; it does not call generation and does not
prove that a configured model can generate. Cached `/models` success cannot override a default route
whose eligible targets are all circuit-open. Successful database and upstream probes may be cached
for `HEALTH_CHECK_CACHE_TTL_SECS` (default 5 seconds). Failures are never cached, so a restored
dependency is rechecked on the next probe. Draining override is not part of this endpoint yet.

- Auth: none
- Response:
  - `200 OK` when `database`, `upstream`, and `default_route` are healthy
  - `503 Service Unavailable` when any required dependency is unhealthy
- Probe timeout: `HEALTH_CHECK_TIMEOUT` (default 2 seconds)

Example response (`503`):

```json
{
  "status": "not_ready",
  "timestamp": "2026-03-13T00:00:00Z",
  "dependencies": {
    "database": {
      "status": "unhealthy",
      "latency_ms": 12,
      "error": "Authentication database unavailable"
    },
    "upstream": { "status": "healthy", "latency_ms": 8 },
    "default_route": { "status": "healthy" }
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
  credential is returned **once** and cannot be retrieved later. One-time output is not a delivery
  or exactly-once guarantee. Protected-request deadlines still apply to this mutation. A timeout or
  disconnect during commit acknowledgement does not prove rollback. If a key was committed and the
  creation response was lost, the plaintext cannot be revealed again; list inventory and revoke or
  replace the key with the existing management API or `opmux-admin`. This MVP does not recover
  plaintext, add idempotency storage, or reconcile commit with the HTTP response.
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
- `413 Payload Too Large` raw JSON body above `max_request_body_bytes` (`PAYLOAD_TOO_LARGE`)
- `503 Service Unavailable` authentication datastore unavailable
- `504 Gateway Timeout` protected-request deadline elapsed. This does not prove the insert rolled
  back.

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
  envelope (`NOT_FOUND`) apart from request correlation.
- First revocation commits `revoked_at` and returns `204`. Repeating DELETE for the same tenant
  returns `204` without changing the original timestamp. Protected-request deadlines still apply. A
  timeout or disconnect during acknowledgement does not prove rollback; check inventory and repeat
  DELETE if the row is still active.
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
- `504 Gateway Timeout` protected-request deadline elapsed. This does not prove the revoke rolled
  back.

## POST /api/v1/route

Protected AI routing endpoint.

- Auth: required inference `X-API-Key`. Management credentials receive `403` and do not start
  upstream generation. Missing, unknown, and malformed credentials receive `401`.
- Headers:
  - `X-API-Key`: required inference credential; missing, empty, duplicate, comma-joined, unknown,
    revoked, and former public mock keys (`test-api-key-123`, `dev-api-key-456`) return `401`
  - `X-Correlation-ID`: optional, echoed in response when provided
- Request body (canonical contract):

```json
{
  "prompt": "hello",
  "metadata": {},
  "route": "fast",
  "allow_fallback": true,
  "parameters": {
    "temperature": 0.2,
    "top_p": 1.0,
    "max_tokens": 32
  }
}
```

Existing `{ "prompt": "...", "metadata": {} }` requests remain valid and use the documented defaults
below.

- `prompt` (required string): must contain a non-whitespace character. Length is measured on the
  **original untrimmed** string. Inclusive maxima are `max_prompt_chars` Unicode scalar values and
  `max_prompt_chars` UTF-8 bytes (default `4000` for both units, overridable by catalog/env).
  Trimming cannot bypass those bounds. An accepted prompt is forwarded unchanged as the user
  message. Whitespace-only input returns `400`.
- `metadata` (required): bounded opaque JSON, serialized size <= `max_metadata_bytes` (default 1000
  bytes). It is not forwarded upstream, logged, or persisted, and cannot select a route, model,
  vendor, URL, or tenant.
- `route` (optional string): configured route name. Omitted selects the catalog `default_route`. An
  unknown name returns `400` before any provider call. Clients cannot inject an arbitrary provider,
  model, or URL. A non-string value returns `400`.
- `allow_fallback` (optional boolean): omitted follows the configured fallback chain; `true` does
  the same and cannot invent fallbacks a route does not have; `false` limits execution to the
  primary target without disabling that target's bounded retries. A non-boolean value returns `400`.
  Eligible fallbacks run in the catalog's flat order under the shared attempt budget and deadline.
  Transient transport, attempt-timeout, and provider 5xx failures may switch to a later
  same-provider target. Invalid client input starts no calls. Malformed or oversized success bodies,
  permanent upstream rejection, shared-credential `401`/`403`, exhausted quota, and same-account
  throttling do not switch models. Throttling may still retry the current target while honoring
  `Retry-After`. A later target whose `max_output_tokens` is below the already-validated
  `max_tokens` is skipped without changing parameters. If no eligible later target can run, the
  original primary error is preserved unless the overall deadline expires.
- `parameters` (optional object): typed generation controls. Unknown parameter names return `400`.
  Values are not coerced or clamped.
  - `temperature` (optional JSON number): inclusive range `0.0` through `2.0`. Omitted: not sent
    upstream (provider default `1.0`).
  - `top_p` (optional JSON number): inclusive range `0.0` through `1.0`. Omitted: not sent upstream
    (provider default `1.0`).
  - `max_tokens` (optional JSON integer): integral, `>= 1`, and `<=` the selected primary target's
    `max_output_tokens`. Omitted: the selected primary cap is sent as `max_tokens`. Fractional,
    zero, negative, and above-cap values return `400` with no provider call.
- Unsupported controls: top-level `stream` and `rewrite` (including `true`) return `400`. Unknown
  top-level fields such as `model` or `url` return `400`. Nested keys inside `metadata` remain
  opaque and cannot change controls.

- Validation:
  - The raw HTTP body, including advertised `Content-Length` and chunked/no-length transfer, must be
    <= `max_request_body_bytes` (default 1048576 bytes, inclusive). Overflow returns `413` before
    generation or key mutation. Health, readiness, and metrics are not this protected-route limit.
  - `prompt` must be a nonempty (after trim) string within the original character and byte bounds
  - serialized `metadata` must be <= `max_metadata_bytes` (default 1000 bytes, inclusive). There is
    no additional metadata structural bound in this MVP. Overflow returns `400 INVALID_REQUEST`.
  - `route`, when present, must be a string naming a configured route
  - `allow_fallback`, when present, must be a boolean
  - `parameters` types/ranges and the selected primary token cap are checked before upstream access
  - Invalid JSON that cannot be decoded and semantic/typed field violations both return `400`.
  - Concurrent generation is capped by `max_concurrent_generations` (default 32). Saturation returns
    `429 OVERLOADED` with `Retry-After: 1` and does not wait for a slot. Health, readiness, and
    metrics are not this generation limit.

Successful generation uses the real OpenAI Chat Completions adapter:
`POST {OPENAI_BASE_URL}/chat/completions` with `Authorization: Bearer`,
`Content-Type: application/json`, the selected target model, the original user prompt, and accepted
typed options. Streaming is not enabled.

Response `200 OK`:

```json
{
  "response": {
    "content": "hello from the model",
    "role": "assistant",
    "finish_reason": "stop"
  },
  "model_used": "reported-snapshot-model",
  "cost": 0.00018,
  "processing_time_ms": 12,
  "usage": {
    "prompt_tokens": 120,
    "completion_tokens": 30
  }
}
```

- `response.content`, `response.role`, and `response.finish_reason` are copied from the provider
  message. Successful Chat Completions `message.role` must be exactly `assistant`. Values such as
  `user`, `system`, or whitespace-padded roles are upstream protocol errors, not fabricated
  successes.
- `model_used` is the provider-reported model. It may differ from the selected catalog target alias
  that was sent as the request `model`.
- `usage` matches the validated provider prompt and completion token counts.
- `cost` is a USD estimate for this successful response only, from the **selected target's**
  configured `input_per_million` and `output_per_million` prices:

  `cost = round((prompt_tokens * input_per_million + completion_tokens * output_per_million) / 1_000_000, 8)`

  Illustrative catalog prices of `1.0` and `2.0` per million tokens with 120 prompt and 30
  completion tokens yield `0.00018`. Distinct catalog target IDs keep their own prices even when
  they request the same provider model, and even when the provider reports the same snapshot model.
  Fallback hops use the successful fallback target's prices, not the primary's. Missing prices fail
  the request; they do not become `0`. Example catalog prices are samples, not current provider
  billing. `cost` is not a bill and does not total retries or abandoned work.

- `processing_time_ms` is a nonnegative elapsed-time measurement for the request.
- Malformed successful Chat Completions JSON, empty `choices`, missing required `model` / message
  `content` / `role` / `finish_reason` / `usage`, non-assistant `message.role`, non-text content,
  negative or fractional token counts, and inconsistent `total_tokens` fail as upstream protocol
  errors. The gateway does not invent `model_used`, content, usage, role, or a zero `cost`. Raw
  upstream bytes are not copied into the client error. These faults are not retried as transport
  failures.
- Provider bodies are limited by `max_upstream_response_bytes` (default 1048576) while reading,
  including advertised `Content-Length` and chunked/no-length transfer. An oversized upstream body
  is an upstream-result error, not a client `413`.

- Response codes:
  - `200 OK` success
  - `400 Bad Request` invalid JSON (`INVALID_JSON`), unknown/unsupported control, metadata size, or
    out-of-range parameter (`INVALID_REQUEST`). Malformed path parameters use `INVALID_PATH`.
  - `401 Unauthorized` invalid/missing/ambiguous API key (`UNAUTHORIZED`)
  - `403 Forbidden` authenticated management key (generation requires inference) (`FORBIDDEN`)
  - `413 Payload Too Large` raw body above `max_request_body_bytes` (`PAYLOAD_TOO_LARGE`)
  - `415 Unsupported Media Type` non-JSON Content-Type (`UNSUPPORTED_MEDIA_TYPE`)
  - `429 Too Many Requests` local generation admission saturation (`OVERLOADED`) or upstream
    provider throttling (`UPSTREAM_RATE_LIMIT`). Local overload returns `Retry-After: 1` and does
    not start a provider call. A valid provider `Retry-After` that cannot finish in remaining time
    returns `UPSTREAM_RATE_LIMIT` only while the overall deadline has not elapsed.
  - `502 Bad Gateway` upstream credential, protocol, oversized, quota, or other provider failure
    (`UPSTREAM_AUTHENTICATION`, `UPSTREAM_PROTOCOL`, `UPSTREAM_ERROR`). Provider 401/403 is never a
    gateway `401`. A complete bounded HTTP 429 JSON body with `error.code` or `error.type`
    `insufficient_quota` is exhausted quota (`UPSTREAM_ERROR`), not throttling. Stalled, malformed,
    or oversized 429 bodies keep header throttling.
  - `504 Gateway Timeout` overall protected-request deadline elapsed (`DEADLINE_EXCEEDED`). Actual
    expiry takes precedence over a ready 429 or saved `Retry-After`.
  - `500 Internal Server Error` unexpected internal fault (`INTERNAL_ERROR`)
  - `503 Service Unavailable` authentication datastore unavailable (`AUTH_DEPENDENCY_UNAVAILABLE`)
    or all eligible targets circuit-open (`CIRCUIT_OPEN`)

Protected route, key-create, key-list, and key-delete errors, including JSON and path extraction
rejections, use one envelope:

```json
{
  "error": {
    "code": "UPSTREAM_ERROR",
    "message": "Upstream provider request failed",
    "request_id": "11111111-1111-4111-8111-111111111111"
  }
}
```

`error.request_id` equals the `X-Request-ID` response header. A valid `X-Correlation-ID` (non-empty,
at most 256 bytes, valid header text) is echoed even on early failure. Empty, overlong, or non-UTF-8
correlation values are ignored and replaced with no correlation header; a request ID is still
generated. Request-scoped logs inherit those IDs from a root span opened before authentication.
Authentication duration ends before downstream inference or management work. Codes and messages do
not copy client correlation IDs, prompts, metadata, SQL, provider bodies, secrets, or
credential-bearing URLs.

Documented codes:

| Code                          | Status | Meaning                                                   |
| ----------------------------- | ------ | --------------------------------------------------------- |
| `INVALID_REQUEST`             | 400    | Semantic validation or unknown control                    |
| `INVALID_JSON`                | 400    | Malformed JSON body                                       |
| `INVALID_PATH`                | 400    | Path parameter could not be parsed                        |
| `UNSUPPORTED_MEDIA_TYPE`      | 415    | Content-Type is not `application/json`                    |
| `UNAUTHORIZED`                | 401    | Missing, malformed, unknown, or revoked gateway key       |
| `FORBIDDEN`                   | 403    | Authenticated key lacks the required capability           |
| `NOT_FOUND`                   | 404    | Same-tenant key missing; other-tenant IDs use this too    |
| `AUTH_DEPENDENCY_UNAVAILABLE` | 503    | Authentication datastore unavailable                      |
| `UPSTREAM_AUTHENTICATION`     | 502    | Upstream rejected provider credentials                    |
| `UPSTREAM_PROTOCOL`           | 502    | Unusable or oversized upstream success body               |
| `UPSTREAM_ERROR`              | 502    | Other upstream execution failure                          |
| `UPSTREAM_RATE_LIMIT`         | 429    | Upstream throttled the request                            |
| `INTERNAL_ERROR`              | 500    | Unexpected internal fault                                 |
| `DEADLINE_EXCEEDED`           | 504    | Protected-request deadline elapsed                        |
| `CIRCUIT_OPEN`                | 503    | All eligible targets are circuit-open                     |
| `OVERLOADED`                  | 429    | Local generation concurrency is saturated                 |
| `PAYLOAD_TOO_LARGE`           | 413    | Raw protected JSON body exceeded `max_request_body_bytes` |

Health and readiness probes keep their existing status documents; they are not this protected-API
envelope.

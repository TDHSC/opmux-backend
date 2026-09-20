# Prometheus Integration Guide

## Metrics endpoint

By default, metrics are exposed at `GET /metrics` when `METRICS_ENABLED=true`.

`/metrics` is an **internal operational scrape surface**. It has no application authentication.
Local deployment binds the gateway on loopback. Production must restrict scrape access at the
network layer (private network, host firewall, or reverse-proxy allowlist). Do not add a separate
metrics authentication system.

Correlation still applies: a scrape may send `X-Correlation-ID` and always receives `X-Request-ID`.

## Environment configuration

- `METRICS_ENABLED=true`
- `METRICS_PATH=/metrics`

## Prometheus scrape config example

```yaml
scrape_configs:
  - job_name: gateway
    metrics_path: /metrics
    static_configs:
      - targets: ['127.0.0.1:3000']
```

Do not scrape this endpoint from the public Internet.

## HTTP metrics

Collected by `axum-prometheus` with prefix `gateway`:

- `gateway_http_requests_total` (labels: `method`, `endpoint`, `status`)
- `gateway_http_requests_pending` (labels: `method`, `endpoint`)
- `gateway_http_requests_duration_seconds` (labels: `method`, `endpoint`, `status`)

Auth failures are included. `endpoint` is a matched route template or a bounded fallback (`/`,
`/health`, `/ready`, `/metrics`, `/api/v1/route`, `/api/v1/auth/keys`, `/api/v1/auth/keys/{id}`, or
`unmatched`). Raw paths, query strings, and key UUIDs are not labels.

## Execution metrics

Recorded only for started provider calls and local admission outcomes:

| Metric                                       | Labels                     | Meaning                                               |
| -------------------------------------------- | -------------------------- | ----------------------------------------------------- |
| `gateway_execution_attempts_total`           | `target`, `outcome`        | One increment per started provider attempt            |
| `gateway_execution_retries_total`            | `target`                   | Extra call on the same target after a prior attempt   |
| `gateway_execution_fallbacks_total`          | `from_target`, `to_target` | Adjacent evaluated hop to a later configured target   |
| `gateway_circuit_transitions_total`          | `target`, `to_state`       | Circuit phase change (`closed`, `half_open`, `open`)  |
| `gateway_circuit_state`                      | `target`                   | Gauge: 0 closed, 1 half-open, 2 open                  |
| `gateway_deadline_exceeded_total`            | none                       | Overall protected-request deadline expiry             |
| `gateway_overload_rejected_total`            | none                       | Local generation admission `429 OVERLOADED`           |
| `gateway_successful_prompt_tokens_total`     | `target`                   | Validated prompt tokens from successful responses     |
| `gateway_successful_completion_tokens_total` | `target`                   | Validated completion tokens from successful responses |

`target` / `from_target` / `to_target` are accepted operator-configured catalog target IDs, exported
verbatim. Cardinality is bounded by catalog membership. The Prometheus exporter escapes label
values; identifiers are not truncated, hashed, or collapsed to `unknown`. Callers never source these
labels from unvalidated request IDs, provider-returned model strings, or URLs.

`outcome` is one of: `success`, `retryable`, `timeout`, `rate_limit`, `quota`, `upstream_auth`,
`protocol`, `rejected`, `deadline`, `cancelled`, `circuit_open`, `internal`. A started attempt that
is dropped after the overall deadline records `deadline`; a drop while time remains records
`cancelled`. Cancelled work does not record successful usage. The attempt guard does not increment
`gateway_deadline_exceeded_total`.

Fallback series record adjacent evaluated hops (`A -> B`, then `B -> C`). An incompatible token-cap
skip does not count as an evaluated hop, so skipping `B` records `A -> C`. Circuit-skipped hops do
not fabricate provider attempts.

Rejected requests (missing/invalid credentials, unknown routes, local overload) are not execution
attempts. Failed-attempt usage is not counted. Local overload and provider throttling
(`outcome="rate_limit"` / `UPSTREAM_RATE_LIMIT`) stay distinct.

## Label policy

Allowed label sources:

- HTTP method, status class, and the bounded endpoint templates above
- Configured catalog target IDs
- Finite protocol/outcome and circuit-state classes

Never used as labels: credentials, prompts, metadata, tenant/key IDs, request or correlation IDs,
raw paths/queries, URLs, provider-returned model strings, or raw error bodies.

## Useful alerts

- High 5xx ratio on `/ready` or `/api/v1/route`
- Rising `gateway_overload_rejected_total` while `/health` stays 200
- `gateway_circuit_state` stuck at 2 for a target
- Elevated `gateway_deadline_exceeded_total`
- No scrape data for the gateway target

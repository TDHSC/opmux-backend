# Gateway Operations Runbook

## Startup checks

1. Confirm required env vars are set (`OPMUX_CONFIG_FILE`, `OPENAI_API_KEY`, `SERVER_PORT`, auth
   settings). Copying `.env` is not process configuration.
2. Start service and verify startup logs show initialized Executor/Health/Ingress services.
3. Validate endpoints:

```bash
curl -i http://127.0.0.1:3000/health
curl -i http://127.0.0.1:3000/ready
curl -i http://127.0.0.1:3000/metrics
```

## Incident triage

### Symptom: `/ready` returns `503`

- Check dependency details in readiness response (`dependencies.error`, `healthy_vendors`).
- Validate vendor credentials and upstream endpoint connectivity.
- Confirm circuit breaker behavior via repeated `/api/v1/route` calls.

### Symptom: `/api/v1/route` returns `503 circuit_open`

- This indicates repeated transient failures for a vendor.
- Wait for breaker cool-down window, then retry.
- Investigate upstream network/timeout/rate-limit conditions.

### Symptom: increased latency

- Inspect metrics endpoint for request counters/latency trends.
- Lower load and run `scripts/run-load-tests.sh` to reproduce under controlled concurrency.

## Monitoring guide

- Use `/metrics` as scrape endpoint.
- Track at minimum:
  - request volume and failure ratio,
  - readiness status transitions,
  - latency and pending request trends,
  - circuit-open error frequency.

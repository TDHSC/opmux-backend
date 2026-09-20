# Performance and Load Testing

This guide provides repeatable load checks for the gateway HTTP pipeline.

## Prerequisites

1. Start the gateway against owned local Supabase and a reachable simulated or real upstream:

```bash
export OPENAI_API_KEY=dummy-key
export OPENAI_BASE_URL=http://127.0.0.1:9/v1
bash scripts/with-owned-database.sh cargo run -p gateway
```

Use a provisioned inference key. Former public mock keys return `401` and count as load-test
failures.

2. In another terminal, run the load script. `/health` and `/ready` must both return `200`.

## Default load test

```bash
GATEWAY_API_KEY="$INFERENCE_KEY" ./scripts/run-load-tests.sh
```

Defaults:

- `GATEWAY_BASE_URL=http://127.0.0.1:3000`
- `GATEWAY_API_KEY` (operator-provisioned inference key; required)
- `TOTAL_REQUESTS=100`
- `CONCURRENCY=10`
- `REQUEST_TIMEOUT_SECS=10`

## Custom load test

```bash
GATEWAY_BASE_URL=http://127.0.0.1:3000 \
GATEWAY_API_KEY="$INFERENCE_KEY" \
TOTAL_REQUESTS=200 \
CONCURRENCY=20 \
./scripts/run-load-tests.sh
```

## Interpreting results

- `Successful 2xx requests`: HTTP 200–299 only
- `Failed non-2xx requests`: 401/403/4xx/5xx and connection or timeout failures
- Unauthorized, overloaded, and upstream errors are failures, not successes
- `Approx throughput`: requests per second from wall clock duration
- Curl operations are bounded by `REQUEST_TIMEOUT_SECS`

Exit `0` only when every request is 2xx. Exit `2` when any request is non-2xx or a transport
failure. Exit `1` for setup errors (missing key, health/ready not 200, invalid counts).

For Task 13 validation, record:

1. total requests and concurrency used,
2. success/failure counts,
3. throughput and wall time,
4. any observed circuit-open behavior during upstream failure scenarios.

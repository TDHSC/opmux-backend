# Performance and Load Testing

This guide provides repeatable load checks for the gateway HTTP pipeline.

## Prerequisites

1. Start the gateway server:

```bash
export OPENAI_API_KEY=dummy-key
export OPENAI_BASE_URL=http://127.0.0.1:9/v1
bash scripts/with-owned-database.sh cargo run -p gateway
```

2. In another terminal, run the load script.

## Default load test

```bash
./scripts/run-load-tests.sh
```

Defaults:

- `GATEWAY_BASE_URL=http://127.0.0.1:3000`
- `GATEWAY_API_KEY` (operator-provisioned inference key; do not reuse former public mock keys)
- `TOTAL_REQUESTS=100`
- `CONCURRENCY=10`

## Custom load test

```bash
GATEWAY_BASE_URL=http://127.0.0.1:3000 \
GATEWAY_API_KEY="$INFERENCE_KEY" \
TOTAL_REQUESTS=200 \
CONCURRENCY=20 \
./scripts/run-load-tests.sh
```

## Interpreting results

- `Successful requests`: count of responses with HTTP status 2xx-4xx
- `Failed requests`: count of responses with HTTP status 5xx or connection failure
- `Approx throughput`: requests per second from wall clock duration

For Task 13 validation, record:

1. total requests and concurrency used,
2. success/failure counts,
3. throughput and wall time,
4. any observed circuit-open behavior during upstream failure scenarios.

#!/bin/bash
# Bounded load check against a running gateway. Only HTTP 2xx counts as
# success. 401/403/4xx/5xx and transport failures are classified as failures.
set -euo pipefail

RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
NC='\033[0m'

BASE_URL="${GATEWAY_BASE_URL:-http://127.0.0.1:3000}"
if [ -z "${GATEWAY_API_KEY:-}" ]; then
  echo -e "${RED}GATEWAY_API_KEY must be an operator-provisioned inference key${NC}" >&2
  exit 1
fi
API_KEY="${GATEWAY_API_KEY}"
TOTAL_REQUESTS="${TOTAL_REQUESTS:-100}"
CONCURRENCY="${CONCURRENCY:-10}"
REQUEST_TIMEOUT_SECS="${REQUEST_TIMEOUT_SECS:-10}"

echo -e "${BLUE}========================================${NC}"
echo -e "${BLUE}Gateway Load Test${NC}"
echo -e "${BLUE}========================================${NC}"
echo ""
echo "Base URL: ${BASE_URL}"
echo "Total requests: ${TOTAL_REQUESTS}"
echo "Concurrency: ${CONCURRENCY}"
echo ""

if [ "$TOTAL_REQUESTS" -le 0 ] || [ "$CONCURRENCY" -le 0 ]; then
  echo -e "${RED}Invalid TOTAL_REQUESTS or CONCURRENCY${NC}" >&2
  exit 1
fi

echo -e "${BLUE}Running warm-up health checks...${NC}"
health_status=$(curl --noproxy '*' -sS --max-time 3 -o /dev/null -w "%{http_code}" \
  "${BASE_URL}/health" || echo "000")
if [ "$health_status" != "200" ]; then
  echo -e "${RED}health check expected HTTP 200, received ${health_status}${NC}" >&2
  exit 1
fi
ready_status=$(curl --noproxy '*' -sS --max-time 3 -o /dev/null -w "%{http_code}" \
  "${BASE_URL}/ready" || echo "000")
if [ "$ready_status" != "200" ]; then
  echo -e "${RED}ready check expected HTTP 200, received ${ready_status}${NC}" >&2
  exit 1
fi

TMP_DIR=$(mktemp -d)
trap 'rm -rf "$TMP_DIR"' EXIT

REQUESTS_PER_WORKER=$((TOTAL_REQUESTS / CONCURRENCY))
REMAINDER=$((TOTAL_REQUESTS % CONCURRENCY))

now_ms() {
  python3 -c 'import time; print(int(time.time() * 1000))'
}

run_worker() {
  local worker_id="$1"
  local request_count="$2"
  local success=0
  local failed=0
  local started
  started=$(now_ms)

  for i in $(seq 1 "$request_count"); do
    status=$(curl --noproxy '*' -sS --max-time "$REQUEST_TIMEOUT_SECS" \
      -o /dev/null -w "%{http_code}" \
      -X POST "${BASE_URL}/api/v1/route" \
      -H "Content-Type: application/json" \
      -H "X-API-Key: ${API_KEY}" \
      -H "X-Correlation-ID: load-${worker_id}-${i}" \
      -d '{"prompt":"load-test-message","metadata":{}}' || echo "000")

    if [ "$status" -ge 200 ] && [ "$status" -lt 300 ]; then
      success=$((success + 1))
    else
      failed=$((failed + 1))
    fi
  done

  ended=$(now_ms)
  elapsed=$((ended - started))
  echo "${success},${failed},${elapsed}" >"${TMP_DIR}/worker-${worker_id}.csv"
}

echo -e "${BLUE}Running concurrent load...${NC}"

for worker in $(seq 1 "$CONCURRENCY"); do
  count=$REQUESTS_PER_WORKER
  if [ "$worker" -le "$REMAINDER" ]; then
    count=$((count + 1))
  fi
  run_worker "$worker" "$count" &
done

wait

total_success=0
total_failed=0
max_elapsed=0

shopt -s nullglob
for file in "$TMP_DIR"/worker-*.csv; do
  IFS=',' read -r s f e <"$file"
  total_success=$((total_success + s))
  total_failed=$((total_failed + f))
  if [ "$e" -gt "$max_elapsed" ]; then
    max_elapsed=$e
  fi
done
shopt -u nullglob

total_done=$((total_success + total_failed))
if [ "$max_elapsed" -eq 0 ]; then
  rps=0
else
  rps=$((total_done * 1000 / max_elapsed))
fi

echo ""
echo -e "${BLUE}========================================${NC}"
echo -e "${BLUE}Load Test Result${NC}"
echo -e "${BLUE}========================================${NC}"
echo "Completed requests: ${total_done}"
echo "Successful 2xx requests: ${total_success}"
echo "Failed non-2xx requests: ${total_failed}"
echo "Approx throughput: ${rps} req/s"
echo "Wall time: ${max_elapsed} ms"

if [ "$total_failed" -gt 0 ]; then
  echo -e "${YELLOW}Load test finished with non-2xx or transport failures${NC}" >&2
  exit 2
fi

echo -e "${GREEN}Load test finished successfully${NC}"

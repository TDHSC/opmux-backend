#!/bin/bash
# Exercise the built gateway without real credentials or external upstream calls.
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
BINARY="${1:-$ROOT_DIR/target/release/gateway}"
PORT="${STARTUP_CHECK_PORT:-3000}"
BASE_URL="http://127.0.0.1:$PORT"
TMP_DIR="$(mktemp -d)"
SERVER_PID=""

cleanup() {
  local status=$?
  if [ -n "$SERVER_PID" ]; then
    kill "$SERVER_PID" 2>/dev/null || true
    wait "$SERVER_PID" 2>/dev/null || true
  fi
  if [ "$status" -ne 0 ]; then
    cat "$TMP_DIR/gateway.log" >&2
  fi
  rm -rf "$TMP_DIR"
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM

env -u ANTHROPIC_API_KEY -u LOG_LEVEL -u LOG_JSON \
  -u HTTP_PROXY -u HTTPS_PROXY -u ALL_PROXY -u http_proxy -u https_proxy -u all_proxy \
  AUTH_DEVELOPMENT_MODE=false SERVER_HOST=127.0.0.1 SERVER_PORT="$PORT" \
  OPMUX_CONFIG_FILE="$ROOT_DIR/config/opmux.example.json" \
  OPENAI_API_KEY=dummy-key OPENAI_BASE_URL=http://127.0.0.1:9/v1 OPENAI_TIMEOUT_MS=200 \
  EXECUTOR_MAX_RETRIES=0 HEALTH_CHECK_TIMEOUT=1 METRICS_ENABLED=true METRICS_PATH=/metrics \
  RUST_LOG=info LOG_FORMAT=json LOG_VERBOSE_DEBUG=false \
  "$BINARY" >"$TMP_DIR/gateway.log" 2>&1 &
SERVER_PID=$!

ready=false
for _ in {1..50}; do
  if ! kill -0 "$SERVER_PID" 2>/dev/null; then
    echo "Gateway exited before startup completed" >&2
    exit 1
  fi
  if curl --noproxy '*' --silent --fail --max-time 1 "$BASE_URL/health" >/dev/null; then
    ready=true
    break
  fi
  sleep 0.1
done
if [ "$ready" != true ]; then
  echo "Gateway did not become live within the startup deadline" >&2
  exit 1
fi

check_response() {
  local path="$1" expected="$2"
  shift 2
  local status
  status=$(curl --noproxy '*' --silent --show-error --max-time 3 \
    --dump-header "$TMP_DIR/headers" --output "$TMP_DIR/body" --write-out '%{http_code}' \
    -H "X-Correlation-ID: startup-check" "$@" "$BASE_URL$path")
  if [ "$status" != "$expected" ]; then
    echo "$path: expected HTTP $expected, received $status" >&2
    exit 1
  fi
  grep -qi '^x-request-id: ' "$TMP_DIR/headers"
  grep -qi $'^x-correlation-id: startup-check\r$' "$TMP_DIR/headers"
}

check_response /health 200
check_response /ready 503
check_response /api/v1/route 401 -X POST -H "Content-Type: application/json" \
  -d '{"prompt":"startup check","metadata":{}}'
check_response /api/v1/route 401 -X POST -H "Content-Type: application/json" \
  -H "X-API-Key: invalid-key" -d '{"prompt":"startup check","metadata":{}}'
check_response /api/v1/route 400 -X POST -H "Content-Type: application/json" \
  -H "X-API-Key: test-api-key-123" -d '{"prompt":"","metadata":{}}'
check_response /metrics 200
grep -q 'gateway_http_requests_total' "$TMP_DIR/body"

if grep -Eq 'Authentication is BYPASSED|AUTH_DEVELOPMENT_MODE is ENABLED' "$TMP_DIR/gateway.log"; then
  echo "Authentication bypass detected in startup logs" >&2
  exit 1
fi
kill -0 "$SERVER_PID"
echo "Startup check passed: liveness, readiness failure, authentication, metrics, and correlation IDs"

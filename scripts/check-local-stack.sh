#!/bin/bash
# Run the documented local Compose stack against the owned database-only
# Supabase and the simulated OpenAI provider. Inspect actual host bindings,
# restart recovery, and cleanup ownership. Does not call real providers or
# mutate hosted databases.
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
COMPOSE_FILE="$ROOT_DIR/docker-compose.yml"
IMAGE="${CONTAINER_IMAGE:-opmux-gateway:mvp}"
GATEWAY_HOST_PORT="${OPMUX_LOCAL_GATEWAY_PORT:-38080}"
SIMULATOR_HOST_PORT="${OPMUX_LOCAL_SIMULATOR_PORT:-38081}"
NETWORK=opmux-mvp-20260919-loopback
DB_CONTAINER=supabase_db_opmux-mvp-20260919
GATEWAY_CONTAINER=opmux-local-gateway
SIMULATOR_CONTAINER=opmux-local-simulator
DUMMY_PROVIDER_KEY=local-stack-dummy-only
WORK_DIR="$(mktemp -d "${TMPDIR:-/tmp}/opmux-local-stack-check.XXXXXX")"
chmod 700 "$WORK_DIR"
TENANT_JSON="$WORK_DIR/tenant.json"
INFERENCE_JSON="$WORK_DIR/inference.json"
touch "$TENANT_JSON" "$INFERENCE_JSON"
chmod 600 "$TENANT_JSON" "$INFERENCE_JSON"
STARTED_STACK=0
TENANT_NAME="local-stack-check-$$"

cleanup() {
  local status=$?
  if [ "$STARTED_STACK" -eq 1 ]; then
    bash "$ROOT_DIR/scripts/local-stack.sh" down >/dev/null 2>&1 || true
  fi
  rm -rf "$WORK_DIR"
  if [ "$status" -ne 0 ]; then
    echo "local stack check failed" >&2
  fi
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM

require_file() {
  if [ ! -f "$1" ]; then
    echo "missing required file $1" >&2
    exit 1
  fi
}

require_file "$COMPOSE_FILE"
require_file "$ROOT_DIR/scripts/local-stack.sh"
require_file "$ROOT_DIR/scripts/openai-simulator.py"
require_file "$ROOT_DIR/simulator/Dockerfile"
require_file "$ROOT_DIR/config/opmux.example.json"

if grep -Eq 'image:[[:space:]]*(postgres|supabase/postgres)' "$COMPOSE_FILE"; then
  echo "compose must not start a second database" >&2
  exit 1
fi
if grep -Eq 'supabase[[:space:]]+start' "$COMPOSE_FILE"; then
  echo "compose must not run supabase start" >&2
  exit 1
fi
if grep -Eq '0\.0\.0\.0:' "$COMPOSE_FILE"; then
  echo "compose must not publish host ports on all interfaces" >&2
  exit 1
fi
if grep -Eq 'api\.openai\.com' "$COMPOSE_FILE"; then
  echo "compose must not default to the paid OpenAI endpoint" >&2
  exit 1
fi
if ! grep -Eq '127\.0\.0\.1:\$\{OPMUX_LOCAL_GATEWAY_PORT:-38080\}:3000|127\.0\.0\.1:38080:3000' "$COMPOSE_FILE"; then
  echo "compose must publish the gateway on 127.0.0.1:38080" >&2
  exit 1
fi
if ! grep -Eq '127\.0\.0\.1:\$\{OPMUX_LOCAL_SIMULATOR_PORT:-38081\}:38081|127\.0\.0\.1:38081:38081' "$COMPOSE_FILE"; then
  echo "compose must publish the simulator on 127.0.0.1:38081" >&2
  exit 1
fi
if ! grep -F 'opmux-mvp-20260919-loopback' "$COMPOSE_FILE" | grep -q .; then
  echo "compose must reuse the owned loopback network" >&2
  exit 1
fi
if ! grep -Eq 'external:[[:space:]]*true' "$COMPOSE_FILE"; then
  echo "compose must use the owned network as external" >&2
  exit 1
fi

published="$(docker port "$DB_CONTAINER" 5432/tcp 2>/dev/null | tr -d '\r' || true)"
if [ "$published" != "127.0.0.1:55432" ]; then
  echo "owned supabase is not bound to 127.0.0.1:55432" >&2
  exit 1
fi
if ! docker exec "$DB_CONTAINER" pg_isready -U postgres -d postgres >/dev/null 2>&1; then
  echo "owned supabase is not accepting connections" >&2
  exit 1
fi
if ! docker network inspect "$NETWORK" >/dev/null 2>&1; then
  echo "owned docker network is missing" >&2
  exit 1
fi

unrelated_before="$(docker ps -a --format '{{.Names}}' | grep -v -e '^opmux-local-' -e '^$' | LC_ALL=C sort)"
if ! printf '%s\n' "$unrelated_before" | grep -qx "$DB_CONTAINER"; then
  echo "owned supabase container missing from docker ps" >&2
  exit 1
fi
for name in mysql redis redis-admin actualbudget-actual_server-1; do
  if ! printf '%s\n' "$unrelated_before" | grep -qx "$name"; then
    echo "expected unrelated container $name to remain present" >&2
    exit 1
  fi
done

assert_loopback_publish() {
  local container="$1"
  local container_port="$2"
  local expected_host_port="$3"
  python3 - "$container" "$container_port" "$expected_host_port" <<'PY'
import json, subprocess, sys

container, container_port, expected_host_port = sys.argv[1], sys.argv[2], sys.argv[3]
raw = subprocess.check_output(
    ["docker", "inspect", "--format", "{{json .NetworkSettings.Ports}}", container],
    text=True,
)
ports = json.loads(raw) or {}
entries = ports.get(container_port) or []
if not entries:
    raise SystemExit(f"{container} has no actual host publish for {container_port}")
allowed = {"127.0.0.1", "::1"}
forbidden = {"", "0.0.0.0", "::", "*"}
for entry in entries:
    host_ip = entry.get("HostIp") or ""
    host_port = str(entry.get("HostPort") or "")
    if host_ip in forbidden or host_ip not in allowed:
        raise SystemExit(
            f"{container} published {container_port} on {host_ip or 'all-interfaces'}"
        )
    if host_port != expected_host_port:
        raise SystemExit(
            f"{container} published {container_port} on {host_ip}:{host_port}, expected {expected_host_port}"
        )

actual = subprocess.check_output(
    ["docker", "port", container, container_port],
    text=True,
).replace("\r", "").strip()
expected = f"127.0.0.1:{expected_host_port}"
if actual != expected and expected not in actual.splitlines():
    raise SystemExit(f"{container} docker port {container_port} is {actual!r}, expected {expected}")
PY
}

http_code() {
  curl --noproxy '*' -sS --max-time 8 \
    --output "$WORK_DIR/body" --write-out '%{http_code}' \
    --dump-header "$WORK_DIR/headers" "$@"
}

client_count() {
  docker exec "$DB_CONTAINER" \
    psql -U postgres -d postgres -tAc \
    "SELECT count(*) FROM opmux_private.clients WHERE display_name = '$TENANT_NAME'"
}

export CONTAINER_IMAGE="$IMAGE"
export OPMUX_LOCAL_GATEWAY_PORT="$GATEWAY_HOST_PORT"
export OPMUX_LOCAL_SIMULATOR_PORT="$SIMULATOR_HOST_PORT"
export OPMUX_LOCAL_PROVIDER_KEY="$DUMMY_PROVIDER_KEY"
if [ -n "${SKIP_IMAGE_BUILD:-}" ]; then
  export OPMUX_LOCAL_BUILD=0
fi

STARTED_STACK=1
bash "$ROOT_DIR/scripts/local-stack.sh" up

assert_loopback_publish "$DB_CONTAINER" "5432/tcp" "55432"
assert_loopback_publish "$GATEWAY_CONTAINER" "3000/tcp" "$GATEWAY_HOST_PORT"
assert_loopback_publish "$SIMULATOR_CONTAINER" "38081/tcp" "$SIMULATOR_HOST_PORT"

if [ "$GATEWAY_HOST_PORT" -lt 38080 ] || [ "$GATEWAY_HOST_PORT" -gt 38089 ]; then
  echo "gateway host port must be in 38080-38089" >&2
  exit 1
fi
if [ "$SIMULATOR_HOST_PORT" -lt 38080 ] || [ "$SIMULATOR_HOST_PORT" -gt 38089 ]; then
  echo "simulator host port must be in 38080-38089" >&2
  exit 1
fi

health_status="$(http_code "http://127.0.0.1:$GATEWAY_HOST_PORT/health")"
ready_status="$(http_code "http://127.0.0.1:$GATEWAY_HOST_PORT/ready")"
metrics_status="$(http_code "http://127.0.0.1:$GATEWAY_HOST_PORT/metrics")"
if [ "$health_status" != "200" ] || [ "$ready_status" != "200" ] || [ "$metrics_status" != "200" ]; then
  echo "expected loopback health/ready/metrics 200 from the local stack" >&2
  exit 1
fi
if ! grep -q 'gateway_http_requests_total' "$WORK_DIR/body"; then
  echo "metrics scrape must return Prometheus series" >&2
  exit 1
fi

models_status="$(http_code -H "Authorization: Bearer $DUMMY_PROVIDER_KEY" \
  "http://127.0.0.1:$SIMULATOR_HOST_PORT/v1/models")"
if [ "$models_status" != "200" ]; then
  echo "simulator /models must be reachable on loopback" >&2
  exit 1
fi

invalid_status="$(http_code -X POST "http://127.0.0.1:$GATEWAY_HOST_PORT/api/v1/route" \
  -H 'Content-Type: application/json' \
  -H 'X-API-Key: test-api-key-123' \
  -d '{"prompt":"local-stack-check","metadata":{}}')"
if [ "$invalid_status" != "401" ]; then
  echo "invalid keys must return 401" >&2
  exit 1
fi

bash "$ROOT_DIR/scripts/local-stack.sh" admin tenant create --name "$TENANT_NAME" >"$TENANT_JSON"
chmod 600 "$TENANT_JSON"
python3 -c '
import json, pathlib, sys
tenant = json.loads(pathlib.Path(sys.argv[1]).read_text())
if not tenant.get("credential", "").startswith("opmx_v1_"):
    raise SystemExit("tenant create did not return a credential")
pathlib.Path(sys.argv[2]).write_text(tenant["client_id"])
' "$TENANT_JSON" "$WORK_DIR/client-id"
bash "$ROOT_DIR/scripts/local-stack.sh" admin key issue \
  --client-id "$(cat "$WORK_DIR/client-id")" \
  --kind inference --name local-stack-route >"$INFERENCE_JSON"
chmod 600 "$INFERENCE_JSON"
python3 -c '
import json, pathlib, sys
issued = json.loads(pathlib.Path(sys.argv[1]).read_text())
credential = issued.get("credential", "")
if not credential.startswith("opmx_v1_"):
    raise SystemExit("inference issue did not return a credential")
pathlib.Path(sys.argv[2]).write_text(credential)
' "$INFERENCE_JSON" "$WORK_DIR/inference.key"
chmod 600 "$WORK_DIR/inference.key"

route_status="$(http_code -X POST "http://127.0.0.1:$GATEWAY_HOST_PORT/api/v1/route" \
  -H 'Content-Type: application/json' \
  -H "X-API-Key: $(cat "$WORK_DIR/inference.key")" \
  -d '{"prompt":"local-stack-check","metadata":{}}')"
python3 -c '
import json, pathlib, sys
status = sys.argv[1]
body = json.loads(pathlib.Path(sys.argv[2]).read_text())
if status != "200":
    raise SystemExit("authenticated generation must return 200")
response = body.get("response") or {}
if response.get("content") != "SIMULATED_OPENAI_OK":
    raise SystemExit("simulated content missing")
if body.get("model_used") != "example-chat-model":
    raise SystemExit("provider-reported model missing")
if "error" in body:
    raise SystemExit("success payload must not include an error")
' "$route_status" "$WORK_DIR/body"

before_stop_count="$(client_count)"
if [ "$before_stop_count" != "1" ]; then
  echo "expected the provisioned local-stack client to exist before stop" >&2
  exit 1
fi

bash "$ROOT_DIR/scripts/local-stack.sh" down
STARTED_STACK=0

if docker inspect "$GATEWAY_CONTAINER" >/dev/null 2>&1; then
  echo "gateway container must be removed on stack stop" >&2
  exit 1
fi
if docker inspect "$SIMULATOR_CONTAINER" >/dev/null 2>&1; then
  echo "simulator container must be removed on stack stop" >&2
  exit 1
fi
if ! docker inspect -f '{{.State.Running}}' "$DB_CONTAINER" | grep -q true; then
  echo "owned supabase must keep running after application-stack stop" >&2
  exit 1
fi
published_after="$(docker port "$DB_CONTAINER" 5432/tcp | tr -d '\r')"
if [ "$published_after" != "127.0.0.1:55432" ]; then
  echo "owned supabase loopback binding changed after stack stop" >&2
  exit 1
fi
after_stop_count="$(client_count)"
if [ "$after_stop_count" != "1" ]; then
  echo "application-stack stop must preserve database records" >&2
  exit 1
fi

unrelated_after_stop="$(docker ps -a --format '{{.Names}}' | grep -v -e '^opmux-local-' -e '^$' | LC_ALL=C sort)"
if [ "$unrelated_before" != "$unrelated_after_stop" ]; then
  echo "stop must not create or remove unrelated containers" >&2
  exit 1
fi

STARTED_STACK=1
bash "$ROOT_DIR/scripts/local-stack.sh" up

ready_after="$(http_code "http://127.0.0.1:$GATEWAY_HOST_PORT/ready")"
route_after="$(http_code -X POST "http://127.0.0.1:$GATEWAY_HOST_PORT/api/v1/route" \
  -H 'Content-Type: application/json' \
  -H "X-API-Key: $(cat "$WORK_DIR/inference.key")" \
  -d '{"prompt":"local-stack-restart","metadata":{}}')"
python3 -c '
import json, pathlib, sys
ready, status = sys.argv[1], sys.argv[2]
body = json.loads(pathlib.Path(sys.argv[3]).read_text())
if ready != "200":
    raise SystemExit("readiness must return after stack restart")
if status != "200":
    raise SystemExit("persisted inference key must work after restart")
if (body.get("response") or {}).get("content") != "SIMULATED_OPENAI_OK":
    raise SystemExit("restart generation must use the local simulator")
' "$ready_after" "$route_after" "$WORK_DIR/body"

after_restart_count="$(client_count)"
if [ "$after_restart_count" != "1" ]; then
  echo "restart must not recreate the local-stack client" >&2
  exit 1
fi

bash "$ROOT_DIR/scripts/local-stack.sh" down
STARTED_STACK=0

unrelated_after="$(docker ps -a --format '{{.Names}}' | grep -v -e '^opmux-local-' -e '^$' | LC_ALL=C sort)"
if [ "$unrelated_before" != "$unrelated_after" ]; then
  echo "cleanup must not touch unrelated containers" >&2
  exit 1
fi
if ! docker inspect -f '{{.State.Running}}' "$DB_CONTAINER" | grep -q true; then
  echo "owned supabase must remain running after cleanup" >&2
  exit 1
fi

echo "local stack check passed: loopback publishes, reused supabase, restart retained records"

#!/bin/bash
# Build and exercise the locked non-root gateway image against owned local
# Supabase and owned loopback simulators. Does not call real providers.
# Credentials, connection strings, and digests are not printed.
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
IMAGE="${CONTAINER_IMAGE:-opmux-gateway:mvp}"
GATEWAY_HOST_PORT="${CONTAINER_GATEWAY_PORT:-38086}"
HTTP_SIM_PORT="${CONTAINER_SIMULATOR_PORT:-38087}"
TLS_SIM_PORT="${CONTAINER_TLS_PORT:-38088}"
NETWORK=opmux-mvp-20260919-loopback
DB_CONTAINER=supabase_db_opmux-mvp-20260919
GATEWAY_CONTAINER=opmux-check-gateway-38086
DUMMY_PROVIDER_KEY=container-check-dummy-only
WORK_DIR="$(mktemp -d "${TMPDIR:-/tmp}/opmux-container-check.XXXXXX")"
chmod 700 "$WORK_DIR"
HTTP_SIM_PID=""
TLS_SIM_PID=""
HTTP_COUNT_FILE="$WORK_DIR/http-counts.json"
TLS_COUNT_FILE="$WORK_DIR/tls-counts.json"
TENANT_JSON="$WORK_DIR/tenant.json"
INFERENCE_JSON="$WORK_DIR/inference.json"
CONTAINER_DB_URL_FILE="$WORK_DIR/database-url"
chmod 600 "$TENANT_JSON" "$INFERENCE_JSON" 2>/dev/null || true
touch "$TENANT_JSON" "$INFERENCE_JSON" "$CONTAINER_DB_URL_FILE"
chmod 600 "$TENANT_JSON" "$INFERENCE_JSON" "$CONTAINER_DB_URL_FILE"

cleanup() {
  local status=$?
  if [ -n "$HTTP_SIM_PID" ]; then
    kill "$HTTP_SIM_PID" 2>/dev/null || true
    wait "$HTTP_SIM_PID" 2>/dev/null || true
  fi
  if [ -n "$TLS_SIM_PID" ]; then
    kill "$TLS_SIM_PID" 2>/dev/null || true
    wait "$TLS_SIM_PID" 2>/dev/null || true
  fi
  if docker inspect "$GATEWAY_CONTAINER" >/dev/null 2>&1; then
    docker stop --time 8 "$GATEWAY_CONTAINER" >/dev/null 2>&1 || true
    docker rm -f "$GATEWAY_CONTAINER" >/dev/null 2>&1 || true
  fi
  rm -rf "$WORK_DIR"
  if [ "$status" -ne 0 ]; then
    echo "container check failed" >&2
  fi
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM

require_loopback_port() {
  local port="$1"
  if lsof -nP -iTCP:"$port" -sTCP:LISTEN >/dev/null 2>&1; then
    echo "port 127.0.0.1:$port is already in use" >&2
    exit 1
  fi
}

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

require_loopback_port "$GATEWAY_HOST_PORT"
require_loopback_port "$HTTP_SIM_PORT"
require_loopback_port "$TLS_SIM_PORT"

if [ -z "${SKIP_IMAGE_BUILD:-}" ]; then
  docker build --file "$ROOT_DIR/gateway/Dockerfile" --tag "$IMAGE" "$ROOT_DIR"
fi

image_user="$(docker image inspect --format '{{.Config.User}}' "$IMAGE")"
if [ "$image_user" != "65532:65532" ]; then
  echo "image user must be 65532:65532" >&2
  exit 1
fi

image_env="$(docker image inspect --format '{{range .Config.Env}}{{println .}}{{end}}' "$IMAGE")"
if printf '%s\n' "$image_env" | grep -Eq '^(DATABASE_URL|OPENAI_API_KEY)='; then
  echo "image must not bake database or provider secrets" >&2
  exit 1
fi
if printf '%s\n' "$image_env" | grep -Eq '^AUTH_DEVELOPMENT_MODE=true$'; then
  echo "image must not enable AUTH_DEVELOPMENT_MODE" >&2
  exit 1
fi
if printf '%s\n' "$image_env" | grep -Eq 'opmx_v1_|postgresql://'; then
  echo "image environment must not contain credentials or connection URLs" >&2
  exit 1
fi

docker run --rm --entrypoint sh "$IMAGE" -c '
  set -e
  test "$(id -u)" = "65532"
  test -x /usr/local/bin/gateway
  test -x /usr/local/bin/opmux-admin
  test -s /etc/ssl/certs/ca-certificates.crt
  test -e /usr/lib/x86_64-linux-gnu/libssl.so.3 -o -e /usr/lib/aarch64-linux-gnu/libssl.so.3
  ldd /usr/local/bin/gateway | grep -q libssl.so
  grep -q example-chat-model /app/config/opmux.example.json
  test ! -f /app/.env
  test ! -f /.env
' >/dev/null

pw="$(docker exec "$DB_CONTAINER" printenv POSTGRES_PASSWORD)"
enc="$(
  PW="$pw" python3 -c 'import os, urllib.parse; print(urllib.parse.quote(os.environ["PW"], safe=""))'
)"
unset pw
OUT="$CONTAINER_DB_URL_FILE" URL="postgresql://postgres:${enc}@${DB_CONTAINER}:5432/postgres?sslmode=disable" \
  python3 -c 'import os, pathlib; pathlib.Path(os.environ["OUT"]).write_text(os.environ["URL"])'
unset enc
chmod 600 "$CONTAINER_DB_URL_FILE"

python3 "$ROOT_DIR/scripts/openai-simulator.py" \
  --host 127.0.0.1 --port "$HTTP_SIM_PORT" \
  --credential "$DUMMY_PROVIDER_KEY" \
  --count-file "$HTTP_COUNT_FILE" &
HTTP_SIM_PID=$!
sh "$ROOT_DIR/scripts/generate-untrusted-tls.sh" "$WORK_DIR/tls"
python3 "$ROOT_DIR/scripts/openai-simulator.py" \
  --host 127.0.0.1 --port "$TLS_SIM_PORT" \
  --credential "$DUMMY_PROVIDER_KEY" \
  --tls-cert "$WORK_DIR/tls/cert.pem" --tls-key "$WORK_DIR/tls/key.pem" \
  --count-file "$TLS_COUNT_FILE" &
TLS_SIM_PID=$!

sim_ready=false
for _ in $(seq 1 20); do
  if ! kill -0 "$HTTP_SIM_PID" 2>/dev/null || ! kill -0 "$TLS_SIM_PID" 2>/dev/null; then
    echo "owned simulators failed to start" >&2
    exit 1
  fi
  if curl --noproxy '*' -fsS --max-time 1 \
    -H "Authorization: Bearer $DUMMY_PROVIDER_KEY" \
    "http://127.0.0.1:$HTTP_SIM_PORT/v1/models" >/dev/null 2>&1; then
    sim_ready=true
    break
  fi
  sleep 0.1
done
if [ "$sim_ready" != true ]; then
  echo "HTTP simulator did not become ready" >&2
  exit 1
fi

start_gateway() {
  local base_url="$1"
  docker rm -f "$GATEWAY_CONTAINER" >/dev/null 2>&1 || true
  docker run -d --name "$GATEWAY_CONTAINER" \
    --user 65532:65532 \
    --network "$NETWORK" \
    --add-host=untrusted.opmux.test:host-gateway \
    --publish "127.0.0.1:${GATEWAY_HOST_PORT}:3000" \
    --env-file "$WORK_DIR/gateway.env" \
    -e "OPENAI_BASE_URL=$base_url" \
    "$IMAGE" >/dev/null
}

write_gateway_env() {
  umask 077
  {
    echo "SERVER_HOST=0.0.0.0"
    echo "SERVER_PORT=3000"
    echo "SERVER_SHUTDOWN_TIMEOUT=5"
    echo "AUTH_DEVELOPMENT_MODE=false"
    echo "OPMUX_CONFIG_FILE=/app/config/opmux.example.json"
    echo "OPENAI_API_KEY=$DUMMY_PROVIDER_KEY"
    echo "OPENAI_TIMEOUT_MS=2000"
    echo "EXECUTOR_MAX_RETRIES=0"
    echo "OPMUX_MAX_TOTAL_ATTEMPTS=1"
    echo "METRICS_ENABLED=true"
    echo "METRICS_PATH=/metrics"
    echo "LOG_FORMAT=json"
    echo "RUST_LOG=info"
    echo "TOKIO_WORKER_THREADS=2"
    echo "NO_PROXY=*"
    echo "no_proxy=*"
    printf 'DATABASE_URL=%s\n' "$(cat "$CONTAINER_DB_URL_FILE")"
  } >"$WORK_DIR/gateway.env"
  chmod 600 "$WORK_DIR/gateway.env"
}

wait_health() {
  local i
  for i in $(seq 1 60); do
    if ! docker inspect -f '{{.State.Running}}' "$GATEWAY_CONTAINER" 2>/dev/null | grep -q true; then
      echo "gateway container is not running" >&2
      return 1
    fi
    if curl --noproxy '*' -fsS --max-time 1 "http://127.0.0.1:$GATEWAY_HOST_PORT/health" >/dev/null 2>&1; then
      return 0
    fi
    sleep 0.25
  done
  echo "gateway did not become live" >&2
  return 1
}

http_code() {
  curl --noproxy '*' -sS --max-time 8 \
    --output "$WORK_DIR/body" --write-out '%{http_code}' \
    --dump-header "$WORK_DIR/headers" "$@"
}

write_gateway_env
start_gateway "http://host.docker.internal:${HTTP_SIM_PORT}/v1"
wait_health

process_uid="$(docker exec "$GATEWAY_CONTAINER" id -u)"
if [ "$process_uid" = "0" ]; then
  echo "gateway process must not run as root" >&2
  exit 1
fi
if [ "$process_uid" != "65532" ]; then
  echo "gateway process UID must be 65532" >&2
  exit 1
fi

binding="$(docker port "$GATEWAY_CONTAINER" 3000/tcp | tr -d '\r')"
if [ "$binding" != "127.0.0.1:$GATEWAY_HOST_PORT" ]; then
  echo "gateway host publish must be loopback in the approved range" >&2
  exit 1
fi

health_status="$(http_code "http://127.0.0.1:$GATEWAY_HOST_PORT/health")"
ready_status="$(http_code "http://127.0.0.1:$GATEWAY_HOST_PORT/ready")"
if [ "$health_status" != "200" ] || [ "$ready_status" != "200" ]; then
  echo "expected health/ready 200 from the container" >&2
  exit 1
fi

generation_before="$(python3 -c 'import json,pathlib,sys; p=pathlib.Path(sys.argv[1]); print(json.loads(p.read_text()).get("generation",0) if p.is_file() else 0)' "$HTTP_COUNT_FILE")"
invalid_status="$(http_code -X POST "http://127.0.0.1:$GATEWAY_HOST_PORT/api/v1/route" \
  -H 'Content-Type: application/json' \
  -H 'X-API-Key: test-api-key-123' \
  -d '{"prompt":"container-check","metadata":{}}')"
unknown_status="$(http_code -X POST "http://127.0.0.1:$GATEWAY_HOST_PORT/api/v1/route" \
  -H 'Content-Type: application/json' \
  -H 'X-API-Key: invalid-container-key' \
  -d '{"prompt":"container-check","metadata":{}}')"
if [ "$invalid_status" != "401" ] || [ "$unknown_status" != "401" ]; then
  echo "invalid keys must return 401" >&2
  exit 1
fi
generation_after_deny="$(python3 -c 'import json,pathlib,sys; p=pathlib.Path(sys.argv[1]); print(json.loads(p.read_text()).get("generation",0) if p.is_file() else 0)' "$HTTP_COUNT_FILE")"
if [ "$generation_after_deny" != "$generation_before" ]; then
  echo "unauthorized requests must not call the simulator" >&2
  exit 1
fi

docker run --rm --network "$NETWORK" \
  --user 65532:65532 \
  --env-file "$WORK_DIR/gateway.env" \
  "$IMAGE" opmux-admin tenant create --name "container-check-$$" >"$TENANT_JSON"
chmod 600 "$TENANT_JSON"
python3 -c '
import json, os, pathlib, sys
tenant = json.loads(pathlib.Path(sys.argv[1]).read_text())
client_id = tenant["client_id"]
pathlib.Path(sys.argv[2]).write_text(client_id)
if not tenant.get("credential", "").startswith("opmx_v1_"):
    raise SystemExit("tenant create did not return a credential")
' "$TENANT_JSON" "$WORK_DIR/client-id"
docker run --rm --network "$NETWORK" \
  --user 65532:65532 \
  --env-file "$WORK_DIR/gateway.env" \
  "$IMAGE" opmux-admin key issue \
  --client-id "$(cat "$WORK_DIR/client-id")" \
  --kind inference --name container-route >"$INFERENCE_JSON"
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
  -d '{"prompt":"container-check","metadata":{}}')"
python3 -c '
import json, pathlib, sys
status = sys.argv[1]
body = json.loads(pathlib.Path(sys.argv[2]).read_text())
if status != "200":
    raise SystemExit("authenticated generation must return 200")
response = body.get("response") or {}
if response.get("content") != "SIMULATED_OPENAI_OK":
    raise SystemExit("simulated content missing")
if response.get("role") != "assistant":
    raise SystemExit("assistant role missing")
if body.get("model_used") != "example-chat-model":
    raise SystemExit("provider-reported model missing")
if "error" in body:
    raise SystemExit("success payload must not include an error")
' "$route_status" "$WORK_DIR/body"
generation_after_ok="$(python3 -c 'import json,pathlib,sys; print(json.loads(pathlib.Path(sys.argv[1]).read_text()).get("generation",0))' "$HTTP_COUNT_FILE")"
if [ "$generation_after_ok" -le "$generation_after_deny" ]; then
  echo "authenticated generation must reach the simulator" >&2
  exit 1
fi

docker stop --time 8 "$GATEWAY_CONTAINER" >/dev/null
if docker inspect -f '{{.State.Running}}' "$GATEWAY_CONTAINER" | grep -q true; then
  echo "owned gateway container did not stop" >&2
  exit 1
fi
docker rm -f "$GATEWAY_CONTAINER" >/dev/null

tls_before="$(python3 -c 'import json,pathlib,sys; p=pathlib.Path(sys.argv[1]); print(json.loads(p.read_text()).get("generation",0) if p.is_file() else 0)' "$TLS_COUNT_FILE")"
start_gateway "https://untrusted.opmux.test:${TLS_SIM_PORT}/v1"
wait_health
tls_ready="$(http_code "http://127.0.0.1:$GATEWAY_HOST_PORT/ready")"
tls_status="$(http_code -X POST "http://127.0.0.1:$GATEWAY_HOST_PORT/api/v1/route" \
  -H 'Content-Type: application/json' \
  -H "X-API-Key: $(cat "$WORK_DIR/inference.key")" \
  -d '{"prompt":"container-tls-check","metadata":{}}')"
python3 -c '
import json, pathlib, sys
status = sys.argv[1]
ready = sys.argv[2]
body = json.loads(pathlib.Path(sys.argv[3]).read_text())
if ready == "200":
    raise SystemExit("untrusted TLS upstream must not report ready")
if status != "502":
    raise SystemExit("untrusted TLS must fail as sanitized upstream 502")
error = body.get("error") or {}
if error.get("code") != "UPSTREAM_ERROR":
    raise SystemExit("untrusted TLS must use UPSTREAM_ERROR")
text = json.dumps(body)
if "BEGIN CERTIFICATE" in text or "untrusted.opmux.test" in text:
    raise SystemExit("TLS failure leaked certificate details")
if body.get("response"):
    raise SystemExit("untrusted TLS must not return a success payload")
' "$tls_status" "$tls_ready" "$WORK_DIR/body"
tls_after="$(python3 -c 'import json,pathlib,sys; p=pathlib.Path(sys.argv[1]); print(json.loads(p.read_text()).get("generation",0) if p.is_file() else 0)' "$TLS_COUNT_FILE")"
if [ "$tls_after" != "$tls_before" ]; then
  echo "untrusted TLS must not complete a simulator generation" >&2
  exit 1
fi

docker stop --time 8 "$GATEWAY_CONTAINER" >/dev/null
docker rm -f "$GATEWAY_CONTAINER" >/dev/null

echo "container check passed: locked non-root image, persisted-auth generation, invalid-key 401, untrusted TLS 502"

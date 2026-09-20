#!/bin/bash
# Start, stop, and inspect the documented loopback local stack.
# Reuses supabase_db_opmux-mvp-20260919. Does not create a second database,
# publish all interfaces, call real providers, or stop unrelated containers.
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
COMPOSE_FILE="$ROOT_DIR/docker-compose.yml"
ENV_FILE="$ROOT_DIR/.env.opmux-local"
NETWORK=opmux-mvp-20260919-loopback
DB_CONTAINER=supabase_db_opmux-mvp-20260919
GATEWAY_CONTAINER=opmux-local-gateway
SIMULATOR_CONTAINER=opmux-local-simulator
GATEWAY_HOST_PORT="${OPMUX_LOCAL_GATEWAY_PORT:-38080}"
SIMULATOR_HOST_PORT="${OPMUX_LOCAL_SIMULATOR_PORT:-38081}"
PROVIDER_KEY="${OPMUX_LOCAL_PROVIDER_KEY:-local-stack-dummy-only}"

usage() {
  echo "usage: bash scripts/local-stack.sh up|down|status|bindings|admin [-- args]" >&2
  exit 2
}

compose() {
  docker compose \
    --project-directory "$ROOT_DIR" \
    -f "$COMPOSE_FILE" \
    --project-name opmux-local \
    --env-file "$ENV_FILE" \
    "$@"
}

require_owned_database() {
  local published
  if ! published="$(docker port "$DB_CONTAINER" 5432/tcp 2>/dev/null)"; then
    echo "owned supabase container $DB_CONTAINER is not running on 127.0.0.1:55432" >&2
    echo "start that database-only container; do not run supabase start from this repo" >&2
    exit 1
  fi
  published=$(printf '%s' "$published" | tr -d '\r')
  if [ "$published" != "127.0.0.1:55432" ]; then
    echo "owned supabase is not bound to 127.0.0.1:55432" >&2
    exit 1
  fi
  if ! docker exec "$DB_CONTAINER" pg_isready -U postgres -d postgres >/dev/null 2>&1; then
    echo "owned supabase is not accepting connections" >&2
    exit 1
  fi
  if ! docker network inspect "$NETWORK" >/dev/null 2>&1; then
    echo "owned docker network $NETWORK is missing" >&2
    exit 1
  fi
}

write_env_file() {
  local pw enc url
  if ! command -v python3 >/dev/null 2>&1; then
    echo "python3 is required to encode the owned database password" >&2
    exit 1
  fi
  pw="$(docker exec "$DB_CONTAINER" printenv POSTGRES_PASSWORD)"
  if ! enc="$(
    PW="$pw" python3 -c 'import os, urllib.parse; print(urllib.parse.quote(os.environ["PW"], safe=""))'
  )"; then
    echo "failed to encode owned database password" >&2
    exit 1
  fi
  unset pw
  url="postgresql://postgres:${enc}@${DB_CONTAINER}:5432/postgres?sslmode=disable"
  unset enc
  umask 077
  {
    printf 'OPMUX_LOCAL_PROVIDER_KEY=%s\n' "$PROVIDER_KEY"
    printf 'OPMUX_LOCAL_GATEWAY_PORT=%s\n' "$GATEWAY_HOST_PORT"
    printf 'OPMUX_LOCAL_SIMULATOR_PORT=%s\n' "$SIMULATOR_HOST_PORT"
    printf 'OPMUX_LOCAL_DATABASE_URL=%s\n' "$url"
  } >"$ENV_FILE"
  chmod 600 "$ENV_FILE"
}

port_in_approved_range() {
  local port="$1"
  [ "$port" -ge 38080 ] && [ "$port" -le 38089 ]
}

require_free_or_owned_port() {
  local port="$1"
  local container="$2"
  if ! lsof -nP -iTCP:"$port" -sTCP:LISTEN >/dev/null 2>&1; then
    return 0
  fi
  if docker inspect -f '{{.State.Running}}' "$container" 2>/dev/null | grep -q true; then
    local binding
    binding="$(docker port "$container" 2>/dev/null | tr -d '\r' || true)"
    if printf '%s\n' "$binding" | grep -q "127.0.0.1:${port}"; then
      return 0
    fi
  fi
  echo "port 127.0.0.1:$port is already in use by a process this stack does not own" >&2
  exit 1
}

wait_http() {
  local url="$1"
  shift
  local i
  for i in $(seq 1 60); do
    if curl --noproxy '*' -fsS --max-time 2 "$@" "$url" >/dev/null 2>&1; then
      return 0
    fi
    sleep 0.25
  done
  echo "timed out waiting for $url" >&2
  return 1
}

assert_running() {
  if ! docker inspect -f '{{.State.Running}}' "$1" 2>/dev/null | grep -q true; then
    echo "container $1 is not running" >&2
    return 1
  fi
}

cmd_up() {
  require_owned_database
  if ! port_in_approved_range "$GATEWAY_HOST_PORT" || ! port_in_approved_range "$SIMULATOR_HOST_PORT"; then
    echo "gateway and simulator host ports must be in 38080-38089" >&2
    exit 1
  fi
  require_free_or_owned_port "$GATEWAY_HOST_PORT" "$GATEWAY_CONTAINER"
  require_free_or_owned_port "$SIMULATOR_HOST_PORT" "$SIMULATOR_CONTAINER"
  write_env_file

  local gateway_image="${CONTAINER_IMAGE:-opmux-gateway:mvp}"
  if [ "${OPMUX_LOCAL_BUILD:-auto}" = "1" ]; then
    compose build
  else
    if ! docker image inspect opmux-simulator:local >/dev/null 2>&1; then
      compose build simulator
    fi
    if ! docker image inspect "$gateway_image" >/dev/null 2>&1; then
      if [ "${OPMUX_LOCAL_BUILD:-auto}" = "0" ]; then
        echo "gateway image is missing; run docker build --file gateway/Dockerfile --tag opmux-gateway:mvp ." >&2
        exit 1
      fi
      compose build gateway
    fi
  fi

  compose up -d --no-build
  assert_running "$SIMULATOR_CONTAINER"
  assert_running "$GATEWAY_CONTAINER"

  wait_http "http://127.0.0.1:${SIMULATOR_HOST_PORT}/v1/models" \
    -H "Authorization: Bearer ${PROVIDER_KEY}"
  wait_http "http://127.0.0.1:${GATEWAY_HOST_PORT}/health"
  wait_http "http://127.0.0.1:${GATEWAY_HOST_PORT}/ready"
  cmd_bindings
}

cmd_down() {
  if [ -f "$ENV_FILE" ]; then
    compose down --timeout 8
  elif docker inspect "$GATEWAY_CONTAINER" >/dev/null 2>&1 \
    || docker inspect "$SIMULATOR_CONTAINER" >/dev/null 2>&1; then
    docker compose \
      --project-directory "$ROOT_DIR" \
      -f "$COMPOSE_FILE" \
      --project-name opmux-local \
      down --timeout 8
  fi
  rm -f "$ENV_FILE"
  if docker inspect -f '{{.State.Running}}' "$DB_CONTAINER" 2>/dev/null | grep -q true; then
    local published
    published="$(docker port "$DB_CONTAINER" 5432/tcp | tr -d '\r')"
    if [ "$published" != "127.0.0.1:55432" ]; then
      echo "owned supabase loopback binding changed during stack stop" >&2
      exit 1
    fi
  fi
}

cmd_bindings() {
  require_owned_database
  local db_bind gateway_bind simulator_bind
  db_bind="$(docker port "$DB_CONTAINER" 5432/tcp | tr -d '\r')"
  gateway_bind="$(docker port "$GATEWAY_CONTAINER" 3000/tcp | tr -d '\r')"
  simulator_bind="$(docker port "$SIMULATOR_CONTAINER" 38081/tcp | tr -d '\r')"
  printf 'database %s\n' "$db_bind"
  printf 'gateway %s (health/ready/metrics)\n' "$gateway_bind"
  printf 'simulator %s\n' "$simulator_bind"
  python3 - "$db_bind" "$gateway_bind" "$simulator_bind" "$GATEWAY_HOST_PORT" "$SIMULATOR_HOST_PORT" <<'PY'
import sys

db, gateway, simulator, gateway_port, simulator_port = sys.argv[1:6]
expected = {
    "database": "127.0.0.1:55432",
    "gateway": f"127.0.0.1:{gateway_port}",
    "simulator": f"127.0.0.1:{simulator_port}",
}
actual = {"database": db, "gateway": gateway, "simulator": simulator}
for name, value in actual.items():
    if value != expected[name]:
        raise SystemExit(f"{name} host publish is {value!r}, expected {expected[name]}")
    if value.startswith("0.0.0.0:") or value.startswith("[::]:"):
        raise SystemExit(f"{name} must not publish all interfaces")
PY
}

cmd_status() {
  require_owned_database
  compose ps
  cmd_bindings
}

cmd_admin() {
  require_owned_database
  if [ ! -f "$ENV_FILE" ]; then
    write_env_file
  fi
  compose run --rm --no-deps -T --user 65532:65532 gateway opmux-admin "$@"
}

if [ "$#" -lt 1 ]; then
  usage
fi

case "$1" in
  up) cmd_up ;;
  down) cmd_down ;;
  status) cmd_status ;;
  bindings) cmd_bindings ;;
  admin)
    shift
    cmd_admin "$@"
    ;;
  *) usage ;;
esac

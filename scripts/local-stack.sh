#!/bin/bash
# Start, stop, and inspect the documented loopback local stack.
# Reuses supabase_db_opmux-mvp-20260919. Does not create a second database,
# publish all interfaces, call real providers, or stop unrelated containers.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
ROOT_DIR="$(cd "$SCRIPT_DIR/.." && pwd -P)"
COMPOSE_FILE="$ROOT_DIR/docker-compose.yml"
GENERATED_ENV_FILE=
NETWORK=opmux-mvp-20260919-loopback
DB_CONTAINER=supabase_db_opmux-mvp-20260919
GATEWAY_CONTAINER=opmux-local-gateway
SIMULATOR_CONTAINER=opmux-local-simulator
COMPOSE_PROJECT=opmux-local
GATEWAY_IMAGE="${CONTAINER_IMAGE:-opmux-gateway:mvp}"
GATEWAY_HOST_PORT="${OPMUX_LOCAL_GATEWAY_PORT:-38080}"
SIMULATOR_HOST_PORT="${OPMUX_LOCAL_SIMULATOR_PORT:-38081}"
PROVIDER_KEY="${OPMUX_LOCAL_PROVIDER_KEY:-local-stack-dummy-only}"
LOCAL_CONFIG_HOST="${OPMUX_LOCAL_CONFIG_HOST:-$ROOT_DIR/config/opmux.local-stack.json}"
GENERATED_ENV_DIR=
OWNED_DATABASE_URL=

usage() {
  echo "usage: bash scripts/local-stack.sh up|down|status|bindings|admin [-- args]" >&2
  exit 2
}

canonical_path() {
  python3 -c 'import os, sys; print(os.path.realpath(sys.argv[1]))' "$1"
}

cleanup_generated_env() {
  if [ -n "${GENERATED_ENV_DIR:-}" ] && [ -d "$GENERATED_ENV_DIR" ]; then
    rm -rf "$GENERATED_ENV_DIR"
  fi
  GENERATED_ENV_DIR=
}

trap cleanup_generated_env EXIT

compose() {
  if [ -z "${GENERATED_ENV_FILE:-}" ] || [ ! -f "$GENERATED_ENV_FILE" ]; then
    echo "internal error: generated compose env is missing" >&2
    exit 1
  fi
  env \
    -u COMPOSE_FILE \
    -u COMPOSE_PROJECT_NAME \
    -u COMPOSE_ENV_FILES \
    CONTAINER_IMAGE="$GATEWAY_IMAGE" \
    OPMUX_LOCAL_DATABASE_URL="$OWNED_DATABASE_URL" \
    OPMUX_LOCAL_PROVIDER_KEY="$PROVIDER_KEY" \
    OPMUX_LOCAL_GATEWAY_PORT="$GATEWAY_HOST_PORT" \
    OPMUX_LOCAL_SIMULATOR_PORT="$SIMULATOR_HOST_PORT" \
    OPMUX_LOCAL_CONFIG_HOST="$LOCAL_CONFIG_HOST" \
    docker compose \
    --project-directory "$ROOT_DIR" \
    -f "$COMPOSE_FILE" \
    --project-name "$COMPOSE_PROJECT" \
    --env-file "$GENERATED_ENV_FILE" \
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

load_owned_database_url() {
  local pw enc
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
  OWNED_DATABASE_URL="postgresql://postgres:${enc}@${DB_CONTAINER}:5432/postgres?sslmode=disable"
  unset enc
}

prepare_generated_env() {
  require_owned_database
  load_owned_database_url
  GENERATED_ENV_DIR="$(mktemp -d "${TMPDIR:-/tmp}/opmux-local-stack.XXXXXX")"
  chmod 700 "$GENERATED_ENV_DIR"
  GENERATED_ENV_FILE="$GENERATED_ENV_DIR/stack.env"
  umask 077
  {
    printf 'CONTAINER_IMAGE=%s\n' "$GATEWAY_IMAGE"
    printf 'OPMUX_LOCAL_PROVIDER_KEY=%s\n' "$PROVIDER_KEY"
    printf 'OPMUX_LOCAL_GATEWAY_PORT=%s\n' "$GATEWAY_HOST_PORT"
    printf 'OPMUX_LOCAL_SIMULATOR_PORT=%s\n' "$SIMULATOR_HOST_PORT"
    printf 'OPMUX_LOCAL_CONFIG_HOST=%s\n' "$LOCAL_CONFIG_HOST"
    printf 'OPMUX_LOCAL_DATABASE_URL=%s\n' "$OWNED_DATABASE_URL"
  } >"$GENERATED_ENV_FILE"
  chmod 600 "$GENERATED_ENV_FILE"
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

container_label() {
  docker inspect -f "{{index .Config.Labels \"$2\"}}" "$1"
}

assert_owned_app_container() {
  local name="$1"
  local expected_service="$2"
  local project service workdir config_files root_real work_real compose_real file_real found
  project="$(container_label "$name" "com.docker.compose.project")"
  service="$(container_label "$name" "com.docker.compose.service")"
  workdir="$(container_label "$name" "com.docker.compose.project.working_dir")"
  config_files="$(container_label "$name" "com.docker.compose.project.config_files")"
  if [ "$project" != "$COMPOSE_PROJECT" ] || [ "$service" != "$expected_service" ]; then
    echo "refusing to stop $name: not the owned $COMPOSE_PROJECT $expected_service container" >&2
    exit 1
  fi
  root_real="$(canonical_path "$ROOT_DIR")"
  compose_real="$(canonical_path "$COMPOSE_FILE")"
  if [ -n "$workdir" ]; then
    work_real="$(canonical_path "$workdir")"
    if [ "$work_real" != "$root_real" ]; then
      echo "refusing to stop $name: working directory is not this repository" >&2
      exit 1
    fi
  fi
  if [ -n "$config_files" ]; then
    found=0
    IFS=','
    for file in $config_files; do
      file_real="$(canonical_path "$file")"
      if [ "$file_real" = "$compose_real" ]; then
        found=1
      fi
    done
    unset IFS
    if [ "$found" -eq 0 ]; then
      echo "refusing to stop $name: compose file is not this repository stack" >&2
      exit 1
    fi
  fi
  if [ -z "$workdir" ] && [ -z "$config_files" ]; then
    echo "refusing to stop $name: missing compose ownership labels" >&2
    exit 1
  fi
}

stop_owned_app_container() {
  local name="$1"
  local expected_service="$2"
  local exit_code
  if ! docker inspect "$name" >/dev/null 2>&1; then
    return 0
  fi
  assert_owned_app_container "$name" "$expected_service"
  if docker inspect -f '{{.State.Running}}' "$name" 2>/dev/null | grep -q true; then
    # Fixed 605s ceiling covers the 600s application maximum plus margin.
    docker stop --time 605 "$name" >/dev/null
  fi
  exit_code="$(docker inspect -f '{{.State.ExitCode}}' "$name")"
  echo "$name exited $exit_code"
  if [ "$exit_code" = "137" ]; then
    echo "container $name received SIGKILL; docker stop undercut graceful shutdown" >&2
    exit 1
  fi
  docker rm "$name" >/dev/null
}

assert_database_untouched() {
  local published
  if docker inspect -f '{{.State.Running}}' "$DB_CONTAINER" 2>/dev/null | grep -q true; then
    published="$(docker port "$DB_CONTAINER" 5432/tcp | tr -d '\r')"
    if [ "$published" != "127.0.0.1:55432" ]; then
      echo "owned supabase loopback binding changed during stack stop" >&2
      exit 1
    fi
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
  prepare_generated_env

  if [ "${OPMUX_LOCAL_BUILD:-auto}" = "1" ]; then
    compose build
  else
    if ! docker image inspect opmux-simulator:local >/dev/null 2>&1; then
      compose build simulator
    fi
    if ! docker image inspect "$GATEWAY_IMAGE" >/dev/null 2>&1; then
      if [ "${OPMUX_LOCAL_BUILD:-auto}" = "0" ]; then
        echo "selected gateway image $GATEWAY_IMAGE is missing; build it with docker build --file gateway/Dockerfile --tag $GATEWAY_IMAGE . or omit OPMUX_LOCAL_BUILD=0" >&2
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

stop_owned_one_off_containers() {
  local id name service
  while IFS= read -r id; do
    [ -n "$id" ] || continue
    if ! docker inspect "$id" >/dev/null 2>&1; then
      continue
    fi
    name="$(docker inspect -f '{{.Name}}' "$id")"
    name="${name#/}"
    if [ "$name" = "$GATEWAY_CONTAINER" ] \
      || [ "$name" = "$SIMULATOR_CONTAINER" ] \
      || [ "$name" = "$DB_CONTAINER" ]; then
      continue
    fi
    service="$(container_label "$id" "com.docker.compose.service")"
    if [ "$service" != "gateway" ] && [ "$service" != "simulator" ]; then
      continue
    fi
    stop_owned_app_container "$id" "$service"
  done <<EOF
$(docker ps -aq --filter "label=com.docker.compose.project=${COMPOSE_PROJECT}" 2>/dev/null || true)
EOF
}

cmd_down() {
  stop_owned_app_container "$GATEWAY_CONTAINER" gateway
  stop_owned_app_container "$SIMULATOR_CONTAINER" simulator
  stop_owned_one_off_containers
  assert_database_untouched
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
  prepare_generated_env
  compose ps
  cmd_bindings
}

cmd_admin() {
  prepare_generated_env
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

#!/bin/sh
# Export DATABASE_URL for the mission-owned local Supabase and exec a command.
# Does not print the URL or password. Does not start a second database.
set -eu

REPO="$(CDPATH= cd -- "$(dirname "$0")/.." && pwd)"
CONTAINER=supabase_db_opmux-mvp-20260919
HOST=127.0.0.1
PORT=55432

if [ -z "${DATABASE_URL:-}" ]; then
  if ! docker port "$CONTAINER" 5432/tcp >/dev/null 2>&1; then
    echo "owned supabase container $CONTAINER is not running on $HOST:$PORT" >&2
    echo "persistence tests require that database and do not skip" >&2
    exit 1
  fi
  published="$(docker port "$CONTAINER" 5432/tcp)"
  if [ "$published" != "$HOST:$PORT" ]; then
    echo "owned supabase is not bound to $HOST:$PORT" >&2
    exit 1
  fi
  if ! docker exec "$CONTAINER" pg_isready -U postgres -d postgres >/dev/null; then
    echo "owned supabase is not accepting connections" >&2
    exit 1
  fi
  pw="$(docker exec "$CONTAINER" printenv POSTGRES_PASSWORD)"
  enc="$(
    PW="$pw" python3 -c 'import os, urllib.parse; print(urllib.parse.quote(os.environ["PW"], safe=""))'
  )"
  DATABASE_URL="postgresql://postgres:${enc}@${HOST}:${PORT}/postgres?sslmode=disable"
  export DATABASE_URL
fi

cd "$REPO"
exec "$@"

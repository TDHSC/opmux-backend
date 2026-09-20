#!/bin/sh
# Verify the mission-owned local Supabase, then exec a command with a private
# DATABASE_URL for that fixture. Inherited DATABASE_URL and libpq destination
# variables are replaced or cleared and are never printed.
#
# Use this wrapper for tests, local smoke, and mission migrations. It does not
# restrict production gateway or operator DATABASE_URL configuration.
set -eu

REPO="$(CDPATH= cd -- "$(dirname "$0")/.." && pwd)"
CONTAINER=supabase_db_opmux-mvp-20260919
HOST=127.0.0.1
PORT=55432

if ! published="$(docker port "$CONTAINER" 5432/tcp 2>/dev/null)"; then
  echo "owned supabase container $CONTAINER is not running on $HOST:$PORT" >&2
  echo "persistence tests require that database and do not skip" >&2
  exit 1
fi
published=$(printf '%s' "$published" | tr -d '\r')
if [ -z "$published" ] || [ "$published" != "$HOST:$PORT" ]; then
  echo "owned supabase is not bound to $HOST:$PORT" >&2
  exit 1
fi
if ! docker exec "$CONTAINER" pg_isready -U postgres -d postgres >/dev/null 2>&1; then
  echo "owned supabase is not accepting connections" >&2
  exit 1
fi
if ! command -v python3 >/dev/null 2>&1; then
  echo "python3 is required to encode the owned database password" >&2
  exit 1
fi
pw="$(docker exec "$CONTAINER" printenv POSTGRES_PASSWORD)"
if ! enc="$(
  PW="$pw" python3 -c 'import os, urllib.parse; print(urllib.parse.quote(os.environ["PW"], safe=""))'
)"; then
  echo "failed to encode owned database password" >&2
  exit 1
fi
DATABASE_URL="postgresql://postgres:${enc}@${HOST}:${PORT}/postgres?sslmode=disable"
export DATABASE_URL
unset PGHOST PGHOSTADDR PGPORT PGOPTIONS PGDATABASE PGUSER PGPASSWORD \
  PGSSLMODE PGSERVICE PGSERVICEFILE PGPASSFILE

cd "$REPO"
exec "$@"

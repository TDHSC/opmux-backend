#!/bin/sh
# Create stub Supabase-compatible roles so canonical migrations are portable
# to disposable vanilla Postgres (CI). No-op when the roles already exist.
# Does not start a second database or print DATABASE_URL.
set -eu

REPO="$(CDPATH= cd -- "$(dirname "$0")/.." && pwd)"

if [ -z "${DATABASE_URL:-}" ]; then
  echo "DATABASE_URL is required to prepare migration roles" >&2
  echo "CI must provision disposable Postgres; local tests wrap with scripts/with-owned-database.sh" >&2
  echo "Persistence checks do not skip" >&2
  exit 1
fi

"$REPO/scripts/run-sql.sh" <<'SQL'
DO $$
BEGIN
  IF NOT EXISTS (SELECT 1 FROM pg_roles WHERE rolname = 'anon') THEN
    CREATE ROLE anon NOLOGIN;
  END IF;
  IF NOT EXISTS (SELECT 1 FROM pg_roles WHERE rolname = 'authenticated') THEN
    CREATE ROLE authenticated NOLOGIN;
  END IF;
  IF NOT EXISTS (SELECT 1 FROM pg_roles WHERE rolname = 'authenticator') THEN
    CREATE ROLE authenticator NOLOGIN;
  END IF;
  IF NOT EXISTS (SELECT 1 FROM pg_roles WHERE rolname = 'service_role') THEN
    CREATE ROLE service_role NOLOGIN;
  END IF;
END
$$;
SQL

echo "migration roles are present"

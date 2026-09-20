#!/bin/sh
# Apply canonical supabase/migrations with Supabase tracking, then reapply.
# Reapplication is a no-op. Does not reset retained data, start a second
# database, or create a SQLx migration ledger.
set -eu

REPO="$(CDPATH= cd -- "$(dirname "$0")/.." && pwd)"

if [ -z "${DATABASE_URL:-}" ]; then
  echo "DATABASE_URL is required to apply canonical migrations" >&2
  echo "CI must provision disposable Postgres; local tests wrap with scripts/with-owned-database.sh" >&2
  echo "Persistence checks do not skip" >&2
  exit 1
fi

"$REPO/scripts/prepare-postgres-roles.sh"
"$REPO/scripts/db-migrate.sh"

versions_before="$("$REPO/scripts/run-sql.sh" <<'SQL'
SELECT version FROM supabase_migrations.schema_migrations ORDER BY version;
SQL
)"
if [ -z "$versions_before" ]; then
  echo "canonical supabase migration history is empty after db-migrate.sh" >&2
  echo "Persistence checks do not skip" >&2
  exit 1
fi

schema="$("$REPO/scripts/run-sql.sh" <<'SQL'
SELECT to_regclass('opmux_private.api_keys');
SQL
)"
if [ "$schema" != "opmux_private.api_keys" ]; then
  echo "opmux_private.api_keys is missing after canonical migrations" >&2
  echo "Persistence checks do not skip" >&2
  exit 1
fi

"$REPO/scripts/db-migrate.sh"

versions_after="$("$REPO/scripts/run-sql.sh" <<'SQL'
SELECT version FROM supabase_migrations.schema_migrations ORDER BY version;
SQL
)"
if [ "$versions_before" != "$versions_after" ]; then
  echo "reapplying canonical migrations changed the Supabase history" >&2
  exit 1
fi

sqlx_ledger="$("$REPO/scripts/run-sql.sh" <<'SQL'
SELECT COALESCE(to_regclass('public._sqlx_migrations')::text, '');
SQL
)"
if [ -n "$sqlx_ledger" ]; then
  echo "must not create a competing SQLx migration ledger" >&2
  exit 1
fi

echo "canonical supabase/migrations applied; reapplication is a no-op"

#!/bin/sh
# Apply supabase/migrations to DATABASE_URL using Supabase CLI tracking.
# Never links a hosted project, never starts a second database, and never
# runs from gateway replica startup.
#
# Production/operator use: set DATABASE_URL to the chosen Postgres (not
# localhost-restricted). Local tests and mission migrations wrap this script
# with scripts/with-owned-database.sh so inherited URLs cannot be used.
set -eu

REPO="$(CDPATH= cd -- "$(dirname "$0")/.." && pwd)"

if [ -z "${DATABASE_URL:-}" ]; then
  exec "$REPO/scripts/with-owned-database.sh" "$0" "$@"
fi

if [ -n "${SUPABASE_CLI:-}" ]; then
  CLI=$SUPABASE_CLI
elif [ -x /tmp/opmux-mvp-readiness-20260919/tools/node_modules/.bin/supabase ]; then
  CLI=/tmp/opmux-mvp-readiness-20260919/tools/node_modules/.bin/supabase
elif command -v supabase >/dev/null 2>&1; then
  CLI="$(command -v supabase)"
else
  echo "supabase CLI 2.117.0 is required to apply migrations" >&2
  exit 1
fi

version="$("$CLI" --version 2>/dev/null || true)"
case "$version" in
  *2.117.*) ;;
  *)
    echo "warning: expected supabase CLI 2.117.x, using $CLI ($version)" >&2
    ;;
esac

# --db-url targets the already-owned database. --skip-vault avoids hosted vault.
# Output is suppressed because the CLI may echo connection details.
if ! "$CLI" db push \
  --db-url "$DATABASE_URL" \
  --workdir "$REPO" \
  --yes \
  --include-all \
  --skip-vault \
  --log-level error >/dev/null; then
  echo "supabase db push failed; inspect CLI status without printing DATABASE_URL" >&2
  exit 1
fi

echo "applied supabase/migrations to the configured database"

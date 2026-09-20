#!/bin/sh
# Execute SQL from stdin against DATABASE_URL without printing the URL or password.
# Uses the owned loopback container when the URL targets 127.0.0.1:55432.
# Otherwise uses psql with libpq variables parsed from DATABASE_URL.
set -eu

if [ -z "${DATABASE_URL:-}" ]; then
  echo "DATABASE_URL is required for SQL against the migration target" >&2
  echo "CI must provision Postgres; local tests wrap with scripts/with-owned-database.sh" >&2
  echo "Persistence checks do not skip" >&2
  exit 1
fi

SQL_FILE=$(mktemp "${TMPDIR:-/tmp}/opmux-sql.XXXXXX")
trap 'rm -f "$SQL_FILE"' EXIT
cat >"$SQL_FILE"

OPMUX_SQL_FILE="$SQL_FILE" python3 - <<'PY'
import os
import shutil
import subprocess
import sys
import urllib.parse

sql_path = os.environ.get("OPMUX_SQL_FILE", "")
url = os.environ.get("DATABASE_URL", "")
if not sql_path or not url.strip():
    print("DATABASE_URL is required for SQL against the migration target", file=sys.stderr)
    print("Persistence checks do not skip", file=sys.stderr)
    sys.exit(1)

try:
    sql = open(sql_path, encoding="utf-8").read()
except OSError:
    print("failed to read SQL payload", file=sys.stderr)
    sys.exit(1)

parsed = urllib.parse.urlparse(url)
host = parsed.hostname or ""
port = parsed.port or 5432
user = urllib.parse.unquote(parsed.username or "postgres")
password = urllib.parse.unquote(parsed.password or "")
database = urllib.parse.unquote((parsed.path or "/postgres").lstrip("/") or "postgres")
database = database.split("/")[0] or "postgres"

OWNED_CONTAINER = "supabase_db_opmux-mvp-20260919"
owned = False
if host == "127.0.0.1" and int(port) == 55432:
    inspect = subprocess.run(
        ["docker", "port", OWNED_CONTAINER, "5432/tcp"],
        capture_output=True,
        text=True,
    )
    published = (inspect.stdout or "").strip().replace("\r", "")
    owned = inspect.returncode == 0 and published == "127.0.0.1:55432"

if owned:
    command = [
        "docker",
        "exec",
        "-i",
        OWNED_CONTAINER,
        "psql",
        "-U",
        "postgres",
        "-d",
        "postgres",
        "-v",
        "ON_ERROR_STOP=1",
        "-Atq",
    ]
    env = os.environ.copy()
else:
    if shutil.which("psql") is None:
        print(
            "psql is required to apply SQL to disposable Postgres",
            file=sys.stderr,
        )
        print("Persistence checks do not skip", file=sys.stderr)
        sys.exit(1)
    command = ["psql", "-v", "ON_ERROR_STOP=1", "-Atq"]
    env = os.environ.copy()
    env["PGHOST"] = host
    env["PGPORT"] = str(port)
    env["PGUSER"] = user
    env["PGPASSWORD"] = password
    env["PGDATABASE"] = database
    env.pop("DATABASE_URL", None)

result = subprocess.run(
    command,
    input=sql,
    env=env,
    text=True,
    stdout=subprocess.PIPE,
    stderr=subprocess.PIPE,
)
if result.returncode != 0:
    err = (result.stderr or result.stdout or "sql execution failed").strip()
    for secret in filter(None, [password, url, user]):
        err = err.replace(secret, "[redacted]")
    print("sql execution failed; diagnostics omit connection secrets", file=sys.stderr)
    sys.exit(1)
sys.stdout.write(result.stdout)
PY

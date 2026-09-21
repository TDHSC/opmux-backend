#!/bin/bash
# Local equivalent of .github/workflows/ci.yml.
# Uses the owned loopback Supabase, not a second database and not this
# workflow's remote runners. Does not push, trigger remote CI, or call real
# providers. Persistence setup fails clearly when the owned database is missing.
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT_DIR"

SKIP_IMAGE="${SKIP_IMAGE:-0}"
SKIP_CONTAINER_CHECK="${SKIP_CONTAINER_CHECK:-0}"
STARTUP_CHECK_PORT="${STARTUP_CHECK_PORT:-38082}"

echo "applying canonical supabase/migrations to the owned database"
bash scripts/with-owned-database.sh bash scripts/ci-setup-db.sh

echo "running locked workspace tests with dummy loopback provider settings"
bash scripts/with-owned-database.sh bash scripts/with-safe-test-env.sh \
  env CARGO_BUILD_JOBS=2 \
  cargo test --workspace --locked --all-features -j 2 -- --test-threads=2

echo "running cargo check"
cargo check --workspace --locked --all-targets --all-features -j 2

echo "running clippy"
cargo clippy --workspace --locked --all-targets --all-features -j 2 -- -D warnings

echo "checking rustfmt"
cargo fmt --all -- --check

echo "checking prettier"
npm run format:check

echo "running startup smoke against owned database"
cargo build -p gateway --locked -j 2
bash scripts/with-owned-database.sh env STARTUP_CHECK_PORT="$STARTUP_CHECK_PORT" \
  bash scripts/check-startup.sh "$ROOT_DIR/target/debug/gateway"

skipped_gates=()
if [ "$SKIP_IMAGE" = "1" ]; then
  echo "skipping image build (development skip)"
  skipped_gates+=("image build")
else
  echo "building locked gateway image"
  docker build --file gateway/Dockerfile --tag opmux-gateway:mvp .
fi

if [ "$SKIP_IMAGE" = "1" ] || [ "$SKIP_CONTAINER_CHECK" = "1" ]; then
  echo "skipping container runtime acceptance (development skip)"
  skipped_gates+=("container runtime acceptance")
else
  echo "running owned-container acceptance"
  # Reuse the image built above in this same run; this is not a development skip.
  # Pin the tag so an inherited CONTAINER_IMAGE cannot runtime-test a different image.
  SKIP_IMAGE_BUILD=1 CONTAINER_IMAGE=opmux-gateway:mvp bash scripts/check-container.sh
fi

if [ "${#skipped_gates[@]}" -ne 0 ]; then
  joined=$(IFS=', '; echo "${skipped_gates[*]}")
  echo "local CI equivalent PARTIAL (not full acceptance): skipped ${joined}"
  exit 0
fi

echo "local CI equivalent passed"

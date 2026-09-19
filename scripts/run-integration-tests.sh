#!/bin/bash
# Opt-in runner for deferred live-provider executor tests.
#
# Live OpenAI verification is unrun by default. Inherited OPENAI_API_KEY does
# not activate these tests. This script requires an explicit opt-in.
#
# Usage (only when live verification is explicitly requested):
#   OPMUX_LIVE_PROVIDER_TESTS=1 OPENAI_API_KEY=... ./scripts/run-integration-tests.sh

set -euo pipefail

RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
NC='\033[0m'

echo -e "${BLUE}========================================${NC}"
echo -e "${BLUE}Deferred live-provider tests (opt-in)${NC}"
echo -e "${BLUE}========================================${NC}"
echo ""

if [ "${OPMUX_LIVE_PROVIDER_TESTS:-}" != "1" ] && [ "${OPMUX_LIVE_PROVIDER_TESTS:-}" != "true" ]; then
    echo -e "${RED}Refusing to run live-provider tests.${NC}"
    echo ""
    echo "These tests are ignored by default and are not part of routine verification."
    echo "Inherited OPENAI_API_KEY does not activate them."
    echo "Set OPMUX_LIVE_PROVIDER_TESTS=1 only when live verification is explicitly requested."
    echo ""
    echo "For local simulator coverage use:"
    echo "  cargo test -p gateway --test http_fixture_test --test openai_adapter_http_test"
    exit 1
fi

if [ -z "${OPENAI_API_KEY:-}" ]; then
    echo -e "${RED}OPENAI_API_KEY is required after opt-in.${NC}"
    exit 1
fi

echo -e "${YELLOW}WARNING: This contacts a real provider and may incur charges.${NC}"
echo -e "${YELLOW}Live verification remains an explicit opt-in, not a CI gate.${NC}"
echo ""

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT_DIR"

cargo test -p gateway --test executor_integration_test -- --ignored --nocapture

echo ""
echo -e "${GREEN}Opt-in live-provider tests finished.${NC}"
echo "This is not evidence used by routine CI or local simulator validation."

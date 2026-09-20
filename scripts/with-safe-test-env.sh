#!/bin/sh
# Clear inherited provider/proxy settings and force dummy loopback OpenAI
# configuration. Credential presence cannot activate live-provider tests.
# DATABASE_URL is preserved for persistence checks.
set -eu

exec env \
  -u OPENAI_API_KEY \
  -u ANTHROPIC_API_KEY \
  -u HTTP_PROXY \
  -u HTTPS_PROXY \
  -u ALL_PROXY \
  -u http_proxy \
  -u https_proxy \
  -u all_proxy \
  -u OPMUX_LIVE_PROVIDER_TESTS \
  AUTH_DEVELOPMENT_MODE=false \
  OPENAI_API_KEY=ci-dummy-not-a-real-provider-key \
  OPENAI_BASE_URL=http://127.0.0.1:9/v1 \
  NO_PROXY='*' \
  no_proxy='*' \
  "$@"

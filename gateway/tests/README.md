# Integration Tests

This directory contains HTTP and process integration tests for the Gateway service.

## Local HTTP fixtures (default)

Routine tests use dummy credentials and an owned loopback OpenAI simulator. They exercise the
**shared production router** (`gateway::app::build_production_router`) and the **real Reqwest
adapter**, not only `LLMVendor` mocks.

| File                                                     | What it covers                                                                                                                                                     |
| -------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------ |
| `http_fixture_test.rs`                                   | Two isolated production-router runs: `/health`, `/ready`, authenticated generation, metrics/correlation, loopback bind and drop cleanup, shared-builder call sites |
| `openai_adapter_http_test.rs`                            | Actual OpenAI adapter success, scripted 500, inherited env/proxy isolation                                                                                         |
| `observability_integration_test.rs`                      | Correlation, metrics, health/ready, circuit-open via the shared router                                                                                             |
| `config_startup_test.rs` / `startup_integration_test.rs` | Binary catalog/logging startup with canonical `OPMUX_CONFIG_FILE`                                                                                                  |
| `support/`                                               | Environment isolation, loopback simulator with request capture and scripted replies                                                                                |

Fixtures bind `127.0.0.1` with an OS-assigned port and abort the owned task on drop. They clear
inherited `OPENAI_*`, `ANTHROPIC_API_KEY`, and proxy variables, set `NO_PROXY=*`, and inject dummy
loopback settings explicitly.

```bash
env -u OPENAI_API_KEY -u ANTHROPIC_API_KEY -u HTTP_PROXY -u HTTPS_PROXY -u ALL_PROXY \
  -u http_proxy -u https_proxy -u all_proxy \
  OPENAI_BASE_URL=http://127.0.0.1:9/v1 NO_PROXY='*' \
  cargo test -p gateway --test http_fixture_test --test openai_adapter_http_test
```

Do not treat these simulator results as live OpenAI verification.

## Deferred live-provider tests (unrun)

`executor_integration_test.rs` still contains live OpenAI cases. They are **`#[ignore]`** and also
require `OPMUX_LIVE_PROVIDER_TESTS=1`. Inherited `OPENAI_API_KEY` does **not** activate them. They
are unrun in routine CI and local test commands.

Do not run them unless live verification is explicitly requested. They can incur charges.

```bash
OPMUX_LIVE_PROVIDER_TESTS=1 cargo test -p gateway --test executor_integration_test -- --ignored --nocapture
```

## Additional Task 13 artifacts

- End-to-end integration coverage for resilience behavior in `observability_integration_test.rs`
  (circuit-open transition under repeated upstream failures).
- Performance/load testing guide in `PERFORMANCE_LOAD_TESTING.md`.
- Load test runner script at `scripts/run-load-tests.sh`.

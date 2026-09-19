# Integration Tests

This directory contains integration tests for the Gateway service that make real API calls to
external services.

## Additional Task 13 Artifacts

- End-to-end integration coverage for resilience behavior in `observability_integration_test.rs`
  (circuit-open transition under repeated upstream failures).
- Performance/load testing guide in `PERFORMANCE_LOAD_TESTING.md`.
- Load test runner script at `scripts/run-load-tests.sh`.

## Executor Layer Integration Tests

### Overview

The `executor_integration_test.rs` file contains integration tests for the Executor Layer that make
real API calls to OpenAI (or OpenAI-compatible endpoints).

### Test Coverage

The integration tests cover:

1. **Basic Execution** - Simple API call with gpt-3.5-turbo
2. **Different Models** - Testing with different model configurations
3. **Parameter Extraction** - Verifying JSON payload parsing
4. **Retry Logic** - Ensuring retry mechanism works with real API
5. **Unsupported Model** - Error handling for invalid models
6. **Cost Calculation** - Verifying token usage and cost calculation

### Prerequisites

To run these tests, you need:

1. **Valid API Key** - An OpenAI API key or compatible service key
2. **Network Access** - Internet connection to reach the API endpoint

### Running the Tests

#### Using Environment Variables

```bash
# Set environment variables
export OPENAI_API_KEY=your-api-key-here
export OPENAI_BASE_URL=https://api.openai.com/v1  # Optional, defaults to https://api.openai.com/v1

# Run all integration tests
cargo test --test executor_integration_test -- --nocapture

# Run a specific test
cargo test --test executor_integration_test test_openai_api_basic_execution -- --nocapture
```

#### Using the Helper Script

```bash
# Set environment variables
export OPENAI_API_KEY=your-api-key-here
export OPENAI_BASE_URL=https://api.openai.com/v1  # Optional

# Run the script
./scripts/run-integration-tests.sh
```

#### One-Line Command

```bash
OPENAI_API_KEY=your-key OPENAI_BASE_URL=https://api.openai.com/v1 cargo test --test executor_integration_test -- --nocapture
```

### Test Behavior

- **Skipped if no API key**: Tests are automatically skipped if `OPENAI_API_KEY` is not set
- **Real API calls**: Tests make actual HTTP requests to the configured endpoint
- **Cost**: Tests use gpt-3.5-turbo (cheapest model) with small token limits to minimize costs
- **Output**: Use `--nocapture` flag to see detailed test output including API responses

### Expected Output

```
running 6 tests
✅ ExecutorService initialized with 1 vendors
✅ Unsupported model test passed (correctly rejected)
test test_openai_api_unsupported_model ... ok
✅ gpt-3.5-turbo test passed
test test_openai_api_with_different_models ... ok
✅ Retry logic test passed
test test_openai_api_retry_logic ... ok
✅ API call successful!
   Model: gpt-3.5-turbo
   Response: Hello, World!
   Prompt tokens: 17
   Completion tokens: 4
   Total cost: $0.000015
   Finish reason: stop
test test_openai_api_basic_execution ... ok
✅ Parameter extraction and execution successful!
   Response: 2 + 2 equals 4.
test test_openai_api_parameter_extraction ... ok
✅ Cost calculation test:
   Prompt tokens: 15
   Completion tokens: 14
   Total cost: $0.000029
test test_openai_api_cost_calculation ... ok

test result: ok. 6 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
```

### Configuration Options

#### Environment Variables

| Variable               | Required | Default                     | Description                      |
| ---------------------- | -------- | --------------------------- | -------------------------------- |
| `OPENAI_API_KEY`       | Yes      | -                           | OpenAI API key or compatible key |
| `OPENAI_BASE_URL`      | No       | `https://api.openai.com/v1` | API endpoint URL                 |
| `OPENAI_TIMEOUT_MS`    | No       | `30000`                     | Request timeout in milliseconds  |
| `EXECUTOR_MAX_RETRIES` | No       | `3`                         | Maximum retry attempts           |

#### Supported Base URLs

For official OpenAI:

- `https://api.openai.com/v1` (default)

For OpenAI-compatible services:

- Use the `/v1` endpoint (e.g., `https://your-proxy.com/v1`)
- Do not use the full path `/v1/chat/completions` (the SDK appends this automatically)

### Cost Considerations

Each test run makes approximately 6 API calls with the following characteristics:

- Model: gpt-3.5-turbo (cheapest option)
- Max tokens: 20-50 per request
- Estimated cost: < $0.001 per full test run

**Total estimated cost per test run: < $0.001 USD**

### Troubleshooting

#### Tests are skipped

```
⏭️  Skipping integration test: OPENAI_API_KEY not set
```

**Solution**: Set the `OPENAI_API_KEY` environment variable.

#### API call failed with authentication error

```
❌ API call failed: AuthenticationFailed("openai")
```

**Solution**: Verify your API key is valid and has not expired.

#### API call failed with network error

```
❌ API call failed: NetworkError("Connection failed")
```

**Solution**:

1. Check your internet connection
2. Verify the `OPENAI_BASE_URL` is correct
3. Check if the API endpoint is accessible

#### Unsupported model error

```
❌ API call failed: UnsupportedModel("openai", "gpt-5")
```

**Solution**: The model is not in the supported models list. Check
`gateway/src/features/executor/config.rs` for supported models.

### Adding New Tests

To add new integration tests:

1. Create a new test function in `executor_integration_test.rs`
2. Use the `should_run_integration_tests()` helper to skip if no API key
3. Use helper functions like `create_test_route_plan()` and `create_test_payload()`
4. Add assertions to verify expected behavior
5. Use `println!()` for debugging output (visible with `--nocapture`)

Example:

```rust
#[tokio::test]
async fn test_my_new_feature() {
    if !should_run_integration_tests() {
        println!("⏭️  Skipping integration test: OPENAI_API_KEY not set");
        return;
    }

    let config = ExecutorConfig::from_env();
    let service = ExecutorService::from_config(config)
        .expect("Failed to create ExecutorService");

    let plan = create_test_route_plan("openai", "gpt-3.5-turbo");
    let payload = create_test_payload("Your test prompt");

    let result = service.execute(&plan, &payload).await;

    assert!(result.is_ok(), "Test should succeed");
    println!("✅ My new feature test passed");
}
```

### Security Notes

- **Never commit API keys** to version control
- Use environment variables for sensitive configuration
- The test script masks API keys in output for security
- Consider using separate API keys for testing vs production

### CI/CD Integration

These tests are **not** run in CI by default because they:

1. Require valid API credentials
2. Make real API calls (cost money)
3. Depend on external service availability

To run in CI, you would need to:

1. Add `OPENAI_API_KEY` as a secret in your CI environment
2. Optionally set `OPENAI_BASE_URL` for a test endpoint
3. Add a separate CI job that runs integration tests
4. Consider using a dedicated test API key with rate limits

Example GitHub Actions workflow:

```yaml
integration-tests:
  runs-on: ubuntu-latest
  steps:
    - uses: actions/checkout@v3
    - uses: actions-rs/toolchain@v1
      with:
        toolchain: stable
    - name: Run integration tests
      env:
        OPENAI_API_KEY: ${{ secrets.OPENAI_API_KEY }}
        OPENAI_BASE_URL: ${{ secrets.OPENAI_BASE_URL }}
      run: cargo test --test executor_integration_test
```

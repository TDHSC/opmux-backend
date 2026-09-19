//! Catalog, limit, credential, and injection tests for operator configuration.

use super::*;
use serde_json::json;
use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

struct TempCatalog {
    path: PathBuf,
}

impl TempCatalog {
    fn write(contents: &str) -> Self {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("time")
            .as_nanos();
        let path = std::env::temp_dir()
            .join(format!("opmux-catalog-{}-{nanos}.json", std::process::id()));
        fs::write(&path, contents).expect("write temp catalog");
        Self { path }
    }
}

impl Drop for TempCatalog {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

fn example_catalog_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../config/opmux.example.json")
}

fn valid_catalog() -> String {
    json!({
        "version": 1,
        "default_route": "default",
        "targets": {
            "primary": {
                "vendor": "openai",
                "model": "example-chat-model",
                "max_output_tokens": 512,
                "pricing": {
                    "input_per_million": 1.0,
                    "output_per_million": 2.0
                }
            },
            "secondary": {
                "model": "example-chat-model-mini",
                "max_output_tokens": 256,
                "pricing": {
                    "input_per_million": 0.25,
                    "output_per_million": 0.5
                }
            }
        },
        "routes": {
            "default": {
                "primary": "primary",
                "fallbacks": ["secondary"]
            }
        }
    })
    .to_string()
}

fn dummy_env() -> HashMap<String, String> {
    let mut env = HashMap::new();
    env.insert("OPENAI_API_KEY".into(), "test-dummy-openai-key".into());
    env.insert("OPENAI_BASE_URL".into(), "http://127.0.0.1:9/v1".into());
    env.insert("SERVER_HOST".into(), "127.0.0.1".into());
    env.insert("SERVER_PORT".into(), "3000".into());
    env
}

fn load_json(contents: &str) -> Result<Settings, ConfigError> {
    let catalog = TempCatalog::write(contents);
    Settings::load_from(&catalog.path, &dummy_env())
}

fn load_json_with_env(
    contents: &str,
    env: &HashMap<String, String>,
) -> Result<Settings, ConfigError> {
    let catalog = TempCatalog::write(contents);
    Settings::load_from(&catalog.path, env)
}

#[test]
fn example_catalog_loads_documented_defaults() {
    let settings = Settings::load_from(example_catalog_path(), &dummy_env())
        .expect("example catalog should load");

    assert_eq!(settings.catalog.version, 1);
    assert_eq!(settings.catalog.default_route, "default");
    let primary = settings
        .catalog
        .targets
        .get("primary")
        .expect("primary target");
    assert_eq!(primary.vendor, VendorKind::Openai);
    assert_eq!(primary.model, "example-chat-model");
    assert_eq!(primary.max_output_tokens, 512);
    assert_eq!(primary.pricing.input_per_million, 1.0);
    assert_eq!(primary.pricing.output_per_million, 2.0);
    let route = settings
        .catalog
        .routes
        .get("default")
        .expect("default route");
    assert_eq!(route.primary, "primary");
    assert_eq!(route.fallbacks, vec!["secondary".to_string()]);

    assert_eq!(
        settings.limits.protected_request_deadline_ms(),
        PROTECTED_REQUEST_DEADLINE_MS.default
    );
    assert_eq!(
        settings.limits.max_attempt_timeout_ms(),
        MAX_ATTEMPT_TIMEOUT_MS.default
    );
    assert_eq!(
        settings.limits.retries_per_target,
        RETRIES_PER_TARGET.default as u32
    );
    assert_eq!(
        settings.limits.max_total_attempts,
        MAX_TOTAL_ATTEMPTS.default as u32
    );
    assert_eq!(
        settings.limits.max_fallback_targets,
        MAX_FALLBACK_TARGETS.default as u32
    );
    assert_eq!(settings.limits.backoff_cap_ms(), BACKOFF_CAP_MS.default);
}

#[test]
fn catalog_values_are_injected_into_settings() {
    let settings = load_json(&valid_catalog()).expect("valid catalog");
    let injected = std::sync::Arc::new(settings);
    assert_eq!(injected.catalog.default_route, "default");
    assert_eq!(
        injected.catalog.targets["primary"]
            .pricing
            .output_per_million,
        2.0
    );
    assert_eq!(injected.catalog.routes["default"].fallbacks.len(), 1);
}

#[test]
fn env_overrides_replace_documented_defaults() {
    let mut env = dummy_env();
    env.insert("OPMUX_PROTECTED_REQUEST_DEADLINE_MS".into(), "15000".into());
    env.insert("OPMUX_MAX_ATTEMPT_TIMEOUT_MS".into(), "4000".into());
    env.insert("OPMUX_RETRIES_PER_TARGET".into(), "0".into());
    env.insert("OPMUX_MAX_TOTAL_ATTEMPTS".into(), "2".into());
    env.insert("OPMUX_MAX_FALLBACK_TARGETS".into(), "1".into());
    env.insert("OPMUX_BACKOFF_CAP_MS".into(), "500".into());

    let settings = load_json_with_env(&valid_catalog(), &env).expect("overrides");
    assert_eq!(settings.limits.protected_request_deadline_ms(), 15_000);
    assert_eq!(settings.limits.max_attempt_timeout_ms(), 4_000);
    assert_eq!(settings.limits.retries_per_target, 0);
    assert_eq!(settings.limits.max_total_attempts, 2);
    assert_eq!(settings.limits.max_fallback_targets, 1);
    assert_eq!(settings.limits.backoff_cap_ms(), 500);
}

#[test]
fn zero_prices_are_valid() {
    let mut catalog: serde_json::Value = serde_json::from_str(&valid_catalog()).unwrap();
    catalog["targets"]["primary"]["pricing"]["input_per_million"] = json!(0.0);
    catalog["targets"]["primary"]["pricing"]["output_per_million"] = json!(0.0);
    catalog["targets"]["secondary"]["pricing"]["input_per_million"] = json!(0);
    catalog["targets"]["secondary"]["pricing"]["output_per_million"] = json!(0);
    load_json(&catalog.to_string()).expect("zero prices are valid");
}

#[test]
fn unreadable_catalog_fails() {
    let error = Settings::load_from("/no/such/opmux-catalog.json", &dummy_env())
        .expect_err("missing file");
    assert_eq!(error.category(), "catalog_unreadable");
}

#[test]
fn invalid_catalogs_fail_with_expected_category() {
    let mut cases: Vec<(String, &str)> = vec![
        ("not json".to_string(), "catalog_malformed_json"),
        (
            json!({"version": 2, "default_route": "default", "targets": {}, "routes": {}}).to_string(),
            "catalog_unsupported_version",
        ),
        (
            {
                let mut value: serde_json::Value =
                    serde_json::from_str(&valid_catalog()).unwrap();
                value["unexpected"] = json!(true);
                value.to_string()
            },
            "catalog_unknown_field",
        ),
        (
            {
                let mut value: serde_json::Value =
                    serde_json::from_str(&valid_catalog()).unwrap();
                value["default_route"] = json!("missing");
                value.to_string()
            },
            "catalog_missing_default_route",
        ),
        (
            {
                let mut value: serde_json::Value =
                    serde_json::from_str(&valid_catalog()).unwrap();
                value["routes"]["default"]["primary"] = json!("unknown");
                value.to_string()
            },
            "catalog_unknown_target_reference",
        ),
        (
            {
                let mut value: serde_json::Value =
                    serde_json::from_str(&valid_catalog()).unwrap();
                value["routes"]["default"]["fallbacks"] = json!(["unknown"]);
                value.to_string()
            },
            "catalog_unknown_target_reference",
        ),
        (
            {
                let mut value: serde_json::Value =
                    serde_json::from_str(&valid_catalog()).unwrap();
                value["routes"]["default"]["primary"] = json!("");
                value.to_string()
            },
            "catalog_empty_identifier",
        ),
        (
            {
                let mut value: serde_json::Value =
                    serde_json::from_str(&valid_catalog()).unwrap();
                value["targets"]["primary"]["model"] = json!("");
                value.to_string()
            },
            "catalog_empty_identifier",
        ),
        (
            {
                let mut value: serde_json::Value =
                    serde_json::from_str(&valid_catalog()).unwrap();
                value["routes"]["default"]["fallbacks"] =
                    json!([{"primary": "secondary", "fallbacks": []}]);
                value.to_string()
            },
            "catalog_nested_chain",
        ),
        (
            {
                let mut value: serde_json::Value =
                    serde_json::from_str(&valid_catalog()).unwrap();
                value["routes"]["default"]["fallbacks"] = json!(["primary"]);
                value.to_string()
            },
            "catalog_duplicate_chain_target",
        ),
        (
            {
                let mut value: serde_json::Value =
                    serde_json::from_str(&valid_catalog()).unwrap();
                value["targets"]["third"] = json!({
                    "model": "example-three",
                    "max_output_tokens": 64,
                    "pricing": { "input_per_million": 1.0, "output_per_million": 1.0 }
                });
                value["targets"]["fourth"] = json!({
                    "model": "example-four",
                    "max_output_tokens": 64,
                    "pricing": { "input_per_million": 1.0, "output_per_million": 1.0 }
                });
                value["routes"]["default"]["fallbacks"] =
                    json!(["secondary", "third", "fourth"]);
                value.to_string()
            },
            "catalog_fallback_limit",
        ),
        (
            {
                let mut value: serde_json::Value =
                    serde_json::from_str(&valid_catalog()).unwrap();
                value["targets"]["primary"]["vendor"] = json!("anthropic");
                value.to_string()
            },
            "catalog_unsupported_vendor",
        ),
        (
            {
                let mut value: serde_json::Value =
                    serde_json::from_str(&valid_catalog()).unwrap();
                value["targets"]["primary"]["pricing"]
                    .as_object_mut()
                    .unwrap()
                    .remove("output_per_million");
                value.to_string()
            },
            "catalog_incomplete_pricing",
        ),
        (
            {
                let mut value: serde_json::Value =
                    serde_json::from_str(&valid_catalog()).unwrap();
                value["targets"]["primary"]["pricing"]["input_per_million"] = json!(-1.0);
                value.to_string()
            },
            "catalog_invalid_price",
        ),
        (
            r#"{
                "version": 1,
                "default_route": "default",
                "targets": {
                    "primary": {
                        "model": "example-chat-model",
                        "max_output_tokens": 512,
                        "pricing": { "input_per_million": 1.0, "output_per_million": 1e400 }
                    }
                },
                "routes": { "default": { "primary": "primary", "fallbacks": [] } }
            }"#
            .to_string(),
            "catalog_invalid_price",
        ),
        (
            {
                let mut value: serde_json::Value =
                    serde_json::from_str(&valid_catalog()).unwrap();
                value["routes"] = json!("default");
                value.to_string()
            },
            "catalog_invalid_type",
        ),
        (
            {
                let mut value: serde_json::Value =
                    serde_json::from_str(&valid_catalog()).unwrap();
                value["targets"]["primary"]["max_output_tokens"] = json!("512");
                value.to_string()
            },
            "catalog_invalid_type",
        ),
        (
            {
                let mut value: serde_json::Value =
                    serde_json::from_str(&valid_catalog()).unwrap();
                value["targets"]["primary"]["pricing"]["input_per_million"] = json!("1.0");
                value.to_string()
            },
            "catalog_invalid_type",
        ),
    ];

    let duplicate_targets = r#"{
        "version": 1,
        "default_route": "default",
        "targets": {
            "primary": {
                "model": "example-chat-model",
                "max_output_tokens": 512,
                "pricing": { "input_per_million": 1.0, "output_per_million": 2.0 }
            },
            "primary": {
                "model": "other-model",
                "max_output_tokens": 64,
                "pricing": { "input_per_million": 1.0, "output_per_million": 2.0 }
            }
        },
        "routes": {
            "default": { "primary": "primary", "fallbacks": [] }
        }
    }"#;
    cases.push((duplicate_targets.to_string(), "catalog_duplicate_target"));

    let duplicate_routes = r#"{
        "version": 1,
        "default_route": "default",
        "targets": {
            "primary": {
                "model": "example-chat-model",
                "max_output_tokens": 512,
                "pricing": { "input_per_million": 1.0, "output_per_million": 2.0 }
            }
        },
        "routes": {
            "default": { "primary": "primary", "fallbacks": [] },
            "default": { "primary": "primary", "fallbacks": [] }
        }
    }"#;
    cases.push((duplicate_routes.to_string(), "catalog_duplicate_route"));

    let repeated_fallback = {
        let mut value: serde_json::Value =
            serde_json::from_str(&valid_catalog()).unwrap();
        value["routes"]["default"]["fallbacks"] = json!(["secondary", "secondary"]);
        value.to_string()
    };
    cases.push((repeated_fallback, "catalog_duplicate_chain_target"));

    for (json_body, category) in cases {
        let error = load_json(&json_body)
            .expect_err("invalid catalog must fail")
            .category()
            .to_string();
        assert_eq!(error, category, "body={json_body}");
    }
}

#[test]
fn forbidden_limits_fail() {
    let cases = [
        ("OPMUX_PROTECTED_REQUEST_DEADLINE_MS", "0"),
        ("OPMUX_MAX_ATTEMPT_TIMEOUT_MS", "-1"),
        ("OPMUX_MAX_TOTAL_ATTEMPTS", "1.5"),
        ("OPMUX_MAX_CONCURRENT_GENERATIONS", "99999"),
        ("OPMUX_RETRIES_PER_TARGET", "9"),
        ("OPMUX_MAX_REQUEST_BODY_BYTES", "0"),
    ];
    for (name, value) in cases {
        let mut env = dummy_env();
        env.insert(name.to_string(), value.to_string());
        let error = load_json_with_env(&valid_catalog(), &env).expect_err(name);
        assert_eq!(error.category(), "invalid_limit", "{name}={value}");
    }
}

#[test]
fn zero_retries_are_distinct_from_zero_attempts() {
    let mut env = dummy_env();
    env.insert("OPMUX_RETRIES_PER_TARGET".into(), "0".into());
    let settings = load_json_with_env(&valid_catalog(), &env).expect("zero retries");
    assert_eq!(settings.limits.retries_per_target, 0);
    assert_eq!(settings.limits.max_total_attempts, 3);

    env.insert("OPMUX_MAX_TOTAL_ATTEMPTS".into(), "0".into());
    let error = load_json_with_env(&valid_catalog(), &env).expect_err("zero attempts");
    assert_eq!(error.category(), "invalid_limit");
}

#[test]
fn missing_and_blank_credentials_fail() {
    let mut env = dummy_env();
    env.remove("OPENAI_API_KEY");
    let error = load_json_with_env(&valid_catalog(), &env).expect_err("missing key");
    assert_eq!(error.category(), "missing_credential");

    env.insert("OPENAI_API_KEY".into(), "   ".into());
    let error = load_json_with_env(&valid_catalog(), &env).expect_err("blank key");
    assert_eq!(error.category(), "missing_credential");
    assert!(!error.to_string().contains("   "));
}

#[test]
fn invalid_provider_url_is_sanitized() {
    let mut env = dummy_env();
    env.insert(
        "OPENAI_BASE_URL".into(),
        "https://user:sentinel-url-secret@example.invalid/v1".into(),
    );
    let error = load_json_with_env(&valid_catalog(), &env).expect_err("credential url");
    assert_eq!(error.category(), "invalid_provider_url");
    let text = error.to_string();
    assert!(!text.contains("sentinel-url-secret"));
    assert!(!text.contains("user:"));

    env.insert("OPENAI_BASE_URL".into(), "ftp://127.0.0.1/v1".into());
    let error = load_json_with_env(&valid_catalog(), &env).expect_err("scheme");
    assert_eq!(error.category(), "invalid_provider_url");
}

#[test]
fn settings_debug_does_not_dump_fields() {
    let settings = Settings::for_tests();
    let rendered = format!("{settings:?}");
    assert_eq!(rendered, "Settings");
    assert!(!rendered.contains("test-dummy-openai-key"));
    assert!(!rendered.contains("127.0.0.1"));
}

#[test]
fn secret_string_never_prints_contents() {
    let secret = SecretString::new("cfg-sentinel-key-do-not-leak");
    assert_eq!(format!("{secret}"), "[redacted]");
    assert_eq!(format!("{secret:?}"), "[redacted]");
}

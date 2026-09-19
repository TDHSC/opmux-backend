//! Checks logging configuration through the actual binary, without provider credentials.

use std::path::PathBuf;
use std::process::{Command, Output};

type EnvSettings<'a> = &'a [(&'a str, &'a str)];

fn example_catalog_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../config/opmux.example.json")
}

fn run_without_vendors(settings: &[(&str, &str)]) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_gateway"));
    for name in [
        "OPENAI_API_KEY",
        "ANTHROPIC_API_KEY",
        "OPENAI_BASE_URL",
        "HTTP_PROXY",
        "HTTPS_PROXY",
        "ALL_PROXY",
        "http_proxy",
        "https_proxy",
        "all_proxy",
        "RUST_LOG",
        "LOG_LEVEL",
        "LOG_FORMAT",
        "LOG_JSON",
        "LOG_VERBOSE_DEBUG",
    ] {
        command.env_remove(name);
    }
    command
        .env("AUTH_DEVELOPMENT_MODE", "false")
        .env("SERVER_HOST", "127.0.0.1")
        .env("SERVER_PORT", "0")
        .env("METRICS_ENABLED", "false")
        .env("OPMUX_CONFIG_FILE", example_catalog_path())
        .envs(settings.iter().copied())
        .output()
        .expect("gateway should launch")
}

#[test]
fn test_startup_logging_configuration() {
    let cases: &[(EnvSettings<'_>, &str, bool, bool)] = &[
        (&[], "info", true, false),
        (
            &[("LOG_LEVEL", "debug"), ("LOG_JSON", "true")],
            "debug",
            true,
            false,
        ),
        (
            &[("LOG_LEVEL", "info"), ("LOG_JSON", "false")],
            "info",
            false,
            false,
        ),
        (
            &[
                ("RUST_LOG", "info,gateway=debug"),
                ("LOG_LEVEL", "error"),
                ("LOG_FORMAT", "json"),
                ("LOG_JSON", "false"),
                ("LOG_VERBOSE_DEBUG", "1"),
            ],
            "info,gateway=debug",
            true,
            true,
        ),
        (
            &[("LOG_FORMAT", "pretty"), ("LOG_JSON", "true")],
            "info",
            false,
            false,
        ),
        (
            &[("LOG_FORMAT", "invalid"), ("LOG_JSON", "false")],
            "info",
            true,
            false,
        ),
    ];

    for (settings, level, json, verbose) in cases {
        let output = run_without_vendors(settings);
        assert_eq!(output.status.code(), Some(1), "settings: {settings:?}");
        let stdout = String::from_utf8(output.stdout).unwrap();
        assert!(
            stdout.contains("missing_credential"),
            "settings: {settings:?}"
        );

        if *json {
            let records: Vec<serde_json::Value> = stdout
                .lines()
                .map(|line| serde_json::from_str(line).expect("valid JSON log line"))
                .collect();
            let initialized = records
                .iter()
                .find(|record| record["fields"]["message"] == "Tracing initialized")
                .expect("configurable tracing initializer should run");
            assert_eq!(initialized["fields"]["log_level"], *level);
            assert_eq!(initialized["fields"]["format"], "Json");
            assert_eq!(initialized["fields"]["verbose_debug"], *verbose);
            assert_eq!(initialized.get("threadId").is_some(), *verbose);
            assert_eq!(initialized.get("line_number").is_some(), *verbose);
        } else {
            assert!(stdout.contains("Tracing initialized"));
            assert!(stdout.contains("Pretty"));
            assert!(!stdout.trim_start().starts_with('{'));
        }
    }
}

#[test]
fn test_startup_respects_log_level_and_rust_log_precedence() {
    for settings in [
        vec![("LOG_LEVEL", "error")],
        vec![("RUST_LOG", "error"), ("LOG_LEVEL", "debug")],
    ] {
        let output = run_without_vendors(&settings);
        assert_eq!(output.status.code(), Some(1));
        let stdout = String::from_utf8(output.stdout).unwrap();
        assert!(stdout.contains("missing_credential"));
        assert!(!stdout.contains("Starting gateway"));
        for line in stdout.lines() {
            let record: serde_json::Value = serde_json::from_str(line).unwrap();
            assert_eq!(record["level"], "ERROR");
        }
    }
}

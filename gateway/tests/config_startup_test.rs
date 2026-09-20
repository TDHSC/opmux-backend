//! Binary configuration tests: valid catalog startup and fail-closed invalid input.

use std::collections::HashMap;
use std::fs;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const SENTINEL_KEY: &str = "cfg-sentinel-key-do-not-leak";
const SENTINEL_URL_SECRET: &str = "sentinel-url-secret";

struct TempCatalog {
    path: PathBuf,
}

impl TempCatalog {
    fn write(contents: &str) -> Self {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("time")
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "opmux-bin-catalog-{}-{nanos}.json",
            std::process::id()
        ));
        fs::write(&path, contents).expect("write catalog");
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
    fs::read_to_string(example_catalog_path()).expect("example catalog")
}

fn unused_loopback_port() -> u16 {
    TcpListener::bind("127.0.0.1:0")
        .expect("bind probe")
        .local_addr()
        .expect("probe addr")
        .port()
}

fn gateway_command() -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_gateway"));
    for name in [
        "OPENAI_API_KEY",
        "ANTHROPIC_API_KEY",
        "OPENAI_BASE_URL",
        "OPMUX_CONFIG_FILE",
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
    ] {
        command.env_remove(name);
    }
    command
        .env("AUTH_DEVELOPMENT_MODE", "false")
        .env("SERVER_HOST", "127.0.0.1")
        .env("METRICS_ENABLED", "false")
        .env("RUST_LOG", "info")
        .env("LOG_FORMAT", "json")
        .env("NO_PROXY", "*");
    command
}

fn run_to_exit(extra_env: &[(&str, &str)], timeout: Duration) -> (Option<i32>, String) {
    let mut command = gateway_command();
    for (key, value) in extra_env {
        command.env(key, value);
    }
    let mut child = command
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn gateway");
    let started = Instant::now();
    loop {
        match child.try_wait().expect("wait") {
            Some(status) => {
                let mut stdout = String::new();
                let mut stderr = String::new();
                if let Some(mut out) = child.stdout.take() {
                    let _ = out.read_to_string(&mut stdout);
                }
                if let Some(mut err) = child.stderr.take() {
                    let _ = err.read_to_string(&mut stderr);
                }
                return (status.code(), format!("{stdout}{stderr}"));
            }
            None if started.elapsed() > timeout => {
                let _ = child.kill();
                let _ = child.wait();
                panic!("gateway did not exit before timeout");
            }
            None => thread::sleep(Duration::from_millis(20)),
        }
    }
}

fn assert_no_listener(port: u16) {
    let connected = TcpStream::connect_timeout(
        &format!("127.0.0.1:{port}").parse().unwrap(),
        Duration::from_millis(100),
    )
    .is_ok();
    assert!(!connected, "gateway must not listen after a config failure");
}

fn health_ok(port: u16) -> bool {
    let mut stream = match TcpStream::connect_timeout(
        &format!("127.0.0.1:{port}").parse().unwrap(),
        Duration::from_millis(200),
    ) {
        Ok(stream) => stream,
        Err(_) => return false,
    };
    let _ = stream.set_read_timeout(Some(Duration::from_millis(200)));
    if stream
        .write_all(
            b"GET /health HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n",
        )
        .is_err()
    {
        return false;
    }
    let mut body = String::new();
    let _ = stream.read_to_string(&mut body);
    body.contains("HTTP/1.1 200")
}

fn assert_no_secrets(output: &str) {
    assert!(!output.contains(SENTINEL_KEY));
    assert!(!output.contains(SENTINEL_URL_SECRET));
    assert!(!output.contains("Settings {"));
}

#[test]
fn valid_example_catalog_starts_and_serves_health() {
    let port = unused_loopback_port();
    let mut command = gateway_command();
    command
        .env("SERVER_PORT", port.to_string())
        .env("OPMUX_CONFIG_FILE", example_catalog_path())
        .env("OPENAI_API_KEY", "test-dummy-openai-key")
        .env("OPENAI_BASE_URL", "http://127.0.0.1:9/v1")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = command.spawn().expect("spawn valid gateway");
    let started = Instant::now();
    let mut healthy = false;
    while started.elapsed() < Duration::from_secs(5) {
        if let Ok(Some(status)) = child.try_wait() {
            panic!("gateway exited early with {status}");
        }
        if health_ok(port) {
            healthy = true;
            break;
        }
        thread::sleep(Duration::from_millis(50));
    }
    let _ = child.kill();
    let _ = child.wait();
    assert!(healthy, "valid configuration must serve /health");
}

#[test]
fn invalid_catalogs_exit_before_listen() {
    let catalog = TempCatalog::write("{not json");
    let port = unused_loopback_port();
    let (code, output) = run_to_exit(
        &[
            ("SERVER_PORT", &port.to_string()),
            ("OPMUX_CONFIG_FILE", catalog.path.to_str().unwrap()),
            ("OPENAI_API_KEY", SENTINEL_KEY),
            ("OPENAI_BASE_URL", "http://127.0.0.1:9/v1"),
        ],
        Duration::from_secs(5),
    );
    assert_ne!(code, Some(0));
    assert!(output.contains("catalog_malformed_json"));
    assert_no_listener(port);
    assert_no_secrets(&output);
}

#[test]
fn missing_config_file_exits_nonzero() {
    let port = unused_loopback_port();
    let (code, output) = run_to_exit(
        &[
            ("SERVER_PORT", &port.to_string()),
            ("OPENAI_API_KEY", SENTINEL_KEY),
            ("OPENAI_BASE_URL", "http://127.0.0.1:9/v1"),
        ],
        Duration::from_secs(5),
    );
    assert_ne!(code, Some(0));
    assert!(output.contains("missing_config_file"));
    assert_no_listener(port);
    assert_no_secrets(&output);
}

#[test]
fn blank_credential_exits_before_listen() {
    let catalog = TempCatalog::write(&valid_catalog());
    let port = unused_loopback_port();
    let (code, output) = run_to_exit(
        &[
            ("SERVER_PORT", &port.to_string()),
            ("OPMUX_CONFIG_FILE", catalog.path.to_str().unwrap()),
            ("OPENAI_API_KEY", "   "),
            ("OPENAI_BASE_URL", "http://127.0.0.1:9/v1"),
        ],
        Duration::from_secs(5),
    );
    assert_ne!(code, Some(0));
    assert!(output.contains("missing_credential"));
    assert_no_listener(port);
    assert_no_secrets(&output);
}

fn assert_invalid_provider_url_exits_before_listen(url: &str) {
    let catalog = TempCatalog::write(&valid_catalog());
    let port = unused_loopback_port();
    let port_text = port.to_string();
    let catalog_path = catalog.path.to_str().unwrap().to_string();
    let (code, output) = run_to_exit(
        &[
            ("SERVER_PORT", &port_text),
            ("OPMUX_CONFIG_FILE", &catalog_path),
            ("OPENAI_API_KEY", SENTINEL_KEY),
            ("OPENAI_BASE_URL", url),
        ],
        Duration::from_secs(5),
    );
    assert_ne!(code, Some(0), "url={url}");
    assert!(
        output.contains("invalid_provider_url"),
        "url={url} output={output}"
    );
    assert_no_listener(port);
    assert_no_secrets(&output);
    assert!(!output.contains("Settings {"));
    assert!(!format!("{output:?}").contains(SENTINEL_URL_SECRET));
}

#[test]
fn credential_bearing_url_is_omitted_from_diagnostics() {
    let url = format!("https://user:{SENTINEL_URL_SECRET}@example.invalid/v1");
    assert_invalid_provider_url_exits_before_listen(&url);
}

#[test]
fn query_and_fragment_provider_urls_exit_before_listen() {
    let cases = [
        format!("https://example.invalid/v1?api_key={SENTINEL_URL_SECRET}"),
        format!("https://example.invalid/v1#{SENTINEL_URL_SECRET}"),
        format!(
            "https://example.invalid/v1?api_key={SENTINEL_URL_SECRET}#{SENTINEL_URL_SECRET}"
        ),
        "http://127.0.0.1:9/v1?".to_string(),
        "http://127.0.0.1:9/v1#".to_string(),
        "http://127.0.0.1:9/v1?#".to_string(),
    ];
    for url in cases {
        assert_invalid_provider_url_exits_before_listen(&url);
    }
}

#[test]
fn invalid_limit_override_exits_before_listen() {
    let catalog = TempCatalog::write(&valid_catalog());
    let port = unused_loopback_port();
    let (code, output) = run_to_exit(
        &[
            ("SERVER_PORT", &port.to_string()),
            ("OPMUX_CONFIG_FILE", catalog.path.to_str().unwrap()),
            ("OPENAI_API_KEY", SENTINEL_KEY),
            ("OPENAI_BASE_URL", "http://127.0.0.1:9/v1"),
            ("OPMUX_MAX_TOTAL_ATTEMPTS", "0"),
        ],
        Duration::from_secs(5),
    );
    assert_ne!(code, Some(0));
    assert!(output.contains("invalid_limit"));
    assert_no_listener(port);
    assert_no_secrets(&output);
}

#[test]
fn binary_catalog_matrix_covers_remaining_invalid_cases() {
    let mut cases: HashMap<&str, String> = HashMap::new();
    cases.insert(
        "catalog_unsupported_version",
        serde_json::json!({
            "version": 2,
            "default_route": "default",
            "targets": {
                "primary": {
                    "model": "example-chat-model",
                    "max_output_tokens": 512,
                    "pricing": { "input_per_million": 1.0, "output_per_million": 2.0 }
                }
            },
            "routes": { "default": { "primary": "primary", "fallbacks": [] } }
        })
        .to_string(),
    );
    cases.insert("catalog_unknown_field", {
        let mut value: serde_json::Value =
            serde_json::from_str(&valid_catalog()).unwrap();
        value["extra"] = serde_json::json!(true);
        value.to_string()
    });
    cases.insert(
        "catalog_duplicate_target",
        r#"{
            "version": 1,
            "default_route": "default",
            "targets": {
                "primary": {
                    "model": "example-chat-model",
                    "max_output_tokens": 512,
                    "pricing": { "input_per_million": 1.0, "output_per_million": 2.0 }
                },
                "primary": {
                    "model": "other",
                    "max_output_tokens": 8,
                    "pricing": { "input_per_million": 1.0, "output_per_million": 2.0 }
                }
            },
            "routes": { "default": { "primary": "primary", "fallbacks": [] } }
        }"#
        .to_string(),
    );

    for (category, body) in cases {
        let catalog = TempCatalog::write(&body);
        let port = unused_loopback_port();
        let (code, output) = run_to_exit(
            &[
                ("SERVER_PORT", &port.to_string()),
                ("OPMUX_CONFIG_FILE", catalog.path.to_str().unwrap()),
                ("OPENAI_API_KEY", SENTINEL_KEY),
                ("OPENAI_BASE_URL", "http://127.0.0.1:9/v1"),
            ],
            Duration::from_secs(5),
        );
        assert_ne!(code, Some(0), "{category}");
        assert!(output.contains(category), "{category}");
        assert_no_listener(port);
        assert_no_secrets(&output);
    }
}

//! Release CI wiring: canonical migrations, local-equivalent gates, and honest
//! status classification. Most cases do not need a live database.

mod support;

use std::fs;
use std::net::TcpStream;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use support::isolate_provider_environment;

const INHERITED_PROVIDER_KEY: &str = "inherited-provider-key-do-not-use";

struct TempRoot {
    path: PathBuf,
}

impl TempRoot {
    fn new(prefix: &str) -> Self {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("time")
            .as_nanos();
        let path =
            std::env::temp_dir().join(format!("{prefix}-{}-{nanos}", std::process::id()));
        fs::create_dir_all(&path).expect("temp root");
        let mut permissions = fs::metadata(&path).expect("temp metadata").permissions();
        permissions.set_mode(0o700);
        fs::set_permissions(&path, permissions).expect("temp mode");
        Self { path }
    }

    fn path(&self) -> &Path {
        &self.path
    }

    fn child(&self, name: &str) -> PathBuf {
        self.path.join(name)
    }
}

impl Drop for TempRoot {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

struct ChildProc(Child);

impl Drop for ChildProc {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("..")
}

fn read_repo(relative: &str) -> String {
    fs::read_to_string(repo_root().join(relative)).unwrap_or_else(|_| {
        panic!("{relative} must exist");
    })
}

fn collapsed(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Collect published Postgres service ports from uncommented YAML list entries.
/// Comments and substring matches such as `55432:5432` are not sufficient.
fn postgres_published_ports(yaml: &str) -> Vec<String> {
    let mut in_postgres = false;
    let mut in_ports = false;
    let mut postgres_indent = 0usize;
    let mut ports_indent = 0usize;
    let mut ports = Vec::new();
    for line in yaml.lines() {
        let raw = line.trim_end();
        let trimmed = raw.trim_start();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        let indent = raw.len() - raw.trim_start().len();
        if trimmed == "postgres:" {
            in_postgres = true;
            postgres_indent = indent;
            in_ports = false;
            continue;
        }
        if in_postgres && indent <= postgres_indent {
            in_postgres = false;
            in_ports = false;
        }
        if !in_postgres {
            continue;
        }
        if trimmed == "ports:" {
            in_ports = true;
            ports_indent = indent;
            continue;
        }
        if in_ports {
            if indent <= ports_indent {
                in_ports = false;
            } else if let Some(value) = trimmed.strip_prefix("- ") {
                let value = value
                    .trim()
                    .trim_matches('"')
                    .trim_matches('\'')
                    .to_string();
                ports.push(value);
                continue;
            } else {
                in_ports = false;
            }
        }
    }
    ports
}

fn fenced_bash_blocks(markdown: &str) -> Vec<String> {
    let mut blocks = Vec::new();
    let mut in_bash = false;
    let mut current = String::new();
    for line in markdown.lines() {
        if line.starts_with("```bash") {
            in_bash = true;
            current.clear();
            continue;
        }
        if in_bash && line.starts_with("```") {
            blocks.push(std::mem::take(&mut current));
            in_bash = false;
            continue;
        }
        if in_bash {
            current.push_str(line);
            current.push('\n');
        }
    }
    blocks
}

fn assert_no_secrets(output: &str) {
    assert!(
        !output.contains(INHERITED_PROVIDER_KEY),
        "diagnostics must omit inherited provider keys"
    );
    assert!(
        !output.contains("postgresql://"),
        "diagnostics must omit connection URLs"
    );
}

fn install_fake_docker(root: &Path, port_status: i32) -> PathBuf {
    let bin = root.join("bin");
    fs::create_dir_all(&bin).expect("fake docker bin");
    let docker = bin.join("docker");
    fs::write(
        &docker,
        format!(
            r#"#!/bin/sh
set -eu
cmd=${{1:-}}
[ -n "$cmd" ]
shift
expected=supabase_db_opmux-mvp-20260919
case "$cmd" in
  port)
    if [ "${{1:-}}" != "$expected" ] || [ "${{2:-}}" != "5432/tcp" ]; then
      exit 1
    fi
    exit {port_status}
    ;;
  *)
    exit 127
    ;;
esac
"#
        ),
    )
    .expect("write fake docker");
    let mut permissions = fs::metadata(&docker)
        .expect("docker metadata")
        .permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&docker, permissions).expect("docker executable");
    bin
}

#[test]
fn ci_workflow_pins_rust_and_locked_workspace_gates() {
    let yaml = collapsed(&read_repo(".github/workflows/ci.yml"));
    let local = collapsed(&read_repo("scripts/ci-local.sh"));
    for source in [&yaml, &local] {
        assert!(
            source.contains(
                "cargo test --workspace --locked --all-features -j 2 -- --test-threads=2"
            ),
            "CI and local equivalent must run the locked workspace test gate"
        );
        assert!(
            source.contains(
                "cargo check --workspace --locked --all-targets --all-features -j 2"
            ),
            "CI and local equivalent must run cargo check"
        );
        assert!(
            source.contains(
                "cargo clippy --workspace --locked --all-targets --all-features -j 2"
            ),
            "CI and local equivalent must run locked clippy"
        );
        assert!(source.contains("-D warnings"), "clippy must deny warnings");
        assert!(
            source.contains("cargo fmt --all -- --check"),
            "fmt check is required"
        );
        assert!(
            source.contains("npm run format:check"),
            "prettier check is required"
        );
        assert!(
            source.contains(
                "docker build --file gateway/Dockerfile --tag opmux-gateway:mvp"
            ),
            "image build is required"
        );
        assert!(
            !source.contains("--ignored"),
            "routine CI must not invoke ignored live-provider tests"
        );
        assert!(
            !source.contains("supabase start"),
            "CI must not start a second Supabase stack"
        );
    }
    assert!(
        yaml.contains("toolchain: \"1.89.0\"") || yaml.contains("toolchain: 1.89.0"),
        "CI must pin Rust 1.89.0"
    );
    assert!(!yaml.contains("nightly") && !yaml.contains("beta"));
}

#[test]
fn ci_postgres_port_parser_ignores_comments_and_requires_complete_mapping() {
    let commented = r#"
    services:
      postgres:
        # ports:
        #   - 127.0.0.1:55432:5432
        ports:
          - 55432:5432
"#;
    assert_eq!(
        postgres_published_ports(commented),
        vec!["55432:5432".to_string()],
        "commented loopback mappings must not count as the published port"
    );
    let loopback = r#"
    services:
      postgres:
        ports:
          - 127.0.0.1:55432:5432
"#;
    assert_eq!(
        postgres_published_ports(loopback),
        vec!["127.0.0.1:55432:5432".to_string()]
    );
}

#[test]
fn ci_workflow_provisions_postgres_and_canonical_migrations() {
    let yaml_raw = read_repo(".github/workflows/ci.yml");
    let yaml = collapsed(&yaml_raw);
    assert!(
        yaml.contains("postgres:17.6"),
        "CI must provision disposable Postgres 17"
    );
    assert_eq!(
        postgres_published_ports(&yaml_raw),
        vec!["127.0.0.1:55432:5432".to_string()],
        "CI Postgres must publish the complete loopback mapping, not a substring or comment"
    );
    assert!(
        yaml.contains("scripts/ci-setup-db.sh"),
        "CI must apply canonical migrations"
    );
    assert!(
        yaml.contains("scripts/with-safe-test-env.sh"),
        "CI tests must use the safe provider wrapper"
    );
    assert!(
        yaml.contains("scripts/check-startup.sh"),
        "CI must run the startup smoke with DATABASE_URL"
    );
    let setup = collapsed(&read_repo("scripts/ci-setup-db.sh"));
    assert!(setup.contains("scripts/db-migrate.sh"));
    assert!(setup.contains("Persistence checks do not skip"));
    assert!(setup.contains("reapplication is a no-op") || setup.contains("no-op"));
}

#[test]
fn ci_workflow_cannot_contact_real_provider_from_inherited_keys() {
    let yaml = collapsed(&read_repo(".github/workflows/ci.yml"));
    let wrapper = collapsed(&read_repo("scripts/with-safe-test-env.sh"));
    assert!(wrapper.contains("-u OPENAI_API_KEY"));
    assert!(wrapper.contains("-u ANTHROPIC_API_KEY"));
    assert!(wrapper.contains("OPENAI_BASE_URL=http://127.0.0.1:9/v1"));
    assert!(wrapper.contains("AUTH_DEVELOPMENT_MODE=false"));
    assert!(wrapper.contains("-u OPMUX_LIVE_PROVIDER_TESTS"));
    assert!(yaml.contains("AUTH_DEVELOPMENT_MODE: \"false\""));
    let security = collapsed(&read_repo(".github/workflows/security-check.yml"));
    assert!(
        !security.contains("scripts/check-startup.sh"),
        "security workflow must not run startup smoke without a database"
    );
}

#[test]
fn rust_toolchain_file_pins_msrv() {
    let toolchain = read_repo("rust-toolchain.toml");
    assert!(toolchain.contains("1.89.0"));
    assert!(toolchain.contains("rustfmt"));
    assert!(toolchain.contains("clippy"));
}

#[test]
fn local_ci_script_requires_owned_database_and_does_not_skip() {
    isolate_provider_environment();
    let root = TempRoot::new("opmux-ci-local-missing-db");
    let bin = install_fake_docker(root.path(), 1);
    let path = format!("{}:{}", bin.display(), std::env::var("PATH").expect("PATH"));
    let output = Command::new("bash")
        .arg(repo_root().join("scripts/ci-local.sh"))
        .current_dir(repo_root())
        .env("PATH", path)
        .env("SKIP_IMAGE", "1")
        .env("SKIP_CONTAINER_CHECK", "1")
        .env("OPENAI_API_KEY", INHERITED_PROVIDER_KEY)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("run ci-local");
    assert_ne!(output.status.code(), Some(0));
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let combined = format!("{stdout}{stderr}");
    assert!(
        combined.contains("owned supabase")
            || combined.contains("not running")
            || combined.contains("do not skip"),
        "missing database must fail clearly"
    );
    assert!(
        !combined.contains("local CI equivalent passed"),
        "ci-local must not report success when setup is unavailable"
    );
    assert!(
        !combined.contains("PARTIAL"),
        "missing database is a failure, not a partial skip result"
    );
    assert_no_secrets(&combined);
}

#[test]
fn native_startup_docs_use_loopback_owned_db_and_matching_curls() {
    let observability = read_repo("gateway/tests/OBSERVABILITY_TESTING.md");
    let troubleshooting = read_repo("docs/CONFIGURATION_TROUBLESHOOTING.md");
    for (path, source) in [
        (
            "gateway/tests/OBSERVABILITY_TESTING.md",
            observability.as_str(),
        ),
        (
            "docs/CONFIGURATION_TROUBLESHOOTING.md",
            troubleshooting.as_str(),
        ),
    ] {
        let blocks = fenced_bash_blocks(source);
        let startup: Vec<&String> = blocks
            .iter()
            .filter(|block| {
                block.contains("cargo run -p gateway --bin gateway")
                    && !block.contains("opmux-admin")
            })
            .collect();
        assert!(
            !startup.is_empty(),
            "{path} must document a native `cargo run -p gateway --bin gateway` recipe"
        );
        for block in startup {
            assert!(
                block.contains("SERVER_HOST=127.0.0.1"),
                "{path} native startup must bind loopback"
            );
            assert!(
                block.contains("SERVER_PORT=38080"),
                "{path} native startup must use approved port 38080"
            );
            assert!(
                block.contains("scripts/with-owned-database.sh"),
                "{path} native startup must use the owned database wrapper"
            );
            assert!(
                block.contains("OPENAI_API_KEY=dummy-key"),
                "{path} native startup must use a dummy provider key"
            );
            assert!(
                block.contains("OPENAI_BASE_URL=http://127.0.0.1:"),
                "{path} native startup must use a loopback upstream"
            );
            assert!(
                !block.contains("SERVER_PORT=3000"),
                "{path} native startup must not run port 3000"
            );
            assert!(
                !block.contains("source .env") && !block.contains("source ./.env"),
                "{path} must not imply automatic .env loading"
            );
        }
        let curls: Vec<&String> = blocks
            .iter()
            .filter(|block| block.contains("curl") && block.contains("/health"))
            .collect();
        assert!(
            !curls.is_empty(),
            "{path} must include adjacent health curls"
        );
        for block in curls {
            assert!(
                block.contains("127.0.0.1:38080"),
                "{path} curls must target the documented loopback port"
            );
            assert!(
                !block.contains("127.0.0.1:3000"),
                "{path} curls must not target port 3000"
            );
        }
    }
    assert!(
        !observability.contains("127.0.0.1:3000"),
        "observability guide must not exercise port 3000"
    );
}

#[test]
fn ci_setup_db_fails_clearly_without_database_url() {
    isolate_provider_environment();
    let output = Command::new("bash")
        .arg(repo_root().join("scripts/ci-setup-db.sh"))
        .current_dir(repo_root())
        .env_remove("DATABASE_URL")
        .env("OPENAI_API_KEY", INHERITED_PROVIDER_KEY)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("run ci-setup-db");
    assert_ne!(output.status.code(), Some(0));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("DATABASE_URL"));
    assert!(stderr.contains("do not skip"));
    assert_no_secrets(&stderr);
}

#[test]
fn startup_check_fails_without_database_url() {
    isolate_provider_environment();
    let output = Command::new("bash")
        .arg(repo_root().join("scripts/check-startup.sh"))
        .arg("/bin/true")
        .current_dir(repo_root())
        .env_remove("DATABASE_URL")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("run startup check");
    assert_ne!(output.status.code(), Some(0));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("DATABASE_URL"));
    assert_no_secrets(&stderr);
}

#[test]
fn safe_test_env_replaces_inherited_provider_configuration() {
    isolate_provider_environment();
    let output = Command::new("bash")
        .arg(repo_root().join("scripts/with-safe-test-env.sh"))
        .args([
            "sh",
            "-c",
            "printf '%s' \"$OPENAI_API_KEY|$OPENAI_BASE_URL|$AUTH_DEVELOPMENT_MODE|$NO_PROXY\"",
        ])
        .env("OPENAI_API_KEY", INHERITED_PROVIDER_KEY)
        .env("OPENAI_BASE_URL", "https://api.openai.com/v1")
        .env("AUTH_DEVELOPMENT_MODE", "true")
        .env("OPMUX_LIVE_PROVIDER_TESTS", "1")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("run safe test env");
    assert_eq!(output.status.code(), Some(0));
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!stdout.contains(INHERITED_PROVIDER_KEY));
    assert!(!stdout.contains("api.openai.com"));
    assert!(stdout.contains("http://127.0.0.1:9/v1"));
    assert!(stdout.contains("false"));
    assert!(stdout.contains('*'));
    assert_no_secrets(&stderr);
}

#[test]
fn load_script_classifies_unauthorized_as_failure() {
    isolate_provider_environment();
    let root = TempRoot::new("opmux-load-401");
    let script = root.child("server.py");
    fs::write(
        &script,
        r#"
import os
from http.server import BaseHTTPRequestHandler, HTTPServer

class Handler(BaseHTTPRequestHandler):
    def do_GET(self):
        self.send_response(200)
        self.end_headers()
        self.wfile.write(b"ok")
    def do_POST(self):
        self.send_response(int(os.environ["ROUTE_STATUS"]))
        self.end_headers()
        self.wfile.write(b"unauthorized")
    def log_message(self, format, *args):
        return

server = HTTPServer(("127.0.0.1", 0), Handler)
with open(os.environ["PORT_FILE"], "w", encoding="utf-8") as handle:
    handle.write(str(server.server_address[1]))
server.serve_forever()
"#,
    )
    .expect("write python server");
    let port_file = root.child("port");
    let child = Command::new("python3")
        .arg(&script)
        .env("ROUTE_STATUS", "401")
        .env("PORT_FILE", &port_file)
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .expect("start python server");
    let _guard = ChildProc(child);
    let deadline = Instant::now() + Duration::from_secs(5);
    let port: u16 = loop {
        if Instant::now() >= deadline {
            panic!("python fixture did not publish a port");
        }
        if let Ok(text) = fs::read_to_string(&port_file) {
            if let Ok(port) = text.trim().parse() {
                break port;
            }
        }
        std::thread::sleep(Duration::from_millis(25));
    };
    wait_for_port(port);

    let output = Command::new("bash")
        .arg(repo_root().join("scripts/run-load-tests.sh"))
        .current_dir(repo_root())
        .env("GATEWAY_BASE_URL", format!("http://127.0.0.1:{port}"))
        .env("GATEWAY_API_KEY", "opmx_v1_load-test-not-a-secret")
        .env("TOTAL_REQUESTS", "1")
        .env("CONCURRENCY", "1")
        .env("REQUEST_TIMEOUT_SECS", "3")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("run load script");
    assert_ne!(
        output.status.code(),
        Some(0),
        "unauthorized responses must not count as load-test success"
    );
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        combined.contains("Failed non-2xx") || combined.contains("non-2xx"),
        "load results must classify non-2xx honestly"
    );
    let script_text = read_repo("scripts/run-load-tests.sh");
    assert!(
        !script_text.contains("-lt 500"),
        "4xx statuses must not be treated as success"
    );
}

fn wait_for_port(port: u16) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        if TcpStream::connect(("127.0.0.1", port)).is_ok() {
            return;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    panic!("python fixture did not listen on loopback");
}

#[test]
fn load_script_requires_provisioned_key() {
    isolate_provider_environment();
    let output = Command::new("bash")
        .arg(repo_root().join("scripts/run-load-tests.sh"))
        .current_dir(repo_root())
        .env_remove("GATEWAY_API_KEY")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("run load script");
    assert_ne!(output.status.code(), Some(0));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("GATEWAY_API_KEY"));
}

//! Untrusted local TLS cannot succeed through the bounded HTTP client.
//!
//! Certificate-chain and hostname verification stay enabled. The fixture
//! certificate is test-only and is not a production CA. No `curl -k` or
//! accept-invalid TLS setting is used.

mod support;

use gateway::core::config::build_bounded_http_client_for_base_url;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use support::isolate_provider_environment;

struct UntrustedTlsSimulator {
    child: Child,
    work_dir: PathBuf,
    base_url: String,
}

impl UntrustedTlsSimulator {
    fn start() -> Self {
        isolate_provider_environment();
        let repo = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .expect("workspace root")
            .to_path_buf();
        let work_dir = unique_work_dir();
        let generate = repo.join("scripts/generate-untrusted-tls.sh");
        let simulator = repo.join("scripts/openai-simulator.py");
        let status = Command::new("sh")
            .arg(&generate)
            .arg(&work_dir)
            .status()
            .expect("spawn generate-untrusted-tls.sh");
        assert!(status.success(), "failed to create untrusted TLS fixture");

        let mut child = Command::new("python3")
            .arg(&simulator)
            .args([
                "--host",
                "127.0.0.1",
                "--port",
                "0",
                "--credential",
                "test-dummy-openai-key",
                "--tls-cert",
            ])
            .arg(work_dir.join("cert.pem"))
            .arg("--tls-key")
            .arg(work_dir.join("key.pem"))
            .arg("--print-port")
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn untrusted TLS simulator");
        let stdout = child.stdout.take().expect("simulator stdout");
        let mut port_line = String::new();
        BufReader::new(stdout)
            .read_line(&mut port_line)
            .expect("read simulator port");
        let port: u16 = port_line.trim().parse().expect("simulator printed a port");
        assert!(port > 0, "simulator port");
        Self {
            child,
            work_dir,
            base_url: format!("https://127.0.0.1:{port}/v1"),
        }
    }
}

impl Drop for UntrustedTlsSimulator {
    fn drop(&mut self) {
        let pid = self.child.id();
        if self.child.try_wait().ok().flatten().is_none() {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
        debug_assert!(
            !pid_alive(pid),
            "untrusted TLS simulator PID must be reaped"
        );
        let _ = std::fs::remove_dir_all(&self.work_dir);
    }
}

fn unique_work_dir() -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("time")
        .as_nanos();
    let dir = std::env::temp_dir().join(format!(
        "opmux-untrusted-tls-{}-{nanos}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).expect("create TLS fixture dir");
    dir
}

fn pid_alive(pid: u32) -> bool {
    Path::new("/proc").join(pid.to_string()).exists()
        || Command::new("kill")
            .args(["-0", &pid.to_string()])
            .stderr(Stdio::null())
            .status()
            .map(|status| status.success())
            .unwrap_or(false)
}

#[tokio::test]
async fn untrusted_local_tls_cannot_complete_an_http_request() {
    let simulator = UntrustedTlsSimulator::start();
    let client = build_bounded_http_client_for_base_url(
        Duration::from_secs(2),
        &simulator.base_url,
    )
    .expect("bounded client");
    let result = client
        .get(format!("{}/models", simulator.base_url))
        .send()
        .await;
    match result {
        Ok(_) => panic!("untrusted TLS simulator must not yield HTTP success"),
        Err(error) => {
            assert!(
                !error.is_status(),
                "TLS verification must fail before an HTTP status"
            );
            let text = error.to_string();
            assert!(!text.contains("test-dummy-openai-key"));
            assert!(!text.contains("BEGIN CERTIFICATE"));
        }
    }
}

#[test]
fn production_http_client_keeps_certificate_and_hostname_verification() {
    let production = include_str!("../src/core/config/http_client.rs")
        .split("#[cfg(test)]")
        .next()
        .expect("production source");
    assert!(!production.contains("danger_accept_invalid_certs"));
    assert!(!production.contains("danger_accept_invalid_hostnames"));
    assert!(!production.contains("danger_accept_invalid"));
    assert!(production.contains("tls_built_in_root_certs(true)"));
    assert!(!production.contains("Client::new()"));
}

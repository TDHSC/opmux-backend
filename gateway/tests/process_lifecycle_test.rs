//! Occupied-port startup and SIGTERM/SIGINT draining against actual gateway
//! subprocesses, the production router, and the owned loopback simulator.
//!
//! Cleanup targets captured owned PIDs only. Evidence omits credentials,
//! digests, connection strings, and raw upstream bodies.

mod support;

use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use gateway::{
    app::Application, core::metrics::MetricsConfig, features::auth::CREDENTIAL_PREFIX,
};
use serial_test::serial;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};
use support::{
    auth_service_from_pool, cleanup_clients, isolate_provider_environment,
    provision_inference_key, required_database_url, settings_for_simulator, test_pool,
    OpenAiSimulator, ScriptedResponse, SIMULATED_CONTENT,
};
use tower::ServiceExt;

const ROUTE_BODY: &str = r#"{"prompt":"lifecycle-drain","metadata":{}}"#;
const SENTINEL_KEY: &str = "process-lifecycle-sentinel-key";
const GRACE_SECS: u64 = 1;
const GRACE_TOLERANCE: Duration = Duration::from_secs(2);
const SLOW_PROVIDER_DELAY: Duration = Duration::from_secs(8);

struct OccupiedListener {
    port: u16,
    stop: Arc<AtomicBool>,
    join: Option<thread::JoinHandle<()>>,
}

impl OccupiedListener {
    fn bind() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("fixture bind");
        let addr = listener.local_addr().expect("fixture addr");
        assert!(addr.ip().is_loopback());
        let stop = Arc::new(AtomicBool::new(false));
        let thread_stop = stop.clone();
        let join = thread::spawn(move || {
            listener.set_nonblocking(true).expect("nonblocking");
            while !thread_stop.load(Ordering::SeqCst) {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        let mut buf = [0u8; 256];
                        let _ = stream.read(&mut buf);
                        let _ = stream.write_all(
                            b"HTTP/1.1 200 OK\r\nContent-Length: 13\r\nConnection: close\r\n\r\nfixture-owner",
                        );
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(10));
                    }
                    Err(_) => break,
                }
            }
        });
        Self {
            port: addr.port(),
            stop,
            join: Some(join),
        }
    }

    fn control_response(&self) -> String {
        let mut stream = TcpStream::connect_timeout(
            &format!("127.0.0.1:{}", self.port).parse().unwrap(),
            Duration::from_secs(1),
        )
        .expect("fixture still accepts");
        stream
            .write_all(b"GET / HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n")
            .expect("fixture write");
        let _ = stream.set_read_timeout(Some(Duration::from_secs(1)));
        let mut body = String::new();
        let _ = stream.read_to_string(&mut body);
        body
    }
}

impl Drop for OccupiedListener {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        let _ = TcpStream::connect_timeout(
            &format!("127.0.0.1:{}", self.port).parse().unwrap(),
            Duration::from_millis(100),
        );
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
    }
}

struct OwnedGateway {
    child: Child,
    port: u16,
}

impl Drop for OwnedGateway {
    fn drop(&mut self) {
        let pid = self.child.id();
        if self.child.try_wait().ok().flatten().is_none() {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
        debug_assert!(
            !pid_alive(pid),
            "owned gateway pid must not remain after drop"
        );
    }
}

struct TtlGuard {
    prev: Option<String>,
}

impl TtlGuard {
    fn set(secs: &str) -> Self {
        let prev = std::env::var("HEALTH_CHECK_CACHE_TTL_SECS").ok();
        std::env::set_var("HEALTH_CHECK_CACHE_TTL_SECS", secs);
        Self { prev }
    }
}

impl Drop for TtlGuard {
    fn drop(&mut self) {
        match &self.prev {
            Some(value) => std::env::set_var("HEALTH_CHECK_CACHE_TTL_SECS", value),
            None => std::env::remove_var("HEALTH_CHECK_CACHE_TTL_SECS"),
        }
    }
}

fn example_catalog_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../config/opmux.example.json")
}

fn pid_alive(pid: u32) -> bool {
    Command::new("kill")
        .args(["-0", &pid.to_string()])
        .stderr(Stdio::null())
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}

fn send_signal(pid: u32, signal: &str) {
    let status = Command::new("kill")
        .arg(format!("-{signal}"))
        .arg(pid.to_string())
        .status()
        .expect("send signal to owned pid");
    assert!(status.success(), "failed to signal owned pid with {signal}");
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
        .env("TOKIO_WORKER_THREADS", "2")
        .env("NO_PROXY", "*")
        .env("RUST_LOG", "info")
        .env("LOG_FORMAT", "json");
    command
}

fn wait_child_exit(child: &mut Child, timeout: Duration) -> Option<ExitStatus> {
    let started = Instant::now();
    loop {
        match child.try_wait().expect("wait owned gateway") {
            Some(status) => return Some(status),
            None if started.elapsed() > timeout => return None,
            None => thread::sleep(Duration::from_millis(20)),
        }
    }
}

async fn wait_child_exit_async(
    child: &mut Child,
    timeout: Duration,
) -> Option<ExitStatus> {
    let started = Instant::now();
    loop {
        match child.try_wait().expect("wait owned gateway") {
            Some(status) => return Some(status),
            None if started.elapsed() > timeout => return None,
            None => tokio::time::sleep(Duration::from_millis(20)).await,
        }
    }
}

fn child_output(child: &mut Child) -> String {
    let mut stdout = String::new();
    let mut stderr = String::new();
    if let Some(mut out) = child.stdout.take() {
        let _ = out.read_to_string(&mut stdout);
    }
    if let Some(mut err) = child.stderr.take() {
        let _ = err.read_to_string(&mut stderr);
    }
    format!("{stdout}{stderr}")
}

fn unused_loopback_port() -> u16 {
    TcpListener::bind("127.0.0.1:0")
        .expect("bind probe")
        .local_addr()
        .expect("probe addr")
        .port()
}

fn http_client(timeout: Duration) -> reqwest::Client {
    reqwest::Client::builder()
        .no_proxy()
        .timeout(timeout)
        .build()
        .expect("http client")
}

async fn wait_health(client: &reqwest::Client, port: u16, child: &mut Child) -> bool {
    let started = Instant::now();
    while started.elapsed() < Duration::from_secs(8) {
        if child.try_wait().ok().flatten().is_some() {
            return false;
        }
        let url = format!("http://127.0.0.1:{port}/health");
        if let Ok(response) = client.get(&url).send().await {
            if response.status().is_success() {
                return true;
            }
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    false
}

fn spawn_gateway(
    port: u16,
    database_url: &str,
    simulator_base: &str,
    extra: &[(&str, &str)],
) -> OwnedGateway {
    let mut command = gateway_command();
    command
        .env("DATABASE_URL", database_url)
        .env("SERVER_PORT", port.to_string())
        .env("OPMUX_CONFIG_FILE", example_catalog_path())
        .env("OPENAI_API_KEY", "test-dummy-openai-key")
        .env("OPENAI_BASE_URL", simulator_base)
        .env("OPENAI_TIMEOUT_MS", "20000")
        .env("SERVER_SHUTDOWN_TIMEOUT", GRACE_SECS.to_string())
        .env("HEALTH_CHECK_CACHE_TTL_SECS", "60")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for (key, value) in extra {
        command.env(key, value);
    }
    let child = command.spawn().expect("spawn owned gateway");
    OwnedGateway { child, port }
}

async fn start_gateway(
    client: &reqwest::Client,
    database_url: &str,
    simulator_base: &str,
    extra: &[(&str, &str)],
) -> OwnedGateway {
    start_gateway_on(
        client,
        unused_loopback_port(),
        database_url,
        simulator_base,
        extra,
    )
    .await
}

async fn start_gateway_on(
    client: &reqwest::Client,
    port: u16,
    database_url: &str,
    simulator_base: &str,
    extra: &[(&str, &str)],
) -> OwnedGateway {
    let started = Instant::now();
    loop {
        let mut gateway = spawn_gateway(port, database_url, simulator_base, extra);
        if wait_health(client, port, &mut gateway.child).await {
            return gateway;
        }
        let output = child_output(&mut gateway.child);
        let _ = gateway.child.kill();
        let _ = gateway.child.wait();
        assert!(
            !output.contains(CREDENTIAL_PREFIX),
            "gateway diagnostics must omit credentials"
        );
        assert!(
            started.elapsed() < Duration::from_secs(8),
            "owned gateway failed to become healthy on the requested port"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

fn listener_closed(port: u16) -> bool {
    TcpStream::connect_timeout(
        &format!("127.0.0.1:{port}").parse().unwrap(),
        Duration::from_millis(150),
    )
    .is_err()
}

#[derive(Debug)]
enum Probe {
    Http { status: u16, body: String },
    ConnectionRefused,
    Other,
}

async fn probe(client: &reqwest::Client, url: &str) -> Probe {
    match client.get(url).send().await {
        Ok(response) => {
            let status = response.status().as_u16();
            let body = response.text().await.unwrap_or_default();
            Probe::Http { status, body }
        }
        Err(error) if error.is_connect() => Probe::ConnectionRefused,
        Err(_) => Probe::Other,
    }
}

async fn probe_generation(
    client: &reqwest::Client,
    url: &str,
    credential: &str,
) -> Probe {
    match client
        .post(url)
        .header("content-type", "application/json")
        .header("x-api-key", credential)
        .body(ROUTE_BODY)
        .send()
        .await
    {
        Ok(response) => {
            let status = response.status().as_u16();
            let body = response.text().await.unwrap_or_default();
            Probe::Http { status, body }
        }
        Err(error) if error.is_connect() => Probe::ConnectionRefused,
        Err(_) => Probe::Other,
    }
}

fn json_code(body: &str) -> String {
    serde_json::from_str::<serde_json::Value>(body)
        .ok()
        .and_then(|value| value["error"]["code"].as_str().map(ToOwned::to_owned))
        .unwrap_or_default()
}

async fn wait_generations(simulator: &OpenAiSimulator, count: usize) {
    let started = Instant::now();
    while simulator.generation_count() < count {
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "simulator did not observe admitted generation"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

async fn body_json(response: axum::http::Response<Body>) -> serde_json::Value {
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("body");
    serde_json::from_slice(&bytes).expect("json")
}

#[test]
#[serial]
fn occupied_port_startup_fails_without_disturbing_listener() {
    isolate_provider_environment();
    let fixture = OccupiedListener::bind();
    let before = fixture.control_response();
    assert!(before.contains("HTTP/1.1 200"));
    assert!(before.contains("fixture-owner"));

    let database_url = required_database_url();
    let mut command = gateway_command();
    command
        .env("DATABASE_URL", &database_url)
        .env("SERVER_PORT", fixture.port.to_string())
        .env("OPMUX_CONFIG_FILE", example_catalog_path())
        .env("OPENAI_API_KEY", SENTINEL_KEY)
        .env("OPENAI_BASE_URL", "http://127.0.0.1:9/v1")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = command.spawn().expect("spawn colliding gateway");
    let pid = child.id();
    let status = wait_child_exit(&mut child, Duration::from_secs(5));
    let output = child_output(&mut child);
    if status.is_none() {
        let _ = child.kill();
        let _ = child.wait();
        panic!("colliding gateway did not exit before the bounded wait");
    }
    let status = status.expect("exit status");
    assert_ne!(status.code(), Some(0));
    assert!(
        output.contains("bind_address_in_use"),
        "bind failure must use a sanitized category"
    );
    assert!(!output.contains("Gateway server running"));
    assert!(!output.contains("gateway listening"));
    assert!(!output.contains("\"status\":\"ready\""));
    assert!(!output.contains(SENTINEL_KEY));
    assert!(!output.contains(CREDENTIAL_PREFIX));
    assert!(!output.contains("postgres://"));
    assert!(!pid_alive(pid), "failed startup must not leave a process");
    assert!(
        !listener_closed(fixture.port),
        "existing listener must remain bound"
    );
    let after = fixture.control_response();
    assert!(after.contains("HTTP/1.1 200"));
    assert!(after.contains("fixture-owner"));
}

#[tokio::test]
#[serial]
async fn production_router_drain_gates_reject_generation_and_override_ready() {
    isolate_provider_environment();
    let _ttl = TtlGuard::set("60");
    let pool = test_pool().await;
    let issued = provision_inference_key(&pool).await;
    let simulator = OpenAiSimulator::start().await;
    let application = Application::from_settings_and_metrics(
        settings_for_simulator(&simulator),
        auth_service_from_pool(pool.clone()),
        MetricsConfig::disabled(),
    )
    .expect("application");
    let shutdown = application.state.shutdown.clone();
    let app = application.into_router(MetricsConfig::disabled());

    let ready = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/ready")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(ready.status(), StatusCode::OK);
    let ready_body = body_json(ready).await;
    assert_eq!(ready_body["status"], "ready");
    assert_eq!(ready_body["draining"], false);

    shutdown.mark_draining();

    let draining_ready = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/ready")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(draining_ready.status(), StatusCode::SERVICE_UNAVAILABLE);
    let draining_body = body_json(draining_ready).await;
    assert_eq!(draining_body["status"], "not_ready");
    assert_eq!(draining_body["draining"], true);

    let health = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/health")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(health.status(), StatusCode::OK);
    assert_eq!(body_json(health).await["status"], "healthy");

    let before = simulator.generation_count();
    let generation = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/route")
                .header("content-type", "application/json")
                .header("x-api-key", &issued.credential)
                .body(Body::from(ROUTE_BODY))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(generation.status(), StatusCode::SERVICE_UNAVAILABLE);
    let generation_body = body_json(generation).await;
    assert_eq!(generation_body["error"]["code"], "DRAINING");
    assert_eq!(
        generation_body["error"]["message"],
        "The service is shutting down"
    );
    assert_eq!(simulator.generation_count(), before);

    cleanup_clients(&pool, &[issued.client_id]).await;
}

async fn drain_signal_run(signal: &str) {
    isolate_provider_environment();
    let pool = test_pool().await;
    let issued = provision_inference_key(&pool).await;
    let simulator = OpenAiSimulator::start().await;
    let database_url = required_database_url();
    let client = http_client(Duration::from_secs(8));
    let mut gateway = start_gateway(
        &client,
        &database_url,
        simulator.base_url(),
        &[("EXECUTOR_MAX_RETRIES", "0")],
    )
    .await;
    let port = gateway.port;
    let pid = gateway.child.id();
    let base = format!("http://127.0.0.1:{port}");

    let (hold_script, hold) = ScriptedResponse::chat_ok().hold();
    simulator.enqueue_chat(hold_script);

    let inflight_client = http_client(Duration::from_secs(15));
    let inflight_url = format!("{base}/api/v1/route");
    let inflight_key = issued.credential.clone();
    let inflight = tokio::spawn(async move {
        inflight_client
            .post(inflight_url)
            .header("content-type", "application/json")
            .header("x-api-key", inflight_key)
            .body(ROUTE_BODY)
            .send()
            .await
    });
    wait_generations(&simulator, 1).await;
    let admitted_at = Instant::now();

    send_signal(pid, signal);
    let signal_at = Instant::now();

    let mut observed_ready_503 = false;
    let mut observed_generation_503 = false;
    let mut observed_refusal = false;
    let probe_deadline = Instant::now() + Duration::from_millis(800);
    while Instant::now() < probe_deadline && pid_alive(pid) {
        match probe(&client, &format!("{base}/health")).await {
            Probe::Http { status, body } => {
                assert_eq!(status, 200);
                assert!(body.contains("\"status\":\"healthy\""));
            }
            Probe::ConnectionRefused => observed_refusal = true,
            Probe::Other => {}
        }
        match probe(&client, &format!("{base}/ready")).await {
            Probe::Http { status, body } => {
                assert_eq!(status, 503);
                assert!(body.contains("\"status\":\"not_ready\""));
                observed_ready_503 = true;
            }
            Probe::ConnectionRefused => observed_refusal = true,
            Probe::Other => {}
        }
        match probe_generation(
            &client,
            &format!("{base}/api/v1/route"),
            &issued.credential,
        )
        .await
        {
            Probe::Http { status, body } => {
                assert_eq!(status, 503);
                assert_eq!(json_code(&body), "DRAINING");
                observed_generation_503 = true;
            }
            Probe::ConnectionRefused => observed_refusal = true,
            Probe::Other => {}
        }
        if observed_ready_503 && observed_generation_503 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(
        observed_ready_503 && observed_generation_503 || observed_refusal,
        "{signal}: expected draining HTTP 503 or connection refusal while shutting down"
    );
    assert_eq!(simulator.generation_count(), 1);

    hold.release();
    let inflight_result = inflight.await.expect("join inflight");
    let inflight_response = inflight_result.expect("inflight request");
    assert_eq!(inflight_response.status(), reqwest::StatusCode::OK);
    let inflight_body = inflight_response.text().await.unwrap_or_default();
    assert!(inflight_body.contains(SIMULATED_CONTENT));

    let status = wait_child_exit_async(
        &mut gateway.child,
        Duration::from_secs(GRACE_SECS) + GRACE_TOLERANCE,
    )
    .await;
    if status.is_none() {
        let _ = gateway.child.kill();
        let _ = gateway.child.wait();
        panic!("{signal}: gateway required a supervisor force-kill after drain");
    }
    assert_eq!(status.and_then(|value| value.code()), Some(0));
    assert!(!pid_alive(pid));
    assert!(listener_closed(port));
    assert!(
        signal_at.elapsed() < Duration::from_secs(GRACE_SECS) + GRACE_TOLERANCE,
        "{signal}: process must exit within grace"
    );
    assert!(admitted_at.elapsed() < Duration::from_secs(GRACE_SECS) + GRACE_TOLERANCE);

    cleanup_clients(&pool, &[issued.client_id]).await;
}

#[tokio::test]
#[serial]
async fn sigterm_drains_before_admitting_further_generation() {
    drain_signal_run("TERM").await;
}

#[tokio::test]
#[serial]
async fn sigint_drains_before_admitting_further_generation() {
    drain_signal_run("INT").await;
}

async fn grace_bound_run(signal: &str, kind: GraceWork) {
    isolate_provider_environment();
    let pool = test_pool().await;
    let issued = provision_inference_key(&pool).await;
    let simulator = OpenAiSimulator::start().await;
    let database_url = required_database_url();
    let client = http_client(Duration::from_secs(8));
    let retries = match kind {
        GraceWork::SlowProvider => "0",
        GraceWork::RetryBackoff => "1",
    };
    let mut gateway = start_gateway(
        &client,
        &database_url,
        simulator.base_url(),
        &[
            ("EXECUTOR_MAX_RETRIES", retries),
            ("OPMUX_BACKOFF_CAP_MS", "10000"),
        ],
    )
    .await;
    let port = gateway.port;
    let pid = gateway.child.id();
    let base = format!("http://127.0.0.1:{port}");

    match kind {
        GraceWork::SlowProvider => {
            simulator.enqueue_chat(
                ScriptedResponse::chat_ok().delay_headers(SLOW_PROVIDER_DELAY),
            );
        }
        GraceWork::RetryBackoff => {
            simulator.enqueue_chat(ScriptedResponse::rate_limited("10"));
            simulator.enqueue_chat(ScriptedResponse::chat_ok());
        }
    }

    let inflight_client = http_client(Duration::from_secs(20));
    let inflight_url = format!("{base}/api/v1/route");
    let inflight_key = issued.credential.clone();
    let inflight = tokio::spawn(async move {
        inflight_client
            .post(inflight_url)
            .header("content-type", "application/json")
            .header("x-api-key", inflight_key)
            .body(ROUTE_BODY)
            .send()
            .await
    });
    wait_generations(&simulator, 1).await;

    send_signal(pid, signal);
    let signal_at = Instant::now();
    let status = wait_child_exit_async(
        &mut gateway.child,
        Duration::from_secs(GRACE_SECS) + GRACE_TOLERANCE,
    )
    .await;
    let elapsed = signal_at.elapsed();
    if status.is_none() {
        let _ = gateway.child.kill();
        let _ = gateway.child.wait();
        panic!("{signal}: grace expiry required a supervisor force-kill");
    }
    assert!(
        elapsed <= Duration::from_secs(GRACE_SECS) + GRACE_TOLERANCE,
        "{signal}: exit {:?} exceeded grace plus tolerance",
        elapsed
    );
    assert!(!pid_alive(pid));
    assert!(listener_closed(port));
    assert_eq!(simulator.generation_count(), 1);
    let attempts = simulator.generation_received_at();
    assert_eq!(attempts.len(), 1);
    assert!(attempts[0] <= signal_at + GRACE_TOLERANCE);
    let _ = inflight.await;
    simulator.clear_chat_script();

    let restarted = start_gateway_on(
        &client,
        port,
        &database_url,
        simulator.base_url(),
        &[("EXECUTOR_MAX_RETRIES", "0")],
    )
    .await;
    let after_restart = simulator.generation_count();
    let (status, body) = {
        let response = client
            .post(format!("http://127.0.0.1:{}/api/v1/route", restarted.port))
            .header("content-type", "application/json")
            .header("x-api-key", &issued.credential)
            .body(ROUTE_BODY)
            .send()
            .await
            .expect("restart generation");
        (
            response.status().as_u16(),
            response.text().await.unwrap_or_default(),
        )
    };
    assert_eq!(status, 200);
    assert!(body.contains(SIMULATED_CONTENT));
    assert_eq!(simulator.generation_count(), after_restart + 1);
    drop(restarted);

    cleanup_clients(&pool, &[issued.client_id]).await;
}

#[derive(Clone, Copy)]
enum GraceWork {
    SlowProvider,
    RetryBackoff,
}

#[tokio::test]
#[serial]
async fn sigterm_cancels_slow_provider_work_at_grace() {
    grace_bound_run("TERM", GraceWork::SlowProvider).await;
}

#[tokio::test]
#[serial]
async fn sigint_cancels_retry_backoff_without_later_attempts() {
    grace_bound_run("INT", GraceWork::RetryBackoff).await;
}

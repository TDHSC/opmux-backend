//! Bounded release acceptance through actual `opmux-admin`, gateway, and
//! simulator processes against real local Supabase.
//!
//! OpenAI is **SIMULATED ONLY**. Evidence omits credentials, digests,
//! connection strings, prompts, and raw upstream bodies. Fixture tenants are
//! isolated and cleaned up; the shared database is retained.

mod support;

use chrono::{DateTime, Utc};
use gateway::features::auth::CREDENTIAL_PREFIX;
use serial_test::serial;
use std::io::Read;
use std::net::TcpListener;
use std::path::PathBuf;
use std::process::{Child, Command, Output, Stdio};
use std::time::{Duration, Instant};
use support::{
    cleanup_clients, isolate_provider_environment, required_database_url,
    rewrite_owned_database_url_port, test_pool, CapturedRequest, OpenAiSimulator,
    RecoverableDbProxy, ScriptedResponse, SIMULATED_CONTENT,
};
use uuid::Uuid;

const DEFAULT_MODEL: &str = "example-chat-model";
const FAST_MODEL: &str = "example-chat-model-mini";
const NAMED_REPORTED_MODEL: &str = "simulated-named-snapshot";
const FALLBACK_REPORTED_MODEL: &str = "simulated-fallback-snapshot";
const FALLBACK_CONTENT: &str = "SIMULATED_OPENAI_FALLBACK_OK";
const PROMPT_TOKENS: i64 = 120;
const COMPLETION_TOKENS: i64 = 30;
const SECONDARY_COST: f64 = 0.000045;
const NAMED_BODY: &str = r#"{"prompt":"release-named","metadata":{},"route":"fast"}"#;
const DEFAULT_BODY: &str = r#"{"prompt":"release-default","metadata":{}}"#;
// Example `secondary` cap is 256. Omitted max_tokens uses primary 512 and
// would skip the configured fallback hop.
const FALLBACK_BODY: &str =
    r#"{"prompt":"release-fallback","metadata":{},"parameters":{"max_tokens":128}}"#;
const CREATE_INFERENCE_BODY: &str =
    r#"{"name":"a-release-inference","kind":"inference"}"#;
const CREATE_REPLACEMENT_MANAGER_BODY: &str =
    r#"{"name":"a-release-replacement-manager","kind":"management"}"#;
const CREATE_REPLACEMENT_INFERENCE_BODY: &str =
    r#"{"name":"a-release-replacement-inference","kind":"inference"}"#;

struct Issued {
    client_id: Uuid,
    key_id: Uuid,
    credential: String,
}

struct OwnedGateway {
    child: Child,
    port: u16,
}

impl Drop for OwnedGateway {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

struct HttpCall {
    status: u16,
    body: String,
    cache_control: Option<String>,
}

fn example_catalog_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../config/opmux.example.json")
}

fn admin_bin() -> PathBuf {
    std::path::Path::new(env!("CARGO_BIN_EXE_gateway")).with_file_name("opmux-admin")
}

fn run_admin(args: &[&str]) -> Output {
    Command::new(admin_bin())
        .args(args)
        .env("DATABASE_URL", required_database_url())
        .env("OPMUX_DB_ROLE", "opmux_operator")
        .env("NO_PROXY", "*")
        .env_remove("OPENAI_API_KEY")
        .env_remove("ANTHROPIC_API_KEY")
        .output()
        .expect("spawn opmux-admin")
}

fn stdout_text(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn parse_issued_json(raw: &str) -> Issued {
    let value: serde_json::Value = serde_json::from_str(raw.trim())
        .unwrap_or_else(|_| panic!("issuance stdout was not JSON; output omitted"));
    let credential = value
        .get("credential")
        .and_then(|item| item.as_str())
        .unwrap_or("")
        .to_string();
    let client_id = value
        .get("client_id")
        .and_then(|item| item.as_str())
        .and_then(|item| Uuid::parse_str(item).ok())
        .expect("client_id");
    let key_id = value
        .get("key_id")
        .and_then(|item| item.as_str())
        .and_then(|item| Uuid::parse_str(item).ok())
        .expect("key_id");
    assert!(
        credential.starts_with(CREDENTIAL_PREFIX),
        "issuance must include a versioned credential"
    );
    Issued {
        client_id,
        key_id,
        credential,
    }
}

fn parse_cli_issued(output: &Output) -> Issued {
    assert!(
        output.status.success(),
        "opmux-admin exited nonzero; output omitted"
    );
    parse_issued_json(&stdout_text(output))
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
        .env("TOKIO_WORKER_THREADS", "2")
        .env("NO_PROXY", "*")
        .env("RUST_LOG", "info")
        .env("LOG_FORMAT", "json");
    command
}

fn spawn_gateway(port: u16, database_url: &str, simulator_base: &str) -> OwnedGateway {
    let mut command = gateway_command();
    command
        .env("DATABASE_URL", database_url)
        .env("SERVER_PORT", port.to_string())
        .env("OPMUX_CONFIG_FILE", example_catalog_path())
        .env("OPENAI_API_KEY", "test-dummy-openai-key")
        .env("OPENAI_BASE_URL", simulator_base)
        .env("OPENAI_TIMEOUT_MS", "2000")
        .env("EXECUTOR_MAX_RETRIES", "0")
        .env("OPMUX_BACKOFF_CAP_MS", "1")
        .env("OPMUX_DB_ACQUIRE_TIMEOUT_MS", "1000")
        .env("OPMUX_DB_MAX_CONNECTIONS", "2")
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    let child = command.spawn().expect("spawn gateway");
    OwnedGateway { child, port }
}

async fn wait_health(client: &reqwest::Client, port: u16, child: &mut Child) -> bool {
    let started = Instant::now();
    while started.elapsed() < Duration::from_secs(8) {
        if let Ok(Some(_)) = child.try_wait() {
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

async fn start_gateway(
    client: &reqwest::Client,
    database_url: &str,
    simulator_base: &str,
) -> OwnedGateway {
    let port = unused_loopback_port();
    let mut gateway = spawn_gateway(port, database_url, simulator_base);
    if !wait_health(client, port, &mut gateway.child).await {
        let mut stderr = String::new();
        if let Some(mut pipe) = gateway.child.stderr.take() {
            let _ = pipe.read_to_string(&mut stderr);
        }
        let _ = gateway.child.kill();
        let _ = gateway.child.wait();
        assert!(
            !stderr.contains(CREDENTIAL_PREFIX),
            "gateway diagnostics must omit credentials"
        );
        panic!("gateway failed to become healthy");
    }
    gateway
}

fn http_client() -> reqwest::Client {
    reqwest::Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(8))
        .build()
        .expect("http client")
}

async fn http_call(
    client: &reqwest::Client,
    method: reqwest::Method,
    url: &str,
    api_key: Option<&str>,
    body: Option<&str>,
) -> HttpCall {
    let mut request = client.request(method, url);
    if let Some(key) = api_key {
        request = request.header("x-api-key", key);
    }
    if let Some(body) = body {
        request = request
            .header("content-type", "application/json")
            .body(body.to_string());
    }
    let response = request.send().await.expect("process http");
    let status = response.status().as_u16();
    let cache_control = response
        .headers()
        .get("cache-control")
        .and_then(|value| value.to_str().ok())
        .map(ToOwned::to_owned);
    let body = response.text().await.unwrap_or_default();
    HttpCall {
        status,
        body,
        cache_control,
    }
}

fn json_value(body: &str) -> serde_json::Value {
    serde_json::from_str(body).unwrap_or(serde_json::json!({}))
}

fn error_code(body: &str) -> String {
    json_value(body)
        .get("error")
        .and_then(|error| error.get("code"))
        .and_then(|code| code.as_str())
        .unwrap_or("")
        .to_string()
}

fn assert_cost_eq(actual: f64, expected: f64) {
    assert!(
        (actual - expected).abs() < 1e-12,
        "cost differed from the configured estimate"
    );
    assert!(actual >= 0.0, "estimated cost must be nonnegative");
}

fn assert_secret_free(body: &str, secrets: &[&str]) {
    for secret in secrets {
        assert!(
            !body.contains(secret),
            "response must not echo a fixture credential"
        );
    }
    assert!(
        !body.contains("digest"),
        "response must not include a digest field"
    );
    assert!(
        !body.to_lowercase().contains("postgres://"),
        "response must not include a connection string"
    );
}

fn inventory_ids(body: &str) -> Vec<Uuid> {
    json_value(body)
        .get("keys")
        .and_then(|item| item.as_array())
        .map(|keys| {
            keys.iter()
                .filter_map(|item| {
                    item.get("key_id")
                        .and_then(|value| value.as_str())
                        .and_then(|value| Uuid::parse_str(value).ok())
                })
                .collect()
        })
        .unwrap_or_default()
}

fn inventory_item(body: &str, key_id: Uuid) -> Option<serde_json::Value> {
    json_value(body)
        .get("keys")
        .and_then(|item| item.as_array())
        .and_then(|keys| {
            keys.iter().find(|item| {
                item.get("key_id").and_then(|value| value.as_str())
                    == Some(key_id.to_string()).as_deref()
            })
        })
        .cloned()
}

fn generation_captures(simulator: &OpenAiSimulator) -> Vec<CapturedRequest> {
    simulator
        .captured()
        .into_iter()
        .filter(|capture| capture.is_generation())
        .collect()
}

fn generation_model(capture: &CapturedRequest) -> Option<&str> {
    capture
        .body
        .as_ref()
        .and_then(|body| body.get("model"))
        .and_then(|value| value.as_str())
}

async fn key_row(
    pool: &sqlx::PgPool,
    key_id: Uuid,
) -> Option<(Uuid, String, String, Option<DateTime<Utc>>)> {
    sqlx::query_as(
        "SELECT client_id, name, kind, revoked_at
         FROM opmux_private.api_keys WHERE id = $1",
    )
    .bind(key_id)
    .fetch_optional(pool)
    .await
    .expect("key row")
}

async fn last_used_at(pool: &sqlx::PgPool, key_id: Uuid) -> Option<DateTime<Utc>> {
    sqlx::query_scalar("SELECT last_used_at FROM opmux_private.api_keys WHERE id = $1")
        .bind(key_id)
        .fetch_one(pool)
        .await
        .expect("last_used_at")
}

fn enqueue_named_success(simulator: &OpenAiSimulator) {
    simulator.enqueue_chat(ScriptedResponse::ChatSuccess {
        content: SIMULATED_CONTENT.to_string(),
        model: Some(NAMED_REPORTED_MODEL.to_string()),
        prompt_tokens: PROMPT_TOKENS,
        completion_tokens: COMPLETION_TOKENS,
        finish_reason: "stop".to_string(),
        role: "assistant".to_string(),
    });
}

fn enqueue_fallback_pair(simulator: &OpenAiSimulator) {
    simulator.enqueue_chat(ScriptedResponse::json_status(
        500,
        serde_json::json!({"error":{"message":"simulated transient primary"}}),
    ));
    simulator.enqueue_chat(ScriptedResponse::ChatSuccess {
        content: FALLBACK_CONTENT.to_string(),
        model: Some(FALLBACK_REPORTED_MODEL.to_string()),
        prompt_tokens: PROMPT_TOKENS,
        completion_tokens: COMPLETION_TOKENS,
        finish_reason: "stop".to_string(),
        role: "assistant".to_string(),
    });
}

#[tokio::test]
#[serial]
async fn cli_http_lifecycle_composes_across_restart_and_simulated_faults() {
    isolate_provider_environment();
    let pool = test_pool().await;
    let name_a = format!("xacc-a-{}", Uuid::new_v4().simple());
    let name_b = format!("xacc-b-{}", Uuid::new_v4().simple());

    let a_manager =
        parse_cli_issued(&run_admin(&["tenant", "create", "--name", &name_a]));
    let b_manager =
        parse_cli_issued(&run_admin(&["tenant", "create", "--name", &name_b]));
    assert_ne!(a_manager.client_id, b_manager.client_id);
    assert_eq!(
        key_row(&pool, a_manager.key_id).await.expect("a manager").2,
        "management"
    );
    assert_eq!(
        key_row(&pool, b_manager.key_id).await.expect("b manager").2,
        "management"
    );

    let simulator = OpenAiSimulator::start().await;
    let proxy = RecoverableDbProxy::start().await;
    tokio::task::yield_now().await;
    let proxied = rewrite_owned_database_url_port(&required_database_url(), proxy.port());
    let client = http_client();
    let gateway = start_gateway(&client, &proxied, simulator.base_url()).await;
    let base = format!("http://127.0.0.1:{}", gateway.port);
    let secrets = [a_manager.credential.as_str(), b_manager.credential.as_str()];

    let created = http_call(
        &client,
        reqwest::Method::POST,
        &format!("{base}/api/v1/auth/keys"),
        Some(&a_manager.credential),
        Some(CREATE_INFERENCE_BODY),
    )
    .await;
    assert_eq!(created.status, 201);
    assert_eq!(created.cache_control.as_deref(), Some("no-store"));
    let a_inference = parse_issued_json(&created.body);
    assert_eq!(a_inference.client_id, a_manager.client_id);
    assert_eq!(
        key_row(&pool, a_inference.key_id)
            .await
            .expect("a inference")
            .2,
        "inference"
    );

    let listed = http_call(
        &client,
        reqwest::Method::GET,
        &format!("{base}/api/v1/auth/keys"),
        Some(&a_manager.credential),
        None,
    )
    .await;
    assert_eq!(listed.status, 200);
    let listed_ids = inventory_ids(&listed.body);
    assert!(listed_ids.contains(&a_manager.key_id));
    assert!(listed_ids.contains(&a_inference.key_id));
    assert!(!listed_ids.contains(&b_manager.key_id));
    assert_secret_free(
        &listed.body,
        &[
            &a_manager.credential,
            &b_manager.credential,
            &a_inference.credential,
        ],
    );

    let b_listed = http_call(
        &client,
        reqwest::Method::GET,
        &format!("{base}/api/v1/auth/keys"),
        Some(&b_manager.credential),
        None,
    )
    .await;
    assert_eq!(b_listed.status, 200);
    let b_ids = inventory_ids(&b_listed.body);
    assert_eq!(b_ids, vec![b_manager.key_id]);
    assert!(!b_ids.contains(&a_inference.key_id));
    assert_secret_free(&b_listed.body, &secrets);

    enqueue_named_success(&simulator);
    let before_named = simulator.generation_count();
    let named = http_call(
        &client,
        reqwest::Method::POST,
        &format!("{base}/api/v1/route"),
        Some(&a_inference.credential),
        Some(NAMED_BODY),
    )
    .await;
    assert_eq!(named.status, 200);
    let named_json = json_value(&named.body);
    assert_eq!(named_json["response"]["content"], SIMULATED_CONTENT);
    assert_eq!(named_json["response"]["role"], "assistant");
    assert_eq!(named_json["model_used"], NAMED_REPORTED_MODEL);
    assert_ne!(named_json["model_used"], FAST_MODEL);
    assert_eq!(named_json["usage"]["prompt_tokens"], PROMPT_TOKENS);
    assert_eq!(named_json["usage"]["completion_tokens"], COMPLETION_TOKENS);
    assert_cost_eq(
        named_json["cost"].as_f64().expect("named cost"),
        SECONDARY_COST,
    );
    assert!(
        named_json["processing_time_ms"]
            .as_u64()
            .unwrap_or(u64::MAX)
            < 30_000
    );
    assert_secret_free(&named.body, &[&a_inference.credential]);
    assert_eq!(simulator.generation_count(), before_named + 1);
    let named_capture = generation_captures(&simulator)
        .into_iter()
        .last()
        .expect("named capture");
    assert!(named_capture.authorization_matches_fixture);
    assert_eq!(generation_model(&named_capture), Some(FAST_MODEL));
    assert_eq!(named_capture.path, "/v1/chat/completions");

    let before_default = simulator.generation_count();
    let default_route = http_call(
        &client,
        reqwest::Method::POST,
        &format!("{base}/api/v1/route"),
        Some(&a_inference.credential),
        Some(DEFAULT_BODY),
    )
    .await;
    assert_eq!(default_route.status, 200);
    let default_json = json_value(&default_route.body);
    assert_eq!(default_json["response"]["content"], SIMULATED_CONTENT);
    assert_eq!(simulator.generation_count(), before_default + 1);
    let default_capture = generation_captures(&simulator)
        .into_iter()
        .last()
        .expect("default capture");
    assert_eq!(generation_model(&default_capture), Some(DEFAULT_MODEL));

    let b_before = key_row(&pool, b_manager.key_id)
        .await
        .expect("b manager before cross-tenant delete");
    let before_denied = simulator.generation_count();
    let cross = http_call(
        &client,
        reqwest::Method::DELETE,
        &format!("{base}/api/v1/auth/keys/{}", b_manager.key_id),
        Some(&a_manager.credential),
        None,
    )
    .await;
    assert_eq!(cross.status, 404);
    assert_eq!(error_code(&cross.body), "NOT_FOUND");
    assert_eq!(
        key_row(&pool, b_manager.key_id)
            .await
            .expect("b manager unchanged"),
        b_before
    );

    let inference_create = http_call(
        &client,
        reqwest::Method::POST,
        &format!("{base}/api/v1/auth/keys"),
        Some(&a_inference.credential),
        Some(r#"{"name":"denied-inference-create","kind":"inference"}"#),
    )
    .await;
    assert_eq!(inference_create.status, 403);
    assert_eq!(error_code(&inference_create.body), "FORBIDDEN");

    let manager_generate = http_call(
        &client,
        reqwest::Method::POST,
        &format!("{base}/api/v1/route"),
        Some(&a_manager.credential),
        Some(DEFAULT_BODY),
    )
    .await;
    assert_eq!(manager_generate.status, 403);
    assert_eq!(error_code(&manager_generate.body), "FORBIDDEN");
    assert_eq!(simulator.generation_count(), before_denied);
    assert_secret_free(
        &cross.body,
        &[
            &a_manager.credential,
            &b_manager.credential,
            &a_inference.credential,
        ],
    );

    let replacement_manager_http = http_call(
        &client,
        reqwest::Method::POST,
        &format!("{base}/api/v1/auth/keys"),
        Some(&a_manager.credential),
        Some(CREATE_REPLACEMENT_MANAGER_BODY),
    )
    .await;
    assert_eq!(replacement_manager_http.status, 201);
    let replacement_manager = parse_issued_json(&replacement_manager_http.body);
    assert_eq!(replacement_manager.client_id, a_manager.client_id);
    let replacement_list = http_call(
        &client,
        reqwest::Method::GET,
        &format!("{base}/api/v1/auth/keys"),
        Some(&replacement_manager.credential),
        None,
    )
    .await;
    assert_eq!(replacement_list.status, 200);
    assert!(inventory_ids(&replacement_list.body).contains(&a_manager.key_id));

    let revoke_old_manager = http_call(
        &client,
        reqwest::Method::DELETE,
        &format!("{base}/api/v1/auth/keys/{}", a_manager.key_id),
        Some(&replacement_manager.credential),
        None,
    )
    .await;
    assert_eq!(revoke_old_manager.status, 204);
    let old_manager_revoked_at = key_row(&pool, a_manager.key_id)
        .await
        .expect("old manager retained")
        .3
        .expect("old manager revoked_at");
    let old_manager_denied = http_call(
        &client,
        reqwest::Method::GET,
        &format!("{base}/api/v1/auth/keys"),
        Some(&a_manager.credential),
        None,
    )
    .await;
    assert_eq!(old_manager_denied.status, 401);
    assert_eq!(error_code(&old_manager_denied.body), "UNAUTHORIZED");

    let before_revoked_inference = simulator.generation_count();
    let revoke_inference = http_call(
        &client,
        reqwest::Method::DELETE,
        &format!("{base}/api/v1/auth/keys/{}", a_inference.key_id),
        Some(&replacement_manager.credential),
        None,
    )
    .await;
    assert_eq!(revoke_inference.status, 204);
    let old_inference_revoked_at = key_row(&pool, a_inference.key_id)
        .await
        .expect("old inference retained")
        .3
        .expect("old inference revoked_at");
    let revoked_generate = http_call(
        &client,
        reqwest::Method::POST,
        &format!("{base}/api/v1/route"),
        Some(&a_inference.credential),
        Some(DEFAULT_BODY),
    )
    .await;
    assert_eq!(revoked_generate.status, 401);
    assert_eq!(error_code(&revoked_generate.body), "UNAUTHORIZED");
    assert_eq!(simulator.generation_count(), before_revoked_inference);

    let replacement_inference_http = http_call(
        &client,
        reqwest::Method::POST,
        &format!("{base}/api/v1/auth/keys"),
        Some(&replacement_manager.credential),
        Some(CREATE_REPLACEMENT_INFERENCE_BODY),
    )
    .await;
    assert_eq!(replacement_inference_http.status, 201);
    let replacement_inference = parse_issued_json(&replacement_inference_http.body);
    assert_eq!(replacement_inference.client_id, a_manager.client_id);

    enqueue_fallback_pair(&simulator);
    let before_fallback = simulator.generation_count();
    let fallback = http_call(
        &client,
        reqwest::Method::POST,
        &format!("{base}/api/v1/route"),
        Some(&replacement_inference.credential),
        Some(FALLBACK_BODY),
    )
    .await;
    assert_eq!(fallback.status, 200);
    let fallback_json = json_value(&fallback.body);
    assert_eq!(fallback_json["response"]["content"], FALLBACK_CONTENT);
    assert_eq!(fallback_json["model_used"], FALLBACK_REPORTED_MODEL);
    assert_eq!(fallback_json["usage"]["prompt_tokens"], PROMPT_TOKENS);
    assert_eq!(
        fallback_json["usage"]["completion_tokens"],
        COMPLETION_TOKENS
    );
    assert_cost_eq(
        fallback_json["cost"].as_f64().expect("fallback cost"),
        SECONDARY_COST,
    );
    assert_eq!(simulator.generation_count(), before_fallback + 2);
    let fallback_models: Vec<String> = generation_captures(&simulator)
        .into_iter()
        .rev()
        .take(2)
        .rev()
        .filter_map(|capture| generation_model(&capture).map(ToOwned::to_owned))
        .collect();
    assert_eq!(
        fallback_models,
        vec![DEFAULT_MODEL.to_string(), FAST_MODEL.to_string()]
    );

    let before_outage = simulator.generation_count();
    proxy.pause();
    tokio::time::sleep(Duration::from_millis(50)).await;
    let outage = http_call(
        &client,
        reqwest::Method::POST,
        &format!("{base}/api/v1/route"),
        Some(&replacement_inference.credential),
        Some(DEFAULT_BODY),
    )
    .await;
    assert_eq!(outage.status, 503);
    assert_eq!(error_code(&outage.body), "AUTH_DEPENDENCY_UNAVAILABLE");
    assert_secret_free(&outage.body, &[&replacement_inference.credential]);
    assert_eq!(simulator.generation_count(), before_outage);

    proxy.resume();
    tokio::time::sleep(Duration::from_millis(50)).await;
    let recovered = http_call(
        &client,
        reqwest::Method::POST,
        &format!("{base}/api/v1/route"),
        Some(&replacement_inference.credential),
        Some(DEFAULT_BODY),
    )
    .await;
    assert_eq!(recovered.status, 200);
    assert_eq!(
        json_value(&recovered.body)["response"]["content"],
        SIMULATED_CONTENT
    );
    assert!(simulator.generation_count() > before_outage);

    let used_before_restart = last_used_at(&pool, replacement_inference.key_id)
        .await
        .expect("replacement inference last-used");
    drop(gateway);

    let gateway = start_gateway(&client, &proxied, simulator.base_url()).await;
    let base = format!("http://127.0.0.1:{}", gateway.port);
    let after_restart_calls = simulator.generation_count();

    let restarted_list = http_call(
        &client,
        reqwest::Method::GET,
        &format!("{base}/api/v1/auth/keys"),
        Some(&replacement_manager.credential),
        None,
    )
    .await;
    assert_eq!(restarted_list.status, 200);
    let restarted_ids = inventory_ids(&restarted_list.body);
    assert!(restarted_ids.contains(&replacement_manager.key_id));
    assert!(restarted_ids.contains(&replacement_inference.key_id));
    assert!(restarted_ids.contains(&a_manager.key_id));
    assert!(restarted_ids.contains(&a_inference.key_id));
    assert!(!restarted_ids.contains(&b_manager.key_id));
    let listed_old_manager = inventory_item(&restarted_list.body, a_manager.key_id)
        .expect("listed old manager");
    let listed_old_inference = inventory_item(&restarted_list.body, a_inference.key_id)
        .expect("listed old inference");
    assert!(listed_old_manager
        .get("revoked_at")
        .and_then(|v| v.as_str())
        .is_some());
    assert!(listed_old_inference
        .get("revoked_at")
        .and_then(|v| v.as_str())
        .is_some());
    assert_secret_free(
        &restarted_list.body,
        &[
            &a_manager.credential,
            &a_inference.credential,
            &replacement_manager.credential,
            &replacement_inference.credential,
        ],
    );

    let restarted_generate = http_call(
        &client,
        reqwest::Method::POST,
        &format!("{base}/api/v1/route"),
        Some(&replacement_inference.credential),
        Some(DEFAULT_BODY),
    )
    .await;
    assert_eq!(restarted_generate.status, 200);
    assert_eq!(
        json_value(&restarted_generate.body)["response"]["content"],
        SIMULATED_CONTENT
    );

    let old_manager_after = http_call(
        &client,
        reqwest::Method::GET,
        &format!("{base}/api/v1/auth/keys"),
        Some(&a_manager.credential),
        None,
    )
    .await;
    let old_inference_after = http_call(
        &client,
        reqwest::Method::POST,
        &format!("{base}/api/v1/route"),
        Some(&a_inference.credential),
        Some(DEFAULT_BODY),
    )
    .await;
    assert_eq!(old_manager_after.status, 401);
    assert_eq!(old_inference_after.status, 401);
    assert_eq!(simulator.generation_count(), after_restart_calls + 1);

    assert_eq!(
        key_row(&pool, a_manager.key_id)
            .await
            .expect("old manager after restart")
            .3,
        Some(old_manager_revoked_at)
    );
    assert_eq!(
        key_row(&pool, a_inference.key_id)
            .await
            .expect("old inference after restart")
            .3,
        Some(old_inference_revoked_at)
    );
    let used_after_restart = last_used_at(&pool, replacement_inference.key_id)
        .await
        .expect("replacement inference last-used after restart");
    assert!(used_after_restart >= used_before_restart);
    assert!(key_row(&pool, replacement_manager.key_id)
        .await
        .expect("replacement manager")
        .3
        .is_none());
    assert!(key_row(&pool, replacement_inference.key_id)
        .await
        .expect("replacement inference")
        .3
        .is_none());
    assert_eq!(
        key_row(&pool, b_manager.key_id)
            .await
            .expect("b manager after restart")
            .3,
        None
    );

    drop(gateway);
    cleanup_clients(&pool, &[a_manager.client_id, b_manager.client_id]).await;
}

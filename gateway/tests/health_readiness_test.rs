//! Liveness vs readiness against real Supabase and the owned simulator.
//!
//! Evidence omits credentials, digests, SQL, and raw upstream bodies.

mod support;

use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use chrono::Utc;
use gateway::{
    app::Application,
    core::{
        config::{Route, Settings, Target, TargetPricing, VendorKind},
        db::DatabasePoolConfig,
        metrics::MetricsConfig,
    },
    features::auth::{
        ApiKeyKind, AuthService, AuthStore, PostgresAuthStore, ProvisioningService,
    },
};
use serial_test::serial;
use std::str::FromStr;
use std::sync::Arc;
use std::time::{Duration, Instant};
use support::{
    auth_service_from_pool, cleanup_clients, isolate_provider_environment,
    production_router_with_auth, production_router_with_settings,
    provision_inference_key, required_database_url, rewrite_owned_database_url_port,
    test_pool, OpenAiSimulator, RecoverableDbProxy, ScriptedResponse,
};
use tokio::time::sleep;
use tower::ServiceExt;
use uuid::Uuid;

const MODEL_A: &str = "ready-model-a";
const MODEL_B: &str = "ready-model-b";

struct HealthEnv {
    prev_ttl: Option<String>,
    prev_timeout: Option<String>,
}

impl HealthEnv {
    fn apply(ttl_secs: &str, timeout_secs: &str) -> Self {
        let prev_ttl = std::env::var("HEALTH_CHECK_CACHE_TTL_SECS").ok();
        let prev_timeout = std::env::var("HEALTH_CHECK_TIMEOUT").ok();
        std::env::set_var("HEALTH_CHECK_CACHE_TTL_SECS", ttl_secs);
        std::env::set_var("HEALTH_CHECK_TIMEOUT", timeout_secs);
        Self {
            prev_ttl,
            prev_timeout,
        }
    }
}

impl Drop for HealthEnv {
    fn drop(&mut self) {
        match &self.prev_ttl {
            Some(value) => std::env::set_var("HEALTH_CHECK_CACHE_TTL_SECS", value),
            None => std::env::remove_var("HEALTH_CHECK_CACHE_TTL_SECS"),
        }
        match &self.prev_timeout {
            Some(value) => std::env::set_var("HEALTH_CHECK_TIMEOUT", value),
            None => std::env::remove_var("HEALTH_CHECK_TIMEOUT"),
        }
    }
}

async fn body_json(response: axum::http::Response<Body>) -> serde_json::Value {
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("response body");
    serde_json::from_slice(&bytes).expect("json body")
}

async fn get_path(app: axum::Router, path: &str) -> (StatusCode, serde_json::Value) {
    let response = app
        .oneshot(Request::builder().uri(path).body(Body::empty()).unwrap())
        .await
        .unwrap();
    let status = response.status();
    (status, body_json(response).await)
}

async fn post_route(
    app: axum::Router,
    credential: &str,
    body: serde_json::Value,
) -> (StatusCode, serde_json::Value) {
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/route")
                .header("content-type", "application/json")
                .header("x-api-key", credential)
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    (status, body_json(response).await)
}

fn route_body() -> serde_json::Value {
    serde_json::json!({
        "prompt": "readiness-generation",
        "metadata": {}
    })
}

fn assert_safe_ready_body(body: &serde_json::Value) {
    let encoded = body.to_string();
    assert!(!encoded.contains("opmux_private"));
    assert!(!encoded.contains("postgres"));
    assert!(!encoded.contains("127.0.0.1"));
    assert!(!encoded.contains("DATABASE_URL"));
    assert!(!encoded.contains("Bearer"));
    assert!(!encoded.contains("key_digest"));
    assert!(body.get("vendor_count").is_none());
    assert!(body.get("healthy_vendors").is_none());
    assert!(body["dependencies"].get("database").is_some());
    assert!(body["dependencies"].get("upstream").is_some());
    assert!(body["dependencies"].get("default_route").is_some());
}

fn chat_content(model: &str, content: &str) -> ScriptedResponse {
    ScriptedResponse::ChatSuccess {
        content: content.to_string(),
        model: Some(model.to_string()),
        prompt_tokens: 10,
        completion_tokens: 5,
        finish_reason: "stop".to_string(),
        role: "assistant".to_string(),
    }
}

fn models_down() -> ScriptedResponse {
    ScriptedResponse::json_status(
        500,
        serde_json::json!({"error":{"message":"models down"}}),
    )
}

fn upstream_500() -> ScriptedResponse {
    ScriptedResponse::json_status(
        500,
        serde_json::json!({"error":{"message":"simulated transient"}}),
    )
}

fn target(model: &str) -> Target {
    Target {
        vendor: VendorKind::Openai,
        model: model.to_string(),
        max_output_tokens: 512,
        pricing: TargetPricing {
            input_per_million: 1.0,
            output_per_million: 2.0,
        },
    }
}

fn fallback_settings(simulator: &OpenAiSimulator) -> Settings {
    let mut settings =
        Settings::for_tests_with_provider(simulator.base_url(), simulator.credential());
    settings
        .catalog
        .targets
        .insert("alpha".to_string(), target(MODEL_A));
    settings
        .catalog
        .targets
        .insert("beta".to_string(), target(MODEL_B));
    settings.catalog.default_route = "default".to_string();
    settings.catalog.routes.insert(
        "default".to_string(),
        Route {
            primary: "alpha".to_string(),
            fallbacks: vec!["beta".to_string()],
        },
    );
    settings.limits.retries_per_target = 0;
    settings.limits.max_total_attempts = 3;
    settings.limits.circuit_failure_threshold = 1;
    settings.limits.circuit_cooldown = Duration::from_millis(250);
    settings.limits.backoff_cap = Duration::from_millis(1);
    settings
}

async fn template1_auth() -> Arc<AuthService> {
    let url = required_database_url();
    let options = sqlx::postgres::PgConnectOptions::from_str(&url)
        .expect("owned url")
        .database("template1");
    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(1)
        .acquire_timeout(Duration::from_secs(5))
        .connect_with(options)
        .await
        .expect("template1 must exist on the owned cluster");
    Arc::new(AuthService::new(Arc::new(PostgresAuthStore::new(pool))))
}

#[tokio::test]
#[serial]
async fn healthy_gateway_is_live_and_ready_without_generation_probes() {
    isolate_provider_environment();
    let _env = HealthEnv::apply("5", "2");
    let pool = test_pool().await;
    let issued = provision_inference_key(&pool).await;
    let simulator = OpenAiSimulator::start().await;
    let app = production_router_with_auth(
        &simulator,
        auth_service_from_pool(pool.clone()),
        MetricsConfig::disabled(),
    );

    let (health_status, health_body) = get_path(app.clone(), "/health").await;
    assert_eq!(health_status, StatusCode::OK);
    assert_eq!(health_body["status"], "healthy");

    let (ready_status, ready_body) = get_path(app.clone(), "/ready").await;
    assert_eq!(ready_status, StatusCode::OK);
    assert_eq!(ready_body["status"], "ready");
    assert_eq!(ready_body["dependencies"]["database"]["status"], "healthy");
    assert_eq!(ready_body["dependencies"]["upstream"]["status"], "healthy");
    assert_eq!(
        ready_body["dependencies"]["default_route"]["status"],
        "healthy"
    );
    assert_safe_ready_body(&ready_body);
    assert_eq!(simulator.generation_count(), 0);
    assert!(simulator.models_probe_count() >= 1);

    let (gen_status, _) = post_route(app, &issued.credential, route_body()).await;
    assert_eq!(gen_status, StatusCode::OK);
    assert!(simulator.generation_count() >= 1);
    cleanup_clients(&pool, &[issued.client_id]).await;
}

#[tokio::test]
#[serial]
async fn missing_authentication_schema_cannot_report_ready() {
    isolate_provider_environment();
    let _env = HealthEnv::apply("0", "2");
    let simulator = OpenAiSimulator::start().await;
    let app = production_router_with_auth(
        &simulator,
        template1_auth().await,
        MetricsConfig::disabled(),
    );

    let (health_status, _) = get_path(app.clone(), "/health").await;
    assert_eq!(health_status, StatusCode::OK);
    let started = Instant::now();
    let (ready_status, ready_body) = get_path(app, "/ready").await;
    assert!(started.elapsed() < Duration::from_secs(3));
    assert_eq!(ready_status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(ready_body["status"], "not_ready");
    assert_eq!(
        ready_body["dependencies"]["database"]["status"],
        "unhealthy"
    );
    assert_eq!(
        ready_body["dependencies"]["database"]["error"],
        "Authentication database unavailable"
    );
    assert_safe_ready_body(&ready_body);
    assert_eq!(simulator.generation_count(), 0);
}

#[tokio::test]
#[serial]
async fn database_outage_cache_recovery_and_revoked_key_persistence() {
    isolate_provider_environment();
    let _env = HealthEnv::apply("1", "2");
    let control_pool = test_pool().await;
    let provisioning =
        ProvisioningService::new(Arc::new(PostgresAuthStore::new(control_pool.clone())));
    let tenant = format!("health-db-{}", Uuid::new_v4().simple());
    let created = provisioning.create_tenant(&tenant).await.expect("tenant");
    let active = provisioning
        .issue_key(
            created.key.client_id,
            ApiKeyKind::Inference,
            "active-inference",
        )
        .await
        .expect("active");
    let revoked = provisioning
        .issue_key(
            created.key.client_id,
            ApiKeyKind::Inference,
            "revoked-inference",
        )
        .await
        .expect("revoked");
    PostgresAuthStore::new(control_pool.clone())
        .revoke_key(created.key.client_id, revoked.key_id, Utc::now())
        .await
        .expect("revoke before outage");

    let proxy = RecoverableDbProxy::start().await;
    tokio::task::yield_now().await;
    let proxied = rewrite_owned_database_url_port(&required_database_url(), proxy.port());
    let pool = DatabasePoolConfig::new(proxied)
        .expect("proxied url")
        .with_max_connections(2)
        .expect("pool size")
        .with_acquire_timeout(Duration::from_millis(400))
        .expect("acquire timeout")
        .connect()
        .await
        .expect("connect through proxy");
    {
        let _warm = pool.acquire().await.expect("warm pool");
    }
    let simulator = OpenAiSimulator::start().await;
    let app = production_router_with_auth(
        &simulator,
        Arc::new(AuthService::new(Arc::new(PostgresAuthStore::new(pool)))),
        MetricsConfig::disabled(),
    );

    let (ready_ok, _) = get_path(app.clone(), "/ready").await;
    assert_eq!(ready_ok, StatusCode::OK);
    let (gen_ok, _) = post_route(app.clone(), active.credential(), route_body()).await;
    assert_eq!(gen_ok, StatusCode::OK);
    let generation_before_outage = simulator.generation_count();

    proxy.pause();
    tokio::task::yield_now().await;
    let cached = get_path(app.clone(), "/ready").await;
    assert_eq!(cached.0, StatusCode::OK);

    sleep(Duration::from_millis(1200)).await;
    let started = Instant::now();
    let (ready_down, ready_body) = get_path(app.clone(), "/ready").await;
    assert!(started.elapsed() < Duration::from_secs(3));
    assert_eq!(ready_down, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(ready_body["status"], "not_ready");
    assert_eq!(
        ready_body["dependencies"]["database"]["status"],
        "unhealthy"
    );
    assert_safe_ready_body(&ready_body);

    let (health_status, _) = get_path(app.clone(), "/health").await;
    assert_eq!(health_status, StatusCode::OK);

    let (outage_status, outage_body) =
        post_route(app.clone(), active.credential(), route_body()).await;
    assert_eq!(outage_status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(outage_body["error"]["code"], "AUTH_DEPENDENCY_UNAVAILABLE");
    assert_ne!(outage_status, StatusCode::UNAUTHORIZED);
    assert_eq!(simulator.generation_count(), generation_before_outage);

    proxy.resume();
    tokio::task::yield_now().await;
    let recovered_started = Instant::now();
    let (ready_up, _) = get_path(app.clone(), "/ready").await;
    assert!(recovered_started.elapsed() < Duration::from_secs(3));
    assert_eq!(ready_up, StatusCode::OK);

    let (revoked_status, _) =
        post_route(app.clone(), revoked.credential(), route_body()).await;
    assert_eq!(revoked_status, StatusCode::UNAUTHORIZED);
    let (control_status, _) = post_route(app, active.credential(), route_body()).await;
    assert_eq!(control_status, StatusCode::OK);
    assert_eq!(simulator.generation_count(), generation_before_outage + 1);

    cleanup_clients(&control_pool, &[created.key.client_id]).await;
}

#[tokio::test]
#[serial]
async fn upstream_models_outage_expires_and_recovers_without_negative_cache() {
    isolate_provider_environment();
    let _env = HealthEnv::apply("1", "2");
    let pool = test_pool().await;
    let issued = provision_inference_key(&pool).await;
    let simulator = OpenAiSimulator::start().await;
    let app = production_router_with_auth(
        &simulator,
        auth_service_from_pool(pool.clone()),
        MetricsConfig::disabled(),
    );

    let (ready_ok, _) = get_path(app.clone(), "/ready").await;
    assert_eq!(ready_ok, StatusCode::OK);
    let probes_after_success = simulator.models_probe_count();
    assert!(probes_after_success >= 1);
    assert_eq!(simulator.generation_count(), 0);

    for _ in 0..8 {
        simulator.enqueue_models(models_down());
    }
    let cached = get_path(app.clone(), "/ready").await;
    assert_eq!(cached.0, StatusCode::OK);
    assert_eq!(simulator.models_probe_count(), probes_after_success);

    sleep(Duration::from_millis(1200)).await;
    let started = Instant::now();
    let (ready_down, ready_body) = get_path(app.clone(), "/ready").await;
    assert!(started.elapsed() < Duration::from_secs(3));
    assert_eq!(ready_down, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(
        ready_body["dependencies"]["upstream"]["status"],
        "unhealthy"
    );
    assert_eq!(
        ready_body["dependencies"]["upstream"]["error"],
        "Upstream provider unreachable"
    );
    assert_eq!(ready_body["dependencies"]["database"]["status"], "healthy");
    let (health_status, _) = get_path(app.clone(), "/health").await;
    assert_eq!(health_status, StatusCode::OK);
    assert_eq!(simulator.generation_count(), 0);

    simulator.clear_models_script();
    let recovered_started = Instant::now();
    let (ready_up, _) = get_path(app, "/ready").await;
    assert!(recovered_started.elapsed() < Duration::from_secs(3));
    assert_eq!(ready_up, StatusCode::OK);
    assert_eq!(simulator.generation_count(), 0);
    cleanup_clients(&pool, &[issued.client_id]).await;
}

#[tokio::test]
#[serial]
async fn default_route_usable_targets_override_cached_models_health() {
    isolate_provider_environment();
    let _env = HealthEnv::apply("60", "2");
    let pool = test_pool().await;
    let issued = provision_inference_key(&pool).await;
    let simulator = OpenAiSimulator::start().await;
    let settings = fallback_settings(&simulator);
    let app = production_router_with_settings(
        Arc::new(settings),
        auth_service_from_pool(pool.clone()),
        MetricsConfig::disabled(),
    );

    let (ready_ok, _) = get_path(app.clone(), "/ready").await;
    assert_eq!(ready_ok, StatusCode::OK);
    let probes_after_cache = simulator.models_probe_count();
    assert!(probes_after_cache >= 1);

    simulator.enqueue_chat(upstream_500());
    simulator.enqueue_chat(chat_content(MODEL_B, "FALLBACK_OK"));
    let (fallback_status, _) =
        post_route(app.clone(), &issued.credential, route_body()).await;
    assert_eq!(fallback_status, StatusCode::OK);
    let generation_after_primary_open = simulator.generation_count();

    let (ready_fallback, ready_fallback_body) = get_path(app.clone(), "/ready").await;
    assert_eq!(ready_fallback, StatusCode::OK);
    assert_eq!(
        ready_fallback_body["dependencies"]["default_route"]["status"],
        "healthy"
    );
    assert_eq!(simulator.generation_count(), generation_after_primary_open);
    assert_eq!(simulator.models_probe_count(), probes_after_cache);

    simulator.enqueue_chat(upstream_500());
    let (both_open_status, both_open_body) =
        post_route(app.clone(), &issued.credential, route_body()).await;
    assert_eq!(both_open_status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(both_open_body["error"]["code"], "CIRCUIT_OPEN");
    let generation_both_open = simulator.generation_count();
    assert_eq!(generation_both_open, generation_after_primary_open + 1);

    let (health_status, _) = get_path(app.clone(), "/health").await;
    assert_eq!(health_status, StatusCode::OK);
    let (ready_open, ready_open_body) = get_path(app.clone(), "/ready").await;
    assert_eq!(ready_open, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(ready_open_body["status"], "not_ready");
    assert_eq!(
        ready_open_body["dependencies"]["upstream"]["status"],
        "healthy"
    );
    assert_eq!(
        ready_open_body["dependencies"]["default_route"]["status"],
        "unhealthy"
    );
    assert_eq!(
        ready_open_body["dependencies"]["default_route"]["error"],
        "No usable default-route target"
    );
    assert_safe_ready_body(&ready_open_body);
    assert_eq!(simulator.generation_count(), generation_both_open);
    assert_eq!(simulator.models_probe_count(), probes_after_cache);

    sleep(Duration::from_millis(350)).await;
    simulator.enqueue_chat(chat_content(MODEL_A, "RECOVERED"));
    let (recovered_gen, recovered_body) =
        post_route(app.clone(), &issued.credential, route_body()).await;
    assert_eq!(recovered_gen, StatusCode::OK);
    assert_eq!(recovered_body["response"]["content"], "RECOVERED");

    let (ready_recovered, _) = get_path(app, "/ready").await;
    assert_eq!(ready_recovered, StatusCode::OK);
    cleanup_clients(&pool, &[issued.client_id]).await;
}

#[tokio::test]
#[serial]
async fn production_application_wires_auth_into_readiness() {
    isolate_provider_environment();
    let _env = HealthEnv::apply("0", "2");
    let pool = test_pool().await;
    let issued = provision_inference_key(&pool).await;
    let simulator = OpenAiSimulator::start().await;
    let settings =
        Settings::for_tests_with_provider(simulator.base_url(), simulator.credential());
    let app = Application::from_settings(
        Arc::new(settings),
        auth_service_from_pool(pool.clone()),
    )
    .expect("application")
    .into_router(MetricsConfig::disabled());

    let (ready_status, ready_body) = get_path(app, "/ready").await;
    assert_eq!(ready_status, StatusCode::OK);
    assert_eq!(ready_body["dependencies"]["database"]["status"], "healthy");
    cleanup_clients(&pool, &[issued.client_id]).await;
}

fn isolated_probe_role_name() -> String {
    let mut name = String::from("opmux_p_");
    for byte in Uuid::new_v4().as_bytes() {
        name.push(char::from(b'a' + (byte >> 4)));
        name.push(char::from(b'a' + (byte & 0x0f)));
    }
    name
}

fn assert_trusted_sql_ident(value: &str) {
    assert!(
        !value.is_empty() && value.chars().all(|ch| ch.is_ascii_lowercase() || ch == '_'),
        "SQL identifier must be lowercase letters and underscore"
    );
}

async fn create_select_only_role(admin: &sqlx::PgPool, role: &str) {
    assert_trusted_sql_ident(role);
    sqlx::query(&format!("CREATE ROLE {role} NOLOGIN"))
        .execute(admin)
        .await
        .expect("create isolated NOLOGIN role");
    sqlx::query(&format!(
        "GRANT {role} TO CURRENT_USER WITH INHERIT FALSE, SET TRUE"
    ))
    .execute(admin)
    .await
    .expect("grant SET ROLE for isolated role");
    sqlx::query(&format!("GRANT USAGE ON SCHEMA opmux_private TO {role}"))
        .execute(admin)
        .await
        .expect("grant schema usage");
    sqlx::query(&format!(
        "GRANT SELECT ON TABLE opmux_private.api_keys TO {role}"
    ))
    .execute(admin)
    .await
    .expect("grant table select");
}

async fn grant_column_update(admin: &sqlx::PgPool, role: &str, column: &str) {
    assert_trusted_sql_ident(role);
    assert_trusted_sql_ident(column);
    sqlx::query(&format!(
        "GRANT UPDATE ({column}) ON TABLE opmux_private.api_keys TO {role}"
    ))
    .execute(admin)
    .await
    .expect("grant column update");
}

async fn drop_isolated_role(admin: &sqlx::PgPool, role: &str) {
    assert_trusted_sql_ident(role);
    let _ = sqlx::query(&format!(
        "REVOKE ALL ON TABLE opmux_private.api_keys FROM {role}"
    ))
    .execute(admin)
    .await;
    let _ = sqlx::query(&format!("REVOKE USAGE ON SCHEMA opmux_private FROM {role}"))
        .execute(admin)
        .await;
    let _ = sqlx::query(&format!("REVOKE {role} FROM CURRENT_USER"))
        .execute(admin)
        .await;
    sqlx::query(&format!("DROP ROLE IF EXISTS {role}"))
        .execute(admin)
        .await
        .expect("drop owned isolated role");
}

async fn table_privilege(admin: &sqlx::PgPool, role: &str, privilege: &str) -> bool {
    sqlx::query_scalar("SELECT has_table_privilege($1, $2, $3)")
        .bind(role)
        .bind("opmux_private.api_keys")
        .bind(privilege)
        .fetch_one(admin)
        .await
        .expect("runtime grant inquiry")
}

async fn key_last_used_at(
    admin: &sqlx::PgPool,
    key_id: Uuid,
) -> Option<chrono::DateTime<Utc>> {
    sqlx::query_scalar("SELECT last_used_at FROM opmux_private.api_keys WHERE id = $1")
        .bind(key_id)
        .fetch_one(admin)
        .await
        .expect("last_used_at")
}

async fn assert_unready_database(app: axum::Router) {
    let (health_status, _) = get_path(app.clone(), "/health").await;
    assert_eq!(health_status, StatusCode::OK);
    let (ready_status, ready_body) = get_path(app, "/ready").await;
    assert_eq!(ready_status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(ready_body["status"], "not_ready");
    assert_eq!(
        ready_body["dependencies"]["database"]["status"],
        "unhealthy"
    );
    assert_eq!(
        ready_body["dependencies"]["database"]["error"],
        "Authentication database unavailable"
    );
    assert_safe_ready_body(&ready_body);
}

#[tokio::test]
#[serial]
async fn select_only_runtime_role_is_unready_until_last_used_update() {
    isolate_provider_environment();
    let _env = HealthEnv::apply("0", "2");
    let admin = test_pool().await;
    let issued = provision_inference_key(&admin).await;
    let role = isolated_probe_role_name();
    create_select_only_role(&admin, &role).await;
    let runtime_pool = DatabasePoolConfig::new(required_database_url())
        .expect("owned url")
        .with_max_connections(2)
        .expect("pool size")
        .with_acquire_timeout(Duration::from_secs(10))
        .expect("acquire timeout")
        .connect_with_role(&role)
        .await
        .expect("dedicated isolated-role pool");

    let selected: Option<Uuid> =
        sqlx::query_scalar("SELECT id FROM opmux_private.api_keys WHERE id = $1")
            .bind(issued.key_id)
            .fetch_optional(&runtime_pool)
            .await
            .expect("SELECT must succeed for the isolated role");
    assert_eq!(selected, Some(issued.key_id));
    let last_used_before = key_last_used_at(&admin, issued.key_id).await;

    let simulator = OpenAiSimulator::start().await;
    let app = production_router_with_auth(
        &simulator,
        Arc::new(AuthService::new(Arc::new(PostgresAuthStore::new(
            runtime_pool.clone(),
        )))),
        MetricsConfig::disabled(),
    );

    assert_unready_database(app.clone()).await;
    let (auth_status, auth_body) =
        post_route(app.clone(), &issued.credential, route_body()).await;
    assert_eq!(auth_status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(auth_body["error"]["code"], "AUTH_DEPENDENCY_UNAVAILABLE");
    assert_eq!(simulator.generation_count(), 0);
    assert_eq!(
        key_last_used_at(&admin, issued.key_id).await,
        last_used_before
    );

    grant_column_update(&admin, &role, "name").await;
    assert_unready_database(app.clone()).await;
    assert_eq!(simulator.generation_count(), 0);
    assert_eq!(
        key_last_used_at(&admin, issued.key_id).await,
        last_used_before
    );

    grant_column_update(&admin, &role, "last_used_at").await;
    let (ready_status, ready_body) = get_path(app.clone(), "/ready").await;
    assert_eq!(ready_status, StatusCode::OK);
    assert_eq!(ready_body["status"], "ready");
    assert_eq!(ready_body["dependencies"]["database"]["status"], "healthy");
    let (health_status, _) = get_path(app, "/health").await;
    assert_eq!(health_status, StatusCode::OK);
    assert_eq!(simulator.generation_count(), 0);
    assert_eq!(
        key_last_used_at(&admin, issued.key_id).await,
        last_used_before
    );

    assert!(table_privilege(&admin, "opmux_runtime", "SELECT").await);
    assert!(table_privilege(&admin, "opmux_runtime", "UPDATE").await);

    runtime_pool.close().await;
    tokio::task::yield_now().await;
    drop_isolated_role(&admin, &role).await;
    cleanup_clients(&admin, &[issued.client_id]).await;
}

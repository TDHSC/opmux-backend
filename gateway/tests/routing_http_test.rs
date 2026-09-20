//! Configured default/named route selection through the production router.
//!
//! Evidence omits credentials, prompts, metadata, and raw provider bodies.

mod support;

use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use gateway::{
    app::Application,
    core::{config::Settings, metrics::MetricsConfig},
    features::auth::AuthService,
};
use serial_test::serial;
use std::io::Write;
use std::sync::{Arc, Mutex};
use support::{
    auth_service_from_pool, cleanup_clients, isolate_provider_environment,
    provision_inference_key, test_pool, CapturedRequest, OpenAiSimulator,
    SIMULATED_CONTENT,
};
use tower::ServiceExt;

const DEFAULT_MODEL: &str = "example-chat-model";
const FAST_MODEL: &str = "example-chat-model-mini";
const META_SENTINEL: &str = "OPAQUE_META_SENTINEL_7c2e";
const INJECTED_URL: &str = "https://injected.example/v1";
const INJECTED_MODEL: &str = "unconfigured-injected-model";
const MOCK_HISTORY: &str = "Hello, how are you?";

async fn body_string(response: axum::http::Response<Body>) -> String {
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("response body");
    String::from_utf8(bytes.to_vec()).expect("utf8 body")
}

fn generation_body(capture: &CapturedRequest) -> &serde_json::Value {
    capture
        .body
        .as_ref()
        .expect("generation capture must include JSON")
}

fn generation_model(capture: &CapturedRequest) -> Option<&str> {
    generation_body(capture)
        .get("model")
        .and_then(|value| value.as_str())
}

fn user_contents(capture: &CapturedRequest) -> Vec<&str> {
    generation_body(capture)
        .get("messages")
        .and_then(|value| value.as_array())
        .map(|messages| {
            messages
                .iter()
                .filter(|message| {
                    message.get("role").and_then(|role| role.as_str()) == Some("user")
                })
                .filter_map(|message| {
                    message.get("content").and_then(|content| content.as_str())
                })
                .collect()
        })
        .unwrap_or_default()
}

fn opaque_metadata() -> serde_json::Value {
    serde_json::json!({
        "conversation_id": "conv-shared-stateless",
        "route": "fast",
        "model": INJECTED_MODEL,
        "vendor": "anthropic",
        "url": INJECTED_URL,
        "stream": true,
        "client_id": "11111111-1111-1111-1111-111111111111",
        "tenant_id": "injected-tenant",
        META_SENTINEL: true
    })
}

fn routing_router(
    simulator: &OpenAiSimulator,
    auth_service: Arc<AuthService>,
) -> axum::Router {
    let mut settings =
        Settings::for_tests_with_provider(simulator.base_url(), simulator.credential());
    settings
        .catalog
        .routes
        .get_mut("default")
        .expect("default route")
        .fallbacks = vec!["secondary".to_string()];
    Application::from_settings(Arc::new(settings), auth_service)
        .expect("application should build from routing fixture settings")
        .into_router(MetricsConfig::disabled())
}

async fn post_route(
    app: axum::Router,
    credential: &str,
    body: serde_json::Value,
) -> axum::http::Response<Body> {
    app.oneshot(
        Request::builder()
            .method("POST")
            .uri("/api/v1/route")
            .header("content-type", "application/json")
            .header("x-api-key", credential)
            .body(Body::from(body.to_string()))
            .unwrap(),
    )
    .await
    .unwrap()
}

#[tokio::test]
#[serial]
async fn default_and_named_routes_select_configured_primary_models() {
    isolate_provider_environment();
    let pool = test_pool().await;
    let issued = provision_inference_key(&pool).await;
    let simulator = OpenAiSimulator::start().await;
    let app = routing_router(&simulator, auth_service_from_pool(pool.clone()));

    let default_response = post_route(
        app.clone(),
        &issued.credential,
        serde_json::json!({
            "prompt": "select default route",
            "metadata": {}
        }),
    )
    .await;
    assert_eq!(default_response.status(), StatusCode::OK);
    let default_body = body_string(default_response).await;
    assert!(default_body.contains(SIMULATED_CONTENT));
    assert!(default_body.contains(&format!("\"model_used\":\"{DEFAULT_MODEL}\"")));

    let named_response = post_route(
        app,
        &issued.credential,
        serde_json::json!({
            "prompt": "select named route",
            "metadata": {},
            "route": "fast"
        }),
    )
    .await;
    assert_eq!(named_response.status(), StatusCode::OK);
    let named_body = body_string(named_response).await;
    assert!(named_body.contains(&format!("\"model_used\":\"{FAST_MODEL}\"")));

    let captures: Vec<_> = simulator
        .captured()
        .into_iter()
        .filter(|capture| capture.is_generation())
        .collect();
    assert_eq!(captures.len(), 2);
    assert_eq!(generation_model(&captures[0]), Some(DEFAULT_MODEL));
    assert_eq!(generation_model(&captures[1]), Some(FAST_MODEL));
    cleanup_clients(&pool, &[issued.client_id]).await;
}

#[tokio::test]
#[serial]
async fn allow_fallback_choices_keep_successful_primary() {
    isolate_provider_environment();
    let pool = test_pool().await;
    let issued = provision_inference_key(&pool).await;
    let simulator = OpenAiSimulator::start().await;
    let app = routing_router(&simulator, auth_service_from_pool(pool.clone()));

    for allow_fallback in [None, Some(true), Some(false)] {
        let mut body = serde_json::json!({
            "prompt": "successful primary",
            "metadata": {}
        });
        if let Some(flag) = allow_fallback {
            body["allow_fallback"] = serde_json::json!(flag);
        }
        let response = post_route(app.clone(), &issued.credential, body).await;
        assert_eq!(response.status(), StatusCode::OK);
        let response_body = body_string(response).await;
        assert!(response_body.contains(&format!("\"model_used\":\"{DEFAULT_MODEL}\"")));
    }

    let captures: Vec<_> = simulator
        .captured()
        .into_iter()
        .filter(|capture| capture.is_generation())
        .collect();
    assert_eq!(captures.len(), 3);
    for capture in &captures {
        assert_eq!(generation_model(capture), Some(DEFAULT_MODEL));
    }
    cleanup_clients(&pool, &[issued.client_id]).await;
}

#[tokio::test]
#[serial]
async fn unknown_route_fails_before_provider_access() {
    isolate_provider_environment();
    let pool = test_pool().await;
    let issued = provision_inference_key(&pool).await;
    let simulator = OpenAiSimulator::start().await;
    let app = routing_router(&simulator, auth_service_from_pool(pool.clone()));

    let response = post_route(
        app,
        &issued.credential,
        serde_json::json!({
            "prompt": "unknown route must not execute",
            "metadata": {},
            "route": "not-a-configured-route"
        }),
    )
    .await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    assert_eq!(simulator.generation_count(), 0);
    cleanup_clients(&pool, &[issued.client_id]).await;
}

#[tokio::test]
#[serial]
async fn metadata_cannot_choose_unconfigured_model_vendor_or_url() {
    isolate_provider_environment();
    let pool = test_pool().await;
    let issued = provision_inference_key(&pool).await;
    let simulator = OpenAiSimulator::start().await;
    let app = routing_router(&simulator, auth_service_from_pool(pool.clone()));

    let response = post_route(
        app,
        &issued.credential,
        serde_json::json!({
            "prompt": "metadata cannot inject a target",
            "metadata": opaque_metadata()
        }),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);

    let captures: Vec<_> = simulator
        .captured()
        .into_iter()
        .filter(|capture| capture.is_generation())
        .collect();
    assert_eq!(captures.len(), 1);
    let body = generation_body(&captures[0]);
    assert_eq!(generation_model(&captures[0]), Some(DEFAULT_MODEL));
    let serialized = body.to_string();
    assert!(!serialized.contains(INJECTED_MODEL));
    assert!(!serialized.contains(INJECTED_URL));
    assert!(!serialized.contains("anthropic"));
    assert!(!serialized.contains(META_SENTINEL));
    assert!(body.get("metadata").is_none());
    cleanup_clients(&pool, &[issued.client_id]).await;
}

#[tokio::test]
#[serial]
async fn independent_prompts_stay_stateless_and_metadata_stays_opaque() {
    isolate_provider_environment();
    let pool = test_pool().await;
    let issued = provision_inference_key(&pool).await;
    let simulator = OpenAiSimulator::start().await;
    let (log_buf, _guard) = install_debug_capture();
    let app = routing_router(&simulator, auth_service_from_pool(pool.clone()));

    let first_prompt = "stateless prompt one";
    let second_prompt = "stateless prompt two";
    let metadata = opaque_metadata();

    let first = post_route(
        app.clone(),
        &issued.credential,
        serde_json::json!({
            "prompt": first_prompt,
            "metadata": metadata
        }),
    )
    .await;
    assert_eq!(first.status(), StatusCode::OK);

    let second = post_route(
        app,
        &issued.credential,
        serde_json::json!({
            "prompt": second_prompt,
            "metadata": metadata
        }),
    )
    .await;
    assert_eq!(second.status(), StatusCode::OK);

    let captures: Vec<_> = simulator
        .captured()
        .into_iter()
        .filter(|capture| capture.is_generation())
        .collect();
    assert_eq!(captures.len(), 2);

    let first_users = user_contents(&captures[0]);
    let second_users = user_contents(&captures[1]);
    let first_only_own = first_users == [first_prompt];
    let second_only_own = second_users == [second_prompt];
    assert!(
        first_only_own,
        "first provider request must contain only its original user prompt"
    );
    assert!(
        second_only_own,
        "second provider request must contain only its original user prompt"
    );
    assert!(!first_users.contains(&second_prompt));
    assert!(!second_users.contains(&first_prompt));

    for capture in &captures {
        let body = generation_body(capture);
        let serialized = body.to_string();
        assert_eq!(
            body.get("messages")
                .and_then(|value| value.as_array())
                .map(|items| items.len()),
            Some(1)
        );
        assert!(!serialized.contains(MOCK_HISTORY));
        assert!(!serialized.contains(META_SENTINEL));
        assert!(!serialized.contains(INJECTED_URL));
        assert!(body.get("metadata").is_none());
        assert_eq!(generation_model(capture), Some(DEFAULT_MODEL));
    }

    let logs = String::from_utf8_lossy(&log_buf.lock().expect("log lock")).into_owned();
    assert!(
        !logs.is_empty(),
        "ingress debug capture must be nonempty for absence checks"
    );
    assert!(!logs.contains(META_SENTINEL));
    assert!(!logs.contains(INJECTED_URL));
    assert!(!logs.contains(INJECTED_MODEL));
    assert!(!logs.contains("injected-tenant"));
    assert!(!logs.contains(first_prompt));
    assert!(!logs.contains(second_prompt));

    let sentinel_in_key_row: bool = sqlx::query_scalar(
        "SELECT EXISTS (
            SELECT 1 FROM opmux_private.api_keys
            WHERE client_id = $1 AND (
                name LIKE '%' || $2 || '%'
                OR display_id LIKE '%' || $2 || '%'
            )
        )",
    )
    .bind(issued.client_id)
    .bind(META_SENTINEL)
    .fetch_one(&pool)
    .await
    .expect("metadata persistence query");
    assert!(
        !sentinel_in_key_row,
        "opaque metadata must not be persisted on the authenticated key"
    );

    cleanup_clients(&pool, &[issued.client_id]).await;
}

#[test]
fn production_ingress_has_no_memory_or_router_operations() {
    let service = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/src/features/ingress/service.rs"
    ));
    let repository = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/src/features/ingress/repository.rs"
    ));
    let module = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/src/features/ingress/mod.rs"
    ));
    let mockdata_path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/src/features/ingress/mockdata.rs"
    );

    assert!(!service.contains("get_context"));
    assert!(!service.contains("update_context"));
    assert!(!service.contains("optimize_route"));
    assert!(!service.contains("Memory Service"));
    assert!(!service.contains("Router Service"));
    assert!(!repository.contains("context_cache"));
    assert!(!repository.contains("MockDataProvider"));
    assert!(!repository.contains("get_mock_context"));
    assert!(!module.contains("mockdata"));
    assert!(
        !std::path::Path::new(mockdata_path).exists(),
        "production ingress must not ship a Memory/Router mockdata module"
    );
}

struct CaptureWriter(Arc<Mutex<Vec<u8>>>);

impl Write for CaptureWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().expect("capture lock").extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn install_debug_capture() -> (Arc<Mutex<Vec<u8>>>, tracing::subscriber::DefaultGuard) {
    let buf = Arc::new(Mutex::new(Vec::new()));
    let writer = buf.clone();
    let subscriber = tracing_subscriber::fmt()
        .with_ansi(false)
        .with_env_filter(tracing_subscriber::EnvFilter::new("gateway=debug"))
        .with_writer(move || CaptureWriter(writer.clone()))
        .finish();
    let guard = tracing::subscriber::set_default(subscriber);
    (buf, guard)
}

//! Production-router HTTP tests for management/inference capabilities,
//! same-tenant key creation, and bounded tenant inventory.
//!
//! Capture issued credentials privately. Do not print secrets, digests, or
//! bodies that contain them.

mod support;

use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use chrono::{Duration, Utc};
use gateway::{
    core::metrics::MetricsConfig,
    features::auth::{
        hash_credential, parse_credential_payload, ApiKeyKind, AuthService, AuthStore,
        KeyDigest, NewApiKey, PostgresAuthStore, ProvisioningService, CREDENTIAL_PREFIX,
        KEY_DIGEST_LEN, MAX_KEY_LIST_LIMIT, SECRET_PAYLOAD_LEN,
    },
};
use serial_test::serial;
use std::collections::HashSet;
use std::io::Write;
use std::sync::{Arc, Mutex};
use support::{
    cleanup_clients, isolate_provider_environment, production_router_with_auth,
    test_pool, OpenAiSimulator, SIMULATED_CONTENT,
};
use tower::ServiceExt;
use uuid::Uuid;

const ROUTE_BODY: &str = r#"{"prompt":"capability generation","metadata":{}}"#;

struct Issued {
    client_id: Uuid,
    key_id: Uuid,
    credential: String,
}

struct TwoTenantFixture {
    pool: sqlx::PgPool,
    service: ProvisioningService,
    a_management: Issued,
    a_inference: Issued,
    b_management: Issued,
    b_inference: Issued,
}

impl TwoTenantFixture {
    async fn provision() -> Self {
        let pool = test_pool().await;
        let service =
            ProvisioningService::new(Arc::new(PostgresAuthStore::new(pool.clone())));
        let created_a = service
            .create_tenant(&format!("keys-a-{}", Uuid::new_v4().simple()))
            .await
            .expect("tenant a");
        let created_b = service
            .create_tenant(&format!("keys-b-{}", Uuid::new_v4().simple()))
            .await
            .expect("tenant b");
        let a_inference = service
            .issue_key(
                created_a.key.client_id,
                ApiKeyKind::Inference,
                "a-inference",
            )
            .await
            .expect("a inference");
        let b_inference = service
            .issue_key(
                created_b.key.client_id,
                ApiKeyKind::Inference,
                "b-inference",
            )
            .await
            .expect("b inference");
        Self {
            pool,
            service,
            a_management: Issued {
                client_id: created_a.key.client_id,
                key_id: created_a.key.key_id,
                credential: created_a.key.credential().to_string(),
            },
            a_inference: Issued {
                client_id: a_inference.client_id,
                key_id: a_inference.key_id,
                credential: a_inference.credential().to_string(),
            },
            b_management: Issued {
                client_id: created_b.key.client_id,
                key_id: created_b.key.key_id,
                credential: created_b.key.credential().to_string(),
            },
            b_inference: Issued {
                client_id: b_inference.client_id,
                key_id: b_inference.key_id,
                credential: b_inference.credential().to_string(),
            },
        }
    }

    fn client_ids(&self) -> [Uuid; 2] {
        [self.a_management.client_id, self.b_management.client_id]
    }

    fn auth_service(&self) -> Arc<AuthService> {
        Arc::new(AuthService::new(Arc::new(PostgresAuthStore::new(
            self.pool.clone(),
        ))))
    }

    async fn drop_rows(&self) {
        cleanup_clients(&self.pool, &self.client_ids()).await;
    }
}

async fn body_string(response: axum::http::Response<Body>) -> String {
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("response body");
    String::from_utf8(bytes.to_vec()).expect("utf8 body")
}

async fn key_count(pool: &sqlx::PgPool, client_id: Uuid) -> i64 {
    sqlx::query_scalar("SELECT count(*) FROM opmux_private.api_keys WHERE client_id = $1")
        .bind(client_id)
        .fetch_one(pool)
        .await
        .expect("key count")
}

async fn persisted_matches(
    pool: &sqlx::PgPool,
    key_id: Uuid,
    client_id: Uuid,
    name: &str,
    kind: &str,
    credential: &str,
) -> bool {
    let row: Option<(Uuid, String, String, Vec<u8>, String)> = sqlx::query_as(
        "SELECT client_id, name, kind, key_digest, display_id
         FROM opmux_private.api_keys WHERE id = $1",
    )
    .bind(key_id)
    .fetch_optional(pool)
    .await
    .expect("persisted key");
    let Some((stored_client, stored_name, stored_kind, digest, display_id)) = row else {
        return false;
    };
    let expected = hash_credential(credential);
    stored_client == client_id
        && stored_name == name
        && stored_kind == kind
        && digest.as_slice() == expected.as_bytes().as_slice()
        && !display_id.contains(credential)
        && !display_id.is_empty()
}

fn route_request(api_key: Option<&str>) -> Request<Body> {
    let mut builder = Request::builder()
        .method("POST")
        .uri("/api/v1/route")
        .header("content-type", "application/json");
    if let Some(key) = api_key {
        builder = builder.header("x-api-key", key);
    }
    builder.body(Body::from(ROUTE_BODY)).unwrap()
}

fn keys_request(
    method: &str,
    api_key: Option<&str>,
    body: Option<String>,
) -> Request<Body> {
    keys_request_uri(method, "/api/v1/auth/keys", api_key, body)
}

fn keys_request_uri(
    method: &str,
    uri: &str,
    api_key: Option<&str>,
    body: Option<String>,
) -> Request<Body> {
    let mut builder = Request::builder()
        .method(method)
        .uri(uri)
        .header("content-type", "application/json");
    if let Some(key) = api_key {
        builder = builder.header("x-api-key", key);
    }
    builder.body(Body::from(body.unwrap_or_default())).unwrap()
}

fn keys_get(api_key: Option<&str>, query: &str) -> Request<Body> {
    let uri = if query.is_empty() {
        "/api/v1/auth/keys".to_string()
    } else {
        format!("/api/v1/auth/keys?{query}")
    };
    keys_request_uri("GET", &uri, api_key, None)
}

fn unique_digest() -> KeyDigest {
    let mut bytes = [0_u8; KEY_DIGEST_LEN];
    let id = Uuid::new_v4();
    bytes[..16].copy_from_slice(id.as_bytes());
    bytes[16..].copy_from_slice(Uuid::new_v4().as_bytes());
    KeyDigest::from_bytes(bytes)
}

async fn insert_named_key(
    store: &PostgresAuthStore,
    client_id: Uuid,
    name: &str,
    kind: ApiKeyKind,
    created_at: chrono::DateTime<Utc>,
) -> (Uuid, KeyDigest) {
    let digest = unique_digest();
    let record = store
        .insert_key(NewApiKey {
            id: Uuid::new_v4(),
            client_id,
            digest,
            display_id: format!("opk_{}", Uuid::new_v4().simple()),
            name: name.to_string(),
            kind,
            created_at,
        })
        .await
        .expect("insert named key");
    (record.id, digest)
}

fn parse_inventory(body: &str) -> (Vec<serde_json::Value>, bool) {
    let json: serde_json::Value = serde_json::from_str(body).expect("inventory json");
    let keys = json
        .get("keys")
        .and_then(|value| value.as_array())
        .cloned()
        .unwrap_or_default();
    let has_more = json
        .get("has_more")
        .and_then(|value| value.as_bool())
        .expect("has_more");
    (keys, has_more)
}

fn listed_ids(keys: &[serde_json::Value]) -> HashSet<Uuid> {
    keys.iter()
        .filter_map(|item| item.get("key_id").and_then(|value| value.as_str()))
        .filter_map(|value| Uuid::parse_str(value).ok())
        .collect()
}

fn assert_safe_inventory_item(item: &serde_json::Value) {
    let object = item.as_object().expect("key object");
    assert!(object.contains_key("client_id"));
    assert!(object.contains_key("key_id"));
    assert!(object.contains_key("display_id"));
    assert!(object.contains_key("name"));
    assert!(object.contains_key("kind"));
    assert!(object.contains_key("created_at"));
    assert!(object.contains_key("last_used_at"));
    assert!(object.contains_key("revoked_at"));
    assert!(!object.contains_key("credential"));
    assert!(!object.contains_key("digest"));
    assert!(!object.contains_key("key_digest"));
    let kind = object
        .get("kind")
        .and_then(|value| value.as_str())
        .unwrap_or("");
    assert!(kind == "management" || kind == "inference");
    let display_id = object
        .get("display_id")
        .and_then(|value| value.as_str())
        .unwrap_or("");
    assert!(display_id.starts_with("opk_"));
}

fn digest_hex(digest: &[u8; 32]) -> String {
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn omits_secret_and_digest(haystack: &str, credential: &str, digest: &[u8; 32]) -> bool {
    let hex = digest_hex(digest);
    let hex_upper = hex.to_uppercase();
    let debug_bytes = format!("{digest:?}");
    !haystack.contains(credential)
        && !haystack.contains(&hex)
        && !haystack.contains(&hex_upper)
        && !haystack.contains(&debug_bytes)
        && !haystack.contains("key_digest")
}

struct DebugCapture {
    buf: Arc<Mutex<Vec<u8>>>,
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

impl DebugCapture {
    fn install() -> (Self, tracing::subscriber::DefaultGuard) {
        let buf = Arc::new(Mutex::new(Vec::new()));
        let writer = buf.clone();
        let subscriber = tracing_subscriber::fmt()
            .with_ansi(false)
            .with_env_filter(tracing_subscriber::EnvFilter::new("gateway=debug"))
            .with_writer(move || CaptureWriter(writer.clone()))
            .finish();
        let guard = tracing::subscriber::set_default(subscriber);
        (Self { buf }, guard)
    }

    fn take(&self) -> String {
        let mut buf = self.buf.lock().expect("capture lock");
        let text = String::from_utf8(buf.clone()).unwrap_or_default();
        buf.clear();
        text
    }
}

#[tokio::test]
#[serial]
async fn management_and_inference_capabilities_are_separate_for_both_tenants() {
    isolate_provider_environment();
    let fixture = TwoTenantFixture::provision().await;
    let simulator = OpenAiSimulator::start().await;
    let app = production_router_with_auth(
        &simulator,
        fixture.auth_service(),
        MetricsConfig::disabled(),
    );

    let tenants = [
        ("a", &fixture.a_management, &fixture.a_inference),
        ("b", &fixture.b_management, &fixture.b_inference),
    ];
    let mut created_ids = Vec::new();

    for (label, manager, inference) in tenants {
        let before_count = key_count(&fixture.pool, manager.client_id).await;
        let create_name = format!("{label}-created-inference");
        let create = app
            .clone()
            .oneshot(keys_request(
                "POST",
                Some(&manager.credential),
                Some(format!(r#"{{"name":"{create_name}","kind":"inference"}}"#)),
            ))
            .await
            .unwrap();
        assert_eq!(
            create.status(),
            StatusCode::CREATED,
            "{label} manager create must return 201"
        );
        let created_body = body_string(create).await;
        let created: serde_json::Value =
            serde_json::from_str(&created_body).expect("created json");
        let created_id = created
            .get("key_id")
            .and_then(|value| value.as_str())
            .and_then(|value| Uuid::parse_str(value).ok())
            .expect("created key_id");
        created_ids.push(created_id);
        assert_eq!(
            key_count(&fixture.pool, manager.client_id).await,
            before_count + 1
        );

        let list = app
            .clone()
            .oneshot(keys_request("GET", Some(&manager.credential), None))
            .await
            .unwrap();
        assert_eq!(
            list.status(),
            StatusCode::OK,
            "{label} manager list must return 200"
        );
        let list_body = body_string(list).await;
        assert!(list_body.contains(&created_id.to_string()));
        assert!(!list_body.contains("credential"));

        let before_generate = simulator.generation_count();
        let manager_generate = app
            .clone()
            .oneshot(route_request(Some(&manager.credential)))
            .await
            .unwrap();
        assert_eq!(
            manager_generate.status(),
            StatusCode::FORBIDDEN,
            "{label} manager generation must return 403"
        );
        assert_eq!(
            simulator.generation_count(),
            before_generate,
            "{label} manager generation must not reach upstream"
        );

        let inference_generate = app
            .clone()
            .oneshot(route_request(Some(&inference.credential)))
            .await
            .unwrap();
        assert_eq!(
            inference_generate.status(),
            StatusCode::OK,
            "{label} inference generation must return 200"
        );
        let generate_body = body_string(inference_generate).await;
        assert!(generate_body.contains(SIMULATED_CONTENT));

        let denied_count = key_count(&fixture.pool, inference.client_id).await;
        let inference_create = app
            .clone()
            .oneshot(keys_request(
                "POST",
                Some(&inference.credential),
                Some(r#"{"name":"should-not-create","kind":"inference"}"#.to_string()),
            ))
            .await
            .unwrap();
        assert_eq!(
            inference_create.status(),
            StatusCode::FORBIDDEN,
            "{label} inference create must return 403"
        );
        let _ = body_string(inference_create).await;
        assert_eq!(
            key_count(&fixture.pool, inference.client_id).await,
            denied_count,
            "{label} denied create must not mutate inventory"
        );

        let inference_list = app
            .clone()
            .oneshot(keys_request("GET", Some(&inference.credential), None))
            .await
            .unwrap();
        assert_eq!(
            inference_list.status(),
            StatusCode::FORBIDDEN,
            "{label} inference list must return 403"
        );
        let _ = body_string(inference_list).await;
    }

    let missing_post = app
        .clone()
        .oneshot(keys_request(
            "POST",
            None,
            Some(r#"{"name":"missing","kind":"inference"}"#.to_string()),
        ))
        .await
        .unwrap()
        .status();
    let unknown_post = app
        .clone()
        .oneshot(keys_request(
            "POST",
            Some("opmx_v1_unknown-management-key"),
            Some(r#"{"name":"unknown","kind":"inference"}"#.to_string()),
        ))
        .await
        .unwrap()
        .status();
    let missing_get = app
        .clone()
        .oneshot(keys_request("GET", None, None))
        .await
        .unwrap()
        .status();
    let unknown_get = app
        .clone()
        .oneshot(keys_request(
            "GET",
            Some("opmx_v1_unknown-management-key"),
            None,
        ))
        .await
        .unwrap()
        .status();
    for (label, status) in [
        ("missing POST", missing_post),
        ("unknown POST", unknown_post),
        ("missing GET", missing_get),
        ("unknown GET", unknown_get),
    ] {
        assert_eq!(
            status,
            StatusCode::UNAUTHORIZED,
            "{label} must return 401 on key management routes"
        );
    }

    assert_eq!(created_ids.len(), 2);
    fixture.drop_rows().await;
}

#[tokio::test]
#[serial]
async fn same_tenant_http_issuance_returns_usable_secret_once() {
    isolate_provider_environment();
    let fixture = TwoTenantFixture::provision().await;
    let simulator = OpenAiSimulator::start().await;
    let (capture, _guard) = DebugCapture::install();
    let app = production_router_with_auth(
        &simulator,
        fixture.auth_service(),
        MetricsConfig::disabled(),
    );

    let cases = [
        ("a-http-manager", "management"),
        ("a-http-inference", "inference"),
    ];
    let mut issued = Vec::new();

    for (name, kind) in cases {
        let response = app
            .clone()
            .oneshot(keys_request(
                "POST",
                Some(&fixture.a_management.credential),
                Some(format!(r#"{{"name":"{name}","kind":"{kind}"}}"#)),
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::CREATED);
        let cache_control = response
            .headers()
            .get("cache-control")
            .and_then(|value| value.to_str().ok())
            .unwrap_or("");
        assert_eq!(cache_control, "no-store");
        let body = body_string(response).await;
        let logs = capture.take();
        let value: serde_json::Value = serde_json::from_str(&body).expect("created json");
        let credential = value
            .get("credential")
            .and_then(|item| item.as_str())
            .unwrap_or("")
            .to_string();
        let key_id = value
            .get("key_id")
            .and_then(|item| item.as_str())
            .and_then(|item| Uuid::parse_str(item).ok())
            .expect("key_id");
        let client_id = value
            .get("client_id")
            .and_then(|item| item.as_str())
            .and_then(|item| Uuid::parse_str(item).ok())
            .expect("client_id");
        let payload = parse_credential_payload(&credential).expect("payload");
        assert!(credential.starts_with(CREDENTIAL_PREFIX));
        assert_eq!(payload.len(), SECRET_PAYLOAD_LEN);
        assert_eq!(client_id, fixture.a_management.client_id);
        assert_eq!(value.get("name").and_then(|item| item.as_str()), Some(name));
        assert_eq!(value.get("kind").and_then(|item| item.as_str()), Some(kind));
        assert!(value.get("digest").is_none());
        assert!(value.get("key_digest").is_none());
        assert!(
            persisted_matches(
                &fixture.pool,
                key_id,
                fixture.a_management.client_id,
                name,
                kind,
                &credential
            )
            .await
        );
        let digest = hash_credential(&credential);
        assert!(omits_secret_and_digest(
            &logs,
            &credential,
            digest.as_bytes()
        ));
        issued.push((kind, key_id, credential));
    }

    let new_manager_secret = issued
        .iter()
        .find(|(kind, _, _)| *kind == "management")
        .map(|(_, _, secret)| secret.clone())
        .expect("new manager secret");
    let new_inference_secret = issued
        .iter()
        .find(|(kind, _, _)| *kind == "inference")
        .map(|(_, _, secret)| secret.clone())
        .expect("new inference secret");
    let new_manager_id = issued
        .iter()
        .find(|(kind, _, _)| *kind == "management")
        .map(|(_, id, _)| *id)
        .expect("new manager id");
    let new_inference_id = issued
        .iter()
        .find(|(kind, _, _)| *kind == "inference")
        .map(|(_, id, _)| *id)
        .expect("new inference id");

    let list = app
        .clone()
        .oneshot(keys_request("GET", Some(&new_manager_secret), None))
        .await
        .unwrap();
    assert_eq!(list.status(), StatusCode::OK);
    let list_body = body_string(list).await;
    let list_json: serde_json::Value =
        serde_json::from_str(&list_body).expect("list json");
    let keys = list_json
        .get("keys")
        .and_then(|value| value.as_array())
        .cloned()
        .unwrap_or_default();
    let listed_ids: HashSet<Uuid> = keys
        .iter()
        .filter_map(|item| item.get("key_id").and_then(|value| value.as_str()))
        .filter_map(|value| Uuid::parse_str(value).ok())
        .collect();
    assert!(listed_ids.contains(&fixture.a_management.key_id));
    assert!(listed_ids.contains(&fixture.a_inference.key_id));
    assert!(listed_ids.contains(&new_manager_id));
    assert!(listed_ids.contains(&new_inference_id));
    assert!(!listed_ids.contains(&fixture.b_management.key_id));
    assert!(!listed_ids.contains(&fixture.b_inference.key_id));
    assert!(!list_body.contains(&new_manager_secret));
    assert!(!list_body.contains(&new_inference_secret));
    assert!(!list_body.contains("credential"));
    assert!(!list_body.contains("digest"));
    for item in &keys {
        assert!(item.get("digest").is_none());
        assert!(item.get("credential").is_none());
        assert!(item.get("key_digest").is_none());
    }

    let generate = app
        .clone()
        .oneshot(route_request(Some(&new_inference_secret)))
        .await
        .unwrap();
    assert_eq!(generate.status(), StatusCode::OK);
    let generate_body = body_string(generate).await;
    assert!(generate_body.contains(SIMULATED_CONTENT));
    assert!(!generate_body.contains(&new_inference_secret));

    let identity = fixture
        .service
        .resolve_credential(&new_inference_secret)
        .await
        .expect("resolve")
        .expect("identity");
    assert_eq!(identity.client_id, fixture.a_management.client_id);
    assert_eq!(identity.key_id, new_inference_id);
    assert_eq!(identity.kind, ApiKeyKind::Inference);

    fixture.drop_rows().await;
}

#[tokio::test]
#[serial]
async fn ownership_injection_and_invalid_kinds_cannot_redirect_creation_or_listing() {
    isolate_provider_environment();
    let fixture = TwoTenantFixture::provision().await;
    let simulator = OpenAiSimulator::start().await;
    let app = production_router_with_auth(
        &simulator,
        fixture.auth_service(),
        MetricsConfig::disabled(),
    );
    let before_a = key_count(&fixture.pool, fixture.a_management.client_id).await;
    let before_b = key_count(&fixture.pool, fixture.b_management.client_id).await;

    let payloads = [
        format!(
            r#"{{"name":"stolen","kind":"inference","client_id":"{}"}}"#,
            fixture.b_management.client_id
        ),
        format!(
            r#"{{"name":"stolen","kind":"inference","tenant_id":"{}"}}"#,
            fixture.b_management.client_id
        ),
        r#"{"name":"admin-kind","kind":"admin"}"#.to_string(),
    ];
    for payload in payloads {
        let response = app
            .clone()
            .oneshot(keys_request(
                "POST",
                Some(&fixture.a_management.credential),
                Some(payload),
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let _ = body_string(response).await;
    }

    let inference_claim = app
        .clone()
        .oneshot(keys_request(
            "POST",
            Some(&fixture.a_inference.credential),
            Some(r#"{"name":"escalate","kind":"management"}"#.to_string()),
        ))
        .await
        .unwrap();
    assert_eq!(inference_claim.status(), StatusCode::FORBIDDEN);
    let _ = body_string(inference_claim).await;

    let list_selectors = [
        format!("client_id={}", fixture.b_management.client_id),
        format!("tenant_id={}", fixture.b_management.client_id),
        "kind=admin".to_string(),
        "limit=0".to_string(),
        "limit=-1".to_string(),
        format!("limit={}", MAX_KEY_LIST_LIMIT + 1),
        "limit=abc".to_string(),
        "offset=-1".to_string(),
        "offset=nope".to_string(),
        "unknown=1".to_string(),
        "limit=1&limit=2".to_string(),
    ];
    for query in list_selectors {
        let response = app
            .clone()
            .oneshot(keys_get(Some(&fixture.a_management.credential), &query))
            .await
            .unwrap();
        assert_eq!(
            response.status(),
            StatusCode::BAD_REQUEST,
            "list query {query} must fail"
        );
        let _ = body_string(response).await;
    }

    let inference_list_inject = app
        .clone()
        .oneshot(keys_get(
            Some(&fixture.a_inference.credential),
            &format!("client_id={}", fixture.b_management.client_id),
        ))
        .await
        .unwrap();
    assert_eq!(inference_list_inject.status(), StatusCode::FORBIDDEN);
    let _ = body_string(inference_list_inject).await;

    assert_eq!(
        key_count(&fixture.pool, fixture.a_management.client_id).await,
        before_a
    );
    assert_eq!(
        key_count(&fixture.pool, fixture.b_management.client_id).await,
        before_b
    );
    fixture.drop_rows().await;
}

#[tokio::test]
#[serial]
async fn inventory_is_bounded_tenant_scoped_and_secret_free() {
    isolate_provider_environment();
    let fixture = TwoTenantFixture::provision().await;
    let simulator = OpenAiSimulator::start().await;
    let app = production_router_with_auth(
        &simulator,
        fixture.auth_service(),
        MetricsConfig::disabled(),
    );
    let store = PostgresAuthStore::new(fixture.pool.clone());

    let empty = fixture
        .service
        .create_tenant(&format!("keys-empty-{}", Uuid::new_v4().simple()))
        .await
        .expect("empty tenant");
    let empty_client = empty.key.client_id;
    let empty_manager_id = empty.key.key_id;
    let empty_secret = empty.key.credential().to_string();

    let empty_all = app
        .clone()
        .oneshot(keys_get(Some(&empty_secret), ""))
        .await
        .unwrap();
    assert_eq!(empty_all.status(), StatusCode::OK);
    let empty_all_body = body_string(empty_all).await;
    let (empty_all_keys, empty_all_more) = parse_inventory(&empty_all_body);
    assert_eq!(empty_all_keys.len(), 1);
    assert!(!empty_all_more);
    assert_eq!(
        listed_ids(&empty_all_keys),
        HashSet::from([empty_manager_id])
    );
    assert_safe_inventory_item(&empty_all_keys[0]);
    assert!(!empty_all_body.contains("credential"));
    assert!(!empty_all_body.contains("digest"));
    assert!(!empty_all_body.contains("key_digest"));
    assert!(!empty_all_body.contains("postgres://"));
    assert!(!empty_all_body.contains(&empty_secret));

    let empty_inference = app
        .clone()
        .oneshot(keys_get(Some(&empty_secret), "kind=inference"))
        .await
        .unwrap();
    assert_eq!(empty_inference.status(), StatusCode::OK);
    let empty_inference_body = body_string(empty_inference).await;
    let (empty_inference_keys, empty_inference_more) =
        parse_inventory(&empty_inference_body);
    assert!(empty_inference_keys.is_empty());
    assert!(!empty_inference_more);
    assert!(!empty_inference_body.contains(&empty_secret));

    let a_list = app
        .clone()
        .oneshot(keys_get(Some(&fixture.a_management.credential), ""))
        .await
        .unwrap();
    assert_eq!(a_list.status(), StatusCode::OK);
    let a_body = body_string(a_list).await;
    let (a_keys, a_more) = parse_inventory(&a_body);
    assert!(!a_more);
    for item in &a_keys {
        assert_safe_inventory_item(item);
        assert_eq!(
            item.get("client_id").and_then(|value| value.as_str()),
            Some(fixture.a_management.client_id.to_string()).as_deref()
        );
    }
    let a_ids = listed_ids(&a_keys);
    assert_eq!(
        a_ids,
        HashSet::from([fixture.a_management.key_id, fixture.a_inference.key_id])
    );
    assert!(!a_ids.contains(&fixture.b_management.key_id));
    assert!(!a_ids.contains(&fixture.b_inference.key_id));
    assert!(!a_body.contains(&fixture.a_management.credential));
    assert!(!a_body.contains(&fixture.b_management.credential));

    let b_list = app
        .clone()
        .oneshot(keys_get(Some(&fixture.b_management.credential), ""))
        .await
        .unwrap();
    assert_eq!(b_list.status(), StatusCode::OK);
    let b_body = body_string(b_list).await;
    let (b_keys, b_more) = parse_inventory(&b_body);
    assert!(!b_more);
    let b_ids = listed_ids(&b_keys);
    assert_eq!(
        b_ids,
        HashSet::from([fixture.b_management.key_id, fixture.b_inference.key_id])
    );
    assert!(!b_ids.contains(&fixture.a_management.key_id));
    assert!(!b_ids.contains(&fixture.a_inference.key_id));

    let a_inference_only = app
        .clone()
        .oneshot(keys_get(
            Some(&fixture.a_management.credential),
            "kind=inference",
        ))
        .await
        .unwrap();
    assert_eq!(a_inference_only.status(), StatusCode::OK);
    let a_inference_body = body_string(a_inference_only).await;
    let (a_inference_keys, a_inference_more) = parse_inventory(&a_inference_body);
    assert!(!a_inference_more);
    assert_eq!(
        listed_ids(&a_inference_keys),
        HashSet::from([fixture.a_inference.key_id])
    );

    let page = app
        .clone()
        .oneshot(keys_get(
            Some(&fixture.a_management.credential),
            "limit=1&offset=0",
        ))
        .await
        .unwrap();
    assert_eq!(page.status(), StatusCode::OK);
    let page_body = body_string(page).await;
    let (page_keys, page_more) = parse_inventory(&page_body);
    assert_eq!(page_keys.len(), 1);
    assert!(page_more);
    let first_id = listed_ids(&page_keys);
    let next = app
        .clone()
        .oneshot(keys_get(
            Some(&fixture.a_management.credential),
            "limit=1&offset=1",
        ))
        .await
        .unwrap();
    assert_eq!(next.status(), StatusCode::OK);
    let next_body = body_string(next).await;
    let (next_keys, next_more) = parse_inventory(&next_body);
    assert_eq!(next_keys.len(), 1);
    assert!(!next_more);
    let second_id = listed_ids(&next_keys);
    assert!(first_id.is_disjoint(&second_id));
    assert_eq!(
        first_id.union(&second_id).copied().collect::<HashSet<_>>(),
        a_ids
    );

    let extra_count = (MAX_KEY_LIST_LIMIT + 1) as usize;
    let base = Utc::now();
    let mut extra_ids = Vec::with_capacity(extra_count);
    let mut extra_digests = Vec::with_capacity(extra_count);
    for index in 0..extra_count {
        let (id, digest) = insert_named_key(
            &store,
            fixture.a_management.client_id,
            &format!("bound-{index}"),
            ApiKeyKind::Inference,
            base + Duration::milliseconds(index as i64),
        )
        .await;
        extra_ids.push(id);
        extra_digests.push(digest);
    }
    let newest_bound: HashSet<Uuid> = extra_ids[1..].iter().copied().collect();
    let bound_list = app
        .clone()
        .oneshot(keys_get(Some(&fixture.a_management.credential), ""))
        .await
        .unwrap();
    assert_eq!(bound_list.status(), StatusCode::OK);
    let bound_body = body_string(bound_list).await;
    let (bound_keys, bound_more) = parse_inventory(&bound_body);
    assert_eq!(bound_keys.len(), MAX_KEY_LIST_LIMIT as usize);
    assert!(bound_more);
    assert_eq!(listed_ids(&bound_keys), newest_bound);
    assert!(!listed_ids(&bound_keys).contains(&extra_ids[0]));
    assert!(!listed_ids(&bound_keys).contains(&fixture.b_management.key_id));
    for item in &bound_keys {
        assert_safe_inventory_item(item);
    }
    for digest in &extra_digests {
        assert!(omits_secret_and_digest(
            &bound_body,
            &fixture.a_management.credential,
            digest.as_bytes()
        ));
    }
    assert!(!bound_body.contains("credential"));
    assert!(!bound_body.contains("postgres://"));

    let remainder = app
        .clone()
        .oneshot(keys_get(
            Some(&fixture.a_management.credential),
            &format!("limit={MAX_KEY_LIST_LIMIT}&offset={MAX_KEY_LIST_LIMIT}"),
        ))
        .await
        .unwrap();
    assert_eq!(remainder.status(), StatusCode::OK);
    let remainder_body = body_string(remainder).await;
    let (remainder_keys, remainder_more) = parse_inventory(&remainder_body);
    assert_eq!(remainder_keys.len(), 3);
    assert!(!remainder_more);
    let remainder_ids = listed_ids(&remainder_keys);
    assert!(remainder_ids.contains(&extra_ids[0]));
    assert!(remainder_ids.contains(&fixture.a_management.key_id));
    assert!(remainder_ids.contains(&fixture.a_inference.key_id));

    let past_end = app
        .clone()
        .oneshot(keys_get(
            Some(&fixture.a_management.credential),
            "offset=1000",
        ))
        .await
        .unwrap();
    assert_eq!(past_end.status(), StatusCode::OK);
    let past_end_body = body_string(past_end).await;
    let (past_end_keys, past_end_more) = parse_inventory(&past_end_body);
    assert!(past_end_keys.is_empty());
    assert!(!past_end_more);

    cleanup_clients(&fixture.pool, &[empty_client]).await;
    fixture.drop_rows().await;
}

#[test]
fn http_issuance_reuses_cli_provisioning_service() {
    let service = include_str!("../src/features/auth/service.rs");
    let handler = include_str!("../src/features/auth/handler.rs");
    let implementation = service.split("mod tests").next().expect("implementation");
    assert!(implementation.contains("ProvisioningService"));
    assert!(implementation.contains("issue_key"));
    assert!(
        implementation.contains("generate_credential")
            || implementation.contains("issue_key")
    );
    assert!(handler.contains("create_api_key") || handler.contains("create_key"));
}

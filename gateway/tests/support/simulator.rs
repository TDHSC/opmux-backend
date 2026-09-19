//! Owned loopback OpenAI HTTP simulator with request capture and scripted replies.

#![allow(dead_code)]

use axum::{
    extract::{Request, State},
    http::{header, HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
    Router,
};
use serde_json::{json, Value};
use std::collections::VecDeque;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use tokio::task::JoinHandle;

/// Dummy provider credential accepted by the fixture. Not a live key.
pub const FIXTURE_PROVIDER_KEY: &str = "test-dummy-openai-key";

/// Deterministic assistant content returned on the default success path.
pub const SIMULATED_CONTENT: &str = "SIMULATED_OPENAI_OK";

const MAX_CAPTURE_BYTES: usize = 1_048_576;

/// Captured inbound provider request. Authorization values are not stored.
#[derive(Clone, Debug)]
pub struct CapturedRequest {
    /// HTTP method.
    pub method: String,
    /// Request path, including `/v1/...`.
    pub path: String,
    /// Content-Type header when present.
    pub content_type: Option<String>,
    /// Whether an Authorization header was present.
    pub authorization_present: bool,
    /// Whether Authorization matched the fixture Bearer credential.
    pub authorization_matches_fixture: bool,
    /// Parsed JSON body when the request had a JSON object.
    pub body: Option<Value>,
}

impl CapturedRequest {
    /// True when this capture is a Chat Completions generation call.
    pub fn is_generation(&self) -> bool {
        self.method == "POST" && self.path.ends_with("/chat/completions")
    }

    /// True when this capture is a `/models` probe.
    pub fn is_models_probe(&self) -> bool {
        self.method == "GET" && self.path.ends_with("/models")
    }
}

/// Scripted simulator response for the next matching request.
#[derive(Clone, Debug)]
pub enum ScriptedResponse {
    /// OpenAI-shaped chat completion success.
    ChatSuccess {
        /// Assistant message content.
        content: String,
        /// Model name echoed in the body; request model is used when `None`.
        model: Option<String>,
        /// Prompt token count.
        prompt_tokens: i64,
        /// Completion token count.
        completion_tokens: i64,
    },
    /// JSON HTTP response with an explicit status.
    Json {
        /// HTTP status code.
        status: u16,
        /// JSON body.
        body: Value,
        /// Optional Retry-After header value.
        retry_after: Option<String>,
    },
    /// Raw bytes, used for malformed bodies.
    Bytes {
        /// HTTP status code.
        status: u16,
        /// Raw response bytes.
        body: Vec<u8>,
        /// Content-Type value.
        content_type: &'static str,
    },
}

impl ScriptedResponse {
    /// Default deterministic chat success.
    pub fn chat_ok() -> Self {
        Self::ChatSuccess {
            content: SIMULATED_CONTENT.to_string(),
            model: None,
            prompt_tokens: 10,
            completion_tokens: 5,
        }
    }

    /// JSON error or other non-success chat reply.
    pub fn json_status(status: u16, body: Value) -> Self {
        Self::Json {
            status,
            body,
            retry_after: None,
        }
    }

    /// Models list success used by readiness probes.
    pub fn models_ok() -> Self {
        Self::Json {
            status: 200,
            body: json!({
                "object": "list",
                "data": [
                    {
                        "id": "gpt-4",
                        "object": "model",
                        "owned_by": "local-simulator"
                    }
                ]
            }),
            retry_after: None,
        }
    }
}

struct SimulatorInner {
    credential: String,
    captures: Mutex<Vec<CapturedRequest>>,
    chat_script: Mutex<VecDeque<ScriptedResponse>>,
    models_script: Mutex<VecDeque<ScriptedResponse>>,
}

/// Owned loopback OpenAI simulator. Aborting the task releases the listener.
pub struct OpenAiSimulator {
    addr: SocketAddr,
    base_url: String,
    credential: String,
    inner: Arc<SimulatorInner>,
    join: Option<JoinHandle<()>>,
}

impl OpenAiSimulator {
    /// Binds `127.0.0.1:0` and serves Chat Completions plus `/models`.
    pub async fn start() -> Self {
        Self::start_with_credential(FIXTURE_PROVIDER_KEY).await
    }

    /// Starts the simulator with an explicit dummy credential.
    pub async fn start_with_credential(credential: impl Into<String>) -> Self {
        let credential = credential.into();
        let inner = Arc::new(SimulatorInner {
            credential: credential.clone(),
            captures: Mutex::new(Vec::new()),
            chat_script: Mutex::new(VecDeque::new()),
            models_script: Mutex::new(VecDeque::new()),
        });

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("simulator should bind loopback");
        let addr = listener.local_addr().expect("simulator local addr");
        assert!(addr.ip().is_loopback(), "simulator must bind loopback only");
        assert!(
            !addr.ip().is_unspecified(),
            "simulator must not bind all interfaces"
        );

        let app = Router::new()
            .route("/v1/models", get(models_handler))
            .route("/v1/chat/completions", post(chat_handler))
            .with_state(inner.clone());

        let join = tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });

        Self {
            addr,
            base_url: format!("http://127.0.0.1:{}/v1", addr.port()),
            credential,
            inner,
            join: Some(join),
        }
    }

    /// Bound socket address.
    pub fn addr(&self) -> SocketAddr {
        self.addr
    }

    /// Provider base URL including the `/v1` suffix.
    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    /// Dummy credential the simulator accepts.
    pub fn credential(&self) -> &str {
        &self.credential
    }

    /// Queues the next Chat Completions response.
    pub fn enqueue_chat(&self, response: ScriptedResponse) {
        lock_vec(&self.inner.chat_script).push_back(response);
    }

    /// Queues the next `/models` response.
    pub fn enqueue_models(&self, response: ScriptedResponse) {
        lock_vec(&self.inner.models_script).push_back(response);
    }

    /// Snapshot of captured requests, oldest first.
    pub fn captured(&self) -> Vec<CapturedRequest> {
        lock_vec(&self.inner.captures).clone()
    }

    /// Number of Chat Completions calls captured.
    pub fn generation_count(&self) -> usize {
        self.captured()
            .iter()
            .filter(|capture| capture.is_generation())
            .count()
    }

    /// Number of `/models` probes captured.
    pub fn models_probe_count(&self) -> usize {
        self.captured()
            .iter()
            .filter(|capture| capture.is_models_probe())
            .count()
    }
}

impl Drop for OpenAiSimulator {
    fn drop(&mut self) {
        if let Some(join) = self.join.take() {
            join.abort();
        }
    }
}

fn lock_vec<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn capture_from_headers(
    method: &str,
    path: &str,
    headers: &HeaderMap,
    body: Option<Value>,
    expected_credential: &str,
) -> CapturedRequest {
    let authorization = headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok());
    let expected = format!("Bearer {expected_credential}");
    CapturedRequest {
        method: method.to_string(),
        path: path.to_string(),
        content_type: headers
            .get(header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .map(ToOwned::to_owned),
        authorization_present: authorization.is_some(),
        authorization_matches_fixture: authorization == Some(expected.as_str()),
        body,
    }
}

fn unauthorized() -> Response {
    (
        StatusCode::UNAUTHORIZED,
        axum::Json(json!({"error":{"message":"Dummy fixture authorization required"}})),
    )
        .into_response()
}

fn render(script: ScriptedResponse, request_model: Option<&str>) -> Response {
    match script {
        ScriptedResponse::ChatSuccess {
            content,
            model,
            prompt_tokens,
            completion_tokens,
        } => {
            let model = model
                .or_else(|| request_model.map(ToOwned::to_owned))
                .unwrap_or_else(|| "gpt-4".to_string());
            axum::Json(json!({
                "id": "chatcmpl-local-fixture",
                "object": "chat.completion",
                "created": 0,
                "model": model,
                "choices": [{
                    "index": 0,
                    "message": {"role": "assistant", "content": content},
                    "finish_reason": "stop"
                }],
                "usage": {
                    "prompt_tokens": prompt_tokens,
                    "completion_tokens": completion_tokens,
                    "total_tokens": prompt_tokens + completion_tokens
                }
            }))
            .into_response()
        }
        ScriptedResponse::Json {
            status,
            body,
            retry_after,
        } => {
            let status =
                StatusCode::from_u16(status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
            let mut response = (status, axum::Json(body)).into_response();
            if let Some(retry_after) = retry_after {
                if let Ok(value) = retry_after.parse() {
                    response.headers_mut().insert(header::RETRY_AFTER, value);
                }
            }
            response
        }
        ScriptedResponse::Bytes {
            status,
            body,
            content_type,
        } => {
            let status =
                StatusCode::from_u16(status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
            (status, [(header::CONTENT_TYPE, content_type)], body).into_response()
        }
    }
}

async fn models_handler(
    State(state): State<Arc<SimulatorInner>>,
    request: Request,
) -> Response {
    let headers = request.headers().clone();
    let capture = capture_from_headers(
        "GET",
        request.uri().path(),
        &headers,
        None,
        &state.credential,
    );
    lock_vec(&state.captures).push(capture.clone());
    if !capture.authorization_matches_fixture {
        return unauthorized();
    }
    let script = lock_vec(&state.models_script)
        .pop_front()
        .unwrap_or_else(ScriptedResponse::models_ok);
    render(script, None)
}

async fn chat_handler(
    State(state): State<Arc<SimulatorInner>>,
    request: Request,
) -> Response {
    let headers = request.headers().clone();
    let path = request.uri().path().to_string();
    let body_bytes = axum::body::to_bytes(request.into_body(), MAX_CAPTURE_BYTES)
        .await
        .unwrap_or_else(|_| axum::body::Bytes::new());
    let parsed: Option<Value> = serde_json::from_slice(&body_bytes).ok();
    let model = parsed
        .as_ref()
        .and_then(|value| value.get("model"))
        .and_then(Value::as_str)
        .map(ToOwned::to_owned);

    let capture =
        capture_from_headers("POST", &path, &headers, parsed, &state.credential);
    lock_vec(&state.captures).push(capture.clone());
    if !capture.authorization_matches_fixture {
        return unauthorized();
    }

    let script = lock_vec(&state.chat_script)
        .pop_front()
        .unwrap_or_else(ScriptedResponse::chat_ok);
    render(script, model.as_deref())
}

//! Owned loopback OpenAI HTTP simulator with request capture and scripted replies.

#![allow(dead_code)]

use axum::{
    body::{Body, Bytes},
    extract::{Request, State},
    http::{header, HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
    Router,
};
use futures_util::stream;
use serde_json::{json, Value};
use std::collections::VecDeque;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::Notify;
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
        /// Provider finish reason.
        finish_reason: String,
        /// Provider message role.
        role: String,
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
    /// Raw body with explicit length or chunked transfer.
    Raw {
        /// HTTP status code.
        status: u16,
        /// Payload bytes the client is intended to read first.
        body: Vec<u8>,
        /// Content-Type value.
        content_type: &'static str,
        /// When set, advertise this Content-Length even if it does not match `body`.
        advertised_content_length: Option<u64>,
        /// When true, stream without Content-Length so HTTP/1.1 uses chunked encoding.
        chunked: bool,
        /// Extra bytes the stream can still yield if the client keeps reading.
        extra_unread_bytes: usize,
    },
    /// Delay headers and/or the first body bytes of an inner script.
    Delayed {
        /// Sleep before the handler returns status and headers.
        before_headers: Duration,
        /// Sleep after headers, before the first body chunk.
        before_body: Duration,
        /// Response produced after the stall.
        inner: Box<ScriptedResponse>,
    },
    /// Wait for an explicit release before producing the inner response.
    Held {
        /// Signaled by [`ResponseHold::release`].
        notify: Arc<Notify>,
        /// Response produced after release.
        inner: Box<ScriptedResponse>,
    },
}

/// Releases a held simulator response.
#[derive(Clone)]
pub struct ResponseHold {
    notify: Arc<Notify>,
}

impl ResponseHold {
    /// Lets the waiting handler continue.
    pub fn release(&self) {
        self.notify.notify_waiters();
    }
}

impl ScriptedResponse {
    /// Default deterministic chat success.
    pub fn chat_ok() -> Self {
        Self::chat_ok_with(None, 10, 5)
    }

    /// Chat success with an explicit reported model and usage.
    pub fn chat_ok_with(
        model: Option<&str>,
        prompt_tokens: i64,
        completion_tokens: i64,
    ) -> Self {
        Self::ChatSuccess {
            content: SIMULATED_CONTENT.to_string(),
            model: model.map(ToOwned::to_owned),
            prompt_tokens,
            completion_tokens,
            finish_reason: "stop".to_string(),
            role: "assistant".to_string(),
        }
    }

    /// Chat success that can differ from the requested model and default finish reason.
    pub fn chat_reported(
        model: impl Into<String>,
        prompt_tokens: i64,
        completion_tokens: i64,
        finish_reason: impl Into<String>,
    ) -> Self {
        Self::ChatSuccess {
            content: SIMULATED_CONTENT.to_string(),
            model: Some(model.into()),
            prompt_tokens,
            completion_tokens,
            finish_reason: finish_reason.into(),
            role: "assistant".to_string(),
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

    /// JSON or malformed bytes with a 200 status.
    pub fn raw_json_bytes(body: impl Into<Vec<u8>>) -> Self {
        Self::Bytes {
            status: 200,
            body: body.into(),
            content_type: "application/json",
        }
    }

    /// Chunked success/error body without Content-Length.
    pub fn chunked(body: impl Into<Vec<u8>>, extra_unread_bytes: usize) -> Self {
        Self::Raw {
            status: 200,
            body: body.into(),
            content_type: "application/json",
            advertised_content_length: None,
            chunked: true,
            extra_unread_bytes,
        }
    }

    /// Body with an explicit advertised Content-Length, which may lie.
    pub fn advertised_length(body: impl Into<Vec<u8>>, content_length: u64) -> Self {
        Self::Raw {
            status: 200,
            body: body.into(),
            content_type: "application/json",
            advertised_content_length: Some(content_length),
            chunked: false,
            extra_unread_bytes: 0,
        }
    }

    /// Stalls sending status/headers, then yields `self`.
    pub fn delay_headers(self, delay: Duration) -> Self {
        Self::Delayed {
            before_headers: delay,
            before_body: Duration::ZERO,
            inner: Box::new(self),
        }
    }

    /// Sends headers, then stalls before the first body chunk.
    pub fn delay_body(self, delay: Duration) -> Self {
        Self::Delayed {
            before_headers: Duration::ZERO,
            before_body: delay,
            inner: Box::new(self),
        }
    }

    /// Holds this response until the returned latch is released.
    pub fn hold(self) -> (Self, ResponseHold) {
        let notify = Arc::new(Notify::new());
        (
            Self::Held {
                notify: notify.clone(),
                inner: Box::new(self),
            },
            ResponseHold { notify },
        )
    }
}

/// Minimum compact Chat Completions JSON used by padded fixtures.
pub fn min_padded_chat_completion_len() -> usize {
    padded_chat_completion_bytes_with(None, "gpt-4", SIMULATED_CONTENT).len()
}

/// Valid Chat Completions JSON padded to an exact byte length.
pub fn padded_chat_completion_bytes(target_len: usize) -> Vec<u8> {
    padded_chat_completion_bytes_with(Some(target_len), "gpt-4", SIMULATED_CONTENT)
}

fn padded_chat_completion_bytes_with(
    target_len: Option<usize>,
    model: &str,
    content: &str,
) -> Vec<u8> {
    let make = |pad: &str| {
        json!({
            "id": "chatcmpl-bound",
            "object": "chat.completion",
            "created": 0,
            "model": model,
            "choices": [{
                "index": 0,
                "message": {"role": "assistant", "content": content},
                "finish_reason": "stop"
            }],
            "usage": {
                "prompt_tokens": 10,
                "completion_tokens": 5,
                "total_tokens": 15
            },
            "pad": pad
        })
        .to_string()
    };
    let empty = make("");
    let Some(target_len) = target_len else {
        return empty.into_bytes();
    };
    assert!(
        target_len >= empty.len(),
        "target length {target_len} is below minimum padded chat JSON {}",
        empty.len()
    );
    let out = make(&"a".repeat(target_len - empty.len()));
    assert_eq!(out.len(), target_len);
    out.into_bytes()
}

struct SimulatorInner {
    credential: String,
    captures: Mutex<Vec<CapturedRequest>>,
    chat_script: Mutex<VecDeque<ScriptedResponse>>,
    models_script: Mutex<VecDeque<ScriptedResponse>>,
    chat_bytes_yielded: Arc<AtomicUsize>,
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
            chat_bytes_yielded: Arc::new(AtomicUsize::new(0)),
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

    /// Bytes yielded by the most recent Chat Completions response stream.
    pub fn last_chat_bytes_yielded(&self) -> usize {
        self.inner.chat_bytes_yielded.load(Ordering::SeqCst)
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

fn render(
    script: ScriptedResponse,
    request_model: Option<&str>,
    state: &Arc<SimulatorInner>,
) -> Response {
    match script {
        ScriptedResponse::ChatSuccess {
            content,
            model,
            prompt_tokens,
            completion_tokens,
            finish_reason,
            role,
        } => {
            let model = model
                .or_else(|| request_model.map(ToOwned::to_owned))
                .unwrap_or_else(|| "gpt-4".to_string());
            let body = json!({
                "id": "chatcmpl-local-fixture",
                "object": "chat.completion",
                "created": 0,
                "model": model,
                "choices": [{
                    "index": 0,
                    "message": {"role": role, "content": content},
                    "finish_reason": finish_reason
                }],
                "usage": {
                    "prompt_tokens": prompt_tokens,
                    "completion_tokens": completion_tokens,
                    "total_tokens": prompt_tokens + completion_tokens
                }
            });
            let encoded = body.to_string().into_bytes();
            state
                .chat_bytes_yielded
                .store(encoded.len(), Ordering::SeqCst);
            axum::Json(body).into_response()
        }
        ScriptedResponse::Json {
            status,
            body,
            retry_after,
        } => {
            let status =
                StatusCode::from_u16(status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
            let encoded = body.to_string().into_bytes();
            state
                .chat_bytes_yielded
                .store(encoded.len(), Ordering::SeqCst);
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
            state.chat_bytes_yielded.store(body.len(), Ordering::SeqCst);
            (status, [(header::CONTENT_TYPE, content_type)], body).into_response()
        }
        ScriptedResponse::Raw {
            status,
            body,
            content_type,
            advertised_content_length,
            chunked,
            extra_unread_bytes,
        } => {
            let status =
                StatusCode::from_u16(status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
            state.chat_bytes_yielded.store(0, Ordering::SeqCst);
            let stream_body =
                streamed_body(body, extra_unread_bytes, state.chat_bytes_yielded.clone());
            let mut response =
                (status, [(header::CONTENT_TYPE, content_type)], stream_body)
                    .into_response();
            if let Some(length) = advertised_content_length {
                response.headers_mut().insert(
                    header::CONTENT_LENGTH,
                    length.to_string().parse().expect("content-length"),
                );
            } else if chunked {
                response.headers_mut().remove(header::CONTENT_LENGTH);
            }
            response
        }
        ScriptedResponse::Delayed { .. } | ScriptedResponse::Held { .. } => {
            unreachable!("delayed or held scripts are unwrapped before render")
        }
    }
}

async fn respond(
    script: ScriptedResponse,
    request_model: Option<&str>,
    state: &Arc<SimulatorInner>,
) -> Response {
    let mut script = script;
    loop {
        match script {
            ScriptedResponse::Delayed {
                before_headers,
                before_body,
                inner,
            } => {
                if !before_headers.is_zero() {
                    tokio::time::sleep(before_headers).await;
                }
                let response = render(*inner, request_model, state);
                return stall_body(response, before_body).await;
            }
            ScriptedResponse::Held { notify, inner } => {
                notify.notified().await;
                script = *inner;
            }
            other => return render(other, request_model, state),
        }
    }
}

async fn stall_body(response: Response, delay: Duration) -> Response {
    if delay.is_zero() {
        return response;
    }
    let (parts, body) = response.into_parts();
    let collected = axum::body::to_bytes(body, MAX_CAPTURE_BYTES)
        .await
        .unwrap_or_default();
    let stalled = Body::from_stream(futures_util::stream::once(async move {
        tokio::time::sleep(delay).await;
        Ok::<Bytes, std::io::Error>(collected)
    }));
    Response::from_parts(parts, stalled)
}

fn streamed_body(
    body: Vec<u8>,
    extra_unread_bytes: usize,
    yielded: Arc<AtomicUsize>,
) -> Body {
    const CHUNK_SIZE: usize = 16;
    let mut chunks: Vec<Bytes> = body
        .chunks(CHUNK_SIZE)
        .map(Bytes::copy_from_slice)
        .collect();
    let mut remaining_extra = extra_unread_bytes;
    while remaining_extra > 0 {
        let size = remaining_extra.min(CHUNK_SIZE);
        chunks.push(Bytes::from(vec![b'X'; size]));
        remaining_extra -= size;
    }
    if chunks.is_empty() {
        chunks.push(Bytes::new());
    }
    Body::from_stream(stream::iter(chunks.into_iter().map(move |chunk| {
        yielded.fetch_add(chunk.len(), Ordering::SeqCst);
        Ok::<Bytes, std::io::Error>(chunk)
    })))
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
    respond(script, None, &state).await
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

    state.chat_bytes_yielded.store(0, Ordering::SeqCst);
    let script = lock_vec(&state.chat_script)
        .pop_front()
        .unwrap_or_else(ScriptedResponse::chat_ok);
    respond(script, model.as_deref(), &state).await
}

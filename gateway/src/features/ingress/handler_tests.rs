#[cfg(test)]
mod tests {
    use crate::core::correlation::RequestContext;
    use crate::core::deadline::RequestDeadline;
    use crate::features::auth::{
        ApiKeyKind, AuthContext, AuthService, UnavailableAuthStore,
    };
    use crate::features::executor::{
        config::ExecutorConfig,
        error::ExecutorError,
        models::{ExecutionParams, ExecutionResult},
        repository::ExecutorRepository,
        service::ExecutorService,
        vendors::LLMVendor,
    };
    use crate::features::health::HealthService;
    use crate::AppState;
    use async_trait::async_trait;
    use axum::{
        body::Body,
        http::{Request, StatusCode},
        routing::post,
        Extension, Router,
    };
    use serde_json::json;
    use std::collections::HashMap;
    use std::sync::Arc;
    use std::time::Duration;
    use tower::ServiceExt;

    #[derive(Clone)]
    struct MockVendor {
        vendor_id: String,
        supported_models: Vec<String>,
    }

    impl MockVendor {
        fn new(vendor_id: &str, models: Vec<&str>) -> Self {
            Self {
                vendor_id: vendor_id.to_string(),
                supported_models: models.into_iter().map(|m| m.to_string()).collect(),
            }
        }
    }

    #[async_trait]
    impl LLMVendor for MockVendor {
        async fn execute(
            &self,
            model: &str,
            _target_id: &str,
            _params: ExecutionParams,
        ) -> Result<ExecutionResult, ExecutorError> {
            Ok(ExecutionResult {
                content: "Mock handler response".to_string(),
                role: "assistant".to_string(),
                model_used: model.to_string(),
                prompt_tokens: 12,
                completion_tokens: 20,
                total_cost: 0.001,
                finish_reason: "stop".to_string(),
            })
        }

        fn vendor_id(&self) -> &str {
            &self.vendor_id
        }

        fn supports_model(&self, model: &str) -> bool {
            self.supported_models.contains(&model.to_string())
        }

        fn calculate_cost(
            &self,
            _prompt_tokens: i64,
            _completion_tokens: i64,
            _target_id: &str,
        ) -> Result<f64, ExecutorError> {
            Ok(0.001)
        }

        async fn health_check(&self, _timeout_secs: u64) -> Result<(), ExecutorError> {
            Ok(())
        }
    }

    fn create_mock_executor_service(models: Vec<&str>) -> Arc<ExecutorService> {
        let vendor = MockVendor::new("openai", models);

        let mut vendors = HashMap::new();
        vendors.insert("openai".to_string(), Arc::new(vendor) as Arc<dyn LLMVendor>);

        let repository = ExecutorRepository { vendors };

        Arc::new(ExecutorService::from_repository(
            repository,
            ExecutorConfig::mock_policy(3, 30_000),
        ))
    }

    fn build_test_app(models: Vec<&str>) -> Router {
        build_test_app_with_kind(models, ApiKeyKind::Inference)
    }

    fn build_test_app_with_kind(models: Vec<&str>, kind: ApiKeyKind) -> Router {
        let executor_service = create_mock_executor_service(models);
        let settings = Arc::new(crate::core::config::Settings::for_tests());
        let app_state = AppState {
            settings: settings.clone(),
            ingress_service: Arc::new(
                crate::features::ingress::service::IngressService::new(
                    executor_service.clone(),
                    settings.clone(),
                ),
            ),
            executor_service,
            health_service: Arc::new(HealthService::new()),
            auth_service: Arc::new(AuthService::new(Arc::new(UnavailableAuthStore))),
            admission: crate::core::admission::AdmissionLimiter::new(
                settings.limits.max_concurrent_generations,
            ),
            shutdown: crate::core::lifecycle::ShutdownState::new(),
        };

        Router::new()
            .route(
                "/api/v1/route",
                post(crate::features::ingress::ingress_handler),
            )
            .layer(Extension(AuthContext {
                client_id: uuid::Uuid::nil(),
                key_id: uuid::Uuid::nil(),
                kind,
            }))
            .layer(Extension(RequestContext::new(
                "req-handler-1".to_string(),
                Some("corr-1".to_string()),
            )))
            .layer(Extension(RequestDeadline::from_timeout(
                Duration::from_secs(60),
            )))
            .with_state(app_state)
    }

    #[tokio::test]
    async fn management_kind_cannot_generate() {
        let app = build_test_app_with_kind(
            vec!["example-chat-model", "example-chat-model-mini"],
            ApiKeyKind::Management,
        );

        let request = Request::builder()
            .method("POST")
            .uri("/api/v1/route")
            .header("content-type", "application/json")
            .body(Body::from(
                json!({ "prompt": "Hello", "metadata": {} }).to_string(),
            ))
            .unwrap();

        let response = app.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let body_str = String::from_utf8(body.to_vec()).unwrap();
        assert!(body_str.contains("permission"));
        assert!(!body_str.contains("Mock handler response"));
    }

    #[tokio::test]
    async fn test_ingress_handler_returns_400_for_empty_prompt() {
        let app = build_test_app(vec!["example-chat-model", "example-chat-model-mini"]);

        let request = Request::builder()
            .method("POST")
            .uri("/api/v1/route")
            .header("content-type", "application/json")
            .body(Body::from(
                json!({ "prompt": "   ", "metadata": {} }).to_string(),
            ))
            .unwrap();

        let response = app.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);

        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let body_str = String::from_utf8(body.to_vec()).unwrap();
        assert!(body_str.contains("Prompt cannot be empty"));
    }

    #[tokio::test]
    async fn test_ingress_handler_returns_200_for_valid_request() {
        let app = build_test_app(vec!["example-chat-model", "example-chat-model-mini"]);

        let request = Request::builder()
            .method("POST")
            .uri("/api/v1/route")
            .header("content-type", "application/json")
            .body(Body::from(
                json!({ "prompt": "Hello", "metadata": {} }).to_string(),
            ))
            .unwrap();

        let response = app.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let body_str = String::from_utf8(body.to_vec()).unwrap();
        assert!(body_str.contains("\"role\":\"assistant\""));
        assert!(body_str.contains("\"model_used\":\"example-chat-model\""));
    }

    #[tokio::test]
    async fn test_ingress_handler_returns_400_for_malformed_json() {
        let app = build_test_app(vec!["example-chat-model", "example-chat-model-mini"]);

        let request = Request::builder()
            .method("POST")
            .uri("/api/v1/route")
            .header("content-type", "application/json")
            .body(Body::from("{"))
            .unwrap();

        let response = app.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let body_str = String::from_utf8(body.to_vec()).unwrap();
        assert!(!body_str.contains("Mock handler response"));
    }

    #[tokio::test]
    async fn test_ingress_handler_returns_400_for_unknown_route() {
        let app = build_test_app(vec!["example-chat-model", "example-chat-model-mini"]);

        let request = Request::builder()
            .method("POST")
            .uri("/api/v1/route")
            .header("content-type", "application/json")
            .body(Body::from(
                json!({ "prompt": "Hello", "metadata": {}, "route": "missing" })
                    .to_string(),
            ))
            .unwrap();

        let response = app.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);

        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let body_str = String::from_utf8(body.to_vec()).unwrap();
        assert!(body_str.contains("Unknown route"));
        assert!(!body_str.contains("Mock handler response"));
    }

    #[tokio::test]
    async fn test_ingress_handler_returns_400_for_executor_unsupported_model() {
        let app = build_test_app(vec!["gpt-3.5-turbo"]);

        let request = Request::builder()
            .method("POST")
            .uri("/api/v1/route")
            .header("content-type", "application/json")
            .body(Body::from(
                json!({ "prompt": "Trigger unsupported model", "metadata": {} })
                    .to_string(),
            ))
            .unwrap();

        let response = app.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);

        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let body_str = String::from_utf8(body.to_vec()).unwrap();
        assert!(body_str.contains("INVALID_REQUEST"));
        assert!(!body_str.contains("gpt-3.5-turbo"));
    }

    #[tokio::test]
    async fn test_ingress_handler_returns_400_for_prompt_too_long() {
        let app = build_test_app(vec!["example-chat-model", "example-chat-model-mini"]);
        let long_prompt = "a".repeat(4001);

        let request = Request::builder()
            .method("POST")
            .uri("/api/v1/route")
            .header("content-type", "application/json")
            .body(Body::from(
                json!({ "prompt": long_prompt, "metadata": {} }).to_string(),
            ))
            .unwrap();

        let response = app.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);

        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let body_str = String::from_utf8(body.to_vec()).unwrap();
        assert!(body_str.contains("Prompt exceeds maximum length"));
    }

    #[tokio::test]
    async fn test_ingress_handler_returns_400_for_unsupported_stream_control() {
        let app = build_test_app(vec!["example-chat-model", "example-chat-model-mini"]);

        let request = Request::builder()
            .method("POST")
            .uri("/api/v1/route")
            .header("content-type", "application/json")
            .body(Body::from(
                json!({ "prompt": "Hello", "metadata": {}, "stream": true }).to_string(),
            ))
            .unwrap();

        let response = app.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);

        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let body_str = String::from_utf8(body.to_vec()).unwrap();
        assert!(body_str.contains("stream is not supported"));
        assert!(!body_str.contains("Mock handler response"));
    }

    #[tokio::test]
    async fn test_ingress_handler_returns_400_for_unknown_top_level_control() {
        let app = build_test_app(vec!["example-chat-model", "example-chat-model-mini"]);

        let request = Request::builder()
            .method("POST")
            .uri("/api/v1/route")
            .header("content-type", "application/json")
            .body(Body::from(
                json!({ "prompt": "Hello", "metadata": {}, "model": "gpt-4" })
                    .to_string(),
            ))
            .unwrap();

        let response = app.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);

        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let body_str = String::from_utf8(body.to_vec()).unwrap();
        assert!(body_str.contains("unknown control"));
        assert!(!body_str.contains("Mock handler response"));
    }

    #[tokio::test]
    async fn test_ingress_handler_returns_400_for_metadata_too_large() {
        let app = build_test_app(vec!["example-chat-model", "example-chat-model-mini"]);
        let large_value = "x".repeat(1_200);

        let request = Request::builder()
            .method("POST")
            .uri("/api/v1/route")
            .header("content-type", "application/json")
            .body(Body::from(
                json!({ "prompt": "Hello", "metadata": { "blob": large_value } })
                    .to_string(),
            ))
            .unwrap();

        let response = app.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);

        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let body_str = String::from_utf8(body.to_vec()).unwrap();
        assert!(body_str.contains("Metadata exceeds maximum size"));
    }
}

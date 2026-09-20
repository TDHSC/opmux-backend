// Tests for HealthService

#[cfg(test)]
mod tests {
    use crate::core::config::{Route, Settings};
    use crate::core::lifecycle::ShutdownState;
    use crate::features::auth::persist::{
        ApiKeyKind, ApiKeyRecord, ClientRecord, KeyDigest, NewApiKey, NewClient,
        RevokeOutcome,
    };
    use crate::features::auth::{
        AuthService, AuthStore, AuthStoreError, UnavailableAuthStore,
    };
    use crate::features::executor::{
        error::ExecutorError,
        models::{ExecutionParams, ExecutionResult},
        repository::ExecutorRepository,
        service::ExecutorService,
        vendors::LLMVendor,
    };
    use crate::features::health::service::{HealthConfig, HealthService};
    use async_trait::async_trait;
    use axum::{
        body::Body,
        http::{Request, StatusCode},
        routing::get,
        Router,
    };
    use chrono::{DateTime, Utc};
    use serial_test::serial;
    use std::collections::HashMap;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;
    use std::time::Duration;
    use tower::ServiceExt;
    use uuid::Uuid;

    #[derive(Clone)]
    struct MockVendor {
        vendor_id: String,
        supported_models: Vec<String>,
        health_check_error: Option<ExecutorError>,
        is_healthy: Option<Arc<AtomicBool>>,
        probes: Arc<std::sync::atomic::AtomicUsize>,
    }

    impl MockVendor {
        fn new_healthy(vendor_id: &str, models: Vec<&str>) -> Self {
            Self {
                vendor_id: vendor_id.to_string(),
                supported_models: models.iter().map(|s| s.to_string()).collect(),
                health_check_error: None,
                is_healthy: None,
                probes: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
            }
        }

        fn new_unhealthy(
            vendor_id: &str,
            models: Vec<&str>,
            error: ExecutorError,
        ) -> Self {
            Self {
                vendor_id: vendor_id.to_string(),
                supported_models: models.iter().map(|s| s.to_string()).collect(),
                health_check_error: Some(error),
                is_healthy: None,
                probes: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
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
                content: format!("Mock response from {}", self.vendor_id),
                role: "assistant".to_string(),
                model_used: model.to_string(),
                prompt_tokens: 10,
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
            self.probes.fetch_add(1, Ordering::SeqCst);
            if let Some(flag) = &self.is_healthy {
                if flag.load(Ordering::SeqCst) {
                    return Ok(());
                }
                return Err(ExecutorError::AuthenticationFailed(self.vendor_id.clone()));
            }
            match &self.health_check_error {
                Some(error) => Err(error.clone()),
                None => Ok(()),
            }
        }
    }

    struct ReadyAuthStore;

    #[async_trait]
    impl AuthStore for ReadyAuthStore {
        async fn provision_client_with_key(
            &self,
            _client: NewClient,
            _key: NewApiKey,
        ) -> Result<(ClientRecord, ApiKeyRecord), AuthStoreError> {
            Err(AuthStoreError::Unavailable)
        }

        async fn insert_key(
            &self,
            _key: NewApiKey,
        ) -> Result<ApiKeyRecord, AuthStoreError> {
            Err(AuthStoreError::Unavailable)
        }

        async fn find_key_by_digest(
            &self,
            _digest: &KeyDigest,
        ) -> Result<Option<ApiKeyRecord>, AuthStoreError> {
            Ok(None)
        }

        async fn authenticate_digest(
            &self,
            _digest: &KeyDigest,
            _used_at: DateTime<Utc>,
        ) -> Result<Option<ApiKeyRecord>, AuthStoreError> {
            Ok(None)
        }

        async fn touch_last_used(
            &self,
            _client_id: Uuid,
            _key_id: Uuid,
            _used_at: DateTime<Utc>,
        ) -> Result<bool, AuthStoreError> {
            Ok(false)
        }

        async fn list_keys_for_client(
            &self,
            _client_id: Uuid,
            _limit: i64,
            _offset: i64,
            _kind: Option<ApiKeyKind>,
        ) -> Result<Vec<ApiKeyRecord>, AuthStoreError> {
            Ok(Vec::new())
        }

        async fn revoke_key(
            &self,
            _client_id: Uuid,
            _key_id: Uuid,
            _revoked_at: DateTime<Utc>,
        ) -> Result<RevokeOutcome, AuthStoreError> {
            Ok(RevokeOutcome::NotFound)
        }

        async fn probe_authentication_access(&self) -> Result<(), AuthStoreError> {
            Ok(())
        }
    }

    struct DynamicAuthStore {
        available: Arc<AtomicBool>,
        probes: Arc<std::sync::atomic::AtomicUsize>,
    }

    #[async_trait]
    impl AuthStore for DynamicAuthStore {
        async fn provision_client_with_key(
            &self,
            _client: NewClient,
            _key: NewApiKey,
        ) -> Result<(ClientRecord, ApiKeyRecord), AuthStoreError> {
            Err(AuthStoreError::Unavailable)
        }

        async fn insert_key(
            &self,
            _key: NewApiKey,
        ) -> Result<ApiKeyRecord, AuthStoreError> {
            Err(AuthStoreError::Unavailable)
        }

        async fn find_key_by_digest(
            &self,
            _digest: &KeyDigest,
        ) -> Result<Option<ApiKeyRecord>, AuthStoreError> {
            Err(AuthStoreError::Unavailable)
        }

        async fn authenticate_digest(
            &self,
            _digest: &KeyDigest,
            _used_at: DateTime<Utc>,
        ) -> Result<Option<ApiKeyRecord>, AuthStoreError> {
            Err(AuthStoreError::Unavailable)
        }

        async fn touch_last_used(
            &self,
            _client_id: Uuid,
            _key_id: Uuid,
            _used_at: DateTime<Utc>,
        ) -> Result<bool, AuthStoreError> {
            Err(AuthStoreError::Unavailable)
        }

        async fn list_keys_for_client(
            &self,
            _client_id: Uuid,
            _limit: i64,
            _offset: i64,
            _kind: Option<ApiKeyKind>,
        ) -> Result<Vec<ApiKeyRecord>, AuthStoreError> {
            Err(AuthStoreError::Unavailable)
        }

        async fn revoke_key(
            &self,
            _client_id: Uuid,
            _key_id: Uuid,
            _revoked_at: DateTime<Utc>,
        ) -> Result<RevokeOutcome, AuthStoreError> {
            Err(AuthStoreError::Unavailable)
        }

        async fn probe_authentication_access(&self) -> Result<(), AuthStoreError> {
            self.probes.fetch_add(1, Ordering::SeqCst);
            if self.available.load(Ordering::SeqCst) {
                Ok(())
            } else {
                Err(AuthStoreError::Unavailable)
            }
        }
    }

    fn ready_auth() -> Arc<AuthService> {
        Arc::new(AuthService::new(Arc::new(ReadyAuthStore)))
    }

    fn mock_executor(vendor: MockVendor) -> Arc<ExecutorService> {
        let mut vendors = HashMap::new();
        vendors.insert(
            vendor.vendor_id.clone(),
            Arc::new(vendor) as Arc<dyn LLMVendor>,
        );
        Arc::new(ExecutorService::from_repository(
            ExecutorRepository { vendors },
            crate::features::executor::config::ExecutorConfig::mock_policy(3, 30_000),
        ))
    }

    fn healthy_executor() -> Arc<ExecutorService> {
        mock_executor(MockVendor::new_healthy(
            "mock-vendor",
            vec!["example-chat-model"],
        ))
    }

    fn ready_service(executor: Arc<ExecutorService>) -> HealthService {
        HealthService::with_dependencies(
            executor,
            ready_auth(),
            Arc::new(Settings::for_tests()),
            HealthConfig::new(2, 5),
        )
    }

    fn fallback_settings() -> Settings {
        let mut settings = Settings::for_tests();
        settings.catalog.routes.insert(
            "default".to_string(),
            Route {
                primary: "primary".to_string(),
                fallbacks: vec!["secondary".to_string()],
            },
        );
        settings
    }

    #[tokio::test]
    async fn test_check_health_returns_version_and_uptime() {
        let service = HealthService::new();
        let result = service.check_health().await.unwrap();
        assert_eq!(result.status, "healthy");
        assert_eq!(result.version, Some(env!("CARGO_PKG_VERSION").to_string()));
        assert!(result.uptime_seconds.is_some());
    }

    #[tokio::test]
    async fn test_check_health_always_returns_healthy() {
        let service = HealthService::new();
        for _ in 0..5 {
            let result = service.check_health().await.unwrap();
            assert_eq!(result.status, "healthy");
        }
    }

    #[tokio::test]
    async fn liveness_stays_healthy_when_readiness_dependencies_are_missing() {
        let service = HealthService::new();
        let health = service.check_health().await.unwrap();
        let ready = service.check_readiness().await.unwrap();
        assert_eq!(health.status, "healthy");
        assert_eq!(ready.status, "not_ready");
        assert_eq!(ready.dependencies.database.status, "unhealthy");
        assert_eq!(ready.dependencies.upstream.status, "unhealthy");
        assert_eq!(ready.dependencies.default_route.status, "unhealthy");
    }

    #[tokio::test]
    async fn test_check_readiness_with_healthy_dependencies() {
        let service = ready_service(healthy_executor());
        let result = service.check_readiness().await.unwrap();
        assert_eq!(result.status, "ready");
        assert_eq!(result.dependencies.database.status, "healthy");
        assert_eq!(result.dependencies.upstream.status, "healthy");
        assert_eq!(result.dependencies.default_route.status, "healthy");
        assert!(result.dependencies.database.error.is_none());
        assert!(result.dependencies.upstream.error.is_none());
    }

    #[tokio::test]
    async fn test_check_readiness_with_unhealthy_upstream() {
        let executor = mock_executor(MockVendor::new_unhealthy(
            "mock-vendor",
            vec!["example-chat-model"],
            ExecutorError::AuthenticationFailed("mock-vendor".to_string()),
        ));
        let service = ready_service(executor);
        let result = service.check_readiness().await.unwrap();
        assert_eq!(result.status, "not_ready");
        assert_eq!(result.dependencies.upstream.status, "unhealthy");
        assert_eq!(
            result.dependencies.upstream.error.as_deref(),
            Some("Upstream provider unreachable")
        );
        assert_eq!(result.dependencies.database.status, "healthy");
    }

    #[tokio::test]
    async fn missing_auth_store_cannot_report_ready() {
        let service = HealthService::with_executor(healthy_executor());
        let result = service.check_readiness().await.unwrap();
        assert_eq!(result.status, "not_ready");
        assert_eq!(result.dependencies.database.status, "unhealthy");
        assert_eq!(
            result.dependencies.database.error.as_deref(),
            Some("Authentication database unavailable")
        );
    }

    #[tokio::test]
    async fn unavailable_auth_store_cannot_report_ready() {
        let service = HealthService::with_dependencies(
            healthy_executor(),
            Arc::new(AuthService::new(Arc::new(UnavailableAuthStore))),
            Arc::new(Settings::for_tests()),
            HealthConfig::new(2, 0),
        );
        let result = service.check_readiness().await.unwrap();
        assert_eq!(result.status, "not_ready");
        assert_eq!(result.dependencies.database.status, "unhealthy");
    }

    #[tokio::test]
    async fn test_cache_hit_skips_upstream_probe() {
        let vendor = MockVendor::new_healthy("mock-vendor", vec!["example-chat-model"]);
        let probes = vendor.probes.clone();
        let service = ready_service(mock_executor(vendor));
        assert_eq!(service.check_readiness().await.unwrap().status, "ready");
        assert_eq!(probes.load(Ordering::SeqCst), 1);
        assert_eq!(service.check_readiness().await.unwrap().status, "ready");
        assert_eq!(probes.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    #[serial]
    async fn test_cache_miss_after_ttl_expiry() {
        let vendor = MockVendor::new_healthy("mock-vendor", vec!["example-chat-model"]);
        let probes = vendor.probes.clone();
        let service = HealthService::with_dependencies(
            mock_executor(vendor),
            ready_auth(),
            Arc::new(Settings::for_tests()),
            HealthConfig::new(2, 1),
        );
        assert_eq!(service.check_readiness().await.unwrap().status, "ready");
        assert_eq!(probes.load(Ordering::SeqCst), 1);
        tokio::time::sleep(Duration::from_millis(1100)).await;
        assert_eq!(service.check_readiness().await.unwrap().status, "ready");
        assert_eq!(probes.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn failed_upstream_probe_is_not_cached() {
        let is_healthy = Arc::new(AtomicBool::new(false));
        let vendor = MockVendor {
            vendor_id: "dynamic-vendor".to_string(),
            supported_models: vec!["example-chat-model".to_string()],
            health_check_error: None,
            is_healthy: Some(is_healthy.clone()),
            probes: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
        };
        let service = ready_service(mock_executor(vendor));
        assert_eq!(service.check_readiness().await.unwrap().status, "not_ready");
        is_healthy.store(true, Ordering::SeqCst);
        assert_eq!(service.check_readiness().await.unwrap().status, "ready");
    }

    #[tokio::test]
    async fn failed_database_probe_is_not_cached() {
        let available = Arc::new(AtomicBool::new(false));
        let probes = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let auth = Arc::new(AuthService::new(Arc::new(DynamicAuthStore {
            available: available.clone(),
            probes: probes.clone(),
        })));
        let service = HealthService::with_dependencies(
            healthy_executor(),
            auth,
            Arc::new(Settings::for_tests()),
            HealthConfig::new(2, 5),
        );
        assert_eq!(service.check_readiness().await.unwrap().status, "not_ready");
        assert_eq!(probes.load(Ordering::SeqCst), 1);
        available.store(true, Ordering::SeqCst);
        assert_eq!(service.check_readiness().await.unwrap().status, "ready");
        assert_eq!(probes.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn draining_overrides_cached_readiness_without_new_probes() {
        let vendor = MockVendor::new_healthy("mock-vendor", vec!["example-chat-model"]);
        let probes = vendor.probes.clone();
        let shutdown = ShutdownState::new();
        let service = HealthService::with_dependencies(
            mock_executor(vendor),
            ready_auth(),
            Arc::new(Settings::for_tests()),
            HealthConfig::new(2, 60),
        )
        .with_shutdown_state(shutdown.clone());
        let ready = service.check_readiness().await.unwrap();
        assert_eq!(ready.status, "ready");
        assert!(!ready.draining);
        assert_eq!(probes.load(Ordering::SeqCst), 1);

        shutdown.mark_draining();
        let draining = service.check_readiness().await.unwrap();
        assert_eq!(draining.status, "not_ready");
        assert!(draining.draining);
        assert_eq!(draining.dependencies.upstream.status, "healthy");
        assert_eq!(probes.load(Ordering::SeqCst), 1);

        let live = service.check_health().await.unwrap();
        assert_eq!(live.status, "healthy");
    }

    #[tokio::test]
    async fn open_primary_with_usable_fallback_stays_ready() {
        let executor = healthy_executor();
        executor.force_open_target("primary");
        let service = HealthService::with_dependencies(
            executor,
            ready_auth(),
            Arc::new(fallback_settings()),
            HealthConfig::new(2, 5),
        );
        let result = service.check_readiness().await.unwrap();
        assert_eq!(result.status, "ready");
        assert_eq!(result.dependencies.default_route.status, "healthy");
    }

    #[tokio::test]
    async fn all_open_default_targets_override_cached_upstream_health() {
        let vendor = MockVendor::new_healthy("mock-vendor", vec!["example-chat-model"]);
        let probes = vendor.probes.clone();
        let executor = mock_executor(vendor);
        let service = HealthService::with_dependencies(
            executor.clone(),
            ready_auth(),
            Arc::new(fallback_settings()),
            HealthConfig::new(2, 60),
        );
        assert_eq!(service.check_readiness().await.unwrap().status, "ready");
        assert_eq!(probes.load(Ordering::SeqCst), 1);

        executor.force_open_target("primary");
        executor.force_open_target("secondary");
        let result = service.check_readiness().await.unwrap();
        assert_eq!(result.status, "not_ready");
        assert_eq!(result.dependencies.upstream.status, "healthy");
        assert_eq!(result.dependencies.default_route.status, "unhealthy");
        assert_eq!(
            result.dependencies.default_route.error.as_deref(),
            Some("No usable default-route target")
        );
        assert_eq!(probes.load(Ordering::SeqCst), 1);

        executor.force_close_target("secondary");
        let recovered = service.check_readiness().await.unwrap();
        assert_eq!(recovered.status, "ready");
        assert_eq!(probes.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    #[serial]
    async fn test_health_config_from_env_default() {
        std::env::remove_var("HEALTH_CHECK_TIMEOUT");
        let config = HealthConfig::from_env();
        assert_eq!(config.timeout, 2);
    }

    #[tokio::test]
    #[serial]
    async fn test_health_config_from_env_custom() {
        std::env::set_var("HEALTH_CHECK_TIMEOUT", "5");
        let config = HealthConfig::from_env();
        assert_eq!(config.timeout, 5);
        std::env::remove_var("HEALTH_CHECK_TIMEOUT");
    }

    #[tokio::test]
    #[serial]
    async fn test_health_config_from_env_invalid() {
        std::env::set_var("HEALTH_CHECK_TIMEOUT", "invalid");
        let config = HealthConfig::from_env();
        assert_eq!(config.timeout, 2);
        std::env::remove_var("HEALTH_CHECK_TIMEOUT");
    }

    #[tokio::test]
    async fn test_check_readiness_timestamp_format() {
        let service = HealthService::new();
        let result = service.check_readiness().await.unwrap();
        assert!(result.timestamp.contains('T'));
        assert!(result.timestamp.contains('+') || result.timestamp.contains('Z'));
    }

    #[tokio::test]
    async fn test_check_health_timestamp_format() {
        let service = HealthService::new();
        let result = service.check_health().await.unwrap();
        assert!(result.timestamp.contains('T'));
        assert!(result.timestamp.contains('+') || result.timestamp.contains('Z'));
    }

    fn build_test_app(health_service: HealthService) -> Router {
        let executor = healthy_executor();
        let settings = Arc::new(crate::core::config::Settings::for_tests());
        let app_state = crate::AppState {
            settings: settings.clone(),
            ingress_service: Arc::new(
                crate::features::ingress::service::IngressService::new(
                    executor.clone(),
                    settings.clone(),
                ),
            ),
            executor_service: executor,
            health_service: Arc::new(health_service),
            auth_service: Arc::new(crate::features::auth::AuthService::new(Arc::new(
                crate::features::auth::UnavailableAuthStore,
            ))),
            admission: crate::core::admission::AdmissionLimiter::new(
                settings.limits.max_concurrent_generations,
            ),
            shutdown: crate::core::lifecycle::ShutdownState::new(),
        };

        Router::new()
            .route("/health", get(crate::features::health::health_handler))
            .route("/ready", get(crate::features::health::ready_handler))
            .with_state(app_state)
    }

    #[tokio::test]
    async fn test_health_handler_returns_200_ok() {
        let app = build_test_app(HealthService::new());
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/health")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn test_health_handler_returns_json_with_version() {
        let app = build_test_app(HealthService::new());
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/health")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let body_str = String::from_utf8(body.to_vec()).unwrap();
        assert!(body_str.contains("\"status\":\"healthy\""));
        assert!(
            body_str.contains(&format!("\"version\":\"{}\"", env!("CARGO_PKG_VERSION")))
        );
        assert!(body_str.contains("\"uptime_seconds\""));
    }

    #[tokio::test]
    async fn test_ready_handler_returns_503_when_not_ready() {
        let executor = mock_executor(MockVendor::new_unhealthy(
            "mock-vendor",
            vec!["example-chat-model"],
            ExecutorError::AuthenticationFailed("mock-vendor".to_string()),
        ));
        let service = ready_service(executor);
        let app = build_test_app(service);
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/ready")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let body_str = String::from_utf8(body.to_vec()).unwrap();
        assert!(body_str.contains("\"status\":\"not_ready\""));
        assert!(body_str.contains("\"error\":\"Upstream provider unreachable\""));
    }

    #[tokio::test]
    async fn test_ready_handler_returns_200_when_ready() {
        let app = build_test_app(ready_service(healthy_executor()));
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/ready")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let body_str = String::from_utf8(body.to_vec()).unwrap();
        assert!(body_str.contains("\"status\":\"ready\""));
        assert!(body_str.contains("\"database\""));
        assert!(body_str.contains("\"upstream\""));
        assert!(body_str.contains("\"default_route\""));
    }
}

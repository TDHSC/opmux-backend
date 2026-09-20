//! Shared HTTP fixture helpers for production-router integration tests.

#![allow(dead_code, unused_imports)]

mod db;
mod env;
mod proxy;
mod simulator;

use gateway::{
    app::Application,
    core::{config::Settings, metrics::MetricsConfig},
    features::{auth::AuthService, executor::config::OpenAIConfig},
};
use std::sync::Arc;

pub use db::{
    auth_service_from_pool, cleanup_clients, owned_database_url_violation,
    provision_inference_key, required_database_url, rewrite_owned_database_url_port,
    test_pool, IssuedInference, OWNED_DATABASE_HOST, OWNED_DATABASE_NAME,
    OWNED_DATABASE_PORT,
};
pub use env::isolate_provider_environment;
pub use proxy::RecoverableDbProxy;
pub use simulator::{
    min_padded_chat_completion_len, padded_chat_completion_bytes, CapturedRequest,
    OpenAiSimulator, ResponseHold, ScriptedResponse, FIXTURE_PROVIDER_KEY,
    SIMULATED_CONTENT,
};

/// Former public mock gateway key. Runtime authentication must reject it.
pub const MOCK_GATEWAY_API_KEY: &str = "test-api-key-123";

/// Builds dummy local settings aimed at an owned simulator.
pub fn settings_for_simulator(simulator: &OpenAiSimulator) -> Arc<Settings> {
    Arc::new(Settings::for_tests_with_provider(
        simulator.base_url(),
        simulator.credential(),
    ))
}

/// Builds dummy local settings and applies test-only limit overrides.
pub fn settings_for_simulator_with(
    simulator: &OpenAiSimulator,
    mutate: impl FnOnce(&mut Settings),
) -> Arc<Settings> {
    let mut settings =
        Settings::for_tests_with_provider(simulator.base_url(), simulator.credential());
    mutate(&mut settings);
    Arc::new(settings)
}

/// Builds the production router against an owned simulator and injected auth.
pub fn production_router_with_auth(
    simulator: &OpenAiSimulator,
    auth_service: Arc<AuthService>,
    metrics: MetricsConfig,
) -> axum::Router {
    production_router_with_settings(
        settings_for_simulator(simulator),
        auth_service,
        metrics,
    )
}

/// Builds the production router from explicit settings and injected auth.
pub fn production_router_with_settings(
    settings: Arc<Settings>,
    auth_service: Arc<AuthService>,
    metrics: MetricsConfig,
) -> axum::Router {
    Application::from_settings_and_metrics(settings, auth_service, metrics.clone())
        .expect("application should build from fixture settings")
        .into_router(metrics)
}

/// OpenAI adapter config aimed at an owned simulator.
pub fn openai_config_for_simulator(simulator: &OpenAiSimulator) -> OpenAIConfig {
    let mut config = OpenAIConfig::for_tests();
    config.api_key = simulator.credential().to_string();
    config.base_url = simulator.base_url().to_string();
    config.timeout_ms = 2_000;
    config
}

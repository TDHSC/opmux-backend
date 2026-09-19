//! Shared HTTP fixture helpers for production-router integration tests.

#![allow(dead_code, unused_imports)]

mod env;
mod simulator;

use gateway::{
    app::Application,
    core::{config::Settings, metrics::MetricsConfig},
    features::executor::config::OpenAIConfig,
};
use std::sync::Arc;

pub use env::isolate_provider_environment;
pub use simulator::{
    CapturedRequest, OpenAiSimulator, ScriptedResponse, FIXTURE_PROVIDER_KEY,
    SIMULATED_CONTENT,
};

/// Existing mock gateway key accepted by current authentication.
pub const MOCK_GATEWAY_API_KEY: &str = "test-api-key-123";

/// Builds dummy local settings aimed at an owned simulator.
pub fn settings_for_simulator(simulator: &OpenAiSimulator) -> Arc<Settings> {
    Arc::new(Settings::for_tests_with_provider(
        simulator.base_url(),
        simulator.credential(),
    ))
}

/// Builds the production router against an owned simulator.
pub fn production_router_for_simulator(
    simulator: &OpenAiSimulator,
    metrics: MetricsConfig,
) -> axum::Router {
    Application::from_settings(settings_for_simulator(simulator))
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

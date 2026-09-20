//! Gateway Service Library
//!
//! AI API Router Gateway Microservice
//! Modules will be added as needed during development

use std::sync::Arc;

use crate::core::admission::AdmissionLimiter;
use crate::core::lifecycle::ShutdownState;

// Shared production application and router composition
pub mod app;

// Core reusable primitives
pub mod core;

// Business-specific feature modules
pub mod features;

// HTTP middleware
pub mod middleware;

/// Application state shared across all handlers.
///
/// This struct holds all shared services and resources that need to be
/// accessible throughout the application lifecycle. Defined at the library
/// root level to avoid coupling between features.
///
/// # Architecture
/// - Defined in lib.rs (library root) rather than in any feature module
/// - All features import AppState from crate root: `use crate::AppState;`
/// - Prevents feature-to-feature coupling (e.g., billing importing from ingress)
///
/// # Future Extensions
/// - Database connection pool (`db_pool: Arc<DbPool>`)
/// - Metrics client (`metrics_client: Arc<MetricsClient>`)
/// - Cache client (`cache_client: Arc<CacheClient>`)
#[derive(Clone)]
pub struct AppState {
    /// Validated operator settings injected at startup.
    pub settings: Arc<core::config::Settings>,

    /// Shared ExecutorService for LLM execution across all requests
    pub executor_service: Arc<features::executor::service::ExecutorService>,

    /// Shared IngressService for stateless configured routing
    pub ingress_service: Arc<features::ingress::service::IngressService>,

    /// Shared HealthService for health and readiness checks
    pub health_service: Arc<features::health::HealthService>,

    /// Shared AuthService for persisted API-key authentication
    pub auth_service: Arc<features::auth::AuthService>,

    /// Non-blocking concurrent generation admission limiter.
    ///
    /// Shared with [`Self::shutdown`]. Drain closes this limiter before
    /// publishing the drain flag.
    pub admission: AdmissionLimiter,

    /// Process drain flag shared by readiness and generation admission.
    pub shutdown: ShutdownState,
}

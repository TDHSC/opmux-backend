//! Ingress module for stateless AI routing.
//!
//! Accepts prompt and opaque metadata, optional route/fallback controls, and
//! typed generation parameters. Selects an operator-configured route and
//! executes through the executor boundary. Metadata is never forwarded, logged,
//! or persisted.
//!
//! # Request Flow
//!
//! 1. **Handler** - Validates HTTP requests, extracts data, and admits generation
//! 2. **Service** - Selects a configured route and builds a flat target plan
//! 3. **Repository** - Calls the executor execution boundary
//!
//! # Usage
//!
//! ```bash
//! curl -X POST http://localhost:3000/api/v1/route \
//!   -H "Content-Type: application/json" \
//!   -H "X-API-Key: $INFERENCE_KEY" \
//!   -d '{"prompt": "Hello!", "metadata": {}}'
//! ```
//!
//! Omitted `route` selects the catalog default. Omitted `allow_fallback`
//! follows the configured chain; `allow_fallback=false` limits execution to
//! the primary target without disabling its retries. Omitted `parameters`
//! uses documented defaults, including the selected target's output-token cap.

/// Error handling for ingress operations.
pub mod error;
/// Handler Layer - HTTP request/response processing.
pub mod handler;
/// Repository Layer - executor execution boundary.
pub mod repository;
/// Configured route selection.
mod routing;
/// Service Layer - Business logic and orchestration.
pub mod service;
/// Canonical request parsing and validation.
mod validate;

/// Constants and hardcoded values.
pub mod constants;

#[cfg(test)]
mod handler_tests;
#[cfg(test)]
mod service_tests;

// Re-export the handler for easy access
pub use handler::ingress_handler;

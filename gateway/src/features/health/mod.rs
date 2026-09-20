//! Health check module following 3-layer architecture.
//!
//! This module provides comprehensive health check functionality for the gateway service,
//! implementing a clean 3-layer architecture pattern with proper error handling and
//! structured responses.
//!
//! # Architecture
//!
//! The module follows the standard 3-layer architecture:
//!
//! - **Handler Layer** (`handler.rs`) - HTTP request/response processing
//! - **Service Layer** (`service.rs`) - Business logic and orchestration
//! - **Repository Layer** (`repository.rs`) - Data access and system monitoring
//! - **Error Layer** (`error.rs`) - Structured error handling
//!
//! # Usage
//!
//! The health check endpoint is typically mounted at `/health` and provides:
//!
//! ```bash
//! curl http://localhost:3000/health
//! # Returns: {"status":"healthy","timestamp":"2025-09-01T16:53:30.625665+00:00"}
//! ```
//!
//! # Health Check Types
//!
//! Currently supports:
//! - Process liveness on `/health`
//! - Authentication-database schema/access, upstream `/models` reachability,
//!   and usable default-route targets on `/ready`
//!
//! # Error Handling
//!
//! All errors follow the structured error handling pattern:
//! - Business operation-focused error variants
//! - Consistent JSON error responses
//! - Appropriate HTTP status codes (503 for health failures)
//! - Structured logging integration

/// Error handling for health check operations.
///
/// Provides structured error types for all health check business operations.
pub mod error;

/// Handler Layer - HTTP request/response processing.
///
/// Contains the HTTP handler function for health check endpoints.
pub mod handler;

/// Repository Layer - Data access & system monitoring.
///
/// Handles data access operations for health checks and system status monitoring.
pub mod repository;

/// Service Layer - Business logic abstraction.
///
/// Contains business logic for health check orchestration and status determination.
pub mod service;

/// Unit tests for HealthService.
#[cfg(test)]
mod service_tests;

// Re-export handlers and service for easy access
pub use handler::{health_handler, ready_handler};
pub use service::{HealthConfig, HealthService};

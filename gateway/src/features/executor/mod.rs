//! Executor feature - LLM execution with retry and fallback logic.
//!
//! This feature provides LLM execution capabilities with business logic
//! for retry, fallback, and parameter extraction.
//!
//! # Architecture
//!
//! - **Service Layer** - Business logic: retry, fallback, parameter extraction
//! - **Repository Layer** - Data access: vendor management, direct LLM API calls
//!
//! # Usage
//!
//! ```rust,ignore
//! use gateway::features::executor::{ExecutorService, ExecutorConfig};
//!
//! let config = ExecutorConfig::from_env();
//! let service = ExecutorService::from_config(config)?;
//! let result = service.execute(&plan, &payload, deadline).await?;
//! ```

pub mod attempt;
pub mod bounded_body;
pub(crate) mod budget;
pub(crate) mod circuit;
pub mod config;
pub mod error;
pub mod models;
pub(crate) mod openai_response;
pub mod pricing;
pub mod repository;
pub mod service;
pub mod vendors;

// Re-export commonly used types
pub use attempt::AttemptContext;
pub use config::ExecutorConfig;
pub use error::ExecutorError;
pub use models::{ExecutionParams, ExecutionResult, Message};
pub use repository::ExecutorRepository;
pub use service::ExecutorService;

// Tests in separate files
#[cfg(test)]
mod budget_tests;
#[cfg(test)]
mod circuit_tests;
#[cfg(test)]
mod deadline_tests;
#[cfg(test)]
mod fallback_tests;
#[cfg(test)]
mod repository_tests;
#[cfg(test)]
mod service_tests;

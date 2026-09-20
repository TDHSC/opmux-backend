// Core module containing reusable primitives and shared functionality

pub mod admission; // Non-blocking concurrent generation admission
pub mod config; // Centralized configuration management
pub mod contracts;
pub mod correlation; // Request correlation and context management (Task 10.1.2)
pub mod db; // Bounded SQLx pool configuration (runtime SQL, no migrator)
pub mod deadline; // Monotonic protected-request deadline
pub mod error; // Application-wide error handling
pub mod extract; // Protected-endpoint extractors with the canonical envelope
pub mod http_error; // Canonical protected-API error envelope
pub mod lifecycle; // Drain state and bounded process shutdown
pub mod metrics; // Prometheus metrics configuration (Task 10.1.4)
pub mod tracing; // Tracing and logging configuration (Task 10.1.3)

//! # Media Gateway Core
//!
//! Core data structures and types for the Media Gateway platform.
//!
//! This crate provides the fundamental building blocks for content management,
//! user profiles, search functionality, and error handling across the Media Gateway ecosystem.
//!
//! ## Modules
//!
//! - `types`: Core type definitions and enums
//! - `models`: Domain models for content, users, and search
//! - `error`: Error types and handling
//! - `validation`: Validation utilities and functions
//! - `database`: Shared PostgreSQL connection pool
//! - `math`: Mathematical utilities for vector operations
//! - `metrics`: Prometheus metrics collection and exposition
//! - `observability`: Structured logging and distributed tracing
//! - `health`: Production-ready health check system
//! - `config`: Configuration loading and validation
//! - `retry`: Exponential backoff retry utilities

pub mod config;
pub mod database;
pub mod error;
pub mod health;
pub mod math;
pub mod metrics;
pub mod models;
pub mod observability;
pub mod retry;
pub mod types;
pub mod validation;

#[cfg(test)]
mod tests;

// Re-export commonly used types
pub use config::{
    ConfigLoader, DatabaseConfig as ConfigDatabaseConfig, RedisConfig, ServiceConfig,
    load_dotenv,
};
pub use database::{DatabaseConfig, DatabasePool, PoolStats};
pub use error::MediaGatewayError;
pub use health::{
    AggregatedHealth, ComponentHealth, HealthCheck, HealthChecker, HealthStatus, SimpleHealth,
};
pub use math::{cosine_similarity, dot_product, l2_distance, normalize_vector};
pub use metrics::{
    decrement_active_connections, increment_active_connections, metrics_handler,
    observe_http_duration, record_cache_hit, record_cache_miss, record_http_request,
    update_db_pool_metrics, MetricsMiddleware, MetricsRegistry, METRICS_REGISTRY,
};
pub use models::{content, search, user};
pub use observability::{
    api_span, current_correlation_id, db_span, init_logging, request_span, with_correlation_id,
    LogConfig, LogFormat, ObservabilityError,
};
pub use retry::{retry_with_backoff, RetryPolicy};
pub use types::*;

/// Result type alias for Media Gateway operations
pub type Result<T> = std::result::Result<T, MediaGatewayError>;

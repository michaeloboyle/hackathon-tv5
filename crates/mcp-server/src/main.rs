//! MCP Server binary
//!
//! Entry point for the Model Context Protocol server.

use axum::{
    routing::{get, post},
    Router,
};
use media_gateway_core::{init_logging, DatabaseConfig, DatabasePool, LogConfig, LogFormat};
use media_gateway_mcp::{handlers, McpServerConfig, McpServerState};
use std::sync::Arc;
use tower_http::cors::CorsLayer;
use tracing::{error, info};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Load environment variables
    dotenvy::dotenv().ok();

    // Initialize logging
    let log_config = LogConfig {
        format: LogFormat::Json,
        level: std::env::var("RUST_LOG").unwrap_or_else(|_| "info".to_string()),
        service_name: "mcp-server".to_string(),
    };
    init_logging(&log_config)?;

    info!("Starting Media Gateway MCP Server");

    // Load configuration
    let config = McpServerConfig::from_env();
    info!(
        host = %config.host,
        port = config.port,
        "Server configuration loaded"
    );

    // Connect to database
    info!("Connecting to database...");
    let db_config = DatabaseConfig {
        database_url: config.database_url.clone(),
        max_connections: 10,
        min_connections: 2,
        acquire_timeout: std::time::Duration::from_secs(30),
        idle_timeout: std::time::Duration::from_secs(600),
    };
    let db_pool = DatabasePool::new(&db_config)
        .await
        .map_err(|e| {
            error!(error = %e, "Failed to connect to database");
            anyhow::anyhow!("Database connection failed: {}", e)
        })?;
    info!("Database connection established");

    // Create server state
    let state = Arc::new(McpServerState::new(db_pool.pool().clone()));

    // Build router
    let app = Router::new()
        .route("/", post(handlers::handle_jsonrpc))
        .route("/health", get(handlers::health_check))
        .with_state(state)
        .layer(CorsLayer::permissive());

    // Start server
    let addr = config.address();
    info!(address = %addr, "Starting MCP server");

    let listener = tokio::net::TcpListener::bind(&addr)
        .await
        .map_err(|e| {
            error!(error = %e, address = %addr, "Failed to bind to address");
            anyhow::anyhow!("Failed to bind: {}", e)
        })?;

    info!("MCP server listening on {}", addr);
    info!("Health check endpoint: http://{}/health", addr);
    info!("JSON-RPC endpoint: http://{}/", addr);

    axum::serve(listener, app)
        .await
        .map_err(|e| {
            error!(error = %e, "Server error");
            anyhow::anyhow!("Server failed: {}", e)
        })?;

    Ok(())
}

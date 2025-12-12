mod api;
mod config;
mod logging;
mod provider;
mod proxy;
mod state;
mod transform;
mod types;

use crate::config::AppConfig;
use crate::logging::{start_logging_task, Logger};
use crate::state::{start_cleanup_task, AppState};
use axum::{
    routing::{any, get, patch, post},
    Router,
};
use std::net::SocketAddr;
use std::sync::Arc;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

#[tokio::main]
async fn main() {
    // Initialize tracing
    tracing_subscriber::registry()
        .with(tracing_subscriber::EnvFilter::new(
            std::env::var("RUST_LOG").unwrap_or_else(|_| "info".to_string()),
        ))
        .with(tracing_subscriber::fmt::layer())
        .init();

    // Load config
    let config = AppConfig::load().expect("Failed to load configuration");
    tracing::info!("Configuration loaded");
    tracing::info!(
        "Available providers: {:?}",
        config.providers.keys().collect::<Vec<_>>()
    );

    // Get server address from config
    let addr = SocketAddr::new(
        config.server.host.parse().expect("Invalid host address"),
        config.server.port,
    );

    // Initialize logging backends
    let logger = Arc::new(Logger::from_config(&config.logging));
    tracing::info!("Logging backends: {:?}", logger.sink_names());

    // Store cleanup interval before moving config
    let cleanup_interval = config.state.cleanup_interval_secs;

    // Initialize state
    let state = AppState::new(config);

    // Start logging task
    let log_rx = state.analysis_tx.subscribe();
    let _logging_shutdown = start_logging_task(logger, log_rx);

    // Start conversation cleanup task
    let _cleanup_shutdown = start_cleanup_task(state.conversations.clone(), cleanup_interval);

    // Build router
    let app = Router::new()
        // Health check
        .route("/health", get(api::health_handler))
        // Control API - Providers
        .route("/providers", get(api::providers_handler))
        // Control API - Conversations
        .route("/conversations", get(api::conversations_handler))
        .route("/conversations/stats", get(api::stats_handler))
        .route("/conversations/:conversation_id", get(api::conversation_handler))
        .route("/conversations/:conversation_id", patch(api::update_conversation_handler))
        // Control API - Injections
        .route("/inject/:conversation_id", post(api::inject_handler))
        .route("/clear/:conversation_id", post(api::clear_handler))
        // Control API - Logs
        .route("/logs/stream", get(api::logs_stream_handler))
        // Proxy API (capture all /v1 methods)
        .route("/v1/{*path}", any(proxy::proxy_handler))
        .with_state(state);

    // Run server
    tracing::info!("Listening on {}", addr);

    let listener = tokio::net::TcpListener::bind(addr).await.unwrap();
    axum::serve(listener, app).await.unwrap();
}

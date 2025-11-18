mod config;
mod proxy;
mod state;
mod api;

use crate::config::AppConfig;
use crate::state::AppState;
use axum::{
    routing::{get, post, any},
    Router,
};
use std::net::SocketAddr;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

#[tokio::main]
async fn main() {
    // Initialize logging
    tracing_subscriber::registry()
        .with(tracing_subscriber::EnvFilter::new(
            std::env::var("RUST_LOG").unwrap_or_else(|_| "info".to_string()),
        ))
        .with(tracing_subscriber::fmt::layer())
        .init();

    // Load config
    let config = AppConfig::load().expect("Failed to load configuration");
    tracing::info!("Configuration loaded");

    // Initialize state
    let state = AppState::new(config);

    // Build router
    let app = Router::new()
        // Control API
        .route("/inject/:conversation_id", post(api::inject_handler))
        .route("/clear/:conversation_id", post(api::clear_handler))
        .route("/logs/stream", get(api::logs_stream_handler))
        // Proxy API (capture all /v1 methods)
        .route("/v1/*path", any(proxy::proxy_handler))
        .with_state(state);

    // Run server
    let addr = SocketAddr::from(([127, 0, 0, 1], 3000));
    tracing::info!("Listening on {}", addr);
    
    let listener = tokio::net::TcpListener::bind(addr).await.unwrap();
    axum::serve(listener, app).await.unwrap();
}

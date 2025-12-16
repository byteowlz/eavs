mod api;
mod aws_sigv4;
mod capture;
mod cli;
mod config;
mod keys;
mod logging;
mod policy;
mod provider;
mod plugins;
mod proxy;
mod state;
mod transform;
mod types;
mod upstream;

#[cfg(all(test, feature = "integration"))]
mod integration_tests;

use crate::cli::{
    ensure_server_running, run_key_create, run_key_info, run_key_list, run_key_revoke,
    run_key_usage, run_service_logs, run_service_restart, run_service_start, run_service_status,
    run_service_stop, run_test_bench, run_test_chat, run_test_health, run_test_rate_limit, Cli,
    run_test_image, run_test_tool_call, CliConfig, Commands, EavsClient, KeyCommands,
    ServiceCommands, TestCommands,
};
use crate::config::AppConfig;
use crate::logging::{start_logging_task, Logger};
use crate::plugins::start_analysis_plugins;
use crate::state::{start_cleanup_task, AppState};
use axum::{
    routing::{any, delete, get, patch, post},
    Router,
};
use clap::Parser;
use std::net::SocketAddr;
use std::sync::Arc;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

#[tokio::main]
async fn main() {
    let cli = Cli::parse();

    match cli.command {
        Commands::Serve { host, port, config } => {
            run_server(host, port, config).await;
        }
        Commands::Service { action } => {
            if let Err(e) = run_service_command(action).await {
                eprintln!("Error: {}", e);
                std::process::exit(1);
            }
        }
        Commands::Key { action } => {
            if let Err(e) = run_key_command(action).await {
                eprintln!("Error: {}", e);
                std::process::exit(1);
            }
        }
        Commands::Test { action } => {
            if let Err(e) = run_test_command(action).await {
                eprintln!("Error: {}", e);
                std::process::exit(1);
            }
        }
    }
}

async fn run_server(host: Option<String>, port: Option<u16>, config_path: Option<String>) {
    // Initialize tracing
    tracing_subscriber::registry()
        .with(tracing_subscriber::EnvFilter::new(
            std::env::var("RUST_LOG").unwrap_or_else(|_| "info".to_string()),
        ))
        .with(tracing_subscriber::fmt::layer())
        .init();

    // Load config
    let mut config = if let Some(path) = config_path {
        AppConfig::load_from(&path).expect("Failed to load configuration")
    } else {
        AppConfig::load_or_init().expect("Failed to load configuration")
    };

    // Override with CLI args
    if let Some(h) = host {
        config.server.host = h;
    }
    if let Some(p) = port {
        config.server.port = p;
    }

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

    // Store cleanup interval and keys config before moving config
    let cleanup_interval = config.state.cleanup_interval_secs;
    let keys_enabled = config.keys.enabled;
    let capture_config = config.capture.clone();
    let server_port = config.server.port;

    // Initialize state
    let state = AppState::new(config);

    // Initialize key store if enabled
    if keys_enabled {
        if let Err(e) = state.init_key_store().await {
            tracing::error!("Failed to initialize key store: {}", e);
            // Continue without keys - they're optional
        }
    }

    // Start logging task
    let log_rx = state.analysis_tx.subscribe();
    let _logging_shutdown = start_logging_task(logger, log_rx);

    // Start analysis plugins (optional)
    let _plugin_tasks = start_analysis_plugins(state.clone());

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
        .route(
            "/conversations/:conversation_id",
            get(api::conversation_handler),
        )
        .route(
            "/conversations/:conversation_id",
            patch(api::update_conversation_handler),
        )
        // Control API - Injections
        .route("/inject/:conversation_id", post(api::inject_handler))
        .route("/clear/:conversation_id", post(api::clear_handler))
        // Control API - Logs
        .route("/logs/stream", get(api::logs_stream_handler))
        // Admin API - Virtual Keys
        .route("/admin/keys", post(api::create_key_handler))
        .route("/admin/keys", get(api::list_keys_handler))
        .route("/admin/keys/stats", get(api::key_stats_handler))
        .route("/admin/keys/:key_hash", get(api::get_key_handler))
        .route("/admin/keys/:key_hash", delete(api::delete_key_handler))
        .route("/admin/keys/:key_hash/usage", get(api::key_usage_handler))
        // Admin API - Pricing
        .route("/admin/pricing/update", post(api::update_pricing_handler))
        // Self-provisioning endpoint
        .route("/keys/provision", post(api::provision_key_handler))
        // WebSocket proxy (e.g. OpenAI Realtime)
        .route("/v1/realtime", get(proxy::ws_proxy_handler))
        // Proxy API (capture all /v1 methods)
        .route("/v1/*path", any(proxy::proxy_handler))
        .with_state(state);

    // Start mitmproxy capture if enabled
    let _capture_handle = if capture_config.enabled {
        match capture::start_capture_async(capture_config, server_port).await {
            Ok(handle) => {
                tracing::info!("Transparent capture mode: enabled (via mitmproxy)");
                Some(handle)
            }
            Err(e) => {
                tracing::error!("Failed to start capture mode: {}", e);
                tracing::warn!("Continuing without transparent capture...");
                None
            }
        }
    } else {
        None
    };

    // Run server
    tracing::info!("Listening on {}", addr);
    if keys_enabled {
        tracing::info!("Virtual API keys: enabled");
    }

    let listener = tokio::net::TcpListener::bind(addr).await.unwrap();
    axum::serve(listener, app).await.unwrap();

    // Clean up capture on shutdown (handle will be dropped automatically)
    if let Some(handle) = _capture_handle {
        handle.stop();
    }
}

async fn run_service_command(action: ServiceCommands) -> Result<(), cli::CliError> {
    match action {
        ServiceCommands::Start { port, config, wait } => {
            run_service_start(port, config, wait).await
        }
        ServiceCommands::Stop { port, force } => run_service_stop(port, force).await,
        ServiceCommands::Restart { port, config } => run_service_restart(port, config).await,
        ServiceCommands::Status { port, format } => run_service_status(port, format).await,
        ServiceCommands::Logs { lines, follow } => run_service_logs(lines, follow).await,
    }
}

async fn run_key_command(action: KeyCommands) -> Result<(), cli::CliError> {
    let client = EavsClient::new(CliConfig::default());

    match action {
        KeyCommands::Create {
            name,
            models,
            blocked_models,
            providers,
            rpm,
            tpm,
            rpd,
            budget,
            expires,
            format,
        } => {
            run_key_create(
                &client,
                name,
                models,
                blocked_models,
                providers,
                rpm,
                tpm,
                rpd,
                budget,
                expires,
                format,
            )
            .await
        }
        KeyCommands::List { all, format } => run_key_list(&client, all, format).await,
        KeyCommands::Info { key, format } => run_key_info(&client, key, format).await,
        KeyCommands::Revoke { key, yes } => run_key_revoke(&client, key, yes).await,
        KeyCommands::Usage { key, days, format } => run_key_usage(&client, key, days, format).await,
    }
}

async fn run_test_command(action: TestCommands) -> Result<(), cli::CliError> {
    match action {
        TestCommands::Chat {
            message,
            provider,
            model,
            key,
            stream,
            url,
            config,
        } => {
            // Ensure server is running, auto-start if needed
            let server = ensure_server_running(&url, config.as_deref()).await?;
            let client = EavsClient::with_url(server.url);
            run_test_chat(&client, message, model, provider, key, stream).await
        }
        TestCommands::Image {
            image,
            prompt,
            provider,
            model,
            key,
            stream,
            url,
            config,
        } => {
            let server = ensure_server_running(&url, config.as_deref()).await?;
            let client = EavsClient::with_url(server.url);
            run_test_image(&client, image, prompt, model, provider, key, stream).await
        }
        TestCommands::ToolCall {
            prompt,
            provider,
            model,
            key,
            stream,
            url,
            config,
        } => {
            let server = ensure_server_running(&url, config.as_deref()).await?;
            let client = EavsClient::with_url(server.url);
            run_test_tool_call(&client, prompt, model, provider, key, stream).await
        }
        TestCommands::RateLimit {
            count,
            key,
            url,
            config,
        } => {
            // Ensure server is running, auto-start if needed
            let server = ensure_server_running(&url, config.as_deref()).await?;
            let client = EavsClient::with_url(server.url);
            run_test_rate_limit(&client, count, key).await
        }
        TestCommands::Bench {
            count,
            provider,
            model,
            key,
            compare_direct,
            direct_url,
            direct_key,
            stream,
            concurrent,
            duration,
            url,
            format,
            config,
        } => {
            // Ensure server is running, auto-start if needed
            let server = ensure_server_running(&url, config.as_deref()).await?;
            run_test_bench(
                count,
                provider,
                model,
                key,
                compare_direct,
                direct_url,
                direct_key,
                stream,
                concurrent,
                duration,
                server.url,
                format,
            )
            .await
        }
        TestCommands::Health { url, format, config } => {
            // Ensure server is running, auto-start if needed
            let server = ensure_server_running(&url, config.as_deref()).await?;
            let client = EavsClient::with_url(server.url);
            run_test_health(&client, format).await
        }
    }
}

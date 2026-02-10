mod api;
mod aws_credentials;
mod aws_sigv4;
mod capture;
mod cli;
mod config;
mod keys;
mod logging;
mod oauth;
mod plugins;
mod policy;
mod provider;
mod proxy;
mod runtime_state;
mod state;
mod transform;
mod transform_plugins;
mod types;
mod upstream;

#[cfg(all(test, feature = "integration"))]
mod integration_tests;

use crate::cli::{
    ensure_server_running, run_auth_logout, run_auth_status, run_login, run_secret_delete,
    run_secret_get, run_secret_list, run_secret_set, run_service_logs, run_service_restart,
    run_service_start, run_service_status, run_service_stop, run_test_bench, run_test_chat,
    run_test_health, run_test_image, run_test_oauth, run_test_rate_limit, run_test_routing,
    run_test_tool_call, AuthCommands, Cli, Commands, EavsClient, KeyCommands, ProviderCommands,
    SecretCommands, ServiceCommands, TestCommands,
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
        Commands::Provider { action } => {
            if let Err(e) = run_provider_command(action) {
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
        Commands::Login {
            provider,
            user,
            callback_port,
            config,
        } => {
            if let Err(e) = run_login(provider, user, callback_port, config.as_deref()).await {
                eprintln!("Error: {}", e);
                std::process::exit(1);
            }
        }
        Commands::Auth { action } => {
            if let Err(e) = run_auth_command(action).await {
                eprintln!("Error: {}", e);
                std::process::exit(1);
            }
        }
        Commands::Secret { action } => {
            if let Err(e) = run_secret_command(action) {
                eprintln!("Error: {}", e);
                std::process::exit(1);
            }
        }
    }
}

fn run_secret_command(action: SecretCommands) -> Result<(), cli::CliError> {
    match action {
        SecretCommands::Set { account, value } => {
            run_secret_set(&account, value.as_deref())
        }
        SecretCommands::Get { account, reveal } => run_secret_get(&account, reveal),
        SecretCommands::Delete { account, yes } => run_secret_delete(&account, yes),
        SecretCommands::List { config, check } => run_secret_list(config.as_deref(), check),
    }
}

async fn run_auth_command(action: AuthCommands) -> Result<(), cli::CliError> {
    match action {
        AuthCommands::Status { user, config } => run_auth_status(&user, config.as_deref()).await,
        AuthCommands::Logout {
            provider,
            user,
            yes,
            config,
        } => run_auth_logout(&provider, &user, yes, config.as_deref()).await,
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
        if let Err(e) = state.init_oauth_store().await {
            tracing::error!("Failed to initialize OAuth store: {}", e);
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
        // OAuth API
        .route("/auth/login/:provider", post(api::oauth_login_handler))
        .route("/auth/callback", post(api::oauth_callback_handler))
        .route("/auth/poll/:provider", post(api::oauth_poll_handler))
        .route("/auth/code/:provider", post(api::oauth_code_handler))
        .route("/auth/status/:user_id", get(api::oauth_status_handler))
        .route(
            "/auth/:user_id/:provider",
            delete(api::oauth_delete_handler),
        )
        // Self-provisioning endpoint
        .route("/keys/provision", post(api::provision_key_handler))
        // WebSocket proxy (e.g. OpenAI Realtime) - default route
        .route("/v1/realtime", get(proxy::ws_proxy_handler))
        // Provider-prefixed WebSocket proxy (e.g. /openai/v1/realtime)
        .route(
            "/:provider/v1/realtime",
            get(proxy::provider_ws_proxy_handler),
        )
        // Provider-prefixed proxy routes (e.g. /openai/v1/chat/completions)
        // This allows explicit provider selection via URL path
        .route("/:provider/v1/*path", any(proxy::provider_proxy_handler))
        // Default proxy route with X-Provider header or auto-detection
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

/// Load config from file path or default location
fn load_config(config_path: Option<&str>) -> crate::config::AppConfig {
    if let Some(path) = config_path {
        crate::config::AppConfig::load_from(path).unwrap_or_else(|e| {
            tracing::warn!("Failed to load config from {}: {}, using defaults", path, e);
            crate::config::AppConfig::with_defaults()
        })
    } else {
        crate::config::AppConfig::load().unwrap_or_else(|e| {
            tracing::debug!("No config file found: {}, using defaults", e);
            crate::config::AppConfig::with_defaults()
        })
    }
}

async fn run_key_command(action: KeyCommands) -> Result<(), cli::CliError> {
    use crate::cli::{
        run_key_bind_direct, run_key_create_direct, run_key_info_direct, run_key_list_direct,
        run_key_revoke_direct, run_key_usage_direct,
    };
    use crate::keys::KeyStore;

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
            url: _, // Not needed for direct DB access
            config,
        } => {
            let app_config = load_config(config.as_deref());
            let db_path = app_config.keys.resolved_database_path();
            let store = KeyStore::new(&db_path)
                .await
                .map_err(|e| cli::CliError::Other(format!("Failed to open key database: {}", e)))?;
            run_key_create_direct(
                &store,
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
        KeyCommands::List {
            all,
            format,
            url: _,
            config,
        } => {
            let app_config = load_config(config.as_deref());
            let db_path = app_config.keys.resolved_database_path();
            let store = KeyStore::new(&db_path)
                .await
                .map_err(|e| cli::CliError::Other(format!("Failed to open key database: {}", e)))?;
            run_key_list_direct(&store, all, format).await
        }
        KeyCommands::Info {
            key,
            format,
            url: _,
            config,
        } => {
            let app_config = load_config(config.as_deref());
            let db_path = app_config.keys.resolved_database_path();
            let store = KeyStore::new(&db_path)
                .await
                .map_err(|e| cli::CliError::Other(format!("Failed to open key database: {}", e)))?;
            run_key_info_direct(&store, &key, format).await
        }
        KeyCommands::Revoke {
            key,
            yes,
            url: _,
            config,
        } => {
            let app_config = load_config(config.as_deref());
            let db_path = app_config.keys.resolved_database_path();
            let store = KeyStore::new(&db_path)
                .await
                .map_err(|e| cli::CliError::Other(format!("Failed to open key database: {}", e)))?;
            run_key_revoke_direct(&store, &key, yes).await
        }
        KeyCommands::Usage {
            key,
            days,
            format,
            url: _,
            config,
        } => {
            let app_config = load_config(config.as_deref());
            let db_path = app_config.keys.resolved_database_path();
            let store = KeyStore::new(&db_path)
                .await
                .map_err(|e| cli::CliError::Other(format!("Failed to open key database: {}", e)))?;
            run_key_usage_direct(&store, &key, days, format).await
        }
        KeyCommands::Bind {
            key,
            oauth_user,
            clear,
            format,
            config,
        } => {
            let app_config = load_config(config.as_deref());
            let db_path = app_config.keys.resolved_database_path();
            let store = KeyStore::new(&db_path)
                .await
                .map_err(|e| cli::CliError::Other(format!("Failed to open key database: {}", e)))?;
            let oauth_user = if clear { None } else { oauth_user };
            if !clear && oauth_user.is_none() {
                return Err(cli::CliError::Other(
                    "Provide --oauth-user or use --clear".to_string(),
                ));
            }
            run_key_bind_direct(&store, &key, oauth_user, format).await
        }
    }
}

fn run_provider_command(action: ProviderCommands) -> Result<(), cli::CliError> {
    use crate::cli::{
        run_provider_clear, run_provider_current, run_provider_list, run_provider_use,
    };

    match action {
        ProviderCommands::Current => run_provider_current(),
        ProviderCommands::Use { provider, config } => {
            run_provider_use(&provider, config.as_deref())
        }
        ProviderCommands::Clear => run_provider_clear(),
        ProviderCommands::List { config } => run_provider_list(config.as_deref()),
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
        TestCommands::Health {
            url,
            format,
            config,
        } => {
            // Ensure server is running, auto-start if needed
            let server = ensure_server_running(&url, config.as_deref()).await?;
            let client = EavsClient::with_url(server.url);
            run_test_health(&client, format).await
        }
        TestCommands::Routing {
            provider,
            model,
            url,
            format,
            config,
        } => {
            // Ensure server is running, auto-start if needed
            let server = ensure_server_running(&url, config.as_deref()).await?;
            run_test_routing(&server.url, &provider, model, format).await
        }
        TestCommands::Oauth {
            user,
            provider,
            model,
            message,
            stream,
            url,
            format,
            config,
        } => {
            // Ensure server is running, auto-start if needed
            let server = ensure_server_running(&url, config.as_deref()).await?;
            run_test_oauth(
                &user,
                provider,
                model,
                message,
                stream,
                &server.url,
                format,
                config.as_deref(),
            )
            .await
        }
    }
}

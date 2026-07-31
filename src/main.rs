mod api;
mod aws_credentials;
mod aws_sigv4;
mod capture;
mod cli;
mod config;
mod egress;
mod export;
mod keys;
mod logging;
mod mock_provider;
mod model_catalog;
mod model_defaults;
mod model_discovery;
mod network_acl;
mod oauth;
mod paths;
mod plugins;
mod policy;
mod provider;
mod provider_probe;
mod provider_store;
mod provider_templates;
mod proxy;
mod runtime_state;
mod setup;
mod state;
mod transform;
mod transform_plugins;
mod types;
mod upstream;
mod upstream_quota;

#[cfg(all(test, feature = "integration"))]
mod integration_tests;

use crate::cli::{
    ensure_server_running, run_auth_logout, run_auth_status, run_login, run_secret_delete,
    run_secret_get, run_secret_list, run_secret_set, run_service_logs, run_service_restart,
    run_service_start, run_service_status, run_service_stop, run_test_bench, run_test_chat,
    run_test_health, run_test_image, run_test_oauth, run_test_rate_limit, run_test_routing,
    run_test_tool_call, AuthCommands, Cli, Commands, EavsClient, KeyCommands, ModelCommands,
    ProviderCommands, SecretCommands, ServiceCommands, SetupCommands, TestCommands,
};
use crate::config::AppConfig;
use crate::logging::{start_logging_task, Logger};
use crate::plugins::start_analysis_plugins;
use crate::state::{start_cleanup_task, AppState};
use axum::{
    routing::{any, delete, get, patch, post, put},
    Router,
};
use clap::Parser;
use std::collections::HashMap;
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
        Commands::Cost { action } => {
            if let Err(e) = run_cost_command(action).await {
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
        Commands::Setup { action } => {
            if let Err(e) = run_setup_command(action).await {
                eprintln!("Error: {}", e);
                std::process::exit(1);
            }
        }
        Commands::Models { action } => {
            if let Err(e) = run_models_command(action).await {
                eprintln!("Error: {}", e);
                std::process::exit(1);
            }
        }
        Commands::Quotas { json } => {
            if let Err(e) = run_quotas_command(json).await {
                eprintln!("Error: {}", e);
                std::process::exit(1);
            }
        }
    }
}

async fn run_quotas_command(json_output: bool) -> Result<(), cli::CliError> {
    let config = cli::CliConfig::default();
    let url = format!("{}/admin/quotas", config.server_url);

    // Try env var first, then read from config file
    let master_key = std::env::var("EAVS_MASTER_KEY")
        .ok()
        .or_else(|| {
            let app_config = crate::config::AppConfig::load().ok()?;
            app_config.keys.resolved_master_key()
        })
        .ok_or_else(|| {
            cli::CliError::Other(
                "EAVS_MASTER_KEY not set and keys.master_key not found in config".to_string(),
            )
        })?;

    let client = reqwest::Client::new();
    let resp = client
        .get(&url)
        .header("Authorization", format!("Bearer {}", master_key))
        .send()
        .await
        .map_err(|e| cli::CliError::Other(format!("Failed to fetch quotas: {}", e)))?;

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        return Err(cli::CliError::Other(format!(
            "Server returned {}: {}",
            status, body
        )));
    }

    let quotas: Vec<upstream_quota::QuotaSnapshot> = resp
        .json()
        .await
        .map_err(|e| cli::CliError::Other(format!("Failed to parse response: {}", e)))?;

    if quotas.is_empty() {
        eprintln!("No upstream quotas observed yet.");
        return Ok(());
    }

    if json_output {
        println!(
            "{}",
            serde_json::to_string_pretty(&quotas).unwrap_or_else(|_| "[]".to_string())
        );
    } else {
        println!(
            "{:<20} {:<12} {:>10} {:>10} {:>12} {:>12} {:>8}",
            "PROVIDER", "ACCOUNT", "REQ LEFT", "REQ LIMIT", "TOK LEFT", "TOK LIMIT", "AGE(s)"
        );
        println!("{}", "-".repeat(90));
        for q in &quotas {
            println!(
                "{:<20} {:<12} {:>10} {:>10} {:>12} {:>12} {:>8.0}",
                q.provider,
                q.account,
                q.requests_remaining
                    .map(|v| v.to_string())
                    .unwrap_or_else(|| "-".to_string()),
                q.requests_limit
                    .map(|v| v.to_string())
                    .unwrap_or_else(|| "-".to_string()),
                q.tokens_remaining
                    .map(|v| v.to_string())
                    .unwrap_or_else(|| "-".to_string()),
                q.tokens_limit
                    .map(|v| v.to_string())
                    .unwrap_or_else(|| "-".to_string()),
                q.age_secs,
            );
        }
    }
    Ok(())
}

fn run_secret_command(action: SecretCommands) -> Result<(), cli::CliError> {
    match action {
        SecretCommands::Set { account, value } => run_secret_set(&account, value.as_deref()),
        SecretCommands::Get { account, reveal } => run_secret_get(&account, reveal),
        SecretCommands::Delete { account, yes } => run_secret_delete(&account, yes),
        SecretCommands::List { config, check, all } => {
            run_secret_list(config.as_deref(), check, all)
        }
    }
}

async fn run_models_command(action: ModelCommands) -> Result<(), cli::CliError> {
    use crate::model_catalog::ModelCatalog;

    match action {
        ModelCommands::List { provider, json } => {
            let catalog = ModelCatalog::load()
                .await
                .map_err(|e| cli::CliError::Other(format!("Failed to load catalog: {}", e)))?;

            let models = catalog.catalog_models(&provider);
            if models.is_empty() {
                eprintln!("No models found for provider '{}'", provider);
                eprintln!("Available providers: {}", catalog.provider_ids().join(", "));
                return Ok(());
            }

            if json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&models).unwrap_or_else(|_| "[]".to_string())
                );
            } else {
                println!(
                    "{:<40} {:<30} {:>6} {:>10} {:>10} {:>8} {:>8}",
                    "ID", "NAME", "REASON", "CONTEXT", "OUTPUT", "$/M IN", "$/M OUT"
                );
                println!("{}", "-".repeat(115));
                for m in &models {
                    println!(
                        "{:<40} {:<30} {:>6} {:>10} {:>10} {:>8.2} {:>8.2}",
                        m.id,
                        truncate(&m.name, 29),
                        if m.reasoning { "yes" } else { "" },
                        format_num(m.limit.context),
                        format_num(m.limit.output),
                        m.cost.input,
                        m.cost.output,
                    );
                }
                println!("\n{} models", models.len());
            }
        }
        ModelCommands::Configured {
            provider,
            json,
            discover,
        } => {
            use crate::config::AppConfig;
            use crate::model_discovery::discover_provider_models;

            let config = AppConfig::load()
                .map_err(|e| cli::CliError::Other(format!("Failed to load config: {}", e)))?;

            let providers_to_show: Vec<(String, crate::config::ProviderConfig)> =
                if let Some(p) = provider {
                    match config.providers.get(&p) {
                        Some(cfg) => vec![(p.clone(), cfg.clone())],
                        None => {
                            eprintln!("Provider '{}' not found in config", p);
                            eprintln!(
                                "Available: {}",
                                config
                                    .providers
                                    .keys()
                                    .cloned()
                                    .collect::<Vec<_>>()
                                    .join(", ")
                            );
                            return Ok(());
                        }
                    }
                } else {
                    config
                        .providers
                        .iter()
                        .map(|(k, v)| (k.clone(), v.clone()))
                        .collect()
                };

            // Discover from endpoints if requested
            let discovered: HashMap<String, Vec<crate::model_discovery::DiscoveredModel>> =
                if discover {
                    let mut results = HashMap::new();
                    for (name, cfg) in &providers_to_show {
                        match discover_provider_models(name, cfg).await {
                            Ok(models) => {
                                if !models.is_empty() {
                                    results.insert(name.clone(), models);
                                }
                            }
                            Err(e) => {
                                eprintln!("  Note: Could not discover models from {}: {}", name, e);
                            }
                        }
                    }
                    results
                } else {
                    HashMap::new()
                };

            if json {
                let result: serde_json::Value = providers_to_show
                    .into_iter()
                    .map(|(name, cfg)| {
                        let models: Vec<_> = cfg
                            .models
                            .iter()
                            .map(|m| {
                                serde_json::json!({
                                    "id": m.id,
                                    "name": m.name,
                                    "reasoning": m.reasoning,
                                    "context_window": m.context_window,
                                    "max_tokens": m.max_tokens,
                                    "cost": m.cost,
                                    "input": m.input,
                                })
                            })
                            .collect();
                        let discovered_models = discovered
                            .get(&name)
                            .map(|d| {
                                d.iter()
                                    .map(|m| {
                                        serde_json::json!({
                                            "id": &m.id,
                                            "name": &m.name,
                                            "context_window": m.context_window,
                                            "source": "discovered",
                                        })
                                    })
                                    .collect::<Vec<_>>()
                            })
                            .unwrap_or_default();
                        (
                            name,
                            serde_json::json!({
                                "type": cfg.type_,
                                "models": models,
                                "discovered": discovered_models,
                            }),
                        )
                    })
                    .collect::<serde_json::Map<String, serde_json::Value>>()
                    .into();
                println!(
                    "{}",
                    serde_json::to_string_pretty(&result).unwrap_or_else(|_| "{}".to_string())
                );
            } else {
                for (name, cfg) in &providers_to_show {
                    println!("\n{} [{}]", name, cfg.type_);
                    println!("{}", "-".repeat(60));

                    // Show configured models
                    if cfg.models.is_empty() {
                        println!("  Configured: (none - uses full catalog)");
                    } else {
                        println!("  Configured models:");
                        println!(
                            "    {:<30} {:>10} {:>10} {:>8} {:>8}",
                            "MODEL", "CONTEXT", "OUTPUT", "$/M IN", "$/M OUT"
                        );
                        for m in &cfg.models {
                            println!(
                                "    {:<30} {:>10} {:>10} {:>8.2} {:>8.2}",
                                if m.name.is_empty() { &m.id } else { &m.name },
                                format_num(m.context_window),
                                format_num(m.max_tokens),
                                m.cost.input,
                                m.cost.output,
                            );
                        }
                    }

                    // Show discovered models
                    if let Some(disc) = discovered.get(name) {
                        println!("  Discovered from endpoint:");
                        for m in disc {
                            let name_str = m.name.as_deref().unwrap_or(&m.id);
                            let ctx_str = m
                                .context_window
                                .map(|c| format!("{} ctx", format_num(c)))
                                .unwrap_or_default();
                            println!("    - {} {}", name_str, ctx_str);
                        }
                    }
                }
                let total_configured: usize = providers_to_show
                    .iter()
                    .map(|(_, cfg)| cfg.models.len())
                    .sum();
                let total_discovered: usize = discovered.values().map(|v| v.len()).sum();
                println!(
                    "\nTotal: {} providers, {} configured, {} discovered",
                    providers_to_show.len(),
                    total_configured,
                    total_discovered
                );
                if !discover {
                    println!("\nTip: Use --discover to probe endpoints for available models");
                }
            }
        }
        ModelCommands::Search { query, json } => {
            let catalog = ModelCatalog::load()
                .await
                .map_err(|e| cli::CliError::Other(format!("Failed to load catalog: {}", e)))?;

            let query_lower = query.to_lowercase();
            let mut results: Vec<(String, crate::model_catalog::CatalogModel)> = Vec::new();

            for provider_id in catalog.provider_ids() {
                for model in catalog.catalog_models(provider_id) {
                    if model.id.to_lowercase().contains(&query_lower)
                        || model.name.to_lowercase().contains(&query_lower)
                    {
                        results.push((provider_id.to_string(), model.clone()));
                    }
                }
            }

            if results.is_empty() {
                eprintln!("No models matching '{}'", query);
                return Ok(());
            }

            results.sort_by(|a, b| a.1.id.cmp(&b.1.id));

            if json {
                let json_results: Vec<serde_json::Value> = results
                    .iter()
                    .map(|(p, m)| {
                        let mut v = serde_json::to_value(m).unwrap_or_default();
                        v.as_object_mut()
                            .map(|o| o.insert("provider".to_string(), serde_json::json!(p)));
                        v
                    })
                    .collect();
                println!(
                    "{}",
                    serde_json::to_string_pretty(&json_results)
                        .unwrap_or_else(|_| "[]".to_string())
                );
            } else {
                println!(
                    "{:<20} {:<40} {:<30} {:>6} {:>8} {:>8}",
                    "PROVIDER", "ID", "NAME", "REASON", "$/M IN", "$/M OUT"
                );
                println!("{}", "-".repeat(115));
                for (provider, m) in &results {
                    println!(
                        "{:<20} {:<40} {:<30} {:>6} {:>8.2} {:>8.2}",
                        provider,
                        m.id,
                        truncate(&m.name, 29),
                        if m.reasoning { "yes" } else { "" },
                        m.cost.input,
                        m.cost.output,
                    );
                }
                println!("\n{} matches", results.len());
            }
        }
        ModelCommands::Update => {
            println!("Fetching model catalog from models.dev...");
            let catalog = ModelCatalog::refresh()
                .await
                .map_err(|e| cli::CliError::Other(format!("Failed to refresh: {}", e)))?;
            println!(
                "Cached {} models across {} providers",
                catalog.total_models(),
                catalog.provider_ids().len()
            );
        }
        ModelCommands::Export {
            adapter: adapter_name,
            base_url,
            api_key,
            config: config_path,
            merge: merge_path,
        } => {
            use crate::api::{pi_api_for_provider, ProviderDetail};
            use crate::provider::ProviderType;

            // No adapter name -> list available adapters
            let adapter_name = match adapter_name {
                Some(name) => name,
                None => {
                    match export::list_adapters() {
                        Ok(adapters) if !adapters.is_empty() => {
                            println!("Available export adapters:");
                            for name in &adapters {
                                // Try to get info for each
                                if let Ok(info) = export::adapter_info(name) {
                                    let _display = info["displayName"].as_str().unwrap_or(name);
                                    let desc = info["description"].as_str().unwrap_or("");
                                    let file = info["outputFile"].as_str().unwrap_or("");
                                    println!("  {:<12} {} ({})", name, desc, file);
                                } else {
                                    println!("  {}", name);
                                }
                            }
                            println!(
                                "\nUsage: eavs models export <adapter> [--api-key KEY] [--base-url URL]"
                            );
                        }
                        Ok(_) => {
                            eprintln!("No export adapters found.");
                            if let Err(e) = export::adapters_dir() {
                                eprintln!("{}", e);
                            }
                        }
                        Err(e) => {
                            eprintln!("Error discovering adapters: {}", e);
                        }
                    }
                    return Ok(());
                }
            };

            // Load eavs config to find configured providers
            let app_config = if let Some(ref path) = config_path {
                crate::config::AppConfig::load_from(path)
            } else {
                crate::config::AppConfig::load()
            }
            .map_err(|e| cli::CliError::Other(format!("Failed to load config: {}", e)))?;

            // Load catalog for enriching shortlists
            let catalog = ModelCatalog::load().await.ok();

            // Resolve base URL from config or override
            let eavs_base = base_url.unwrap_or_else(|| {
                format!(
                    "http://{}:{}",
                    app_config.server.host, app_config.server.port
                )
            });

            let key = api_key.unwrap_or_else(|| "EAVS_API_KEY".to_string());

            // Build provider details (same logic as the /providers/detail API endpoint)
            let mut details: Vec<ProviderDetail> = Vec::new();
            for (name, provider_config) in &app_config.providers {
                let provider_type = ProviderType::from_str(&provider_config.type_);
                let has_api_key = !provider_config.api_key.is_empty();

                let models = if !provider_config.models.is_empty() {
                    provider_config.models.clone()
                } else if let Some(ref cat) = catalog {
                    let catalog_id =
                        crate::model_catalog::eavs_to_catalog_id(name, &provider_config.type_);
                    cat.models_for_provider(catalog_id, &provider_config.models)
                } else {
                    Vec::new()
                };

                let mut headers = std::collections::HashMap::new();
                for k in provider_config.headers.keys() {
                    headers.insert(k.clone(), "EAVS_API_KEY".to_string());
                }

                details.push(ProviderDetail {
                    name: name.clone(),
                    type_: provider_config.type_.clone(),
                    pi_api: pi_api_for_provider(&provider_type),
                    oauth: api::provider_uses_oauth(&provider_type),
                    has_api_key,
                    headers,
                    api_version: provider_config.api_version.clone(),
                    compat: api::provider_compat_json(provider_config),
                    models,
                });
            }

            // If --merge is specified, read the existing file and use merge mode
            let existing =
                if let Some(ref path) = merge_path {
                    Some(std::fs::read_to_string(path).map_err(|e| {
                        cli::CliError::Other(format!("Failed to read {}: {}", path, e))
                    })?)
                } else {
                    None
                };

            let output = export::run_adapter(
                &adapter_name,
                &details,
                &eavs_base,
                &key,
                existing.as_deref(),
            )
            .map_err(|e| cli::CliError::Other(format!("{:#}", e)))?;

            println!(
                "{}",
                serde_json::to_string_pretty(&output).unwrap_or_else(|_| "{}".to_string())
            );
        }
        ModelCommands::Stats => {
            let catalog = ModelCatalog::load()
                .await
                .map_err(|e| cli::CliError::Other(format!("Failed to load catalog: {}", e)))?;

            if catalog.is_empty() {
                println!("Catalog is empty. Run `eavs models update` to fetch.");
                return Ok(());
            }

            println!("Model catalog (models.dev):");
            println!("  Total models: {}", catalog.total_models());
            println!("  Providers: {}", catalog.provider_ids().len());
            println!();

            let mut providers: Vec<(&str, usize)> = catalog
                .provider_ids()
                .iter()
                .map(|&id| (id, catalog.catalog_models(id).len()))
                .collect();
            providers.sort_by_key(|p| std::cmp::Reverse(p.1));

            println!("{:<30} {:>8}", "PROVIDER", "MODELS");
            println!("{}", "-".repeat(40));
            for (id, count) in &providers {
                println!("{:<30} {:>8}", id, count);
            }
        }
    }
    Ok(())
}

fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else {
        format!("{}...", &s[..max.saturating_sub(3)])
    }
}

fn format_num(n: u64) -> String {
    if n == 0 {
        return "-".to_string();
    }
    if n >= 1_000_000 {
        format!("{}M", n / 1_000_000)
    } else if n >= 1_000 {
        format!("{}K", n / 1_000)
    } else {
        n.to_string()
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
    let egress_config = config.egress.clone();
    let egress_network = config.network.clone();

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

    // Initialize provider store (for admin CRUD API)
    if let Err(e) = state.init_provider_store().await {
        tracing::error!("Failed to initialize provider store: {}", e);
        // Continue without provider store - it's optional
    }

    // Load model catalog from models.dev (async, non-blocking)
    state.init_model_catalog().await;

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
        .route("/defaults", get(api::defaults_handler))
        // Control API - Providers
        .route("/providers", get(api::providers_handler))
        .route("/providers/detail", get(api::providers_detail_handler))
        .route("/providers/templates", get(api::provider_templates_handler))
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
        .route("/admin/usage/by-owner", get(api::owner_usage_handler))
        .route(
            "/admin/keys/:key_hash/owner",
            put(api::update_key_owner_handler),
        )
        // Admin API - Providers
        .route("/admin/providers", post(api::upsert_provider_handler))
        .route("/admin/providers", get(api::list_providers_handler))
        .route(
            "/admin/providers/from-template",
            post(api::provider_from_template_handler),
        )
        .route("/admin/providers/probe", post(api::probe_provider_handler))
        .route("/admin/providers/:name", get(api::get_provider_handler))
        .route(
            "/admin/providers/:name",
            delete(api::delete_provider_handler),
        )
        .route(
            "/admin/providers/:name/models",
            post(api::add_model_handler),
        )
        .route(
            "/admin/providers/:name/models",
            get(api::get_models_handler),
        )
        .route(
            "/admin/providers/:name/models/:model_id",
            delete(api::remove_model_handler),
        )
        // Admin API - Pricing
        .route("/admin/pricing/update", post(api::update_pricing_handler))
        .route("/admin/quotas", get(api::upstream_quotas_handler))
        .route("/catalog/lookup", get(api::catalog_lookup_handler))
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
        // Codex Responses proxy - default route
        // GET is the WebSocket upgrade, POST the SSE transport (pi falls back
        // to SSE when the WebSocket is unavailable or connection-limited)
        .route(
            "/v1/codex/responses",
            get(proxy::codex_ws_handler).post(proxy::proxy_handler),
        )
        // Provider-prefixed Codex Responses proxy
        .route(
            "/:provider/v1/codex/responses",
            get(proxy::provider_codex_ws_handler).post(proxy::provider_codex_sse_handler),
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

    // Transparent egress proxy for the oqto sandbox netns (no-op unless enabled).
    egress::spawn(egress_config, egress_network);

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
            owner,
            oauth_user,
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
                owner,
                oauth_user,
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

async fn run_cost_command(action: cli::CostCommands) -> Result<(), cli::CliError> {
    use crate::cli::{run_cost_by_key_direct, run_cost_by_owner_direct, CostCommands};
    use crate::keys::KeyStore;

    let open_store = |config: Option<String>| async move {
        let app_config = load_config(config.as_deref());
        let db_path = app_config.keys.resolved_database_path();
        KeyStore::new(&db_path)
            .await
            .map_err(|e| cli::CliError::Other(format!("Failed to open key database: {}", e)))
    };

    match action {
        CostCommands::ByOwner {
            owner,
            days,
            breakdown,
            format,
            config,
        } => {
            let store = open_store(config).await?;
            run_cost_by_owner_direct(
                &store,
                owner.as_deref(),
                breakdown.as_deref(),
                days,
                format,
            )
            .await
        }
        CostCommands::ByKey {
            days,
            format,
            config,
        } => {
            let store = open_store(config).await?;
            run_cost_by_key_direct(&store, days, format).await
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
            key,
            url,
            format,
            config,
        } => {
            // Ensure server is running, auto-start if needed
            let server = ensure_server_running(&url, config.as_deref()).await?;
            run_test_routing(&server.url, &provider, model, key, format).await
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

async fn run_setup_command(action: SetupCommands) -> Result<(), cli::CliError> {
    match action {
        SetupCommands::Add {
            config,
            batch,
            env_file,
            import,
        } => {
            setup::run_setup_add(
                config.as_deref(),
                batch,
                env_file.as_deref(),
                import.as_deref(),
            )
            .await
        }
        SetupCommands::Test {
            provider,
            model,
            message,
            config,
            format,
        } => {
            setup::run_setup_test(
                &provider,
                model.as_deref(),
                &message,
                config.as_deref(),
                &format,
            )
            .await
        }
        SetupCommands::TestAll {
            config,
            model,
            format,
        } => setup::run_setup_test_all(config.as_deref(), model.as_deref(), &format).await,
        SetupCommands::Show {
            provider,
            config,
            reveal,
        } => setup::run_setup_show(&provider, config.as_deref(), reveal),
    }
}

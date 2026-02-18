//! Interactive setup wizard for adding and testing provider configurations.
//!
//! Provides:
//! - `eavs setup add`      -- guided flow for adding a new provider
//! - `eavs setup test`     -- direct API test of any configured provider (no server needed)
//! - `eavs setup test-all` -- batch-test every provider in the config
//! - `eavs setup show`     -- display the effective (resolved) config for a provider

use crate::cli::{CliError, OutputFormat};
use crate::config::{AppConfig, ProviderConfig};
use crate::provider::ProviderType;
use dialoguer::{Confirm, Select};
// Note: we intentionally avoid dialoguer::Input for text fields because it
// uses raw terminal mode which breaks bracketed paste (duplicating text).
// Plain stdin line reads via prompt_input() handle paste correctly.
use std::collections::HashMap;
use std::io::{self, BufRead, IsTerminal, Write};
use std::path::PathBuf;

/// Outcome of a single provider test.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TestOutcome {
    /// The API call succeeded.
    Passed,
    /// The API call failed, but the user chose to continue (interactive only).
    FailedContinue,
}

// =============================================================================
// Provider type choices for the interactive menu
// =============================================================================

/// (display_name, type_string, description)
const PROVIDER_CHOICES: &[(&str, &str, &str)] = &[
    ("OpenAI", "openai", "GPT-4o, GPT-4, o1, o3, etc."),
    ("Anthropic", "anthropic", "Claude Opus, Sonnet, Haiku, etc."),
    ("Google Gemini", "google", "Gemini Pro, Flash, Ultra, etc."),
    ("Mistral", "mistral", "Mistral Large, Medium, Small, etc."),
    ("Groq", "groq", "Fast inference (Llama, Mixtral, etc.)"),
    ("Cerebras", "cerebras", "Fast inference (Llama, etc.)"),
    ("xAI", "xai", "Grok models"),
    ("OpenRouter", "openrouter", "Unified API for many models"),
    (
        "Azure AI Foundry (OpenAI models)",
        "openai",
        "GPT-4o, o1, o3 hosted on Azure Foundry",
    ),
    (
        "Azure AI Foundry (Anthropic models)",
        "anthropic",
        "Claude hosted on Azure Foundry",
    ),
    (
        "Azure AI Foundry (Other models)",
        "openai-compatible",
        "DeepSeek, Llama, Phi, etc. on Azure Foundry",
    ),
    ("Azure OpenAI", "azure", "Azure OpenAI Service deployments"),
    ("AWS Bedrock", "bedrock", "AWS-hosted models (SigV4 auth)"),
    (
        "Google Vertex AI",
        "google-vertex",
        "Gemini via Vertex AI platform",
    ),
    (
        "Ollama (local)",
        "ollama",
        "Local models via Ollama (no API key)",
    ),
    (
        "OpenAI-Compatible",
        "openai-compatible",
        "vLLM, LM Studio, or any OpenAI-compatible API",
    ),
];

/// Indices in PROVIDER_CHOICES that correspond to Azure Foundry entries
const AZURE_FOUNDRY_OPENAI_IDX: usize = 8;
const AZURE_FOUNDRY_ANTHROPIC_IDX: usize = 9;
const AZURE_FOUNDRY_OTHER_IDX: usize = 10;

// =============================================================================
// eavs setup add -- interactive wizard
// =============================================================================

/// Run the interactive setup wizard.
pub async fn run_setup_add(config_path: Option<&str>, batch: bool) -> Result<(), CliError> {
    let config_file = resolve_config_path(config_path)?;

    // Ensure a base config exists (server, logging, etc.)
    ensure_base_config(&config_file)?;

    if batch {
        // Batch mode: scan environment for known API keys, offer each
        run_batch_add(&config_file).await?;
    } else {
        // Interactive loop: add providers one at a time
        loop {
            add_single_provider(&config_file).await?;

            println!();
            let add_more = Confirm::new()
                .with_prompt("Add another provider?")
                .default(false)
                .interact()
                .map_err(|e| CliError::Other(format!("Confirmation cancelled: {}", e)))?;

            if !add_more {
                break;
            }
        }
    }

    println!();
    println!("Config written to {}", config_file.display());
    Ok(())
}

/// Known environment variable -> provider mappings for batch mode.
const ENV_PROVIDERS: &[(&str, &str, &str, &str)] = &[
    // (env_var, provider_name, provider_type, display_name)
    ("OPENAI_API_KEY", "openai", "openai", "OpenAI"),
    ("ANTHROPIC_API_KEY", "anthropic", "anthropic", "Anthropic"),
    ("GEMINI_API_KEY", "google", "google", "Google Gemini"),
    ("GOOGLE_API_KEY", "google", "google", "Google Gemini"),
    ("MISTRAL_API_KEY", "mistral", "mistral", "Mistral"),
    ("GROQ_API_KEY", "groq", "groq", "Groq"),
    ("XAI_API_KEY", "xai", "xai", "xAI"),
    (
        "OPENROUTER_API_KEY",
        "openrouter",
        "openrouter",
        "OpenRouter",
    ),
    ("CEREBRAS_API_KEY", "cerebras", "cerebras", "Cerebras"),
];

/// Batch mode: scan environment for API keys and offer to add each.
async fn run_batch_add(config_file: &PathBuf) -> Result<(), CliError> {
    let is_tty = std::io::stdin().is_terminal();

    println!();
    println!("EAVS Provider Setup (batch)");
    println!("{}", "=".repeat(50));
    println!();
    println!("Scanning environment for API keys...");
    println!();

    let existing = std::fs::read_to_string(config_file).unwrap_or_default();
    let mut added_count = 0;
    let mut first_provider: Option<String> = None;

    for &(env_var, provider_name, provider_type, display_name) in ENV_PROVIDERS {
        // Skip if key not in environment
        if std::env::var(env_var).is_err() {
            continue;
        }

        // Skip if already in config
        let section = format!("[providers.{}]", provider_name);
        if existing.contains(&section) {
            println!("  {} -- already configured, skipping", display_name);
            continue;
        }

        // In interactive mode, ask. In non-interactive, auto-add.
        let should_add = if is_tty {
            Confirm::new()
                .with_prompt(format!(
                    "Configure {} ({} found in env)?",
                    display_name, env_var
                ))
                .default(true)
                .interact()
                .unwrap_or(false)
        } else {
            true
        };

        if !should_add {
            continue;
        }

        let config = SetupProviderConfig {
            type_: provider_type.to_string(),
            api_key: format!("env:{}", env_var),
            base_url: None,
            api_version: None,
            deployment: None,
            aws_region: None,
            aws_access_key_id: None,
            aws_secret_access_key: None,
            aws_session_token: None,
            gcp_project: None,
            gcp_location: None,
            compat_supports_store: None,
            compat_max_tokens_field: None,
        };

        save_provider_to_config(config_file, provider_name, &config)?;
        println!("  Added {}", display_name);
        added_count += 1;

        if first_provider.is_none() {
            first_provider = Some(provider_name.to_string());
        }
    }

    // If interactive, offer to add more (custom providers)
    if is_tty {
        loop {
            println!();
            let add_custom = Confirm::new()
                .with_prompt("Add a custom provider?")
                .default(false)
                .interact()
                .map_err(|e| CliError::Other(format!("Confirmation cancelled: {}", e)))?;

            if !add_custom {
                break;
            }

            add_single_provider(config_file).await?;
            added_count += 1;
        }
    }

    // Set the first provider as the default route
    if let Some(ref first) = first_provider {
        set_default_provider(config_file, first)?;
    }

    if added_count == 0 {
        println!("No providers added.");
    } else {
        println!();
        println!("{} provider(s) configured.", added_count);
        if let Some(ref first) = first_provider {
            println!("Default provider: {}", first);
        }
    }

    Ok(())
}

/// Add a single provider interactively (the original wizard flow).
async fn add_single_provider(config_file: &PathBuf) -> Result<(), CliError> {
    println!();
    println!("EAVS Provider Setup");
    println!("{}", "=".repeat(50));
    println!();

    // 1. Select provider type
    let items: Vec<String> = PROVIDER_CHOICES
        .iter()
        .map(|(name, _, desc)| format!("{:<40} {}", name, desc))
        .collect();

    let selection = Select::new()
        .with_prompt("Select provider type")
        .items(&items)
        .default(0)
        .interact()
        .map_err(|e| CliError::Other(format!("Selection cancelled: {}", e)))?;

    let (display_name, type_str, _) = PROVIDER_CHOICES[selection];
    let is_azure_foundry = matches!(
        selection,
        AZURE_FOUNDRY_OPENAI_IDX | AZURE_FOUNDRY_ANTHROPIC_IDX | AZURE_FOUNDRY_OTHER_IDX
    );

    println!();
    println!("Selected: {}", display_name);

    // For OpenAI-family providers, let the user choose between API formats
    let type_str = if type_str == "openai" {
        println!();
        let api_choices = vec![
            "Chat Completions API (/v1/chat/completions)   Standard chat format",
            "Responses API (/v1/responses)                 Newer stateful format",
        ];
        let api_sel = Select::new()
            .with_prompt("Select API format")
            .items(&api_choices)
            .default(0)
            .interact()
            .map_err(|e| CliError::Other(format!("Selection cancelled: {}", e)))?;
        match api_sel {
            1 => "openai-responses",
            _ => type_str,
        }
    } else {
        type_str
    };

    let provider_type = ProviderType::from_str(type_str);

    println!();

    // 2. Provider name
    let default_name = suggest_provider_name(display_name, selection);
    let provider_name = prompt_input(
        "Provider name (used in config and X-Provider header)",
        Some(&default_name),
    )?;

    let provider_name = provider_name.trim().to_lowercase().replace(' ', "-");
    if provider_name.is_empty() {
        return Err(CliError::Other("Provider name cannot be empty".to_string()));
    }

    // 3. Collect provider-specific fields
    let setup_config =
        collect_provider_fields(provider_type, type_str, is_azure_foundry, selection)?;

    // 4. Test the configuration
    println!();
    let should_test = Confirm::new()
        .with_prompt("Test this configuration now?")
        .default(true)
        .interact()
        .map_err(|e| CliError::Other(format!("Confirmation cancelled: {}", e)))?;

    if should_test {
        let prov_config = setup_config.to_provider_config();
        let _ = test_provider_direct(&provider_name, &prov_config, None, None).await?;
    }

    // 5. Save to config
    println!();
    let should_save = Confirm::new()
        .with_prompt("Save this provider to config?")
        .default(true)
        .interact()
        .map_err(|e| CliError::Other(format!("Confirmation cancelled: {}", e)))?;

    if should_save {
        save_provider_to_config(config_file, &provider_name, &setup_config)?;
        println!("Provider '{}' saved.", provider_name);
    } else {
        println!("Skipped.");
    }

    Ok(())
}

/// Ensure the config file has a base scaffold (server, logging, keys sections).
/// Does nothing if the file already exists and has content.
fn ensure_base_config(config_file: &PathBuf) -> Result<(), CliError> {
    if config_file.exists() {
        let content = std::fs::read_to_string(config_file).unwrap_or_default();
        if content.contains("[server]") {
            return Ok(()); // Already has base config
        }
    }

    // Create parent dirs
    if let Some(parent) = config_file.parent() {
        std::fs::create_dir_all(parent).map_err(|e| {
            CliError::Other(format!("Failed to create {}: {}", parent.display(), e))
        })?;
    }

    let scaffold = r#""$schema" = "https://raw.githubusercontent.com/byteowlz/schemas/refs/heads/main/eavs/eavs.config.schema.json"

# EAVS Configuration
# Docs: https://github.com/byteowlz/eavs

[server]
host = "127.0.0.1"
port = 3033

[logging]
default = "stdout"

[analysis]
enabled = true
broadcast_channel_size = 1024

[state]
enabled = true
ttl_secs = 3600
cleanup_interval_secs = 60
max_conversations = 10000

[keys]
enabled = true
require_key = true
master_key = "env:EAVS_MASTER_KEY"
allow_self_provisioning = false
default_rpm_limit = 60
default_budget_usd = 50.0
update_pricing_on_startup = true
"#;

    std::fs::write(config_file, scaffold).map_err(|e| {
        CliError::Other(format!("Failed to write {}: {}", config_file.display(), e))
    })?;

    println!("Created base config: {}", config_file.display());

    Ok(())
}

/// Set the default provider in the config (copies type + api_key from an existing provider).
fn set_default_provider(config_file: &PathBuf, provider_name: &str) -> Result<(), CliError> {
    let content = std::fs::read_to_string(config_file).unwrap_or_default();

    // Already has a default? Skip.
    if content.contains("[providers.default]") {
        return Ok(());
    }

    // Find the provider's type and api_key
    let section = format!("[providers.{}]", provider_name);
    if let Some(start) = content.find(&section) {
        let block = &content[start..];
        let type_val = block
            .lines()
            .find(|l| l.starts_with("type = "))
            .unwrap_or("type = \"openai\"");
        let key_val = block
            .lines()
            .find(|l| l.starts_with("api_key = "))
            .unwrap_or("api_key = \"\"");

        // Insert default provider before the first [providers.*] section
        let default_block = format!("\n[providers.default]\n{}\n{}\n", type_val, key_val);

        // Find insertion point: just before the first [providers.*]
        if let Some(pos) = content.find("[providers.") {
            let mut new_content = String::with_capacity(content.len() + default_block.len());
            new_content.push_str(&content[..pos]);
            new_content.push_str(&default_block);
            new_content.push_str(&content[pos..]);

            std::fs::write(config_file, new_content).map_err(|e| {
                CliError::Other(format!("Failed to write {}: {}", config_file.display(), e))
            })?;
        }
    }

    Ok(())
}

// =============================================================================
// eavs setup test -- standalone direct test of a configured provider
// =============================================================================

/// Test a single provider from the config file by making a direct API call
/// (no server needed).
pub async fn run_setup_test(
    provider_name: &str,
    model: Option<&str>,
    message: &str,
    config_path: Option<&str>,
    format: &OutputFormat,
) -> Result<(), CliError> {
    let config = load_config(config_path)?;

    let lookup = config.resolve_provider(provider_name).ok_or_else(|| {
        let available = config
            .provider_names()
            .into_iter()
            .map(|s| s.as_str())
            .collect::<Vec<_>>()
            .join(", ");
        CliError::Other(format!(
            "Provider '{}' not found in config. Available: {}",
            provider_name, available
        ))
    })?;

    let prov_config = lookup.config;

    let result =
        test_provider_direct(&lookup.resolved_name, prov_config, model, Some(message)).await;

    match format {
        OutputFormat::Json => {
            match &result {
                Ok(TestOutcome::FailedContinue) => {
                    let output = serde_json::json!({
                        "provider": provider_name,
                        "success": false,
                        "skipped": true,
                    });
                    println!("{}", serde_json::to_string_pretty(&output).unwrap());
                }
                Err(ref e) => {
                    let output = serde_json::json!({
                        "provider": provider_name,
                        "success": false,
                        "error": e.to_string(),
                    });
                    println!("{}", serde_json::to_string_pretty(&output).unwrap());
                }
                Ok(TestOutcome::Passed) => {
                    // Success output is already printed inside test_provider_direct
                }
            }
        }
        OutputFormat::Text => {
            // text output is handled inside test_provider_direct
        }
    }

    // Map FailedContinue to an error for the exit code
    match result {
        Ok(TestOutcome::Passed) => Ok(()),
        Ok(TestOutcome::FailedContinue) => Err(CliError::Other("Test failed".to_string())),
        Err(e) => Err(e),
    }
}

// =============================================================================
// eavs setup test-all -- batch test every provider
// =============================================================================

/// Test all providers in the config file.
pub async fn run_setup_test_all(
    config_path: Option<&str>,
    model: Option<&str>,
    format: &OutputFormat,
) -> Result<(), CliError> {
    let config = load_config(config_path)?;

    let mut names: Vec<String> = config.providers.keys().cloned().collect();
    names.sort();

    if names.is_empty() {
        println!("No providers configured.");
        return Ok(());
    }

    println!();
    println!("Testing {} providers...", names.len());
    println!("{}", "=".repeat(60));

    let mut results: Vec<serde_json::Value> = Vec::new();
    let mut passed = 0u32;
    let mut failed = 0u32;

    for name in &names {
        let prov_config = &config.providers[name];
        println!();
        let result = test_provider_direct(name, prov_config, model, None).await;

        match &result {
            Ok(TestOutcome::Passed) => {
                passed += 1;
                results.push(serde_json::json!({
                    "provider": name,
                    "success": true,
                }));
            }
            Ok(TestOutcome::FailedContinue) => {
                failed += 1;
                results.push(serde_json::json!({
                    "provider": name,
                    "success": false,
                    "skipped": true,
                }));
            }
            Err(e) => {
                failed += 1;
                results.push(serde_json::json!({
                    "provider": name,
                    "success": false,
                    "error": e.to_string(),
                }));
            }
        }
    }

    println!();
    println!("{}", "=".repeat(60));

    match format {
        OutputFormat::Json => {
            let output = serde_json::json!({
                "total": names.len(),
                "passed": passed,
                "failed": failed,
                "results": results,
            });
            println!("{}", serde_json::to_string_pretty(&output).unwrap());
        }
        OutputFormat::Text => {
            println!(
                "Summary: {}/{} passed, {} failed",
                passed,
                names.len(),
                failed
            );
        }
    }

    if failed > 0 {
        Err(CliError::Other(format!("{} provider(s) failed", failed)))
    } else {
        Ok(())
    }
}

// =============================================================================
// eavs setup show -- display resolved config
// =============================================================================

/// Show the effective (resolved) configuration for a provider.
pub fn run_setup_show(
    provider_name: &str,
    config_path: Option<&str>,
    reveal: bool,
) -> Result<(), CliError> {
    let config = load_config(config_path)?;

    let lookup = config.resolve_provider(provider_name).ok_or_else(|| {
        let available = config
            .provider_names()
            .into_iter()
            .map(|s| s.as_str())
            .collect::<Vec<_>>()
            .join(", ");
        CliError::Other(format!(
            "Provider '{}' not found in config. Available: {}",
            provider_name, available
        ))
    })?;

    let prov = lookup.config;
    let provider_type = prov.provider_type();

    println!();
    println!("Provider: {}", lookup.resolved_name);
    println!("{}", "=".repeat(50));
    println!("  type       = {}", prov.type_);
    println!("  base_url   = {}", prov.resolved_base_url());

    let api_key = prov.resolved_api_key();
    if !api_key.is_empty() {
        if reveal {
            println!("  api_key    = {}", api_key);
        } else {
            let masked = mask_secret(&api_key);
            println!("  api_key    = {} (use --reveal to show)", masked);
        }
    } else {
        println!("  api_key    = (not set)");
    }

    if let Some(v) = prov.resolved_api_version() {
        println!("  api_version = {}", v);
    }
    if let Some(d) = prov.resolved_deployment() {
        println!("  deployment = {}", d);
    }
    if let Some(r) = prov.resolved_aws_region() {
        println!("  aws_region = {}", r);
    }
    if let Some(p) = prov.resolved_gcp_project() {
        println!("  gcp_project = {}", p);
    }
    if let Some(l) = prov.resolved_gcp_location() {
        println!("  gcp_location = {}", l);
    }

    // Show provider-type metadata
    let info = provider_type.info();
    println!();
    println!("Provider metadata:");
    println!("  auth_style     = {:?}", info.auth_style);
    if let Some(env_key) = info.env_key_name {
        let env_set = std::env::var(env_key).is_ok();
        println!(
            "  env_key_name   = {} ({})",
            env_key,
            if env_set { "set" } else { "NOT set" }
        );
    }
    if let Some(default_url) = info.default_base_url {
        println!("  default_url    = {}", default_url);
    }

    Ok(())
}

// =============================================================================
// Core test function -- used by both wizard and standalone commands
// =============================================================================

/// Test a provider configuration by making a direct API call.
///
/// This bypasses the EAVS proxy entirely -- it resolves credentials from the
/// config, builds the correct request for the provider type, and fires it
/// directly at the upstream.
///
/// When called from the interactive wizard, `message` is `None` (uses a
/// default) and the model is prompted interactively.
/// When called from `eavs setup test`, both can be supplied.
async fn test_provider_direct(
    provider_name: &str,
    prov_config: &ProviderConfig,
    model_override: Option<&str>,
    message_override: Option<&str>,
) -> Result<TestOutcome, CliError> {
    let provider_type = prov_config.provider_type();
    let base_url = prov_config.resolved_base_url();
    let api_key = prov_config.resolved_api_key();

    let model = match model_override {
        Some(m) => m.to_string(),
        None => {
            let default = resolve_test_model(prov_config);
            if std::io::stdin().is_terminal() {
                prompt_input(
                    &format!("Model to test '{}' with", provider_name),
                    Some(&default),
                )?
            } else {
                default
            }
        }
    };

    let message = message_override.unwrap_or("Say 'test successful' in exactly those words.");

    println!(
        "Testing '{}' -> {} (model: {})...",
        provider_name, base_url, model
    );

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()
        .map_err(|e| CliError::Other(format!("Failed to create HTTP client: {}", e)))?;

    let (url, body, headers) = build_test_request(
        provider_type,
        &base_url,
        &api_key,
        &model,
        prov_config,
        message,
    );

    let mut req = client.post(&url).header("Content-Type", "application/json");
    for (key, value) in &headers {
        req = req.header(key.as_str(), value.as_str());
    }
    req = req.json(&body);

    let start = std::time::Instant::now();
    match req.send().await {
        Ok(resp) => {
            let status = resp.status();
            let elapsed = start.elapsed();

            if status.is_success() {
                let resp_body: serde_json::Value = resp.json().await.unwrap_or_default();

                let resp_model = resp_body
                    .get("model")
                    .and_then(|m| m.as_str())
                    .unwrap_or(&model);

                let content = extract_response_content(&resp_body).unwrap_or("<no content>");

                println!("  PASSED ({:.2?})", elapsed);
                println!("  Model:    {}", resp_model);
                println!(
                    "  Response: {}",
                    if content.len() > 100 {
                        format!("{}...", &content[..100])
                    } else {
                        content.to_string()
                    }
                );
                Ok(TestOutcome::Passed)
            } else {
                let body_text = resp.text().await.unwrap_or_default();
                println!("  FAILED (HTTP {})", status);
                println!(
                    "  Response: {}",
                    if body_text.len() > 300 {
                        format!("{}...", &body_text[..300])
                    } else {
                        body_text.clone()
                    }
                );

                // In interactive mode, offer to continue; in non-interactive, just fail
                if std::io::stdin().is_terminal() {
                    let continue_anyway = Confirm::new()
                        .with_prompt("Continue anyway?")
                        .default(true)
                        .interact()
                        .map_err(|e| CliError::Other(format!("Confirmation cancelled: {}", e)))?;

                    if continue_anyway {
                        Ok(TestOutcome::FailedContinue)
                    } else {
                        Err(CliError::Api {
                            status: status.as_u16(),
                            message: body_text,
                        })
                    }
                } else {
                    Err(CliError::Api {
                        status: status.as_u16(),
                        message: body_text,
                    })
                }
            }
        }
        Err(e) => {
            println!("  FAILED: {}", e);

            if std::io::stdin().is_terminal() {
                let continue_anyway = Confirm::new()
                    .with_prompt("Continue anyway?")
                    .default(true)
                    .interact()
                    .map_err(|e| CliError::Other(format!("Confirmation cancelled: {}", e)))?;

                if continue_anyway {
                    Ok(TestOutcome::FailedContinue)
                } else {
                    Err(CliError::Other(e.to_string()))
                }
            } else {
                Err(CliError::Other(e.to_string()))
            }
        }
    }
}

/// Extract response content from various API response formats.
fn extract_response_content(body: &serde_json::Value) -> Option<&str> {
    // OpenAI / OpenAI-compatible format
    body.get("choices")
        .and_then(|c| c.get(0))
        .and_then(|c| c.get("message"))
        .and_then(|m| m.get("content"))
        .and_then(|c| c.as_str())
        // Anthropic format
        .or_else(|| {
            body.get("content")
                .and_then(|c| c.get(0))
                .and_then(|c| c.get("text"))
                .and_then(|t| t.as_str())
        })
        // Google Gemini format
        .or_else(|| {
            body.get("candidates")
                .and_then(|c| c.get(0))
                .and_then(|c| c.get("content"))
                .and_then(|c| c.get("parts"))
                .and_then(|p| p.get(0))
                .and_then(|p| p.get("text"))
                .and_then(|t| t.as_str())
        })
}

// =============================================================================
// Request builders for direct provider testing
// =============================================================================

/// Build the HTTP request (URL, body, headers) for a direct provider test.
fn build_test_request(
    provider_type: ProviderType,
    base_url: &str,
    api_key: &str,
    model: &str,
    prov_config: &ProviderConfig,
    message: &str,
) -> (String, serde_json::Value, Vec<(String, String)>) {
    let mut headers = Vec::new();

    match provider_type {
        ProviderType::Anthropic => {
            let url = format!("{}/messages", base_url.trim_end_matches('/'));
            headers.push(("x-api-key".to_string(), api_key.to_string()));
            headers.push(("anthropic-version".to_string(), "2023-06-01".to_string()));
            let body = serde_json::json!({
                "model": model,
                "max_tokens": 64,
                "messages": [{"role": "user", "content": message}]
            });
            (url, body, headers)
        }
        ProviderType::Google => {
            let url = format!(
                "{}/models/{}:generateContent?key={}",
                base_url.trim_end_matches('/'),
                model,
                api_key
            );
            let body = serde_json::json!({
                "contents": [{"parts": [{"text": message}]}],
                "generationConfig": {"maxOutputTokens": 64}
            });
            (url, body, headers)
        }
        ProviderType::Azure => {
            let deployment = prov_config
                .resolved_deployment()
                .unwrap_or_else(|| model.to_string());
            let api_version = prov_config
                .resolved_api_version()
                .unwrap_or_else(|| "2024-12-01-preview".to_string());
            let url = format!(
                "{}openai/deployments/{}/chat/completions?api-version={}",
                ensure_trailing_slash(base_url),
                deployment,
                api_version
            );
            headers.push(("api-key".to_string(), api_key.to_string()));
            let body = serde_json::json!({
                "model": model,
                "max_tokens": 64,
                "messages": [{"role": "user", "content": message}]
            });
            (url, body, headers)
        }
        ProviderType::GoogleVertex => {
            let project = prov_config.resolved_gcp_project().unwrap_or_default();
            let location = prov_config
                .resolved_gcp_location()
                .unwrap_or_else(|| "us-central1".to_string());
            let url = format!(
                "https://{}-aiplatform.googleapis.com/v1/projects/{}/locations/{}/publishers/google/models/{}:generateContent",
                location, project, location, model
            );
            if !api_key.is_empty() {
                headers.push(("Authorization".to_string(), format!("Bearer {}", api_key)));
            }
            let body = serde_json::json!({
                "contents": [{"parts": [{"text": message}]}],
                "generationConfig": {"maxOutputTokens": 64}
            });
            (url, body, headers)
        }
        ProviderType::OpenAIResponses | ProviderType::OpenAICodex => {
            // OpenAI Responses API format
            let url = format!("{}/responses", base_url.trim_end_matches('/'));
            if !api_key.is_empty() {
                headers.push(("Authorization".to_string(), format!("Bearer {}", api_key)));
            }
            let body = serde_json::json!({
                "model": model,
                "input": message,
                "max_output_tokens": 64
            });
            (url, body, headers)
        }
        _ => {
            // OpenAI-compatible chat completions format (covers OpenAI, Groq,
            // Mistral, xAI, OpenRouter, Cerebras, Ollama, Foundry, etc.)
            let url = format!("{}/chat/completions", base_url.trim_end_matches('/'));
            if !api_key.is_empty() {
                headers.push(("Authorization".to_string(), format!("Bearer {}", api_key)));
            }
            let body = serde_json::json!({
                "model": model,
                "max_tokens": 64,
                "messages": [{"role": "user", "content": message}]
            });
            (url, body, headers)
        }
    }
}

fn ensure_trailing_slash(url: &str) -> String {
    if url.ends_with('/') {
        url.to_string()
    } else {
        format!("{}/", url)
    }
}

/// Resolve the test model for a provider using this precedence:
///
/// 1. `test_model` from the provider's config.toml section
/// 2. First model found in `~/.pi/agent/models.json` whose provider `baseUrl`
///    matches this provider's `base_url`
/// 3. Hardcoded default based on provider type
fn resolve_test_model(prov_config: &ProviderConfig) -> String {
    // 1. Explicit test_model in config
    if !prov_config.test_model.is_empty() {
        return prov_config.test_model.clone();
    }

    // 2. Try to find a matching model in ~/.pi/agent/models.json
    let base_url = prov_config.resolved_base_url();
    if !base_url.is_empty() {
        if let Some(model_id) = find_pi_model_for_base_url(&base_url) {
            return model_id;
        }
    }

    // 3. Hardcoded defaults
    suggest_test_model_default(prov_config.provider_type())
}

/// Load `~/.pi/agent/models.json` and find the first model whose provider's
/// `baseUrl` matches (or is a prefix of) the given base URL.
fn find_pi_model_for_base_url(base_url: &str) -> Option<String> {
    let pi_models_path = resolve_pi_models_path()?;
    let content = std::fs::read_to_string(&pi_models_path).ok()?;
    let doc: serde_json::Value = serde_json::from_str(&content).ok()?;

    let providers = doc.get("providers")?.as_object()?;
    let normalized_base = base_url.trim_end_matches('/').to_lowercase();

    for (_name, provider) in providers {
        let provider_base = provider
            .get("baseUrl")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .trim_end_matches('/')
            .to_lowercase();

        if provider_base.is_empty() {
            continue;
        }

        // Match if the URLs are equal or one is a prefix of the other
        // (e.g., config has "http://localhost:8080/v1" and models.json has "http://localhost:8080/v1")
        if normalized_base == provider_base
            || normalized_base.starts_with(&provider_base)
            || provider_base.starts_with(&normalized_base)
        {
            let models = provider.get("models").and_then(|v| v.as_array())?;
            if let Some(first) = models.first() {
                return first.get("id").and_then(|v| v.as_str()).map(String::from);
            }
        }
    }

    None
}

/// Resolve the path to `~/.pi/agent/models.json`, checking
/// `PI_CODING_AGENT_DIR` env var first.
fn resolve_pi_models_path() -> Option<std::path::PathBuf> {
    // Check PI_CODING_AGENT_DIR first (pi's own override)
    if let Ok(agent_dir) = std::env::var("PI_CODING_AGENT_DIR") {
        let path = std::path::PathBuf::from(agent_dir).join("models.json");
        if path.exists() {
            return Some(path);
        }
    }

    // Default: ~/.pi/agent/models.json
    let home = std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .ok()?;
    let path = std::path::PathBuf::from(home)
        .join(".pi")
        .join("agent")
        .join("models.json");
    if path.exists() {
        Some(path)
    } else {
        None
    }
}

/// Hardcoded default test models per provider type.
fn suggest_test_model_default(provider_type: ProviderType) -> String {
    match provider_type {
        ProviderType::OpenAI | ProviderType::OpenAIResponses => "gpt-4o-mini".to_string(),
        ProviderType::Anthropic => "claude-sonnet-4-20250514".to_string(),
        ProviderType::Google | ProviderType::GoogleVertex | ProviderType::GoogleGeminiCli => {
            "gemini-2.0-flash".to_string()
        }
        ProviderType::Mistral => "mistral-small-latest".to_string(),
        ProviderType::Groq => "llama-3.3-70b-versatile".to_string(),
        ProviderType::Cerebras => "llama-3.3-70b".to_string(),
        ProviderType::XAI => "grok-2-latest".to_string(),
        ProviderType::OpenRouter => "openai/gpt-4o-mini".to_string(),
        ProviderType::Azure => "gpt-4o-mini".to_string(),
        ProviderType::OpenAICompatible => "default".to_string(),
        _ => "default".to_string(),
    }
}

// =============================================================================
// Interactive field collectors
// =============================================================================

/// Suggest a default provider name based on selection.
fn suggest_provider_name(display_name: &str, selection: usize) -> String {
    match selection {
        AZURE_FOUNDRY_OPENAI_IDX => "foundry-openai".to_string(),
        AZURE_FOUNDRY_ANTHROPIC_IDX => "foundry-claude".to_string(),
        AZURE_FOUNDRY_OTHER_IDX => "foundry-other".to_string(),
        _ => display_name
            .split_whitespace()
            .next()
            .unwrap_or("provider")
            .to_lowercase(),
    }
}

/// Collect provider-specific configuration fields interactively.
fn collect_provider_fields(
    provider_type: ProviderType,
    type_str: &str,
    is_azure_foundry: bool,
    selection: usize,
) -> Result<SetupProviderConfig, CliError> {
    let mut config = SetupProviderConfig {
        type_: type_str.to_string(),
        ..Default::default()
    };

    // -- API Key --
    match provider_type {
        ProviderType::Bedrock | ProviderType::Mock => {
            // No API key needed
        }
        ProviderType::OpenAICompatible if !is_azure_foundry => {
            let api_key = collect_api_key("API key (leave empty if not required)", true)?;
            config.api_key = api_key;
        }
        _ => {
            let env_hint = provider_type
                .info()
                .env_key_name
                .map(|e| format!(" (or use env:{})", e))
                .unwrap_or_default();
            let prompt = if is_azure_foundry {
                "API key (or use env:AZURE_FOUNDRY_API_KEY)".to_string()
            } else {
                format!("API key{}", env_hint)
            };
            config.api_key = collect_api_key(&prompt, false)?;
        }
    }

    // -- Base URL --
    if is_azure_foundry {
        config.base_url = Some(collect_azure_foundry_base_url(selection)?);
    } else {
        match provider_type {
            ProviderType::Azure => {
                let base_url = prompt_input(
                    "Azure OpenAI endpoint URL (e.g. https://your-resource.openai.azure.com/)",
                    None,
                )?;
                if !base_url.is_empty() {
                    config.base_url = Some(base_url);
                }
            }
            ProviderType::OpenAICompatible => {
                let base_url = prompt_input(
                    "Base URL (e.g. http://localhost:8000/v1)",
                    None,
                )?;
                if !base_url.is_empty() {
                    config.base_url = Some(base_url);
                }
            }
            ProviderType::Bedrock | ProviderType::GoogleVertex => {
                // Region/location based -- handled below
            }
            _ => {
                let default_url = provider_type
                    .info()
                    .default_base_url
                    .unwrap_or("")
                    .to_string();

                let base_url = prompt_input("Base URL", Some(&default_url))?;

                if !base_url.is_empty() && base_url != default_url {
                    config.base_url = Some(base_url);
                }
            }
        }
    }

    // -- Provider-specific extra fields --
    match provider_type {
        ProviderType::Azure if !is_azure_foundry => {
            let api_version = prompt_input("API version", Some("2024-12-01-preview"))?;
            config.api_version = Some(api_version);

            let deployment = prompt_input(
                "Deployment name (leave empty to use model name)",
                None,
            )?;
            if !deployment.is_empty() {
                config.deployment = Some(deployment);
            }
        }
        ProviderType::Bedrock => {
            let region = prompt_input("AWS region", Some("us-east-1"))?;
            config.aws_region = Some(region);

            config.aws_access_key_id = Some(collect_api_key_or_env(
                "AWS access key ID",
                "AWS_ACCESS_KEY_ID",
            )?);
            config.aws_secret_access_key = Some(collect_api_key_or_env(
                "AWS secret access key",
                "AWS_SECRET_ACCESS_KEY",
            )?);

            let session_token = prompt_input(
                "AWS session token (leave empty if not needed)",
                None,
            )?;
            if !session_token.is_empty() {
                config.aws_session_token = Some(session_token);
            }
        }
        ProviderType::GoogleVertex => {
            let project = prompt_input_required("GCP project ID")?;
            config.gcp_project = Some(project);

            let location = prompt_input("GCP location", Some("us-central1"))?;
            config.gcp_location = Some(location);
        }
        _ => {}
    }

    // -- Compat settings for OpenAI-compatible --
    if selection == AZURE_FOUNDRY_OTHER_IDX
        || (provider_type == ProviderType::OpenAICompatible && !is_azure_foundry)
    {
        let configure_compat = Confirm::new()
            .with_prompt("Configure compatibility settings?")
            .default(false)
            .interact()
            .map_err(|e| CliError::Other(format!("Confirmation cancelled: {}", e)))?;

        if configure_compat {
            let supports_store = Confirm::new()
                .with_prompt("Supports 'store' parameter?")
                .default(false)
                .interact()
                .map_err(|e| CliError::Other(format!("Confirmation cancelled: {}", e)))?;
            config.compat_supports_store = Some(supports_store);

            let max_tokens_field = prompt_input("Max tokens field name", Some("max_tokens"))?;
            config.compat_max_tokens_field = Some(max_tokens_field);
        }
    }

    // -- Summary --
    println!();
    println!("Configuration Summary");
    println!("{}", "-".repeat(40));
    println!("  type     = \"{}\"", config.type_);
    if !config.api_key.is_empty() {
        let display_key =
            if config.api_key.starts_with("env:") || config.api_key.starts_with("keychain:") {
                config.api_key.clone()
            } else {
                mask_secret(&config.api_key)
            };
        println!("  api_key  = \"{}\"", display_key);
    }
    if let Some(ref url) = config.base_url {
        println!("  base_url = \"{}\"", url);
    }
    if let Some(ref v) = config.api_version {
        println!("  api_version = \"{}\"", v);
    }
    if let Some(ref d) = config.deployment {
        println!("  deployment = \"{}\"", d);
    }
    if let Some(ref r) = config.aws_region {
        println!("  aws_region = \"{}\"", r);
    }
    if let Some(ref p) = config.gcp_project {
        println!("  gcp_project = \"{}\"", p);
    }
    if let Some(ref l) = config.gcp_location {
        println!("  gcp_location = \"{}\"", l);
    }

    Ok(config)
}

/// Collect API key interactively, supporting env:, keychain:, or literal value.
fn collect_api_key(prompt: &str, allow_empty: bool) -> Result<String, CliError> {
    println!();
    println!("API key input options:");
    println!("  1. Enter key directly (will be masked)");
    println!("  2. Use environment variable reference (env:VAR_NAME)");
    println!("  3. Use system keychain reference (keychain:account)");
    if allow_empty {
        println!("  4. Skip (no API key)");
    }
    println!();

    let choices: Vec<&str> = if allow_empty {
        vec![
            "Enter key directly",
            "Environment variable (env:...)",
            "System keychain (keychain:...)",
            "Skip",
        ]
    } else {
        vec![
            "Enter key directly",
            "Environment variable (env:...)",
            "System keychain (keychain:...)",
        ]
    };

    let key_method = Select::new()
        .with_prompt(prompt)
        .items(&choices)
        .default(0)
        .interact()
        .map_err(|e| CliError::Other(format!("Selection cancelled: {}", e)))?;

    match key_method {
        0 => {
            let key = rpassword::prompt_password("API key: ")
                .map_err(|e| CliError::Other(format!("Input cancelled: {}", e)))?;
            if key.is_empty() && !allow_empty {
                return Err(CliError::Other("API key cannot be empty".to_string()));
            }
            Ok(key)
        }
        1 => {
            let var_name = prompt_input_required("Environment variable name")?;
            if std::env::var(&var_name).is_err() {
                println!(
                    "  Warning: ${} is not currently set in the environment",
                    var_name
                );
            } else {
                println!("  ${} is set", var_name);
            }
            Ok(format!("env:{}", var_name))
        }
        2 => {
            let account = prompt_input_required("Keychain account name")?;
            if crate::config::get_keychain_secret(&account).is_some() {
                println!("  Keychain entry '{}' found", account);
            } else {
                println!("  Warning: Keychain entry '{}' not found", account);
                let store_now = Confirm::new()
                    .with_prompt("Store a secret for this account now?")
                    .default(true)
                    .interact()
                    .map_err(|e| CliError::Other(format!("Confirmation cancelled: {}", e)))?;

                if store_now {
                    let secret = rpassword::prompt_password("Secret value: ")
                        .map_err(|e| CliError::Other(format!("Input cancelled: {}", e)))?;
                    crate::config::set_keychain_secret(&account, &secret)
                        .map_err(CliError::Other)?;
                    println!("  Stored secret in keychain");
                }
            }
            Ok(format!("keychain:{}", account))
        }
        3 if allow_empty => Ok(String::new()),
        _ => Err(CliError::Other("Invalid selection".to_string())),
    }
}

/// Collect a credential that can be a literal value or env: reference.
fn collect_api_key_or_env(prompt: &str, default_env: &str) -> Result<String, CliError> {
    let choices = vec![
        format!("Use environment variable (env:{})", default_env),
        "Enter value directly".to_string(),
    ];

    let method = Select::new()
        .with_prompt(prompt)
        .items(&choices)
        .default(0)
        .interact()
        .map_err(|e| CliError::Other(format!("Selection cancelled: {}", e)))?;

    match method {
        0 => {
            let var_name = prompt_input("Environment variable name", Some(default_env))?;
            Ok(format!("env:{}", var_name))
        }
        1 => {
            let value = rpassword::prompt_password(format!("{}: ", prompt))
                .map_err(|e| CliError::Other(format!("Input cancelled: {}", e)))?;
            Ok(value)
        }
        _ => Err(CliError::Other("Invalid selection".to_string())),
    }
}

/// Collect Azure Foundry base URL based on the specific variant.
fn collect_azure_foundry_base_url(selection: usize) -> Result<String, CliError> {
    let resource = prompt_input_required("Azure AI Foundry resource name (e.g. 'my-resource')")?;

    let base_url = match selection {
        AZURE_FOUNDRY_OPENAI_IDX => {
            format!("https://{}.services.ai.azure.com/openai/v1", resource)
        }
        AZURE_FOUNDRY_ANTHROPIC_IDX => {
            format!("https://{}.openai.azure.com/anthropic/v1", resource)
        }
        AZURE_FOUNDRY_OTHER_IDX => {
            format!("https://{}.services.ai.azure.com/openai/v1", resource)
        }
        _ => unreachable!(),
    };

    println!("  Base URL: {}", base_url);

    let use_custom = Confirm::new()
        .with_prompt("Use a custom URL instead?")
        .default(false)
        .interact()
        .map_err(|e| CliError::Other(format!("Confirmation cancelled: {}", e)))?;

    if use_custom {
        let custom_url = prompt_input_required("Custom base URL")?;
        Ok(custom_url)
    } else {
        Ok(base_url)
    }
}

// =============================================================================
// Config file I/O
// =============================================================================

/// Load config from file path or default location.
fn load_config(config_path: Option<&str>) -> Result<AppConfig, CliError> {
    if let Some(path) = config_path {
        AppConfig::load_from(path)
            .map_err(|e| CliError::Other(format!("Failed to load config from {}: {}", path, e)))
    } else {
        AppConfig::load().map_err(|e| CliError::Other(format!("Failed to load config: {}", e)))
    }
}

/// Resolve the config file path to write to.
fn resolve_config_path(config_path: Option<&str>) -> Result<PathBuf, CliError> {
    if let Some(path) = config_path {
        return Ok(PathBuf::from(path));
    }

    let config_home = std::env::var("XDG_CONFIG_HOME")
        .ok()
        .filter(|v| !v.trim().is_empty())
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var("HOME")
                .ok()
                .map(|home| PathBuf::from(home).join(".config"))
        })
        .ok_or_else(|| CliError::Other("Cannot determine config directory".to_string()))?;

    Ok(config_home.join("eavs").join("config.toml"))
}

/// Save a provider configuration to the TOML config file.
fn save_provider_to_config(
    config_file: &PathBuf,
    provider_name: &str,
    config: &SetupProviderConfig,
) -> Result<(), CliError> {
    if let Some(parent) = config_file.parent() {
        std::fs::create_dir_all(parent).map_err(|e| {
            CliError::Other(format!(
                "Failed to create config directory {}: {}",
                parent.display(),
                e
            ))
        })?;
    }

    let mut toml_lines = Vec::new();
    toml_lines.push(String::new());
    toml_lines.push(format!("[providers.{}]", provider_name));
    toml_lines.push(format!("type = \"{}\"", config.type_));

    if !config.api_key.is_empty() {
        toml_lines.push(format!("api_key = \"{}\"", config.api_key));
    }
    if let Some(ref url) = config.base_url {
        toml_lines.push(format!("base_url = \"{}\"", url));
    }
    if let Some(ref v) = config.api_version {
        toml_lines.push(format!("api_version = \"{}\"", v));
    }
    if let Some(ref d) = config.deployment {
        toml_lines.push(format!("deployment = \"{}\"", d));
    }
    if let Some(ref r) = config.aws_region {
        toml_lines.push(format!("aws_region = \"{}\"", r));
    }
    if let Some(ref k) = config.aws_access_key_id {
        toml_lines.push(format!("aws_access_key_id = \"{}\"", k));
    }
    if let Some(ref k) = config.aws_secret_access_key {
        toml_lines.push(format!("aws_secret_access_key = \"{}\"", k));
    }
    if let Some(ref t) = config.aws_session_token {
        toml_lines.push(format!("aws_session_token = \"{}\"", t));
    }
    if let Some(ref p) = config.gcp_project {
        toml_lines.push(format!("gcp_project = \"{}\"", p));
    }
    if let Some(ref l) = config.gcp_location {
        toml_lines.push(format!("gcp_location = \"{}\"", l));
    }

    if config.compat_supports_store.is_some() || config.compat_max_tokens_field.is_some() {
        toml_lines.push(format!("[providers.{}.compat]", provider_name));
        if let Some(supports_store) = config.compat_supports_store {
            toml_lines.push(format!("supports_store = {}", supports_store));
        }
        if let Some(ref field) = config.compat_max_tokens_field {
            toml_lines.push(format!("max_tokens_field = \"{}\"", field));
        }
    }

    let snippet = toml_lines.join("\n");

    let existing = std::fs::read_to_string(config_file).unwrap_or_default();

    let section_header = format!("[providers.{}]", provider_name);
    if existing.contains(&section_header) {
        println!(
            "  Warning: Provider '{}' already exists in config. Appending new entry.",
            provider_name
        );
        println!(
            "  You may need to manually remove the old entry from {}",
            config_file.display()
        );
    }

    let mut new_content = existing;
    if !new_content.ends_with('\n') && !new_content.is_empty() {
        new_content.push('\n');
    }
    new_content.push_str(&snippet);
    new_content.push('\n');

    std::fs::write(config_file, new_content).map_err(|e| {
        CliError::Other(format!(
            "Failed to write config file {}: {}",
            config_file.display(),
            e
        ))
    })?;

    Ok(())
}

// =============================================================================
// Helpers
// =============================================================================

/// Prompt for text input using plain stdin (cooked mode).
///
/// Unlike `dialoguer::Input`, this handles pasted text correctly because
/// the terminal stays in cooked/line-buffered mode. Bracketed paste
/// sequences are consumed by the terminal driver instead of leaking into
/// the application as duplicate characters.
fn prompt_input(prompt: &str, default: Option<&str>) -> Result<String, CliError> {
    match default {
        Some(d) if !d.is_empty() => print!("{} [{}]: ", prompt, d),
        _ => print!("{}: ", prompt),
    }
    io::stdout()
        .flush()
        .map_err(|e| CliError::Other(e.to_string()))?;

    let mut line = String::new();
    io::stdin()
        .lock()
        .read_line(&mut line)
        .map_err(|e| CliError::Other(format!("Input cancelled: {}", e)))?;

    let trimmed = line.trim().to_string();
    if trimmed.is_empty() {
        if let Some(d) = default {
            return Ok(d.to_string());
        }
    }
    Ok(trimmed)
}

/// Prompt for required (non-empty) text input.
fn prompt_input_required(prompt: &str) -> Result<String, CliError> {
    let value = prompt_input(prompt, None)?;
    if value.is_empty() {
        return Err(CliError::Other(format!("{} cannot be empty", prompt)));
    }
    Ok(value)
}

/// Mask a secret value for display.
fn mask_secret(value: &str) -> String {
    if value.len() > 8 {
        format!("{}****{}", &value[..4], &value[value.len() - 4..])
    } else {
        "****".to_string()
    }
}

// =============================================================================
// Intermediate config used during the "add" wizard
// =============================================================================

#[derive(Debug, Default)]
pub struct SetupProviderConfig {
    pub type_: String,
    pub api_key: String,
    pub base_url: Option<String>,
    pub api_version: Option<String>,
    pub deployment: Option<String>,
    pub aws_region: Option<String>,
    pub aws_access_key_id: Option<String>,
    pub aws_secret_access_key: Option<String>,
    pub aws_session_token: Option<String>,
    pub gcp_project: Option<String>,
    pub gcp_location: Option<String>,
    pub compat_supports_store: Option<bool>,
    pub compat_max_tokens_field: Option<String>,
}

impl SetupProviderConfig {
    /// Convert to a `ProviderConfig` for testing.
    fn to_provider_config(&self) -> ProviderConfig {
        ProviderConfig {
            type_: self.type_.clone(),
            api_key: self.api_key.clone(),
            base_url: self.base_url.clone().unwrap_or_default(),
            api_version: self.api_version.clone(),
            deployment: self.deployment.clone().unwrap_or_default(),
            aws_region: self.aws_region.clone().unwrap_or_default(),
            aws_access_key_id: self.aws_access_key_id.clone().unwrap_or_default(),
            aws_secret_access_key: self.aws_secret_access_key.clone().unwrap_or_default(),
            aws_session_token: self.aws_session_token.clone().unwrap_or_default(),
            aws_service: String::new(),
            gcp_project: self.gcp_project.clone().unwrap_or_default(),
            gcp_location: self.gcp_location.clone().unwrap_or_default(),
            compat: crate::provider::CompatSettings::default(),
            headers: HashMap::new(),
            test_model: String::new(),
            models: Vec::new(),
        }
    }
}

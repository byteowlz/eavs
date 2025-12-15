//! CLI module for EAVS.
//!
//! Provides subcommands for:
//! - `serve` - Run the proxy server (foreground)
//! - `service` - Manage the proxy server as a background service
//! - `key` - Manage virtual API keys
//! - `test` - Test the proxy functionality

use clap::{Parser, Subcommand};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::time::Duration;

/// EAVS - Bidirectional LLM Proxy with Virtual API Keys
#[derive(Parser)]
#[command(name = "eavs")]
#[command(about = "Bidirectional LLM proxy with virtual API keys, rate limiting, and cost tracking")]
#[command(version)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Commands,
}

#[derive(Subcommand)]
pub enum Commands {
    /// Start the EAVS proxy server (foreground)
    Serve {
        /// Host to bind to
        #[arg(short = 'H', long, env = "EAVS_HOST")]
        host: Option<String>,

        /// Port to bind to
        #[arg(short, long, env = "EAVS_PORT")]
        port: Option<u16>,

        /// Path to config file
        #[arg(short, long, env = "EAVS_CONFIG")]
        config: Option<String>,
    },

    /// Manage the EAVS server as a background service
    Service {
        #[command(subcommand)]
        action: ServiceCommands,
    },

    /// Manage virtual API keys
    Key {
        #[command(subcommand)]
        action: KeyCommands,
    },

    /// Test the proxy functionality
    Test {
        #[command(subcommand)]
        action: TestCommands,
    },
}

#[derive(Subcommand)]
pub enum ServiceCommands {
    /// Start the EAVS server in the background
    Start {
        /// Port to bind to
        #[arg(short, long, env = "EAVS_PORT", default_value = "3000")]
        port: u16,

        /// Path to config file
        #[arg(short, long, env = "EAVS_CONFIG")]
        config: Option<String>,

        /// Wait for server to be ready before returning
        #[arg(long, default_value = "true")]
        wait: bool,
    },

    /// Stop the running EAVS server
    Stop {
        /// Port the server is running on (to identify the process)
        #[arg(short, long, env = "EAVS_PORT", default_value = "3000")]
        port: u16,

        /// Force kill if graceful shutdown fails
        #[arg(short, long)]
        force: bool,
    },

    /// Restart the EAVS server
    Restart {
        /// Port to bind to
        #[arg(short, long, env = "EAVS_PORT", default_value = "3000")]
        port: u16,

        /// Path to config file
        #[arg(short, long, env = "EAVS_CONFIG")]
        config: Option<String>,
    },

    /// Show the status of the EAVS server
    Status {
        /// Port to check
        #[arg(short, long, env = "EAVS_PORT", default_value = "3000")]
        port: u16,

        /// Output format
        #[arg(long, default_value = "text")]
        format: OutputFormat,
    },

    /// Show server logs (if running with file logging)
    Logs {
        /// Number of lines to show
        #[arg(short, long, default_value = "50")]
        lines: usize,

        /// Follow log output (like tail -f)
        #[arg(short, long)]
        follow: bool,
    },
}

#[derive(Subcommand)]
pub enum KeyCommands {
    /// Create a new virtual API key
    Create {
        /// Name for the key
        #[arg(short, long)]
        name: Option<String>,

        /// Allowed model patterns (glob), can be specified multiple times
        #[arg(short = 'm', long = "model")]
        models: Vec<String>,

        /// Blocked model patterns (glob), can be specified multiple times
        #[arg(long = "block-model")]
        blocked_models: Vec<String>,

        /// Allowed providers, can be specified multiple times
        #[arg(long = "provider")]
        providers: Vec<String>,

        /// Requests per minute limit
        #[arg(long)]
        rpm: Option<u32>,

        /// Tokens per minute limit
        #[arg(long)]
        tpm: Option<u32>,

        /// Requests per day limit
        #[arg(long)]
        rpd: Option<u32>,

        /// Maximum budget in USD
        #[arg(long)]
        budget: Option<f64>,

        /// Key expiration (e.g., "30d", "24h", "never")
        #[arg(long, default_value = "never")]
        expires: String,

        /// Output format
        #[arg(long, default_value = "text")]
        format: OutputFormat,
    },

    /// List all virtual API keys
    List {
        /// Include disabled keys
        #[arg(long)]
        all: bool,

        /// Output format
        #[arg(long, default_value = "text")]
        format: OutputFormat,
    },

    /// Get information about a key
    Info {
        /// Key hash or key ID prefix
        key: String,

        /// Output format
        #[arg(long, default_value = "text")]
        format: OutputFormat,
    },

    /// Revoke (disable) a key
    Revoke {
        /// Key hash or key ID prefix
        key: String,

        /// Skip confirmation prompt
        #[arg(short, long)]
        yes: bool,
    },

    /// Show usage history for a key
    Usage {
        /// Key hash or key ID prefix
        key: String,

        /// Number of days of history
        #[arg(long, default_value = "7")]
        days: u32,

        /// Output format
        #[arg(long, default_value = "text")]
        format: OutputFormat,
    },
}

#[derive(Subcommand)]
pub enum TestCommands {
    /// Send a chat completion request through the proxy
    Chat {
        /// Message to send
        message: String,

        /// Provider to use
        #[arg(short, long, default_value = "default")]
        provider: String,

        /// Model to use
        #[arg(short, long, default_value = "gpt-4o-mini")]
        model: String,

        /// API key to use (virtual or real)
        #[arg(short, long, env = "EAVS_API_KEY")]
        key: Option<String>,

        /// Use streaming
        #[arg(short, long)]
        stream: bool,

        /// EAVS server URL
        #[arg(long, default_value = "http://127.0.0.1:3000", env = "EAVS_URL")]
        url: String,

        /// Path to config file to use when auto-starting the server
        #[arg(long, env = "EAVS_CONFIG")]
        config: Option<String>,
    },

    /// Test rate limiting with rapid requests
    RateLimit {
        /// Number of requests to send
        #[arg(short, long, default_value = "10")]
        count: u32,

        /// API key to test
        #[arg(short, long)]
        key: String,

        /// EAVS server URL
        #[arg(long, default_value = "http://127.0.0.1:3000", env = "EAVS_URL")]
        url: String,

        /// Path to config file to use when auto-starting the server
        #[arg(long, env = "EAVS_CONFIG")]
        config: Option<String>,
    },

    /// Benchmark latency overhead introduced by EAVS proxy
    Bench {
        /// Number of requests for benchmarking
        #[arg(short, long, default_value = "10")]
        count: u32,

        /// Provider to use
        #[arg(short, long, default_value = "default")]
        provider: String,

        /// Model to use
        #[arg(short, long, default_value = "gpt-4o-mini")]
        model: String,

        /// API key to use
        #[arg(short, long, env = "EAVS_API_KEY")]
        key: Option<String>,

        /// Also test direct provider access for comparison (requires provider API key)
        #[arg(long)]
        compare_direct: bool,

        /// Direct provider URL for comparison
        #[arg(long, default_value = "https://api.openai.com")]
        direct_url: String,

        /// Direct provider API key for comparison
        #[arg(long, env = "OPENAI_API_KEY")]
        direct_key: Option<String>,

        /// Use streaming mode
        #[arg(short, long)]
        stream: bool,

        /// EAVS server URL
        #[arg(long, default_value = "http://127.0.0.1:3000", env = "EAVS_URL")]
        url: String,

        /// Output format
        #[arg(long, default_value = "text")]
        format: OutputFormat,

        /// Path to config file to use when auto-starting the server
        #[arg(long, env = "EAVS_CONFIG")]
        config: Option<String>,
    },

    /// Check proxy health and configuration
    Health {
        /// EAVS server URL
        #[arg(long, default_value = "http://127.0.0.1:3000", env = "EAVS_URL")]
        url: String,

        /// Output format
        #[arg(long, default_value = "text")]
        format: OutputFormat,

        /// Path to config file to use when auto-starting the server
        #[arg(long, env = "EAVS_CONFIG")]
        config: Option<String>,
    },
}

#[derive(Clone, Debug, Default, clap::ValueEnum)]
pub enum OutputFormat {
    #[default]
    Text,
    Json,
}

/// Configuration for CLI client operations
pub struct CliConfig {
    pub server_url: String,
    pub master_key: Option<String>,
    pub timeout: Duration,
}

impl Default for CliConfig {
    fn default() -> Self {
        Self {
            server_url: std::env::var("EAVS_URL")
                .unwrap_or_else(|_| "http://127.0.0.1:3000".to_string()),
            master_key: std::env::var("EAVS_MASTER_KEY").ok(),
            timeout: Duration::from_secs(30),
        }
    }
}

/// Client for talking to EAVS server
pub struct EavsClient {
    client: reqwest::Client,
    pub config: CliConfig,
}

impl EavsClient {
    pub fn new(config: CliConfig) -> Self {
        let client = reqwest::Client::builder()
            .timeout(config.timeout)
            .build()
            .expect("Failed to create HTTP client");

        Self { client, config }
    }

    pub fn with_url(url: String) -> Self {
        Self::new(CliConfig {
            server_url: url,
            ..Default::default()
        })
    }

    /// Create a new virtual API key
    pub async fn create_key(
        &self,
        request: &CreateKeyCliRequest,
    ) -> Result<CreateKeyResponse, CliError> {
        let url = format!("{}/admin/keys", self.config.server_url);

        let mut req = self.client.post(&url).json(request);

        if let Some(ref key) = self.config.master_key {
            req = req.header("X-Master-Key", key);
        }

        let resp = req.send().await.map_err(CliError::Request)?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(CliError::Api {
                status: status.as_u16(),
                message: body,
            });
        }

        resp.json().await.map_err(CliError::Request)
    }

    /// List all virtual API keys
    pub async fn list_keys(&self, include_disabled: bool) -> Result<Vec<KeyInfo>, CliError> {
        let url = format!("{}/admin/keys", self.config.server_url);

        let mut req = self.client.get(&url);

        if include_disabled {
            req = req.query(&[("include_disabled", "true")]);
        }

        if let Some(ref key) = self.config.master_key {
            req = req.header("X-Master-Key", key);
        }

        let resp = req.send().await.map_err(CliError::Request)?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(CliError::Api {
                status: status.as_u16(),
                message: body,
            });
        }

        resp.json().await.map_err(CliError::Request)
    }

    /// Get key info by hash
    pub async fn get_key(&self, key_hash: &str) -> Result<KeyInfo, CliError> {
        let url = format!("{}/admin/keys/{}", self.config.server_url, key_hash);

        let mut req = self.client.get(&url);

        if let Some(ref key) = self.config.master_key {
            req = req.header("X-Master-Key", key);
        }

        let resp = req.send().await.map_err(CliError::Request)?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(CliError::Api {
                status: status.as_u16(),
                message: body,
            });
        }

        resp.json().await.map_err(CliError::Request)
    }

    /// Revoke (disable) a key
    pub async fn revoke_key(&self, key_hash: &str) -> Result<(), CliError> {
        let url = format!("{}/admin/keys/{}", self.config.server_url, key_hash);

        let mut req = self.client.delete(&url);

        if let Some(ref key) = self.config.master_key {
            req = req.header("X-Master-Key", key);
        }

        let resp = req.send().await.map_err(CliError::Request)?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(CliError::Api {
                status: status.as_u16(),
                message: body,
            });
        }

    Ok(())
}
    /// Get usage history for a key
    pub async fn get_usage(&self, key_hash: &str, days: u32) -> Result<Vec<UsageRecord>, CliError> {
        let url = format!("{}/admin/keys/{}/usage", self.config.server_url, key_hash);

        let mut req = self.client.get(&url).query(&[("days", days.to_string())]);

        if let Some(ref key) = self.config.master_key {
            req = req.header("X-Master-Key", key);
        }

        let resp = req.send().await.map_err(CliError::Request)?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(CliError::Api {
                status: status.as_u16(),
                message: body,
            });
        }

        resp.json().await.map_err(CliError::Request)
    }

    /// Send a chat completion request
    pub async fn chat(
        &self,
        message: &str,
        model: &str,
        provider: &str,
        api_key: Option<&str>,
        stream: bool,
    ) -> Result<ChatResponse, CliError> {
        let url = format!("{}/v1/chat/completions", self.config.server_url);

        let body = serde_json::json!({
            "model": model,
            "messages": [{"role": "user", "content": message}],
            "stream": stream,
            "max_tokens": 256
        });

        let mut req = self
            .client
            .post(&url)
            .header("Content-Type", "application/json")
            .header("X-Provider", provider)
            .json(&body);

        if let Some(key) = api_key {
            req = req.header("Authorization", format!("Bearer {}", key));
        }

        let resp = req.send().await.map_err(CliError::Request)?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(CliError::Api {
                status: status.as_u16(),
                message: body,
            });
        }

        if stream {
            // For streaming, collect the full response
            let text = resp.text().await.map_err(CliError::Request)?;
            let mut content = String::new();

            for line in text.lines() {
                if let Some(data) = line.strip_prefix("data: ") {
                    if data == "[DONE]" {
                        break;
                    }
                    if let Ok(chunk) = serde_json::from_str::<serde_json::Value>(data) {
                        if let Some(delta) = chunk
                            .get("choices")
                            .and_then(|c| c.get(0))
                            .and_then(|c| c.get("delta"))
                            .and_then(|d| d.get("content"))
                            .and_then(|c| c.as_str())
                        {
                            content.push_str(delta);
                        }
                    }
                }
            }

            Ok(ChatResponse {
                content,
                model: model.to_string(),
                usage: None,
            })
        } else {
            let json: serde_json::Value = resp.json().await.map_err(CliError::Request)?;

            let content = json
                .get("choices")
                .and_then(|c| c.get(0))
                .and_then(|c| c.get("message"))
                .and_then(|m| m.get("content"))
                .and_then(|c| c.as_str())
                .unwrap_or("")
                .to_string();

            let usage = json.get("usage").map(|u| ChatUsage {
                prompt_tokens: u.get("prompt_tokens").and_then(|v| v.as_u64()).unwrap_or(0) as u32,
                completion_tokens: u
                    .get("completion_tokens")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0) as u32,
            });

            Ok(ChatResponse {
                content,
                model: json
                    .get("model")
                    .and_then(|m| m.as_str())
                    .unwrap_or(model)
                    .to_string(),
                usage,
            })
        }
    }

    /// Check server health
    pub async fn health(&self) -> Result<HealthResponse, CliError> {
        let url = format!("{}/health", self.config.server_url);

        let resp = self
            .client
            .get(&url)
            .send()
            .await
            .map_err(CliError::Request)?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(CliError::Api {
                status: status.as_u16(),
                message: body,
            });
        }

        // Try to parse as JSON, but health endpoint may return empty body
        let text = resp.text().await.unwrap_or_default();
        if text.is_empty() {
            Ok(HealthResponse {
                status: "ok".to_string(),
                version: None,
                uptime_secs: None,
            })
        } else {
            serde_json::from_str(&text).map_err(|e| {
                CliError::Other(format!("Failed to parse health response: {}", e))
            })
        }
    }

    /// List providers
    pub async fn providers(&self) -> Result<Vec<String>, CliError> {
        let url = format!("{}/providers", self.config.server_url);

        let resp = self
            .client
            .get(&url)
            .send()
            .await
            .map_err(CliError::Request)?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(CliError::Api {
                status: status.as_u16(),
                message: body,
            });
        }

        resp.json().await.map_err(CliError::Request)
    }
}

// Request/Response types for CLI

#[derive(Debug, Serialize)]
pub struct CreateKeyCliRequest {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<String>,
    pub permissions: KeyPermissions,
}

#[derive(Debug, Serialize, Default)]
pub struct KeyPermissions {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub allowed_models: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub blocked_models: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub allowed_providers: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rpm_limit: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tpm_limit: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rpd_limit: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_budget_usd: Option<f64>,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct CreateKeyResponse {
    pub key: String,
    pub key_id: String,
    pub key_hash: String,
    pub expires_at: Option<String>,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct KeyInfo {
    pub key_id: String,
    pub key_hash: String,
    pub name: Option<String>,
    pub created_at: String,
    pub expires_at: Option<String>,
    pub disabled: bool,
    pub permissions: serde_json::Value,
    pub usage: serde_json::Value,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct UsageRecord {
    pub timestamp: String,
    pub model: String,
    pub provider: String,
    pub input_tokens: u32,
    pub output_tokens: u32,
    pub cached_tokens: u32,
    pub cost_usd: f64,
}

#[derive(Debug)]
pub struct ChatResponse {
    pub content: String,
    #[allow(dead_code)]
    pub model: String,
    pub usage: Option<ChatUsage>,
}

#[derive(Debug)]
pub struct ChatUsage {
    pub prompt_tokens: u32,
    pub completion_tokens: u32,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct HealthResponse {
    pub status: String,
    #[serde(default)]
    pub version: Option<String>,
    #[serde(default)]
    pub uptime_secs: Option<u64>,
}

#[derive(Debug)]
pub enum CliError {
    Request(reqwest::Error),
    Api { status: u16, message: String },
    Other(String),
}

impl std::fmt::Display for CliError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Request(e) => write!(f, "Request error: {}", e),
            Self::Api { status, message } => write!(f, "API error ({}): {}", status, message),
            Self::Other(msg) => write!(f, "{}", msg),
        }
    }
}

impl std::error::Error for CliError {}

/// Parse expiration string like "30d", "24h", "never" into ISO timestamp
pub fn parse_expiration(s: &str) -> Option<String> {
    if s == "never" || s.is_empty() {
        return None;
    }

    let now = chrono::Utc::now();
    let duration = if let Some(days) = s.strip_suffix('d') {
        let days: i64 = days.parse().ok()?;
        chrono::Duration::days(days)
    } else if let Some(hours) = s.strip_suffix('h') {
        let hours: i64 = hours.parse().ok()?;
        chrono::Duration::hours(hours)
    } else if let Some(mins) = s.strip_suffix('m') {
        let mins: i64 = mins.parse().ok()?;
        chrono::Duration::minutes(mins)
    } else {
        return None;
    };

    Some((now + duration).to_rfc3339())
}

// CLI execution functions

pub async fn run_key_create(
    client: &EavsClient,
    name: Option<String>,
    models: Vec<String>,
    blocked_models: Vec<String>,
    providers: Vec<String>,
    rpm: Option<u32>,
    tpm: Option<u32>,
    rpd: Option<u32>,
    budget: Option<f64>,
    expires: String,
    format: OutputFormat,
) -> Result<(), CliError> {
    let request = CreateKeyCliRequest {
        name,
        expires_at: parse_expiration(&expires),
        permissions: KeyPermissions {
            allowed_models: if models.is_empty() {
                None
            } else {
                Some(models)
            },
            blocked_models: if blocked_models.is_empty() {
                None
            } else {
                Some(blocked_models)
            },
            allowed_providers: if providers.is_empty() {
                None
            } else {
                Some(providers)
            },
            rpm_limit: rpm,
            tpm_limit: tpm,
            rpd_limit: rpd,
            max_budget_usd: budget,
        },
    };

    let response = client.create_key(&request).await?;

    match format {
        OutputFormat::Json => {
            println!("{}", serde_json::to_string_pretty(&response).unwrap());
        }
        OutputFormat::Text => {
            println!("Key created successfully!");
            println!();
            println!("  API Key: {}", response.key);
            println!("  Key ID:  {}", response.key_id);
            println!("  Hash:    {}", response.key_hash);
            if let Some(expires) = response.expires_at {
                println!("  Expires: {}", expires);
            }
            println!();
            println!("Save this key securely - it cannot be retrieved later.");
        }
    }

    Ok(())
}

pub async fn run_key_list(
    client: &EavsClient,
    include_disabled: bool,
    format: OutputFormat,
) -> Result<(), CliError> {
    let keys = client.list_keys(include_disabled).await?;

    match format {
        OutputFormat::Json => {
            println!("{}", serde_json::to_string_pretty(&keys).unwrap());
        }
        OutputFormat::Text => {
            if keys.is_empty() {
                println!("No keys found.");
                return Ok(());
            }

            println!(
                "{:<12} {:<20} {:<10} {:<20}",
                "KEY ID", "NAME", "STATUS", "CREATED"
            );
            println!("{}", "-".repeat(64));

            for key in keys {
                let status = if key.disabled { "disabled" } else { "active" };
                let name = key.name.unwrap_or_else(|| "-".to_string());
                println!(
                    "{:<12} {:<20} {:<10} {:<20}",
                    &key.key_id[..12.min(key.key_id.len())],
                    &name[..20.min(name.len())],
                    status,
                    &key.created_at[..19.min(key.created_at.len())]
                );
            }
        }
    }

    Ok(())
}

pub async fn run_key_info(
    client: &EavsClient,
    key: String,
    format: OutputFormat,
) -> Result<(), CliError> {
    let info = client.get_key(&key).await?;

    match format {
        OutputFormat::Json => {
            println!("{}", serde_json::to_string_pretty(&info).unwrap());
        }
        OutputFormat::Text => {
            println!("Key Information");
            println!("{}", "=".repeat(40));
            println!("Key ID:      {}", info.key_id);
            println!("Hash:        {}", info.key_hash);
            println!(
                "Name:        {}",
                info.name.unwrap_or_else(|| "-".to_string())
            );
            println!("Created:     {}", info.created_at);
            println!(
                "Expires:     {}",
                info.expires_at.unwrap_or_else(|| "never".to_string())
            );
            println!(
                "Status:      {}",
                if info.disabled { "disabled" } else { "active" }
            );
            println!();
            println!("Permissions:");
            println!(
                "{}",
                serde_json::to_string_pretty(&info.permissions).unwrap()
            );
            println!();
            println!("Usage:");
            println!("{}", serde_json::to_string_pretty(&info.usage).unwrap());
        }
    }

    Ok(())
}

pub async fn run_key_revoke(client: &EavsClient, key: String, yes: bool) -> Result<(), CliError> {
    if !yes {
        eprint!("Are you sure you want to revoke key '{}'? [y/N] ", key);
        let mut input = String::new();
        std::io::stdin().read_line(&mut input).unwrap();
        if !input.trim().eq_ignore_ascii_case("y") {
            println!("Cancelled.");
            return Ok(());
        }
    }

    client.revoke_key(&key).await?;
    println!("Key '{}' has been revoked.", key);

    Ok(())
}

pub async fn run_key_usage(
    client: &EavsClient,
    key: String,
    days: u32,
    format: OutputFormat,
) -> Result<(), CliError> {
    let records = client.get_usage(&key, days).await?;

    match format {
        OutputFormat::Json => {
            println!("{}", serde_json::to_string_pretty(&records).unwrap());
        }
        OutputFormat::Text => {
            if records.is_empty() {
                println!("No usage records found for the past {} days.", days);
                return Ok(());
            }

            println!(
                "{:<20} {:<15} {:<10} {:<10} {:<10}",
                "TIMESTAMP", "MODEL", "INPUT", "OUTPUT", "COST"
            );
            println!("{}", "-".repeat(70));

            let mut total_input = 0u32;
            let mut total_output = 0u32;
            let mut total_cost = 0.0f64;

            for record in &records {
                println!(
                    "{:<20} {:<15} {:<10} {:<10} ${:<9.4}",
                    &record.timestamp[..19.min(record.timestamp.len())],
                    &record.model[..15.min(record.model.len())],
                    record.input_tokens,
                    record.output_tokens,
                    record.cost_usd
                );
                total_input += record.input_tokens;
                total_output += record.output_tokens;
                total_cost += record.cost_usd;
            }

            println!("{}", "-".repeat(70));
            println!(
                "{:<20} {:<15} {:<10} {:<10} ${:<9.4}",
                "TOTAL", "", total_input, total_output, total_cost
            );
        }
    }

    Ok(())
}

pub async fn run_test_chat(
    client: &EavsClient,
    message: String,
    model: String,
    provider: String,
    api_key: Option<String>,
    stream: bool,
) -> Result<(), CliError> {
    println!("Sending request to EAVS...");
    println!("  Provider: {}", provider);
    println!("  Model: {}", model);
    println!("  Stream: {}", stream);
    println!();

    let response = client
        .chat(&message, &model, &provider, api_key.as_deref(), stream)
        .await?;

    println!("Response:");
    println!("{}", response.content);
    println!();

    if let Some(usage) = response.usage {
        println!(
            "Usage: {} prompt + {} completion tokens",
            usage.prompt_tokens, usage.completion_tokens
        );
    }

    Ok(())
}

pub async fn run_test_rate_limit(
    client: &EavsClient,
    count: u32,
    api_key: String,
) -> Result<(), CliError> {
    println!("Testing rate limiting with {} rapid requests...", count);
    println!();

    let mut successes = 0;
    let mut rate_limited = 0;
    let mut errors = 0;

    for i in 1..=count {
        let result = client
            .chat("Say 'ok'", "gpt-4o-mini", "default", Some(&api_key), false)
            .await;

        match result {
            Ok(_) => {
                print!(".");
                successes += 1;
            }
            Err(CliError::Api { status: 429, .. }) => {
                print!("R");
                rate_limited += 1;
            }
            Err(_) => {
                print!("X");
                errors += 1;
            }
        }

        if i % 10 == 0 {
            println!(" [{}/{}]", i, count);
        }
    }

    println!();
    println!();
    println!("Results:");
    println!("  Successful: {}", successes);
    println!("  Rate limited (429): {}", rate_limited);
    println!("  Other errors: {}", errors);

    Ok(())
}

pub async fn run_test_health(client: &EavsClient, format: OutputFormat) -> Result<(), CliError> {
    let health = client.health().await?;
    let providers = client.providers().await.unwrap_or_default();

    match format {
        OutputFormat::Json => {
            let output = serde_json::json!({
                "health": health,
                "providers": providers
            });
            println!("{}", serde_json::to_string_pretty(&output).unwrap());
        }
        OutputFormat::Text => {
            println!("EAVS Server Status");
            println!("{}", "=".repeat(40));
            println!("Status: {}", health.status);
            if let Some(version) = health.version {
                println!("Version: {}", version);
            }
            if let Some(uptime) = health.uptime_secs {
                println!("Uptime: {}s", uptime);
            }
            println!();
            println!("Available Providers:");
            for provider in providers {
                println!("  - {}", provider);
            }
        }
    }

    Ok(())
}

/// Benchmark results
#[derive(Debug, Serialize)]
pub struct BenchmarkResults {
    pub target: String,
    pub requests: u32,
    pub successful: u32,
    pub failed: u32,
    pub latencies_ms: Vec<f64>,
    pub min_ms: f64,
    pub max_ms: f64,
    pub mean_ms: f64,
    pub median_ms: f64,
    pub p95_ms: f64,
    pub p99_ms: f64,
    pub stddev_ms: f64,
}

impl BenchmarkResults {
    fn from_latencies(target: String, latencies: Vec<f64>, failed: u32) -> Self {
        let successful = latencies.len() as u32;
        let requests = successful + failed;

        if latencies.is_empty() {
            return Self {
                target,
                requests,
                successful: 0,
                failed,
                latencies_ms: vec![],
                min_ms: 0.0,
                max_ms: 0.0,
                mean_ms: 0.0,
                median_ms: 0.0,
                p95_ms: 0.0,
                p99_ms: 0.0,
                stddev_ms: 0.0,
            };
        }

        let mut sorted = latencies.clone();
        sorted.sort_by(|a, b| a.partial_cmp(b).unwrap());

        let min_ms = sorted[0];
        let max_ms = sorted[sorted.len() - 1];
        let mean_ms = sorted.iter().sum::<f64>() / sorted.len() as f64;

        let median_ms = if sorted.len().is_multiple_of(2) {
            (sorted[sorted.len() / 2 - 1] + sorted[sorted.len() / 2]) / 2.0
        } else {
            sorted[sorted.len() / 2]
        };

        let p95_idx = ((sorted.len() as f64 * 0.95) as usize).min(sorted.len() - 1);
        let p99_idx = ((sorted.len() as f64 * 0.99) as usize).min(sorted.len() - 1);
        let p95_ms = sorted[p95_idx];
        let p99_ms = sorted[p99_idx];

        let variance = sorted.iter().map(|x| (x - mean_ms).powi(2)).sum::<f64>() / sorted.len() as f64;
        let stddev_ms = variance.sqrt();

        Self {
            target,
            requests,
            successful,
            failed,
            latencies_ms: sorted,
            min_ms,
            max_ms,
            mean_ms,
            median_ms,
            p95_ms,
            p99_ms,
            stddev_ms,
        }
    }

    fn print_text(&self) {
        println!("  Requests:  {} ({} successful, {} failed)", self.requests, self.successful, self.failed);
        if self.successful > 0 {
            println!("  Min:       {:.2}ms", self.min_ms);
            println!("  Max:       {:.2}ms", self.max_ms);
            println!("  Mean:      {:.2}ms", self.mean_ms);
            println!("  Median:    {:.2}ms", self.median_ms);
            println!("  P95:       {:.2}ms", self.p95_ms);
            println!("  P99:       {:.2}ms", self.p99_ms);
            println!("  Std Dev:   {:.2}ms", self.stddev_ms);
        }
    }
}

/// Run a single timed request
async fn timed_request(
    client: &reqwest::Client,
    url: &str,
    body: &serde_json::Value,
    api_key: Option<&str>,
    provider: Option<&str>,
) -> Result<Duration, CliError> {
    let start = std::time::Instant::now();

    let mut req = client
        .post(url)
        .header("Content-Type", "application/json")
        .json(body);

    if let Some(key) = api_key {
        req = req.header("Authorization", format!("Bearer {}", key));
    }

    if let Some(prov) = provider {
        req = req.header("X-Provider", prov);
    }

    let resp = req.send().await.map_err(CliError::Request)?;

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        return Err(CliError::Api {
            status: status.as_u16(),
            message: body,
        });
    }

    // Consume the response body to measure full latency
    let _ = resp.bytes().await;

    Ok(start.elapsed())
}

pub async fn run_test_bench(
    count: u32,
    provider: String,
    model: String,
    api_key: Option<String>,
    compare_direct: bool,
    direct_url: String,
    direct_key: Option<String>,
    stream: bool,
    eavs_url: String,
    format: OutputFormat,
) -> Result<(), CliError> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(60))
        .build()
        .expect("Failed to create HTTP client");

    // Small, fast request for benchmarking
    let body = serde_json::json!({
        "model": model,
        "messages": [{"role": "user", "content": "Say 'ok'"}],
        "stream": stream,
        "max_tokens": 5
    });

    println!("EAVS Latency Benchmark");
    println!("{}", "=".repeat(50));
    println!("Model: {}", model);
    println!("Provider: {}", provider);
    println!("Requests: {}", count);
    println!("Streaming: {}", stream);
    println!();

    // Warm-up request
    println!("Warming up...");
    let warmup_url = format!("{}/v1/chat/completions", eavs_url);
    let _ = timed_request(&client, &warmup_url, &body, api_key.as_deref(), Some(&provider)).await;

    // Benchmark EAVS proxy
    println!("Benchmarking EAVS proxy ({})...", eavs_url);
    let mut eavs_latencies = Vec::with_capacity(count as usize);
    let mut eavs_failed = 0u32;

    for i in 1..=count {
        match timed_request(&client, &warmup_url, &body, api_key.as_deref(), Some(&provider)).await {
            Ok(duration) => {
                eavs_latencies.push(duration.as_secs_f64() * 1000.0);
                print!(".");
            }
            Err(_) => {
                eavs_failed += 1;
                print!("X");
            }
        }
        if i % 10 == 0 {
            println!(" [{}/{}]", i, count);
        }
        std::io::Write::flush(&mut std::io::stdout()).ok();
    }
    if !count.is_multiple_of(10) {
        println!();
    }

    let eavs_results = BenchmarkResults::from_latencies(
        format!("EAVS ({})", eavs_url),
        eavs_latencies,
        eavs_failed,
    );

    // Optionally benchmark direct provider access
    let direct_results = if compare_direct {
        let direct_api_key = direct_key.ok_or_else(|| {
            CliError::Other("--direct-key is required for direct comparison".to_string())
        })?;

        println!();
        println!("Benchmarking direct provider ({})...", direct_url);

        let direct_endpoint = format!("{}/v1/chat/completions", direct_url);
        let mut direct_latencies = Vec::with_capacity(count as usize);
        let mut direct_failed = 0u32;

        for i in 1..=count {
            match timed_request(&client, &direct_endpoint, &body, Some(&direct_api_key), None).await {
                Ok(duration) => {
                    direct_latencies.push(duration.as_secs_f64() * 1000.0);
                    print!(".");
                }
                Err(_) => {
                    direct_failed += 1;
                    print!("X");
                }
            }
            if i % 10 == 0 {
                println!(" [{}/{}]", i, count);
            }
            std::io::Write::flush(&mut std::io::stdout()).ok();
        }
        if !count.is_multiple_of(10) {
            println!();
        }

        Some(BenchmarkResults::from_latencies(
            format!("Direct ({})", direct_url),
            direct_latencies,
            direct_failed,
        ))
    } else {
        None
    };

    println!();

    match format {
        OutputFormat::Json => {
            let output = serde_json::json!({
                "eavs": eavs_results,
                "direct": direct_results,
                "overhead_ms": direct_results.as_ref().map(|d| eavs_results.mean_ms - d.mean_ms),
                "overhead_pct": direct_results.as_ref().map(|d| {
                    if d.mean_ms > 0.0 {
                        ((eavs_results.mean_ms - d.mean_ms) / d.mean_ms) * 100.0
                    } else {
                        0.0
                    }
                }),
            });
            println!("{}", serde_json::to_string_pretty(&output).unwrap());
        }
        OutputFormat::Text => {
            println!("Results");
            println!("{}", "=".repeat(50));
            println!();
            println!("EAVS Proxy:");
            eavs_results.print_text();

            if let Some(ref direct) = direct_results {
                println!();
                println!("Direct Provider:");
                direct.print_text();

                println!();
                println!("Overhead Analysis:");
                let overhead_ms = eavs_results.mean_ms - direct.mean_ms;
                let overhead_pct = if direct.mean_ms > 0.0 {
                    (overhead_ms / direct.mean_ms) * 100.0
                } else {
                    0.0
                };
                println!("  Mean overhead:   {:.2}ms ({:.1}%)", overhead_ms, overhead_pct);

                let median_overhead = eavs_results.median_ms - direct.median_ms;
                println!("  Median overhead: {:.2}ms", median_overhead);
            }
        }
    }

    Ok(())
}

// =============================================================================
// Server Auto-Start Functionality
// =============================================================================

/// Result of checking/starting an EAVS server
pub struct ServerStatus {
    pub url: String,
    #[allow(dead_code)]
    pub port: u16,
    #[allow(dead_code)]
    pub was_started: bool,
}

/// Check if a port is available for binding
pub fn is_port_available(port: u16) -> bool {
    std::net::TcpListener::bind(("127.0.0.1", port)).is_ok()
}

/// Check if an EAVS server is running at the given URL
pub async fn is_eavs_server_running(url: &str) -> bool {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(2))
        .build()
        .unwrap();

    let health_url = format!("{}/health", url);
    match client.get(&health_url).send().await {
        Ok(resp) => {
            if resp.status().is_success() {
                // Check that we don't get HTML (which would indicate a non-EAVS server)
                if let Ok(text) = resp.text().await {
                    // EAVS returns empty body or JSON, never HTML
                    !text.contains("<!DOCTYPE") && !text.contains("<html")
                } else {
                    // Empty body is OK for EAVS health endpoint
                    true
                }
            } else {
                false
            }
        }
        Err(_) => false,
    }
}

/// Find an available port starting from the given port
pub fn find_available_port(start_port: u16) -> u16 {
    let mut port = start_port;
    let max_port = start_port + 100; // Try up to 100 ports

    while port < max_port {
        if is_port_available(port) {
            return port;
        }
        port += 1;
    }

    // If we couldn't find a port, return the start port and let it fail later
    start_port
}

/// Start the EAVS server in the background
pub fn start_server_background(
    port: u16,
    config_path: Option<&str>,
) -> Result<std::process::Child, CliError> {
    let exe_path = std::env::current_exe().map_err(|e| {
        CliError::Other(format!("Failed to get current executable path: {}", e))
    })?;

    let mut cmd = std::process::Command::new(&exe_path);
    cmd.arg("serve")
        .arg("--port")
        .arg(port.to_string())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());

    if let Some(config) = config_path {
        cmd.arg("--config").arg(config);
    }

    cmd.spawn().map_err(|e| {
        CliError::Other(format!("Failed to start EAVS server: {}", e))
    })
}

/// Ensure an EAVS server is running, starting one if necessary.
/// Returns the URL of the running server.
pub async fn ensure_server_running(
    preferred_url: &str,
    config_path: Option<&str>,
) -> Result<ServerStatus, CliError> {
    // Parse the preferred URL to get host and port
    let url = url::Url::parse(preferred_url).map_err(|e| {
        CliError::Other(format!("Invalid URL '{}': {}", preferred_url, e))
    })?;

    let host = url.host_str().unwrap_or("127.0.0.1");
    let preferred_port = url.port().unwrap_or(3000);

    // First, check if EAVS is already running at the preferred URL
    if is_eavs_server_running(preferred_url).await {
        return Ok(ServerStatus {
            url: preferred_url.to_string(),
            port: preferred_port,
            was_started: false,
        });
    }

    // Check if something else is running on that port (non-EAVS)
    let port = if !is_port_available(preferred_port) {
        // Port is in use but not by EAVS, find another port
        let new_port = find_available_port(preferred_port + 1);
        eprintln!(
            "Port {} is in use by another application, using port {} instead",
            preferred_port, new_port
        );
        new_port
    } else {
        preferred_port
    };

    // Start the server
    eprintln!("Starting EAVS server on port {}...", port);
    let _child = start_server_background(port, config_path)?;

    // Build the new URL
    let new_url = format!("{}://{}:{}", url.scheme(), host, port);

    // Wait for the server to be ready (with timeout)
    let start = std::time::Instant::now();
    let timeout = Duration::from_secs(10);

    while start.elapsed() < timeout {
        if is_eavs_server_running(&new_url).await {
            eprintln!("EAVS server started successfully");
            return Ok(ServerStatus {
                url: new_url,
                port,
                was_started: true,
            });
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }

    Err(CliError::Other(format!(
        "Timed out waiting for EAVS server to start on port {}",
        port
    )))
}

// =============================================================================
// Service Management Functions
// =============================================================================

fn xdg_state_dir() -> PathBuf {
    if let Ok(val) = std::env::var("XDG_STATE_HOME") {
        if !val.trim().is_empty() {
            return PathBuf::from(val);
        }
    }

    if let Ok(home) = std::env::var("HOME") {
        if !home.trim().is_empty() {
            return PathBuf::from(home).join(".local/state");
        }
    }

    PathBuf::from("/tmp")
}

/// Get the PID file path for a given port
fn get_pid_file_path(port: u16) -> std::path::PathBuf {
    xdg_state_dir().join("eavs").join(format!("eavs-{}.pid", port))
}

/// Write the PID file
fn write_pid_file(port: u16, pid: u32) -> Result<(), CliError> {
    let pid_path = get_pid_file_path(port);
    if let Some(parent) = pid_path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| {
            CliError::Other(format!("Failed to create PID directory: {}", e))
        })?;
    }
    std::fs::write(&pid_path, pid.to_string()).map_err(|e| {
        CliError::Other(format!("Failed to write PID file: {}", e))
    })
}

/// Read the PID from file
fn read_pid_file(port: u16) -> Option<u32> {
    let pid_path = get_pid_file_path(port);
    std::fs::read_to_string(&pid_path)
        .ok()
        .and_then(|s| s.trim().parse().ok())
}

/// Remove the PID file
fn remove_pid_file(port: u16) {
    let pid_path = get_pid_file_path(port);
    let _ = std::fs::remove_file(pid_path);
}

/// Check if a process with the given PID is running
fn is_process_running(pid: u32) -> bool {
    #[cfg(unix)]
    {
        // Use kill -0 to check if process exists
        unsafe { libc::kill(pid as i32, 0) == 0 }
    }
    #[cfg(not(unix))]
    {
        // On non-Unix, try a different approach
        std::process::Command::new("tasklist")
            .args(["/FI", &format!("PID eq {}", pid)])
            .output()
            .map(|o| String::from_utf8_lossy(&o.stdout).contains(&pid.to_string()))
            .unwrap_or(false)
    }
}

/// Kill a process by PID
fn kill_process(pid: u32, force: bool) -> Result<(), CliError> {
    #[cfg(unix)]
    {
        let signal = if force { libc::SIGKILL } else { libc::SIGTERM };
        let result = unsafe { libc::kill(pid as i32, signal) };
        if result == 0 {
            Ok(())
        } else {
            Err(CliError::Other(format!(
                "Failed to kill process {}: {}",
                pid,
                std::io::Error::last_os_error()
            )))
        }
    }
    #[cfg(not(unix))]
    {
        std::process::Command::new("taskkill")
            .args(if force { vec!["/F", "/PID"] } else { vec!["/PID"] })
            .arg(pid.to_string())
            .output()
            .map_err(|e| CliError::Other(format!("Failed to kill process: {}", e)))?;
        Ok(())
    }
}

/// Find EAVS process by port (fallback when no PID file)
fn find_eavs_pid_by_port(port: u16) -> Option<u32> {
    #[cfg(unix)]
    {
        // Use lsof to find the process using the port
        let output = std::process::Command::new("lsof")
            .args(["-ti", &format!(":{}", port)])
            .output()
            .ok()?;

        let stdout = String::from_utf8_lossy(&output.stdout);
        stdout.lines().next().and_then(|s| s.trim().parse().ok())
    }
    #[cfg(not(unix))]
    {
        // On Windows, use netstat
        let output = std::process::Command::new("netstat")
            .args(["-ano"])
            .output()
            .ok()?;

        let stdout = String::from_utf8_lossy(&output.stdout);
        for line in stdout.lines() {
            if line.contains(&format!(":{}", port)) && line.contains("LISTENING") {
                if let Some(pid_str) = line.split_whitespace().last() {
                    if let Ok(pid) = pid_str.parse() {
                        return Some(pid);
                    }
                }
            }
        }
        None
    }
}

/// Service status information
#[derive(Debug, Serialize)]
pub struct ServiceStatus {
    pub running: bool,
    pub pid: Option<u32>,
    pub port: u16,
    pub url: String,
    pub uptime_secs: Option<u64>,
    pub providers: Vec<String>,
}

/// Start the EAVS service in the background
pub async fn run_service_start(
    port: u16,
    config_path: Option<String>,
    wait: bool,
) -> Result<(), CliError> {
    let url = format!("http://127.0.0.1:{}", port);

    // Check if already running
    if is_eavs_server_running(&url).await {
        println!("EAVS server is already running on port {}", port);
        return Ok(());
    }

    // Check if port is in use by something else
    if !is_port_available(port) {
        return Err(CliError::Other(format!(
            "Port {} is already in use by another application",
            port
        )));
    }

    // Start the server
    println!("Starting EAVS server on port {}...", port);
    let child = start_server_background(port, config_path.as_deref())?;

    // Write PID file
    write_pid_file(port, child.id())?;

    if wait {
        // Wait for server to be ready
        let start = std::time::Instant::now();
        let timeout = Duration::from_secs(10);

        while start.elapsed() < timeout {
            if is_eavs_server_running(&url).await {
                println!("EAVS server started successfully (PID: {})", child.id());
                println!("  URL: {}", url);
                return Ok(());
            }
            tokio::time::sleep(Duration::from_millis(200)).await;
        }

        // Cleanup on failure
        remove_pid_file(port);
        return Err(CliError::Other(format!(
            "Timed out waiting for EAVS server to start on port {}",
            port
        )));
    } else {
        println!("EAVS server starting in background (PID: {})", child.id());
    }

    Ok(())
}

/// Stop the EAVS service
pub async fn run_service_stop(port: u16, force: bool) -> Result<(), CliError> {
    let url = format!("http://127.0.0.1:{}", port);

    // Try to find PID from file first, then by port
    let pid = read_pid_file(port).or_else(|| find_eavs_pid_by_port(port));

    match pid {
        Some(pid) => {
            if !is_process_running(pid) {
                println!("EAVS server is not running (stale PID file)");
                remove_pid_file(port);
                return Ok(());
            }

            println!("Stopping EAVS server (PID: {})...", pid);

            // Send SIGTERM (graceful) or SIGKILL (force)
            kill_process(pid, force)?;

            // Wait for process to exit
            let start = std::time::Instant::now();
            let timeout = Duration::from_secs(if force { 2 } else { 10 });

            while start.elapsed() < timeout {
                if !is_process_running(pid) {
                    remove_pid_file(port);
                    println!("EAVS server stopped successfully");
                    return Ok(());
                }
                tokio::time::sleep(Duration::from_millis(100)).await;
            }

            // If graceful shutdown failed, try force kill
            if !force {
                println!("Graceful shutdown timed out, forcing...");
                kill_process(pid, true)?;
                tokio::time::sleep(Duration::from_millis(500)).await;
            }

            remove_pid_file(port);

            if is_process_running(pid) {
                return Err(CliError::Other(format!(
                    "Failed to stop EAVS server (PID: {})",
                    pid
                )));
            }

            println!("EAVS server stopped successfully");
            Ok(())
        }
        None => {
            // Check if something is responding on the port
            if is_eavs_server_running(&url).await {
                println!("EAVS server is running but PID unknown. Try: kill $(lsof -ti:{})", port);
                return Err(CliError::Other(
                    "Could not determine EAVS server PID".to_string(),
                ));
            }
            println!("EAVS server is not running on port {}", port);
            Ok(())
        }
    }
}

/// Restart the EAVS service
pub async fn run_service_restart(port: u16, config_path: Option<String>) -> Result<(), CliError> {
    println!("Restarting EAVS server...");

    // Stop if running
    let _ = run_service_stop(port, false).await;

    // Small delay to ensure port is released
    tokio::time::sleep(Duration::from_millis(500)).await;

    // Start again
    run_service_start(port, config_path, true).await
}

/// Get the status of the EAVS service
pub async fn run_service_status(port: u16, format: OutputFormat) -> Result<(), CliError> {
    let url = format!("http://127.0.0.1:{}", port);
    let pid = read_pid_file(port).or_else(|| find_eavs_pid_by_port(port));

    let running = is_eavs_server_running(&url).await;

    // Get additional info if running
    let (uptime, providers) = if running {
        let client = EavsClient::with_url(url.clone());
        let health = client.health().await.ok();
        let providers = client.providers().await.unwrap_or_default();
        (health.and_then(|h| h.uptime_secs), providers)
    } else {
        (None, vec![])
    };

    let status = ServiceStatus {
        running,
        pid: if running { pid } else { None },
        port,
        url: url.clone(),
        uptime_secs: uptime,
        providers: providers.clone(),
    };

    match format {
        OutputFormat::Json => {
            println!("{}", serde_json::to_string_pretty(&status).unwrap());
        }
        OutputFormat::Text => {
            if running {
                println!("EAVS Server Status: RUNNING");
                println!("{}", "=".repeat(40));
                if let Some(pid) = status.pid {
                    println!("  PID:       {}", pid);
                }
                println!("  Port:      {}", port);
                println!("  URL:       {}", url);
                if let Some(uptime) = uptime {
                    let hours = uptime / 3600;
                    let minutes = (uptime % 3600) / 60;
                    let seconds = uptime % 60;
                    println!("  Uptime:    {}h {}m {}s", hours, minutes, seconds);
                }
                if !status.providers.is_empty() {
                    println!("  Providers: {}", status.providers.join(", "));
                }
            } else {
                println!("EAVS Server Status: STOPPED");
                println!("{}", "=".repeat(40));
                println!("  Port:      {}", port);

                // Check if port is in use by something else
                if !is_port_available(port) {
                    println!("  Note:      Port {} is in use by another application", port);
                }
            }
        }
    }

    Ok(())
}

/// Show EAVS logs (placeholder - needs file logging to be configured)
pub async fn run_service_logs(lines: usize, follow: bool) -> Result<(), CliError> {
    // Try to find log file from XDG state directory
    let log_file = xdg_state_dir().join("eavs").join("eavs.log");

    if !log_file.exists() {
        println!("No log file found at {:?}", log_file);
        println!();
        println!("To enable file logging, add to your config.toml:");
        println!();
        println!("  [[logging.backends]]");
        println!("  type = \"file\"");
        println!("  path = \"~/.local/state/eavs/eavs.log\"");
        println!("  rotate = \"daily\"");
        return Ok(());
    }

    if follow {
        // Use tail -f
        let mut cmd = std::process::Command::new("tail")
            .args(["-f", "-n", &lines.to_string()])
            .arg(&log_file)
            .spawn()
            .map_err(|e| CliError::Other(format!("Failed to tail logs: {}", e)))?;

        cmd.wait()
            .map_err(|e| CliError::Other(format!("Failed to wait for tail: {}", e)))?;
    } else {
        // Use tail
        let output = std::process::Command::new("tail")
            .args(["-n", &lines.to_string()])
            .arg(&log_file)
            .output()
            .map_err(|e| CliError::Other(format!("Failed to read logs: {}", e)))?;

        print!("{}", String::from_utf8_lossy(&output.stdout));
    }

    Ok(())
}

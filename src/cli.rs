//! CLI module for EAVS.
//!
//! Provides subcommands for:
//! - `serve` - Run the proxy server (foreground)
//! - `service` - Manage the proxy server as a background service
//! - `key` - Manage virtual API keys
//! - `test` - Test the proxy functionality

use clap::{Parser, Subcommand};
use base64::Engine;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::time::Duration;

/// Parse SSE stream response and extract content from delta chunks.
/// Returns the accumulated content from all chunks.
fn parse_sse_stream_content(text: &str) -> String {
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
    
    content
}

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
    /// Switch provider shortcuts for the default endpoint
    Provider {
        #[command(subcommand)]
        action: ProviderCommands,
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
        /// Port to bind to (overrides config file)
        #[arg(short, long, env = "EAVS_PORT")]
        port: Option<u16>,

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
        #[arg(short, long, env = "EAVS_PORT")]
        port: Option<u16>,

        /// Force kill if graceful shutdown fails
        #[arg(short, long)]
        force: bool,
    },

    /// Restart the EAVS server
    Restart {
        /// Port to bind to (overrides config file)
        #[arg(short, long, env = "EAVS_PORT")]
        port: Option<u16>,

        /// Path to config file
        #[arg(short, long, env = "EAVS_CONFIG")]
        config: Option<String>,
    },

    /// Show the status of the EAVS server
    Status {
        /// Port to check
        #[arg(short, long, env = "EAVS_PORT")]
        port: Option<u16>,

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

        /// EAVS server URL (default: from config file or http://127.0.0.1:3000)
        #[arg(long, env = "EAVS_URL")]
        url: Option<String>,

        /// Path to config file to use when auto-starting the server
        #[arg(long, env = "EAVS_CONFIG")]
        config: Option<String>,
    },

    /// List all virtual API keys
    List {
        /// Include disabled keys
        #[arg(long)]
        all: bool,

        /// Output format
        #[arg(long, default_value = "text")]
        format: OutputFormat,

        /// EAVS server URL (default: from config file or http://127.0.0.1:3000)
        #[arg(long, env = "EAVS_URL")]
        url: Option<String>,

        /// Path to config file to use when auto-starting the server
        #[arg(long, env = "EAVS_CONFIG")]
        config: Option<String>,
    },

    /// Get information about a key
    Info {
        /// Key hash or key ID prefix
        key: String,

        /// Output format
        #[arg(long, default_value = "text")]
        format: OutputFormat,

        /// EAVS server URL (default: from config file or http://127.0.0.1:3000)
        #[arg(long, env = "EAVS_URL")]
        url: Option<String>,

        /// Path to config file to use when auto-starting the server
        #[arg(long, env = "EAVS_CONFIG")]
        config: Option<String>,
    },

    /// Revoke (disable) a key
    Revoke {
        /// Key hash or key ID prefix
        key: String,

        /// Skip confirmation prompt
        #[arg(short, long)]
        yes: bool,

        /// EAVS server URL (default: from config file or http://127.0.0.1:3000)
        #[arg(long, env = "EAVS_URL")]
        url: Option<String>,

        /// Path to config file to use when auto-starting the server
        #[arg(long, env = "EAVS_CONFIG")]
        config: Option<String>,
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

        /// EAVS server URL (default: from config file or http://127.0.0.1:3000)
        #[arg(long, env = "EAVS_URL")]
        url: Option<String>,

        /// Path to config file to use when auto-starting the server
        #[arg(long, env = "EAVS_CONFIG")]
        config: Option<String>,
    },

    /// Bind or clear an OAuth user for a key
    Bind {
        /// Key hash or key ID prefix
        key: String,

        /// OAuth user id to bind (for OAuth-backed providers)
        #[arg(long)]
        oauth_user: Option<String>,

        /// Clear the OAuth user binding
        #[arg(long, conflicts_with = "oauth_user")]
        clear: bool,

        /// Output format
        #[arg(long, default_value = "text")]
        format: OutputFormat,

        /// Path to config file to use when resolving the key database
        #[arg(long, env = "EAVS_CONFIG")]
        config: Option<String>,
    },
}

#[derive(Subcommand)]
pub enum ProviderCommands {
    /// Show the current default provider override
    Current,

    /// Set the default provider override (applies to auto endpoint)
    Use {
        /// Provider name (must exist in config)
        provider: String,

        /// Path to config file to validate provider name
        #[arg(long, env = "EAVS_CONFIG")]
        config: Option<String>,
    },

    /// Clear the default provider override
    Clear,

    /// List providers available in the config file
    List {
        /// Path to config file to read provider names from
        #[arg(long, env = "EAVS_CONFIG")]
        config: Option<String>,
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

    /// Send a vision/multimodal request with an image
    Image {
        /// Path to an image file (png/jpg/webp/gif)
        image: String,

        /// Prompt to send alongside the image
        prompt: String,

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

    /// Send a request that encourages a tool call
    ToolCall {
        /// Prompt to send
        #[arg(default_value = "What's the weather in Paris? Call get_weather(city) as needed.")]
        prompt: String,

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

        /// Provider to use (use "mock" for cost-free benchmarking)
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

        /// Number of concurrent requests (default: 1 = sequential)
        #[arg(long, default_value = "1")]
        concurrent: u32,

        /// Duration for sustained load test (e.g., "30s", "1m"). Overrides --count.
        #[arg(long)]
        duration: Option<String>,

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

    /// Test provider routing mechanisms
    Routing {
        /// Provider to test routing for
        #[arg(short, long, default_value = "default")]
        provider: String,

        /// Model to use (for auto-detection test)
        #[arg(short, long)]
        model: Option<String>,

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

/// Configuration for CLI client operations (used for test/health commands that need server)
pub struct CliConfig {
    pub server_url: String,
    pub timeout: Duration,
}

impl Default for CliConfig {
    fn default() -> Self {
        Self::from_config(None)
    }
}

impl CliConfig {
    /// Create a CliConfig by loading settings from config file.
    /// Priority: EAVS_URL env > config file > default (127.0.0.1:3000)
    pub fn from_config(config_path: Option<&str>) -> Self {
        // Try to load config for port
        let config = if let Some(path) = config_path {
            crate::config::AppConfig::load_from(path).ok()
        } else {
            crate::config::AppConfig::load().ok()
        };

        // EAVS_URL env var takes highest priority for URL
        let server_url = if let Ok(url) = std::env::var("EAVS_URL") {
            if !url.trim().is_empty() {
                url
            } else {
                let port = config.as_ref().map(|c| c.server.port).unwrap_or(3000);
                format!("http://127.0.0.1:{}", port)
            }
        } else {
            let port = config.as_ref().map(|c| c.server.port).unwrap_or(3000);
            format!("http://127.0.0.1:{}", port)
        };

        Self {
            server_url,
            timeout: Duration::from_secs(30),
        }
    }
}

/// Client for talking to EAVS server (used for test/health commands)
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

    async fn post_chat_completions(
        &self,
        body: &serde_json::Value,
        provider: &str,
        api_key: Option<&str>,
    ) -> Result<reqwest::Response, CliError> {
        let url = format!("{}/v1/chat/completions", self.config.server_url);

        let mut req = self
            .client
            .post(&url)
            .header("Content-Type", "application/json")
            .header("X-Provider", provider)
            .json(body);

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

        Ok(resp)
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
        let body = serde_json::json!({
            "model": model,
            "messages": [{"role": "user", "content": message}],
            "stream": stream,
            "max_tokens": 256
        });
        let resp = self.post_chat_completions(&body, provider, api_key).await?;

        if stream {
            // For streaming, collect the full response using helper
            let text = resp.text().await.map_err(CliError::Request)?;
            let content = parse_sse_stream_content(&text);

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

    /// Send a multimodal request (OpenAI vision format).
    pub async fn chat_with_image(
        &self,
        prompt: &str,
        image_path: &str,
        model: &str,
        provider: &str,
        api_key: Option<&str>,
        stream: bool,
    ) -> Result<ChatResponse, CliError> {
        // Use async file I/O to avoid blocking the tokio runtime
        let bytes = tokio::fs::read(image_path).await.map_err(CliError::Io)?;
        let mime = guess_image_mime(image_path).ok_or_else(|| {
            CliError::Other("Unsupported image extension (expected png/jpg/jpeg/webp/gif)".to_string())
        })?;
        let b64 = base64::engine::general_purpose::STANDARD.encode(bytes);
        let data_url = format!("data:{};base64,{}", mime, b64);

        let body = serde_json::json!({
            "model": model,
            "messages": [{
                "role": "user",
                "content": [
                    {"type": "text", "text": prompt},
                    {"type": "image_url", "image_url": {"url": data_url}}
                ]
            }],
            "stream": stream,
            "max_tokens": 256
        });

        let resp = self.post_chat_completions(&body, provider, api_key).await?;

        if stream {
            // For streaming, collect the full response using helper
            let text = resp.text().await.map_err(CliError::Request)?;
            let content = parse_sse_stream_content(&text);

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

    /// Send a request that encourages a tool call and return the raw response JSON.
    pub async fn tool_call(
        &self,
        prompt: &str,
        model: &str,
        provider: &str,
        api_key: Option<&str>,
        stream: bool,
    ) -> Result<serde_json::Value, CliError> {
        let body = serde_json::json!({
            "model": model,
            "messages": [{"role": "user", "content": prompt}],
            "tools": [{
                "type": "function",
                "function": {
                    "name": "get_weather",
                    "description": "Get the weather for a city",
                    "parameters": {
                        "type": "object",
                        "properties": {"city": {"type": "string"}},
                        "required": ["city"]
                    }
                }
            }],
            "tool_choice": "auto",
            "stream": stream,
            "max_tokens": 256
        });

        let resp = self.post_chat_completions(&body, provider, api_key).await?;

        if stream {
            Ok(serde_json::json!({
                "stream": true,
                "raw": resp.text().await.map_err(CliError::Request)?
            }))
        } else {
            resp.json().await.map_err(CliError::Request)
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
    Io(std::io::Error),
    Other(String),
}

impl std::fmt::Display for CliError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Request(e) => write!(f, "Request error: {}", e),
            Self::Api { status, message } => write!(f, "API error ({}): {}", status, message),
            Self::Io(e) => write!(f, "IO error: {}", e),
            Self::Other(msg) => write!(f, "{}", msg),
        }
    }
}

impl std::error::Error for CliError {}

// Parse expiration string like "30d", "24h", "never" into ISO timestamp
// =============================================================================
// Direct Database Key Management (no server required)
// =============================================================================

use crate::keys::{KeyStore, CreateKeyRequest, KeyPermissions};
use std::collections::HashSet;

/// Parse expiration string to DateTime
fn parse_expiration_datetime(s: &str) -> Option<chrono::DateTime<chrono::Utc>> {
    let s = s.trim().to_lowercase();
    if s.is_empty() || s == "never" || s == "none" {
        return None;
    }

    // Parse duration like "30d", "24h", "60m"
    let (num_str, unit) = s.split_at(s.len().saturating_sub(1));
    let num: i64 = num_str.parse().ok()?;

    let duration = match unit {
        "d" => chrono::Duration::days(num),
        "h" => chrono::Duration::hours(num),
        "m" => chrono::Duration::minutes(num),
        _ => return None,
    };

    Some(chrono::Utc::now() + duration)
}

#[allow(dead_code)]
fn parse_expiration(s: &str) -> Option<String> {
    parse_expiration_datetime(s).map(|dt| dt.to_rfc3339())
}

/// Create a key directly in the database (no server required)
#[allow(clippy::too_many_arguments)]
pub async fn run_key_create_direct(
    store: &KeyStore,
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
    let request = CreateKeyRequest {
        name,
        expires_at: parse_expiration_datetime(&expires),
        permissions: KeyPermissions {
            allowed_models: if models.is_empty() { None } else { Some(models.into_iter().collect::<HashSet<_>>()) },
            blocked_models: if blocked_models.is_empty() { None } else { Some(blocked_models.into_iter().collect::<HashSet<_>>()) },
            allowed_providers: if providers.is_empty() { None } else { Some(providers.into_iter().collect::<HashSet<_>>()) },
            rpm_limit: rpm,
            tpm_limit: tpm,
            rpd_limit: rpd,
            max_budget_usd: budget,
            budget_window: None,
        },
        metadata: serde_json::Value::Null,
        oauth_user: None,
    };

    let response = store.create_key(request).await.map_err(|e| {
        CliError::Other(format!("Failed to create key: {}", e))
    })?;

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

/// List keys directly from the database (no server required)
pub async fn run_key_list_direct(
    store: &KeyStore,
    _include_disabled: bool,  // TODO: implement filtering
    format: OutputFormat,
) -> Result<(), CliError> {
    let keys = store.list_keys().await.map_err(|e| {
        CliError::Other(format!("Failed to list keys: {}", e))
    })?;

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
                "{:<15} {:<20} {:<10} {:<20}",
                "KEY ID", "NAME", "STATUS", "CREATED"
            );
            println!("{}", "-".repeat(70));

            for key in keys {
                let status = if key.disabled { "disabled" } else { "active" };
                let name = key.name.unwrap_or_else(|| "-".to_string());
                let name_display = if name.len() > 20 { 
                    format!("{}...", &name[..17])
                } else { 
                    name 
                };
                let created = key.created_at.format("%Y-%m-%d %H:%M:%S").to_string();
                println!(
                    "{:<15} {:<20} {:<10} {:<20}",
                    key.key_id,
                    name_display,
                    status,
                    created
                );
            }
        }
    }

    Ok(())
}

/// Get key info directly from the database (no server required)
pub async fn run_key_info_direct(
    store: &KeyStore,
    key_id: &str,
    format: OutputFormat,
) -> Result<(), CliError> {
    // Try to find by human ID first, then by hash prefix
    let key = store.get_by_human_id(key_id)
        .or_else(|| store.get_by_hash(key_id))
        .ok_or_else(|| CliError::Other(format!("Key not found: {}", key_id)))?;

    let info = key.to_info();

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
            println!(
                "OAuth User:  {}",
                info.oauth_user.unwrap_or_else(|| "-".to_string())
            );
            println!("Created:     {}", info.created_at);
            println!(
                "Expires:     {}",
                info.expires_at.map(|d| d.to_string()).unwrap_or_else(|| "never".to_string())
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

/// Revoke a key directly in the database (no server required)
pub async fn run_key_revoke_direct(
    store: &KeyStore,
    key_id: &str,
    yes: bool,
) -> Result<(), CliError> {
    // Find the key first
    let key = store.get_by_human_id(key_id)
        .or_else(|| store.get_by_hash(key_id))
        .ok_or_else(|| CliError::Other(format!("Key not found: {}", key_id)))?;

    if !yes {
        eprint!("Are you sure you want to revoke key '{}'? [y/N] ", key.key_id);
        let mut input = String::new();
        std::io::stdin().read_line(&mut input).unwrap();
        if !input.trim().eq_ignore_ascii_case("y") {
            println!("Cancelled.");
            return Ok(());
        }
    }

    store.disable_key(&key.key_hash).await.map_err(|e| {
        CliError::Other(format!("Failed to revoke key: {}", e))
    })?;

    println!("Key '{}' has been revoked.", key.key_id);

    Ok(())
}

/// Get usage history directly from the database (no server required)
pub async fn run_key_usage_direct(
    store: &KeyStore,
    key_id: &str,
    days: u32,
    format: OutputFormat,
) -> Result<(), CliError> {
    // Find the key first
    let key = store.get_by_human_id(key_id)
        .or_else(|| store.get_by_hash(key_id))
        .ok_or_else(|| CliError::Other(format!("Key not found: {}", key_id)))?;

    let records = store.get_usage_history(&key.key_hash, Some(days)).await.map_err(|e| {
        CliError::Other(format!("Failed to get usage history: {}", e))
    })?;

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
                let ts = record.timestamp.format("%Y-%m-%d %H:%M:%S").to_string();
                let model_display = if record.model.len() > 15 { 
                    format!("{}...", &record.model[..12])
                } else { 
                    record.model.clone() 
                };
                println!(
                    "{:<20} {:<15} {:<10} {:<10} ${:<9.4}",
                    ts,
                    model_display,
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

/// Bind or clear OAuth user for a key directly in the database.
pub async fn run_key_bind_direct(
    store: &KeyStore,
    key_id: &str,
    oauth_user: Option<String>,
    format: OutputFormat,
) -> Result<(), CliError> {
    let key = store
        .get_by_human_id(key_id)
        .or_else(|| store.get_by_hash(key_id))
        .ok_or_else(|| CliError::Other(format!("Key not found: {}", key_id)))?;

    let updated = store
        .update_oauth_user(&key.key_hash, oauth_user.clone())
        .await
        .map_err(|e| CliError::Other(format!("Failed to update key: {}", e)))?;

    if !updated {
        return Err(CliError::Other(format!("Key not found: {}", key_id)));
    }

    match format {
        OutputFormat::Json => {
            let response = serde_json::json!({
                "key_id": key.key_id,
                "key_hash": key.key_hash,
                "oauth_user": oauth_user,
            });
            println!("{}", serde_json::to_string_pretty(&response).unwrap());
        }
        OutputFormat::Text => {
            if let Some(user) = oauth_user {
                println!("Key '{}' is now bound to OAuth user '{}'.", key.key_id, user);
            } else {
                println!("OAuth binding cleared for key '{}'.", key.key_id);
            }
        }
    }

    Ok(())
}

pub fn run_provider_current() -> Result<(), CliError> {
    let state = crate::runtime_state::load_runtime_state().unwrap_or_default();
    if let Some(provider) = state.default_provider {
        println!("{}", provider);
    } else {
        println!("default");
    }
    Ok(())
}

pub fn run_provider_use(provider: &str, config_path: Option<&str>) -> Result<(), CliError> {
    let config = if let Some(path) = config_path {
        crate::config::AppConfig::load_from(path).ok()
    } else {
        crate::config::AppConfig::load().ok()
    };

    if let Some(cfg) = config {
        if cfg.resolve_provider(provider).is_none() {
            let available = cfg.provider_names();
            return Err(CliError::Other(format!(
                "Unknown provider '{}'. Available providers: {:?}",
                provider, available
            )));
        }
    }

    let mut state = crate::runtime_state::load_runtime_state().unwrap_or_default();
    state.default_provider = Some(provider.to_string());
    crate::runtime_state::save_runtime_state(&state)
        .map_err(CliError::Other)?;
    println!("{}", provider);
    Ok(())
}

pub fn run_provider_clear() -> Result<(), CliError> {
    let mut state = crate::runtime_state::load_runtime_state().unwrap_or_default();
    state.default_provider = None;
    crate::runtime_state::save_runtime_state(&state)
        .map_err(CliError::Other)?;
    println!("default");
    Ok(())
}

pub fn run_provider_list(config_path: Option<&str>) -> Result<(), CliError> {
    let config = if let Some(path) = config_path {
        crate::config::AppConfig::load_from(path).map_err(|e| {
            CliError::Other(format!("Failed to load config from {}: {}", path, e))
        })?
    } else {
        crate::config::AppConfig::load().map_err(|e| {
            CliError::Other(format!("Failed to load config: {}", e))
        })?
    };

    let mut providers = config
        .provider_names()
        .into_iter()
        .map(|p| p.to_string())
        .collect::<Vec<_>>();
    providers.sort();
    for provider in providers {
        println!("{}", provider);
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
    let start = std::time::Instant::now();

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

    println!("Timing: {:.2?}", start.elapsed());

    if let Some(usage) = response.usage {
        println!(
            "Usage: {} prompt + {} completion tokens",
            usage.prompt_tokens, usage.completion_tokens
        );
    }

    Ok(())
}

pub async fn run_test_image(
    client: &EavsClient,
    image: String,
    prompt: String,
    model: String,
    provider: String,
    api_key: Option<String>,
    stream: bool,
) -> Result<(), CliError> {
    let start = std::time::Instant::now();

    println!("Sending image request to EAVS...");
    println!("  Provider: {}", provider);
    println!("  Model: {}", model);
    println!("  Stream: {}", stream);
    println!("  Image: {}", image);
    println!();

    let response = client
        .chat_with_image(&prompt, &image, &model, &provider, api_key.as_deref(), stream)
        .await?;

    println!("Response:");
    println!("{}", response.content);
    println!();

    println!("Timing: {:.2?}", start.elapsed());

    if let Some(usage) = response.usage {
        println!(
            "Usage: {} prompt + {} completion tokens",
            usage.prompt_tokens, usage.completion_tokens
        );
    }

    Ok(())
}

pub async fn run_test_tool_call(
    client: &EavsClient,
    prompt: String,
    model: String,
    provider: String,
    api_key: Option<String>,
    stream: bool,
) -> Result<(), CliError> {
    let start = std::time::Instant::now();

    println!("Sending tool-call request to EAVS...");
    println!("  Provider: {}", provider);
    println!("  Model: {}", model);
    println!("  Stream: {}", stream);
    println!();

    let response = client
        .tool_call(&prompt, &model, &provider, api_key.as_deref(), stream)
        .await?;

    println!("Response:");
    println!("{}", serde_json::to_string_pretty(&response).unwrap_or_default());
    println!();

    println!("Timing: {:.2?}", start.elapsed());

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

/// Result of a single routing test
#[derive(Debug, Serialize)]
pub struct RoutingTestResult {
    pub method: String,
    pub success: bool,
    pub resolved_provider: Option<String>,
    pub error: Option<String>,
}

/// Results of all routing tests
#[derive(Debug, Serialize)]
pub struct RoutingTestResults {
    pub provider_prefix: RoutingTestResult,
    pub x_provider_header: RoutingTestResult,
    pub model_auto_detect: Option<RoutingTestResult>,
}

/// Test provider routing mechanisms
pub async fn run_test_routing(
    server_url: &str,
    provider: &str,
    model: Option<String>,
    format: OutputFormat,
) -> Result<(), CliError> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
        .expect("Failed to create HTTP client");

    println!("Testing Provider Routing");
    println!("{}", "=".repeat(50));
    println!("Server: {}", server_url);
    println!("Provider: {}", provider);
    if let Some(ref m) = model {
        println!("Model: {}", m);
    }
    println!();

    // Test 1: Provider-prefixed path (e.g., /openai/v1/models)
    println!("1. Testing provider-prefixed path: /{}/v1/models", provider);
    let prefix_result = test_routing_method(
        &client,
        &format!("{}/{}/v1/models", server_url, provider),
        None,
        "provider-prefixed path",
    ).await;

    // Test 2: X-Provider header
    println!("2. Testing X-Provider header: /v1/models with X-Provider: {}", provider);
    let header_result = test_routing_method(
        &client,
        &format!("{}/v1/models", server_url),
        Some(provider),
        "X-Provider header",
    ).await;

    // Test 3: Model auto-detection (only if model is provided)
    let auto_detect_result = if let Some(ref m) = model {
        // Use provider detection to predict expected provider
        let expected_provider = crate::provider::detect_provider_from_model(m);
        println!("3. Testing model auto-detection with model: {}", m);
        if let Some(ref expected) = expected_provider {
            println!("   Expected provider from model: {}", expected);
        } else {
            println!("   No provider auto-detected from model name");
        }

        // Make a request with the model to see what provider it resolves to
        // We use chat/completions with a minimal body
        let body = serde_json::json!({
            "model": m,
            "messages": [{"role": "user", "content": "test"}],
            "max_tokens": 1
        });

        let resp = client
            .post(format!("{}/v1/chat/completions", server_url))
            .header("Content-Type", "application/json")
            .json(&body)
            .send()
            .await;

        let result = match resp {
            Ok(r) => {
                let resolved = r.headers()
                    .get("x-eavs-provider")
                    .and_then(|v| v.to_str().ok())
                    .map(|s| s.to_string());
                
                // Consider it a success if we got a provider header back
                // (even if the actual API call failed due to missing API key)
                RoutingTestResult {
                    method: "model auto-detection".to_string(),
                    success: resolved.is_some(),
                    resolved_provider: resolved,
                    error: None,
                }
            }
            Err(e) => RoutingTestResult {
                method: "model auto-detection".to_string(),
                success: false,
                resolved_provider: None,
                error: Some(e.to_string()),
            },
        };
        Some(result)
    } else {
        println!("3. Model auto-detection: skipped (no --model provided)");
        None
    };

    let results = RoutingTestResults {
        provider_prefix: prefix_result,
        x_provider_header: header_result,
        model_auto_detect: auto_detect_result,
    };

    println!();
    println!("Results");
    println!("{}", "=".repeat(50));

    match format {
        OutputFormat::Json => {
            println!("{}", serde_json::to_string_pretty(&results).unwrap());
        }
        OutputFormat::Text => {
            print_routing_result(&results.provider_prefix);
            print_routing_result(&results.x_provider_header);
            if let Some(ref auto) = results.model_auto_detect {
                print_routing_result(auto);
            }
            
            println!();
            let successes = [
                results.provider_prefix.success,
                results.x_provider_header.success,
                results.model_auto_detect.as_ref().map(|r| r.success).unwrap_or(true),
            ].iter().filter(|&&s| s).count();
            
            let total = if results.model_auto_detect.is_some() { 3 } else { 2 };
            println!("Summary: {}/{} routing methods working", successes, total);
        }
    }

    Ok(())
}

/// Test a single routing method
async fn test_routing_method(
    client: &reqwest::Client,
    url: &str,
    x_provider_header: Option<&str>,
    method_name: &str,
) -> RoutingTestResult {
    let mut req = client.get(url);
    
    if let Some(provider) = x_provider_header {
        req = req.header("X-Provider", provider);
    }

    match req.send().await {
        Ok(resp) => {
            let resolved = resp.headers()
                .get("x-eavs-provider")
                .and_then(|v| v.to_str().ok())
                .map(|s| s.to_string());
            
            let status = resp.status();
            
            // Consider success if we got the provider header (routing worked)
            // even if status is 401/403 (auth issue, not routing issue)
            let success = resolved.is_some() || status.is_success();
            
            let error = if !status.is_success() && resolved.is_none() {
                Some(format!("HTTP {}", status))
            } else {
                None
            };
            
            RoutingTestResult {
                method: method_name.to_string(),
                success,
                resolved_provider: resolved,
                error,
            }
        }
        Err(e) => RoutingTestResult {
            method: method_name.to_string(),
            success: false,
            resolved_provider: None,
            error: Some(e.to_string()),
        },
    }
}

/// Print a single routing test result in text format
fn print_routing_result(result: &RoutingTestResult) {
    let status = if result.success { "OK" } else { "FAIL" };
    print!("  {}: {}", result.method, status);
    
    if let Some(ref provider) = result.resolved_provider {
        print!(" (resolved to: {})", provider);
    }
    
    if let Some(ref err) = result.error {
        print!(" - {}", err);
    }
    
    println!();
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

fn guess_image_mime(path: &str) -> Option<&'static str> {
    let lower = path.to_lowercase();
    if lower.ends_with(".png") {
        Some("image/png")
    } else if lower.ends_with(".jpg") || lower.ends_with(".jpeg") {
        Some("image/jpeg")
    } else if lower.ends_with(".webp") {
        Some("image/webp")
    } else if lower.ends_with(".gif") {
        Some("image/gif")
    } else {
        None
    }
}

/// Parse duration string like "30s", "1m", "5m30s" into Duration
fn parse_duration_string(s: &str) -> Option<Duration> {
    let s = s.trim().to_lowercase();
    let mut total_secs = 0u64;
    let mut current_num = String::new();
    
    for c in s.chars() {
        if c.is_ascii_digit() {
            current_num.push(c);
        } else {
            let num: u64 = current_num.parse().ok()?;
            current_num.clear();
            match c {
                's' => total_secs += num,
                'm' => total_secs += num * 60,
                'h' => total_secs += num * 3600,
                _ => return None,
            }
        }
    }
    
    // Handle case where just a number is given (assume seconds)
    if !current_num.is_empty() {
        total_secs += current_num.parse::<u64>().ok()?;
    }
    
    if total_secs > 0 {
        Some(Duration::from_secs(total_secs))
    } else {
        None
    }
}

/// Concurrent benchmark results with throughput metrics
#[derive(Debug, Serialize)]
pub struct ConcurrentBenchmarkResults {
    pub target: String,
    pub concurrency: u32,
    pub duration_secs: f64,
    pub total_requests: u32,
    pub successful: u32,
    pub failed: u32,
    pub requests_per_second: f64,
    pub latencies_ms: Vec<f64>,
    pub min_ms: f64,
    pub max_ms: f64,
    pub mean_ms: f64,
    pub median_ms: f64,
    pub p95_ms: f64,
    pub p99_ms: f64,
    pub stddev_ms: f64,
}

impl ConcurrentBenchmarkResults {
    fn from_results(
        target: String,
        concurrency: u32,
        duration_secs: f64,
        latencies: Vec<f64>,
        failed: u32,
    ) -> Self {
        let successful = latencies.len() as u32;
        let total_requests = successful + failed;
        let requests_per_second = if duration_secs > 0.0 {
            total_requests as f64 / duration_secs
        } else {
            0.0
        };

        if latencies.is_empty() {
            return Self {
                target,
                concurrency,
                duration_secs,
                total_requests,
                successful: 0,
                failed,
                requests_per_second,
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
            concurrency,
            duration_secs,
            total_requests,
            successful,
            failed,
            requests_per_second,
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
        println!("  Concurrency:       {}", self.concurrency);
        println!("  Duration:          {:.2}s", self.duration_secs);
        println!("  Total requests:    {} ({} successful, {} failed)", 
                 self.total_requests, self.successful, self.failed);
        println!("  Throughput:        {:.2} req/s", self.requests_per_second);
        if self.successful > 0 {
            println!("  Latency:");
            println!("    Min:             {:.2}ms", self.min_ms);
            println!("    Max:             {:.2}ms", self.max_ms);
            println!("    Mean:            {:.2}ms", self.mean_ms);
            println!("    Median:          {:.2}ms", self.median_ms);
            println!("    P95:             {:.2}ms", self.p95_ms);
            println!("    P99:             {:.2}ms", self.p99_ms);
            println!("    Std Dev:         {:.2}ms", self.stddev_ms);
        }
    }
}

#[allow(clippy::too_many_arguments)]
pub async fn run_test_bench(
    count: u32,
    provider: String,
    model: String,
    api_key: Option<String>,
    compare_direct: bool,
    direct_url: String,
    direct_key: Option<String>,
    stream: bool,
    concurrent: u32,
    duration: Option<String>,
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

    // Parse duration if provided
    let test_duration = duration.as_ref().and_then(|d| parse_duration_string(d));
    let is_duration_test = test_duration.is_some();
    let concurrent = concurrent.max(1);

    println!("EAVS Benchmark");
    println!("{}", "=".repeat(50));
    println!("Model: {}", model);
    println!("Provider: {}", provider);
    if is_duration_test {
        println!("Duration: {:?}", test_duration.unwrap());
    } else {
        println!("Requests: {}", count);
    }
    println!("Concurrency: {}", concurrent);
    println!("Streaming: {}", stream);
    println!();

    // Warm-up request
    println!("Warming up...");
    let endpoint_url = format!("{}/v1/chat/completions", eavs_url);
    let _ = timed_request(&client, &endpoint_url, &body, api_key.as_deref(), Some(&provider)).await;

    // Run benchmark
    let eavs_results = if concurrent == 1 {
        // Sequential benchmark (original behavior)
        run_sequential_benchmark(
            &client,
            &endpoint_url,
            &body,
            api_key.as_deref(),
            Some(&provider),
            count,
            test_duration,
            format!("EAVS ({})", eavs_url),
        ).await
    } else {
        // Concurrent benchmark
        run_concurrent_benchmark(
            &client,
            &endpoint_url,
            &body,
            api_key.as_deref(),
            Some(&provider),
            count,
            concurrent,
            test_duration,
            format!("EAVS ({})", eavs_url),
        ).await
    };

    // Optionally benchmark direct provider access
    let direct_results = if compare_direct {
        let direct_api_key = direct_key.ok_or_else(|| {
            CliError::Other("--direct-key is required for direct comparison".to_string())
        })?;

        println!();
        println!("Benchmarking direct provider ({})...", direct_url);

        let direct_endpoint = format!("{}/v1/chat/completions", direct_url);
        
        if concurrent == 1 {
            Some(run_sequential_benchmark(
                &client,
                &direct_endpoint,
                &body,
                Some(&direct_api_key),
                None,
                count,
                test_duration,
                format!("Direct ({})", direct_url),
            ).await)
        } else {
            Some(run_concurrent_benchmark(
                &client,
                &direct_endpoint,
                &body,
                Some(&direct_api_key),
                None,
                count,
                concurrent,
                test_duration,
                format!("Direct ({})", direct_url),
            ).await)
        }
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
                println!("  Median overhead: {:.2}ms", eavs_results.median_ms - direct.median_ms);
            }
        }
    }

    Ok(())
}

/// Run sequential benchmark (original behavior)
#[allow(clippy::too_many_arguments)]
async fn run_sequential_benchmark(
    client: &reqwest::Client,
    url: &str,
    body: &serde_json::Value,
    api_key: Option<&str>,
    provider: Option<&str>,
    count: u32,
    duration: Option<Duration>,
    target_name: String,
) -> ConcurrentBenchmarkResults {
    println!("Benchmarking {} (sequential)...", target_name);
    
    let start_time = std::time::Instant::now();
    let mut latencies = Vec::with_capacity(count as usize);
    let mut failed = 0u32;
    let mut completed = 0u32;

    loop {
        // Check termination conditions
        if let Some(dur) = duration {
            if start_time.elapsed() >= dur {
                break;
            }
        } else if completed >= count {
            break;
        }

        match timed_request(client, url, body, api_key, provider).await {
            Ok(dur) => {
                latencies.push(dur.as_secs_f64() * 1000.0);
                print!(".");
            }
            Err(_) => {
                failed += 1;
                print!("X");
            }
        }
        completed += 1;
        
        if completed.is_multiple_of(10) {
            if duration.is_some() {
                println!(" [{} in {:.1}s]", completed, start_time.elapsed().as_secs_f64());
            } else {
                println!(" [{}/{}]", completed, count);
            }
        }
        std::io::Write::flush(&mut std::io::stdout()).ok();
    }
    
    if !completed.is_multiple_of(10) {
        println!();
    }

    ConcurrentBenchmarkResults::from_results(
        target_name,
        1,
        start_time.elapsed().as_secs_f64(),
        latencies,
        failed,
    )
}

/// Run concurrent benchmark
#[allow(clippy::too_many_arguments)]
async fn run_concurrent_benchmark(
    client: &reqwest::Client,
    url: &str,
    body: &serde_json::Value,
    api_key: Option<&str>,
    provider: Option<&str>,
    count: u32,
    concurrency: u32,
    duration: Option<Duration>,
    target_name: String,
) -> ConcurrentBenchmarkResults {
    use std::sync::atomic::{AtomicU32, AtomicBool, Ordering};
    use std::sync::Arc;
    use tokio::sync::Mutex;

    println!("Benchmarking {} ({} concurrent)...", target_name, concurrency);

    let start_time = std::time::Instant::now();
    let latencies = Arc::new(Mutex::new(Vec::with_capacity(count as usize)));
    let failed = Arc::new(AtomicU32::new(0));
    let completed = Arc::new(AtomicU32::new(0));
    let should_stop = Arc::new(AtomicBool::new(false));
    
    // Spawn worker tasks
    let mut handles = Vec::new();
    
    for _ in 0..concurrency {
        let client = client.clone();
        let url = url.to_string();
        let body = body.clone();
        let api_key = api_key.map(|s| s.to_string());
        let provider = provider.map(|s| s.to_string());
        let latencies = latencies.clone();
        let failed = failed.clone();
        let completed = completed.clone();
        let should_stop = should_stop.clone();
        let start = start_time;
        
        let handle = tokio::spawn(async move {
            loop {
                // Check if we should stop
                if should_stop.load(Ordering::Relaxed) {
                    break;
                }
                
                // Check termination conditions
                if let Some(dur) = duration {
                    if start.elapsed() >= dur {
                        should_stop.store(true, Ordering::Relaxed);
                        break;
                    }
                } else {
                    let current = completed.load(Ordering::Relaxed);
                    if current >= count {
                        break;
                    }
                }
                
                // Make request
                let result = timed_request(
                    &client,
                    &url,
                    &body,
                    api_key.as_deref(),
                    provider.as_deref(),
                ).await;
                
                match result {
                    Ok(dur) => {
                        let mut lats = latencies.lock().await;
                        lats.push(dur.as_secs_f64() * 1000.0);
                    }
                    Err(_) => {
                        failed.fetch_add(1, Ordering::Relaxed);
                    }
                }
                completed.fetch_add(1, Ordering::Relaxed);
            }
        });
        
        handles.push(handle);
    }
    
    // Progress reporting task
    let completed_progress = completed.clone();
    let failed_progress = failed.clone();
    let should_stop_progress = should_stop.clone();
    let progress_handle = tokio::spawn(async move {
        let mut last_count = 0;
        loop {
            tokio::time::sleep(Duration::from_millis(500)).await;
            if should_stop_progress.load(Ordering::Relaxed) {
                break;
            }
            let current = completed_progress.load(Ordering::Relaxed);
            let fails = failed_progress.load(Ordering::Relaxed);
            if current > last_count {
                print!("\r  Progress: {} requests ({} failed)    ", current, fails);
                std::io::Write::flush(&mut std::io::stdout()).ok();
                last_count = current;
            }
        }
    });
    
    // Wait for all workers
    for handle in handles {
        let _ = handle.await;
    }
    should_stop.store(true, Ordering::Relaxed);
    let _ = progress_handle.await;
    
    println!();

    let final_latencies = latencies.lock().await.clone();
    let final_failed = failed.load(Ordering::Relaxed);
    
    ConcurrentBenchmarkResults::from_results(
        target_name,
        concurrency,
        start_time.elapsed().as_secs_f64(),
        final_latencies,
        final_failed,
    )
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
    port: Option<u16>,
    config_path: Option<&str>,
) -> Result<std::process::Child, CliError> {
    let exe_path = std::env::current_exe().map_err(|e| {
        CliError::Other(format!("Failed to get current executable path: {}", e))
    })?;

    let mut cmd = std::process::Command::new(&exe_path);
    cmd.arg("serve")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());

    // Only pass --port if explicitly specified (let config file determine port otherwise)
    if let Some(p) = port {
        cmd.arg("--port").arg(p.to_string());
    }

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
    // Use port from URL if specified, otherwise get from config
    let preferred_port = url.port().unwrap_or_else(|| get_effective_port(None, config_path));

    // First, check if EAVS is already running at the preferred URL
    let check_url = format!("{}://{}:{}", url.scheme(), host, preferred_port);
    if is_eavs_server_running(&check_url).await {
        return Ok(ServerStatus {
            url: check_url,
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

    // Start the server (don't pass port - let config determine it, unless we had to find a new port)
    eprintln!("Starting EAVS server on port {}...", port);
    let cli_port = if port != preferred_port { Some(port) } else { None };
    let _child = start_server_background(cli_port, config_path)?;

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
    pub capture_enabled: bool,
}

/// Get the effective port from CLI arg, env, or config file
pub fn get_effective_port(cli_port: Option<u16>, config_path: Option<&str>) -> u16 {
    // CLI/env takes precedence
    if let Some(p) = cli_port {
        return p;
    }
    
    // Try to load from config file
    let config = if let Some(path) = config_path {
        crate::config::AppConfig::load_from(path).ok()
    } else {
        crate::config::AppConfig::load().ok()
    };
    
    config.map(|c| c.server.port).unwrap_or(3000)
}

/// Start the EAVS service in the background
pub async fn run_service_start(
    port: Option<u16>,
    config_path: Option<String>,
    wait: bool,
) -> Result<(), CliError> {
    // Load config to check capture settings
    let config = if let Some(ref path) = config_path {
        crate::config::AppConfig::load_from(path).ok()
    } else {
        crate::config::AppConfig::load().ok()
    };
    let capture_enabled = config.as_ref().map(|c| c.capture.enabled).unwrap_or(false);

    // Determine the effective port (from CLI, env, or config)
    let effective_port = get_effective_port(port, config_path.as_deref());
    let url = format!("http://127.0.0.1:{}", effective_port);

    // Check if already running
    if is_eavs_server_running(&url).await {
        println!("EAVS server is already running on port {}", effective_port);
        return Ok(());
    }

    // Check if port is in use by something else
    if !is_port_available(effective_port) {
        return Err(CliError::Other(format!(
            "Port {} is already in use by another application",
            effective_port
        )));
    }

    // Start the server (pass the CLI port only if explicitly set)
    println!("Starting EAVS server on port {}...", effective_port);
    if capture_enabled {
        println!("  Transparent capture: enabled (mitmproxy will be started)");
    }
    let child = start_server_background(port, config_path.as_deref())?;

    // Write PID file
    write_pid_file(effective_port, child.id())?;

    if wait {
        // Wait for server to be ready
        let start = std::time::Instant::now();
        let timeout = Duration::from_secs(10);

        while start.elapsed() < timeout {
            if is_eavs_server_running(&url).await {
                println!("EAVS server started successfully (PID: {})", child.id());
                println!("  URL: {}", url);
                if capture_enabled {
                    println!("  Capture: mitmproxy running (intercepts LLM traffic)");
                }
                return Ok(());
            }
            tokio::time::sleep(Duration::from_millis(200)).await;
        }

        // Cleanup on failure
        remove_pid_file(effective_port);
        return Err(CliError::Other(format!(
            "Timed out waiting for EAVS server to start on port {}",
            effective_port
        )));
    } else {
        println!("EAVS server starting in background (PID: {})", child.id());
        if capture_enabled {
            println!("  Capture: mitmproxy will intercept LLM traffic");
        }
    }

    Ok(())
}

/// Stop the EAVS service
pub async fn run_service_stop(port: Option<u16>, force: bool) -> Result<(), CliError> {
    let effective_port = get_effective_port(port, None);
    let url = format!("http://127.0.0.1:{}", effective_port);

    // Try to find PID from file first, then by port
    let pid = read_pid_file(effective_port).or_else(|| find_eavs_pid_by_port(effective_port));

    match pid {
        Some(pid) => {
            if !is_process_running(pid) {
                println!("EAVS server is not running (stale PID file)");
                remove_pid_file(effective_port);
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
                    remove_pid_file(effective_port);
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

            remove_pid_file(effective_port);

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
                println!("EAVS server is running but PID unknown. Try: kill $(lsof -ti:{})", effective_port);
                return Err(CliError::Other(
                    "Could not determine EAVS server PID".to_string(),
                ));
            }
            println!("EAVS server is not running on port {}", effective_port);
            Ok(())
        }
    }
}

/// Restart the EAVS service
pub async fn run_service_restart(port: Option<u16>, config_path: Option<String>) -> Result<(), CliError> {
    println!("Restarting EAVS server...");

    // Stop if running (use config to find port if not specified)
    let _ = run_service_stop(port, false).await;

    // Small delay to ensure port is released
    tokio::time::sleep(Duration::from_millis(500)).await;

    // Start again
    run_service_start(port, config_path, true).await
}

/// Get the status of the EAVS service
pub async fn run_service_status(port: Option<u16>, format: OutputFormat) -> Result<(), CliError> {
    let effective_port = get_effective_port(port, None);
    let url = format!("http://127.0.0.1:{}", effective_port);
    let pid = read_pid_file(effective_port).or_else(|| find_eavs_pid_by_port(effective_port));

    let running = is_eavs_server_running(&url).await;

    // Load config to check capture settings
    let config = crate::config::AppConfig::load().ok();
    let capture_enabled = config.as_ref().map(|c| c.capture.enabled).unwrap_or(false);

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
        port: effective_port,
        url: url.clone(),
        uptime_secs: uptime,
        providers: providers.clone(),
        capture_enabled,
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
                println!("  Port:      {}", effective_port);
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
                if capture_enabled {
                    println!("  Capture:   enabled (mitmproxy)");
                }
            } else {
                println!("EAVS Server Status: STOPPED");
                println!("{}", "=".repeat(40));
                println!("  Port:      {}", effective_port);
                if capture_enabled {
                    println!("  Capture:   enabled in config (will start with server)");
                }

                // Check if port is in use by something else
                if !is_port_available(effective_port) {
                    println!("  Note:      Port {} is in use by another application", effective_port);
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::env;
    use std::fs;
    use tempfile::tempdir;

    #[test]
    fn test_get_effective_port_cli_takes_precedence() {
        // CLI port should always win
        let port = get_effective_port(Some(8080), None);
        assert_eq!(port, 8080);
    }

    #[test]
    fn test_get_effective_port_from_config_file() {
        // Create a temp config file with a custom port
        let dir = tempdir().unwrap();
        let config_path = dir.path().join("test-config.toml");
        fs::write(&config_path, r#"
            [server]
            port = 4242
        "#).unwrap();

        let port = get_effective_port(None, Some(config_path.to_str().unwrap()));
        assert_eq!(port, 4242);
    }

    #[test]
    fn test_get_effective_port_default_fallback() {
        // When no CLI port and no config, should fall back to 3000
        // This test uses a non-existent config path
        let port = get_effective_port(None, Some("/nonexistent/path/config.toml"));
        assert_eq!(port, 3000);
    }

    #[test]
    fn test_get_effective_port_cli_overrides_config() {
        // Create a temp config file with a custom port
        let dir = tempdir().unwrap();
        let config_path = dir.path().join("test-config.toml");
        fs::write(&config_path, r#"
            [server]
            port = 4242
        "#).unwrap();

        // CLI port should override config file port
        let port = get_effective_port(Some(9999), Some(config_path.to_str().unwrap()));
        assert_eq!(port, 9999);
    }

    #[test]
    fn test_get_effective_port_env_via_config() {
        // When EAVS_PORT env is set, config library should pick it up
        // Note: The env var is processed by the config crate, not get_effective_port directly
        // This test verifies the CLI arg takes precedence over everything
        let port = get_effective_port(Some(7777), None);
        assert_eq!(port, 7777);
    }

    #[test]
    fn test_parse_expiration_days() {
        let result = parse_expiration("30d");
        assert!(result.is_some());
        // Should be a valid RFC3339 timestamp
        let timestamp = result.unwrap();
        assert!(timestamp.contains("T"));
    }

    #[test]
    fn test_parse_expiration_hours() {
        let result = parse_expiration("24h");
        assert!(result.is_some());
        let timestamp = result.unwrap();
        assert!(timestamp.contains("T"));
    }

    #[test]
    fn test_parse_expiration_minutes() {
        let result = parse_expiration("60m");
        assert!(result.is_some());
        let timestamp = result.unwrap();
        assert!(timestamp.contains("T"));
    }

    #[test]
    fn test_parse_expiration_never() {
        assert!(parse_expiration("never").is_none());
        assert!(parse_expiration("").is_none());
    }

    #[test]
    fn test_parse_expiration_invalid() {
        assert!(parse_expiration("invalid").is_none());
        assert!(parse_expiration("30").is_none());
        assert!(parse_expiration("30x").is_none());
    }

    #[test]
    fn test_is_port_available() {
        // Port 0 should be available (OS assigns random port)
        // We can't reliably test specific ports as they might be in use
        // Just verify the function doesn't panic
        let _ = is_port_available(0);
        let _ = is_port_available(65535);
    }

    #[test]
    fn test_find_available_port() {
        // Should return a port in the range
        let port = find_available_port(50000);
        assert!(port >= 50000);
        assert!(port < 50100);
    }

    #[test]
    fn test_benchmark_results_from_empty_latencies() {
        let results = ConcurrentBenchmarkResults::from_results("test".to_string(), 1, 1.0, vec![], 5);
        assert_eq!(results.successful, 0);
        assert_eq!(results.failed, 5);
        assert_eq!(results.total_requests, 5);
        assert_eq!(results.min_ms, 0.0);
        assert_eq!(results.max_ms, 0.0);
    }

    #[test]
    fn test_benchmark_results_statistics() {
        let latencies = vec![10.0, 20.0, 30.0, 40.0, 50.0];
        let results = ConcurrentBenchmarkResults::from_results("test".to_string(), 1, 1.0, latencies, 0);
        
        assert_eq!(results.successful, 5);
        assert_eq!(results.failed, 0);
        assert_eq!(results.min_ms, 10.0);
        assert_eq!(results.max_ms, 50.0);
        assert_eq!(results.mean_ms, 30.0);
        assert_eq!(results.median_ms, 30.0);
    }

    #[test]
    fn test_parse_duration_string() {
        assert_eq!(parse_duration_string("30s"), Some(Duration::from_secs(30)));
        assert_eq!(parse_duration_string("1m"), Some(Duration::from_secs(60)));
        assert_eq!(parse_duration_string("2m30s"), Some(Duration::from_secs(150)));
        assert_eq!(parse_duration_string("1h"), Some(Duration::from_secs(3600)));
        assert_eq!(parse_duration_string("1h30m"), Some(Duration::from_secs(5400)));
        assert_eq!(parse_duration_string("invalid"), None);
        assert_eq!(parse_duration_string(""), None);
    }

    #[test]
    fn test_concurrent_benchmark_throughput() {
        let latencies = vec![100.0, 100.0, 100.0, 100.0, 100.0]; // 5 requests at 100ms each
        let results = ConcurrentBenchmarkResults::from_results("test".to_string(), 5, 1.0, latencies, 0);
        
        assert_eq!(results.total_requests, 5);
        assert_eq!(results.requests_per_second, 5.0); // 5 requests in 1 second
        assert_eq!(results.concurrency, 5);
    }

    #[test]
    fn test_guess_image_mime() {
        assert_eq!(guess_image_mime("test.png"), Some("image/png"));
        assert_eq!(guess_image_mime("test.PNG"), Some("image/png"));
        assert_eq!(guess_image_mime("test.jpg"), Some("image/jpeg"));
        assert_eq!(guess_image_mime("test.jpeg"), Some("image/jpeg"));
        assert_eq!(guess_image_mime("test.webp"), Some("image/webp"));
        assert_eq!(guess_image_mime("test.gif"), Some("image/gif"));
        assert_eq!(guess_image_mime("test.txt"), None);
        assert_eq!(guess_image_mime("test.pdf"), None);
    }

    #[test]
    fn test_xdg_state_dir() {
        // Save original env
        let original_state = env::var("XDG_STATE_HOME").ok();
        let original_home = env::var("HOME").ok();

        // Test with XDG_STATE_HOME set
        env::set_var("XDG_STATE_HOME", "/custom/state");
        assert_eq!(xdg_state_dir(), PathBuf::from("/custom/state"));

        // Test without XDG_STATE_HOME but with HOME
        env::remove_var("XDG_STATE_HOME");
        env::set_var("HOME", "/home/testuser");
        assert_eq!(xdg_state_dir(), PathBuf::from("/home/testuser/.local/state"));

        // Restore original env
        if let Some(val) = original_state {
            env::set_var("XDG_STATE_HOME", val);
        } else {
            env::remove_var("XDG_STATE_HOME");
        }
        if let Some(val) = original_home {
            env::set_var("HOME", val);
        } else {
            env::remove_var("HOME");
        }
    }
}

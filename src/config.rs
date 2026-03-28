use config::{Config, ConfigError, File};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;

use crate::policy::PolicyConfig;
use crate::provider::{CompatSettings, ProviderType};

fn env_var_nonempty(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|v| !v.trim().is_empty())
}

fn expand_path(input: &str) -> PathBuf {
    let mut out = String::new();
    let mut chars = input.chars().peekable();

    if input.starts_with("~/") {
        if let Some(home) = env_var_nonempty("HOME") {
            out.push_str(&home);
            out.push('/');
            for _ in 0..2 {
                let _ = chars.next();
            }
        }
    }

    #[allow(clippy::while_let_on_iterator)]
    while let Some(ch) = chars.next() {
        if ch != '$' {
            out.push(ch);
            continue;
        }

        match chars.peek().copied() {
            Some('{') => {
                let _ = chars.next();
                let mut var = String::new();
                while let Some(c) = chars.next() {
                    if c == '}' {
                        break;
                    }
                    var.push(c);
                }
                if let Some(val) = env_var_nonempty(&var) {
                    out.push_str(&val);
                } else {
                    out.push_str("${");
                    out.push_str(&var);
                    out.push('}');
                }
            }
            Some(c) if c.is_ascii_alphanumeric() || c == '_' => {
                let mut var = String::new();
                while let Some(c2) = chars.peek().copied() {
                    if c2.is_ascii_alphanumeric() || c2 == '_' {
                        var.push(c2);
                        let _ = chars.next();
                    } else {
                        break;
                    }
                }
                if let Some(val) = env_var_nonempty(&var) {
                    out.push_str(&val);
                } else {
                    out.push('$');
                    out.push_str(&var);
                }
            }
            _ => out.push('$'),
        }
    }

    if out.is_empty() {
        PathBuf::from(input)
    } else {
        PathBuf::from(out)
    }
}

#[derive(Debug, Deserialize, Clone)]
pub struct AppConfig {
    #[serde(default)]
    pub server: ServerConfig,
    #[serde(default)]
    pub providers: HashMap<String, ProviderConfig>,
    /// Legacy support: map upstream -> providers
    #[serde(default)]
    pub upstream: HashMap<String, ProviderConfig>,
    #[serde(default)]
    pub logging: LoggingConfig,
    #[serde(default)]
    pub analysis: AnalysisConfig,
    #[serde(default)]
    pub policy: PolicyConfig,
    #[serde(default)]
    pub state: StateConfig,
    #[serde(default)]
    pub keys: KeysConfig,
    #[serde(default)]
    pub capture: CaptureConfig,
    /// Request/response transform plugins
    #[serde(default)]
    pub transform: TransformConfig,
    /// Network access control (domain allow/deny lists)
    #[serde(default)]
    pub network: NetworkConfig,
    /// Mock provider predefined responses
    #[serde(default)]
    pub mock_responses: MockResponsesConfig,
}

impl AppConfig {
    /// Create a config with all default values
    pub fn with_defaults() -> Self {
        Self {
            server: ServerConfig::default(),
            providers: HashMap::new(),
            upstream: HashMap::new(),
            logging: LoggingConfig::default(),
            analysis: AnalysisConfig::default(),
            policy: PolicyConfig::default(),
            state: StateConfig::default(),
            keys: KeysConfig::default(),
            capture: CaptureConfig::default(),
            transform: TransformConfig::default(),
            network: NetworkConfig::default(),
            mock_responses: MockResponsesConfig::default(),
        }
    }
}

/// Network access control configuration.
///
/// Controls which upstream domains/IPs the proxy is allowed to connect to.
/// Precedence: deny list is checked first, then allow list.
/// - If deny list is non-empty and the domain matches: BLOCKED
/// - If allow list is non-empty and the domain does NOT match: BLOCKED
/// - Otherwise: ALLOWED
///
/// Empty lists = no restriction for that list.
#[derive(Debug, Deserialize, Clone, Default)]
pub struct NetworkConfig {
    /// Allowed upstream domains (glob patterns). Empty = allow all.
    /// Examples: ["api.openai.com", "*.anthropic.com", "api.groq.com"]
    #[serde(default)]
    pub allow_domains: Vec<String>,

    /// Denied upstream domains (glob patterns). Checked before allow list.
    /// Examples: ["*.internal.corp", "localhost", "127.*"]
    #[serde(default)]
    pub deny_domains: Vec<String>,

    /// Block requests to private/internal IP ranges (10.x, 172.16-31.x, 192.168.x, 127.x).
    /// Default: true (prevents SSRF attacks).
    #[serde(default = "default_true")]
    pub block_private_ips: bool,
}

fn default_true() -> bool {
    true
}

#[derive(Debug, Deserialize, Clone)]
#[serde(default)]
pub struct ServerConfig {
    pub host: String,
    pub port: u16,
    /// Maximum request body size in bytes. Default: 10 MB.
    /// Set to 0 for unlimited (not recommended).
    pub max_body_size: usize,
    /// Redact sensitive data (API keys, tokens) from logs. Default: true.
    pub log_redact: bool,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            host: "127.0.0.1".to_string(),
            port: 3033,
            max_body_size: 10 * 1024 * 1024, // 10 MB default
            log_redact: true,
        }
    }
}

/// Redact sensitive information from URLs and strings.
///
/// Redacts:
/// - API keys in query parameters (api_key=xxx, key=xxx)
/// - Bearer tokens (keeps prefix for debugging)
/// - Common API key patterns (sk-, eavs-, etc.)
pub fn redact_sensitive(input: &str) -> String {
    let mut result = input.to_string();

    // Redact query parameters with sensitive names
    for param_name in &[
        "api_key", "key", "token", "secret", "password", "api-key", "apikey",
    ] {
        result = redact_query_param(&result, param_name);
    }

    // Redact Bearer tokens (keep first 8 chars for debugging)
    result = redact_bearer_token(&result);

    // Redact common API key patterns
    result = redact_api_key_patterns(&result);

    result
}

/// Redact a specific query parameter value.
fn redact_query_param(input: &str, param_name: &str) -> String {
    let mut result = String::with_capacity(input.len());
    let mut remaining = input;

    while let Some(idx) = remaining.find(&format!("{}=", param_name)) {
        // Check it's actually a parameter (preceded by ? or &)
        let prefix_ok = idx == 0 || {
            let prev_char = remaining.chars().nth(idx.saturating_sub(1));
            prev_char == Some('?') || prev_char == Some('&')
        };

        if !prefix_ok {
            result.push_str(&remaining[..idx + param_name.len() + 1]);
            remaining = &remaining[idx + param_name.len() + 1..];
            continue;
        }

        // Copy up to and including the =
        result.push_str(&remaining[..idx + param_name.len() + 1]);
        result.push_str("[REDACTED]");
        remaining = &remaining[idx + param_name.len() + 1..];

        // Skip the actual value
        if let Some(end_idx) = remaining.find(['&', ' ', '\n']) {
            remaining = &remaining[end_idx..];
        } else {
            remaining = "";
        }
    }

    result.push_str(remaining);
    result
}

/// Redact Bearer token values, keeping prefix for debugging.
fn redact_bearer_token(input: &str) -> String {
    let mut result = String::with_capacity(input.len());
    let mut remaining = input;

    while let Some(idx) = remaining.to_lowercase().find("bearer ") {
        // Copy up to and including "Bearer "
        result.push_str(&remaining[..idx + 7]);
        remaining = &remaining[idx + 7..];

        // Keep first 8 chars of the token for debugging, redact rest
        let token_end = remaining
            .find(|c: char| c.is_whitespace() || c == '"' || c == '\'')
            .unwrap_or(remaining.len());
        let token = &remaining[..token_end];

        if token.len() > 8 {
            result.push_str(&token[..8]);
            result.push_str("[REDACTED]");
        } else {
            result.push_str(token);
        }

        remaining = &remaining[token_end..];
    }

    result.push_str(remaining);
    result
}

/// Redact common API key patterns like sk-xxx, eavs-xxx.
fn redact_api_key_patterns(input: &str) -> String {
    let prefixes = ["sk-", "eavs-", "api-", "key-", "pk-", "rk-"];
    let mut result = input.to_string();

    for prefix in prefixes {
        result = redact_prefixed_key(&result, prefix);
    }

    result
}

/// Redact keys with a specific prefix.
fn redact_prefixed_key(input: &str, prefix: &str) -> String {
    let mut result = String::with_capacity(input.len());
    let mut remaining = input;

    while let Some(idx) = remaining.find(prefix) {
        // Copy up to and including the prefix
        result.push_str(&remaining[..idx + prefix.len()]);
        remaining = &remaining[idx + prefix.len()..];

        // Find the end of the key (alphanumeric chars)
        let key_len = remaining
            .chars()
            .take_while(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
            .count();

        if key_len > 8 {
            // Keep first 8 chars, redact rest
            let key = &remaining[..key_len];
            result.push_str(&key[..8.min(key.len())]);
            result.push_str("[REDACTED]");
            remaining = &remaining[key_len..];
        } else {
            // Short key, keep as-is
            result.push_str(&remaining[..key_len]);
            remaining = &remaining[key_len..];
        }
    }

    result.push_str(remaining);
    result
}

#[derive(Debug, Deserialize, Clone)]
pub struct ProviderConfig {
    /// Provider type: openai, anthropic, google, azure, mistral, groq, cerebras, xai, openrouter, ollama, etc.
    #[serde(rename = "type", default)]
    pub type_: String,
    /// API key - supports "env:VAR_NAME" syntax or direct value
    #[serde(default)]
    pub api_key: String,
    /// Base URL - defaults based on provider type if not specified
    #[serde(default)]
    pub base_url: String,
    /// API version (primarily for Azure)
    pub api_version: Option<String>,
    /// Compatibility settings for OpenAI-compatible APIs
    #[serde(default)]
    pub compat: CompatSettings,
    /// Custom headers to add to requests
    #[serde(default)]
    pub headers: HashMap<String, String>,
    /// AWS region (Bedrock only). Supports `env:VAR_NAME` syntax.
    #[serde(default)]
    pub aws_region: String,
    /// AWS access key id (Bedrock only). Supports `env:VAR_NAME` syntax.
    #[serde(default)]
    pub aws_access_key_id: String,
    /// AWS secret access key (Bedrock only). Supports `env:VAR_NAME` syntax.
    #[serde(default)]
    pub aws_secret_access_key: String,
    /// AWS session token (Bedrock only, optional). Supports `env:VAR_NAME` syntax.
    #[serde(default)]
    pub aws_session_token: String,
    /// AWS service name for SigV4 (Bedrock only). Defaults to `bedrock`.
    #[serde(default)]
    pub aws_service: String,
    /// Azure deployment name (Azure only). If not set, uses model name as deployment.
    /// Supports `env:VAR_NAME` syntax.
    #[serde(default)]
    pub deployment: String,
    /// Google Cloud project ID (Vertex AI only). Supports `env:VAR_NAME` syntax.
    #[serde(default)]
    pub gcp_project: String,
    /// Google Cloud location/region (Vertex AI only). Supports `env:VAR_NAME` syntax.
    #[serde(default)]
    pub gcp_location: String,
    /// Model name to use for `eavs setup test` and `eavs setup test-all`.
    /// Overrides the built-in default for this provider type.
    #[serde(default)]
    pub test_model: String,
    /// Curated model shortlist for this provider.
    /// Used by integrations (e.g., octo) to generate models.json for Pi.
    /// If empty, the full model catalog from models.dev is used.
    /// If non-empty, ONLY these models are exposed (locks down the choice).
    #[serde(default)]
    pub models: Vec<ModelShortlistEntry>,
}

/// A model entry in a provider's shortlist.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ModelShortlistEntry {
    /// Model ID as sent to the API (e.g., "gpt-5.2-codex", "claude-opus-4-6")
    pub id: String,
    /// Human-readable display name
    #[serde(default)]
    pub name: String,
    /// Whether the model supports extended thinking/reasoning
    #[serde(default)]
    pub reasoning: bool,
    /// Input modalities: ["text"] or ["text", "image"]
    #[serde(default = "default_input_modalities")]
    pub input: Vec<String>,
    /// Context window size in tokens
    #[serde(default = "default_context_window")]
    pub context_window: u64,
    /// Maximum output tokens
    #[serde(default = "default_max_tokens")]
    pub max_tokens: u64,
    /// Cost per million tokens: input, output, cache_read
    #[serde(default)]
    pub cost: ModelCost,
    /// Compatibility flags for Pi (e.g., supportsDeveloperRole).
    /// Passed through to models.json as-is.
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub compat: HashMap<String, serde_json::Value>,
}

fn default_input_modalities() -> Vec<String> {
    vec!["text".to_string()]
}

fn default_context_window() -> u64 {
    128_000
}

fn default_max_tokens() -> u64 {
    16_384
}

/// Cost per million tokens.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct ModelCost {
    #[serde(default)]
    pub input: f64,
    #[serde(default)]
    pub output: f64,
    #[serde(default)]
    pub cache_read: f64,
}

impl Default for ProviderConfig {
    fn default() -> Self {
        Self {
            type_: "openai".to_string(),
            api_key: String::new(),
            base_url: String::new(),
            api_version: None,
            compat: CompatSettings::default(),
            headers: HashMap::new(),
            aws_region: String::new(),
            aws_access_key_id: String::new(),
            aws_secret_access_key: String::new(),
            aws_session_token: String::new(),
            aws_service: String::new(),
            deployment: String::new(),
            gcp_project: String::new(),
            gcp_location: String::new(),
            test_model: String::new(),
            models: Vec::new(),
        }
    }
}

impl ProviderConfig {
    /// Get the resolved base URL (using provider defaults if not specified).
    pub fn resolved_base_url(&self) -> String {
        if let Some(base_url) = get_api_key(&self.base_url) {
            if !base_url.is_empty() {
                return base_url;
            }
        }

        if self.base_url.is_empty() {
            let provider = ProviderType::from_str(&self.type_);
            if provider == ProviderType::Bedrock {
                if let Some(region) = self.resolved_aws_region() {
                    return format!("https://bedrock-runtime.{}.amazonaws.com", region);
                }
            }
            if provider == ProviderType::GoogleVertex {
                if let Some(location) = self.resolved_gcp_location() {
                    return format!("https://{}-aiplatform.googleapis.com", location);
                }
            }
            provider
                .info()
                .default_base_url
                .unwrap_or("http://localhost:8000/v1")
                .to_string()
        } else {
            self.base_url.clone()
        }
    }

    /// Get the resolved API key (from env var if specified).
    pub fn resolved_api_key(&self) -> String {
        get_api_key(&self.api_key).unwrap_or_else(|| {
            // Try provider-specific env var as fallback
            let provider = ProviderType::from_str(&self.type_);
            if let Some(env_name) = provider.info().env_key_name {
                std::env::var(env_name).unwrap_or_default()
            } else {
                String::new()
            }
        })
    }

    /// Get the resolved API version (from env var if specified).
    pub fn resolved_api_version(&self) -> Option<String> {
        self.api_version.as_ref().and_then(|v| get_api_key(v))
    }

    /// Get the resolved Azure deployment name (from env var if specified).
    /// Returns None if not set, allowing fallback to model name.
    pub fn resolved_deployment(&self) -> Option<String> {
        if self.deployment.is_empty() {
            None
        } else {
            get_api_key(&self.deployment)
        }
    }

    pub fn resolved_aws_region(&self) -> Option<String> {
        get_api_key(&self.aws_region).or_else(|| std::env::var("AWS_REGION").ok())
    }

    pub fn resolved_aws_access_key_id(&self) -> Option<String> {
        get_api_key(&self.aws_access_key_id).or_else(|| std::env::var("AWS_ACCESS_KEY_ID").ok())
    }

    pub fn resolved_aws_secret_access_key(&self) -> Option<String> {
        get_api_key(&self.aws_secret_access_key)
            .or_else(|| std::env::var("AWS_SECRET_ACCESS_KEY").ok())
    }

    pub fn resolved_aws_session_token(&self) -> Option<String> {
        get_api_key(&self.aws_session_token).or_else(|| std::env::var("AWS_SESSION_TOKEN").ok())
    }

    pub fn resolved_aws_service(&self) -> String {
        if let Some(s) = get_api_key(&self.aws_service) {
            if !s.is_empty() {
                return s;
            }
        }
        "bedrock".to_string()
    }

    /// Get the resolved GCP project (Vertex AI).
    pub fn resolved_gcp_project(&self) -> Option<String> {
        get_api_key(&self.gcp_project)
            .or_else(|| std::env::var("GOOGLE_CLOUD_PROJECT").ok())
            .or_else(|| std::env::var("GCLOUD_PROJECT").ok())
    }

    /// Get the resolved GCP location (Vertex AI).
    pub fn resolved_gcp_location(&self) -> Option<String> {
        get_api_key(&self.gcp_location).or_else(|| std::env::var("GOOGLE_CLOUD_LOCATION").ok())
    }

    /// Get the provider type enum.
    pub fn provider_type(&self) -> ProviderType {
        ProviderType::from_str(&self.type_)
    }

    /// Get compat settings merged with URL-detected defaults.
    pub fn resolved_compat(&self) -> CompatSettings {
        self.compat
            .clone()
            .with_detected_defaults(&self.resolved_base_url())
    }
}

/// Logging configuration with multiple backend support.
#[derive(Debug, Deserialize, Clone)]
#[serde(default)]
pub struct LoggingConfig {
    /// Default logging backend: "stdout", "file", "none"
    pub default: String,
    /// Additional logging backends
    #[serde(default)]
    pub backends: Vec<LogBackend>,
    /// Legacy support for "sink" field
    #[serde(default)]
    pub sink: String,
}

impl Default for LoggingConfig {
    fn default() -> Self {
        Self {
            default: "stdout".to_string(),
            backends: Vec::new(),
            sink: String::new(),
        }
    }
}

impl LoggingConfig {
    /// Get effective default backend (handles legacy "sink" field).
    pub fn effective_default(&self) -> &str {
        if !self.sink.is_empty() {
            &self.sink
        } else {
            &self.default
        }
    }
}

/// Individual logging backend configuration.
#[derive(Debug, Deserialize, Clone)]
#[serde(tag = "type")]
pub enum LogBackend {
    /// Standard output logging
    #[serde(rename = "stdout")]
    Stdout {
        /// Log format: "json" or "pretty"
        #[serde(default = "default_format")]
        format: String,
    },
    /// File-based logging
    #[serde(rename = "file")]
    File {
        /// Path to log file
        path: String,
        /// Rotation strategy: "daily", "size", "none"
        #[serde(default)]
        rotate: String,
        /// Max file size in bytes (for size-based rotation)
        #[serde(default)]
        #[allow(dead_code)]
        max_size: Option<u64>,
    },
    /// Webhook/HTTP endpoint
    #[serde(rename = "webhook")]
    Webhook {
        /// URL to POST logs to
        url: String,
        /// Custom headers (supports env: syntax for secrets)
        #[serde(default)]
        headers: HashMap<String, String>,
        /// Batch size before sending
        #[serde(default = "default_batch_size")]
        batch_size: usize,
        /// Flush interval in seconds
        #[serde(default = "default_flush_interval")]
        flush_interval_secs: u64,
    },
    /// OpenTelemetry export
    #[serde(rename = "otel", alias = "opentelemetry")]
    OpenTelemetry {
        /// OTLP endpoint
        endpoint: String,
        /// Protocol: "grpc" or "http"
        #[serde(default = "default_otel_protocol")]
        protocol: String,
        /// Service name for traces
        #[serde(default = "default_service_name")]
        service_name: String,
    },
}

fn default_format() -> String {
    "json".to_string()
}

fn default_batch_size() -> usize {
    100
}

fn default_flush_interval() -> u64 {
    5
}

fn default_otel_protocol() -> String {
    "grpc".to_string()
}

fn default_service_name() -> String {
    "eavs".to_string()
}

#[derive(Debug, Deserialize, Clone)]
#[serde(default)]
pub struct AnalysisConfig {
    pub enabled: bool,
    pub broadcast_channel_size: usize,
    pub plugins: Vec<AnalysisPluginConfig>,
}

impl Default for AnalysisConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            broadcast_channel_size: 1024,
            plugins: Vec::new(),
        }
    }
}

/// Analysis plugin configuration.
#[derive(Debug, Deserialize, Clone, Default)]
#[serde(default)]
pub struct AnalysisPluginConfig {
    /// Unique name for the plugin (for logs/observability).
    pub name: String,
    /// Executable to run (resolved via PATH unless absolute).
    pub command: String,
    /// Arguments to pass to the plugin.
    pub args: Vec<String>,
    /// Environment variables for the plugin (supports `env:VAR` values).
    pub env: HashMap<String, String>,
}

/// Configuration for request/response transform plugins.
///
/// Transform plugins allow customizing requests and responses before/after
/// they are sent to upstream providers. This is useful for:
/// - Provider-specific quirks (e.g., OAuth token restrictions)
/// - Custom header injection
/// - Request/response logging and modification
///
/// Plugins are external scripts that receive JSON on stdin and output
/// modified JSON on stdout.
#[derive(Debug, Deserialize, Clone, Default)]
#[serde(default)]
pub struct TransformConfig {
    /// Enable transform plugins
    pub enabled: bool,
    /// Transform plugins for requests/responses
    pub plugins: Vec<TransformPluginConfig>,
}

/// Transform plugin configuration.
#[derive(Debug, Deserialize, Clone, Default)]
#[serde(default)]
pub struct TransformPluginConfig {
    /// Unique name for the plugin (for logs/observability).
    pub name: String,
    /// Executable to run (resolved via PATH unless absolute).
    pub command: String,
    /// Arguments to pass to the plugin.
    pub args: Vec<String>,
    /// Environment variables for the plugin (supports `env:VAR` values).
    pub env: HashMap<String, String>,
    /// Provider filter - only run for specific providers (e.g., ["anthropic", "openai"]).
    /// Empty means run for all providers.
    #[serde(default)]
    pub providers: Vec<String>,
    /// Whether to run for OAuth requests only
    #[serde(default)]
    pub oauth_only: bool,
    /// Timeout in milliseconds for plugin execution (default: 5000)
    #[serde(default = "default_transform_timeout")]
    pub timeout_ms: u64,
}

fn default_transform_timeout() -> u64 {
    5000
}

/// Configuration for conversation state storage.
#[derive(Debug, Deserialize, Clone)]
#[serde(default)]
pub struct StateConfig {
    /// Enable conversation state storage
    pub enabled: bool,
    /// Track all conversations (not just those with injections)
    pub capture_all: bool,
    /// TTL for conversation state in seconds (0 = no expiration)
    pub ttl_secs: u64,
    /// How often to run cleanup in seconds
    pub cleanup_interval_secs: u64,
    /// Maximum number of conversations to store (0 = unlimited)
    pub max_conversations: usize,
}

impl Default for StateConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            capture_all: false, // Only track conversations with injections by default
            ttl_secs: 3600,     // 1 hour default
            cleanup_interval_secs: 60, // Cleanup every minute
            max_conversations: 10000, // Max 10k conversations
        }
    }
}

/// Configuration for virtual API keys.
#[derive(Debug, Deserialize, Clone)]
#[serde(default)]
pub struct KeysConfig {
    /// Enable virtual API key support
    pub enabled: bool,
    /// Require a valid virtual API key for all requests.
    /// When true, requests without a valid eavs_ key are rejected.
    /// When false (default), requests without keys pass through (backward compatible).
    pub require_key: bool,
    /// Path to the SQLite database file
    pub database_path: String,
    /// Master key for admin API (if not set, admin API is disabled)
    pub master_key: Option<String>,
    /// Allow self-provisioning of keys (without master key)
    pub allow_self_provisioning: bool,
    /// Default rate limit for new keys (requests per minute, 0 = unlimited)
    pub default_rpm_limit: Option<u32>,
    /// Default budget for new keys (USD, None = unlimited)
    pub default_budget_usd: Option<f64>,
    /// Update pricing from LiteLLM on startup
    pub update_pricing_on_startup: bool,
    /// Path to word lists TOML file for human-readable key IDs.
    /// If not specified, downloads from eavs GitHub repo to XDG data directory.
    pub word_lists_path: Option<String>,
    /// OAuth credential storage backend: "keychain" (default) or "sqlite".
    /// "keychain" uses the system keychain (macOS Keychain, libsecret, Windows
    /// Credential Manager) and falls back to "sqlite" if unavailable.
    /// "sqlite" stores tokens in the same database as virtual API keys.
    #[serde(default = "default_oauth_backend")]
    pub oauth_backend: String,
}

fn default_oauth_backend() -> String {
    if cfg!(target_os = "macos") || cfg!(target_os = "windows") {
        "keychain".to_string()
    } else {
        "sqlite".to_string()
    }
}

impl Default for KeysConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            require_key: false,
            database_path: "~/.eavs/keys.db".to_string(),
            master_key: None,
            allow_self_provisioning: false,
            default_rpm_limit: None,
            default_budget_usd: None,
            update_pricing_on_startup: false,
            word_lists_path: None,
            oauth_backend: default_oauth_backend(),
        }
    }
}

/// Configuration for mock provider predefined responses.
///
/// Allows defining named responses that can be triggered by sending
/// a user message with `##<name>` to the mock provider.
///
/// Example config:
///   [mock_responses.success]
///   content = "All tests passed"
///
///   [mock_responses.error]
///   type = "error"
///   code = "rate_limit_exceeded"
///   message = "Too many requests"
///   status = 429
///
///   [mock_responses.tool1]
///   type = "tool_call"
///   name = "get_weather"
///   arguments = '{"location": "NYC"}'
#[derive(Debug, Deserialize, Clone, Default)]
#[serde(default)]
pub struct MockResponsesConfig {
    /// Map of response name -> response definition
    #[serde(flatten)]
    pub responses: HashMap<String, MockResponse>,
}

/// A single predefined mock response.
#[derive(Debug, Deserialize, Clone, Default)]
#[serde(default)]
pub struct MockResponse {
    /// Response content (for normal text responses)
    pub content: String,
    /// Response type: "text", "error", "tool_call"
    #[serde(rename = "type")]
    pub response_type: String,
    /// Error code (for error responses)
    pub code: String,
    /// Error message (for error responses)
    pub message: String,
    /// HTTP status code (for error responses)
    pub status: u16,
    /// Tool function name (for tool_call responses)
    pub name: String,
    /// Tool arguments JSON string (for tool_call responses)
    pub arguments: String,
}

/// Configuration for transparent traffic capture via mitmproxy.
///
/// When enabled, Eaves will automatically start mitmproxy with the
/// eavs_capture.py addon to intercept LLM API traffic.
#[derive(Debug, Deserialize, Clone)]
#[serde(default)]
pub struct CaptureConfig {
    /// Enable automatic mitmproxy capture mode
    pub enabled: bool,
    /// Path to mitmdump executable (default: "mitmdump" - uses PATH)
    /// Note: Use mitmdump (not mitmproxy) for non-interactive/background usage
    pub mitmproxy_path: String,
    /// Capture mode: "local" for all traffic, "local:AppName" for specific app
    pub mode: String,
    /// Enable verbose logging from mitmproxy addon
    pub verbose: bool,
    /// Only capture API traffic (skip desktop app domains like chat.openai.com)
    pub api_only: bool,
    /// Path to the eavs_capture.py addon script (default: bundled with eavs)
    pub addon_path: Option<String>,
    /// Additional mitmproxy arguments
    pub extra_args: Vec<String>,
}

impl Default for CaptureConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            mitmproxy_path: "mitmdump".to_string(),
            mode: "local".to_string(),
            verbose: false,
            api_only: false,
            addon_path: None,
            extra_args: Vec::new(),
        }
    }
}

impl CaptureConfig {
    /// Get the resolved addon script path.
    ///
    /// If explicitly configured, returns that path.
    /// Otherwise, looks for the addon in standard locations:
    /// 1. Next to the eavs binary (for installed versions)
    /// 2. In the scripts/ directory (for development)
    pub fn resolved_addon_path(&self) -> Option<PathBuf> {
        if let Some(path) = &self.addon_path {
            return Some(expand_path(path));
        }

        // Try to find addon relative to current executable
        if let Ok(exe_path) = std::env::current_exe() {
            if let Some(exe_dir) = exe_path.parent() {
                // Check next to binary
                let addon_next_to_exe = exe_dir.join("eavs_capture.py");
                if addon_next_to_exe.exists() {
                    return Some(addon_next_to_exe);
                }

                // Check in scripts/ subdirectory
                let addon_in_scripts = exe_dir.join("scripts").join("eavs_capture.py");
                if addon_in_scripts.exists() {
                    return Some(addon_in_scripts);
                }

                // Check in share/eavs/ (for system installs)
                let addon_in_share = exe_dir.join("../share/eavs/eavs_capture.py");
                if addon_in_share.exists() {
                    return addon_in_share.canonicalize().ok();
                }
            }
        }

        // Check current working directory
        let cwd_addon = PathBuf::from("scripts/eavs_capture.py");
        if cwd_addon.exists() {
            return Some(cwd_addon);
        }

        None
    }

    /// Build mitmproxy command arguments.
    pub fn build_mitmproxy_args(&self, eavs_port: u16) -> Vec<String> {
        let mut args = vec!["--mode".to_string(), self.mode.clone()];

        // Add addon script
        if let Some(addon_path) = self.resolved_addon_path() {
            args.push("-s".to_string());
            args.push(addon_path.to_string_lossy().to_string());
        }

        // Set Eaves port for the addon
        args.push("--set".to_string());
        args.push(format!("eavs_port={}", eavs_port));

        // Verbose mode
        if self.verbose {
            args.push("--set".to_string());
            args.push("eavs_verbose=true".to_string());
        }

        // API-only mode
        if self.api_only {
            args.push("--set".to_string());
            args.push("eavs_api_only=true".to_string());
        }

        // Add any extra arguments
        args.extend(self.extra_args.clone());

        args
    }
}

impl KeysConfig {
    /// Get the resolved database path (expanding ~).
    pub fn resolved_database_path(&self) -> PathBuf {
        let path = &self.database_path;
        if let Some(stripped) = path.strip_prefix("~/") {
            if let Ok(home) = std::env::var("HOME") {
                return PathBuf::from(home).join(stripped);
            }
        }
        PathBuf::from(path)
    }

    /// Get the master key (from env if specified).
    pub fn resolved_master_key(&self) -> Option<String> {
        self.master_key.as_ref().and_then(|k| get_api_key(k))
    }

    /// Get the resolved word lists path.
    ///
    /// If explicitly configured, expands ~ and returns that path.
    /// Otherwise, returns the XDG data directory default:
    /// `$XDG_DATA_HOME/eavs/word_lists.toml` or `~/.local/share/eavs/word_lists.toml`
    #[allow(dead_code)]
    pub fn resolved_word_lists_path(&self) -> PathBuf {
        if let Some(path) = &self.word_lists_path {
            // Expand ~ in configured path
            if let Some(stripped) = path.strip_prefix("~/") {
                if let Ok(home) = std::env::var("HOME") {
                    return PathBuf::from(home).join(stripped);
                }
            }
            return PathBuf::from(path);
        }

        // Default to XDG data directory
        Self::get_xdg_data_path()
    }

    /// Get the XDG data directory path for word lists.
    #[allow(dead_code)]
    fn get_xdg_data_path() -> PathBuf {
        let data_home = env_var_nonempty("XDG_DATA_HOME")
            .map(PathBuf::from)
            .or_else(|| {
                env_var_nonempty("HOME").map(|home| PathBuf::from(home).join(".local/share"))
            })
            .unwrap_or_else(|| PathBuf::from("."));

        data_home.join("eavs").join("word_lists.toml")
    }
}

impl AppConfig {
    pub fn load() -> Result<Self, ConfigError> {
        Self::load_with_override_path(None)
    }

    pub fn load_or_init() -> Result<Self, ConfigError> {
        if let Some(path) = Self::get_xdg_config_path() {
            if !path.exists() {
                Self::init_default_global_config(&path)?;
            }
        }
        Self::load()
    }

    /// Load configuration from a specific file path.
    pub fn load_from(path: &str) -> Result<Self, ConfigError> {
        Self::load_with_override_path(Some(path))
    }

    fn finalize_config(
        builder: config::ConfigBuilder<config::builder::DefaultState>,
    ) -> Result<Self, ConfigError> {
        let mut config: AppConfig = builder.build()?.try_deserialize()?;

        // Merge legacy "upstream" into "providers" for backward compatibility
        if config.providers.is_empty() && !config.upstream.is_empty() {
            config.providers = config.upstream.clone();
        }

        // Ensure we have at least a default provider
        if config.providers.is_empty() {
            config.providers.insert(
                "default".to_string(),
                ProviderConfig {
                    type_: "openai".to_string(),
                    api_key: "env:OPENAI_API_KEY".to_string(),
                    ..Default::default()
                },
            );
        }

        Ok(config)
    }

    fn load_with_override_path(override_path: Option<&str>) -> Result<Self, ConfigError> {
        let mut builder = Config::builder();

        // Global config (XDG)
        if let Some(path) = Self::get_xdg_config_path() {
            if path.exists() {
                tracing::info!("Loading config from XDG path: {:?}", path);
                builder = builder.add_source(File::from(path).required(false));
            }
        }

        // Local config (current directory)
        if std::path::Path::new("eavs.toml").exists() {
            tracing::info!("Loading config from local path: eavs.toml");
            builder = builder.add_source(File::from(std::path::Path::new("eavs.toml")));
        } else if std::path::Path::new("eavs.yaml").exists() {
            tracing::info!("Loading config from local path: eavs.yaml");
            builder = builder.add_source(File::from(std::path::Path::new("eavs.yaml")));
        } else if std::path::Path::new("eavs.yml").exists() {
            tracing::info!("Loading config from local path: eavs.yml");
            builder = builder.add_source(File::from(std::path::Path::new("eavs.yml")));
        }

        // Environment overrides (EAVS_SERVER__PORT=..., EAVS_PROVIDERS__AZURE__BASE_URL=..., etc.)
        builder = builder.add_source(
            config::Environment::with_prefix("EAVS")
                .separator("__")
                .try_parsing(true),
        );

        // Explicit config path (highest priority among file/env sources)
        if let Some(path) = override_path {
            let expanded = expand_path(path);
            tracing::info!("Loading config from explicit path: {:?}", expanded);
            builder = builder.add_source(File::from(expanded));
        }

        Self::finalize_config(builder)
    }

    fn get_xdg_config_path() -> Option<PathBuf> {
        let config_home = env_var_nonempty("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .or_else(|| env_var_nonempty("HOME").map(|home| PathBuf::from(home).join(".config")))?;

        Some(config_home.join("eavs").join("config.toml"))
    }

    fn init_default_global_config(config_path: &PathBuf) -> Result<(), ConfigError> {
        let Some(dir) = config_path.parent() else {
            return Ok(());
        };

        std::fs::create_dir_all(dir).map_err(|e| {
            ConfigError::Message(format!(
                "Failed to create config dir {}: {}",
                dir.display(),
                e
            ))
        })?;

        let schema_path = dir.join("config.schema.json");
        if !schema_path.exists() {
            std::fs::write(&schema_path, include_bytes!("../config/config.schema.json")).map_err(
                |e| {
                    ConfigError::Message(format!(
                        "Failed to write schema file {}: {}",
                        schema_path.display(),
                        e
                    ))
                },
            )?;
        }

        if !config_path.exists() {
            std::fs::write(config_path, include_str!("../config/config.example.toml")).map_err(
                |e| {
                    ConfigError::Message(format!(
                        "Failed to write default config file {}: {}",
                        config_path.display(),
                        e
                    ))
                },
            )?;
        }

        Ok(())
    }

    /// Get a provider config by name (case-insensitive).
    /// Returns None if the provider is not found.
    /// Use "default" explicitly if you want the default provider.
    #[allow(dead_code)]
    pub fn get_provider(&self, name: &str) -> Option<&ProviderConfig> {
        let name_lower = name.to_lowercase();
        self.providers
            .iter()
            .find(|(k, _)| k.to_lowercase() == name_lower)
            .map(|(_, v)| v)
    }

    /// Get a provider config by name (case-insensitive).
    /// If the provider is not found and the name is empty, falls back to "default".
    /// Returns the config along with metadata about how it was resolved.
    pub fn resolve_provider(&self, name: &str) -> Option<ProviderLookupResult<'_>> {
        let name_lower = name.to_lowercase();

        // First, try exact match (case-insensitive)
        if let Some((resolved_name, config)) = self
            .providers
            .iter()
            .find(|(k, _)| k.to_lowercase() == name_lower)
        {
            return Some(ProviderLookupResult {
                config,
                resolved_name: resolved_name.clone(),
                was_fallback: false,
            });
        }

        // If name is empty, fall back to default
        if name.is_empty() {
            if let Some(config) = self.providers.get("default") {
                return Some(ProviderLookupResult {
                    config,
                    resolved_name: "default".to_string(),
                    was_fallback: true,
                });
            }
        }

        None
    }

    /// Get available provider names.
    pub fn provider_names(&self) -> Vec<&String> {
        self.providers.keys().collect()
    }
}

/// Result of provider lookup with information about what was resolved.
#[derive(Debug, Clone)]
pub struct ProviderLookupResult<'a> {
    pub config: &'a ProviderConfig,
    pub resolved_name: String,
    #[allow(dead_code)]
    pub was_fallback: bool,
}

/// Keychain service name used for provider API keys stored via `keychain:` syntax.
pub const KEYCHAIN_SERVICE: &str = "eavs";

/// Resolve API key from config value.
///
/// Supports three syntaxes:
/// - `"env:VAR_NAME"` -- read from environment variable
/// - `"keychain:account"` -- read from system keychain (service: "eavs", account: the value after the prefix)
/// - anything else -- used as a literal value
///
/// Keychain uses the OS-native credential store:
/// - macOS: Keychain
/// - Linux: D-Bus Secret Service (gnome-keyring, kwallet, KeePassXC)
/// - Windows: Credential Manager
pub fn get_api_key(config_key: &str) -> Option<String> {
    if config_key.is_empty() {
        return None;
    }

    if let Some(var_name) = config_key.strip_prefix("env:") {
        std::env::var(var_name).ok()
    } else if let Some(account) = config_key.strip_prefix("keychain:") {
        get_keychain_secret(account)
    } else {
        Some(config_key.to_string())
    }
}

/// Read a secret from the system keychain.
///
/// Returns `None` if the entry doesn't exist or the keychain is inaccessible,
/// logging a warning on errors other than "not found".
pub fn get_keychain_secret(account: &str) -> Option<String> {
    match keyring::Entry::new(KEYCHAIN_SERVICE, account) {
        Ok(entry) => match entry.get_password() {
            Ok(secret) => Some(secret),
            Err(keyring::Error::NoEntry) => {
                tracing::warn!(
                    "Keychain entry not found: service={}, account={}. Use 'eavs secret set {}' to store it.",
                    KEYCHAIN_SERVICE,
                    account,
                    account,
                );
                None
            }
            Err(e) => {
                tracing::warn!(
                    "Failed to read keychain entry (service={}, account={}): {}",
                    KEYCHAIN_SERVICE,
                    account,
                    e,
                );
                None
            }
        },
        Err(e) => {
            tracing::warn!("Failed to access keychain: {}", e);
            None
        }
    }
}

/// Store a secret in the system keychain.
///
/// Creates or updates the entry under service "eavs" with the given account name.
pub fn set_keychain_secret(account: &str, secret: &str) -> Result<(), String> {
    let entry = keyring::Entry::new(KEYCHAIN_SERVICE, account)
        .map_err(|e| format!("{}\n\n{}", e, keychain_help_text()))?;
    entry
        .set_password(secret)
        .map_err(|e| format!("{}\n\n{}", e, keychain_help_text()))
}

/// Delete a secret from the system keychain.
///
/// Returns `true` if the entry was deleted, `false` if it didn't exist.
pub fn delete_keychain_secret(account: &str) -> Result<bool, String> {
    let entry = keyring::Entry::new(KEYCHAIN_SERVICE, account)
        .map_err(|e| format!("{}\n\n{}", e, keychain_help_text()))?;
    match entry.delete_credential() {
        Ok(()) => Ok(true),
        Err(keyring::Error::NoEntry) => Ok(false),
        Err(e) => Err(format!("{}\n\n{}", e, keychain_help_text())),
    }
}

/// Help text shown when the system keychain is not accessible.
fn keychain_help_text() -> &'static str {
    if cfg!(target_os = "linux") {
        "The system secret service (D-Bus Secret Service API) is not available.\n\
         This typically means no keyring daemon is running or unlocked.\n\n\
         To fix this:\n\
         - Desktop: ensure gnome-keyring-daemon or kwallet is running\n\
         - Headless/SSH: install and start gnome-keyring-daemon:\n\
         \n\
             sudo apt install gnome-keyring libsecret-1-0  # Debian/Ubuntu\n\
             sudo pacman -S gnome-keyring libsecret         # Arch\n\
         \n\
             eval $(gnome-keyring-daemon --start --components=secrets 2>/dev/null)\n\
         \n\
         - Alternatively, use env: vars or literal keys in config.toml:\n\
             api_key = \"env:OPENAI_API_KEY\"\n\
             api_key = \"sk-your-key-here\""
    } else {
        "The system keychain is not accessible.\n\
         Alternatively, use env: vars or literal keys in config.toml:\n\
             api_key = \"env:OPENAI_API_KEY\"\n\
             api_key = \"sk-your-key-here\""
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::env;

    #[test]
    fn test_get_api_key_raw() {
        let key = "sk-12345";
        assert_eq!(get_api_key(key), Some("sk-12345".to_string()));
    }

    #[test]
    fn test_get_api_key_env() {
        env::set_var("TEST_API_KEY", "secret-value");
        assert_eq!(
            get_api_key("env:TEST_API_KEY"),
            Some("secret-value".to_string())
        );
        env::remove_var("TEST_API_KEY");
    }

    #[test]
    fn test_get_api_key_env_missing() {
        assert_eq!(get_api_key("env:NON_EXISTENT_VAR"), None);
    }

    #[test]
    fn test_get_api_key_empty_string() {
        assert_eq!(get_api_key(""), None);
    }

    #[test]
    fn test_get_api_key_keychain_prefix_parsed() {
        // We can't reliably test actual keychain access in CI, but we can
        // verify the prefix is recognised and doesn't fall through to literal.
        let result = get_api_key("keychain:test-account");
        // On systems without a keychain or where the entry doesn't exist,
        // this returns None (not the literal string "keychain:test-account").
        assert_ne!(
            result,
            Some("keychain:test-account".to_string()),
            "keychain: prefix should be parsed, not returned as literal"
        );
    }

    #[test]
    fn test_get_api_key_keychain_empty_account() {
        // "keychain:" with empty account should still trigger keychain path
        let result = get_api_key("keychain:");
        assert_ne!(result, Some("keychain:".to_string()));
    }

    #[test]
    fn test_provider_config_resolved_base_url() {
        let config = ProviderConfig {
            type_: "openai".to_string(),
            base_url: String::new(),
            ..Default::default()
        };
        assert_eq!(config.resolved_base_url(), "https://api.openai.com/v1");

        let config_with_url = ProviderConfig {
            type_: "openai".to_string(),
            base_url: "https://custom.api.com/v1".to_string(),
            ..Default::default()
        };
        assert_eq!(
            config_with_url.resolved_base_url(),
            "https://custom.api.com/v1"
        );
    }

    #[test]
    fn test_provider_config_resolved_base_url_env() {
        env::set_var("TEST_BASE_URL", "https://env.api.test/v1");
        let config_with_env = ProviderConfig {
            type_: "openai".to_string(),
            base_url: "env:TEST_BASE_URL".to_string(),
            ..Default::default()
        };
        assert_eq!(
            config_with_env.resolved_base_url(),
            "https://env.api.test/v1"
        );
        env::remove_var("TEST_BASE_URL");
    }

    #[test]
    fn test_provider_config_resolved_api_key_fallback() {
        env::set_var("OPENAI_API_KEY", "fallback-key");
        let config = ProviderConfig {
            type_: "openai".to_string(),
            api_key: String::new(),
            ..Default::default()
        };
        assert_eq!(config.resolved_api_key(), "fallback-key");
        env::remove_var("OPENAI_API_KEY");
    }

    #[test]
    fn test_logging_config_effective_default() {
        // New style
        let config = LoggingConfig {
            default: "file".to_string(),
            sink: String::new(),
            backends: Vec::new(),
        };
        assert_eq!(config.effective_default(), "file");

        // Legacy style
        let legacy = LoggingConfig {
            default: "stdout".to_string(),
            sink: "file".to_string(),
            backends: Vec::new(),
        };
        assert_eq!(legacy.effective_default(), "file");
    }

    #[test]
    fn test_provider_config_deserialization() {
        let toml_str = r#"
            type = "anthropic"
            api_key = "env:ANTHROPIC_API_KEY"
            base_url = "https://api.anthropic.com/v1"
        "#;

        let config: ProviderConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(config.type_, "anthropic");
        assert_eq!(config.provider_type(), ProviderType::Anthropic);
    }

    #[test]
    fn test_provider_config_with_compat() {
        let toml_str = r#"
            type = "openai-compatible"
            api_key = "dummy"
            base_url = "http://localhost:8000/v1"
            
            [compat]
            supports_store = false
            max_tokens_field = "max_tokens"
            supports_stream_options = false
        "#;

        let config: ProviderConfig = toml::from_str(toml_str).unwrap();
        assert!(!config.compat.supports_store.unwrap());
        assert!(!config.compat.supports_stream_options.unwrap());

        // Test resolved compat merges with URL detection
        let resolved = config.resolved_compat();
        assert!(!resolved.supports_store());
        assert!(!resolved.supports_stream_options());
        assert_eq!(resolved.max_tokens_field(), "max_tokens");
    }

    #[test]
    fn test_logging_backend_deserialization() {
        let toml_str = r#"
            [[backends]]
            type = "stdout"
            format = "pretty"
            
            [[backends]]
            type = "file"
            path = "./logs/eavs.jsonl"
            rotate = "daily"
            
            [[backends]]
            type = "webhook"
            url = "https://example.com/logs"
            batch_size = 50
            flush_interval_secs = 10
            
            [[backends]]
            type = "otel"
            endpoint = "http://localhost:4317"
            protocol = "grpc"
        "#;

        let config: LoggingConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(config.backends.len(), 4);

        match &config.backends[0] {
            LogBackend::Stdout { format } => assert_eq!(format, "pretty"),
            _ => panic!("Expected Stdout backend"),
        }

        match &config.backends[1] {
            LogBackend::File { path, rotate, .. } => {
                assert_eq!(path, "./logs/eavs.jsonl");
                assert_eq!(rotate, "daily");
            }
            _ => panic!("Expected File backend"),
        }

        match &config.backends[2] {
            LogBackend::Webhook {
                url,
                batch_size,
                flush_interval_secs,
                ..
            } => {
                assert_eq!(url, "https://example.com/logs");
                assert_eq!(*batch_size, 50);
                assert_eq!(*flush_interval_secs, 10);
            }
            _ => panic!("Expected Webhook backend"),
        }

        match &config.backends[3] {
            LogBackend::OpenTelemetry {
                endpoint, protocol, ..
            } => {
                assert_eq!(endpoint, "http://localhost:4317");
                assert_eq!(protocol, "grpc");
            }
            _ => panic!("Expected OpenTelemetry backend"),
        }
    }

    #[test]
    fn test_full_app_config_deserialization() {
        let toml_str = r#"
            [server]
            host = "0.0.0.0"
            port = 8080

            [providers.default]
            type = "openai"
            api_key = "env:OPENAI_API_KEY"

            [providers.anthropic]
            type = "anthropic"
            api_key = "env:ANTHROPIC_API_KEY"

            [providers.local]
            type = "ollama"
            base_url = "http://localhost:11434/v1"

            [logging]
            default = "stdout"

            [analysis]
            enabled = true
            broadcast_channel_size = 512
        "#;

        let config: AppConfig = toml::from_str(toml_str).unwrap();

        assert_eq!(config.server.host, "0.0.0.0");
        assert_eq!(config.server.port, 8080);
        assert_eq!(config.providers.len(), 3);
        assert!(config.providers.contains_key("default"));
        assert!(config.providers.contains_key("anthropic"));
        assert!(config.providers.contains_key("local"));
        assert!(config.analysis.enabled);
    }

    #[test]
    fn test_xdg_config_path_with_xdg_env() {
        let original = env::var("XDG_CONFIG_HOME").ok();
        env::set_var("XDG_CONFIG_HOME", "/tmp/test-xdg");

        let path = AppConfig::get_xdg_config_path();
        assert!(path.is_some());
        assert_eq!(
            path.unwrap(),
            PathBuf::from("/tmp/test-xdg/eavs/config.toml")
        );

        if let Some(val) = original {
            env::set_var("XDG_CONFIG_HOME", val);
        } else {
            env::remove_var("XDG_CONFIG_HOME");
        }
    }

    #[test]
    fn test_resolve_provider_case_insensitive() {
        let toml_str = r#"
            [providers.OpenAI]
            type = "openai"
            api_key = "test-key"

            [providers.Anthropic]
            type = "anthropic"
            api_key = "test-key"
        "#;

        let config: AppConfig = toml::from_str(toml_str).unwrap();

        // Test various case combinations
        let result = config.resolve_provider("openai");
        assert!(result.is_some());
        assert_eq!(result.unwrap().resolved_name, "OpenAI");

        let result = config.resolve_provider("OPENAI");
        assert!(result.is_some());
        assert_eq!(result.unwrap().resolved_name, "OpenAI");

        let result = config.resolve_provider("OpenAI");
        assert!(result.is_some());
        assert_eq!(result.unwrap().resolved_name, "OpenAI");

        let result = config.resolve_provider("ANTHROPIC");
        assert!(result.is_some());
        assert_eq!(result.unwrap().resolved_name, "Anthropic");

        // Test unknown provider returns None
        let result = config.resolve_provider("unknown");
        assert!(result.is_none());
    }

    #[test]
    fn test_resolve_provider_fallback_to_default() {
        let toml_str = r#"
            [providers.default]
            type = "openai"
            api_key = "test-key"

            [providers.anthropic]
            type = "anthropic"
            api_key = "test-key"
        "#;

        let config: AppConfig = toml::from_str(toml_str).unwrap();

        // Empty string should fall back to default
        let result = config.resolve_provider("");
        assert!(result.is_some());
        let lookup = result.unwrap();
        assert_eq!(lookup.resolved_name, "default");
        assert!(lookup.was_fallback);

        // "default" should not be marked as fallback
        let result = config.resolve_provider("default");
        assert!(result.is_some());
        let lookup = result.unwrap();
        assert_eq!(lookup.resolved_name, "default");
        assert!(!lookup.was_fallback);

        // Unknown provider should NOT fall back to default
        let result = config.resolve_provider("unknown");
        assert!(result.is_none());
    }

    #[test]
    fn test_get_provider_case_insensitive() {
        let toml_str = r#"
            [providers.Azure]
            type = "azure"
            api_key = "test-key"
        "#;

        let config: AppConfig = toml::from_str(toml_str).unwrap();

        // All case variants should work
        assert!(config.get_provider("azure").is_some());
        assert!(config.get_provider("AZURE").is_some());
        assert!(config.get_provider("Azure").is_some());
        assert!(config.get_provider("AzUrE").is_some());

        // Unknown should return None
        assert!(config.get_provider("unknown").is_none());
    }
}

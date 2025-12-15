use config::{Config, ConfigError, File};
use serde::Deserialize;
use std::collections::HashMap;
use std::path::PathBuf;

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
    pub state: StateConfig,
    #[serde(default)]
    pub keys: KeysConfig,
}

#[derive(Debug, Deserialize, Clone)]
#[serde(default)]
pub struct ServerConfig {
    pub host: String,
    pub port: u16,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            host: "127.0.0.1".to_string(),
            port: 3000,
        }
    }
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
    #[allow(dead_code)]
    pub compat: CompatSettings,
    /// Custom headers to add to requests
    #[serde(default)]
    pub headers: HashMap<String, String>,
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
        }
    }
}

impl ProviderConfig {
    /// Get the resolved base URL (using provider defaults if not specified).
    pub fn resolved_base_url(&self) -> String {
        if !self.base_url.is_empty() {
            self.base_url.clone()
        } else {
            let provider = ProviderType::from_str(&self.type_);
            provider
                .info()
                .default_base_url
                .unwrap_or("http://localhost:8000/v1")
                .to_string()
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

    /// Get the provider type enum.
    pub fn provider_type(&self) -> ProviderType {
        ProviderType::from_str(&self.type_)
    }

    /// Get compat settings merged with URL-detected defaults.
    #[allow(dead_code)]
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
}

impl Default for AnalysisConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            broadcast_channel_size: 1024,
        }
    }
}

/// Configuration for conversation state storage.
#[derive(Debug, Deserialize, Clone)]
#[serde(default)]
pub struct StateConfig {
    /// Enable conversation state storage
    pub enabled: bool,
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
            ttl_secs: 3600,            // 1 hour default
            cleanup_interval_secs: 60, // Cleanup every minute
            max_conversations: 10000,  // Max 10k conversations
        }
    }
}

/// Configuration for virtual API keys.
#[derive(Debug, Deserialize, Clone)]
#[serde(default)]
pub struct KeysConfig {
    /// Enable virtual API key support
    pub enabled: bool,
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
}

impl Default for KeysConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            database_path: "~/.eavs/keys.db".to_string(),
            master_key: None,
            allow_self_provisioning: false,
            default_rpm_limit: None,
            default_budget_usd: None,
            update_pricing_on_startup: false,
            word_lists_path: None, // Uses XDG data directory by default
        }
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
        self.master_key.as_ref().and_then(|k| {
            if let Some(var_name) = k.strip_prefix("env:") {
                std::env::var(var_name).ok()
            } else {
                Some(k.clone())
            }
        })
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
            .or_else(|| env_var_nonempty("HOME").map(|home| PathBuf::from(home).join(".local/share")))
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

    fn finalize_config(builder: config::ConfigBuilder<config::builder::DefaultState>) -> Result<Self, ConfigError> {
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
            ConfigError::Message(format!("Failed to create config dir {}: {}", dir.display(), e))
        })?;

        let schema_path = dir.join("config.schema.json");
        if !schema_path.exists() {
            std::fs::write(&schema_path, include_bytes!("../config/config.schema.json")).map_err(|e| {
                ConfigError::Message(format!(
                    "Failed to write schema file {}: {}",
                    schema_path.display(),
                    e
                ))
            })?;
        }

        if !config_path.exists() {
            std::fs::write(config_path, include_str!("../config/config.example.toml")).map_err(|e| {
                ConfigError::Message(format!(
                    "Failed to write default config file {}: {}",
                    config_path.display(),
                    e
                ))
            })?;
        }

        Ok(())
    }

    /// Get a provider config by name (case-insensitive).
    /// Returns None if the provider is not found.
    /// Use "default" explicitly if you want the default provider.
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
        if let Some((resolved_name, config)) = self.providers
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
    pub was_fallback: bool,
}

/// Resolve API key from config value.
/// Supports "env:VAR_NAME" syntax to read from environment variables.
pub fn get_api_key(config_key: &str) -> Option<String> {
    if config_key.is_empty() {
        return None;
    }

    if let Some(var_name) = config_key.strip_prefix("env:") {
        std::env::var(var_name).ok()
    } else {
        Some(config_key.to_string())
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
        assert_eq!(get_api_key("env:TEST_API_KEY"), Some("secret-value".to_string()));
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
        "#;

        let config: ProviderConfig = toml::from_str(toml_str).unwrap();
        assert!(!config.compat.supports_store.unwrap());
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
                endpoint,
                protocol,
                ..
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

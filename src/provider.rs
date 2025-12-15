use reqwest::RequestBuilder;
use serde::Deserialize;

/// Supported LLM provider types with their specific authentication and API patterns.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ProviderType {
    #[default]
    OpenAI,
    Anthropic,
    Google,
    Azure,
    Mistral,
    Groq,
    Cerebras,
    XAI,
    OpenRouter,
    Bedrock,
    /// Generic OpenAI-compatible APIs (Ollama, vLLM, LM Studio, etc.)
    OpenAICompatible,
}

/// Provider metadata with default configuration values.
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct ProviderInfo {
    pub provider_type: ProviderType,
    pub default_base_url: Option<&'static str>,
    pub env_key_name: Option<&'static str>,
    pub auth_style: AuthStyle,
}

/// How authentication is applied for each provider.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)]
pub enum AuthStyle {
    /// Bearer token in Authorization header
    BearerToken,
    /// API key in custom header (e.g., x-api-key for Anthropic)
    ApiKeyHeader(&'static str),
    /// API key in query parameter (e.g., Azure api-version)
    QueryParam(&'static str),
    /// Azure-style api-key header
    AzureApiKey,
    /// No authentication required
    None,
}

impl ProviderType {
    /// Parse provider type from string (case-insensitive).
    pub fn from_str(s: &str) -> Self {
        match s.to_lowercase().as_str() {
            "openai" => Self::OpenAI,
            "anthropic" | "claude" => Self::Anthropic,
            "google" | "gemini" | "vertex" => Self::Google,
            "azure" | "azure-openai" => Self::Azure,
            "mistral" => Self::Mistral,
            "groq" => Self::Groq,
            "cerebras" => Self::Cerebras,
            "xai" | "grok" => Self::XAI,
            "openrouter" => Self::OpenRouter,
            "bedrock" | "aws-bedrock" => Self::Bedrock,
            "ollama" | "vllm" | "lmstudio" | "openai-compatible" | "compatible" => {
                Self::OpenAICompatible
            }
            _ => Self::OpenAI, // Default fallback
        }
    }

    /// Get provider metadata including defaults.
    pub fn info(&self) -> ProviderInfo {
        match self {
            Self::OpenAI => ProviderInfo {
                provider_type: *self,
                default_base_url: Some("https://api.openai.com/v1"),
                env_key_name: Some("OPENAI_API_KEY"),
                auth_style: AuthStyle::BearerToken,
            },
            Self::Anthropic => ProviderInfo {
                provider_type: *self,
                default_base_url: Some("https://api.anthropic.com/v1"),
                env_key_name: Some("ANTHROPIC_API_KEY"),
                auth_style: AuthStyle::ApiKeyHeader("x-api-key"),
            },
            Self::Google => ProviderInfo {
                provider_type: *self,
                default_base_url: Some("https://generativelanguage.googleapis.com/v1beta"),
                env_key_name: Some("GEMINI_API_KEY"),
                auth_style: AuthStyle::BearerToken,
            },
            Self::Azure => ProviderInfo {
                provider_type: *self,
                default_base_url: None, // Must be specified per-deployment
                env_key_name: Some("AZURE_OPENAI_KEY"),
                auth_style: AuthStyle::AzureApiKey,
            },
            Self::Mistral => ProviderInfo {
                provider_type: *self,
                default_base_url: Some("https://api.mistral.ai/v1"),
                env_key_name: Some("MISTRAL_API_KEY"),
                auth_style: AuthStyle::BearerToken,
            },
            Self::Groq => ProviderInfo {
                provider_type: *self,
                default_base_url: Some("https://api.groq.com/openai/v1"),
                env_key_name: Some("GROQ_API_KEY"),
                auth_style: AuthStyle::BearerToken,
            },
            Self::Cerebras => ProviderInfo {
                provider_type: *self,
                default_base_url: Some("https://api.cerebras.ai/v1"),
                env_key_name: Some("CEREBRAS_API_KEY"),
                auth_style: AuthStyle::BearerToken,
            },
            Self::XAI => ProviderInfo {
                provider_type: *self,
                default_base_url: Some("https://api.x.ai/v1"),
                env_key_name: Some("XAI_API_KEY"),
                auth_style: AuthStyle::BearerToken,
            },
            Self::OpenRouter => ProviderInfo {
                provider_type: *self,
                default_base_url: Some("https://openrouter.ai/api/v1"),
                env_key_name: Some("OPENROUTER_API_KEY"),
                auth_style: AuthStyle::BearerToken,
            },
            Self::Bedrock => ProviderInfo {
                provider_type: *self,
                default_base_url: None, // Region-specific; see ProviderConfig.aws_region
                env_key_name: None,
                auth_style: AuthStyle::None,
            },
            Self::OpenAICompatible => ProviderInfo {
                provider_type: *self,
                default_base_url: None, // Must be specified
                env_key_name: None,
                auth_style: AuthStyle::BearerToken,
            },
        }
    }

    /// Apply authentication headers/params to a request builder.
    pub fn apply_auth(&self, builder: RequestBuilder, api_key: &str) -> RequestBuilder {
        let info = self.info();

        match info.auth_style {
            AuthStyle::BearerToken => {
                if !api_key.is_empty() {
                    builder.header("Authorization", format!("Bearer {}", api_key))
                } else {
                    builder
                }
            }
            AuthStyle::ApiKeyHeader(header_name) => builder.header(header_name, api_key),
            AuthStyle::AzureApiKey => builder.header("api-key", api_key),
            AuthStyle::QueryParam(_param_name) => {
                // Query params are handled separately in URL construction
                builder
            }
            AuthStyle::None => builder,
        }
    }

    /// Apply provider-specific headers (e.g., Anthropic version header).
    pub fn apply_extra_headers(&self, builder: RequestBuilder) -> RequestBuilder {
        match self {
            Self::Anthropic => builder.header("anthropic-version", "2023-06-01"),
            Self::OpenRouter => {
                // OpenRouter likes to know the app name
                builder.header("HTTP-Referer", "https://github.com/eavs-proxy")
            }
            _ => builder,
        }
    }

    /// Check if this provider uses OpenAI-compatible chat completions API.
    #[allow(dead_code)]
    pub fn is_openai_compatible(&self) -> bool {
        matches!(
            self,
            Self::OpenAI
                | Self::Azure
                | Self::Mistral
                | Self::Groq
                | Self::Cerebras
                | Self::XAI
                | Self::OpenRouter
                | Self::OpenAICompatible
        )
    }

    /// Check if this provider requires request transformation.
    #[allow(dead_code)]
    pub fn needs_transform(&self) -> bool {
        matches!(self, Self::Anthropic | Self::Google | Self::Bedrock)
    }
}

/// Configuration for compatibility settings with OpenAI-compatible APIs.
/// Some providers have slight differences from the OpenAI API.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct CompatSettings {
    /// Whether provider supports the `store` field (default: true)
    pub supports_store: Option<bool>,
    /// Whether provider supports `developer` role vs `system` (default: true)
    pub supports_developer_role: Option<bool>,
    /// Which max tokens field to use: "max_completion_tokens" or "max_tokens"
    pub max_tokens_field: Option<String>,
}

impl CompatSettings {
    /// Merge with detected defaults based on base_url.
    #[allow(dead_code)]
    pub fn with_detected_defaults(self, base_url: &str) -> Self {
        let detected = Self::detect_from_url(base_url);
        Self {
            supports_store: self.supports_store.or(detected.supports_store),
            supports_developer_role: self
                .supports_developer_role
                .or(detected.supports_developer_role),
            max_tokens_field: self.max_tokens_field.or(detected.max_tokens_field),
        }
    }

    /// Detect compatibility settings from URL patterns.
    #[allow(dead_code)]
    fn detect_from_url(base_url: &str) -> Self {
        let url_lower = base_url.to_lowercase();

        // LiteLLM proxy
        if url_lower.contains("litellm") || url_lower.contains(":4000") {
            return Self {
                supports_store: Some(false),
                supports_developer_role: Some(true),
                max_tokens_field: Some("max_tokens".to_string()),
            };
        }

        // Ollama
        if url_lower.contains("11434") || url_lower.contains("ollama") {
            return Self {
                supports_store: Some(false),
                supports_developer_role: Some(false),
                max_tokens_field: Some("max_tokens".to_string()),
            };
        }

        // vLLM
        if url_lower.contains("vllm") || url_lower.contains(":8000") {
            return Self {
                supports_store: Some(false),
                supports_developer_role: Some(false),
                max_tokens_field: Some("max_tokens".to_string()),
            };
        }

        // Default: full OpenAI compatibility
        Self::default()
    }

    #[allow(dead_code)]
    pub fn supports_store(&self) -> bool {
        self.supports_store.unwrap_or(true)
    }

    #[allow(dead_code)]
    pub fn supports_developer_role(&self) -> bool {
        self.supports_developer_role.unwrap_or(true)
    }

    #[allow(dead_code)]
    pub fn max_tokens_field(&self) -> &str {
        self.max_tokens_field
            .as_deref()
            .unwrap_or("max_completion_tokens")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_provider_type_from_str() {
        assert_eq!(ProviderType::from_str("openai"), ProviderType::OpenAI);
        assert_eq!(ProviderType::from_str("OpenAI"), ProviderType::OpenAI);
        assert_eq!(ProviderType::from_str("anthropic"), ProviderType::Anthropic);
        assert_eq!(ProviderType::from_str("claude"), ProviderType::Anthropic);
        assert_eq!(ProviderType::from_str("google"), ProviderType::Google);
        assert_eq!(ProviderType::from_str("gemini"), ProviderType::Google);
        assert_eq!(ProviderType::from_str("azure"), ProviderType::Azure);
        assert_eq!(ProviderType::from_str("mistral"), ProviderType::Mistral);
        assert_eq!(ProviderType::from_str("groq"), ProviderType::Groq);
        assert_eq!(ProviderType::from_str("cerebras"), ProviderType::Cerebras);
        assert_eq!(ProviderType::from_str("xai"), ProviderType::XAI);
        assert_eq!(ProviderType::from_str("grok"), ProviderType::XAI);
        assert_eq!(ProviderType::from_str("openrouter"), ProviderType::OpenRouter);
        assert_eq!(
            ProviderType::from_str("ollama"),
            ProviderType::OpenAICompatible
        );
        assert_eq!(
            ProviderType::from_str("vllm"),
            ProviderType::OpenAICompatible
        );
        assert_eq!(ProviderType::from_str("unknown"), ProviderType::OpenAI);
    }

    #[test]
    fn test_provider_info_defaults() {
        let openai = ProviderType::OpenAI.info();
        assert_eq!(
            openai.default_base_url,
            Some("https://api.openai.com/v1")
        );
        assert_eq!(openai.env_key_name, Some("OPENAI_API_KEY"));

        let anthropic = ProviderType::Anthropic.info();
        assert_eq!(
            anthropic.default_base_url,
            Some("https://api.anthropic.com/v1")
        );
        assert!(matches!(
            anthropic.auth_style,
            AuthStyle::ApiKeyHeader("x-api-key")
        ));

        let azure = ProviderType::Azure.info();
        assert!(azure.default_base_url.is_none());
        assert!(matches!(azure.auth_style, AuthStyle::AzureApiKey));
    }

    #[test]
    fn test_apply_auth_bearer() {
        let client = reqwest::Client::new();
        let builder = client.get("https://api.openai.com/v1/chat/completions");
        let builder = ProviderType::OpenAI.apply_auth(builder, "sk-test-key");
        let request = builder.build().unwrap();

        assert_eq!(
            request.headers().get("Authorization").unwrap(),
            "Bearer sk-test-key"
        );
    }

    #[test]
    fn test_apply_auth_anthropic() {
        let client = reqwest::Client::new();
        let builder = client.get("https://api.anthropic.com/v1/messages");
        let builder = ProviderType::Anthropic.apply_auth(builder, "sk-ant-key");
        let builder = ProviderType::Anthropic.apply_extra_headers(builder);
        let request = builder.build().unwrap();

        assert_eq!(request.headers().get("x-api-key").unwrap(), "sk-ant-key");
        assert_eq!(
            request.headers().get("anthropic-version").unwrap(),
            "2023-06-01"
        );
    }

    #[test]
    fn test_apply_auth_azure() {
        let client = reqwest::Client::new();
        let builder = client.get("https://myresource.openai.azure.com/");
        let builder = ProviderType::Azure.apply_auth(builder, "azure-key");
        let request = builder.build().unwrap();

        assert_eq!(request.headers().get("api-key").unwrap(), "azure-key");
    }

    #[test]
    fn test_apply_auth_empty_key() {
        let client = reqwest::Client::new();
        let builder = client.get("http://localhost:11434/v1/chat/completions");
        let builder = ProviderType::OpenAICompatible.apply_auth(builder, "");
        let request = builder.build().unwrap();

        assert!(request.headers().get("Authorization").is_none());
    }

    #[test]
    fn test_is_openai_compatible() {
        assert!(ProviderType::OpenAI.is_openai_compatible());
        assert!(ProviderType::Groq.is_openai_compatible());
        assert!(ProviderType::OpenRouter.is_openai_compatible());
        assert!(!ProviderType::Anthropic.is_openai_compatible());
        assert!(!ProviderType::Google.is_openai_compatible());
    }

    #[test]
    fn test_compat_settings_detection() {
        let ollama = CompatSettings::default().with_detected_defaults("http://localhost:11434/v1");
        assert!(!ollama.supports_store());
        assert_eq!(ollama.max_tokens_field(), "max_tokens");

        let openai = CompatSettings::default().with_detected_defaults("https://api.openai.com/v1");
        assert!(openai.supports_store());
        assert_eq!(openai.max_tokens_field(), "max_completion_tokens");
    }

    #[test]
    fn test_compat_settings_override() {
        let custom = CompatSettings {
            supports_store: Some(false),
            supports_developer_role: None,
            max_tokens_field: Some("max_tokens".to_string()),
        }
        .with_detected_defaults("https://api.openai.com/v1");

        // Explicit setting should override detection
        assert!(!custom.supports_store());
        assert_eq!(custom.max_tokens_field(), "max_tokens");
        // Non-overridden should use detection (OpenAI default)
        assert!(custom.supports_developer_role());
    }
}

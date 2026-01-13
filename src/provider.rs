use serde::Deserialize;

/// Supported LLM provider types with their specific authentication and API patterns.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[allow(clippy::upper_case_acronyms)]
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
    /// OpenAI Codex CLI (ChatGPT backend via OAuth) - uses Responses API
    OpenAICodex,
    /// OpenAI Responses API (api.openai.com/v1/responses)
    OpenAIResponses,
    /// Mock provider for benchmarking - returns canned responses without network calls
    Mock,
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
            "openai-codex" | "codex" | "chatgpt" => Self::OpenAICodex,
            "openai-responses" | "responses" => Self::OpenAIResponses,
            "mock" | "echo" | "benchmark" => Self::Mock,
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
            Self::OpenAICodex => ProviderInfo {
                provider_type: *self,
                default_base_url: Some("https://chatgpt.com/backend-api"),
                env_key_name: None, // Uses OAuth
                auth_style: AuthStyle::BearerToken,
            },
            Self::OpenAIResponses => ProviderInfo {
                provider_type: *self,
                default_base_url: Some("https://api.openai.com/v1"),
                env_key_name: Some("OPENAI_API_KEY"),
                auth_style: AuthStyle::BearerToken,
            },
            Self::Mock => ProviderInfo {
                provider_type: *self,
                default_base_url: Some("mock://localhost"), // Special scheme - handled internally
                env_key_name: None,
                auth_style: AuthStyle::None,
            },
        }
    }

    /// Check if this provider is a mock provider (handled internally without network).
    pub fn is_mock(&self) -> bool {
        matches!(self, Self::Mock)
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
        matches!(
            self,
            Self::Anthropic | Self::Google | Self::Bedrock | Self::OpenAICodex | Self::OpenAIResponses
        )
    }
    
    /// Check if this provider uses the Responses API format.
    pub fn uses_responses_api(&self) -> bool {
        matches!(self, Self::OpenAICodex | Self::OpenAIResponses)
    }

    /// Check if this provider supports the /v1/models endpoint natively.
    /// Providers that don't support it will get a synthetic response.
    pub fn supports_models_endpoint(&self) -> bool {
        matches!(
            self,
            Self::OpenAI
                | Self::Anthropic  // Anthropic added /v1/models endpoint
                | Self::Azure      // Azure has /openai/models, we translate the path
                | Self::Mistral
                | Self::Groq
                | Self::XAI
                | Self::OpenRouter
                | Self::OpenAICompatible
                | Self::OpenAIResponses
        )
    }

    /// Get a list of well-known models for providers that don't support /v1/models.
    /// This returns a curated list of popular models for each provider.
    pub fn synthetic_models(&self) -> Vec<&'static str> {
        match self {
            Self::Anthropic => vec![
                "claude-3-5-sonnet-20241022",
                "claude-3-5-haiku-20241022",
                "claude-3-opus-20240229",
                "claude-3-sonnet-20240229",
                "claude-3-haiku-20240307",
                "claude-sonnet-4-20250514",
                "claude-haiku-4-20250514",
            ],
            Self::Google => vec![
                "gemini-1.5-pro",
                "gemini-1.5-flash",
                "gemini-1.5-flash-8b",
                "gemini-2.0-flash-exp",
                "gemini-exp-1206",
            ],
            Self::Azure => vec![
                // Azure uses deployment names, return common model names
                "gpt-4o",
                "gpt-4o-mini",
                "gpt-4",
                "gpt-4-turbo",
                "gpt-35-turbo",
            ],
            Self::Bedrock => vec![
                "anthropic.claude-3-5-sonnet-20241022-v2:0",
                "anthropic.claude-3-5-haiku-20241022-v1:0",
                "anthropic.claude-3-opus-20240229-v1:0",
                "anthropic.claude-3-sonnet-20240229-v1:0",
                "amazon.titan-text-express-v1",
                "meta.llama3-70b-instruct-v1:0",
            ],
            Self::Cerebras => vec![
                "llama3.1-8b",
                "llama3.1-70b",
            ],
            Self::Mock => vec!["mock-model"],
            // OpenAI-compatible providers should fetch from upstream
            _ => vec![],
        }
    }
}

/// Detect provider name from a hostname.
///
/// Used by the mitmproxy integration to automatically detect which provider
/// a request is targeting based on the original host. This allows transparent
/// interception without requiring explicit X-Provider headers.
///
/// Returns `None` if the host doesn't match any known LLM provider.
pub fn detect_provider_from_host(host: &str) -> Option<&'static str> {
    let host_lower = host.to_lowercase();

    // OpenAI (API and desktop app)
    if host_lower.contains("openai.com") || host_lower.contains("chatgpt.com") {
        return Some("openai");
    }

    // Anthropic (API and desktop app)
    if host_lower.contains("anthropic.com") || host_lower.contains("claude.ai") {
        return Some("anthropic");
    }

    // Google AI / Vertex AI / Gemini
    if host_lower.contains("googleapis.com")
        || host_lower.contains("aiplatform.googleapis.com")
        || host_lower.contains("gemini.google.com")
        || host_lower.contains("aistudio.google.com")
    {
        return Some("google");
    }

    // Mistral
    if host_lower.contains("mistral.ai") {
        return Some("mistral");
    }

    // Groq
    if host_lower.contains("groq.com") {
        return Some("groq");
    }

    // Cerebras
    if host_lower.contains("cerebras.ai") {
        return Some("cerebras");
    }

    // xAI (Grok)
    if host_lower.contains("x.ai") {
        return Some("xai");
    }

    // OpenRouter
    if host_lower.contains("openrouter.ai") {
        return Some("openrouter");
    }

    // Together AI
    if host_lower.contains("together.xyz") {
        return Some("together");
    }

    // Cohere
    if host_lower.contains("cohere.ai") || host_lower.contains("cohere.com") {
        return Some("cohere");
    }

    // Perplexity
    if host_lower.contains("perplexity.ai") {
        return Some("perplexity");
    }

    // DeepSeek
    if host_lower.contains("deepseek.com") {
        return Some("deepseek");
    }

    // Fireworks AI
    if host_lower.contains("fireworks.ai") {
        return Some("fireworks");
    }

    // AI21
    if host_lower.contains("ai21.com") {
        return Some("ai21");
    }

    // Replicate
    if host_lower.contains("replicate.com") {
        return Some("replicate");
    }

    None
}

/// Detect provider from model name patterns.
///
/// Used for auto-routing when a request comes through the generic /v1/ endpoint
/// without an explicit provider. Returns a provider name if the model name
/// matches known patterns.
///
/// # Examples
/// - `claude-3-opus` -> Some("anthropic")
/// - `gpt-4` -> Some("openai")
/// - `gemini-pro` -> Some("google")
/// - `mistral-large` -> Some("mistral")
/// - `custom-model` -> None (unknown pattern)
pub fn detect_provider_from_model(model: &str) -> Option<&'static str> {
    let model_lower = model.to_lowercase();

    // Anthropic Claude models
    if model_lower.starts_with("claude") {
        return Some("anthropic");
    }

    // OpenAI Codex models (GPT-5.x Codex, uses ChatGPT backend)
    if model_lower.contains("codex") || model_lower.starts_with("gpt-5") {
        return Some("openai-codex");
    }

    // OpenAI models (GPT, o1, o3, etc.)
    if model_lower.starts_with("gpt-")
        || model_lower.starts_with("o1")
        || model_lower.starts_with("o3")
        || model_lower.starts_with("o4")
        || model_lower.starts_with("chatgpt")
        || model_lower.starts_with("text-embedding")
        || model_lower.starts_with("text-davinci")
        || model_lower.starts_with("davinci")
        || model_lower.starts_with("dall-e")
        || model_lower.starts_with("whisper")
        || model_lower.starts_with("tts-")
    {
        return Some("openai");
    }

    // Google Gemini models
    if model_lower.starts_with("gemini") || model_lower.starts_with("models/gemini") {
        return Some("google");
    }

    // Mistral models
    if model_lower.starts_with("mistral")
        || model_lower.starts_with("codestral")
        || model_lower.starts_with("pixtral")
        || model_lower.starts_with("ministral")
    {
        return Some("mistral");
    }

    // Groq-specific model identifiers (often uses llama/mixtral but via groq)
    // Note: Groq also serves other models, so this is tricky
    // We only match explicit groq prefixes
    if model_lower.starts_with("groq/") {
        return Some("groq");
    }

    // xAI Grok models
    if model_lower.starts_with("grok") {
        return Some("xai");
    }

    // Cohere models
    if model_lower.starts_with("command") || model_lower.starts_with("cohere/") {
        return Some("cohere");
    }

    // Meta Llama models (when not via specific provider)
    // Note: These are often served by multiple providers, so we don't auto-route
    // unless there's a provider prefix
    if model_lower.starts_with("meta/") || model_lower.starts_with("llama-") {
        // Could be Groq, Together, Fireworks, etc. - don't auto-route
        return None;
    }

    // AWS Bedrock model patterns (anthropic.claude-*, amazon.titan-*, etc.)
    if model_lower.contains("anthropic.claude")
        || model_lower.contains("amazon.titan")
        || model_lower.contains("ai21.j2")
        || model_lower.contains("cohere.command")
        || model_lower.contains("meta.llama")
        || model_lower.contains("stability.")
    {
        return Some("bedrock");
    }

    // OpenRouter prefixed models
    if model_lower.starts_with("openrouter/") {
        return Some("openrouter");
    }

    // Perplexity models
    if model_lower.starts_with("pplx-") || model_lower.starts_with("sonar") {
        return Some("perplexity");
    }

    // DeepSeek models
    if model_lower.starts_with("deepseek") {
        return Some("deepseek");
    }

    None
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
            ProviderType::from_str("openai-responses"),
            ProviderType::OpenAIResponses
        );
        assert_eq!(
            ProviderType::from_str("responses"),
            ProviderType::OpenAIResponses
        );
        assert_eq!(
            ProviderType::from_str("openai-codex"),
            ProviderType::OpenAICodex
        );
        assert_eq!(
            ProviderType::from_str("ollama"),
            ProviderType::OpenAICompatible
        );
        assert_eq!(
            ProviderType::from_str("vllm"),
            ProviderType::OpenAICompatible
        );
        assert_eq!(ProviderType::from_str("unknown"), ProviderType::OpenAI);
        assert_eq!(ProviderType::from_str("mock"), ProviderType::Mock);
        assert_eq!(ProviderType::from_str("echo"), ProviderType::Mock);
        assert_eq!(ProviderType::from_str("benchmark"), ProviderType::Mock);
    }

    #[test]
    fn test_mock_provider() {
        let mock = ProviderType::Mock;
        assert!(mock.is_mock());
        assert!(!ProviderType::OpenAI.is_mock());
        
        let info = mock.info();
        assert_eq!(info.default_base_url, Some("mock://localhost"));
        assert!(matches!(info.auth_style, AuthStyle::None));
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

    #[test]
    fn test_detect_provider_from_host() {
        // OpenAI
        assert_eq!(detect_provider_from_host("api.openai.com"), Some("openai"));
        assert_eq!(detect_provider_from_host("chat.openai.com"), Some("openai"));
        assert_eq!(detect_provider_from_host("chatgpt.com"), Some("openai"));

        // Anthropic
        assert_eq!(detect_provider_from_host("api.anthropic.com"), Some("anthropic"));
        assert_eq!(detect_provider_from_host("claude.ai"), Some("anthropic"));

        // Google
        assert_eq!(detect_provider_from_host("generativelanguage.googleapis.com"), Some("google"));
        assert_eq!(detect_provider_from_host("gemini.google.com"), Some("google"));

        // Other providers
        assert_eq!(detect_provider_from_host("api.mistral.ai"), Some("mistral"));
        assert_eq!(detect_provider_from_host("api.groq.com"), Some("groq"));
        assert_eq!(detect_provider_from_host("api.cerebras.ai"), Some("cerebras"));
        assert_eq!(detect_provider_from_host("api.x.ai"), Some("xai"));
        assert_eq!(detect_provider_from_host("openrouter.ai"), Some("openrouter"));
        assert_eq!(detect_provider_from_host("api.together.xyz"), Some("together"));
        assert_eq!(detect_provider_from_host("api.perplexity.ai"), Some("perplexity"));
        assert_eq!(detect_provider_from_host("api.deepseek.com"), Some("deepseek"));

        // Unknown hosts
        assert_eq!(detect_provider_from_host("example.com"), None);
        assert_eq!(detect_provider_from_host("localhost"), None);
    }

    #[test]
    fn test_detect_provider_from_host_case_insensitive() {
        assert_eq!(detect_provider_from_host("API.OPENAI.COM"), Some("openai"));
        assert_eq!(detect_provider_from_host("Api.Anthropic.Com"), Some("anthropic"));
    }

    #[test]
    fn test_detect_provider_from_model_anthropic() {
        assert_eq!(detect_provider_from_model("claude-3-opus"), Some("anthropic"));
        assert_eq!(detect_provider_from_model("claude-3-5-sonnet-20240620"), Some("anthropic"));
        assert_eq!(detect_provider_from_model("claude-sonnet-4-20250514"), Some("anthropic"));
        assert_eq!(detect_provider_from_model("Claude-3-Haiku"), Some("anthropic"));
    }

    #[test]
    fn test_detect_provider_from_model_openai() {
        assert_eq!(detect_provider_from_model("gpt-4"), Some("openai"));
        assert_eq!(detect_provider_from_model("gpt-4-turbo"), Some("openai"));
        assert_eq!(detect_provider_from_model("gpt-4o"), Some("openai"));
        assert_eq!(detect_provider_from_model("o1-preview"), Some("openai"));
        assert_eq!(detect_provider_from_model("o3-mini"), Some("openai"));
        assert_eq!(detect_provider_from_model("text-embedding-3-small"), Some("openai"));
        assert_eq!(detect_provider_from_model("dall-e-3"), Some("openai"));
        assert_eq!(detect_provider_from_model("whisper-1"), Some("openai"));
        assert_eq!(detect_provider_from_model("tts-1-hd"), Some("openai"));
    }

    #[test]
    fn test_detect_provider_from_model_openai_codex() {
        // GPT-5.x models use the Codex provider (ChatGPT backend)
        assert_eq!(detect_provider_from_model("gpt-5.1-codex"), Some("openai-codex"));
        assert_eq!(detect_provider_from_model("gpt-5.2-codex-max"), Some("openai-codex"));
        assert_eq!(detect_provider_from_model("gpt-5.1"), Some("openai-codex"));
        assert_eq!(detect_provider_from_model("gpt-5.2"), Some("openai-codex"));
        assert_eq!(detect_provider_from_model("codex-mini-latest"), Some("openai-codex"));
    }

    #[test]
    fn test_detect_provider_from_model_google() {
        assert_eq!(detect_provider_from_model("gemini-pro"), Some("google"));
        assert_eq!(detect_provider_from_model("gemini-1.5-flash"), Some("google"));
        assert_eq!(detect_provider_from_model("models/gemini-pro"), Some("google"));
    }

    #[test]
    fn test_detect_provider_from_model_mistral() {
        assert_eq!(detect_provider_from_model("mistral-large"), Some("mistral"));
        assert_eq!(detect_provider_from_model("mistral-small-latest"), Some("mistral"));
        assert_eq!(detect_provider_from_model("codestral-latest"), Some("mistral"));
        assert_eq!(detect_provider_from_model("pixtral-12b"), Some("mistral"));
    }

    #[test]
    fn test_detect_provider_from_model_xai() {
        assert_eq!(detect_provider_from_model("grok-2"), Some("xai"));
        assert_eq!(detect_provider_from_model("grok-beta"), Some("xai"));
    }

    #[test]
    fn test_detect_provider_from_model_bedrock() {
        assert_eq!(detect_provider_from_model("anthropic.claude-3-sonnet"), Some("bedrock"));
        assert_eq!(detect_provider_from_model("amazon.titan-text-express-v1"), Some("bedrock"));
        assert_eq!(detect_provider_from_model("meta.llama3-70b-instruct-v1"), Some("bedrock"));
    }

    #[test]
    fn test_detect_provider_from_model_perplexity() {
        assert_eq!(detect_provider_from_model("pplx-70b-online"), Some("perplexity"));
        assert_eq!(detect_provider_from_model("sonar-small-online"), Some("perplexity"));
    }

    #[test]
    fn test_detect_provider_from_model_deepseek() {
        assert_eq!(detect_provider_from_model("deepseek-coder"), Some("deepseek"));
        assert_eq!(detect_provider_from_model("deepseek-chat"), Some("deepseek"));
    }

    #[test]
    fn test_detect_provider_from_model_unknown() {
        // Generic models that could be served by multiple providers
        assert_eq!(detect_provider_from_model("llama-3-70b"), None);
        assert_eq!(detect_provider_from_model("mixtral-8x7b"), None);
        assert_eq!(detect_provider_from_model("custom-finetuned-model"), None);
        assert_eq!(detect_provider_from_model("my-local-model"), None);
    }

    #[test]
    fn test_detect_provider_from_model_case_insensitive() {
        assert_eq!(detect_provider_from_model("CLAUDE-3-OPUS"), Some("anthropic"));
        assert_eq!(detect_provider_from_model("GPT-4"), Some("openai"));
        assert_eq!(detect_provider_from_model("Gemini-Pro"), Some("google"));
    }
}

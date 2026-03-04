//! Provider translation layer.
//!
//! This module handles translation between different LLM provider formats.
//! All providers translate to/from a canonical format defined in `types.rs`.

#![allow(dead_code)]

pub mod anthropic;
pub mod azure;
pub mod bedrock;
pub mod google;
pub mod messages;
pub mod mistral;
pub mod openai;
pub mod openai_responses;

use crate::provider::{CompatSettings, ProviderType};
use crate::types::{Context, StreamEvent, StreamState};

// Re-export transformers for convenience
pub use anthropic::AnthropicTransformer;
pub use azure::AzureTransformer;
pub use bedrock::BedrockTransformer;
pub use google::GoogleTransformer;
pub use mistral::MistralTransformer;
pub use openai::OpenAITransformer;
pub use openai_responses::OpenAIResponsesTransformer;

/// Trait for transforming requests to a provider's format.
pub trait RequestTransformer {
    /// Transform canonical context to provider-specific request body.
    fn transform_request(&self, context: &Context) -> Result<serde_json::Value, TransformError>;

    /// Get required headers for this provider.
    fn headers(&self, api_key: &str) -> Vec<(String, String)>;

    /// Get the endpoint path for this provider.
    fn endpoint_path(&self, context: &Context) -> String;
}

/// Trait for parsing provider responses into canonical events.
pub trait ResponseTransformer {
    /// Parse a provider-specific SSE chunk into canonical stream events.
    fn parse_stream_chunk(
        &self,
        chunk: &str,
        state: &mut StreamState,
    ) -> Result<Vec<StreamEvent>, TransformError>;

    /// Parse a complete (non-streaming) response.
    fn parse_response(&self, body: &serde_json::Value) -> Result<Vec<StreamEvent>, TransformError>;
}

/// Errors during transformation.
#[derive(Debug, Clone)]
pub enum TransformError {
    /// Invalid JSON structure
    InvalidJson(String),
    /// Missing required field
    MissingField(String),
    /// Invalid field value
    InvalidValue(String),
    /// Unsupported feature
    Unsupported(String),
}

impl std::fmt::Display for TransformError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidJson(msg) => write!(f, "Invalid JSON: {}", msg),
            Self::MissingField(field) => write!(f, "Missing required field: {}", field),
            Self::InvalidValue(msg) => write!(f, "Invalid value: {}", msg),
            Self::Unsupported(msg) => write!(f, "Unsupported: {}", msg),
        }
    }
}

impl std::error::Error for TransformError {}

/// A boxed transformer that can both transform requests and parse responses.
pub struct ProviderTransformer {
    inner: Box<dyn RequestResponseTransformer + Send + Sync>,
}

/// Combined trait for transformers that implement both request and response handling.
pub trait RequestResponseTransformer: RequestTransformer + ResponseTransformer {}

// Implement the combined trait for all transformers
impl RequestResponseTransformer for OpenAITransformer {}
impl RequestResponseTransformer for AnthropicTransformer {}
impl RequestResponseTransformer for GoogleTransformer {}
impl RequestResponseTransformer for AzureTransformer {}
impl RequestResponseTransformer for MistralTransformer {}
impl RequestResponseTransformer for BedrockTransformer {}
impl RequestResponseTransformer for OpenAIResponsesTransformer {}

impl ProviderTransformer {
    /// Create a transformer for the given provider type.
    ///
    /// For OpenAI-compatible providers, pass compat settings to control
    /// behavior like developer role, max_tokens field name, and stream_options.
    pub fn for_provider(provider: ProviderType) -> Self {
        Self::for_provider_with_compat(provider, None)
    }

    /// Create a transformer for the given provider type with optional compat settings.
    ///
    /// When `compat` is `Some`, OpenAI-compatible providers use these settings
    /// to control request transformation (developer role, max_tokens field,
    /// stream_options injection, etc.).
    pub fn for_provider_with_compat(
        provider: ProviderType,
        compat: Option<&CompatSettings>,
    ) -> Self {
        let inner: Box<dyn RequestResponseTransformer + Send + Sync> = match provider {
            ProviderType::OpenAI
            | ProviderType::Groq
            | ProviderType::Cerebras
            | ProviderType::XAI
            | ProviderType::OpenRouter
            | ProviderType::OpenAICompatible
            | ProviderType::GithubCopilot // GitHub Copilot uses OpenAI completions API
            | ProviderType::Mock => {
                let transformer = OpenAITransformer::new();
                let transformer = match compat {
                    Some(c) => transformer.with_compat_settings(c),
                    None => transformer,
                };
                Box::new(transformer)
            }
            ProviderType::Anthropic => Box::new(AnthropicTransformer::new()),
            ProviderType::Google
            | ProviderType::GoogleVertex
            | ProviderType::GoogleGeminiCli => Box::new(GoogleTransformer::new()),
            ProviderType::Azure => Box::new(AzureTransformer::new()),
            ProviderType::Mistral => Box::new(MistralTransformer::new()),
            ProviderType::Bedrock => Box::new(BedrockTransformer::new()),
            ProviderType::OpenAICodex | ProviderType::OpenAIResponses => {
                Box::new(OpenAIResponsesTransformer::new())
            }
        };
        Self { inner }
    }

    /// Transform canonical context to provider-specific request body.
    pub fn transform_request(
        &self,
        context: &Context,
    ) -> Result<serde_json::Value, TransformError> {
        self.inner.transform_request(context)
    }

    /// Get required headers for this provider.
    pub fn headers(&self, api_key: &str) -> Vec<(String, String)> {
        self.inner.headers(api_key)
    }

    /// Get the endpoint path for this provider.
    pub fn endpoint_path(&self, context: &Context) -> String {
        self.inner.endpoint_path(context)
    }

    /// Parse a provider-specific SSE chunk into canonical stream events.
    pub fn parse_stream_chunk(
        &self,
        chunk: &str,
        state: &mut StreamState,
    ) -> Result<Vec<StreamEvent>, TransformError> {
        self.inner.parse_stream_chunk(chunk, state)
    }

    /// Parse a complete (non-streaming) response.
    pub fn parse_response(
        &self,
        body: &serde_json::Value,
    ) -> Result<Vec<StreamEvent>, TransformError> {
        self.inner.parse_response(body)
    }
}

/// Parse an incoming request (OpenAI format assumed) to canonical Context.
pub fn parse_incoming_request(body: &serde_json::Value) -> Result<Context, TransformError> {
    openai::parse_openai_request(body)
}

/// Build SSE output in OpenAI format (for responding to OpenAI-format clients).
pub fn build_openai_sse_response(event: &StreamEvent, request_id: &str, model: &str) -> String {
    openai::build_openai_sse(event, request_id, model)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::Message;
    use serde_json::json;

    #[test]
    fn test_transform_error_display() {
        let err = TransformError::MissingField("model".to_string());
        assert_eq!(format!("{}", err), "Missing required field: model");

        let err = TransformError::InvalidJson("unexpected token".to_string());
        assert_eq!(format!("{}", err), "Invalid JSON: unexpected token");
    }

    #[test]
    fn test_provider_transformer_for_openai() {
        let transformer = ProviderTransformer::for_provider(ProviderType::OpenAI);
        let ctx = Context::new("gpt-4").with_messages(vec![Message::user("Hello")]);
        let request = transformer.transform_request(&ctx).unwrap();
        assert_eq!(request["model"], "gpt-4");
    }

    #[test]
    fn test_provider_transformer_for_anthropic() {
        let transformer = ProviderTransformer::for_provider(ProviderType::Anthropic);
        let ctx = Context::new("claude-3-opus")
            .with_messages(vec![Message::user("Hello")])
            .with_max_tokens(1000);
        let request = transformer.transform_request(&ctx).unwrap();
        assert_eq!(request["model"], "claude-3-opus");
        assert_eq!(request["max_tokens"], 1000);
    }

    #[test]
    fn test_provider_transformer_for_google() {
        let transformer = ProviderTransformer::for_provider(ProviderType::Google);
        let ctx = Context::new("gemini-pro").with_messages(vec![Message::user("Hello")]);
        let request = transformer.transform_request(&ctx).unwrap();
        // Google uses "contents" instead of "messages"
        assert!(request["contents"].is_array());
    }

    #[test]
    fn test_parse_incoming_request() {
        let body = json!({
            "model": "gpt-4",
            "messages": [
                {"role": "system", "content": "Be helpful"},
                {"role": "user", "content": "Hello"}
            ],
            "stream": true,
            "max_tokens": 1000
        });

        let ctx = parse_incoming_request(&body).unwrap();
        assert_eq!(ctx.model, "gpt-4");
        assert_eq!(ctx.system_prompt, Some("Be helpful".to_string()));
        assert!(ctx.stream);
        assert_eq!(ctx.max_tokens, Some(1000));
    }

    #[test]
    fn test_cross_provider_translation() {
        // Parse OpenAI format request
        let openai_request = json!({
            "model": "claude-3-opus",
            "messages": [
                {"role": "system", "content": "You are helpful"},
                {"role": "user", "content": "What is 2+2?"}
            ],
            "max_tokens": 100,
            "stream": true
        });

        let ctx = parse_incoming_request(&openai_request).unwrap();

        // Transform to Anthropic format
        let anthropic_transformer = ProviderTransformer::for_provider(ProviderType::Anthropic);
        let anthropic_request = anthropic_transformer.transform_request(&ctx).unwrap();

        // Verify Anthropic format
        assert_eq!(anthropic_request["model"], "claude-3-opus");
        assert_eq!(anthropic_request["max_tokens"], 100);
        // Anthropic uses structured system format with cache_control
        assert!(anthropic_request["system"].is_array());
        let system = anthropic_request["system"].as_array().unwrap();
        assert_eq!(system[0]["text"], "You are helpful");

        assert!(anthropic_request["messages"].is_array());
        let messages = anthropic_request["messages"].as_array().unwrap();
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0]["role"], "user");
    }
}

//! Azure OpenAI transformer.
//!
//! Azure OpenAI uses the same request/response format as OpenAI, but with:
//! - Different authentication (api-key header instead of Bearer token)
//! - Different URL structure (deployment-based endpoints)
//! - Required api-version query parameter

use crate::transform::{RequestTransformer, ResponseTransformer, TransformError};
use crate::types::{Context, StreamEvent, StreamState};
use serde_json::Value;

// Re-use OpenAI parsing/building since format is identical
use super::openai::{build_openai_sse, parse_openai_request, OpenAITransformer};

/// Azure OpenAI transformer.
///
/// Azure uses the same request/response format as OpenAI, but with different
/// authentication and URL patterns.
#[derive(Debug, Clone)]
pub struct AzureTransformer {
    /// The API version to use (e.g., "2024-02-15-preview")
    pub api_version: String,
    /// The deployment name (model deployment in Azure)
    pub deployment: Option<String>,
    /// Inner OpenAI transformer for format handling
    inner: OpenAITransformer,
}

impl Default for AzureTransformer {
    fn default() -> Self {
        Self::new()
    }
}

impl AzureTransformer {
    pub fn new() -> Self {
        Self {
            api_version: "2024-02-15-preview".to_string(),
            deployment: None,
            inner: OpenAITransformer::new(),
        }
    }

    pub fn with_api_version(mut self, version: impl Into<String>) -> Self {
        self.api_version = version.into();
        self
    }

    pub fn with_deployment(mut self, deployment: impl Into<String>) -> Self {
        self.deployment = Some(deployment.into());
        self
    }

    /// Parse an Azure OpenAI request (same format as OpenAI).
    pub fn parse_request(body: &Value) -> Result<Context, TransformError> {
        parse_openai_request(body)
    }

    /// Build Azure SSE response (same format as OpenAI).
    pub fn build_sse(event: &StreamEvent, request_id: &str, model: &str) -> String {
        build_openai_sse(event, request_id, model)
    }
}

impl RequestTransformer for AzureTransformer {
    fn transform_request(&self, context: &Context) -> Result<Value, TransformError> {
        // Use OpenAI format
        self.inner.transform_request(context)
    }

    fn headers(&self, api_key: &str) -> Vec<(String, String)> {
        vec![
            ("Content-Type".to_string(), "application/json".to_string()),
            ("api-key".to_string(), api_key.to_string()),
        ]
    }

    fn endpoint_path(&self, context: &Context) -> String {
        // Azure uses deployment-based URLs:
        // /openai/deployments/{deployment}/chat/completions?api-version=2024-02-15-preview
        let deployment = self
            .deployment.as_deref()
            .unwrap_or(&context.model);

        format!(
            "/openai/deployments/{}/chat/completions?api-version={}",
            deployment, self.api_version
        )
    }
}

impl ResponseTransformer for AzureTransformer {
    fn parse_stream_chunk(
        &self,
        chunk: &str,
        state: &mut StreamState,
    ) -> Result<Vec<StreamEvent>, TransformError> {
        // Azure uses same SSE format as OpenAI
        self.inner.parse_stream_chunk(chunk, state)
    }

    fn parse_response(&self, body: &Value) -> Result<Vec<StreamEvent>, TransformError> {
        // Azure uses same response format as OpenAI
        self.inner.parse_response(body)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::Message;
    use serde_json::json;

    #[test]
    fn test_azure_transformer_new() {
        let transformer = AzureTransformer::new();
        assert_eq!(transformer.api_version, "2024-02-15-preview");
        assert!(transformer.deployment.is_none());
    }

    #[test]
    fn test_azure_transformer_with_config() {
        let transformer = AzureTransformer::new()
            .with_api_version("2024-06-01")
            .with_deployment("gpt-4-deployment");

        assert_eq!(transformer.api_version, "2024-06-01");
        assert_eq!(transformer.deployment, Some("gpt-4-deployment".to_string()));
    }

    #[test]
    fn test_azure_headers() {
        let transformer = AzureTransformer::new();
        let headers = transformer.headers("my-azure-key");

        assert!(headers.iter().any(|(k, v)| k == "api-key" && v == "my-azure-key"));
        assert!(headers.iter().any(|(k, v)| k == "Content-Type" && v == "application/json"));
        // Should NOT have Authorization header
        assert!(!headers.iter().any(|(k, _)| k == "Authorization"));
    }

    #[test]
    fn test_azure_endpoint_path_with_deployment() {
        let transformer = AzureTransformer::new()
            .with_api_version("2024-06-01")
            .with_deployment("my-gpt4");

        let ctx = Context::new("gpt-4");
        let path = transformer.endpoint_path(&ctx);

        assert_eq!(
            path,
            "/openai/deployments/my-gpt4/chat/completions?api-version=2024-06-01"
        );
    }

    #[test]
    fn test_azure_endpoint_path_without_deployment() {
        let transformer = AzureTransformer::new().with_api_version("2024-06-01");

        let ctx = Context::new("gpt-4-turbo");
        let path = transformer.endpoint_path(&ctx);

        // Falls back to model name as deployment
        assert_eq!(
            path,
            "/openai/deployments/gpt-4-turbo/chat/completions?api-version=2024-06-01"
        );
    }

    #[test]
    fn test_azure_parse_request() {
        let body = json!({
            "model": "gpt-4",
            "messages": [
                {"role": "system", "content": "You are helpful"},
                {"role": "user", "content": "Hello"}
            ],
            "stream": true
        });

        let ctx = AzureTransformer::parse_request(&body).unwrap();
        assert_eq!(ctx.model, "gpt-4");
        assert_eq!(ctx.system_prompt, Some("You are helpful".to_string()));
        assert!(ctx.stream);
    }

    #[test]
    fn test_azure_transform_request() {
        let transformer = AzureTransformer::new();
        let ctx = Context::new("gpt-4")
            .with_system("Be helpful")
            .with_messages(vec![Message::user("Hi")])
            .with_max_tokens(1000);

        let request = transformer.transform_request(&ctx).unwrap();

        assert_eq!(request["model"], "gpt-4");
        assert!(request["messages"].is_array());
        assert_eq!(request["max_completion_tokens"], 1000);
    }

    #[test]
    fn test_azure_parse_stream_chunk() {
        let transformer = AzureTransformer::new();
        let mut state = StreamState::default();

        let chunk = r#"data: {"id":"chatcmpl-123","object":"chat.completion.chunk","created":1234567890,"model":"gpt-4","choices":[{"index":0,"delta":{"role":"assistant"},"finish_reason":null}]}

data: {"id":"chatcmpl-123","object":"chat.completion.chunk","created":1234567890,"model":"gpt-4","choices":[{"index":0,"delta":{"content":"Hello"},"finish_reason":null}]}

"#;

        let events = transformer.parse_stream_chunk(chunk, &mut state).unwrap();

        assert!(events.iter().any(|e| matches!(e, StreamEvent::Start { .. })));
        assert!(events.iter().any(|e| matches!(e, StreamEvent::TextDelta { delta, .. } if delta == "Hello")));
    }

    #[test]
    fn test_azure_build_sse() {
        let event = StreamEvent::TextDelta {
            content_index: 0,
            delta: "Hello".to_string(),
        };

        let sse = AzureTransformer::build_sse(&event, "req_123", "gpt-4");
        assert!(sse.starts_with("data: "));
        assert!(sse.contains("\"content\":\"Hello\""));
    }
}

//! Mistral transformer.
//!
//! Mistral uses OpenAI-compatible format but with specific quirks:
//! - Tool IDs must be exactly 9 alphanumeric characters
//! - Tool results require a `name` field
//! - Content cannot be null, must use empty string
//! - Thinking blocks must be converted to `<thinking>` text tags

use crate::transform::{RequestTransformer, ResponseTransformer, TransformError};
use crate::types::{AssistantMessage, ContentBlock, Context, Message, StreamEvent, StreamState};
use serde_json::{json, Value};

// Re-use OpenAI parsing for responses
use super::openai::OpenAITransformer;

/// Mistral transformer with quirk handling.
#[derive(Debug, Clone, Default)]
pub struct MistralTransformer {
    /// Inner OpenAI transformer for base functionality
    inner: OpenAITransformer,
}

impl MistralTransformer {
    pub fn new() -> Self {
        Self {
            inner: OpenAITransformer::new(),
        }
    }

    /// Generate a valid Mistral tool ID (9 alphanumeric characters).
    pub fn generate_tool_id() -> String {
        use std::time::{SystemTime, UNIX_EPOCH};
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        // Use base36 encoding of timestamp, take last 9 chars
        let encoded = format!("{:0>9}", base36_encode(timestamp as u64));
        encoded.chars().take(9).collect()
    }

    /// Validate and fix a tool ID to meet Mistral's requirements.
    pub fn fix_tool_id(id: &str) -> String {
        // Must be exactly 9 alphanumeric characters
        let cleaned: String = id.chars().filter(|c| c.is_alphanumeric()).collect();

        if cleaned.len() >= 9 {
            cleaned.chars().take(9).collect()
        } else {
            // Pad with zeros if too short
            format!("{:0>9}", cleaned)
        }
    }

    /// Transform messages for Mistral compatibility.
    fn transform_messages_for_mistral(context: &Context) -> Vec<Value> {
        let mut messages = Vec::new();

        // Add system prompt
        if let Some(ref system) = context.system_prompt {
            messages.push(json!({
                "role": "system",
                "content": system
            }));
        }

        // Process each message
        for msg in &context.messages {
            match msg {
                Message::User(user) => {
                    let content = build_mistral_content(&user.content);
                    messages.push(json!({
                        "role": "user",
                        "content": content
                    }));
                }
                Message::Assistant(assistant) => {
                    let msg_obj = build_mistral_assistant_message(assistant);
                    messages.push(msg_obj);
                }
                Message::Tool(tool_result) => {
                    // Mistral requires name field in tool results
                    let content_text: String = tool_result
                        .content
                        .iter()
                        .map(|b| match b {
                            ContentBlock::Text(t) => t.text.clone(),
                            _ => String::new(),
                        })
                        .collect();

                    messages.push(json!({
                        "role": "tool",
                        "tool_call_id": Self::fix_tool_id(&tool_result.tool_call_id),
                        "name": tool_result.tool_name,
                        "content": if content_text.is_empty() { " ".to_string() } else { content_text }
                    }));
                }
                Message::System(_) => {
                    // System handled above
                }
            }
        }

        messages
    }
}

impl RequestTransformer for MistralTransformer {
    fn transform_request(&self, context: &Context) -> Result<Value, TransformError> {
        let mut request = json!({
            "model": context.model,
            "stream": context.stream,
        });

        // Build messages with Mistral quirks
        let messages = Self::transform_messages_for_mistral(context);
        request["messages"] = Value::Array(messages);

        // Add optional parameters
        if let Some(max_tokens) = context.max_tokens {
            request["max_tokens"] = json!(max_tokens);
        }

        if let Some(temp) = context.temperature {
            request["temperature"] = json!(temp);
        }

        if let Some(top_p) = context.top_p {
            request["top_p"] = json!(top_p);
        }

        if let Some(ref stop) = context.stop {
            request["stop"] = json!(stop);
        }

        // Add tools
        if let Some(ref tools) = context.tools {
            let mistral_tools: Vec<Value> = tools
                .iter()
                .map(|tool| {
                    json!({
                        "type": "function",
                        "function": {
                            "name": tool.name,
                            "description": tool.description,
                            "parameters": tool.parameters
                        }
                    })
                })
                .collect();
            request["tools"] = Value::Array(mistral_tools);
        }

        Ok(request)
    }

    fn headers(&self, api_key: &str) -> Vec<(String, String)> {
        vec![
            ("Content-Type".to_string(), "application/json".to_string()),
            ("Authorization".to_string(), format!("Bearer {}", api_key)),
        ]
    }

    fn endpoint_path(&self, _context: &Context) -> String {
        "/v1/chat/completions".to_string()
    }
}

impl ResponseTransformer for MistralTransformer {
    fn parse_stream_chunk(
        &self,
        chunk: &str,
        state: &mut StreamState,
    ) -> Result<Vec<StreamEvent>, TransformError> {
        // Mistral uses OpenAI-compatible SSE format
        self.inner.parse_stream_chunk(chunk, state)
    }

    fn parse_response(&self, body: &Value) -> Result<Vec<StreamEvent>, TransformError> {
        // Mistral uses OpenAI-compatible response format
        self.inner.parse_response(body)
    }
}

// ============================================================================
// Internal helpers
// ============================================================================

fn base36_encode(mut n: u64) -> String {
    const CHARS: &[u8] = b"0123456789abcdefghijklmnopqrstuvwxyz";
    if n == 0 {
        return "0".to_string();
    }

    let mut result = Vec::new();
    while n > 0 {
        result.push(CHARS[(n % 36) as usize] as char);
        n /= 36;
    }
    result.reverse();
    result.into_iter().collect()
}

fn build_mistral_content(blocks: &[ContentBlock]) -> Value {
    // Collect text parts, converting thinking to tagged text
    let text_parts: Vec<String> = blocks
        .iter()
        .filter_map(|block| match block {
            ContentBlock::Text(t) => Some(t.text.clone()),
            ContentBlock::Thinking(t) => {
                // Convert thinking to text with tags
                Some(format!("<thinking>\n{}\n</thinking>", t.thinking))
            }
            _ => None,
        })
        .collect();

    let content = text_parts.join("");

    // Mistral can't have null/empty content
    if content.is_empty() {
        Value::String(" ".to_string())
    } else {
        Value::String(content)
    }
}

fn build_mistral_assistant_message(assistant: &AssistantMessage) -> Value {
    let mut msg = json!({"role": "assistant"});

    // Collect text content (including converted thinking)
    let mut text_parts = Vec::new();
    let mut tool_calls = Vec::new();

    for block in &assistant.content {
        match block {
            ContentBlock::Text(t) => text_parts.push(t.text.clone()),
            ContentBlock::Thinking(t) => {
                // Convert thinking to text with tags for Mistral
                text_parts.push(format!("<thinking>\n{}\n</thinking>", t.thinking));
            }
            ContentBlock::ToolCall(tc) => {
                tool_calls.push(json!({
                    "id": MistralTransformer::fix_tool_id(&tc.id),
                    "type": "function",
                    "function": {
                        "name": tc.name,
                        "arguments": serde_json::to_string(&tc.arguments).unwrap_or_default()
                    }
                }));
            }
            _ => {}
        }
    }

    // Set content (can't be null for Mistral)
    let content = text_parts.join("");
    msg["content"] = if content.is_empty() {
        Value::String(" ".to_string())
    } else {
        Value::String(content)
    };

    if !tool_calls.is_empty() {
        msg["tool_calls"] = Value::Array(tool_calls);
    }

    msg
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{TextContent, ThinkingContent, Tool, ToolCall, ToolResultMessage, UserMessage};

    #[test]
    fn test_mistral_transformer_new() {
        let transformer = MistralTransformer::new();
        assert!(transformer.inner.use_developer_role == false);
    }

    #[test]
    fn test_generate_tool_id() {
        let id = MistralTransformer::generate_tool_id();
        assert_eq!(id.len(), 9);
        assert!(id.chars().all(|c| c.is_alphanumeric()));
    }

    #[test]
    fn test_fix_tool_id_too_long() {
        let id = MistralTransformer::fix_tool_id("call_abc123xyz789");
        assert_eq!(id.len(), 9);
        assert_eq!(id, "callabc12"); // Takes first 9 alphanumeric chars
    }

    #[test]
    fn test_fix_tool_id_too_short() {
        let id = MistralTransformer::fix_tool_id("abc");
        assert_eq!(id.len(), 9);
        assert_eq!(id, "000000abc"); // Padded with zeros
    }

    #[test]
    fn test_fix_tool_id_with_special_chars() {
        let id = MistralTransformer::fix_tool_id("call_123-abc");
        assert_eq!(id.len(), 9);
        // Should strip special chars
        assert!(id.chars().all(|c| c.is_alphanumeric()));
    }

    #[test]
    fn test_mistral_headers() {
        let transformer = MistralTransformer::new();
        let headers = transformer.headers("mistral-key");

        assert!(headers
            .iter()
            .any(|(k, v)| k == "Authorization" && v == "Bearer mistral-key"));
    }

    #[test]
    fn test_mistral_endpoint_path() {
        let transformer = MistralTransformer::new();
        let ctx = Context::new("mistral-large");
        assert_eq!(transformer.endpoint_path(&ctx), "/v1/chat/completions");
    }

    #[test]
    fn test_mistral_transform_request_basic() {
        let transformer = MistralTransformer::new();
        let ctx = Context::new("mistral-large")
            .with_system("Be helpful")
            .with_messages(vec![Message::user("Hello")])
            .with_max_tokens(1000);

        let request = transformer.transform_request(&ctx).unwrap();

        assert_eq!(request["model"], "mistral-large");
        assert_eq!(request["max_tokens"], 1000);
        assert!(request["messages"].is_array());

        let messages = request["messages"].as_array().unwrap();
        assert_eq!(messages[0]["role"], "system");
        assert_eq!(messages[0]["content"], "Be helpful");
    }

    #[test]
    fn test_mistral_empty_content_handling() {
        let transformer = MistralTransformer::new();

        // Message with empty content
        let ctx = Context::new("mistral-large").with_messages(vec![Message::User(UserMessage {
            content: vec![],
            timestamp: 0,
        })]);

        let request = transformer.transform_request(&ctx).unwrap();
        let messages = request["messages"].as_array().unwrap();

        // Content should be a space, not empty or null
        assert_eq!(messages[0]["content"], " ");
    }

    #[test]
    fn test_mistral_thinking_conversion() {
        let transformer = MistralTransformer::new();

        let ctx =
            Context::new("mistral-large").with_messages(vec![Message::Assistant(AssistantMessage {
                content: vec![
                    ContentBlock::Thinking(ThinkingContent::new("Let me think...")),
                    ContentBlock::Text(TextContent::new("Here's my answer")),
                ],
                ..Default::default()
            })]);

        let request = transformer.transform_request(&ctx).unwrap();
        let messages = request["messages"].as_array().unwrap();
        let content = messages[0]["content"].as_str().unwrap();

        assert!(content.contains("<thinking>"));
        assert!(content.contains("Let me think..."));
        assert!(content.contains("</thinking>"));
        assert!(content.contains("Here's my answer"));
    }

    #[test]
    fn test_mistral_tool_call_id_fix() {
        let transformer = MistralTransformer::new();

        let ctx =
            Context::new("mistral-large").with_messages(vec![Message::Assistant(AssistantMessage {
                content: vec![ContentBlock::ToolCall(ToolCall::new(
                    "call_very_long_id_12345",
                    "get_weather",
                    json!({"city": "NYC"}),
                ))],
                ..Default::default()
            })]);

        let request = transformer.transform_request(&ctx).unwrap();
        let messages = request["messages"].as_array().unwrap();
        let tool_calls = messages[0]["tool_calls"].as_array().unwrap();

        // ID should be fixed to 9 chars
        let id = tool_calls[0]["id"].as_str().unwrap();
        assert_eq!(id.len(), 9);
    }

    #[test]
    fn test_mistral_tool_result_with_name() {
        let transformer = MistralTransformer::new();

        let ctx = Context::new("mistral-large").with_messages(vec![Message::Tool(
            ToolResultMessage::text("call_123456789", "get_weather", "Sunny, 72F"),
        )]);

        let request = transformer.transform_request(&ctx).unwrap();
        let messages = request["messages"].as_array().unwrap();

        // Should have name field
        assert_eq!(messages[0]["name"], "get_weather");
        assert_eq!(messages[0]["content"], "Sunny, 72F");
        // ID should be fixed
        assert_eq!(messages[0]["tool_call_id"].as_str().unwrap().len(), 9);
    }

    #[test]
    fn test_mistral_tool_result_empty_content() {
        let transformer = MistralTransformer::new();

        let ctx = Context::new("mistral-large").with_messages(vec![Message::Tool(
            ToolResultMessage {
                tool_call_id: "call_123456789".to_string(),
                tool_name: "do_nothing".to_string(),
                content: vec![],
                is_error: false,
                timestamp: 0,
            },
        )]);

        let request = transformer.transform_request(&ctx).unwrap();
        let messages = request["messages"].as_array().unwrap();

        // Empty content should be a space
        assert_eq!(messages[0]["content"], " ");
    }

    #[test]
    fn test_mistral_with_tools() {
        let transformer = MistralTransformer::new();

        let ctx = Context::new("mistral-large")
            .with_messages(vec![Message::user("Get weather")])
            .with_tools(vec![Tool::new(
                "get_weather",
                "Get current weather",
                json!({
                    "type": "object",
                    "properties": {"city": {"type": "string"}}
                }),
            )]);

        let request = transformer.transform_request(&ctx).unwrap();

        assert!(request["tools"].is_array());
        let tools = request["tools"].as_array().unwrap();
        assert_eq!(tools[0]["function"]["name"], "get_weather");
    }

    #[test]
    fn test_base36_encode() {
        assert_eq!(base36_encode(0), "0");
        assert_eq!(base36_encode(35), "z");
        assert_eq!(base36_encode(36), "10");
        assert_eq!(base36_encode(1296), "100");
    }
}

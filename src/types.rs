//! Canonical types for cross-provider message translation.
//!
//! These types form a unified intermediate representation that all providers
//! translate to/from. This allows EAVS to route messages between any provider
//! while preserving semantic meaning.

#![allow(dead_code)]

use serde::{Deserialize, Serialize};

/// API type identifier for tracking message origin.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApiType {
    /// OpenAI /v1/chat/completions
    #[default]
    OpenAICompletions,
    /// OpenAI /v1/responses (newer API)
    OpenAIResponses,
    /// Anthropic /v1/messages
    AnthropicMessages,
    /// Google generateContent
    GoogleGenerativeAI,
}

/// Usage statistics from a response.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Usage {
    /// Tokens used in the prompt
    #[serde(default)]
    pub prompt_tokens: u32,
    /// Tokens generated in the completion
    #[serde(default)]
    pub completion_tokens: u32,
    /// Total tokens used
    #[serde(default)]
    pub total_tokens: u32,
    /// Cache read tokens (Anthropic)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_read_input_tokens: Option<u32>,
    /// Cache creation tokens (Anthropic)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_creation_input_tokens: Option<u32>,
}

/// Reason the model stopped generating.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StopReason {
    /// Natural end of generation
    #[default]
    EndTurn,
    /// Hit a stop sequence
    StopSequence,
    /// Hit max tokens limit
    MaxTokens,
    /// Model wants to use a tool
    ToolUse,
    /// Content was filtered
    ContentFilter,
    /// Unknown or other reason
    #[serde(other)]
    Other,
}

/// Message role in a conversation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MessageRole {
    User,
    Assistant,
    System,
    Tool,
}

/// A user message in the conversation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UserMessage {
    pub content: Vec<ContentBlock>,
    #[serde(default)]
    pub timestamp: i64,
}

impl UserMessage {
    /// Create a simple text user message.
    pub fn text(text: impl Into<String>) -> Self {
        Self {
            content: vec![ContentBlock::Text(TextContent { text: text.into() })],
            timestamp: 0,
        }
    }
}

/// An assistant message in the conversation.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AssistantMessage {
    #[serde(default)]
    pub content: Vec<ContentBlock>,
    /// Which API produced this message
    #[serde(default)]
    pub api: ApiType,
    /// Which provider produced this message
    #[serde(default)]
    pub provider: String,
    /// Which model produced this message
    #[serde(default)]
    pub model: String,
    /// Token usage for this message
    #[serde(default)]
    pub usage: Usage,
    /// Why generation stopped
    #[serde(default)]
    pub stop_reason: StopReason,
    /// Error message if generation failed
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error_message: Option<String>,
    #[serde(default)]
    pub timestamp: i64,
}

impl AssistantMessage {
    /// Create a simple text assistant message.
    pub fn text(text: impl Into<String>) -> Self {
        Self {
            content: vec![ContentBlock::Text(TextContent { text: text.into() })],
            ..Default::default()
        }
    }
}

/// A tool result message in the conversation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolResultMessage {
    pub tool_call_id: String,
    pub tool_name: String,
    pub content: Vec<ContentBlock>,
    #[serde(default)]
    pub is_error: bool,
    #[serde(default)]
    pub timestamp: i64,
}

impl ToolResultMessage {
    /// Create a text tool result.
    pub fn text(
        tool_call_id: impl Into<String>,
        tool_name: impl Into<String>,
        text: impl Into<String>,
    ) -> Self {
        Self {
            tool_call_id: tool_call_id.into(),
            tool_name: tool_name.into(),
            content: vec![ContentBlock::Text(TextContent { text: text.into() })],
            is_error: false,
            timestamp: 0,
        }
    }

    /// Create an error tool result.
    pub fn error(
        tool_call_id: impl Into<String>,
        tool_name: impl Into<String>,
        error: impl Into<String>,
    ) -> Self {
        Self {
            tool_call_id: tool_call_id.into(),
            tool_name: tool_name.into(),
            content: vec![ContentBlock::Text(TextContent { text: error.into() })],
            is_error: true,
            timestamp: 0,
        }
    }
}

/// A system message in the conversation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SystemMessage {
    pub content: String,
    /// Cache control for this message (Anthropic)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_control: Option<CacheControl>,
}

impl SystemMessage {
    pub fn new(content: impl Into<String>) -> Self {
        Self {
            content: content.into(),
            cache_control: None,
        }
    }

    pub fn with_cache(content: impl Into<String>) -> Self {
        Self {
            content: content.into(),
            cache_control: Some(CacheControl::ephemeral()),
        }
    }
}

/// Cache control settings (for Anthropic).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CacheControl {
    #[serde(rename = "type")]
    pub control_type: String,
}

impl CacheControl {
    pub fn ephemeral() -> Self {
        Self {
            control_type: "ephemeral".to_string(),
        }
    }
}

/// A message in the canonical conversation format.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "role", rename_all = "snake_case")]
pub enum Message {
    User(UserMessage),
    Assistant(AssistantMessage),
    Tool(ToolResultMessage),
    System(SystemMessage),
}

impl Message {
    /// Create a user text message.
    pub fn user(text: impl Into<String>) -> Self {
        Self::User(UserMessage::text(text))
    }

    /// Create an assistant text message.
    pub fn assistant(text: impl Into<String>) -> Self {
        Self::Assistant(AssistantMessage::text(text))
    }

    /// Create a system message.
    pub fn system(text: impl Into<String>) -> Self {
        Self::System(SystemMessage::new(text))
    }

    /// Get the role of this message.
    pub fn role(&self) -> MessageRole {
        match self {
            Self::User(_) => MessageRole::User,
            Self::Assistant(_) => MessageRole::Assistant,
            Self::Tool(_) => MessageRole::Tool,
            Self::System(_) => MessageRole::System,
        }
    }
}

/// Content blocks that can appear in messages.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ContentBlock {
    Text(TextContent),
    Thinking(ThinkingContent),
    Image(ImageContent),
    ToolCall(ToolCall),
    ToolResult(ToolResultContent),
}

/// Plain text content.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TextContent {
    pub text: String,
}

impl TextContent {
    pub fn new(text: impl Into<String>) -> Self {
        Self { text: text.into() }
    }
}

/// Thinking/reasoning content (for extended thinking models).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ThinkingContent {
    pub thinking: String,
    /// Provider-specific signature for thinking continuity
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signature: Option<String>,
}

impl ThinkingContent {
    pub fn new(thinking: impl Into<String>) -> Self {
        Self {
            thinking: thinking.into(),
            signature: None,
        }
    }

    pub fn with_signature(thinking: impl Into<String>, signature: impl Into<String>) -> Self {
        Self {
            thinking: thinking.into(),
            signature: Some(signature.into()),
        }
    }
}

/// Image content.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImageContent {
    /// Base64 encoded image data or URL
    pub data: String,
    /// MIME type (e.g., "image/png", "image/jpeg")
    pub mime_type: String,
    /// Whether data is a URL rather than base64
    #[serde(default)]
    pub is_url: bool,
}

impl ImageContent {
    pub fn base64(data: impl Into<String>, mime_type: impl Into<String>) -> Self {
        Self {
            data: data.into(),
            mime_type: mime_type.into(),
            is_url: false,
        }
    }

    pub fn url(url: impl Into<String>) -> Self {
        Self {
            data: url.into(),
            mime_type: String::new(),
            is_url: true,
        }
    }
}

/// A tool call from the assistant.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub arguments: serde_json::Value,
}

impl ToolCall {
    pub fn new(
        id: impl Into<String>,
        name: impl Into<String>,
        arguments: serde_json::Value,
    ) -> Self {
        Self {
            id: id.into(),
            name: name.into(),
            arguments,
        }
    }
}

/// Tool result content (inline in a message).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolResultContent {
    pub tool_call_id: String,
    pub content: String,
    #[serde(default)]
    pub is_error: bool,
}

/// Tool definition.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Tool {
    pub name: String,
    pub description: String,
    /// JSON Schema for parameters
    pub parameters: serde_json::Value,
}

impl Tool {
    pub fn new(
        name: impl Into<String>,
        description: impl Into<String>,
        parameters: serde_json::Value,
    ) -> Self {
        Self {
            name: name.into(),
            description: description.into(),
            parameters,
        }
    }
}

/// The complete context for a request.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Context {
    /// System prompt (extracted from messages or explicit)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub system_prompt: Option<String>,
    /// Conversation messages
    #[serde(default)]
    pub messages: Vec<Message>,
    /// Available tools
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<Tool>>,
    /// Model to use
    #[serde(default)]
    pub model: String,
    /// Maximum tokens to generate
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u32>,
    /// Temperature for sampling
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f32>,
    /// Top-p sampling
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub top_p: Option<f32>,
    /// Stop sequences
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stop: Option<Vec<String>>,
    /// Whether to stream the response
    #[serde(default)]
    pub stream: bool,
    /// Original request for pass-through fields
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub original_request: Option<serde_json::Value>,
}

impl Context {
    pub fn new(model: impl Into<String>) -> Self {
        Self {
            model: model.into(),
            ..Default::default()
        }
    }

    pub fn with_system(mut self, system: impl Into<String>) -> Self {
        self.system_prompt = Some(system.into());
        self
    }

    pub fn with_messages(mut self, messages: Vec<Message>) -> Self {
        self.messages = messages;
        self
    }

    pub fn with_tools(mut self, tools: Vec<Tool>) -> Self {
        self.tools = Some(tools);
        self
    }

    pub fn with_max_tokens(mut self, max_tokens: u32) -> Self {
        self.max_tokens = Some(max_tokens);
        self
    }

    pub fn with_stream(mut self, stream: bool) -> Self {
        self.stream = stream;
        self
    }
}

/// Streaming events from provider responses.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum StreamEvent {
    /// Stream started, includes partial message metadata
    Start {
        #[serde(flatten)]
        partial: AssistantMessage,
    },
    /// Text content started at index
    TextStart { content_index: usize },
    /// Text content delta
    TextDelta { content_index: usize, delta: String },
    /// Text content finished
    TextEnd {
        content_index: usize,
        content: String,
    },
    /// Thinking content started
    ThinkingStart { content_index: usize },
    /// Thinking content delta
    ThinkingDelta { content_index: usize, delta: String },
    /// Thinking content finished
    ThinkingEnd {
        content_index: usize,
        content: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        signature: Option<String>,
    },
    /// Tool call started
    ToolCallStart {
        content_index: usize,
        id: String,
        name: String,
    },
    /// Tool call arguments delta
    ToolCallDelta { content_index: usize, delta: String },
    /// Tool call finished
    ToolCallEnd {
        content_index: usize,
        tool_call: ToolCall,
    },
    /// Usage statistics update
    Usage { usage: Usage },
    /// Stream finished successfully
    Done {
        reason: StopReason,
        message: AssistantMessage,
    },
    /// Stream finished with error
    Error {
        reason: StopReason,
        message: AssistantMessage,
    },
}

/// State for tracking streaming response parsing.
#[derive(Debug, Clone, Default)]
pub struct StreamState {
    /// Current message being built
    pub message: AssistantMessage,
    /// Content blocks being accumulated
    pub content_blocks: Vec<ContentBlockState>,
    /// Whether we've seen the start event
    pub started: bool,
    /// Provider-specific state
    pub provider_state: serde_json::Value,
}

/// State for a content block being streamed.
#[derive(Debug, Clone)]
pub struct ContentBlockState {
    pub block_type: ContentBlockType,
    pub text: String,
    pub tool_id: Option<String>,
    pub tool_name: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContentBlockType {
    Text,
    Thinking,
    ToolCall,
}

impl Default for ContentBlockState {
    fn default() -> Self {
        Self {
            block_type: ContentBlockType::Text,
            text: String::new(),
            tool_id: None,
            tool_name: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_user_message_text() {
        let msg = UserMessage::text("Hello, world!");
        assert_eq!(msg.content.len(), 1);
        match &msg.content[0] {
            ContentBlock::Text(t) => assert_eq!(t.text, "Hello, world!"),
            _ => panic!("Expected text content"),
        }
    }

    #[test]
    fn test_assistant_message_default() {
        let msg = AssistantMessage::default();
        assert!(msg.content.is_empty());
        assert_eq!(msg.api, ApiType::OpenAICompletions);
        assert_eq!(msg.stop_reason, StopReason::EndTurn);
    }

    #[test]
    fn test_message_role() {
        assert_eq!(Message::user("hi").role(), MessageRole::User);
        assert_eq!(Message::assistant("hello").role(), MessageRole::Assistant);
        assert_eq!(Message::system("be helpful").role(), MessageRole::System);
    }

    #[test]
    fn test_tool_result_message() {
        let msg = ToolResultMessage::text("call_123", "get_weather", "Sunny, 72F");
        assert_eq!(msg.tool_call_id, "call_123");
        assert_eq!(msg.tool_name, "get_weather");
        assert!(!msg.is_error);

        let err = ToolResultMessage::error("call_456", "search", "Not found");
        assert!(err.is_error);
    }

    #[test]
    fn test_thinking_content() {
        let t = ThinkingContent::new("Let me think about this...");
        assert!(t.signature.is_none());

        let t2 = ThinkingContent::with_signature("Reasoning...", "sig_abc123");
        assert_eq!(t2.signature, Some("sig_abc123".to_string()));
    }

    #[test]
    fn test_image_content() {
        let base64_img = ImageContent::base64("SGVsbG8=", "image/png");
        assert!(!base64_img.is_url);
        assert_eq!(base64_img.mime_type, "image/png");

        let url_img = ImageContent::url("https://example.com/image.png");
        assert!(url_img.is_url);
    }

    #[test]
    fn test_tool_call() {
        let tc = ToolCall::new(
            "call_123",
            "get_weather",
            serde_json::json!({"city": "NYC"}),
        );
        assert_eq!(tc.id, "call_123");
        assert_eq!(tc.name, "get_weather");
        assert_eq!(tc.arguments["city"], "NYC");
    }

    #[test]
    fn test_context_builder() {
        let ctx = Context::new("gpt-4")
            .with_system("You are helpful")
            .with_messages(vec![Message::user("Hi")])
            .with_max_tokens(1000)
            .with_stream(true);

        assert_eq!(ctx.model, "gpt-4");
        assert_eq!(ctx.system_prompt, Some("You are helpful".to_string()));
        assert_eq!(ctx.messages.len(), 1);
        assert_eq!(ctx.max_tokens, Some(1000));
        assert!(ctx.stream);
    }

    #[test]
    fn test_usage_default() {
        let usage = Usage::default();
        assert_eq!(usage.prompt_tokens, 0);
        assert_eq!(usage.completion_tokens, 0);
        assert!(usage.cache_read_input_tokens.is_none());
    }

    #[test]
    fn test_stop_reason_serialization() {
        let json = serde_json::to_string(&StopReason::ToolUse).unwrap();
        assert_eq!(json, "\"tool_use\"");

        let parsed: StopReason = serde_json::from_str("\"unknown_reason\"").unwrap();
        assert_eq!(parsed, StopReason::Other);
    }

    #[test]
    fn test_message_serialization() {
        let msg = Message::user("Hello");
        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.contains("\"role\":\"user\""));

        let msg = Message::assistant("Hi there");
        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.contains("\"role\":\"assistant\""));
    }

    #[test]
    fn test_content_block_serialization() {
        let text = ContentBlock::Text(TextContent::new("Hello"));
        let json = serde_json::to_string(&text).unwrap();
        assert!(json.contains("\"type\":\"text\""));

        let thinking = ContentBlock::Thinking(ThinkingContent::new("Hmm..."));
        let json = serde_json::to_string(&thinking).unwrap();
        assert!(json.contains("\"type\":\"thinking\""));
    }

    #[test]
    fn test_stream_event_serialization() {
        let event = StreamEvent::TextDelta {
            content_index: 0,
            delta: "Hello".to_string(),
        };
        let json = serde_json::to_string(&event).unwrap();
        assert!(json.contains("\"event\":\"text_delta\""));
        assert!(json.contains("\"delta\":\"Hello\""));
    }

    #[test]
    fn test_system_message_with_cache() {
        let msg = SystemMessage::with_cache("Be helpful");
        assert!(msg.cache_control.is_some());
        assert_eq!(msg.cache_control.unwrap().control_type, "ephemeral");
    }

    #[test]
    fn test_tool_definition() {
        let tool = Tool::new(
            "get_weather",
            "Get current weather",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "city": {"type": "string"}
                },
                "required": ["city"]
            }),
        );
        assert_eq!(tool.name, "get_weather");
        assert!(tool.parameters["properties"]["city"]["type"] == "string");
    }

    #[test]
    fn test_content_block_state_default() {
        let state = ContentBlockState::default();
        assert_eq!(state.block_type, ContentBlockType::Text);
        assert!(state.text.is_empty());
        assert!(state.tool_id.is_none());
    }

    #[test]
    fn test_api_type_default() {
        let api: ApiType = Default::default();
        assert_eq!(api, ApiType::OpenAICompletions);
    }
}

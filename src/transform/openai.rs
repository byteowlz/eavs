//! `OpenAI` format parser and builder.
//!
//! Handles parsing incoming `OpenAI` /v1/chat/completions requests into canonical format
//! and building `OpenAI` SSE responses from canonical stream events.

use crate::transform::{RequestTransformer, ResponseTransformer, TransformError};
use crate::types::{
    ApiType, AssistantMessage, ContentBlock, ContentBlockState, ContentBlockType, Context,
    ImageContent, Message, StopReason, StreamEvent, StreamState, TextContent, Tool, ToolCall,
    ToolResultMessage, Usage, UserMessage,
};
use serde::Deserialize;
use serde_json::{json, Value};

/// `OpenAI` completions format transformer.
#[derive(Debug, Clone, Default)]
pub struct OpenAITransformer {
    /// Whether to use developer role instead of system
    pub use_developer_role: bool,
    /// Whether to use `max_tokens` instead of `max_completion_tokens`
    pub use_max_tokens: bool,
    /// Whether the provider supports the store field
    pub supports_store: bool,
    /// Whether the provider supports `stream_options`: { `include_usage`: true }
    pub supports_stream_options: bool,
}

impl OpenAITransformer {
    pub fn new() -> Self {
        Self {
            supports_stream_options: true,
            ..Self::default()
        }
    }

    /// Configure from resolved `CompatSettings`.
    pub fn with_compat_settings(mut self, compat: &crate::provider::CompatSettings) -> Self {
        self.use_developer_role = compat.supports_developer_role();
        self.use_max_tokens = compat.max_tokens_field() == "max_tokens";
        self.supports_store = compat.supports_store();
        self.supports_stream_options = compat.supports_stream_options();
        self
    }

    /// Configure for specific compat settings (legacy API, kept for tests).
    #[cfg(test)]
    pub const fn with_compat(
        mut self,
        supports_developer_role: bool,
        use_max_tokens: bool,
        supports_store: bool,
    ) -> Self {
        self.use_developer_role = supports_developer_role;
        self.use_max_tokens = use_max_tokens;
        self.supports_store = supports_store;
        self
    }
}

impl RequestTransformer for OpenAITransformer {
    fn transform_request(&self, context: &Context) -> Result<Value, TransformError> {
        let mut request = json!({
            "model": context.model,
            "stream": context.stream,
        });

        // Add messages
        let messages = build_openai_messages(context, self.use_developer_role);
        request["messages"] = Value::Array(messages);

        // Add optional parameters
        if let Some(max_tokens) = context.max_tokens {
            let field = if self.use_max_tokens {
                "max_tokens"
            } else {
                "max_completion_tokens"
            };
            request[field] = json!(max_tokens);
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
            let openai_tools: Vec<Value> = tools.iter().map(tool_to_openai).collect();
            request["tools"] = Value::Array(openai_tools);
        }

        // Add stream options for usage reporting (if provider supports it)
        if context.stream && self.supports_stream_options {
            request["stream_options"] = json!({"include_usage": true});
        }

        Ok(request)
    }

    #[cfg(test)]
    fn headers(&self, api_key: &str) -> Vec<(String, String)> {
        let mut headers = vec![("Content-Type".to_string(), "application/json".to_string())];
        if !api_key.is_empty() {
            headers.push(("Authorization".to_string(), format!("Bearer {api_key}")));
        }
        headers
    }

    fn endpoint_path(&self, _context: &Context) -> String {
        "/v1/chat/completions".to_string()
    }
}

impl ResponseTransformer for OpenAITransformer {
    fn parse_stream_chunk(
        &self,
        chunk: &str,
        state: &mut StreamState,
    ) -> Result<Vec<StreamEvent>, TransformError> {
        let mut events = Vec::new();

        for line in chunk.lines() {
            let line = line.trim();

            // Skip empty lines and comments
            if line.is_empty() || line.starts_with(':') {
                continue;
            }

            // Parse SSE data line
            if let Some(data) = line.strip_prefix("data: ") {
                let data = data.trim();

                // Check for stream end
                if data == "[DONE]" {
                    let stop_reason = state.message.stop_reason.clone();
                    events.push(StreamEvent::Done {
                        reason: stop_reason,
                        message: state.message.clone(),
                    });
                    continue;
                }

                // Parse JSON
                let parsed: OpenAIStreamChunk = serde_json::from_str(data)
                    .map_err(|e| TransformError::InvalidJson(e.to_string()))?;

                events.extend(process_openai_chunk(parsed, state));
            }
        }

        Ok(events)
    }

    fn parse_response(&self, body: &Value) -> Result<Vec<StreamEvent>, TransformError> {
        let response: OpenAIResponse = serde_json::from_value(body.clone())
            .map_err(|e| TransformError::InvalidJson(e.to_string()))?;

        let mut events = Vec::new();

        // Get the first choice
        let choice = response
            .choices
            .first()
            .ok_or_else(|| TransformError::InvalidJson("No choices in response".to_string()))?;

        // Build message
        let mut message = AssistantMessage {
            api: ApiType::OpenAICompletions,
            model: response.model,
            usage: Usage {
                prompt_tokens: response.usage.prompt_tokens,
                completion_tokens: response.usage.completion_tokens,
                total_tokens: response.usage.total_tokens,
                ..Default::default()
            },
            stop_reason: parse_finish_reason(choice.finish_reason.as_deref()),
            ..Default::default()
        };

        // Parse content
        if let Some(ref content) = choice.message.content {
            message
                .content
                .push(ContentBlock::Text(TextContent::new(content)));
            events.push(StreamEvent::TextEnd {
                content_index: 0,
                content: content.clone(),
            });
        }

        // Parse tool calls
        if let Some(ref tool_calls) = choice.message.tool_calls {
            for (idx, tc) in tool_calls.iter().enumerate() {
                let tool_call = ToolCall::new(
                    &tc.id,
                    &tc.function.name,
                    serde_json::from_str(&tc.function.arguments).unwrap_or(Value::Null),
                );
                message
                    .content
                    .push(ContentBlock::ToolCall(tool_call.clone()));
                events.push(StreamEvent::ToolCallEnd {
                    content_index: idx + 1,
                    tool_call,
                });
            }
        }

        events.push(StreamEvent::Done {
            reason: message.stop_reason.clone(),
            message,
        });

        Ok(events)
    }
}

/// Parse an `OpenAI` request body into canonical Context.
pub fn parse_openai_request(body: &Value) -> Result<Context, TransformError> {
    let model = body["model"]
        .as_str()
        .ok_or_else(|| TransformError::MissingField("model".to_string()))?
        .to_string();

    let mut context = Context::new(model);

    // Parse messages
    let messages = body["messages"]
        .as_array()
        .ok_or_else(|| TransformError::MissingField("messages".to_string()))?;

    for msg in messages {
        let role = msg["role"]
            .as_str()
            .ok_or_else(|| TransformError::MissingField("role".to_string()))?;

        match role {
            "system" | "developer" => {
                let system_content = msg["content"].as_str().unwrap_or_default().to_string();
                context.system_prompt = Some(system_content);
            }
            "user" => {
                let user_msg = parse_user_message(msg);
                context.messages.push(Message::User(user_msg));
            }
            "assistant" => {
                let assistant_msg = parse_assistant_message(msg);
                context.messages.push(Message::Assistant(assistant_msg));
            }
            "tool" => {
                let tool_msg = parse_tool_message(msg)?;
                context.messages.push(Message::Tool(tool_msg));
            }
            _ => {
                // Ignore unknown roles
            }
        }
    }

    // Parse optional parameters
    context.stream = body["stream"].as_bool().unwrap_or(false);
    context.max_tokens = body["max_tokens"]
        .as_u64()
        .or_else(|| body["max_completion_tokens"].as_u64())
        .map(|v| v as u32);
    context.temperature = body["temperature"].as_f64().map(|v| v as f32);
    context.top_p = body["top_p"].as_f64().map(|v| v as f32);

    if let Some(stop) = body["stop"].as_array() {
        context.stop = Some(
            stop.iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect(),
        );
    } else if let Some(stop) = body["stop"].as_str() {
        context.stop = Some(vec![stop.to_string()]);
    }

    // Parse tools
    if let Some(tools) = body["tools"].as_array() {
        let parsed_tools: Result<Vec<Tool>, _> = tools.iter().map(parse_tool).collect();
        context.tools = Some(parsed_tools?);
    }

    // Store original request for pass-through
    context.original_request = Some(body.clone());

    Ok(context)
}

/// Build `OpenAI` SSE response from canonical stream events.
pub fn build_openai_sse(event: &StreamEvent, request_id: &str, model: &str) -> String {
    match event {
        StreamEvent::Start { .. } => openai_sse_start(request_id, model),
        StreamEvent::TextDelta { delta, .. } | StreamEvent::ThinkingDelta { delta, .. } => {
            openai_sse_text_delta(delta, request_id, model)
        }
        StreamEvent::ToolCallStart {
            id,
            name,
            content_index,
        } => openai_sse_tool_call_start(id, name, *content_index, request_id, model),
        StreamEvent::ToolCallDelta {
            delta,
            content_index,
        } => openai_sse_tool_call_delta(delta, *content_index, request_id, model),
        StreamEvent::Usage { usage } => openai_sse_usage(usage, request_id, model),
        StreamEvent::Done { reason, .. } => openai_sse_done(reason, request_id, model),
        StreamEvent::Error { message, .. } => openai_sse_error(message, request_id, model),
        // Handle remaining events with empty deltas
        _ => String::new(),
    }
}

/// Encode a JSON chunk as an SSE `data:` message.
fn openai_sse_frame(chunk: &Value) -> String {
    format!("data: {chunk}\n\n")
}

fn openai_sse_start(request_id: &str, model: &str) -> String {
    let chunk = json!({
        "id": request_id,
        "object": "chat.completion.chunk",
        "created": chrono::Utc::now().timestamp(),
        "model": model,
        "choices": [{
            "index": 0,
            "delta": {"role": "assistant"},
            "finish_reason": null
        }]
    });
    openai_sse_frame(&chunk)
}

fn openai_sse_text_delta(delta: &str, request_id: &str, model: &str) -> String {
    let chunk = json!({
        "id": request_id,
        "object": "chat.completion.chunk",
        "created": chrono::Utc::now().timestamp(),
        "model": model,
        "choices": [{
            "index": 0,
            "delta": {"content": delta},
            "finish_reason": null
        }]
    });
    openai_sse_frame(&chunk)
}

fn openai_sse_tool_call_start(
    id: &str,
    name: &str,
    content_index: usize,
    request_id: &str,
    model: &str,
) -> String {
    let chunk = json!({
        "id": request_id,
        "object": "chat.completion.chunk",
        "created": chrono::Utc::now().timestamp(),
        "model": model,
        "choices": [{
            "index": 0,
            "delta": {
                "tool_calls": [{
                    "index": content_index,
                    "id": id,
                    "type": "function",
                    "function": {"name": name, "arguments": ""}
                }]
            },
            "finish_reason": null
        }]
    });
    openai_sse_frame(&chunk)
}

fn openai_sse_tool_call_delta(
    delta: &str,
    content_index: usize,
    request_id: &str,
    model: &str,
) -> String {
    let chunk = json!({
        "id": request_id,
        "object": "chat.completion.chunk",
        "created": chrono::Utc::now().timestamp(),
        "model": model,
        "choices": [{
            "index": 0,
            "delta": {
                "tool_calls": [{
                    "index": content_index,
                    "function": {"arguments": delta}
                }]
            },
            "finish_reason": null
        }]
    });
    openai_sse_frame(&chunk)
}

fn openai_sse_usage(usage: &Usage, request_id: &str, model: &str) -> String {
    let chunk = json!({
        "id": request_id,
        "object": "chat.completion.chunk",
        "created": chrono::Utc::now().timestamp(),
        "model": model,
        "choices": [],
        "usage": {
            "prompt_tokens": usage.prompt_tokens,
            "completion_tokens": usage.completion_tokens,
            "total_tokens": usage.total_tokens
        }
    });
    openai_sse_frame(&chunk)
}

fn openai_sse_done(reason: &StopReason, request_id: &str, model: &str) -> String {
    let finish_reason = match reason {
        StopReason::EndTurn | StopReason::StopSequence | StopReason::Other => "stop",
        StopReason::MaxTokens => "length",
        StopReason::ToolUse => "tool_calls",
        StopReason::ContentFilter => "content_filter",
    };
    let chunk = json!({
        "id": request_id,
        "object": "chat.completion.chunk",
        "created": chrono::Utc::now().timestamp(),
        "model": model,
        "choices": [{
            "index": 0,
            "delta": {},
            "finish_reason": finish_reason
        }]
    });
    openai_sse_frame(&chunk) + "data: [DONE]\n\n"
}

fn openai_sse_error(message: &AssistantMessage, _request_id: &str, _model: &str) -> String {
    let error_msg = message.error_message.as_deref().unwrap_or("Unknown error");
    let chunk = json!({
        "error": {
            "message": error_msg,
            "type": "server_error"
        }
    });
    openai_sse_frame(&chunk)
}

// ============================================================================
// Internal helpers
// ============================================================================

fn parse_user_message(msg: &Value) -> UserMessage {
    let mut content = Vec::new();

    match &msg["content"] {
        Value::String(s) => {
            content.push(ContentBlock::Text(TextContent::new(s)));
        }
        Value::Array(parts) => {
            for part in parts {
                let part_type = part["type"].as_str().unwrap_or("text");
                match part_type {
                    "text" => {
                        let text = part["text"].as_str().unwrap_or_default();
                        content.push(ContentBlock::Text(TextContent::new(text)));
                    }
                    "image_url" => {
                        let url = part["image_url"]["url"].as_str().unwrap_or_default();
                        if url.starts_with("data:") {
                            // Parse data URL
                            if let Some((mime, data)) = parse_data_url(url) {
                                content.push(ContentBlock::Image(ImageContent::base64(data, mime)));
                            }
                        } else {
                            content.push(ContentBlock::Image(ImageContent::url(url)));
                        }
                    }
                    _ => {}
                }
            }
        }
        _ => {}
    }

    UserMessage {
        content,
        timestamp: 0,
    }
}

fn parse_assistant_message(msg: &Value) -> AssistantMessage {
    let mut content = Vec::new();

    // Parse text content
    if let Some(text) = msg["content"].as_str() {
        content.push(ContentBlock::Text(TextContent::new(text)));
    }

    // Parse tool calls
    if let Some(tool_calls) = msg["tool_calls"].as_array() {
        for tc in tool_calls {
            let id = tc["id"].as_str().unwrap_or_default().to_string();
            let name = tc["function"]["name"]
                .as_str()
                .unwrap_or_default()
                .to_string();
            let arguments: Value = tc["function"]["arguments"]
                .as_str()
                .and_then(|s| serde_json::from_str(s).ok())
                .unwrap_or(Value::Null);

            content.push(ContentBlock::ToolCall(ToolCall::new(id, name, arguments)));
        }
    }

    AssistantMessage {
        content,
        api: ApiType::OpenAICompletions,
        ..Default::default()
    }
}

fn parse_tool_message(msg: &Value) -> Result<ToolResultMessage, TransformError> {
    let tool_call_id = msg["tool_call_id"]
        .as_str()
        .ok_or_else(|| TransformError::MissingField("tool_call_id".to_string()))?
        .to_string();

    let content_text = msg["content"].as_str().unwrap_or_default().to_string();

    Ok(ToolResultMessage {
        tool_call_id,
        tool_name: String::new(), // OpenAI doesn't require tool name in result
        content: vec![ContentBlock::Text(TextContent::new(content_text))],
        is_error: false,
        timestamp: 0,
    })
}

fn parse_tool(tool: &Value) -> Result<Tool, TransformError> {
    let function = &tool["function"];
    let name = function["name"]
        .as_str()
        .ok_or_else(|| TransformError::MissingField("function.name".to_string()))?
        .to_string();
    let description = function["description"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    let parameters = function["parameters"].clone();

    Ok(Tool::new(name, description, parameters))
}

fn tool_to_openai(tool: &Tool) -> Value {
    json!({
        "type": "function",
        "function": {
            "name": tool.name,
            "description": tool.description,
            "parameters": tool.parameters
        }
    })
}

fn build_openai_messages(context: &Context, use_developer_role: bool) -> Vec<Value> {
    let mut messages = Vec::new();

    // Add system prompt
    if let Some(ref system) = context.system_prompt {
        let role = if use_developer_role {
            "developer"
        } else {
            "system"
        };
        messages.push(json!({
            "role": role,
            "content": system
        }));
    }

    // Add conversation messages
    for msg in &context.messages {
        match msg {
            Message::User(user) => {
                let user_content = build_openai_content(&user.content);
                messages.push(json!({
                    "role": "user",
                    "content": user_content
                }));
            }
            Message::Assistant(assistant) => {
                let mut msg_obj = json!({
                    "role": "assistant"
                });

                // Extract text and tool calls
                let mut text_parts = Vec::new();
                let mut tool_calls = Vec::new();

                for block in &assistant.content {
                    match block {
                        ContentBlock::Text(t) => text_parts.push(t.text.clone()),
                        ContentBlock::Thinking(t) => {
                            // Convert thinking to text with tags for OpenAI
                            text_parts.push(format!("<thinking>\n{}\n</thinking>", t.thinking));
                        }
                        ContentBlock::ToolCall(tc) => {
                            tool_calls.push(json!({
                                "id": tc.id,
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

                if !text_parts.is_empty() {
                    msg_obj["content"] = Value::String(text_parts.join(""));
                }
                if !tool_calls.is_empty() {
                    msg_obj["tool_calls"] = Value::Array(tool_calls);
                }

                messages.push(msg_obj);
            }
            Message::Tool(tool_result) => {
                messages.push(json!({
                    "role": "tool",
                    "tool_call_id": tool_result.tool_call_id,
                    "content": tool_result.content.iter().map(|b| match b {
                        ContentBlock::Text(t) => t.text.clone(),
                        _ => String::new()
                    }).collect::<String>()
                }));
            }
            Message::System(_) => {
                // System messages are handled separately
            }
        }
    }

    messages
}

fn build_openai_content(blocks: &[ContentBlock]) -> Value {
    if blocks.len() == 1 {
        if let ContentBlock::Text(t) = &blocks[0] {
            return Value::String(t.text.clone());
        }
    }

    let parts: Vec<Value> = blocks
        .iter()
        .filter_map(|block| match block {
            ContentBlock::Text(t) => Some(json!({
                "type": "text",
                "text": t.text
            })),
            ContentBlock::Image(img) => {
                if img.is_url {
                    Some(json!({
                        "type": "image_url",
                        "image_url": {"url": img.data}
                    }))
                } else {
                    Some(json!({
                        "type": "image_url",
                        "image_url": {
                            "url": format!("data:{};base64,{}", img.mime_type, img.data)
                        }
                    }))
                }
            }
            _ => None,
        })
        .collect();

    Value::Array(parts)
}

fn parse_data_url(url: &str) -> Option<(String, String)> {
    let without_prefix = url.strip_prefix("data:")?;
    let (meta, data) = without_prefix.split_once(',')?;
    let mime = meta.strip_suffix(";base64").unwrap_or(meta);
    Some((mime.to_string(), data.to_string()))
}

fn parse_finish_reason(reason: Option<&str>) -> StopReason {
    match reason {
        Some("stop") => StopReason::EndTurn,
        Some("length") => StopReason::MaxTokens,
        Some("tool_calls") => StopReason::ToolUse,
        Some("content_filter") => StopReason::ContentFilter,
        _ => StopReason::Other,
    }
}

// ============================================================================
// OpenAI streaming types
// ============================================================================

#[derive(Debug, Deserialize)]
struct OpenAIStreamChunk {
    #[expect(dead_code, reason = "Deserialized for the upstream wire schema")]
    id: String,
    #[expect(dead_code, reason = "Deserialized for the upstream wire schema")]
    object: String,
    #[expect(dead_code, reason = "Deserialized for the upstream wire schema")]
    created: i64,
    model: String,
    choices: Vec<OpenAIStreamChoice>,
    #[serde(default)]
    usage: Option<OpenAIUsage>,
}

#[derive(Debug, Deserialize)]
struct OpenAIStreamChoice {
    #[expect(dead_code, reason = "Deserialized for the upstream wire schema")]
    index: usize,
    delta: OpenAIDelta,
    finish_reason: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
struct OpenAIDelta {
    #[serde(default)]
    #[expect(dead_code, reason = "Deserialized for the upstream wire schema")]
    role: Option<String>,
    #[serde(default)]
    content: Option<String>,
    #[serde(default)]
    tool_calls: Option<Vec<OpenAIToolCallDelta>>,
}

#[derive(Debug, Deserialize)]
struct OpenAIToolCallDelta {
    index: usize,
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    function: Option<OpenAIFunctionDelta>,
}

#[derive(Debug, Default, Deserialize)]
struct OpenAIFunctionDelta {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    arguments: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
#[expect(
    clippy::struct_field_names,
    reason = "Field names mirror the OpenAI token-count schema in the wire format"
)]
struct OpenAIUsage {
    #[serde(default)]
    prompt_tokens: u32,
    #[serde(default)]
    completion_tokens: u32,
    #[serde(default)]
    total_tokens: u32,
}

#[derive(Debug, Deserialize)]
struct OpenAIResponse {
    #[expect(dead_code, reason = "Deserialized for the upstream wire schema")]
    id: String,
    model: String,
    choices: Vec<OpenAIChoice>,
    usage: OpenAIUsage,
}

#[derive(Debug, Deserialize)]
struct OpenAIChoice {
    #[expect(dead_code, reason = "Deserialized for the upstream wire schema")]
    index: usize,
    message: OpenAIMessage,
    finish_reason: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
struct OpenAIMessage {
    #[serde(default)]
    #[expect(dead_code, reason = "Deserialized for the upstream wire schema")]
    role: Option<String>,
    #[serde(default)]
    content: Option<String>,
    #[serde(default)]
    tool_calls: Option<Vec<OpenAIToolCall>>,
}

#[derive(Debug, Deserialize)]
struct OpenAIToolCall {
    id: String,
    #[serde(rename = "type")]
    #[expect(dead_code, reason = "Deserialized for the upstream wire schema")]
    call_type: String,
    function: OpenAIFunction,
}

#[derive(Debug, Deserialize)]
struct OpenAIFunction {
    name: String,
    arguments: String,
}

fn process_openai_chunk(chunk: OpenAIStreamChunk, state: &mut StreamState) -> Vec<StreamEvent> {
    let mut events = Vec::new();

    // Handle start
    if !state.started {
        state.started = true;
        state.message.model.clone_from(&chunk.model);
        state.message.api = ApiType::OpenAICompletions;
        events.push(StreamEvent::Start {
            partial: state.message.clone(),
        });
    }

    // Process choices
    for choice in chunk.choices {
        handle_openai_choice(choice, state, &mut events);
    }

    // Handle usage
    if let Some(usage) = chunk.usage {
        state.message.usage = Usage {
            prompt_tokens: usage.prompt_tokens,
            completion_tokens: usage.completion_tokens,
            total_tokens: usage.total_tokens,
            ..Default::default()
        };
        events.push(StreamEvent::Usage {
            usage: state.message.usage.clone(),
        });
    }

    events
}

/// Process a single `OpenAI` chunk choice, updating state and emitting events.
fn handle_openai_choice(
    choice: OpenAIStreamChoice,
    state: &mut StreamState,
    events: &mut Vec<StreamEvent>,
) {
    // Handle content
    if let Some(content) = choice.delta.content {
        // Ensure we have a text content block
        if state.content_blocks.is_empty() {
            state.content_blocks.push(ContentBlockState {
                block_type: ContentBlockType::Text,
                text: String::new(),
                tool_id: None,
                tool_name: None,
            });
            events.push(StreamEvent::TextStart { content_index: 0 });
        }

        // Append to current text block
        if let Some(block) = state.content_blocks.first_mut() {
            block.text.push_str(&content);
        }

        events.push(StreamEvent::TextDelta {
            content_index: 0,
            delta: content,
        });
    }

    // Handle tool calls
    if let Some(tool_calls) = choice.delta.tool_calls {
        for tc_delta in tool_calls {
            handle_openai_tool_call_delta(tc_delta, state, events);
        }
    }

    // Handle finish reason
    if let Some(ref reason) = choice.finish_reason {
        state.message.stop_reason = parse_finish_reason(Some(reason.as_str()));
        finalize_openai_content_blocks(state, events);
    }
}

/// Process a single tool-call delta, updating the matching content block.
fn handle_openai_tool_call_delta(
    tc_delta: OpenAIToolCallDelta,
    state: &mut StreamState,
    events: &mut Vec<StreamEvent>,
) {
    let idx = tc_delta.index;

    // Ensure we have enough content blocks
    while state.content_blocks.len() <= idx {
        state.content_blocks.push(ContentBlockState::default());
    }

    let block = &mut state.content_blocks[idx];

    // Handle new tool call
    if let Some(id) = tc_delta.id {
        block.block_type = ContentBlockType::ToolCall;
        block.tool_id = Some(id.clone());
        if let Some(ref func) = tc_delta.function {
            if let Some(ref name) = func.name {
                block.tool_name = Some(name.clone());
                events.push(StreamEvent::ToolCallStart {
                    content_index: idx,
                    id,
                    name: name.clone(),
                });
            }
        }
    }

    // Handle arguments delta
    if let Some(ref func) = tc_delta.function {
        if let Some(ref args) = func.arguments {
            block.text.push_str(args);
            events.push(StreamEvent::ToolCallDelta {
                content_index: idx,
                delta: args.clone(),
            });
        }
    }
}

/// Finalize accumulated content blocks into the message and emit end events.
fn finalize_openai_content_blocks(state: &mut StreamState, events: &mut Vec<StreamEvent>) {
    for (idx, block) in state.content_blocks.iter().enumerate() {
        match block.block_type {
            ContentBlockType::Text => {
                let text_content = TextContent::new(&block.text);
                state.message.content.push(ContentBlock::Text(text_content));
                events.push(StreamEvent::TextEnd {
                    content_index: idx,
                    content: block.text.clone(),
                });
            }
            ContentBlockType::ToolCall => {
                let tool_call = ToolCall::new(
                    block.tool_id.as_deref().unwrap_or_default(),
                    block.tool_name.as_deref().unwrap_or_default(),
                    serde_json::from_str(&block.text).unwrap_or(Value::Null),
                );
                state
                    .message
                    .content
                    .push(ContentBlock::ToolCall(tool_call.clone()));
                events.push(StreamEvent::ToolCallEnd {
                    content_index: idx,
                    tool_call,
                });
            }
            ContentBlockType::Thinking => {
                // OpenAI doesn't have thinking blocks
            }
        }
    }
}

/// Conform a pass-through `OpenAI` chat-completions body to the upstream's quirks.
///
/// Requests to OpenAI-compatible providers skip the canonical transform, so the
/// `CompatSettings` wired into [`OpenAITransformer`] never sees them. Only settings that
/// were explicitly configured or URL-detected are applied; unset fields are left alone so
/// pass-through stays byte-faithful where the upstream's behaviour is unknown.
pub fn apply_compat_to_passthrough_body(
    body: &mut Value,
    compat: &crate::provider::CompatSettings,
) {
    if compat.supports_developer_role == Some(false) {
        if let Some(messages) = body.get_mut("messages").and_then(Value::as_array_mut) {
            for message in messages.iter_mut() {
                if message.get("role").and_then(Value::as_str) == Some("developer") {
                    message["role"] = json!("system");
                }
            }
        }
    }

    if compat.supports_store == Some(false) {
        if let Some(obj) = body.as_object_mut() {
            obj.remove("store");
        }
    }

    if compat.supports_stream_options == Some(false) {
        if let Some(obj) = body.as_object_mut() {
            obj.remove("stream_options");
        }
    }

    if let Some(field) = compat.max_tokens_field.as_deref() {
        let stale = match field {
            "max_tokens" => "max_completion_tokens",
            "max_completion_tokens" => "max_tokens",
            _ => return,
        };
        if let Some(obj) = body.as_object_mut() {
            if let Some(value) = obj.remove(stale) {
                obj.entry(field).or_insert(value);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::CompatSettings;
    use crate::types::ThinkingContent;

    fn passthrough_body() -> Value {
        json!({
            "model": "Kimi-K2.6",
            "messages": [
                {"role": "developer", "content": "be terse"},
                {"role": "user", "content": "hi"}
            ],
            "store": true,
            "stream_options": {"include_usage": true},
            "max_completion_tokens": 20
        })
    }

    #[test]
    fn passthrough_compat_downgrades_developer_role() {
        let mut body = passthrough_body();
        let compat = CompatSettings {
            supports_developer_role: Some(false),
            ..Default::default()
        };

        apply_compat_to_passthrough_body(&mut body, &compat);

        assert_eq!(body["messages"][0]["role"], "system");
        assert_eq!(body["messages"][1]["role"], "user");
        // Untouched settings stay byte-faithful.
        assert_eq!(body["store"], true);
        assert!(body.get("stream_options").is_some());
        assert_eq!(body["max_completion_tokens"], 20);
    }

    #[test]
    fn passthrough_compat_strips_unsupported_fields_and_renames_max_tokens() {
        let mut body = passthrough_body();
        let compat = CompatSettings {
            supports_store: Some(false),
            supports_stream_options: Some(false),
            max_tokens_field: Some("max_tokens".to_string()),
            ..Default::default()
        };

        apply_compat_to_passthrough_body(&mut body, &compat);

        assert!(body.get("store").is_none());
        assert!(body.get("stream_options").is_none());
        assert!(body.get("max_completion_tokens").is_none());
        assert_eq!(body["max_tokens"], 20);
    }

    #[test]
    fn passthrough_compat_leaves_body_alone_when_unset() {
        let mut body = passthrough_body();
        let original = body.clone();

        apply_compat_to_passthrough_body(&mut body, &CompatSettings::default());

        assert_eq!(body, original);
    }

    #[expect(
        clippy::panic_in_result_fn,
        reason = "Test assertions intentionally fail the test while fallible setup propagates errors"
    )]
    #[test]
    fn test_parse_openai_request_simple() -> Result<(), Box<dyn std::error::Error>> {
        let body = json!({
            "model": "gpt-4",
            "messages": [
                {"role": "system", "content": "You are helpful"},
                {"role": "user", "content": "Hello"}
            ],
            "stream": true
        });

        let ctx = parse_openai_request(&body)?;
        assert_eq!(ctx.model, "gpt-4");
        assert_eq!(ctx.system_prompt, Some("You are helpful".to_string()));
        assert_eq!(ctx.messages.len(), 1);
        assert!(ctx.stream);
        Ok(())
    }

    #[expect(
        clippy::panic_in_result_fn,
        reason = "Test assertions intentionally fail the test while fallible setup propagates errors"
    )]
    #[test]
    fn test_parse_openai_request_with_tools() -> Result<(), Box<dyn std::error::Error>> {
        let body = json!({
            "model": "gpt-4",
            "messages": [{"role": "user", "content": "Get weather"}],
            "tools": [{
                "type": "function",
                "function": {
                    "name": "get_weather",
                    "description": "Get current weather",
                    "parameters": {
                        "type": "object",
                        "properties": {"city": {"type": "string"}}
                    }
                }
            }]
        });

        let ctx = parse_openai_request(&body)?;
        assert!(ctx.tools.is_some());
        let tools = ctx.tools.as_ref().ok_or("tools missing")?;
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].name, "get_weather");
        Ok(())
    }

    #[expect(
        clippy::panic_in_result_fn,
        reason = "Test assertions intentionally fail the test while fallible setup propagates errors"
    )]
    #[test]
    fn test_parse_openai_request_with_image() -> Result<(), Box<dyn std::error::Error>> {
        let body = json!({
            "model": "gpt-4-vision",
            "messages": [{
                "role": "user",
                "content": [
                    {"type": "text", "text": "What's in this image?"},
                    {"type": "image_url", "image_url": {"url": "https://example.com/img.png"}}
                ]
            }]
        });

        let ctx = parse_openai_request(&body)?;
        assert_eq!(ctx.messages.len(), 1);

        let Message::User(user) = &ctx.messages[0] else {
            return Err("Expected user message".into());
        };
        assert_eq!(user.content.len(), 2);
        assert!(matches!(&user.content[1], ContentBlock::Image(_)));
        Ok(())
    }

    #[expect(
        clippy::panic_in_result_fn,
        reason = "Test assertions intentionally fail the test while fallible setup propagates errors"
    )]
    #[test]
    fn test_parse_openai_request_with_tool_call() -> Result<(), Box<dyn std::error::Error>> {
        let body = json!({
            "model": "gpt-4",
            "messages": [
                {"role": "user", "content": "Get weather"},
                {
                    "role": "assistant",
                    "tool_calls": [{
                        "id": "call_123",
                        "type": "function",
                        "function": {
                            "name": "get_weather",
                            "arguments": "{\"city\":\"NYC\"}"
                        }
                    }]
                },
                {
                    "role": "tool",
                    "tool_call_id": "call_123",
                    "content": "Sunny, 72F"
                }
            ]
        });

        let ctx = parse_openai_request(&body)?;
        assert_eq!(ctx.messages.len(), 3);

        // Check assistant message with tool call
        let Message::Assistant(assistant) = &ctx.messages[1] else {
            return Err("Expected assistant message".into());
        };
        assert_eq!(assistant.content.len(), 1);
        let ContentBlock::ToolCall(tc) = &assistant.content[0] else {
            return Err("Expected tool call".into());
        };
        assert_eq!(tc.id, "call_123");
        assert_eq!(tc.name, "get_weather");

        // Check tool result
        let Message::Tool(tool) = &ctx.messages[2] else {
            return Err("Expected tool message".into());
        };
        assert_eq!(tool.tool_call_id, "call_123");
        Ok(())
    }

    #[test]
    fn test_build_openai_sse_text_delta() {
        let event = StreamEvent::TextDelta {
            content_index: 0,
            delta: "Hello".to_string(),
        };

        let sse = build_openai_sse(&event, "req_123", "gpt-4");
        assert!(sse.starts_with("data: "));
        assert!(sse.contains("\"content\":\"Hello\""));
    }

    #[test]
    fn test_build_openai_sse_done() {
        let event = StreamEvent::Done {
            reason: StopReason::EndTurn,
            message: AssistantMessage::default(),
        };

        let sse = build_openai_sse(&event, "req_123", "gpt-4");
        assert!(sse.contains("\"finish_reason\":\"stop\""));
        assert!(sse.contains("[DONE]"));
    }

    #[test]
    fn test_build_openai_sse_tool_call() {
        let event = StreamEvent::ToolCallStart {
            content_index: 0,
            id: "call_123".to_string(),
            name: "get_weather".to_string(),
        };

        let sse = build_openai_sse(&event, "req_123", "gpt-4");
        assert!(sse.contains("\"id\":\"call_123\""));
        assert!(sse.contains("\"name\":\"get_weather\""));
    }

    #[expect(
        clippy::panic_in_result_fn,
        reason = "Test assertions intentionally fail the test while fallible setup propagates errors"
    )]
    #[test]
    fn test_openai_transformer_transform_request() -> Result<(), Box<dyn std::error::Error>> {
        let transformer = OpenAITransformer::new();
        let ctx = Context::new("gpt-4")
            .with_system("Be helpful")
            .with_messages(vec![Message::user("Hi")])
            .with_max_tokens(1000)
            .with_stream(true);

        let request = transformer.transform_request(&ctx)?;

        assert_eq!(request["model"], "gpt-4");
        assert_eq!(request["stream"], true);
        assert!(request["messages"].is_array());
        assert_eq!(request["max_completion_tokens"], 1000);
        Ok(())
    }

    #[expect(
        clippy::panic_in_result_fn,
        reason = "Test assertions intentionally fail the test while fallible setup propagates errors"
    )]
    #[test]
    fn test_openai_transformer_with_compat() -> Result<(), Box<dyn std::error::Error>> {
        let transformer = OpenAITransformer::new().with_compat(false, true, false);
        let ctx = Context::new("gpt-4").with_max_tokens(1000);

        let request = transformer.transform_request(&ctx)?;

        // Should use max_tokens instead of max_completion_tokens
        assert_eq!(request["max_tokens"], 1000);
        assert!(request.get("max_completion_tokens").is_none());
        Ok(())
    }

    #[expect(
        clippy::panic_in_result_fn,
        reason = "Test assertions intentionally fail the test while fallible setup propagates errors"
    )]
    #[test]
    fn test_parse_stream_chunk() -> Result<(), Box<dyn std::error::Error>> {
        let transformer = OpenAITransformer::new();
        let mut state = StreamState::default();

        let chunk = r#"data: {"id":"chatcmpl-123","object":"chat.completion.chunk","created":1234567890,"model":"gpt-4","choices":[{"index":0,"delta":{"role":"assistant"},"finish_reason":null}]}

data: {"id":"chatcmpl-123","object":"chat.completion.chunk","created":1234567890,"model":"gpt-4","choices":[{"index":0,"delta":{"content":"Hello"},"finish_reason":null}]}

"#;

        let events = transformer.parse_stream_chunk(chunk, &mut state)?;

        assert!(events
            .iter()
            .any(|e| matches!(e, StreamEvent::Start { .. })));
        assert!(events
            .iter()
            .any(|e| matches!(e, StreamEvent::TextDelta { delta, .. } if delta == "Hello")));
        Ok(())
    }

    #[expect(
        clippy::panic_in_result_fn,
        reason = "Test assertions intentionally fail the test while fallible setup propagates errors"
    )]
    #[test]
    fn test_parse_stream_done() -> Result<(), Box<dyn std::error::Error>> {
        let transformer = OpenAITransformer::new();
        let mut state = StreamState {
            started: true,
            ..Default::default()
        };

        let chunk = "data: [DONE]\n\n";
        let events = transformer.parse_stream_chunk(chunk, &mut state)?;

        assert!(events.iter().any(|e| matches!(e, StreamEvent::Done { .. })));
        Ok(())
    }

    #[expect(
        clippy::panic_in_result_fn,
        reason = "Test assertions intentionally fail the test while fallible setup propagates errors"
    )]
    #[test]
    fn test_parse_data_url() -> Result<(), Box<dyn std::error::Error>> {
        let url = "data:image/png;base64,iVBORw0KGgo=";
        let (mime, data) = parse_data_url(url).ok_or("invalid data url")?;
        assert_eq!(mime, "image/png");
        assert_eq!(data, "iVBORw0KGgo=");
        Ok(())
    }

    #[test]
    fn test_tool_to_openai() {
        let tool = Tool::new(
            "get_weather",
            "Get weather",
            json!({"type": "object", "properties": {"city": {"type": "string"}}}),
        );

        let openai_tool = tool_to_openai(&tool);
        assert_eq!(openai_tool["type"], "function");
        assert_eq!(openai_tool["function"]["name"], "get_weather");
    }

    #[expect(
        clippy::panic_in_result_fn,
        reason = "Test assertions intentionally fail the test while fallible setup propagates errors"
    )]
    #[test]
    fn test_parse_response_non_streaming() -> Result<(), Box<dyn std::error::Error>> {
        let transformer = OpenAITransformer::new();
        let body = json!({
            "id": "chatcmpl-123",
            "object": "chat.completion",
            "created": 1_234_567_890,
            "model": "gpt-4",
            "choices": [{
                "index": 0,
                "message": {
                    "role": "assistant",
                    "content": "Hello!"
                },
                "finish_reason": "stop"
            }],
            "usage": {
                "prompt_tokens": 10,
                "completion_tokens": 5,
                "total_tokens": 15
            }
        });

        let events = transformer.parse_response(&body)?;

        assert!(events
            .iter()
            .any(|e| matches!(e, StreamEvent::TextEnd { content, .. } if content == "Hello!")));
        assert!(events.iter().any(|e| matches!(
            e,
            StreamEvent::Done {
                reason: StopReason::EndTurn,
                ..
            }
        )));
        Ok(())
    }

    #[expect(
        clippy::panic_in_result_fn,
        reason = "Test assertions intentionally fail the test while fallible setup propagates errors"
    )]
    #[test]
    fn test_build_openai_messages_with_thinking() -> Result<(), Box<dyn std::error::Error>> {
        let ctx = Context::new("gpt-4").with_messages(vec![Message::Assistant(AssistantMessage {
            content: vec![
                ContentBlock::Thinking(ThinkingContent::new("Let me think...")),
                ContentBlock::Text(TextContent::new("Here's my answer")),
            ],
            ..Default::default()
        })]);

        let messages = build_openai_messages(&ctx, false);

        // Thinking should be converted to text with tags
        let assistant_msg = &messages[0];
        let content = assistant_msg["content"].as_str().ok_or("content missing")?;
        assert!(content.contains("<thinking>"));
        assert!(content.contains("Let me think..."));
        assert!(content.contains("</thinking>"));
        assert!(content.contains("Here's my answer"));
        Ok(())
    }
}

//! OpenAI format parser and builder.
//!
//! Handles parsing incoming OpenAI /v1/chat/completions requests into canonical format
//! and building OpenAI SSE responses from canonical stream events.

use crate::transform::{RequestTransformer, ResponseTransformer, TransformError};
use crate::types::{
    ApiType, AssistantMessage, ContentBlock, ContentBlockState, ContentBlockType, Context,
    ImageContent, Message, StopReason, StreamEvent, StreamState, TextContent, Tool, ToolCall,
    ToolResultMessage, Usage, UserMessage,
};
use serde::Deserialize;
use serde_json::{json, Value};

/// OpenAI completions format transformer.
#[derive(Debug, Clone, Default)]
pub struct OpenAITransformer {
    /// Whether to use developer role instead of system
    pub use_developer_role: bool,
    /// Whether to use max_tokens instead of max_completion_tokens
    pub use_max_tokens: bool,
    /// Whether the provider supports the store field
    pub supports_store: bool,
}

impl OpenAITransformer {
    pub fn new() -> Self {
        Self::default()
    }

    /// Configure for specific compat settings.
    pub fn with_compat(
        mut self,
        supports_developer_role: bool,
        use_max_tokens: bool,
        supports_store: bool,
    ) -> Self {
        self.use_developer_role = !supports_developer_role;
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

        // Add stream options for usage reporting
        if context.stream {
            request["stream_options"] = json!({"include_usage": true});
        }

        Ok(request)
    }

    fn headers(&self, api_key: &str) -> Vec<(String, String)> {
        let mut headers = vec![("Content-Type".to_string(), "application/json".to_string())];
        if !api_key.is_empty() {
            headers.push(("Authorization".to_string(), format!("Bearer {}", api_key)));
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

                events.extend(process_openai_chunk(parsed, state)?);
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
            stop_reason: parse_finish_reason(&choice.finish_reason),
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

/// Parse an OpenAI request body into canonical Context.
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
                let content = msg["content"].as_str().unwrap_or_default().to_string();
                context.system_prompt = Some(content);
            }
            "user" => {
                let user_msg = parse_user_message(msg)?;
                context.messages.push(Message::User(user_msg));
            }
            "assistant" => {
                let assistant_msg = parse_assistant_message(msg)?;
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

/// Build OpenAI SSE response from canonical stream events.
pub fn build_openai_sse(event: &StreamEvent, request_id: &str, model: &str) -> String {
    match event {
        StreamEvent::Start { partial: _ } => {
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
            format!("data: {}\n\n", chunk)
        }
        StreamEvent::TextDelta { delta, .. } => {
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
            format!("data: {}\n\n", chunk)
        }
        StreamEvent::ThinkingDelta { delta, .. } => {
            // OpenAI doesn't have native thinking, convert to text
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
            format!("data: {}\n\n", chunk)
        }
        StreamEvent::ToolCallStart {
            id,
            name,
            content_index,
        } => {
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
            format!("data: {}\n\n", chunk)
        }
        StreamEvent::ToolCallDelta {
            delta,
            content_index,
        } => {
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
            format!("data: {}\n\n", chunk)
        }
        StreamEvent::Usage { usage } => {
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
            format!("data: {}\n\n", chunk)
        }
        StreamEvent::Done { reason, .. } => {
            let finish_reason = match reason {
                StopReason::EndTurn => "stop",
                StopReason::StopSequence => "stop",
                StopReason::MaxTokens => "length",
                StopReason::ToolUse => "tool_calls",
                StopReason::ContentFilter => "content_filter",
                StopReason::Other => "stop",
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
            format!("data: {}\n\ndata: [DONE]\n\n", chunk)
        }
        StreamEvent::Error { message, .. } => {
            let error_msg = message.error_message.as_deref().unwrap_or("Unknown error");
            let chunk = json!({
                "error": {
                    "message": error_msg,
                    "type": "server_error"
                }
            });
            format!("data: {}\n\n", chunk)
        }
        // Handle remaining events with empty deltas
        _ => String::new(),
    }
}

// ============================================================================
// Internal helpers
// ============================================================================

fn parse_user_message(msg: &Value) -> Result<UserMessage, TransformError> {
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

    Ok(UserMessage {
        content,
        timestamp: 0,
    })
}

fn parse_assistant_message(msg: &Value) -> Result<AssistantMessage, TransformError> {
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

    Ok(AssistantMessage {
        content,
        api: ApiType::OpenAICompletions,
        ..Default::default()
    })
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
                let content = build_openai_content(&user.content);
                messages.push(json!({
                    "role": "user",
                    "content": content
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
                    }).collect::<Vec<_>>().join("")
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
    let (meta, data) = without_prefix.split_once(",")?;
    let mime = meta.strip_suffix(";base64").unwrap_or(meta);
    Some((mime.to_string(), data.to_string()))
}

fn parse_finish_reason(reason: &Option<String>) -> StopReason {
    match reason.as_deref() {
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
    id: String,
    #[allow(dead_code)]
    object: String,
    #[allow(dead_code)]
    created: i64,
    model: String,
    choices: Vec<OpenAIStreamChoice>,
    #[serde(default)]
    usage: Option<OpenAIUsage>,
}

#[derive(Debug, Deserialize)]
struct OpenAIStreamChoice {
    index: usize,
    delta: OpenAIDelta,
    finish_reason: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
struct OpenAIDelta {
    #[serde(default)]
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
    #[allow(dead_code)]
    id: String,
    model: String,
    choices: Vec<OpenAIChoice>,
    usage: OpenAIUsage,
}

#[derive(Debug, Deserialize)]
struct OpenAIChoice {
    #[allow(dead_code)]
    index: usize,
    message: OpenAIMessage,
    finish_reason: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
struct OpenAIMessage {
    #[allow(dead_code)]
    #[serde(default)]
    role: Option<String>,
    #[serde(default)]
    content: Option<String>,
    #[serde(default)]
    tool_calls: Option<Vec<OpenAIToolCall>>,
}

#[derive(Debug, Deserialize)]
struct OpenAIToolCall {
    id: String,
    #[allow(dead_code)]
    #[serde(rename = "type")]
    call_type: String,
    function: OpenAIFunction,
}

#[derive(Debug, Deserialize)]
struct OpenAIFunction {
    name: String,
    arguments: String,
}

fn process_openai_chunk(
    chunk: OpenAIStreamChunk,
    state: &mut StreamState,
) -> Result<Vec<StreamEvent>, TransformError> {
    let mut events = Vec::new();

    // Handle start
    if !state.started {
        state.started = true;
        state.message.model = chunk.model.clone();
        state.message.api = ApiType::OpenAICompletions;
        events.push(StreamEvent::Start {
            partial: state.message.clone(),
        });
    }

    // Process choices
    for choice in chunk.choices {
        // Handle role
        if choice.delta.role.is_some() {
            // Role delta, already handled in Start
        }

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
        }

        // Handle finish reason
        if let Some(ref reason) = choice.finish_reason {
            state.message.stop_reason = parse_finish_reason(&Some(reason.clone()));

            // Finalize content blocks
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

    Ok(events)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::ThinkingContent;

    #[test]
    fn test_parse_openai_request_simple() {
        let body = json!({
            "model": "gpt-4",
            "messages": [
                {"role": "system", "content": "You are helpful"},
                {"role": "user", "content": "Hello"}
            ],
            "stream": true
        });

        let ctx = parse_openai_request(&body).unwrap();
        assert_eq!(ctx.model, "gpt-4");
        assert_eq!(ctx.system_prompt, Some("You are helpful".to_string()));
        assert_eq!(ctx.messages.len(), 1);
        assert!(ctx.stream);
    }

    #[test]
    fn test_parse_openai_request_with_tools() {
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

        let ctx = parse_openai_request(&body).unwrap();
        assert!(ctx.tools.is_some());
        let tools = ctx.tools.unwrap();
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].name, "get_weather");
    }

    #[test]
    fn test_parse_openai_request_with_image() {
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

        let ctx = parse_openai_request(&body).unwrap();
        assert_eq!(ctx.messages.len(), 1);

        if let Message::User(user) = &ctx.messages[0] {
            assert_eq!(user.content.len(), 2);
            assert!(matches!(&user.content[1], ContentBlock::Image(_)));
        } else {
            panic!("Expected user message");
        }
    }

    #[test]
    fn test_parse_openai_request_with_tool_call() {
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

        let ctx = parse_openai_request(&body).unwrap();
        assert_eq!(ctx.messages.len(), 3);

        // Check assistant message with tool call
        if let Message::Assistant(assistant) = &ctx.messages[1] {
            assert_eq!(assistant.content.len(), 1);
            if let ContentBlock::ToolCall(tc) = &assistant.content[0] {
                assert_eq!(tc.id, "call_123");
                assert_eq!(tc.name, "get_weather");
            } else {
                panic!("Expected tool call");
            }
        } else {
            panic!("Expected assistant message");
        }

        // Check tool result
        if let Message::Tool(tool) = &ctx.messages[2] {
            assert_eq!(tool.tool_call_id, "call_123");
        } else {
            panic!("Expected tool message");
        }
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

    #[test]
    fn test_openai_transformer_transform_request() {
        let transformer = OpenAITransformer::new();
        let ctx = Context::new("gpt-4")
            .with_system("Be helpful")
            .with_messages(vec![Message::user("Hi")])
            .with_max_tokens(1000)
            .with_stream(true);

        let request = transformer.transform_request(&ctx).unwrap();

        assert_eq!(request["model"], "gpt-4");
        assert_eq!(request["stream"], true);
        assert!(request["messages"].is_array());
        assert_eq!(request["max_completion_tokens"], 1000);
    }

    #[test]
    fn test_openai_transformer_with_compat() {
        let transformer = OpenAITransformer::new().with_compat(false, true, false);
        let ctx = Context::new("gpt-4").with_max_tokens(1000);

        let request = transformer.transform_request(&ctx).unwrap();

        // Should use max_tokens instead of max_completion_tokens
        assert_eq!(request["max_tokens"], 1000);
        assert!(request.get("max_completion_tokens").is_none());
    }

    #[test]
    fn test_parse_stream_chunk() {
        let transformer = OpenAITransformer::new();
        let mut state = StreamState::default();

        let chunk = r#"data: {"id":"chatcmpl-123","object":"chat.completion.chunk","created":1234567890,"model":"gpt-4","choices":[{"index":0,"delta":{"role":"assistant"},"finish_reason":null}]}

data: {"id":"chatcmpl-123","object":"chat.completion.chunk","created":1234567890,"model":"gpt-4","choices":[{"index":0,"delta":{"content":"Hello"},"finish_reason":null}]}

"#;

        let events = transformer.parse_stream_chunk(chunk, &mut state).unwrap();

        assert!(events
            .iter()
            .any(|e| matches!(e, StreamEvent::Start { .. })));
        assert!(events
            .iter()
            .any(|e| matches!(e, StreamEvent::TextDelta { delta, .. } if delta == "Hello")));
    }

    #[test]
    fn test_parse_stream_done() {
        let transformer = OpenAITransformer::new();
        let mut state = StreamState::default();
        state.started = true;

        let chunk = "data: [DONE]\n\n";
        let events = transformer.parse_stream_chunk(chunk, &mut state).unwrap();

        assert!(events.iter().any(|e| matches!(e, StreamEvent::Done { .. })));
    }

    #[test]
    fn test_parse_data_url() {
        let url = "data:image/png;base64,iVBORw0KGgo=";
        let (mime, data) = parse_data_url(url).unwrap();
        assert_eq!(mime, "image/png");
        assert_eq!(data, "iVBORw0KGgo=");
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

    #[test]
    fn test_parse_response_non_streaming() {
        let transformer = OpenAITransformer::new();
        let body = json!({
            "id": "chatcmpl-123",
            "object": "chat.completion",
            "created": 1234567890,
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

        let events = transformer.parse_response(&body).unwrap();

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
    }

    #[test]
    fn test_build_openai_messages_with_thinking() {
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
        let content = assistant_msg["content"].as_str().unwrap();
        assert!(content.contains("<thinking>"));
        assert!(content.contains("Let me think..."));
        assert!(content.contains("</thinking>"));
        assert!(content.contains("Here's my answer"));
    }
}

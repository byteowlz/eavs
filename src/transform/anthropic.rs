//! Anthropic Messages API transformer.
//!
//! Handles transformation between canonical format and Anthropic's Messages API.

use crate::transform::{RequestTransformer, ResponseTransformer, TransformError};
#[cfg(test)]
use crate::types::ImageContent;
use crate::types::{
    ApiType, AssistantMessage, ContentBlock, ContentBlockState, ContentBlockType, Context, Message,
    StopReason, StreamEvent, StreamState, TextContent, ThinkingContent, Tool, ToolCall, Usage,
};
use serde::Deserialize;
use serde_json::{json, Value};

/// Anthropic Messages API transformer.
#[derive(Debug, Clone, Default)]
pub struct AnthropicTransformer {
    /// Whether to enable caching
    pub enable_cache: bool,
}

impl AnthropicTransformer {
    pub const fn new() -> Self {
        Self { enable_cache: true }
    }

    pub const fn with_cache(mut self, enable: bool) -> Self {
        self.enable_cache = enable;
        self
    }
}

impl RequestTransformer for AnthropicTransformer {
    fn transform_request(&self, context: &Context) -> Result<Value, TransformError> {
        let mut request = json!({
            "model": context.model,
            "stream": context.stream,
        });

        // Add system prompt with cache control
        if let Some(ref system) = context.system_prompt {
            if self.enable_cache {
                request["system"] = json!([{
                    "type": "text",
                    "text": system,
                    "cache_control": {"type": "ephemeral"}
                }]);
            } else {
                request["system"] = json!(system);
            }
        }

        // Build messages
        let messages = build_anthropic_messages(context, self.enable_cache)?;
        request["messages"] = Value::Array(messages);

        // Add max_tokens (required for Anthropic)
        let max_tokens = context.max_tokens.unwrap_or(4096);
        request["max_tokens"] = json!(max_tokens);

        // Add optional parameters
        if let Some(temp) = context.temperature {
            request["temperature"] = json!(temp);
        }

        if let Some(top_p) = context.top_p {
            request["top_p"] = json!(top_p);
        }

        if let Some(ref stop) = context.stop {
            request["stop_sequences"] = json!(stop);
        }

        // Add tools
        if let Some(ref tools) = context.tools {
            let anthropic_tools: Vec<Value> = tools.iter().map(tool_to_anthropic).collect();
            request["tools"] = Value::Array(anthropic_tools);
        }

        // Translate OpenAI-style tool_choice to Anthropic.
        if let Some(orig) = context.original_request.as_ref() {
            if let Some(tool_choice) = orig.get("tool_choice") {
                match anthropic_tool_choice_from_openai(tool_choice) {
                    AnthropicToolChoiceDecision::OmitTools => {
                        // Anthropic doesn't support tool_choice="none"; omit tools entirely.
                        if let Some(obj) = request.as_object_mut() {
                            obj.remove("tools");
                        }
                    }
                    AnthropicToolChoiceDecision::Set(tc) => {
                        request["tool_choice"] = tc;
                    }
                    AnthropicToolChoiceDecision::Ignore => {}
                }
            }
        }

        Ok(request)
    }

    #[cfg(test)]
    fn headers(&self, api_key: &str) -> Vec<(String, String)> {
        vec![
            ("Content-Type".to_string(), "application/json".to_string()),
            ("x-api-key".to_string(), api_key.to_string()),
            ("anthropic-version".to_string(), "2023-06-01".to_string()),
        ]
    }

    fn endpoint_path(&self, _context: &Context) -> String {
        // Note: base_url already includes /v1, so we only need /messages
        "/messages".to_string()
    }
}

impl ResponseTransformer for AnthropicTransformer {
    fn parse_stream_chunk(
        &self,
        chunk: &str,
        state: &mut StreamState,
    ) -> Result<Vec<StreamEvent>, TransformError> {
        let mut events = Vec::new();

        for line in chunk.lines() {
            let line = line.trim();

            // Skip empty lines
            if line.is_empty() {
                continue;
            }

            // Parse event type
            if let Some(event_type) = line.strip_prefix("event: ") {
                state.provider_state["event_type"] = json!(event_type.trim());
                continue;
            }

            // Parse data
            if let Some(data) = line.strip_prefix("data: ") {
                let event_type = state.provider_state["event_type"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string();

                let parsed: Value = serde_json::from_str(data.trim())
                    .map_err(|e| TransformError::InvalidJson(e.to_string()))?;

                events.extend(process_anthropic_event(&event_type, &parsed, state));
            }
        }

        Ok(events)
    }

    fn parse_response(&self, body: &Value) -> Result<Vec<StreamEvent>, TransformError> {
        let response: AnthropicResponse = serde_json::from_value(body.clone())
            .map_err(|e| TransformError::InvalidJson(e.to_string()))?;

        let mut events = Vec::new();
        let mut message = AssistantMessage {
            api: ApiType::AnthropicMessages,
            model: response.model,
            usage: Usage {
                prompt_tokens: response.usage.input_tokens,
                completion_tokens: response.usage.output_tokens,
                total_tokens: response.usage.input_tokens + response.usage.output_tokens,
                cache_read_input_tokens: response.usage.cache_read_input_tokens,
                cache_creation_input_tokens: response.usage.cache_creation_input_tokens,
            },
            stop_reason: parse_stop_reason(response.stop_reason.as_deref()),
            ..Default::default()
        };

        // Parse content blocks
        for (idx, content) in response.content.iter().enumerate() {
            match content.content_type.as_str() {
                "text" => {
                    let text = content.text.as_deref().unwrap_or_default();
                    message
                        .content
                        .push(ContentBlock::Text(TextContent::new(text)));
                    events.push(StreamEvent::TextEnd {
                        content_index: idx,
                        content: text.to_string(),
                    });
                }
                "thinking" => {
                    let thinking = content.thinking.as_deref().unwrap_or_default();
                    let signature = content.signature.clone();
                    message
                        .content
                        .push(ContentBlock::Thinking(signature.clone().map_or_else(
                            || ThinkingContent::new(thinking),
                            |sig| ThinkingContent::with_signature(thinking, sig),
                        )));
                    events.push(StreamEvent::ThinkingEnd {
                        content_index: idx,
                        content: thinking.to_string(),
                        signature,
                    });
                }
                "tool_use" => {
                    let tool_call = ToolCall::new(
                        content.id.as_deref().unwrap_or_default(),
                        strip_oauth_tool_prefix(content.name.as_deref().unwrap_or_default()),
                        content.input.clone().unwrap_or(Value::Null),
                    );
                    message
                        .content
                        .push(ContentBlock::ToolCall(tool_call.clone()));
                    events.push(StreamEvent::ToolCallEnd {
                        content_index: idx,
                        tool_call,
                    });
                }
                _ => {}
            }
        }

        events.push(StreamEvent::Done {
            reason: message.stop_reason.clone(),
            message,
        });

        Ok(events)
    }
}

/// Parse an Anthropic request body into canonical Context.
#[cfg(test)]
pub fn parse_anthropic_request(body: &Value) -> Result<Context, TransformError> {
    let model = body["model"]
        .as_str()
        .ok_or_else(|| TransformError::MissingField("model".to_string()))?
        .to_string();

    let mut context = Context::new(model);

    // Parse system prompt
    if let Some(system) = body["system"].as_str() {
        context.system_prompt = Some(system.to_string());
    } else if let Some(system_array) = body["system"].as_array() {
        // Handle array format with cache_control
        let text: Vec<String> = system_array
            .iter()
            .filter_map(|s| s["text"].as_str().map(String::from))
            .collect();
        if !text.is_empty() {
            context.system_prompt = Some(text.join("\n"));
        }
    }

    // Parse messages
    let messages = body["messages"]
        .as_array()
        .ok_or_else(|| TransformError::MissingField("messages".to_string()))?;

    for msg in messages {
        let role = msg["role"]
            .as_str()
            .ok_or_else(|| TransformError::MissingField("role".to_string()))?;

        match role {
            "user" => {
                let msgs = parse_anthropic_user_message_to_messages(msg);
                context.messages.extend(msgs);
            }
            "assistant" => {
                let assistant_msg = parse_anthropic_assistant_message(msg);
                context.messages.push(Message::Assistant(assistant_msg));
            }
            _ => {}
        }
    }

    // Parse optional parameters
    context.stream = body["stream"].as_bool().unwrap_or(false);
    context.max_tokens = body["max_tokens"].as_u64().map(|v| v as u32);
    context.temperature = body["temperature"].as_f64().map(|v| v as f32);
    context.top_p = body["top_p"].as_f64().map(|v| v as f32);

    if let Some(stop) = body["stop_sequences"].as_array() {
        context.stop = Some(
            stop.iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect(),
        );
    }

    // Parse tools
    if let Some(tools) = body["tools"].as_array() {
        let parsed_tools: Result<Vec<Tool>, _> = tools.iter().map(parse_anthropic_tool).collect();
        context.tools = Some(parsed_tools?);
    }

    context.original_request = Some(body.clone());

    Ok(context)
}

#[cfg(test)]
fn parse_anthropic_user_message_to_messages(msg: &Value) -> Vec<Message> {
    let mut out = Vec::new();

    match &msg["content"] {
        Value::String(s) => {
            out.push(Message::User(crate::types::UserMessage {
                content: vec![ContentBlock::Text(TextContent::new(s))],
                timestamp: 0,
            }));
        }
        Value::Array(parts) => {
            let mut current = Vec::new();
            for part in parts {
                let part_type = part["type"].as_str().unwrap_or("text");
                match part_type {
                    "text" => {
                        let text = part["text"].as_str().unwrap_or_default();
                        current.push(ContentBlock::Text(TextContent::new(text)));
                    }
                    "image" => {
                        let source = &part["source"];
                        let media_type = source["media_type"].as_str().unwrap_or("image/png");
                        let data = source["data"].as_str().unwrap_or_default();
                        current.push(ContentBlock::Image(ImageContent::base64(data, media_type)));
                    }
                    "tool_result" => {
                        if !current.is_empty() {
                            out.push(Message::User(crate::types::UserMessage {
                                content: std::mem::take(&mut current),
                                timestamp: 0,
                            }));
                        }

                        let tool_call_id = part["tool_use_id"].as_str().unwrap_or_default();
                        let result_content = part["content"].as_str().unwrap_or_default();
                        let is_error = part["is_error"].as_bool().unwrap_or(false);

                        out.push(Message::Tool(crate::types::ToolResultMessage {
                            tool_call_id: tool_call_id.to_string(),
                            tool_name: String::new(),
                            content: vec![ContentBlock::Text(TextContent::new(result_content))],
                            is_error,
                            timestamp: 0,
                        }));
                    }
                    _ => {}
                }
            }

            if !current.is_empty() {
                out.push(Message::User(crate::types::UserMessage {
                    content: current,
                    timestamp: 0,
                }));
            }
        }
        _ => {}
    }

    out
}

/// Build Anthropic SSE response from canonical stream events.
#[cfg(test)]
pub fn build_anthropic_sse(event: &StreamEvent) -> String {
    match event {
        StreamEvent::Start { partial } => anthropic_sse_message_start(partial),
        StreamEvent::TextStart { content_index } => {
            anthropic_content_block_start(*content_index, "text", "")
        }
        StreamEvent::ThinkingStart { content_index } => {
            anthropic_content_block_start(*content_index, "thinking", "")
        }
        StreamEvent::TextDelta {
            content_index,
            delta,
        } => anthropic_content_block_delta(*content_index, "text_delta", delta),
        StreamEvent::ThinkingDelta {
            content_index,
            delta,
        } => anthropic_content_block_delta(*content_index, "thinking_delta", delta),
        StreamEvent::TextEnd { content_index, .. }
        | StreamEvent::ToolCallEnd { content_index, .. } => {
            anthropic_content_block_stop(*content_index)
        }
        StreamEvent::ThinkingEnd {
            content_index,
            signature,
            ..
        } => anthropic_content_block_stop_with_signature(*content_index, signature.as_deref()),
        StreamEvent::ToolCallStart {
            content_index,
            id,
            name,
        } => anthropic_sse_tool_call_start(*content_index, id, name),
        StreamEvent::ToolCallDelta {
            content_index,
            delta,
        } => anthropic_sse_tool_call_delta(*content_index, delta),
        StreamEvent::Usage { usage } => anthropic_sse_usage(usage),
        StreamEvent::Done { reason, message } => anthropic_sse_done(reason, message),
        StreamEvent::Error { message, .. } => anthropic_sse_error(message),
    }
}

/// Format a JSON payload as a single SSE `event:`/`data:` frame.
#[cfg(test)]
fn anthropic_sse_frame(event: &str, data: &Value) -> String {
    format!("event: {event}\ndata: {data}\n\n")
}

#[cfg(test)]
fn anthropic_sse_message_start(partial: &AssistantMessage) -> String {
    let data = json!({
        "type": "message_start",
        "message": {
            "id": format!("msg_{}", uuid::Uuid::new_v4()),
            "type": "message",
            "role": "assistant",
            "content": [],
            "model": partial.model,
            "stop_reason": null,
            "stop_sequence": null,
            "usage": {
                "input_tokens": partial.usage.prompt_tokens,
                "output_tokens": 0
            }
        }
    });
    anthropic_sse_frame("message_start", &data)
}

#[cfg(test)]
fn anthropic_content_block_start(content_index: usize, block_type: &str, text: &str) -> String {
    let data = json!({
        "type": "content_block_start",
        "index": content_index,
        "content_block": {
            "type": block_type,
            "text": text
        }
    });
    anthropic_sse_frame("content_block_start", &data)
}

#[cfg(test)]
fn anthropic_content_block_delta(content_index: usize, delta_type: &str, value: &str) -> String {
    let data = json!({
        "type": "content_block_delta",
        "index": content_index,
        "delta": {
            "type": delta_type,
            "text": value
        }
    });
    anthropic_sse_frame("content_block_delta", &data)
}

#[cfg(test)]
fn anthropic_content_block_stop(content_index: usize) -> String {
    let data = json!({
        "type": "content_block_stop",
        "index": content_index
    });
    anthropic_sse_frame("content_block_stop", &data)
}

#[cfg(test)]
fn anthropic_content_block_stop_with_signature(
    content_index: usize,
    signature: Option<&str>,
) -> String {
    let mut data = json!({
        "type": "content_block_stop",
        "index": content_index
    });
    if let Some(sig) = signature {
        data["signature"] = json!(sig);
    }
    anthropic_sse_frame("content_block_stop", &data)
}

#[cfg(test)]
fn anthropic_sse_tool_call_start(content_index: usize, id: &str, name: &str) -> String {
    let data = json!({
        "type": "content_block_start",
        "index": content_index,
        "content_block": {
            "type": "tool_use",
            "id": id,
            "name": name,
            "input": {}
        }
    });
    anthropic_sse_frame("content_block_start", &data)
}

#[cfg(test)]
fn anthropic_sse_tool_call_delta(content_index: usize, delta: &str) -> String {
    let data = json!({
        "type": "content_block_delta",
        "index": content_index,
        "delta": {
            "type": "input_json_delta",
            "partial_json": delta
        }
    });
    anthropic_sse_frame("content_block_delta", &data)
}

#[cfg(test)]
fn anthropic_sse_usage(usage: &Usage) -> String {
    let data = json!({
        "type": "message_delta",
        "usage": {
            "output_tokens": usage.completion_tokens
        }
    });
    anthropic_sse_frame("message_delta", &data)
}

#[cfg(test)]
fn anthropic_sse_done(reason: &StopReason, message: &AssistantMessage) -> String {
    let stop_reason = match reason {
        StopReason::EndTurn | StopReason::Other | StopReason::ContentFilter => "end_turn",
        StopReason::StopSequence => "stop_sequence",
        StopReason::MaxTokens => "max_tokens",
        StopReason::ToolUse => "tool_use",
    };

    let delta_data = json!({
        "type": "message_delta",
        "delta": {
            "stop_reason": stop_reason,
            "stop_sequence": null
        },
        "usage": {
            "output_tokens": message.usage.completion_tokens
        }
    });

    let stop_data = json!({"type": "message_stop"});

    format!(
        "event: message_delta\ndata: {delta_data}\n\nevent: message_stop\ndata: {stop_data}\n\n"
    )
}

#[cfg(test)]
fn anthropic_sse_error(message: &AssistantMessage) -> String {
    let error_msg = message.error_message.as_deref().unwrap_or("Unknown error");
    let data = json!({
        "type": "error",
        "error": {
            "type": "server_error",
            "message": error_msg
        }
    });
    anthropic_sse_frame("error", &data)
}

// ============================================================================
// Internal helpers
// ============================================================================

#[cfg(test)]
#[cfg(test)]
fn parse_anthropic_assistant_message(msg: &Value) -> AssistantMessage {
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
                    "thinking" => {
                        let thinking = part["thinking"].as_str().unwrap_or_default();
                        let signature = part["signature"].as_str().map(String::from);
                        if let Some(sig) = signature {
                            content.push(ContentBlock::Thinking(ThinkingContent::with_signature(
                                thinking, sig,
                            )));
                        } else {
                            content.push(ContentBlock::Thinking(ThinkingContent::new(thinking)));
                        }
                    }
                    "tool_use" => {
                        let id = part["id"].as_str().unwrap_or_default();
                        let name =
                            strip_oauth_tool_prefix(part["name"].as_str().unwrap_or_default());
                        let input = part["input"].clone();
                        content.push(ContentBlock::ToolCall(ToolCall::new(id, name, input)));
                    }
                    _ => {}
                }
            }
        }
        _ => {}
    }

    AssistantMessage {
        content,
        api: ApiType::AnthropicMessages,
        ..Default::default()
    }
}

#[cfg(test)]
fn parse_anthropic_tool(tool: &Value) -> Result<Tool, TransformError> {
    let name = tool["name"]
        .as_str()
        .ok_or_else(|| TransformError::MissingField("name".to_string()))?
        .to_string();
    let description = tool["description"].as_str().unwrap_or_default().to_string();
    let parameters = tool["input_schema"].clone();

    Ok(Tool::new(name, description, parameters))
}

fn strip_oauth_tool_prefix(name: &str) -> String {
    name.strip_prefix("mcp_").unwrap_or(name).to_string()
}

fn tool_to_anthropic(tool: &Tool) -> Value {
    json!({
        "name": tool.name,
        "description": tool.description,
        "input_schema": tool.parameters
    })
}

fn build_anthropic_messages(
    context: &Context,
    enable_cache: bool,
) -> Result<Vec<Value>, TransformError> {
    let mut messages = Vec::new();
    let mut last_user_idx = None;

    // Find last non-empty user message index for cache control
    for (idx, msg) in context.messages.iter().enumerate() {
        if let Message::User(user) = msg {
            if anthropic_message_has_any_content(&user.content) {
                last_user_idx = Some(idx);
            }
        }
    }

    for (idx, msg) in context.messages.iter().enumerate() {
        match msg {
            Message::User(user) => {
                let user_content = build_anthropic_content(&user.content)?;
                if user_content.as_array().is_none_or(std::vec::Vec::is_empty) {
                    // Anthropic rejects empty content blocks; drop empty messages.
                    continue;
                }
                let is_last_user = last_user_idx == Some(idx);

                let mut msg_obj = json!({
                    "role": "user",
                    "content": user_content
                });

                // Add cache control to last user message
                if enable_cache && is_last_user {
                    if let Some(content_array) = msg_obj["content"].as_array_mut() {
                        if let Some(last) = content_array.last_mut() {
                            last["cache_control"] = json!({"type": "ephemeral"});
                        }
                    }
                }

                messages.push(msg_obj);
            }
            Message::Assistant(assistant) => {
                let assistant_content = build_anthropic_assistant_content(&assistant.content);
                if assistant_content
                    .as_array()
                    .is_none_or(std::vec::Vec::is_empty)
                {
                    // Drop empty assistant messages (Anthropic rejects empty blocks).
                    continue;
                }
                messages.push(json!({
                    "role": "assistant",
                    "content": assistant_content
                }));
            }
            Message::Tool(tool_result) => {
                // In Anthropic, tool results go in user messages
                let content_text: String = tool_result
                    .content
                    .iter()
                    .map(|b| match b {
                        ContentBlock::Text(t) => t.text.clone(),
                        _ => String::new(),
                    })
                    .collect();

                let tool_content = if tool_result.is_error {
                    json!([{
                        "type": "tool_result",
                        "tool_use_id": tool_result.tool_call_id,
                        "is_error": true,
                        "content": content_text
                    }])
                } else {
                    json!([{
                        "type": "tool_result",
                        "tool_use_id": tool_result.tool_call_id,
                        "content": content_text
                    }])
                };

                messages.push(json!({
                    "role": "user",
                    "content": tool_content
                }));
            }
            Message::System(_) => {
                // System messages handled separately
            }
        }
    }

    Ok(messages)
}

fn build_anthropic_content(blocks: &[ContentBlock]) -> Result<Value, TransformError> {
    let mut parts = Vec::new();

    for block in blocks {
        match block {
            ContentBlock::Text(t) => {
                if t.text.trim().is_empty() {
                    continue;
                }
                parts.push(json!({
                    "type": "text",
                    "text": t.text
                }));
            }
            ContentBlock::Image(img) => {
                if img.is_url {
                    return Err(TransformError::Unsupported(
                        "Anthropic does not support URL images; provide a data: URL or base64"
                            .to_string(),
                    ));
                }
                parts.push(json!({
                    "type": "image",
                    "source": {
                        "type": "base64",
                        "media_type": img.mime_type,
                        "data": img.data
                    }
                }));
            }
            ContentBlock::ToolResult(tr) => parts.push(json!({
                "type": "tool_result",
                "tool_use_id": tr.tool_call_id,
                "content": tr.content,
                "is_error": tr.is_error
            })),
            _ => {}
        }
    }

    Ok(Value::Array(parts))
}

fn build_anthropic_assistant_content(blocks: &[ContentBlock]) -> Value {
    let parts: Vec<Value> = blocks
        .iter()
        .filter_map(|block| match block {
            ContentBlock::Text(t) => {
                if t.text.trim().is_empty() {
                    return None;
                }
                Some(json!({
                    "type": "text",
                    "text": t.text
                }))
            }
            ContentBlock::Thinking(t) => {
                let mut obj = json!({
                    "type": "thinking",
                    "thinking": t.thinking
                });
                if let Some(ref sig) = t.signature {
                    obj["signature"] = json!(sig);
                }
                Some(obj)
            }
            ContentBlock::ToolCall(tc) => Some(json!({
                "type": "tool_use",
                "id": tc.id,
                "name": tc.name,
                "input": tc.arguments
            })),
            _ => None,
        })
        .collect();

    Value::Array(parts)
}

#[derive(Debug)]
enum AnthropicToolChoiceDecision {
    OmitTools,
    Set(Value),
    Ignore,
}

fn anthropic_tool_choice_from_openai(tool_choice: &Value) -> AnthropicToolChoiceDecision {
    match tool_choice {
        Value::String(s) => match s.as_str() {
            "auto" => AnthropicToolChoiceDecision::Set(json!({"type": "auto"})),
            "any" | "required" => AnthropicToolChoiceDecision::Set(json!({"type": "any"})),
            "none" => AnthropicToolChoiceDecision::OmitTools,
            _ => AnthropicToolChoiceDecision::Ignore,
        },
        Value::Object(obj) => {
            let Some(choice_type) = obj.get("type").and_then(|t| t.as_str()) else {
                return AnthropicToolChoiceDecision::Ignore;
            };
            match choice_type {
                "auto" => AnthropicToolChoiceDecision::Set(json!({"type": "auto"})),
                "any" | "required" => AnthropicToolChoiceDecision::Set(json!({"type": "any"})),
                "none" => AnthropicToolChoiceDecision::OmitTools,
                "function" => {
                    let name = obj
                        .get("function")
                        .and_then(|f| f.get("name"))
                        .and_then(|n| n.as_str());
                    name.map_or(AnthropicToolChoiceDecision::Ignore, |name| {
                        AnthropicToolChoiceDecision::Set(json!({"type": "tool", "name": name}))
                    })
                }
                // Allow callers to pass Anthropic-style tool_choice through.
                "tool" => {
                    let name = obj.get("name").and_then(|n| n.as_str());
                    name.map_or(AnthropicToolChoiceDecision::Ignore, |name| {
                        AnthropicToolChoiceDecision::Set(json!({"type": "tool", "name": name}))
                    })
                }
                _ => AnthropicToolChoiceDecision::Ignore,
            }
        }
        _ => AnthropicToolChoiceDecision::Ignore,
    }
}

fn anthropic_message_has_any_content(blocks: &[ContentBlock]) -> bool {
    blocks.iter().any(|b| match b {
        ContentBlock::Text(t) => !t.text.trim().is_empty(),
        ContentBlock::Image(_) | ContentBlock::ToolResult(_) => true,
        _ => false,
    })
}

fn parse_stop_reason(reason: Option<&str>) -> StopReason {
    match reason {
        Some("end_turn") => StopReason::EndTurn,
        Some("stop_sequence") => StopReason::StopSequence,
        Some("max_tokens") => StopReason::MaxTokens,
        Some("tool_use") => StopReason::ToolUse,
        _ => StopReason::Other,
    }
}

// ============================================================================
// Anthropic response types
// ============================================================================

#[derive(Debug, Deserialize)]
struct AnthropicResponse {
    #[expect(dead_code, reason = "Deserialized for the upstream wire schema")]
    id: String,
    model: String,
    content: Vec<AnthropicContent>,
    stop_reason: Option<String>,
    usage: AnthropicUsage,
}

#[derive(Debug, Deserialize)]
struct AnthropicContent {
    #[serde(rename = "type")]
    content_type: String,
    #[serde(default)]
    text: Option<String>,
    #[serde(default)]
    thinking: Option<String>,
    #[serde(default)]
    signature: Option<String>,
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    input: Option<Value>,
}

#[expect(
    clippy::struct_field_names,
    reason = "Field names mirror the Anthropic token-count schema in the wire format"
)]
#[derive(Debug, Default, Deserialize)]
struct AnthropicUsage {
    #[serde(default)]
    input_tokens: u32,
    #[serde(default)]
    output_tokens: u32,
    #[serde(default)]
    cache_read_input_tokens: Option<u32>,
    #[serde(default)]
    cache_creation_input_tokens: Option<u32>,
}

fn process_anthropic_event(
    event_type: &str,
    data: &Value,
    state: &mut StreamState,
) -> Vec<StreamEvent> {
    let mut events = Vec::new();

    match event_type {
        "message_start" => handle_anthropic_message_start(data, state, &mut events),
        "content_block_start" => handle_anthropic_content_block_start(data, state, &mut events),
        "content_block_delta" => handle_anthropic_content_block_delta(data, state, &mut events),
        "content_block_stop" => handle_anthropic_content_block_stop(data, state, &mut events),
        "message_delta" => handle_anthropic_message_delta(data, state, &mut events),
        "message_stop" => handle_anthropic_message_stop(state, &mut events),
        "error" => handle_anthropic_error(data, state, &mut events),
        _ => {}
    }

    events
}

fn handle_anthropic_message_start(
    data: &Value,
    state: &mut StreamState,
    events: &mut Vec<StreamEvent>,
) {
    let message = &data["message"];
    state.started = true;
    state.message.model = message["model"].as_str().unwrap_or_default().to_string();
    state.message.api = ApiType::AnthropicMessages;

    if let Some(usage) = message.get("usage") {
        state.message.usage.prompt_tokens = usage["input_tokens"].as_u64().unwrap_or(0) as u32;
    }

    events.push(StreamEvent::Start {
        partial: state.message.clone(),
    });
}

fn handle_anthropic_content_block_start(
    data: &Value,
    state: &mut StreamState,
    events: &mut Vec<StreamEvent>,
) {
    let index = data["index"].as_u64().unwrap_or(0) as usize;
    let content_block = &data["content_block"];
    let block_type = content_block["type"].as_str().unwrap_or("text");

    while state.content_blocks.len() <= index {
        state.content_blocks.push(ContentBlockState::default());
    }

    match block_type {
        "text" => {
            state.content_blocks[index].block_type = ContentBlockType::Text;
            events.push(StreamEvent::TextStart {
                content_index: index,
            });
        }
        "thinking" => {
            state.content_blocks[index].block_type = ContentBlockType::Thinking;
            events.push(StreamEvent::ThinkingStart {
                content_index: index,
            });
        }
        "tool_use" => {
            state.content_blocks[index].block_type = ContentBlockType::ToolCall;
            let id = content_block["id"].as_str().unwrap_or_default().to_string();
            let name = strip_oauth_tool_prefix(content_block["name"].as_str().unwrap_or_default());
            state.content_blocks[index].tool_id = Some(id.clone());
            state.content_blocks[index].tool_name = Some(name.clone());
            events.push(StreamEvent::ToolCallStart {
                content_index: index,
                id,
                name,
            });
        }
        _ => {}
    }
}

fn handle_anthropic_content_block_delta(
    data: &Value,
    state: &mut StreamState,
    events: &mut Vec<StreamEvent>,
) {
    let index = data["index"].as_u64().unwrap_or(0) as usize;
    let delta = &data["delta"];
    let delta_type = delta["type"].as_str().unwrap_or("");

    if index < state.content_blocks.len() {
        match delta_type {
            "text_delta" => {
                let text = delta["text"].as_str().unwrap_or_default();
                state.content_blocks[index].text.push_str(text);
                events.push(StreamEvent::TextDelta {
                    content_index: index,
                    delta: text.to_string(),
                });
            }
            "thinking_delta" => {
                let thinking = delta["thinking"].as_str().unwrap_or_default();
                state.content_blocks[index].text.push_str(thinking);
                events.push(StreamEvent::ThinkingDelta {
                    content_index: index,
                    delta: thinking.to_string(),
                });
            }
            "input_json_delta" => {
                let partial = delta["partial_json"].as_str().unwrap_or_default();
                state.content_blocks[index].text.push_str(partial);
                events.push(StreamEvent::ToolCallDelta {
                    content_index: index,
                    delta: partial.to_string(),
                });
            }
            _ => {}
        }
    }
}

fn handle_anthropic_content_block_stop(
    data: &Value,
    state: &mut StreamState,
    events: &mut Vec<StreamEvent>,
) {
    let index = data["index"].as_u64().unwrap_or(0) as usize;

    if index < state.content_blocks.len() {
        let block = &state.content_blocks[index];
        match block.block_type {
            ContentBlockType::Text => {
                state
                    .message
                    .content
                    .push(ContentBlock::Text(TextContent::new(&block.text)));
                events.push(StreamEvent::TextEnd {
                    content_index: index,
                    content: block.text.clone(),
                });
            }
            ContentBlockType::Thinking => {
                let signature = data["signature"].as_str().map(String::from);
                if let Some(ref sig) = signature {
                    state.message.content.push(ContentBlock::Thinking(
                        ThinkingContent::with_signature(&block.text, sig),
                    ));
                } else {
                    state
                        .message
                        .content
                        .push(ContentBlock::Thinking(ThinkingContent::new(&block.text)));
                }
                events.push(StreamEvent::ThinkingEnd {
                    content_index: index,
                    content: block.text.clone(),
                    signature,
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
                    content_index: index,
                    tool_call,
                });
            }
        }
    }
}

fn handle_anthropic_message_delta(
    data: &Value,
    state: &mut StreamState,
    events: &mut Vec<StreamEvent>,
) {
    if let Some(delta) = data.get("delta") {
        if let Some(reason) = delta["stop_reason"].as_str() {
            state.message.stop_reason = parse_stop_reason(Some(reason));
        }
    }
    if let Some(usage) = data.get("usage") {
        state.message.usage.completion_tokens = usage["output_tokens"].as_u64().unwrap_or(0) as u32;
        state.message.usage.total_tokens =
            state.message.usage.prompt_tokens + state.message.usage.completion_tokens;
        events.push(StreamEvent::Usage {
            usage: state.message.usage.clone(),
        });
    }
}

fn handle_anthropic_message_stop(state: &StreamState, events: &mut Vec<StreamEvent>) {
    events.push(StreamEvent::Done {
        reason: state.message.stop_reason.clone(),
        message: state.message.clone(),
    });
}

fn handle_anthropic_error(data: &Value, state: &mut StreamState, events: &mut Vec<StreamEvent>) {
    let error_msg = data["error"]["message"].as_str().unwrap_or("Unknown error");
    state.message.error_message = Some(error_msg.to_string());
    events.push(StreamEvent::Error {
        reason: StopReason::Other,
        message: state.message.clone(),
    });
}

#[cfg(test)]
mod request_quirks_tests {
    use super::*;
    use crate::transform::openai::parse_openai_request;
    use crate::types::Message;

    #[expect(
        clippy::panic_in_result_fn,
        reason = "Test assertions intentionally fail the test while fallible setup propagates errors"
    )]
    #[test]
    fn anthropic_filters_empty_text_blocks() -> Result<(), Box<dyn std::error::Error>> {
        let body = json!({
            "model": "claude-3-5-sonnet-20240620",
            "messages": [{
                "role": "user",
                "content": [
                    {"type":"text","text":""},
                    {"type":"text","text":"  "},
                    {"type":"text","text":"hi"}
                ]
            }],
            "max_tokens": 16
        });
        let ctx = parse_openai_request(&body)?;
        let req = AnthropicTransformer::new().transform_request(&ctx)?;
        let messages = req["messages"].as_array().ok_or("messages missing")?;
        let content = messages[0]["content"].as_array().ok_or("content missing")?;
        assert_eq!(content.len(), 1);
        assert_eq!(content[0]["type"], "text");
        assert_eq!(content[0]["text"], "hi");
        Ok(())
    }

    #[expect(
        clippy::panic_in_result_fn,
        reason = "Test assertions intentionally fail the test while fallible setup propagates errors"
    )]
    #[test]
    fn anthropic_drops_empty_messages() -> Result<(), Box<dyn std::error::Error>> {
        let ctx = Context::new("claude-3-5-sonnet-20240620")
            .with_messages(vec![Message::user(""), Message::user("ok")]);
        let req = AnthropicTransformer::new().transform_request(&ctx)?;
        let messages = req["messages"].as_array().ok_or("messages missing")?;
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0]["role"], "user");
        Ok(())
    }

    #[expect(
        clippy::panic_in_result_fn,
        reason = "Test assertions intentionally fail the test while fallible setup propagates errors"
    )]
    #[test]
    fn anthropic_tool_choice_required_translates_to_any() -> Result<(), Box<dyn std::error::Error>>
    {
        let body = json!({
            "model": "claude-3-5-sonnet-20240620",
            "messages": [{"role":"user","content":"Call a tool"}],
            "tools": [{
                "type": "function",
                "function": {"name":"noop","description":"noop","parameters":{"type":"object","properties":{}}}
            }],
            "tool_choice": "required",
            "max_tokens": 16
        });
        let ctx = parse_openai_request(&body)?;
        let req = AnthropicTransformer::new().transform_request(&ctx)?;
        assert_eq!(req["tool_choice"]["type"], "any");
        Ok(())
    }

    #[expect(
        clippy::panic_in_result_fn,
        reason = "Test assertions intentionally fail the test while fallible setup propagates errors"
    )]
    #[test]
    fn anthropic_tool_choice_function_translates_to_tool() -> Result<(), Box<dyn std::error::Error>>
    {
        let body = json!({
            "model": "claude-3-5-sonnet-20240620",
            "messages": [{"role":"user","content":"Call a tool"}],
            "tools": [{
                "type": "function",
                "function": {"name":"noop","description":"noop","parameters":{"type":"object","properties":{}}}
            }],
            "tool_choice": {"type":"function","function":{"name":"noop"}},
            "max_tokens": 16
        });
        let ctx = parse_openai_request(&body)?;
        let req = AnthropicTransformer::new().transform_request(&ctx)?;
        assert_eq!(req["tool_choice"]["type"], "tool");
        assert_eq!(req["tool_choice"]["name"], "noop");
        Ok(())
    }

    #[expect(
        clippy::panic_in_result_fn,
        reason = "Test assertions intentionally fail the test while fallible setup propagates errors"
    )]
    #[test]
    fn anthropic_tool_choice_none_omits_tools() -> Result<(), Box<dyn std::error::Error>> {
        let body = json!({
            "model": "claude-3-5-sonnet-20240620",
            "messages": [{"role":"user","content":"Don't call tools"}],
            "tools": [{
                "type": "function",
                "function": {"name":"noop","description":"noop","parameters":{"type":"object","properties":{}}}
            }],
            "tool_choice": "none",
            "max_tokens": 16
        });
        let ctx = parse_openai_request(&body)?;
        let req = AnthropicTransformer::new().transform_request(&ctx)?;
        assert!(req.get("tools").is_none());
        assert!(req.get("tool_choice").is_none());
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transform::{parse_incoming_request, OpenAITransformer};
    use crate::types::ToolResultMessage;

    #[expect(
        clippy::panic_in_result_fn,
        reason = "Test assertions intentionally fail the test while fallible setup propagates errors"
    )]
    #[test]
    fn test_parse_anthropic_request_simple() -> Result<(), Box<dyn std::error::Error>> {
        let body = json!({
            "model": "claude-sonnet-4-20250514",
            "system": "You are helpful",
            "messages": [
                {"role": "user", "content": "Hello"}
            ],
            "max_tokens": 1024
        });

        let ctx = parse_anthropic_request(&body)?;
        assert_eq!(ctx.model, "claude-sonnet-4-20250514");
        assert_eq!(ctx.system_prompt, Some("You are helpful".to_string()));
        assert_eq!(ctx.messages.len(), 1);
        assert_eq!(ctx.max_tokens, Some(1024));
        Ok(())
    }

    #[expect(
        clippy::panic_in_result_fn,
        reason = "Test assertions intentionally fail the test while fallible setup propagates errors"
    )]
    #[test]
    fn test_parse_anthropic_request_with_cache() -> Result<(), Box<dyn std::error::Error>> {
        let body = json!({
            "model": "claude-sonnet-4-20250514",
            "system": [{"type": "text", "text": "Be helpful", "cache_control": {"type": "ephemeral"}}],
            "messages": [{"role": "user", "content": "Hello"}],
            "max_tokens": 1024
        });

        let ctx = parse_anthropic_request(&body)?;
        assert_eq!(ctx.system_prompt, Some("Be helpful".to_string()));
        Ok(())
    }

    #[expect(
        clippy::panic_in_result_fn,
        reason = "Test assertions intentionally fail the test while fallible setup propagates errors"
    )]
    #[test]
    fn test_parse_anthropic_request_with_tools() -> Result<(), Box<dyn std::error::Error>> {
        let body = json!({
            "model": "claude-sonnet-4-20250514",
            "messages": [{"role": "user", "content": "Get weather"}],
            "max_tokens": 1024,
            "tools": [{
                "name": "get_weather",
                "description": "Get current weather",
                "input_schema": {
                    "type": "object",
                    "properties": {"city": {"type": "string"}}
                }
            }]
        });

        let ctx = parse_anthropic_request(&body)?;
        assert!(ctx.tools.is_some());
        let tools = ctx.tools.as_ref().ok_or("tools missing")?;
        assert_eq!(tools[0].name, "get_weather");
        Ok(())
    }

    #[expect(
        clippy::panic_in_result_fn,
        reason = "Test assertions intentionally fail the test while fallible setup propagates errors"
    )]
    #[test]
    fn test_parse_anthropic_request_with_thinking() -> Result<(), Box<dyn std::error::Error>> {
        let body = json!({
            "model": "claude-sonnet-4-20250514",
            "messages": [
                {"role": "user", "content": "Think about this"},
                {
                    "role": "assistant",
                    "content": [
                        {"type": "thinking", "thinking": "Let me consider...", "signature": "sig123"},
                        {"type": "text", "text": "Here's my answer"}
                    ]
                }
            ],
            "max_tokens": 1024
        });

        let ctx = parse_anthropic_request(&body)?;

        let Message::Assistant(assistant) = &ctx.messages[1] else {
            return Err("Expected assistant message".into());
        };
        assert_eq!(assistant.content.len(), 2);
        let ContentBlock::Thinking(t) = &assistant.content[0] else {
            return Err("Expected thinking block".into());
        };
        assert_eq!(t.thinking, "Let me consider...");
        assert_eq!(t.signature, Some("sig123".to_string()));
        Ok(())
    }

    #[expect(
        clippy::panic_in_result_fn,
        reason = "Test assertions intentionally fail the test while fallible setup propagates errors"
    )]
    #[test]
    fn test_anthropic_transformer_transform_request() -> Result<(), Box<dyn std::error::Error>> {
        let transformer = AnthropicTransformer::new();
        let ctx = Context::new("claude-sonnet-4-20250514")
            .with_system("Be helpful")
            .with_messages(vec![Message::user("Hi")])
            .with_max_tokens(1000);

        let request = transformer.transform_request(&ctx)?;

        assert_eq!(request["model"], "claude-sonnet-4-20250514");
        assert_eq!(request["max_tokens"], 1000);
        assert!(request["system"].is_array()); // With cache control
        Ok(())
    }

    #[test]
    fn test_anthropic_transformer_headers() {
        let transformer = AnthropicTransformer::new();
        let headers = transformer.headers("sk-ant-test");

        assert!(headers
            .iter()
            .any(|(k, v)| k == "x-api-key" && v == "sk-ant-test"));
        assert!(headers
            .iter()
            .any(|(k, v)| k == "anthropic-version" && v == "2023-06-01"));
    }

    #[test]
    fn test_build_anthropic_sse_text_delta() {
        let event = StreamEvent::TextDelta {
            content_index: 0,
            delta: "Hello".to_string(),
        };

        let sse = build_anthropic_sse(&event);
        assert!(sse.starts_with("event: content_block_delta"));
        assert!(sse.contains("\"text\":\"Hello\""));
    }

    #[test]
    fn test_build_anthropic_sse_done() {
        let event = StreamEvent::Done {
            reason: StopReason::EndTurn,
            message: AssistantMessage::default(),
        };

        let sse = build_anthropic_sse(&event);
        assert!(sse.contains("message_delta"));
        assert!(sse.contains("message_stop"));
        assert!(sse.contains("\"stop_reason\":\"end_turn\""));
    }

    #[expect(
        clippy::panic_in_result_fn,
        reason = "Test assertions intentionally fail the test while fallible setup propagates errors"
    )]
    #[test]
    fn test_parse_stream_chunk() -> Result<(), Box<dyn std::error::Error>> {
        let transformer = AnthropicTransformer::new();
        let mut state = StreamState::default();

        let chunk = r#"event: message_start
data: {"type":"message_start","message":{"id":"msg_123","type":"message","role":"assistant","content":[],"model":"claude-sonnet-4-20250514","stop_reason":null,"usage":{"input_tokens":10,"output_tokens":0}}}

event: content_block_start
data: {"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}

event: content_block_delta
data: {"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"Hello"}}

"#;

        let events = transformer.parse_stream_chunk(chunk, &mut state)?;

        assert!(events
            .iter()
            .any(|e| matches!(e, StreamEvent::Start { .. })));
        assert!(events
            .iter()
            .any(|e| matches!(e, StreamEvent::TextStart { .. })));
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
    fn test_parse_response_non_streaming() -> Result<(), Box<dyn std::error::Error>> {
        let transformer = AnthropicTransformer::new();
        let body = json!({
            "id": "msg_123",
            "type": "message",
            "role": "assistant",
            "content": [
                {"type": "text", "text": "Hello!"}
            ],
            "model": "claude-sonnet-4-20250514",
            "stop_reason": "end_turn",
            "usage": {
                "input_tokens": 10,
                "output_tokens": 5
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

    #[test]
    fn test_tool_to_anthropic() {
        let tool = Tool::new(
            "get_weather",
            "Get weather",
            json!({"type": "object", "properties": {"city": {"type": "string"}}}),
        );

        let anthropic_tool = tool_to_anthropic(&tool);
        assert_eq!(anthropic_tool["name"], "get_weather");
        assert!(anthropic_tool["input_schema"].is_object());
    }

    #[expect(
        clippy::panic_in_result_fn,
        reason = "Test assertions intentionally fail the test while fallible setup propagates errors"
    )]
    #[test]
    fn test_build_anthropic_messages_with_tool_result() -> Result<(), Box<dyn std::error::Error>> {
        let ctx = Context::new("claude-sonnet-4-20250514").with_messages(vec![
            Message::user("Get weather"),
            Message::Assistant(AssistantMessage {
                content: vec![ContentBlock::ToolCall(ToolCall::new(
                    "tool_123",
                    "get_weather",
                    json!({"city": "NYC"}),
                ))],
                ..Default::default()
            }),
            Message::Tool(ToolResultMessage::text(
                "tool_123",
                "get_weather",
                "Sunny, 72F",
            )),
        ]);

        let messages = build_anthropic_messages(&ctx, false)?;

        assert_eq!(messages.len(), 3);
        // Tool result should be in a user message
        assert_eq!(messages[2]["role"], "user");
        assert_eq!(messages[2]["content"][0]["type"], "tool_result");
        Ok(())
    }

    #[expect(
        clippy::panic_in_result_fn,
        reason = "Test assertions intentionally fail the test while fallible setup propagates errors"
    )]
    #[test]
    fn test_build_anthropic_assistant_content_with_thinking(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let blocks = vec![
            ContentBlock::Thinking(ThinkingContent::with_signature("Thinking...", "sig123")),
            ContentBlock::Text(TextContent::new("Answer")),
        ];

        let content = build_anthropic_assistant_content(&blocks);
        let parts = content.as_array().ok_or("content not an array")?;

        assert_eq!(parts.len(), 2);
        assert_eq!(parts[0]["type"], "thinking");
        assert_eq!(parts[0]["signature"], "sig123");
        assert_eq!(parts[1]["type"], "text");
        Ok(())
    }

    #[expect(
        clippy::panic_in_result_fn,
        reason = "Test assertions intentionally fail the test while fallible setup propagates errors"
    )]
    #[test]
    fn test_openai_anthropic_openai_round_trip_preserves_image_and_tool_calls(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let openai_req = json!({
            "model": "claude-sonnet-4-20250514",
            "messages": [
                {"role": "system", "content": "Be helpful"},
                {"role": "user", "content": [
                    {"type": "text", "text": "What's in this image?"},
                    {"type": "image_url", "image_url": {"url": "data:image/png;base64,AAEC"}}
                ]},
                {
                    "role": "assistant",
                    "content": "",
                    "tool_calls": [{
                        "id": "call_123",
                        "type": "function",
                        "function": {"name": "get_weather", "arguments": "{\"city\":\"Paris\"}"}
                    }]
                },
                {"role": "tool", "tool_call_id": "call_123", "content": "{\"temp_c\":20}"}
            ],
            "tools": [{
                "type": "function",
                "function": {
                    "name": "get_weather",
                    "description": "Get weather",
                    "parameters": {
                        "type": "object",
                        "properties": {"city": {"type": "string"}},
                        "required": ["city"]
                    }
                }
            }],
            "max_tokens": 128
        });

        // OpenAI -> canonical
        let ctx = parse_incoming_request(&openai_req)?;
        assert_eq!(ctx.system_prompt.as_deref(), Some("Be helpful"));

        // canonical -> Anthropic
        let anthropic_req = AnthropicTransformer::new().transform_request(&ctx)?;

        // Anthropic -> canonical
        let ctx2 = parse_anthropic_request(&anthropic_req)?;
        assert_eq!(ctx2.system_prompt.as_deref(), Some("Be helpful"));

        // canonical -> OpenAI
        let openai_back = OpenAITransformer::new().transform_request(&ctx2)?;
        let msgs = openai_back["messages"]
            .as_array()
            .ok_or("messages missing")?;

        // User multimodal content preserved (as data URL)
        assert!(msgs[1]["content"].is_array());
        let img_url = msgs[1]["content"][1]["image_url"]["url"]
            .as_str()
            .unwrap_or_default();
        assert!(img_url.starts_with("data:image/png;base64,"));

        // Tool call ID preserved
        assert_eq!(msgs[2]["tool_calls"][0]["id"], "call_123");
        assert_eq!(msgs[3]["tool_call_id"], "call_123");
        Ok(())
    }
}

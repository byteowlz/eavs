//! OpenAI Responses API transformer.
//!
//! Handles the newer OpenAI Responses API format used by:
//! - OpenAI /v1/responses endpoint
//! - ChatGPT backend /codex/responses (via OAuth)
//!
//! The Responses API has a different structure from chat completions:
//! - Uses `input` array instead of `messages`
//! - Uses `instructions` for system prompt
//! - Has different streaming event types (response.output_item.added, etc.)
//! - Supports reasoning/thinking natively

use serde_json::{json, Value};

use super::{RequestTransformer, ResponseTransformer, TransformError};
use crate::types::{
    ApiType, AssistantMessage, ContentBlock, Context, Message, StopReason, StreamEvent,
    StreamState, TextContent, ThinkingContent, Tool, ToolCall, Usage,
};

/// Transformer for OpenAI Responses API format.
#[derive(Debug, Clone, Default)]
pub struct OpenAIResponsesTransformer {
    /// API version for headers
    pub api_version: String,
}

impl OpenAIResponsesTransformer {
    pub fn new() -> Self {
        Self {
            api_version: "2024-12-01".to_string(),
        }
    }

    /// Check if a model ID indicates a Codex model (uses ChatGPT backend)
    pub fn is_codex_model(model: &str) -> bool {
        let lower = model.to_lowercase();
        lower.contains("codex") || lower.contains("gpt-5")
    }
}

impl RequestTransformer for OpenAIResponsesTransformer {
    fn transform_request(&self, context: &Context) -> Result<Value, TransformError> {
        let mut input: Vec<Value> = Vec::new();

        // Convert messages to input items
        for msg in &context.messages {
            match msg {
                Message::User(user_msg) => {
                    let content_value = user_content_to_responses_format(&user_msg.content);
                    input.push(json!({
                        "type": "message",
                        "role": "user",
                        "content": content_value
                    }));
                }
                Message::Assistant(assistant_msg) => {
                    // Process content blocks
                    for block in &assistant_msg.content {
                        match block {
                            ContentBlock::Text(text_content) => {
                                input.push(json!({
                                    "type": "message",
                                    "role": "assistant",
                                    "content": [{"type": "output_text", "text": text_content.text}]
                                }));
                            }
                            ContentBlock::Thinking(thinking_content) => {
                                // Reasoning items
                                input.push(json!({
                                    "type": "reasoning",
                                    "summary": [{"type": "summary_text", "text": thinking_content.thinking}]
                                }));
                            }
                            ContentBlock::ToolCall(tool_call) => {
                                input.push(json!({
                                    "type": "function_call",
                                    "call_id": tool_call.id,
                                    "name": tool_call.name,
                                    "arguments": serde_json::to_string(&tool_call.arguments).unwrap_or_default()
                                }));
                            }
                            _ => {}
                        }
                    }
                }
                Message::Tool(tool_result) => {
                    let output = tool_result
                        .content
                        .iter()
                        .filter_map(|b| match b {
                            ContentBlock::Text(t) => Some(t.text.clone()),
                            _ => None,
                        })
                        .collect::<Vec<_>>()
                        .join("\n");

                    input.push(json!({
                        "type": "function_call_output",
                        "call_id": tool_result.tool_call_id,
                        "output": output
                    }));
                }
                Message::System(_) => {
                    // System messages are handled via instructions field
                }
            }
        }

        let mut body = json!({
            "model": &context.model,
            "input": input,
            "stream": context.stream,
            "store": false,
        });

        // Add instructions (system prompt)
        if let Some(ref system) = context.system_prompt {
            body["instructions"] = json!(system);
        }

        // Add tools if present
        if let Some(ref tools) = context.tools {
            let tools_value: Vec<Value> = tools.iter().map(tool_to_responses_format).collect();
            body["tools"] = json!(tools_value);
        }

        // Max tokens
        if let Some(max_tokens) = context.max_tokens {
            body["max_output_tokens"] = json!(max_tokens);
        }

        // Temperature
        if let Some(temp) = context.temperature {
            body["temperature"] = json!(temp);
        }

        Ok(body)
    }

    fn headers(&self, api_key: &str) -> Vec<(String, String)> {
        vec![
            ("Content-Type".to_string(), "application/json".to_string()),
            ("Authorization".to_string(), format!("Bearer {}", api_key)),
        ]
    }

    fn endpoint_path(&self, _context: &Context) -> String {
        "/responses".to_string()
    }
}

impl ResponseTransformer for OpenAIResponsesTransformer {
    fn parse_stream_chunk(
        &self,
        chunk: &str,
        state: &mut StreamState,
    ) -> Result<Vec<StreamEvent>, TransformError> {
        let mut events = Vec::new();

        for line in chunk.lines() {
            let line = line.trim();
            if line.is_empty() || line == "event: message" {
                continue;
            }

            if let Some(data) = line.strip_prefix("data: ") {
                if data == "[DONE]" {
                    events.push(StreamEvent::Done {
                        reason: state.message.stop_reason.clone(),
                        message: state.message.clone(),
                    });
                    continue;
                }

                let parsed: Value = serde_json::from_str(data)
                    .map_err(|e| TransformError::InvalidJson(e.to_string()))?;

                let event_type = parsed["type"].as_str().unwrap_or("");
                let content_index = state.content_blocks.len();

                match event_type {
                    "response.output_item.added" => {
                        let item = &parsed["item"];
                        let item_type = item["type"].as_str().unwrap_or("");

                        match item_type {
                            "reasoning" => {
                                state.content_blocks.push(crate::types::ContentBlockState {
                                    block_type: crate::types::ContentBlockType::Thinking,
                                    text: String::new(),
                                    tool_id: None,
                                    tool_name: None,
                                });
                                events.push(StreamEvent::ThinkingStart { content_index });
                            }
                            "message" => {
                                state.content_blocks.push(crate::types::ContentBlockState {
                                    block_type: crate::types::ContentBlockType::Text,
                                    text: String::new(),
                                    tool_id: None,
                                    tool_name: None,
                                });
                                events.push(StreamEvent::TextStart { content_index });
                            }
                            "function_call" => {
                                let id = item["call_id"].as_str().unwrap_or("").to_string();
                                let name = item["name"].as_str().unwrap_or("").to_string();
                                state.content_blocks.push(crate::types::ContentBlockState {
                                    block_type: crate::types::ContentBlockType::ToolCall,
                                    text: String::new(),
                                    tool_id: Some(id.clone()),
                                    tool_name: Some(name.clone()),
                                });
                                events.push(StreamEvent::ToolCallStart {
                                    content_index,
                                    id,
                                    name,
                                });
                            }
                            _ => {}
                        }
                    }
                    "response.reasoning_summary_text.delta" | "response.reasoning.delta" => {
                        if let Some(delta) = parsed["delta"].as_str() {
                            let idx = state
                                .content_blocks
                                .iter()
                                .rposition(|b| {
                                    b.block_type == crate::types::ContentBlockType::Thinking
                                })
                                .unwrap_or(0);
                            if let Some(block) = state.content_blocks.get_mut(idx) {
                                block.text.push_str(delta);
                            }
                            events.push(StreamEvent::ThinkingDelta {
                                content_index: idx,
                                delta: delta.to_string(),
                            });
                        }
                    }
                    "response.output_text.delta" | "response.text.delta" => {
                        if let Some(delta) = parsed["delta"].as_str() {
                            let idx = state
                                .content_blocks
                                .iter()
                                .rposition(|b| b.block_type == crate::types::ContentBlockType::Text)
                                .unwrap_or(0);
                            if let Some(block) = state.content_blocks.get_mut(idx) {
                                block.text.push_str(delta);
                            }
                            events.push(StreamEvent::TextDelta {
                                content_index: idx,
                                delta: delta.to_string(),
                            });
                        }
                    }
                    "response.refusal.delta" => {
                        if let Some(delta) = parsed["delta"].as_str() {
                            let idx = state
                                .content_blocks
                                .iter()
                                .rposition(|b| b.block_type == crate::types::ContentBlockType::Text)
                                .unwrap_or(0);
                            if let Some(block) = state.content_blocks.get_mut(idx) {
                                block.text.push_str(delta);
                            }
                            events.push(StreamEvent::TextDelta {
                                content_index: idx,
                                delta: delta.to_string(),
                            });
                        }
                    }
                    "response.function_call_arguments.delta" => {
                        if let Some(delta) = parsed["delta"].as_str() {
                            let idx = state
                                .content_blocks
                                .iter()
                                .rposition(|b| {
                                    b.block_type == crate::types::ContentBlockType::ToolCall
                                })
                                .unwrap_or(0);
                            if let Some(block) = state.content_blocks.get_mut(idx) {
                                block.text.push_str(delta);
                            }
                            events.push(StreamEvent::ToolCallDelta {
                                content_index: idx,
                                delta: delta.to_string(),
                            });
                        }
                    }
                    "response.output_item.done" => {
                        let item = &parsed["item"];
                        let item_type = item["type"].as_str().unwrap_or("");

                        match item_type {
                            "reasoning" => {
                                let idx = state
                                    .content_blocks
                                    .iter()
                                    .rposition(|b| {
                                        b.block_type == crate::types::ContentBlockType::Thinking
                                    })
                                    .unwrap_or(0);
                                let text = state
                                    .content_blocks
                                    .get(idx)
                                    .map(|b| b.text.clone())
                                    .unwrap_or_default();

                                // Add to message
                                state
                                    .message
                                    .content
                                    .push(ContentBlock::Thinking(ThinkingContent::new(&text)));

                                events.push(StreamEvent::ThinkingEnd {
                                    content_index: idx,
                                    content: text,
                                    signature: None,
                                });
                            }
                            "message" => {
                                let idx = state
                                    .content_blocks
                                    .iter()
                                    .rposition(|b| {
                                        b.block_type == crate::types::ContentBlockType::Text
                                    })
                                    .unwrap_or(0);
                                let text = state
                                    .content_blocks
                                    .get(idx)
                                    .map(|b| b.text.clone())
                                    .unwrap_or_default();

                                // Add to message
                                state
                                    .message
                                    .content
                                    .push(ContentBlock::Text(TextContent::new(&text)));

                                events.push(StreamEvent::TextEnd {
                                    content_index: idx,
                                    content: text,
                                });
                            }
                            "function_call" => {
                                let idx = state
                                    .content_blocks
                                    .iter()
                                    .rposition(|b| {
                                        b.block_type == crate::types::ContentBlockType::ToolCall
                                    })
                                    .unwrap_or(0);

                                let block = state.content_blocks.get(idx);
                                let id = item["call_id"]
                                    .as_str()
                                    .map(String::from)
                                    .or_else(|| block.and_then(|b| b.tool_id.clone()))
                                    .unwrap_or_default();
                                let name = item["name"]
                                    .as_str()
                                    .map(String::from)
                                    .or_else(|| block.and_then(|b| b.tool_name.clone()))
                                    .unwrap_or_default();
                                let args_str = item["arguments"]
                                    .as_str()
                                    .map(String::from)
                                    .or_else(|| block.map(|b| b.text.clone()))
                                    .unwrap_or_default();
                                let arguments: Value =
                                    serde_json::from_str(&args_str).unwrap_or(json!({}));

                                let tool_call = ToolCall {
                                    id,
                                    name,
                                    arguments,
                                };

                                // Add to message
                                state
                                    .message
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
                    "response.completed" | "response.done" => {
                        if let Some(response) = parsed.get("response") {
                            // Extract usage
                            if let Some(usage) = response.get("usage") {
                                let input_tokens =
                                    usage["input_tokens"].as_u64().unwrap_or(0) as u32;
                                let output_tokens =
                                    usage["output_tokens"].as_u64().unwrap_or(0) as u32;
                                let cached_tokens = usage["input_tokens_details"]["cached_tokens"]
                                    .as_u64()
                                    .unwrap_or(0)
                                    as u32;

                                state.message.usage = Usage {
                                    prompt_tokens: input_tokens.saturating_sub(cached_tokens),
                                    completion_tokens: output_tokens,
                                    total_tokens: input_tokens + output_tokens,
                                    cache_read_input_tokens: if cached_tokens > 0 {
                                        Some(cached_tokens)
                                    } else {
                                        None
                                    },
                                    cache_creation_input_tokens: None,
                                };
                            }

                            // Extract stop reason
                            let status = response["status"].as_str().unwrap_or("completed");
                            state.message.stop_reason = match status {
                                "completed" => StopReason::EndTurn,
                                "incomplete" | "cancelled" => StopReason::MaxTokens,
                                "failed" => StopReason::Other,
                                _ => StopReason::EndTurn,
                            };
                        }

                        state.message.api = ApiType::OpenAIResponses;

                        events.push(StreamEvent::Done {
                            reason: state.message.stop_reason.clone(),
                            message: state.message.clone(),
                        });
                    }
                    "error" => {
                        let message = parsed["message"].as_str().unwrap_or("Unknown error");
                        return Err(TransformError::InvalidValue(message.to_string()));
                    }
                    "response.failed" => {
                        return Err(TransformError::InvalidValue("Response failed".to_string()));
                    }
                    _ => {
                        // Unknown event type, ignore
                    }
                }
            }
        }

        Ok(events)
    }

    fn parse_response(&self, body: &Value) -> Result<Vec<StreamEvent>, TransformError> {
        let mut events = Vec::new();
        let mut message = AssistantMessage {
            api: ApiType::OpenAIResponses,
            ..Default::default()
        };

        // Parse output items
        if let Some(output) = body.get("output").and_then(|o| o.as_array()) {
            for (idx, item) in output.iter().enumerate() {
                let item_type = item["type"].as_str().unwrap_or("");

                match item_type {
                    "message" => {
                        if let Some(content) = item.get("content").and_then(|c| c.as_array()) {
                            for part in content {
                                let part_type = part["type"].as_str().unwrap_or("");
                                if part_type == "output_text" {
                                    if let Some(text) = part["text"].as_str() {
                                        message
                                            .content
                                            .push(ContentBlock::Text(TextContent::new(text)));
                                        events.push(StreamEvent::TextEnd {
                                            content_index: idx,
                                            content: text.to_string(),
                                        });
                                    }
                                }
                            }
                        }
                    }
                    "function_call" => {
                        let id = item["call_id"].as_str().unwrap_or("").to_string();
                        let name = item["name"].as_str().unwrap_or("").to_string();
                        let args_str = item["arguments"].as_str().unwrap_or("{}");
                        let arguments: Value = serde_json::from_str(args_str).unwrap_or(json!({}));

                        let tool_call = ToolCall {
                            id,
                            name,
                            arguments,
                        };
                        message
                            .content
                            .push(ContentBlock::ToolCall(tool_call.clone()));
                        events.push(StreamEvent::ToolCallEnd {
                            content_index: idx,
                            tool_call,
                        });
                    }
                    "reasoning" => {
                        if let Some(summary) = item.get("summary").and_then(|s| s.as_array()) {
                            let text: String = summary
                                .iter()
                                .filter_map(|s| s["text"].as_str())
                                .collect::<Vec<_>>()
                                .join("\n\n");
                            if !text.is_empty() {
                                message
                                    .content
                                    .push(ContentBlock::Thinking(ThinkingContent::new(&text)));
                                events.push(StreamEvent::ThinkingEnd {
                                    content_index: idx,
                                    content: text,
                                    signature: None,
                                });
                            }
                        }
                    }
                    _ => {}
                }
            }
        }

        // Extract usage
        if let Some(usage_obj) = body.get("usage") {
            let input_tokens = usage_obj["input_tokens"].as_u64().unwrap_or(0) as u32;
            let output_tokens = usage_obj["output_tokens"].as_u64().unwrap_or(0) as u32;
            let cached_tokens = usage_obj["input_tokens_details"]["cached_tokens"]
                .as_u64()
                .unwrap_or(0) as u32;

            message.usage = Usage {
                prompt_tokens: input_tokens.saturating_sub(cached_tokens),
                completion_tokens: output_tokens,
                total_tokens: input_tokens + output_tokens,
                cache_read_input_tokens: if cached_tokens > 0 {
                    Some(cached_tokens)
                } else {
                    None
                },
                cache_creation_input_tokens: None,
            };
        }

        // Determine stop reason
        let status = body["status"].as_str().unwrap_or("completed");
        message.stop_reason = match status {
            "completed" => StopReason::EndTurn,
            "incomplete" | "cancelled" => StopReason::MaxTokens,
            "failed" => StopReason::Other,
            _ => StopReason::EndTurn,
        };

        // Check for tool use
        if message
            .content
            .iter()
            .any(|c| matches!(c, ContentBlock::ToolCall(_)))
        {
            message.stop_reason = StopReason::ToolUse;
        }

        events.push(StreamEvent::Done {
            reason: message.stop_reason.clone(),
            message,
        });

        Ok(events)
    }
}

/// Convert user content blocks to Responses API format
fn user_content_to_responses_format(content: &[ContentBlock]) -> Value {
    let parts: Vec<Value> = content
        .iter()
        .filter_map(|block| match block {
            ContentBlock::Text(t) => Some(json!({"type": "input_text", "text": t.text})),
            ContentBlock::Image(img) => {
                if img.is_url {
                    Some(json!({
                        "type": "input_image",
                        "image_url": img.data,
                        "detail": "auto"
                    }))
                } else {
                    Some(json!({
                        "type": "input_image",
                        "image_url": format!("data:{};base64,{}", img.mime_type, img.data),
                        "detail": "auto"
                    }))
                }
            }
            _ => None,
        })
        .collect();
    json!(parts)
}

/// Convert Tool definition to Responses API format
fn tool_to_responses_format(tool: &Tool) -> Value {
    json!({
        "type": "function",
        "name": tool.name,
        "description": tool.description,
        "parameters": tool.parameters
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_transform_request_basic() {
        let transformer = OpenAIResponsesTransformer::new();
        let ctx = Context::new("gpt-5.1-codex")
            .with_messages(vec![Message::user("Hello")])
            .with_system("Be helpful".to_string());

        let request = transformer.transform_request(&ctx).unwrap();

        assert_eq!(request["model"], "gpt-5.1-codex");
        assert_eq!(request["instructions"], "Be helpful");
        assert!(request["input"].is_array());
    }

    #[test]
    fn test_is_codex_model() {
        assert!(OpenAIResponsesTransformer::is_codex_model("gpt-5.1-codex"));
        assert!(OpenAIResponsesTransformer::is_codex_model(
            "gpt-5.2-codex-max"
        ));
        assert!(OpenAIResponsesTransformer::is_codex_model("gpt-5.1"));
        assert!(!OpenAIResponsesTransformer::is_codex_model("gpt-4o"));
        assert!(!OpenAIResponsesTransformer::is_codex_model("claude-3-opus"));
    }

    #[test]
    fn test_endpoint_path() {
        let transformer = OpenAIResponsesTransformer::new();
        let ctx = Context::new("gpt-5.1-codex");
        assert_eq!(transformer.endpoint_path(&ctx), "/responses");
    }

    #[test]
    fn test_parse_text_delta() {
        let transformer = OpenAIResponsesTransformer::new();
        let mut state = StreamState::default();
        // First add a text block
        state.content_blocks.push(crate::types::ContentBlockState {
            block_type: crate::types::ContentBlockType::Text,
            text: String::new(),
            tool_id: None,
            tool_name: None,
        });

        let chunk = r#"data: {"type":"response.output_text.delta","delta":"Hello"}"#;
        let events = transformer.parse_stream_chunk(chunk, &mut state).unwrap();

        assert_eq!(events.len(), 1);
        match &events[0] {
            StreamEvent::TextDelta { delta, .. } => assert_eq!(delta, "Hello"),
            _ => panic!("Expected TextDelta"),
        }
    }

    #[test]
    fn test_parse_completed() {
        let transformer = OpenAIResponsesTransformer::new();
        let mut state = StreamState::default();

        let chunk = r#"data: {"type":"response.completed","response":{"status":"completed","usage":{"input_tokens":10,"output_tokens":20}}}"#;
        let events = transformer.parse_stream_chunk(chunk, &mut state).unwrap();

        assert_eq!(events.len(), 1);
        match &events[0] {
            StreamEvent::Done { reason, message } => {
                assert_eq!(*reason, StopReason::EndTurn);
                assert_eq!(message.usage.prompt_tokens, 10);
                assert_eq!(message.usage.completion_tokens, 20);
            }
            _ => panic!("Expected Done"),
        }
    }
}

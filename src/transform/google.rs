//! Google Generative AI (Gemini) transformer.
//!
//! Handles transformation between canonical format and Google's GenerateContent API.

use crate::transform::{RequestTransformer, ResponseTransformer, TransformError};
use crate::types::{
    ApiType, AssistantMessage, ContentBlock, ContentBlockState, ContentBlockType, Context,
    ImageContent, Message, StopReason, StreamEvent, StreamState, TextContent, ThinkingContent,
    Tool, ToolCall, Usage,
};
use serde_json::{json, Value};

/// Google GenerativeAI transformer.
#[derive(Debug, Clone, Default)]
pub struct GoogleTransformer {
    /// Whether to use API key in query param vs header
    pub api_key_in_query: bool,
}

impl GoogleTransformer {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_api_key_in_query(mut self, in_query: bool) -> Self {
        self.api_key_in_query = in_query;
        self
    }
}

impl RequestTransformer for GoogleTransformer {
    fn transform_request(&self, context: &Context) -> Result<Value, TransformError> {
        let mut request = json!({});

        // Add system instruction
        if let Some(ref system) = context.system_prompt {
            request["systemInstruction"] = json!({
                "parts": [{"text": system}]
            });
        }

        // Build contents
        let contents = build_google_contents(context)?;
        request["contents"] = Value::Array(contents);

        // Add generation config
        let mut gen_config = json!({});

        if let Some(max_tokens) = context.max_tokens {
            gen_config["maxOutputTokens"] = json!(max_tokens);
        }

        if let Some(temp) = context.temperature {
            gen_config["temperature"] = json!(temp);
        }

        if let Some(top_p) = context.top_p {
            gen_config["topP"] = json!(top_p);
        }

        if let Some(ref stop) = context.stop {
            gen_config["stopSequences"] = json!(stop);
        }

        if gen_config
            .as_object()
            .map(|o| !o.is_empty())
            .unwrap_or(false)
        {
            request["generationConfig"] = gen_config;
        }

        // Add tools
        if let Some(ref tools) = context.tools {
            let function_declarations: Vec<Value> = tools.iter().map(tool_to_google).collect();
            request["tools"] = json!([{
                "functionDeclarations": function_declarations
            }]);
        }

        Ok(request)
    }

    fn headers(&self, api_key: &str) -> Vec<(String, String)> {
        let mut headers = vec![("Content-Type".to_string(), "application/json".to_string())];
        if !self.api_key_in_query && !api_key.is_empty() {
            headers.push(("Authorization".to_string(), format!("Bearer {}", api_key)));
        }
        headers
    }

    fn endpoint_path(&self, context: &Context) -> String {
        let action = if context.stream {
            "streamGenerateContent"
        } else {
            "generateContent"
        };
        format!("/models/{}:{}", context.model, action)
    }
}

impl ResponseTransformer for GoogleTransformer {
    fn parse_stream_chunk(
        &self,
        chunk: &str,
        state: &mut StreamState,
    ) -> Result<Vec<StreamEvent>, TransformError> {
        let mut events = Vec::new();
        let chunk = chunk.trim();

        // Skip empty chunks
        if chunk.is_empty() {
            return Ok(events);
        }

        // Google streams JSON objects, sometimes with array wrapper
        let parsed: Value = if chunk.starts_with('[') {
            // Array of chunks
            serde_json::from_str(chunk).map_err(|e| TransformError::InvalidJson(e.to_string()))?
        } else if chunk.starts_with('{') {
            // Single chunk
            json!([serde_json::from_str::<Value>(chunk)
                .map_err(|e| TransformError::InvalidJson(e.to_string()))?])
        } else {
            return Ok(events);
        };

        // Process each chunk in the array
        if let Some(chunks) = parsed.as_array() {
            for chunk_obj in chunks {
                events.extend(process_google_chunk(chunk_obj, state)?);
            }
        }

        Ok(events)
    }

    fn parse_response(&self, body: &Value) -> Result<Vec<StreamEvent>, TransformError> {
        let mut events = Vec::new();
        let mut state = StreamState::default();

        // Handle as a single chunk
        events.extend(process_google_chunk(body, &mut state)?);

        // Ensure we have a done event
        if !events.iter().any(|e| matches!(e, StreamEvent::Done { .. })) {
            events.push(StreamEvent::Done {
                reason: state.message.stop_reason.clone(),
                message: state.message,
            });
        }

        Ok(events)
    }
}

/// Parse a Google GenerateContent request into canonical Context.
pub fn parse_google_request(body: &Value) -> Result<Context, TransformError> {
    // Model is typically in the URL path, not the body
    let model = body["model"].as_str().unwrap_or("gemini-pro").to_string();
    let mut context = Context::new(model);

    // Parse system instruction
    if let Some(system) = body["systemInstruction"].as_object() {
        if let Some(parts) = system["parts"].as_array() {
            let text: Vec<String> = parts
                .iter()
                .filter_map(|p| p["text"].as_str().map(String::from))
                .collect();
            if !text.is_empty() {
                context.system_prompt = Some(text.join("\n"));
            }
        }
    }

    // Parse contents
    if let Some(contents) = body["contents"].as_array() {
        for content in contents {
            let role = content["role"].as_str().unwrap_or("user");

            match role {
                "user" => {
                    let user_msg = parse_google_user_content(content)?;
                    context.messages.push(Message::User(user_msg));
                }
                "model" => {
                    let assistant_msg = parse_google_model_content(content)?;
                    context.messages.push(Message::Assistant(assistant_msg));
                }
                _ => {}
            }
        }
    }

    // Parse generation config
    if let Some(gen_config) = body["generationConfig"].as_object() {
        context.max_tokens = gen_config
            .get("maxOutputTokens")
            .and_then(|v| v.as_u64())
            .map(|v| v as u32);
        context.temperature = gen_config
            .get("temperature")
            .and_then(|v| v.as_f64())
            .map(|v| v as f32);
        context.top_p = gen_config
            .get("topP")
            .and_then(|v| v.as_f64())
            .map(|v| v as f32);

        if let Some(stop) = gen_config.get("stopSequences").and_then(|v| v.as_array()) {
            context.stop = Some(
                stop.iter()
                    .filter_map(|v| v.as_str().map(String::from))
                    .collect(),
            );
        }
    }

    // Parse tools
    if let Some(tools) = body["tools"].as_array() {
        let mut parsed_tools = Vec::new();
        for tool_group in tools {
            if let Some(declarations) = tool_group["functionDeclarations"].as_array() {
                for decl in declarations {
                    parsed_tools.push(parse_google_tool(decl)?);
                }
            }
        }
        if !parsed_tools.is_empty() {
            context.tools = Some(parsed_tools);
        }
    }

    context.original_request = Some(body.clone());

    Ok(context)
}

/// Build Google SSE response from canonical stream events.
pub fn build_google_sse(event: &StreamEvent, model: &str) -> String {
    match event {
        StreamEvent::Start { .. } => {
            // Google doesn't have a separate start event
            String::new()
        }
        StreamEvent::TextDelta { delta, .. } => {
            let data = json!({
                "candidates": [{
                    "content": {
                        "parts": [{"text": delta}],
                        "role": "model"
                    },
                    "index": 0
                }],
                "modelVersion": model
            });
            format!("{}\n", data)
        }
        StreamEvent::ThinkingDelta { delta, .. } => {
            let data = json!({
                "candidates": [{
                    "content": {
                        "parts": [{"thought": true, "text": delta}],
                        "role": "model"
                    },
                    "index": 0
                }],
                "modelVersion": model
            });
            format!("{}\n", data)
        }
        StreamEvent::ToolCallEnd { tool_call, .. } => {
            let data = json!({
                "candidates": [{
                    "content": {
                        "parts": [{
                            "functionCall": {
                                "name": tool_call.name,
                                "args": tool_call.arguments
                            }
                        }],
                        "role": "model"
                    },
                    "index": 0
                }],
                "modelVersion": model
            });
            format!("{}\n", data)
        }
        StreamEvent::Usage { usage } => {
            let data = json!({
                "usageMetadata": {
                    "promptTokenCount": usage.prompt_tokens,
                    "candidatesTokenCount": usage.completion_tokens,
                    "totalTokenCount": usage.total_tokens
                }
            });
            format!("{}\n", data)
        }
        StreamEvent::Done { reason, message } => {
            let finish_reason = match reason {
                StopReason::EndTurn => "STOP",
                StopReason::MaxTokens => "MAX_TOKENS",
                StopReason::ToolUse => "TOOL_CALLS",
                StopReason::ContentFilter => "SAFETY",
                _ => "STOP",
            };

            let data = json!({
                "candidates": [{
                    "finishReason": finish_reason,
                    "index": 0
                }],
                "usageMetadata": {
                    "promptTokenCount": message.usage.prompt_tokens,
                    "candidatesTokenCount": message.usage.completion_tokens,
                    "totalTokenCount": message.usage.total_tokens
                },
                "modelVersion": model
            });
            format!("{}\n", data)
        }
        StreamEvent::Error { message, .. } => {
            let error_msg = message.error_message.as_deref().unwrap_or("Unknown error");
            let data = json!({
                "error": {
                    "code": 500,
                    "message": error_msg,
                    "status": "INTERNAL"
                }
            });
            format!("{}\n", data)
        }
        _ => String::new(),
    }
}

// ============================================================================
// Internal helpers
// ============================================================================

fn parse_google_user_content(content: &Value) -> Result<crate::types::UserMessage, TransformError> {
    let mut blocks = Vec::new();

    if let Some(parts) = content["parts"].as_array() {
        for part in parts {
            // Text part
            if let Some(text) = part["text"].as_str() {
                blocks.push(ContentBlock::Text(TextContent::new(text)));
            }

            // Inline data (image)
            if let Some(inline_data) = part.get("inlineData") {
                let mime_type = inline_data["mimeType"].as_str().unwrap_or("image/png");
                let data = inline_data["data"].as_str().unwrap_or_default();
                blocks.push(ContentBlock::Image(ImageContent::base64(data, mime_type)));
            }

            // Function response (tool result)
            if let Some(func_response) = part.get("functionResponse") {
                let name = func_response["name"].as_str().unwrap_or_default();
                let response = func_response["response"].clone();
                let response_str = serde_json::to_string(&response).unwrap_or_default();

                blocks.push(ContentBlock::ToolResult(crate::types::ToolResultContent {
                    tool_call_id: name.to_string(), // Google uses function name as ID
                    content: response_str,
                    is_error: false,
                }));
            }
        }
    }

    Ok(crate::types::UserMessage {
        content: blocks,
        timestamp: 0,
    })
}

fn parse_google_model_content(content: &Value) -> Result<AssistantMessage, TransformError> {
    let mut blocks = Vec::new();

    if let Some(parts) = content["parts"].as_array() {
        for part in parts {
            // Check if this is a thought part
            let is_thought = part["thought"].as_bool().unwrap_or(false);

            // Text part
            if let Some(text) = part["text"].as_str() {
                if is_thought {
                    let signature = part["thoughtSignature"].as_str().map(String::from);
                    if let Some(sig) = signature {
                        blocks.push(ContentBlock::Thinking(ThinkingContent::with_signature(
                            text, sig,
                        )));
                    } else {
                        blocks.push(ContentBlock::Thinking(ThinkingContent::new(text)));
                    }
                } else {
                    blocks.push(ContentBlock::Text(TextContent::new(text)));
                }
            }

            // Function call
            if let Some(func_call) = part.get("functionCall") {
                let name = func_call["name"].as_str().unwrap_or_default();
                let args = func_call["args"].clone();

                blocks.push(ContentBlock::ToolCall(ToolCall::new(
                    name, // Google uses function name as ID
                    name, args,
                )));
            }
        }
    }

    Ok(AssistantMessage {
        content: blocks,
        api: ApiType::GoogleGenerativeAI,
        ..Default::default()
    })
}

fn parse_google_tool(decl: &Value) -> Result<Tool, TransformError> {
    let name = decl["name"]
        .as_str()
        .ok_or_else(|| TransformError::MissingField("name".to_string()))?
        .to_string();
    let description = decl["description"].as_str().unwrap_or_default().to_string();
    let parameters = decl["parameters"].clone();

    Ok(Tool::new(name, description, parameters))
}

fn tool_to_google(tool: &Tool) -> Value {
    json!({
        "name": tool.name,
        "description": tool.description,
        "parameters": tool.parameters
    })
}

fn build_google_contents(context: &Context) -> Result<Vec<Value>, TransformError> {
    let mut contents = Vec::new();

    for msg in &context.messages {
        match msg {
            Message::User(user) => {
                let parts = build_google_parts(&user.content)?;
                contents.push(json!({
                    "role": "user",
                    "parts": parts
                }));
            }
            Message::Assistant(assistant) => {
                let parts = build_google_model_parts(&assistant.content);
                contents.push(json!({
                    "role": "model",
                    "parts": parts
                }));
            }
            Message::Tool(tool_result) => {
                // Tool results go in user messages as functionResponse
                let content_text: String = tool_result
                    .content
                    .iter()
                    .map(|b| match b {
                        ContentBlock::Text(t) => t.text.clone(),
                        _ => String::new(),
                    })
                    .collect();

                let response: Value =
                    serde_json::from_str(&content_text).unwrap_or(json!({"result": content_text}));

                contents.push(json!({
                    "role": "user",
                    "parts": [{
                        "functionResponse": {
                            "name": tool_result.tool_name,
                            "response": response
                        }
                    }]
                }));
            }
            Message::System(_) => {
                // System handled separately
            }
        }
    }

    Ok(contents)
}

fn build_google_parts(blocks: &[ContentBlock]) -> Result<Vec<Value>, TransformError> {
    let mut parts = Vec::new();
    for block in blocks {
        match block {
            ContentBlock::Text(t) => parts.push(json!({"text": t.text})),
            ContentBlock::Image(img) => {
                if img.is_url {
                    return Err(TransformError::Unsupported(
                        "Google requires inlineData for images; provide a data: URL or base64"
                            .to_string(),
                    ));
                }
                parts.push(json!({
                    "inlineData": {
                        "mimeType": img.mime_type,
                        "data": img.data
                    }
                }));
            }
            ContentBlock::ToolResult(tr) => {
                let response: Value =
                    serde_json::from_str(&tr.content).unwrap_or(json!({"result": tr.content}));
                parts.push(json!({
                    "functionResponse": {
                        "name": tr.tool_call_id,
                        "response": response
                    }
                }));
            }
            _ => {}
        }
    }
    Ok(parts)
}

fn build_google_model_parts(blocks: &[ContentBlock]) -> Vec<Value> {
    blocks
        .iter()
        .filter_map(|block| match block {
            ContentBlock::Text(t) => Some(json!({"text": t.text})),
            ContentBlock::Thinking(t) => {
                let mut part = json!({
                    "thought": true,
                    "text": t.thinking
                });
                if let Some(ref sig) = t.signature {
                    part["thoughtSignature"] = json!(sig);
                }
                Some(part)
            }
            ContentBlock::ToolCall(tc) => Some(json!({
                "functionCall": {
                    "name": tc.name,
                    "args": tc.arguments
                }
            })),
            _ => None,
        })
        .collect()
}

fn process_google_chunk(
    chunk: &Value,
    state: &mut StreamState,
) -> Result<Vec<StreamEvent>, TransformError> {
    let mut events = Vec::new();

    // Handle start
    if !state.started {
        state.started = true;
        state.message.api = ApiType::GoogleGenerativeAI;

        if let Some(model) = chunk["modelVersion"].as_str() {
            state.message.model = model.to_string();
        }

        events.push(StreamEvent::Start {
            partial: state.message.clone(),
        });
    }

    // Parse candidates
    if let Some(candidates) = chunk["candidates"].as_array() {
        for candidate in candidates {
            // Check finish reason
            if let Some(finish_reason) = candidate["finishReason"].as_str() {
                state.message.stop_reason = parse_finish_reason(finish_reason);
            }

            // Parse content parts
            if let Some(content) = candidate.get("content") {
                if let Some(parts) = content["parts"].as_array() {
                    for part in parts {
                        let is_thought = part["thought"].as_bool().unwrap_or(false);

                        // Text/thinking delta
                        if let Some(text) = part["text"].as_str() {
                            // Get or create content block
                            let idx = if is_thought {
                                get_or_create_block(state, ContentBlockType::Thinking)
                            } else {
                                get_or_create_block(state, ContentBlockType::Text)
                            };

                            state.content_blocks[idx].text.push_str(text);

                            if is_thought {
                                events.push(StreamEvent::ThinkingDelta {
                                    content_index: idx,
                                    delta: text.to_string(),
                                });
                            } else {
                                events.push(StreamEvent::TextDelta {
                                    content_index: idx,
                                    delta: text.to_string(),
                                });
                            }
                        }

                        // Function call
                        if let Some(func_call) = part.get("functionCall") {
                            let name = func_call["name"].as_str().unwrap_or_default();
                            let args = func_call["args"].clone();

                            let idx = state.content_blocks.len();
                            state.content_blocks.push(ContentBlockState {
                                block_type: ContentBlockType::ToolCall,
                                text: serde_json::to_string(&args).unwrap_or_default(),
                                tool_id: Some(name.to_string()),
                                tool_name: Some(name.to_string()),
                            });

                            let tool_call = ToolCall::new(name, name, args);
                            state
                                .message
                                .content
                                .push(ContentBlock::ToolCall(tool_call.clone()));

                            events.push(StreamEvent::ToolCallStart {
                                content_index: idx,
                                id: name.to_string(),
                                name: name.to_string(),
                            });
                            events.push(StreamEvent::ToolCallEnd {
                                content_index: idx,
                                tool_call,
                            });
                        }
                    }
                }
            }
        }
    }

    // Parse usage metadata
    if let Some(usage) = chunk.get("usageMetadata") {
        state.message.usage = Usage {
            prompt_tokens: usage["promptTokenCount"].as_u64().unwrap_or(0) as u32,
            completion_tokens: usage["candidatesTokenCount"].as_u64().unwrap_or(0) as u32,
            total_tokens: usage["totalTokenCount"].as_u64().unwrap_or(0) as u32,
            ..Default::default()
        };
        events.push(StreamEvent::Usage {
            usage: state.message.usage.clone(),
        });
    }

    // Handle finish
    if chunk["candidates"]
        .as_array()
        .and_then(|c| c.first())
        .and_then(|c| c["finishReason"].as_str())
        .is_some()
    {
        // Finalize content blocks
        for (idx, block) in state.content_blocks.iter().enumerate() {
            match block.block_type {
                ContentBlockType::Text => {
                    state
                        .message
                        .content
                        .push(ContentBlock::Text(TextContent::new(&block.text)));
                    events.push(StreamEvent::TextEnd {
                        content_index: idx,
                        content: block.text.clone(),
                    });
                }
                ContentBlockType::Thinking => {
                    state
                        .message
                        .content
                        .push(ContentBlock::Thinking(ThinkingContent::new(&block.text)));
                    events.push(StreamEvent::ThinkingEnd {
                        content_index: idx,
                        content: block.text.clone(),
                        signature: None,
                    });
                }
                ContentBlockType::ToolCall => {
                    // Already handled above
                }
            }
        }

        events.push(StreamEvent::Done {
            reason: state.message.stop_reason.clone(),
            message: state.message.clone(),
        });
    }

    Ok(events)
}

fn get_or_create_block(state: &mut StreamState, block_type: ContentBlockType) -> usize {
    // Find existing block of same type
    for (idx, block) in state.content_blocks.iter().enumerate() {
        if block.block_type == block_type {
            return idx;
        }
    }

    // Create new block
    let idx = state.content_blocks.len();
    state.content_blocks.push(ContentBlockState {
        block_type,
        text: String::new(),
        tool_id: None,
        tool_name: None,
    });
    idx
}

fn parse_finish_reason(reason: &str) -> StopReason {
    match reason {
        "STOP" => StopReason::EndTurn,
        "MAX_TOKENS" => StopReason::MaxTokens,
        "SAFETY" => StopReason::ContentFilter,
        "RECITATION" => StopReason::ContentFilter,
        "TOOL_CALLS" => StopReason::ToolUse,
        _ => StopReason::Other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::ToolResultMessage;

    #[test]
    fn test_parse_google_request_simple() {
        let body = json!({
            "model": "gemini-pro",
            "systemInstruction": {
                "parts": [{"text": "You are helpful"}]
            },
            "contents": [
                {"role": "user", "parts": [{"text": "Hello"}]}
            ],
            "generationConfig": {
                "maxOutputTokens": 1024,
                "temperature": 0.7
            }
        });

        let ctx = parse_google_request(&body).unwrap();
        assert_eq!(ctx.model, "gemini-pro");
        assert_eq!(ctx.system_prompt, Some("You are helpful".to_string()));
        assert_eq!(ctx.messages.len(), 1);
        assert_eq!(ctx.max_tokens, Some(1024));
    }

    #[test]
    fn test_parse_google_request_with_tools() {
        let body = json!({
            "model": "gemini-pro",
            "contents": [{"role": "user", "parts": [{"text": "Get weather"}]}],
            "tools": [{
                "functionDeclarations": [{
                    "name": "get_weather",
                    "description": "Get current weather",
                    "parameters": {
                        "type": "object",
                        "properties": {"city": {"type": "string"}}
                    }
                }]
            }]
        });

        let ctx = parse_google_request(&body).unwrap();
        assert!(ctx.tools.is_some());
        let tools = ctx.tools.unwrap();
        assert_eq!(tools[0].name, "get_weather");
    }

    #[test]
    fn test_parse_google_request_with_thinking() {
        let body = json!({
            "model": "gemini-2.0-flash-thinking",
            "contents": [
                {"role": "user", "parts": [{"text": "Think about this"}]},
                {
                    "role": "model",
                    "parts": [
                        {"thought": true, "text": "Let me consider...", "thoughtSignature": "sig123"},
                        {"text": "Here's my answer"}
                    ]
                }
            ]
        });

        let ctx = parse_google_request(&body).unwrap();

        if let Message::Assistant(assistant) = &ctx.messages[1] {
            assert_eq!(assistant.content.len(), 2);
            if let ContentBlock::Thinking(t) = &assistant.content[0] {
                assert_eq!(t.thinking, "Let me consider...");
                assert_eq!(t.signature, Some("sig123".to_string()));
            } else {
                panic!("Expected thinking block");
            }
        } else {
            panic!("Expected assistant message");
        }
    }

    #[test]
    fn test_google_transformer_transform_request() {
        let transformer = GoogleTransformer::new();
        let ctx = Context::new("gemini-pro")
            .with_system("Be helpful")
            .with_messages(vec![Message::user("Hi")])
            .with_max_tokens(1000);

        let request = transformer.transform_request(&ctx).unwrap();

        assert!(request["systemInstruction"]["parts"][0]["text"] == "Be helpful");
        assert_eq!(request["generationConfig"]["maxOutputTokens"], 1000);
    }

    #[test]
    fn test_google_transformer_endpoint_path() {
        let transformer = GoogleTransformer::new();

        let ctx_stream = Context::new("gemini-pro").with_stream(true);
        assert!(transformer
            .endpoint_path(&ctx_stream)
            .contains("streamGenerateContent"));

        let ctx_normal = Context::new("gemini-pro");
        assert!(transformer
            .endpoint_path(&ctx_normal)
            .contains("generateContent"));
    }

    #[test]
    fn test_build_google_sse_text_delta() {
        let event = StreamEvent::TextDelta {
            content_index: 0,
            delta: "Hello".to_string(),
        };

        let sse = build_google_sse(&event, "gemini-pro");
        assert!(sse.contains("\"text\":\"Hello\""));
    }

    #[test]
    fn test_build_google_sse_done() {
        let event = StreamEvent::Done {
            reason: StopReason::EndTurn,
            message: AssistantMessage::default(),
        };

        let sse = build_google_sse(&event, "gemini-pro");
        assert!(sse.contains("\"finishReason\":\"STOP\""));
    }

    #[test]
    fn test_parse_stream_chunk() {
        let transformer = GoogleTransformer::new();
        let mut state = StreamState::default();

        let chunk = json!({
            "candidates": [{
                "content": {
                    "parts": [{"text": "Hello"}],
                    "role": "model"
                },
                "index": 0
            }],
            "modelVersion": "gemini-pro"
        });

        let events = transformer
            .parse_stream_chunk(&chunk.to_string(), &mut state)
            .unwrap();

        assert!(events
            .iter()
            .any(|e| matches!(e, StreamEvent::Start { .. })));
        assert!(events
            .iter()
            .any(|e| matches!(e, StreamEvent::TextDelta { delta, .. } if delta == "Hello")));
    }

    #[test]
    fn test_parse_response_non_streaming() {
        let transformer = GoogleTransformer::new();
        let body = json!({
            "candidates": [{
                "content": {
                    "parts": [{"text": "Hello!"}],
                    "role": "model"
                },
                "finishReason": "STOP",
                "index": 0
            }],
            "usageMetadata": {
                "promptTokenCount": 10,
                "candidatesTokenCount": 5,
                "totalTokenCount": 15
            },
            "modelVersion": "gemini-pro"
        });

        let events = transformer.parse_response(&body).unwrap();

        assert!(events
            .iter()
            .any(|e| matches!(e, StreamEvent::TextDelta { delta, .. } if delta == "Hello!")));
        assert!(events.iter().any(|e| matches!(
            e,
            StreamEvent::Done {
                reason: StopReason::EndTurn,
                ..
            }
        )));
    }

    #[test]
    fn test_tool_to_google() {
        let tool = Tool::new(
            "get_weather",
            "Get weather",
            json!({"type": "object", "properties": {"city": {"type": "string"}}}),
        );

        let google_tool = tool_to_google(&tool);
        assert_eq!(google_tool["name"], "get_weather");
        assert!(google_tool["parameters"].is_object());
    }

    #[test]
    fn test_build_google_contents_with_tool_result() {
        let ctx = Context::new("gemini-pro").with_messages(vec![
            Message::user("Get weather"),
            Message::Assistant(AssistantMessage {
                content: vec![ContentBlock::ToolCall(ToolCall::new(
                    "get_weather",
                    "get_weather",
                    json!({"city": "NYC"}),
                ))],
                ..Default::default()
            }),
            Message::Tool(ToolResultMessage::text(
                "get_weather",
                "get_weather",
                "Sunny, 72F",
            )),
        ]);

        let contents = build_google_contents(&ctx).unwrap();

        assert_eq!(contents.len(), 3);
        // Tool result should be in a user message with functionResponse
        assert_eq!(contents[2]["role"], "user");
        assert!(contents[2]["parts"][0]["functionResponse"].is_object());
    }

    #[test]
    fn test_build_google_model_parts_with_thinking() {
        let blocks = vec![
            ContentBlock::Thinking(ThinkingContent::with_signature("Thinking...", "sig123")),
            ContentBlock::Text(TextContent::new("Answer")),
        ];

        let parts = build_google_model_parts(&blocks);

        assert_eq!(parts.len(), 2);
        assert_eq!(parts[0]["thought"], true);
        assert_eq!(parts[0]["thoughtSignature"], "sig123");
        assert_eq!(parts[1]["text"], "Answer");
    }
}

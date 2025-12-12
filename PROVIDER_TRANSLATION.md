# Provider Translation Architecture

This document outlines the architecture for implementing universal provider translation in EAVS, based on analysis of pi-ai's approach.

## Goal

Allow clients to send OpenAI-format requests and have EAVS transparently route them to any provider (Anthropic, Google, etc.) with automatic format translation.

```bash
# Send OpenAI-format request, route to Anthropic
curl http://localhost:3000/v1/chat/completions \
  -H "X-Provider: anthropic" \
  -d '{"model": "claude-sonnet-4-20250514", "messages": [...]}'
```

## Architecture Overview

```
┌─────────────────────────────────────────────────────────────────┐
│                         EAVS Proxy                              │
│                                                                 │
│  ┌──────────────┐    ┌──────────────┐    ┌──────────────┐      │
│  │   OpenAI     │    │  Canonical   │    │   Provider   │      │
│  │   Request    │───▶│   Format     │───▶│  Transform   │      │
│  │   Parser     │    │              │    │  (per API)   │      │
│  └──────────────┘    └──────────────┘    └──────────────┘      │
│                                                 │               │
│                                                 ▼               │
│                                          ┌──────────────┐      │
│                                          │   Upstream   │      │
│                                          │   Provider   │      │
│                                          └──────────────┘      │
│                                                 │               │
│                                                 ▼               │
│  ┌──────────────┐    ┌──────────────┐    ┌──────────────┐      │
│  │   OpenAI     │    │  Canonical   │    │   Response   │      │
│  │   Response   │◀───│   Events     │◀───│  Transform   │      │
│  │   Builder    │    │              │    │  (per API)   │      │
│  └──────────────┘    └──────────────┘    └──────────────┘      │
│                                                                 │
└─────────────────────────────────────────────────────────────────┘
```

## Canonical Types

### Message Types

```rust
pub enum MessageRole {
    User,
    Assistant,
    ToolResult,
}

pub struct UserMessage {
    pub role: MessageRole, // User
    pub content: Vec<ContentBlock>,
    pub timestamp: i64,
}

pub struct AssistantMessage {
    pub role: MessageRole, // Assistant
    pub content: Vec<ContentBlock>,
    pub api: ApiType,           // Which API produced this
    pub provider: String,       // Which provider produced this
    pub model: String,          // Which model produced this
    pub usage: Usage,
    pub stop_reason: StopReason,
    pub error_message: Option<String>,
    pub timestamp: i64,
}

pub struct ToolResultMessage {
    pub role: MessageRole, // ToolResult
    pub tool_call_id: String,
    pub tool_name: String,
    pub content: Vec<ContentBlock>,
    pub is_error: bool,
    pub timestamp: i64,
}

pub enum Message {
    User(UserMessage),
    Assistant(AssistantMessage),
    ToolResult(ToolResultMessage),
}
```

### Content Blocks

```rust
pub enum ContentBlock {
    Text(TextContent),
    Thinking(ThinkingContent),
    Image(ImageContent),
    ToolCall(ToolCall),
}

pub struct TextContent {
    pub text: String,
}

pub struct ThinkingContent {
    pub thinking: String,
    pub signature: Option<String>, // Provider-specific signature for continuity
}

pub struct ImageContent {
    pub data: String,      // Base64 encoded
    pub mime_type: String, // e.g., "image/png"
}

pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub arguments: serde_json::Value,
}
```

### Context

```rust
pub struct Context {
    pub system_prompt: Option<String>,
    pub messages: Vec<Message>,
    pub tools: Option<Vec<Tool>>,
}

pub struct Tool {
    pub name: String,
    pub description: String,
    pub parameters: serde_json::Value, // JSON Schema
}
```

### Streaming Events

```rust
pub enum StreamEvent {
    Start { partial: AssistantMessage },
    TextStart { content_index: usize },
    TextDelta { content_index: usize, delta: String },
    TextEnd { content_index: usize, content: String },
    ThinkingStart { content_index: usize },
    ThinkingDelta { content_index: usize, delta: String },
    ThinkingEnd { content_index: usize, content: String },
    ToolCallStart { content_index: usize },
    ToolCallDelta { content_index: usize, delta: String },
    ToolCallEnd { content_index: usize, tool_call: ToolCall },
    Done { reason: StopReason, message: AssistantMessage },
    Error { reason: StopReason, error: AssistantMessage },
}
```

## API Types

```rust
pub enum ApiType {
    OpenAICompletions,  // /v1/chat/completions
    OpenAIResponses,    // /v1/responses (newer API)
    AnthropicMessages,  // /v1/messages
    GoogleGenerativeAI, // generateContent
}
```

## Provider Translation Trait

```rust
pub trait ProviderTranslator {
    /// Transform canonical context to provider-specific request body
    fn transform_request(&self, context: &Context, options: &RequestOptions) -> Result<serde_json::Value>;
    
    /// Parse a provider-specific SSE chunk into canonical stream events
    fn parse_stream_chunk(&self, chunk: &str, state: &mut StreamState) -> Result<Vec<StreamEvent>>;
    
    /// Get the endpoint path for this provider
    fn endpoint_path(&self) -> &str;
    
    /// Get required headers for this provider
    fn headers(&self, api_key: &str) -> Vec<(String, String)>;
}
```

## Cross-Provider Message Transformation

When messages from one provider are sent to a different provider, special handling is needed:

### Thinking Blocks

Thinking/reasoning blocks are provider-specific. When crossing providers:
- Convert `ThinkingContent` to `TextContent` with `<thinking>` tags
- Preserve the content for context continuity

```rust
fn transform_messages(messages: &[Message], target_model: &Model) -> Vec<Message> {
    messages.iter().map(|msg| {
        match msg {
            Message::Assistant(assistant) => {
                // If from different provider/API, transform thinking blocks
                if assistant.provider != target_model.provider 
                   || assistant.api != target_model.api {
                    let transformed_content = assistant.content.iter().map(|block| {
                        match block {
                            ContentBlock::Thinking(t) => {
                                ContentBlock::Text(TextContent {
                                    text: format!("<thinking>\n{}\n</thinking>", t.thinking)
                                })
                            }
                            other => other.clone()
                        }
                    }).collect();
                    
                    Message::Assistant(AssistantMessage {
                        content: transformed_content,
                        ..assistant.clone()
                    })
                } else {
                    msg.clone()
                }
            }
            _ => msg.clone()
        }
    }).collect()
}
```

### Orphan Tool Calls

Filter out tool calls that don't have corresponding tool results (except for the last message):

```rust
fn filter_orphan_tool_calls(messages: &mut [Message]) {
    // For each assistant message (except last), remove tool calls
    // that don't have a matching tool result in subsequent messages
}
```

## Provider-Specific Quirks

### OpenAI Completions API

| Quirk | Description |
|-------|-------------|
| `store` field | Some providers don't support it |
| `developer` vs `system` role | Reasoning models use `developer` |
| `max_completion_tokens` vs `max_tokens` | Field name varies |
| Stream options | `stream_options: { include_usage: true }` |

### Anthropic Messages API

| Quirk | Description |
|-------|-------------|
| `x-api-key` header | Not Bearer token |
| `anthropic-version` header | Required version header |
| Tool call ID format | Must match `^[a-zA-Z0-9_-]+$` |
| Cache control | `cache_control: { type: "ephemeral" }` on system/last user |
| Thinking blocks | Need `signature` for continuity |

### Google GenerativeAI

| Quirk | Description |
|-------|-------------|
| Role names | `model` instead of `assistant` |
| Tool results | Via `functionResponse` in user message |
| Image format | `inlineData` with base64 |
| Thinking | `thought: true` flag on parts |

### Mistral

| Quirk | Description |
|-------|-------------|
| Tool IDs | Must be exactly 9 alphanumeric characters |
| Tool result name | Requires `name` field |
| Empty content | Can't have `null` content, use empty string |
| Thinking | Must convert to `<thinking>` text tags |

## Implementation Phases

### Phase 1: Canonical Types (Foundation)
- [ ] Define all canonical types in `src/types.rs`
- [ ] Implement serialization/deserialization
- [ ] Add comprehensive tests

### Phase 2: OpenAI Format (Input/Output)
- [ ] Parse incoming `/v1/chat/completions` requests
- [ ] Build OpenAI SSE response format
- [ ] Handle streaming chunks

### Phase 3: Anthropic Translation
- [ ] Transform canonical → Anthropic Messages API
- [ ] Parse Anthropic SSE → canonical events
- [ ] Handle thinking blocks and signatures

### Phase 4: Google Translation
- [ ] Transform canonical → Google GenerateContent
- [ ] Parse Google stream → canonical events
- [ ] Handle `thought` parts

### Phase 5: Edge Cases & Polish
- [ ] Implement all compat quirks
- [ ] Add provider-specific tests
- [ ] Performance optimization

## File Structure

```
src/
├── types.rs           # Canonical types
├── transform/
│   ├── mod.rs
│   ├── messages.rs    # Cross-provider message transformation
│   ├── openai.rs      # OpenAI parser/builder
│   ├── anthropic.rs   # Anthropic transformer
│   └── google.rs      # Google transformer
├── provider.rs        # Provider metadata (existing)
├── proxy.rs           # Updated to use transformers
└── ...
```

## References

- [pi-ai types.ts](https://github.com/badlogic/pi-mono/blob/main/packages/ai/src/types.ts)
- [pi-ai transform-messages.ts](https://github.com/badlogic/pi-mono/blob/main/packages/ai/src/providers/transorm-messages.ts)
- [pi-ai anthropic.ts](https://github.com/badlogic/pi-mono/blob/main/packages/ai/src/providers/anthropic.ts)
- [pi-ai openai-completions.ts](https://github.com/badlogic/pi-mono/blob/main/packages/ai/src/providers/openai-completions.ts)
- [pi-ai google.ts](https://github.com/badlogic/pi-mono/blob/main/packages/ai/src/providers/google.ts)

//! Enhanced mock provider for testing and benchmarking.
//!
//! Supports configurable scenarios selected via:
//! 1. `X-Mock-Scenario` request header
//! 2. Model name (e.g., `mock/tool-call`, `mock/rate-limit`)
//! 3. Default: `simple_text`
//!
//! Streaming delay is configurable via `X-Mock-Delay-Ms` header (default: 30ms).

use axum::{
    body::Body,
    http::{HeaderMap, StatusCode},
    response::Response,
};
use bytes::Bytes;
use serde_json::json;
use std::time::Duration;

use crate::state::AnalysisEvent;

/// Default delay between streamed chunks in milliseconds.
const DEFAULT_CHUNK_DELAY_MS: u64 = 30;

/// Available mock scenarios.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MockScenario {
    /// Stream realistic word-by-word text response.
    SimpleText,
    /// Emit a properly formatted tool_call (function name + streamed JSON args).
    ToolCall,
    /// Emit two sequential tool_calls in one response.
    MultiTool,
    /// After tool result in follow-up, respond with text completion.
    ToolCallThenText,
    /// Stream 3 normal chunks then emit an SSE error event.
    ErrorMidStream,
    /// Return 429 with Retry-After header and error body.
    RateLimit,
    /// Return 500/503 with OpenAI-format error JSON.
    ServerError,
    /// Accept request, hold connection open without sending data.
    Timeout,
    /// Drop connection mid-stream after N chunks.
    ConnectionReset,
    /// Stream thinking/reasoning content blocks before main response.
    Thinking,
    /// Stream 500+ tokens to test backpressure and buffer handling.
    LongText,
    /// Return broken SSE formatting.
    MalformedSse,
}

impl MockScenario {
    /// Parse scenario from string (case-insensitive, supports kebab-case and snake_case).
    pub fn from_str(s: &str) -> Option<Self> {
        match s.to_lowercase().replace('-', "_").as_str() {
            "simple_text" | "text" | "default" => Some(Self::SimpleText),
            "tool_call" | "tool" => Some(Self::ToolCall),
            "multi_tool" | "multi_tools" => Some(Self::MultiTool),
            "tool_call_then_text" | "tool_then_text" => Some(Self::ToolCallThenText),
            "error_mid_stream" | "mid_stream_error" => Some(Self::ErrorMidStream),
            "rate_limit" | "ratelimit" | "429" => Some(Self::RateLimit),
            "server_error" | "500" | "503" => Some(Self::ServerError),
            "timeout" | "hang" => Some(Self::Timeout),
            "connection_reset" | "drop" | "reset" => Some(Self::ConnectionReset),
            "thinking" | "reasoning" => Some(Self::Thinking),
            "long_text" | "long" | "backpressure" => Some(Self::LongText),
            "malformed_sse" | "malformed" | "broken" => Some(Self::MalformedSse),
            _ => None,
        }
    }

    /// Derive scenario from mock model name (e.g., `mock/tool-call`).
    pub fn from_model(model: &str) -> Option<Self> {
        let suffix = model
            .strip_prefix("mock/")
            .or_else(|| model.strip_prefix("mock-"));
        suffix.and_then(Self::from_str)
    }

    /// All known scenario names for model registration.
    pub fn all_model_ids() -> Vec<&'static str> {
        vec![
            "mock/simple-text",
            "mock/tool-call",
            "mock/multi-tool",
            "mock/tool-call-then-text",
            "mock/error-mid-stream",
            "mock/rate-limit",
            "mock/server-error",
            "mock/timeout",
            "mock/connection-reset",
            "mock/thinking",
            "mock/long-text",
            "mock/malformed-sse",
        ]
    }
}

/// Parameters for mock response generation.
pub struct MockRequest {
    pub model: String,
    pub stream: bool,
    pub request_id: String,
    pub scenario: MockScenario,
    pub delay_ms: u64,
    pub analysis_tx: tokio::sync::broadcast::Sender<AnalysisEvent>,
}

/// Resolve mock scenario and delay from request headers and model name.
pub fn resolve_mock_params(
    model: &str,
    stream: bool,
    request_id: &str,
    headers: &HeaderMap,
    analysis_tx: tokio::sync::broadcast::Sender<AnalysisEvent>,
) -> MockRequest {
    // Scenario priority: X-Mock-Scenario header > model name > default
    let scenario = headers
        .get("x-mock-scenario")
        .and_then(|h| h.to_str().ok())
        .and_then(MockScenario::from_str)
        .or_else(|| MockScenario::from_model(model))
        .unwrap_or(MockScenario::SimpleText);

    let delay_ms = headers
        .get("x-mock-delay-ms")
        .and_then(|h| h.to_str().ok())
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(DEFAULT_CHUNK_DELAY_MS);

    MockRequest {
        model: model.to_string(),
        stream,
        request_id: request_id.to_string(),
        scenario,
        delay_ms,
        analysis_tx,
    }
}

/// Handle a mock provider request, dispatching to the appropriate scenario.
pub async fn handle_mock_response(req: MockRequest) -> Result<Response, Response> {
    match req.scenario {
        MockScenario::SimpleText => handle_simple_text(req).await,
        MockScenario::ToolCall => handle_tool_call(req).await,
        MockScenario::MultiTool => handle_multi_tool(req).await,
        MockScenario::ToolCallThenText => handle_tool_call_then_text(req).await,
        MockScenario::ErrorMidStream => handle_error_mid_stream(req).await,
        MockScenario::RateLimit => handle_rate_limit(req).await,
        MockScenario::ServerError => handle_server_error(req).await,
        MockScenario::Timeout => handle_timeout(req).await,
        MockScenario::ConnectionReset => handle_connection_reset(req).await,
        MockScenario::Thinking => handle_thinking(req).await,
        MockScenario::LongText => handle_long_text(req).await,
        MockScenario::MalformedSse => handle_malformed_sse(req).await,
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn timestamp() -> i64 {
    chrono::Utc::now().timestamp()
}

/// Log an SSE chunk through the analysis channel with a timestamp.
fn audit_chunk(
    tx: &tokio::sync::broadcast::Sender<AnalysisEvent>,
    request_id: &str,
    chunk: &str,
) {
    let _ = tx.send(AnalysisEvent::ResponseChunk {
        timestamp: chrono::Utc::now().timestamp_millis(),
        id: request_id.to_string(),
        chunk: chunk.to_string(),
    });
}

/// Build a single SSE data line for a chat completion chunk.
fn sse_chunk(id: &str, ts: i64, model: &str, delta: serde_json::Value, finish: Option<&str>) -> String {
    let mut choice = json!({
        "index": 0,
        "delta": delta,
        "finish_reason": serde_json::Value::Null,
    });
    if let Some(reason) = finish {
        choice["finish_reason"] = json!(reason);
    }
    let obj = json!({
        "id": format!("chatcmpl-mock-{}", id),
        "object": "chat.completion.chunk",
        "created": ts,
        "model": model,
        "choices": [choice],
    });
    format!("data: {}\n\n", serde_json::to_string(&obj).unwrap())
}

/// Build an SSE chunk with usage included (final chunk).
fn sse_chunk_with_usage(
    id: &str,
    ts: i64,
    model: &str,
    delta: serde_json::Value,
    finish: Option<&str>,
    prompt_tokens: u32,
    completion_tokens: u32,
) -> String {
    let mut choice = json!({
        "index": 0,
        "delta": delta,
        "finish_reason": serde_json::Value::Null,
    });
    if let Some(reason) = finish {
        choice["finish_reason"] = json!(reason);
    }
    let obj = json!({
        "id": format!("chatcmpl-mock-{}", id),
        "object": "chat.completion.chunk",
        "created": ts,
        "model": model,
        "choices": [choice],
        "usage": {
            "prompt_tokens": prompt_tokens,
            "completion_tokens": completion_tokens,
            "total_tokens": prompt_tokens + completion_tokens,
        },
    });
    format!("data: {}\n\n", serde_json::to_string(&obj).unwrap())
}

/// Build a non-streaming chat completion JSON response.
fn non_streaming_response(
    id: &str,
    model: &str,
    content: &str,
    prompt_tokens: u32,
    completion_tokens: u32,
) -> serde_json::Value {
    json!({
        "id": format!("chatcmpl-mock-{}", id),
        "object": "chat.completion",
        "created": timestamp(),
        "model": model,
        "choices": [{
            "index": 0,
            "message": {
                "role": "assistant",
                "content": content,
            },
            "finish_reason": "stop",
        }],
        "usage": {
            "prompt_tokens": prompt_tokens,
            "completion_tokens": completion_tokens,
            "total_tokens": prompt_tokens + completion_tokens,
        },
    })
}

/// Build a streaming response from a list of SSE chunk strings with delay.
fn build_delayed_stream_response(
    chunks: Vec<String>,
    delay: Duration,
    request_id: &str,
    analysis_tx: tokio::sync::broadcast::Sender<AnalysisEvent>,
) -> Response {
    let rid = request_id.to_string();
    let (tx, rx) = tokio::sync::mpsc::channel::<Result<Bytes, std::io::Error>>(32);

    tokio::spawn(async move {
        for chunk in chunks {
            audit_chunk(&analysis_tx, &rid, &chunk);
            if tx.send(Ok(Bytes::from(chunk))).await.is_err() {
                break;
            }
            if !delay.is_zero() {
                tokio::time::sleep(delay).await;
            }
        }
    });

    let stream = tokio_stream::wrappers::ReceiverStream::new(rx);

    Response::builder()
        .status(StatusCode::OK)
        .header("content-type", "text/event-stream")
        .header("cache-control", "no-cache")
        .header("x-mock-response", "true")
        .header("x-mock-scenario", "true")
        .body(Body::from_stream(stream))
        .unwrap()
}

/// Build a non-streaming JSON response.
fn build_json_response(
    body: serde_json::Value,
    request_id: &str,
    analysis_tx: &tokio::sync::broadcast::Sender<AnalysisEvent>,
) -> Response {
    let serialized = serde_json::to_vec(&body).unwrap();
    audit_chunk(analysis_tx, request_id, &format!("[mock non-streaming] {}", serde_json::to_string(&body).unwrap_or_default()));

    Response::builder()
        .status(StatusCode::OK)
        .header("content-type", "application/json")
        .header("x-mock-response", "true")
        .header("x-mock-scenario", "true")
        .body(Body::from(serialized))
        .unwrap()
}

// ---------------------------------------------------------------------------
// Scenario: simple_text
// ---------------------------------------------------------------------------

const SIMPLE_TEXT_WORDS: &[&str] = &[
    "The", "quick", "brown", "fox", "jumps", "over", "the", "lazy", "dog.",
    "This", "is", "a", "mock", "response", "for", "testing", "streaming",
    "behavior", "in", "the", "proxy.", "Each", "word", "arrives", "as",
    "a", "separate", "SSE", "chunk", "to", "simulate", "realistic",
    "token-by-token", "generation.",
];

async fn handle_simple_text(req: MockRequest) -> Result<Response, Response> {
    let ts = timestamp();
    let delay = Duration::from_millis(req.delay_ms);

    if req.stream {
        let mut chunks = Vec::new();

        // Role chunk
        chunks.push(sse_chunk(
            &req.request_id,
            ts,
            &req.model,
            json!({"role": "assistant", "content": ""}),
            None,
        ));

        // Word-by-word content chunks
        for (i, word) in SIMPLE_TEXT_WORDS.iter().enumerate() {
            let prefix = if i == 0 { "" } else { " " };
            chunks.push(sse_chunk(
                &req.request_id,
                ts,
                &req.model,
                json!({"content": format!("{}{}", prefix, word)}),
                None,
            ));
        }

        // Final chunk with usage
        let completion_tokens = SIMPLE_TEXT_WORDS.len() as u32;
        chunks.push(sse_chunk_with_usage(
            &req.request_id,
            ts,
            &req.model,
            json!({}),
            Some("stop"),
            10,
            completion_tokens,
        ));
        chunks.push("data: [DONE]\n\n".to_string());

        Ok(build_delayed_stream_response(
            chunks,
            delay,
            &req.request_id,
            req.analysis_tx,
        ))
    } else {
        let content = SIMPLE_TEXT_WORDS.join(" ");
        let body = non_streaming_response(
            &req.request_id,
            &req.model,
            &content,
            10,
            SIMPLE_TEXT_WORDS.len() as u32,
        );
        Ok(build_json_response(body, &req.request_id, &req.analysis_tx))
    }
}

// ---------------------------------------------------------------------------
// Scenario: tool_call
// ---------------------------------------------------------------------------

async fn handle_tool_call(req: MockRequest) -> Result<Response, Response> {
    let ts = timestamp();
    let delay = Duration::from_millis(req.delay_ms);

    if req.stream {
        let mut chunks = Vec::new();

        // Role chunk
        chunks.push(sse_chunk(
            &req.request_id,
            ts,
            &req.model,
            json!({"role": "assistant", "content": null}),
            None,
        ));

        // Tool call: function name
        chunks.push(sse_chunk(
            &req.request_id,
            ts,
            &req.model,
            json!({
                "tool_calls": [{
                    "index": 0,
                    "id": "call_mock_001",
                    "type": "function",
                    "function": {
                        "name": "get_weather",
                        "arguments": ""
                    }
                }]
            }),
            None,
        ));

        // Tool call: streamed JSON arguments
        let arg_parts = [
            r#"{"loc"#,
            r#"ation":"#,
            r#" "San"#,
            r#" Fran"#,
            r#"cisco","#,
            r#" "unit"#,
            r#"": "cel"#,
            r#"sius"}"#,
        ];
        for part in &arg_parts {
            chunks.push(sse_chunk(
                &req.request_id,
                ts,
                &req.model,
                json!({
                    "tool_calls": [{
                        "index": 0,
                        "function": {
                            "arguments": part
                        }
                    }]
                }),
                None,
            ));
        }

        // Finish with tool_calls reason
        chunks.push(sse_chunk_with_usage(
            &req.request_id,
            ts,
            &req.model,
            json!({}),
            Some("tool_calls"),
            10,
            15,
        ));
        chunks.push("data: [DONE]\n\n".to_string());

        Ok(build_delayed_stream_response(
            chunks,
            delay,
            &req.request_id,
            req.analysis_tx,
        ))
    } else {
        let body = json!({
            "id": format!("chatcmpl-mock-{}", req.request_id),
            "object": "chat.completion",
            "created": ts,
            "model": req.model,
            "choices": [{
                "index": 0,
                "message": {
                    "role": "assistant",
                    "content": null,
                    "tool_calls": [{
                        "id": "call_mock_001",
                        "type": "function",
                        "function": {
                            "name": "get_weather",
                            "arguments": r#"{"location": "San Francisco", "unit": "celsius"}"#
                        }
                    }]
                },
                "finish_reason": "tool_calls"
            }],
            "usage": {
                "prompt_tokens": 10,
                "completion_tokens": 15,
                "total_tokens": 25
            }
        });
        Ok(build_json_response(body, &req.request_id, &req.analysis_tx))
    }
}

// ---------------------------------------------------------------------------
// Scenario: multi_tool
// ---------------------------------------------------------------------------

async fn handle_multi_tool(req: MockRequest) -> Result<Response, Response> {
    let ts = timestamp();
    let delay = Duration::from_millis(req.delay_ms);

    if req.stream {
        let mut chunks = Vec::new();

        // Role chunk
        chunks.push(sse_chunk(
            &req.request_id,
            ts,
            &req.model,
            json!({"role": "assistant", "content": null}),
            None,
        ));

        // First tool call
        chunks.push(sse_chunk(
            &req.request_id,
            ts,
            &req.model,
            json!({
                "tool_calls": [{
                    "index": 0,
                    "id": "call_mock_001",
                    "type": "function",
                    "function": {
                        "name": "get_weather",
                        "arguments": ""
                    }
                }]
            }),
            None,
        ));
        chunks.push(sse_chunk(
            &req.request_id,
            ts,
            &req.model,
            json!({
                "tool_calls": [{
                    "index": 0,
                    "function": {
                        "arguments": r#"{"location": "San Francisco"}"#
                    }
                }]
            }),
            None,
        ));

        // Second tool call
        chunks.push(sse_chunk(
            &req.request_id,
            ts,
            &req.model,
            json!({
                "tool_calls": [{
                    "index": 1,
                    "id": "call_mock_002",
                    "type": "function",
                    "function": {
                        "name": "get_time",
                        "arguments": ""
                    }
                }]
            }),
            None,
        ));
        chunks.push(sse_chunk(
            &req.request_id,
            ts,
            &req.model,
            json!({
                "tool_calls": [{
                    "index": 1,
                    "function": {
                        "arguments": r#"{"timezone": "PST"}"#
                    }
                }]
            }),
            None,
        ));

        // Finish
        chunks.push(sse_chunk_with_usage(
            &req.request_id,
            ts,
            &req.model,
            json!({}),
            Some("tool_calls"),
            10,
            25,
        ));
        chunks.push("data: [DONE]\n\n".to_string());

        Ok(build_delayed_stream_response(
            chunks,
            delay,
            &req.request_id,
            req.analysis_tx,
        ))
    } else {
        let body = json!({
            "id": format!("chatcmpl-mock-{}", req.request_id),
            "object": "chat.completion",
            "created": ts,
            "model": req.model,
            "choices": [{
                "index": 0,
                "message": {
                    "role": "assistant",
                    "content": null,
                    "tool_calls": [
                        {
                            "id": "call_mock_001",
                            "type": "function",
                            "function": {
                                "name": "get_weather",
                                "arguments": r#"{"location": "San Francisco"}"#
                            }
                        },
                        {
                            "id": "call_mock_002",
                            "type": "function",
                            "function": {
                                "name": "get_time",
                                "arguments": r#"{"timezone": "PST"}"#
                            }
                        }
                    ]
                },
                "finish_reason": "tool_calls"
            }],
            "usage": {
                "prompt_tokens": 10,
                "completion_tokens": 25,
                "total_tokens": 35
            }
        });
        Ok(build_json_response(body, &req.request_id, &req.analysis_tx))
    }
}

// ---------------------------------------------------------------------------
// Scenario: tool_call_then_text
// ---------------------------------------------------------------------------

async fn handle_tool_call_then_text(req: MockRequest) -> Result<Response, Response> {
    // This scenario responds with a text completion, simulating a follow-up
    // response after the client has provided tool results.
    let ts = timestamp();
    let delay = Duration::from_millis(req.delay_ms);
    let text = "Based on the weather data, it is currently 18 degrees Celsius \
                and partly cloudy in San Francisco. Perfect weather for a walk!";

    if req.stream {
        let mut chunks = Vec::new();

        chunks.push(sse_chunk(
            &req.request_id,
            ts,
            &req.model,
            json!({"role": "assistant", "content": ""}),
            None,
        ));

        for word in text.split_whitespace() {
            chunks.push(sse_chunk(
                &req.request_id,
                ts,
                &req.model,
                json!({"content": format!("{} ", word)}),
                None,
            ));
        }

        let word_count = text.split_whitespace().count() as u32;
        chunks.push(sse_chunk_with_usage(
            &req.request_id,
            ts,
            &req.model,
            json!({}),
            Some("stop"),
            25,
            word_count,
        ));
        chunks.push("data: [DONE]\n\n".to_string());

        Ok(build_delayed_stream_response(
            chunks,
            delay,
            &req.request_id,
            req.analysis_tx,
        ))
    } else {
        let word_count = text.split_whitespace().count() as u32;
        let body = non_streaming_response(&req.request_id, &req.model, text, 25, word_count);
        Ok(build_json_response(body, &req.request_id, &req.analysis_tx))
    }
}

// ---------------------------------------------------------------------------
// Scenario: error_mid_stream
// ---------------------------------------------------------------------------

async fn handle_error_mid_stream(req: MockRequest) -> Result<Response, Response> {
    let ts = timestamp();
    let delay = Duration::from_millis(req.delay_ms);

    if req.stream {
        let mut chunks = Vec::new();

        // Role chunk
        chunks.push(sse_chunk(
            &req.request_id,
            ts,
            &req.model,
            json!({"role": "assistant", "content": ""}),
            None,
        ));

        // 3 normal content chunks
        for word in &["Everything", " seems", " fine..."] {
            chunks.push(sse_chunk(
                &req.request_id,
                ts,
                &req.model,
                json!({"content": word}),
                None,
            ));
        }

        // SSE error event
        let error_event = json!({
            "error": {
                "message": "Internal server error during generation",
                "type": "server_error",
                "code": "internal_error"
            }
        });
        chunks.push(format!("data: {}\n\n", serde_json::to_string(&error_event).unwrap()));

        Ok(build_delayed_stream_response(
            chunks,
            delay,
            &req.request_id,
            req.analysis_tx,
        ))
    } else {
        // Non-streaming: just return an error JSON
        let body = json!({
            "error": {
                "message": "Internal server error during generation",
                "type": "server_error",
                "code": "internal_error"
            }
        });
        let serialized = serde_json::to_vec(&body).unwrap();
        audit_chunk(&req.analysis_tx, &req.request_id, "[mock error_mid_stream non-streaming]");

        Ok(Response::builder()
            .status(StatusCode::INTERNAL_SERVER_ERROR)
            .header("content-type", "application/json")
            .header("x-mock-response", "true")
            .header("x-mock-scenario", "error_mid_stream")
            .body(Body::from(serialized))
            .unwrap())
    }
}

// ---------------------------------------------------------------------------
// Scenario: rate_limit
// ---------------------------------------------------------------------------

async fn handle_rate_limit(req: MockRequest) -> Result<Response, Response> {
    let body = json!({
        "error": {
            "message": "Rate limit exceeded. Please retry after 30 seconds.",
            "type": "rate_limit_error",
            "param": null,
            "code": "rate_limit_exceeded"
        }
    });
    let serialized = serde_json::to_vec(&body).unwrap();
    audit_chunk(&req.analysis_tx, &req.request_id, "[mock rate_limit 429]");

    Ok(Response::builder()
        .status(StatusCode::TOO_MANY_REQUESTS)
        .header("content-type", "application/json")
        .header("retry-after", "30")
        .header("x-ratelimit-limit-requests", "100")
        .header("x-ratelimit-remaining-requests", "0")
        .header("x-ratelimit-reset-requests", "30s")
        .header("x-mock-response", "true")
        .header("x-mock-scenario", "rate_limit")
        .body(Body::from(serialized))
        .unwrap())
}

// ---------------------------------------------------------------------------
// Scenario: server_error
// ---------------------------------------------------------------------------

async fn handle_server_error(req: MockRequest) -> Result<Response, Response> {
    let body = json!({
        "error": {
            "message": "The server had an error while processing your request. Sorry about that! You can retry your request, or contact us through our help center if the error persists.",
            "type": "server_error",
            "param": null,
            "code": "server_error"
        }
    });
    let serialized = serde_json::to_vec(&body).unwrap();
    audit_chunk(&req.analysis_tx, &req.request_id, "[mock server_error 503]");

    Ok(Response::builder()
        .status(StatusCode::SERVICE_UNAVAILABLE)
        .header("content-type", "application/json")
        .header("x-mock-response", "true")
        .header("x-mock-scenario", "server_error")
        .body(Body::from(serialized))
        .unwrap())
}

// ---------------------------------------------------------------------------
// Scenario: timeout
// ---------------------------------------------------------------------------

async fn handle_timeout(req: MockRequest) -> Result<Response, Response> {
    audit_chunk(&req.analysis_tx, &req.request_id, "[mock timeout - holding connection open]");

    // Create a stream that sends the SSE header but then blocks forever
    let (tx, rx) = tokio::sync::mpsc::channel::<Result<Bytes, std::io::Error>>(1);

    tokio::spawn(async move {
        // Send a comment to keep the connection alive, then hold indefinitely
        let _ = tx.send(Ok(Bytes::from(": mock timeout scenario\n\n"))).await;
        // Hold the connection open for 5 minutes (effectively forever for tests)
        tokio::time::sleep(Duration::from_secs(300)).await;
        drop(tx);
    });

    let stream = tokio_stream::wrappers::ReceiverStream::new(rx);

    Ok(Response::builder()
        .status(StatusCode::OK)
        .header("content-type", "text/event-stream")
        .header("cache-control", "no-cache")
        .header("x-mock-response", "true")
        .header("x-mock-scenario", "timeout")
        .body(Body::from_stream(stream))
        .unwrap())
}

// ---------------------------------------------------------------------------
// Scenario: connection_reset
// ---------------------------------------------------------------------------

async fn handle_connection_reset(req: MockRequest) -> Result<Response, Response> {
    let ts = timestamp();
    let delay = Duration::from_millis(req.delay_ms);

    audit_chunk(
        &req.analysis_tx,
        &req.request_id,
        "[mock connection_reset - will drop after 3 chunks]",
    );

    let rid = req.request_id.clone();
    let model = req.model.clone();
    let analysis_tx = req.analysis_tx.clone();

    let (tx, rx) = tokio::sync::mpsc::channel::<Result<Bytes, std::io::Error>>(8);

    tokio::spawn(async move {
        // Role chunk
        let chunk = sse_chunk(&rid, ts, &model, json!({"role": "assistant", "content": ""}), None);
        audit_chunk(&analysis_tx, &rid, &chunk);
        let _ = tx.send(Ok(Bytes::from(chunk))).await;
        tokio::time::sleep(delay).await;

        // 3 content chunks then drop
        for word in &["Connection", " will", " drop..."] {
            let chunk = sse_chunk(&rid, ts, &model, json!({"content": word}), None);
            audit_chunk(&analysis_tx, &rid, &chunk);
            if tx.send(Ok(Bytes::from(chunk))).await.is_err() {
                return;
            }
            tokio::time::sleep(delay).await;
        }

        // Drop the sender to simulate connection reset
        drop(tx);
    });

    let stream = tokio_stream::wrappers::ReceiverStream::new(rx);

    Ok(Response::builder()
        .status(StatusCode::OK)
        .header("content-type", "text/event-stream")
        .header("cache-control", "no-cache")
        .header("x-mock-response", "true")
        .header("x-mock-scenario", "connection_reset")
        .body(Body::from_stream(stream))
        .unwrap())
}

// ---------------------------------------------------------------------------
// Scenario: thinking
// ---------------------------------------------------------------------------

async fn handle_thinking(req: MockRequest) -> Result<Response, Response> {
    let ts = timestamp();
    let delay = Duration::from_millis(req.delay_ms);

    if req.stream {
        let mut chunks = Vec::new();

        // Role chunk
        chunks.push(sse_chunk(
            &req.request_id,
            ts,
            &req.model,
            json!({"role": "assistant", "content": ""}),
            None,
        ));

        // Thinking/reasoning content (simulates Anthropic extended thinking format)
        // In OpenAI format, thinking is represented via content blocks or special markers
        let thinking_text = "Let me think about this step by step. \
            First, I need to consider the problem constraints. \
            The user is asking about weather, so I should look at the available data. \
            Based on the information I have, I can provide a helpful response.";

        // Emit thinking as a special content block (OpenAI thinking format)
        for word in thinking_text.split_whitespace() {
            chunks.push(sse_chunk(
                &req.request_id,
                ts,
                &req.model,
                json!({"content": format!("{} ", word)}),
                None,
            ));
        }

        // Separator (simulates transition from thinking to response)
        chunks.push(sse_chunk(
            &req.request_id,
            ts,
            &req.model,
            json!({"content": "\n\n---\n\n"}),
            None,
        ));

        // Actual response text
        let response_words = [
            "The", "weather", "in", "San", "Francisco", "is", "currently",
            "18C", "and", "partly", "cloudy.",
        ];
        for word in &response_words {
            chunks.push(sse_chunk(
                &req.request_id,
                ts,
                &req.model,
                json!({"content": format!("{} ", word)}),
                None,
            ));
        }

        let total_words = thinking_text.split_whitespace().count() + response_words.len() + 1;
        chunks.push(sse_chunk_with_usage(
            &req.request_id,
            ts,
            &req.model,
            json!({}),
            Some("stop"),
            10,
            total_words as u32,
        ));
        chunks.push("data: [DONE]\n\n".to_string());

        Ok(build_delayed_stream_response(
            chunks,
            delay,
            &req.request_id,
            req.analysis_tx,
        ))
    } else {
        let content = "Let me think about this step by step. \
            First, I need to consider the problem constraints. \
            The user is asking about weather, so I should look at the available data. \
            Based on the information I have, I can provide a helpful response.\n\n---\n\n\
            The weather in San Francisco is currently 18C and partly cloudy.";
        let word_count = content.split_whitespace().count() as u32;
        let body = non_streaming_response(&req.request_id, &req.model, content, 10, word_count);
        Ok(build_json_response(body, &req.request_id, &req.analysis_tx))
    }
}

// ---------------------------------------------------------------------------
// Scenario: long_text
// ---------------------------------------------------------------------------

/// Generate 500+ words of lorem-ipsum-style content for backpressure testing.
fn generate_long_text() -> Vec<&'static str> {
    // ~550 words to exceed the 500 token threshold
    let words: Vec<&str> = vec![
        "In", "the", "realm", "of", "artificial", "intelligence,", "large", "language",
        "models", "have", "emerged", "as", "powerful", "tools", "for", "understanding",
        "and", "generating", "human", "language.", "These", "models,", "trained", "on",
        "vast", "corpora", "of", "text", "data,", "can", "perform", "a", "wide",
        "range", "of", "tasks", "from", "translation", "to", "creative", "writing.",
        "The", "architecture", "behind", "these", "systems", "typically", "relies", "on",
        "transformer", "networks,", "which", "use", "self-attention", "mechanisms", "to",
        "process", "sequential", "data", "efficiently.", "Unlike", "earlier", "recurrent",
        "approaches,", "transformers", "can", "handle", "long-range", "dependencies",
        "by", "attending", "to", "all", "positions", "in", "a", "sequence",
        "simultaneously.", "This", "parallel", "processing", "capability", "enables",
        "training", "on", "much", "larger", "datasets", "and", "achieving", "better",
        "performance", "across", "diverse", "benchmarks.", "The", "scaling", "laws",
        "governing", "these", "models", "suggest", "that", "increasing", "both", "model",
        "size", "and", "training", "data", "leads", "to", "predictable", "improvements",
        "in", "capability.", "However,", "this", "comes", "at", "significant",
        "computational", "cost,", "requiring", "specialized", "hardware", "such", "as",
        "GPUs", "and", "TPUs", "for", "both", "training", "and", "inference.",
        "Recent", "advances", "in", "efficiency,", "including", "quantization,",
        "distillation,", "and", "sparse", "attention", "patterns,", "have", "made",
        "it", "possible", "to", "deploy", "these", "models", "in", "more",
        "resource-constrained", "environments.", "The", "fine-tuning", "process",
        "allows", "pre-trained", "models", "to", "be", "adapted", "for", "specific",
        "tasks", "with", "relatively", "small", "amounts", "of", "labeled", "data.",
        "Instruction", "tuning", "and", "reinforcement", "learning", "from", "human",
        "feedback", "further", "align", "model", "outputs", "with", "human",
        "preferences", "and", "values.", "Safety", "considerations", "remain",
        "paramount,", "as", "these", "models", "can", "generate", "harmful",
        "or", "misleading", "content", "if", "not", "properly", "constrained.",
        "Research", "into", "constitutional", "AI,", "red-teaming,", "and",
        "interpretability", "aims", "to", "address", "these", "challenges.",
        "The", "deployment", "of", "language", "models", "in", "production",
        "systems", "introduces", "additional", "concerns", "around", "latency,",
        "throughput,", "and", "cost", "optimization.", "Streaming", "responses",
        "token-by-token", "provides", "a", "better", "user", "experience", "than",
        "waiting", "for", "complete", "generation.", "Load", "balancing", "and",
        "request", "queuing", "help", "manage", "concurrent", "users", "while",
        "maintaining", "quality", "of", "service.", "Caching", "strategies",
        "for", "common", "prompts", "reduce", "redundant", "computation.",
        "Monitoring", "and", "observability", "tools", "track", "key", "metrics",
        "such", "as", "time-to-first-token,", "tokens-per-second,", "and",
        "error", "rates.", "These", "metrics", "inform", "capacity", "planning",
        "and", "help", "identify", "performance", "regressions.", "The", "proxy",
        "layer", "between", "clients", "and", "upstream", "providers", "serves",
        "as", "a", "natural", "point", "for", "implementing", "cross-cutting",
        "concerns", "like", "authentication,", "rate", "limiting,", "usage",
        "tracking,", "and", "request", "transformation.", "By", "abstracting",
        "provider-specific", "APIs", "behind", "a", "common", "interface,",
        "applications", "gain", "the", "flexibility", "to", "switch", "between",
        "providers", "without", "code", "changes.", "This", "architecture", "also",
        "enables", "advanced", "routing", "strategies", "such", "as", "fallback",
        "chains,", "cost-based", "routing,", "and", "A/B", "testing", "of",
        "different", "models.", "The", "mock", "provider", "pattern", "extends",
        "this", "architecture", "by", "allowing", "deterministic", "testing",
        "of", "all", "these", "behaviors", "without", "incurring", "API",
        "costs", "or", "depending", "on", "external", "service", "availability.",
        "Comprehensive", "test", "coverage", "across", "normal", "and", "error",
        "scenarios", "builds", "confidence", "in", "the", "system", "reliability.",
        "Furthermore,", "the", "evolution", "of", "multi-modal", "models",
        "that", "process", "text,", "images,", "audio,", "and", "video",
        "simultaneously", "opens", "new", "possibilities", "for", "rich",
        "interactive", "experiences.", "These", "models", "require", "even",
        "larger", "context", "windows", "and", "more", "sophisticated",
        "attention", "mechanisms", "to", "handle", "diverse", "input",
        "modalities.", "The", "alignment", "problem", "becomes", "more",
        "complex", "in", "multi-modal", "settings,", "as", "the", "model",
        "must", "reason", "across", "different", "types", "of", "information",
        "and", "produce", "coherent,", "helpful", "responses.", "Evaluation",
        "frameworks", "must", "also", "evolve", "to", "assess", "quality",
        "across", "modalities,", "moving", "beyond", "simple", "text-based",
        "benchmarks.", "The", "infrastructure", "supporting", "these",
        "systems", "continues", "to", "advance,", "with", "specialized",
        "chips,", "optimized", "serving", "frameworks,", "and", "efficient",
        "memory", "management", "techniques", "enabling", "faster", "and",
        "more", "cost-effective", "inference.", "This", "concludes", "the",
        "long", "text", "response", "for", "backpressure", "and", "buffer",
        "handling", "verification.",
    ];
    words
}

async fn handle_long_text(req: MockRequest) -> Result<Response, Response> {
    let ts = timestamp();
    let delay = Duration::from_millis(req.delay_ms);
    let words = generate_long_text();

    if req.stream {
        let mut chunks = Vec::new();

        // Role chunk
        chunks.push(sse_chunk(
            &req.request_id,
            ts,
            &req.model,
            json!({"role": "assistant", "content": ""}),
            None,
        ));

        // Word-by-word content
        for (i, word) in words.iter().enumerate() {
            let prefix = if i == 0 { "" } else { " " };
            chunks.push(sse_chunk(
                &req.request_id,
                ts,
                &req.model,
                json!({"content": format!("{}{}", prefix, word)}),
                None,
            ));
        }

        let completion_tokens = words.len() as u32;
        chunks.push(sse_chunk_with_usage(
            &req.request_id,
            ts,
            &req.model,
            json!({}),
            Some("stop"),
            10,
            completion_tokens,
        ));
        chunks.push("data: [DONE]\n\n".to_string());

        Ok(build_delayed_stream_response(
            chunks,
            delay,
            &req.request_id,
            req.analysis_tx,
        ))
    } else {
        let content: String = words.join(" ");
        let word_count = words.len() as u32;
        let body = non_streaming_response(&req.request_id, &req.model, &content, 10, word_count);
        Ok(build_json_response(body, &req.request_id, &req.analysis_tx))
    }
}

// ---------------------------------------------------------------------------
// Scenario: malformed_sse
// ---------------------------------------------------------------------------

async fn handle_malformed_sse(req: MockRequest) -> Result<Response, Response> {
    let ts = timestamp();
    let delay = Duration::from_millis(req.delay_ms);

    audit_chunk(
        &req.analysis_tx,
        &req.request_id,
        "[mock malformed_sse - sending broken SSE data]",
    );

    if req.stream {
        let mut chunks: Vec<String> = Vec::new();

        // Correct first chunk
        chunks.push(sse_chunk(
            &req.request_id,
            ts,
            &req.model,
            json!({"role": "assistant", "content": ""}),
            None,
        ));

        // Missing "data: " prefix
        let obj = json!({
            "id": format!("chatcmpl-mock-{}", req.request_id),
            "object": "chat.completion.chunk",
            "created": ts,
            "model": req.model,
            "choices": [{"index": 0, "delta": {"content": "This "}, "finish_reason": null}],
        });
        chunks.push(format!("{}\n\n", serde_json::to_string(&obj).unwrap()));

        // Double newline inside data field (breaks event boundary)
        chunks.push(format!(
            "data: {{\"content\": \"has\n\nbro\n\nken\"}}\n\n"
        ));

        // Truncated JSON (unclosed brace)
        chunks.push("data: {\"id\":\"chatcmpl-mock-broken\",\"choices\":[{\"delta\":{\"content\":\"form\n\n".to_string());

        // Valid chunk after malformed ones (to test recovery)
        chunks.push(sse_chunk(
            &req.request_id,
            ts,
            &req.model,
            json!({"content": "atting."}),
            None,
        ));

        // Proper termination
        chunks.push(sse_chunk(
            &req.request_id,
            ts,
            &req.model,
            json!({}),
            Some("stop"),
        ));
        chunks.push("data: [DONE]\n\n".to_string());

        Ok(build_delayed_stream_response(
            chunks,
            delay,
            &req.request_id,
            req.analysis_tx,
        ))
    } else {
        // Non-streaming: return malformed JSON
        let broken_json = r#"{"id": "chatcmpl-mock-broken", "choices": [{"message": {"content": "malformed"#;
        audit_chunk(&req.analysis_tx, &req.request_id, "[mock malformed non-streaming]");

        Ok(Response::builder()
            .status(StatusCode::OK)
            .header("content-type", "application/json")
            .header("x-mock-response", "true")
            .header("x-mock-scenario", "malformed_sse")
            .body(Body::from(broken_json))
            .unwrap())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_scenario_from_str() {
        assert_eq!(MockScenario::from_str("simple_text"), Some(MockScenario::SimpleText));
        assert_eq!(MockScenario::from_str("simple-text"), Some(MockScenario::SimpleText));
        assert_eq!(MockScenario::from_str("SIMPLE_TEXT"), Some(MockScenario::SimpleText));
        assert_eq!(MockScenario::from_str("tool_call"), Some(MockScenario::ToolCall));
        assert_eq!(MockScenario::from_str("tool-call"), Some(MockScenario::ToolCall));
        assert_eq!(MockScenario::from_str("multi_tool"), Some(MockScenario::MultiTool));
        assert_eq!(MockScenario::from_str("rate_limit"), Some(MockScenario::RateLimit));
        assert_eq!(MockScenario::from_str("429"), Some(MockScenario::RateLimit));
        assert_eq!(MockScenario::from_str("500"), Some(MockScenario::ServerError));
        assert_eq!(MockScenario::from_str("timeout"), Some(MockScenario::Timeout));
        assert_eq!(MockScenario::from_str("connection_reset"), Some(MockScenario::ConnectionReset));
        assert_eq!(MockScenario::from_str("thinking"), Some(MockScenario::Thinking));
        assert_eq!(MockScenario::from_str("long_text"), Some(MockScenario::LongText));
        assert_eq!(MockScenario::from_str("malformed_sse"), Some(MockScenario::MalformedSse));
        assert_eq!(MockScenario::from_str("unknown"), None);
    }

    #[test]
    fn test_scenario_from_model() {
        assert_eq!(MockScenario::from_model("mock/simple-text"), Some(MockScenario::SimpleText));
        assert_eq!(MockScenario::from_model("mock/tool-call"), Some(MockScenario::ToolCall));
        assert_eq!(MockScenario::from_model("mock-rate-limit"), Some(MockScenario::RateLimit));
        assert_eq!(MockScenario::from_model("mock/long-text"), Some(MockScenario::LongText));
        assert_eq!(MockScenario::from_model("gpt-4"), None);
        assert_eq!(MockScenario::from_model("mock-model"), None);
    }

    #[test]
    fn test_all_model_ids() {
        let ids = MockScenario::all_model_ids();
        assert_eq!(ids.len(), 12);
        assert!(ids.contains(&"mock/simple-text"));
        assert!(ids.contains(&"mock/tool-call"));
        assert!(ids.contains(&"mock/malformed-sse"));
    }

    #[test]
    fn test_long_text_length() {
        let words = generate_long_text();
        assert!(words.len() >= 500, "long_text must generate 500+ words, got {}", words.len());
    }
}

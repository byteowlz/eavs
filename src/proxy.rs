use crate::keys::{is_virtual_key, ValidatedKey};
use crate::state::{AnalysisEvent, AppState, Injection};
use crate::transform::{
    build_openai_sse_response, parse_incoming_request, ProviderTransformer,
};
use crate::types::StreamState;
use axum::{
    body::Body,
    extract::{Request, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use bytes::Bytes;
use futures::stream::StreamExt;
use serde::Serialize;
use serde_json::Value;
use std::sync::{Arc, Mutex};
use uuid::Uuid;

/// Error response for proxy errors.
#[derive(Serialize)]
struct ProxyError {
    error: ProxyErrorDetail,
}

#[derive(Serialize)]
struct ProxyErrorDetail {
    message: String,
    #[serde(rename = "type")]
    error_type: String,
    code: Option<String>,
}

impl ProxyError {
    fn new(message: impl Into<String>, error_type: impl Into<String>) -> Self {
        Self {
            error: ProxyErrorDetail {
                message: message.into(),
                error_type: error_type.into(),
                code: None,
            },
        }
    }

    fn with_code(mut self, code: impl Into<String>) -> Self {
        self.error.code = Some(code.into());
        self
    }
}

pub async fn proxy_handler(
    State(state): State<AppState>,
    req: Request<Body>,
) -> Result<Response, Response> {
    // 1. Generate Correlation ID
    let correlation_id = Uuid::new_v4().to_string();

    // Extract Authorization header for key validation
    let auth_header = req
        .headers()
        .get("authorization")
        .and_then(|h| h.to_str().ok())
        .and_then(|h| h.strip_prefix("Bearer "))
        .map(|s| s.to_string());

    // Extract conversation ID and provider selection from headers
    let conversation_id = req
        .headers()
        .get("X-Conversation-ID")
        .and_then(|h| h.to_str().ok())
        .map(|s| s.to_string())
        .unwrap_or_else(|| "default".to_string());

    // Allow selecting provider via header (e.g., X-Provider: anthropic)
    let provider_name = req
        .headers()
        .get("X-Provider")
        .and_then(|h| h.to_str().ok())
        .map(|s| s.to_string())
        .unwrap_or_else(|| "default".to_string());

    // 2. Read and modify body if needed (Pre-request Injection)
    let (parts, body) = req.into_parts();
    let bytes = axum::body::to_bytes(body, usize::MAX)
        .await
        .map_err(|_| {
            (
                StatusCode::BAD_REQUEST,
                Json(ProxyError::new("Failed to read request body", "invalid_request")),
            )
                .into_response()
        })?;

    let mut json_body: Value = if !bytes.is_empty() {
        serde_json::from_slice(&bytes).unwrap_or(Value::Null)
    } else {
        Value::Null
    };

    // Extract model from request for validation
    let model = json_body
        .get("model")
        .and_then(|m| m.as_str())
        .unwrap_or("unknown")
        .to_string();

    // 3. Validate virtual API key if present
    // Note: validated_key is used for tracking usage after response completes
    #[allow(unused_variables)]
    let validated_key: Option<ValidatedKey> = if let Some(ref key) = auth_header {
        if is_virtual_key(key) {
            // This is a virtual key - validate it
            if let Some(validator) = state.get_key_validator() {
                // Estimate tokens for rate limiting
                let estimated_tokens = state
                    .get_cost_calculator()
                    .map(|calc| calc.estimate_request_tokens(&json_body, &model))
                    .unwrap_or(0);

                match validator
                    .validate(key, &model, &provider_name, Some(estimated_tokens))
                    .await
                {
                    Ok(validated) => Some(validated),
                    Err(e) => {
                        return Err((
                            StatusCode::from_u16(e.status_code()).unwrap_or(StatusCode::UNAUTHORIZED),
                            Json(ProxyError::new(e.to_string(), "authentication_error")
                                .with_code(e.error_code())),
                        )
                            .into_response());
                    }
                }
            } else {
                // Keys enabled in request but validator not initialized
                return Err((
                    StatusCode::SERVICE_UNAVAILABLE,
                    Json(ProxyError::new(
                        "Virtual API keys are not available",
                        "service_unavailable",
                    )),
                )
                    .into_response());
            }
        } else {
            // Not a virtual key - pass through
            None
        }
    } else {
        None
    };

    // Check for injections (new conversation store)
    let injections = state.conversations.take_injections(&conversation_id);
    if !injections.is_empty() {
        apply_injections(&mut json_body, &injections);
    }

    // Legacy fallback: check old injections map
    if let Some((_, legacy_injections)) = state.injections.remove(&conversation_id) {
        apply_injections(&mut json_body, &legacy_injections);
    }

    // Log Request
    let _ = state.analysis_tx.send(AnalysisEvent::Request {
        timestamp: chrono::Utc::now().timestamp_millis(),
        id: correlation_id.clone(),
        method: parts.method.to_string(),
        uri: parts.uri.to_string(),
        body: json_body.clone(),
    });

    // 4. Get provider configuration
    let provider_config = state
        .config
        .get_provider(&provider_name)
        .ok_or_else(|| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ProxyError::new("Provider not found", "configuration_error")),
            )
                .into_response()
        })?;

    // Use real API key from provider config (virtual key was just for auth)
    let api_key = provider_config.resolved_api_key();
    let provider_type = provider_config.provider_type();

    // Check if we need format translation
    let needs_transform = provider_type.needs_transform();
    
    // Get the transformer for this provider
    let transformer = ProviderTransformer::for_provider(provider_type);
    
    // 4. Build request body - transform if needed
    let (request_body, model_name) = if needs_transform {
        // Parse incoming OpenAI-format request to canonical Context
        let context = parse_incoming_request(&json_body).map_err(|e| {
            tracing::error!("Failed to parse request: {}", e);
            (
                StatusCode::BAD_REQUEST,
                Json(ProxyError::new(format!("Failed to parse request: {}", e), "invalid_request")),
            )
                .into_response()
        })?;
        
        let model = context.model.clone();
        
        // Transform to target provider format
        let transformed = transformer.transform_request(&context).map_err(|e| {
            tracing::error!("Failed to transform request: {}", e);
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ProxyError::new(format!("Failed to transform request: {}", e), "internal_error")),
            )
                .into_response()
        })?;
        
        let body = serde_json::to_vec(&transformed).map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ProxyError::new(format!("Failed to serialize request: {}", e), "internal_error")),
            )
                .into_response()
        })?;
        (body, model)
    } else {
        // Pass through for OpenAI-compatible providers
        let model = json_body["model"].as_str().unwrap_or("unknown").to_string();
        let body = serde_json::to_vec(&json_body).map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ProxyError::new(format!("Failed to serialize request: {}", e), "internal_error")),
            )
                .into_response()
        })?;
        (body, model)
    };

    // Construct URL with transformer's endpoint path if transforming
    let base = provider_config.resolved_base_url();
    let base = base.trim_end_matches('/');
    
    let path = if needs_transform {
        // Use transformer's endpoint path for non-OpenAI providers
        let ctx = parse_incoming_request(&json_body).unwrap_or_default();
        transformer.endpoint_path(&ctx)
    } else {
        parts.uri.path().to_string()
    };
    
    let mut url = format!("{}{}", base, path);

    // Handle Query Parameters (Original + API Version for Azure)
    let mut query_string = if needs_transform {
        String::new() // Transformer includes query params in path if needed
    } else {
        parts.uri.query().map(|s| s.to_string()).unwrap_or_default()
    };

    if let Some(ref ver) = provider_config.api_version {
        if !query_string.is_empty() {
            query_string.push('&');
        }
        query_string.push_str(&format!("api-version={}", ver));
    }

    if !query_string.is_empty() && !url.contains('?') {
        url.push('?');
        url.push_str(&query_string);
    }

    // Build request with provider-specific auth and headers
    let mut upstream_req = state
        .client
        .request(parts.method.clone(), &url)
        .header("Content-Type", "application/json");

    upstream_req = provider_type.apply_auth(upstream_req, &api_key);
    upstream_req = provider_type.apply_extra_headers(upstream_req);

    // Add custom headers from provider config
    for (key, value) in &provider_config.headers {
        let resolved_value = if let Some(var_name) = value.strip_prefix("env:") {
            std::env::var(var_name).unwrap_or_default()
        } else {
            value.clone()
        };
        upstream_req = upstream_req.header(key, resolved_value);
    }

    let upstream_req = upstream_req.body(request_body);

    // Execute Upstream Request
    let upstream_res = upstream_req
        .send()
        .await
        .map_err(|e| {
            tracing::error!("Upstream request failed: {}", e);
            (
                StatusCode::BAD_GATEWAY,
                Json(ProxyError::new(format!("Upstream request failed: {}", e), "upstream_error")),
            )
                .into_response()
        })?;

    // 5. Stream Response with optional transformation
    let status = upstream_res.status();
    let headers = upstream_res.headers().clone();
    let stream = upstream_res.bytes_stream();

    let analysis_tx = state.analysis_tx.clone();
    let correlation_id_clone = correlation_id.clone();

    // Prepare usage tracking state for virtual keys
    let usage_tracker: Option<UsageTracker> = validated_key.as_ref().map(|vk| UsageTracker {
        key_hash: vk.key_hash.clone(),
        model: model.clone(),
        provider: provider_name.clone(),
        input_tokens: Arc::new(std::sync::atomic::AtomicU32::new(0)),
        output_tokens: Arc::new(std::sync::atomic::AtomicU32::new(0)),
        cached_tokens: Arc::new(std::sync::atomic::AtomicU32::new(0)),
    });

    if needs_transform {
        // Transform response from provider format back to OpenAI format
        let stream_state = Arc::new(Mutex::new(StreamState::default()));
        let transformer = Arc::new(ProviderTransformer::for_provider(provider_type));
        let model_for_stream = model_name.clone();
        let request_id = correlation_id.clone();
        let tracker_clone = usage_tracker.clone();
        
        let stream_with_transform = stream.map(move |chunk_result| {
            match chunk_result {
                Ok(chunk) => {
                    let text = String::from_utf8_lossy(&chunk).to_string();
                    
                    // Log original chunk
                    let _ = analysis_tx.send(AnalysisEvent::ResponseChunk {
                        timestamp: chrono::Utc::now().timestamp_millis(),
                        id: correlation_id_clone.clone(),
                        chunk: text.clone(),
                    });
                    
                    // Parse and transform to OpenAI format
                    let mut state = stream_state.lock().unwrap();
                    match transformer.parse_stream_chunk(&text, &mut state) {
                        Ok(events) => {
                            let mut output = String::new();
                            for event in events {
                                // Track usage from Usage event
                                if let crate::types::StreamEvent::Usage { usage } = &event {
                                    if let Some(tracker) = &tracker_clone {
                                        tracker.input_tokens.store(
                                            usage.prompt_tokens,
                                            std::sync::atomic::Ordering::SeqCst,
                                        );
                                        tracker.output_tokens.store(
                                            usage.completion_tokens,
                                            std::sync::atomic::Ordering::SeqCst,
                                        );
                                        tracker.cached_tokens.store(
                                            usage.cache_read_input_tokens.unwrap_or(0),
                                            std::sync::atomic::Ordering::SeqCst,
                                        );
                                    }
                                }
                                output.push_str(&build_openai_sse_response(&event, &request_id, &model_for_stream));
                            }
                            Ok(Bytes::from(output))
                        }
                        Err(e) => {
                            tracing::warn!("Failed to parse response chunk: {}", e);
                            // Pass through on parse error
                            Ok(chunk)
                        }
                    }
                }
                Err(e) => Err(std::io::Error::other(e)),
            }
        });

        let mut response = Response::new(Body::from_stream(stream_with_transform));
        *response.status_mut() = status;
        // Set OpenAI-compatible headers for transformed responses
        response.headers_mut().insert(
            "content-type",
            "text/event-stream".parse().unwrap(),
        );
        response.headers_mut().insert(
            "cache-control",
            "no-cache".parse().unwrap(),
        );
        
        // Schedule usage recording after response completes
        // Note: For streaming, usage is captured above and will be recorded when stream ends
        if let Some(tracker) = usage_tracker {
            let state_clone = state.clone();
            tokio::spawn(async move {
                // Give time for stream to complete and usage to be captured
                tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;
                record_usage_from_tracker(&state_clone, &tracker).await;
            });
        }
        
        Ok(response)
    } else {
        // Pass through without transformation
        let tracker_clone = usage_tracker.clone();
        let stream_with_logging = stream.map(move |chunk_result| match chunk_result {
            Ok(chunk) => {
                let text = String::from_utf8_lossy(&chunk).to_string();
                
                // Try to extract usage from OpenAI-format streaming responses
                if let Some(tracker) = &tracker_clone {
                    extract_openai_usage(&text, tracker);
                }
                
                let _ = analysis_tx.send(AnalysisEvent::ResponseChunk {
                    timestamp: chrono::Utc::now().timestamp_millis(),
                    id: correlation_id_clone.clone(),
                    chunk: text,
                });
                Ok(chunk)
            }
            Err(e) => Err(std::io::Error::other(e)),
        });

        let mut response = Response::new(Body::from_stream(stream_with_logging));
        *response.status_mut() = status;
        *response.headers_mut() = headers;

        // Schedule usage recording after response completes
        if let Some(tracker) = usage_tracker {
            let state_clone = state.clone();
            tokio::spawn(async move {
                // Give time for stream to complete and usage to be captured
                tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;
                record_usage_from_tracker(&state_clone, &tracker).await;
            });
        }

        Ok(response)
    }
}

/// Tracks usage during streaming for virtual keys.
#[derive(Clone)]
struct UsageTracker {
    key_hash: String,
    model: String,
    provider: String,
    input_tokens: Arc<std::sync::atomic::AtomicU32>,
    output_tokens: Arc<std::sync::atomic::AtomicU32>,
    cached_tokens: Arc<std::sync::atomic::AtomicU32>,
}

/// Extract usage from OpenAI-format streaming chunks.
fn extract_openai_usage(chunk: &str, tracker: &UsageTracker) {
    // OpenAI sends usage in the final chunk with stream_options.include_usage=true
    // Format: data: {"id":"...","usage":{"prompt_tokens":10,"completion_tokens":20,...}}
    for line in chunk.lines() {
        if let Some(data) = line.strip_prefix("data: ") {
            if let Ok(json) = serde_json::from_str::<Value>(data) {
                if let Some(usage) = json.get("usage") {
                    if let Some(input) = usage.get("prompt_tokens").and_then(|v| v.as_u64()) {
                        tracker.input_tokens.store(input as u32, std::sync::atomic::Ordering::SeqCst);
                    }
                    if let Some(output) = usage.get("completion_tokens").and_then(|v| v.as_u64()) {
                        tracker.output_tokens.store(output as u32, std::sync::atomic::Ordering::SeqCst);
                    }
                    if let Some(cached) = usage
                        .get("prompt_tokens_details")
                        .and_then(|d| d.get("cached_tokens"))
                        .and_then(|v| v.as_u64())
                    {
                        tracker.cached_tokens.store(cached as u32, std::sync::atomic::Ordering::SeqCst);
                    }
                }
            }
        }
    }
}

/// Record usage from tracker to the key validator.
async fn record_usage_from_tracker(state: &AppState, tracker: &UsageTracker) {
    let input = tracker.input_tokens.load(std::sync::atomic::Ordering::SeqCst);
    let output = tracker.output_tokens.load(std::sync::atomic::Ordering::SeqCst);
    let cached = tracker.cached_tokens.load(std::sync::atomic::Ordering::SeqCst);
    
    // Only record if we have any usage data
    if input == 0 && output == 0 {
        return;
    }
    
    // Calculate cost
    let cost = if let Some(calc) = state.get_cost_calculator() {
        calc.calculate_actual_cost(&tracker.model, input, output, cached).await
    } else {
        0.0
    };
    
    // Record to validator
    if let Some(validator) = state.get_key_validator() {
        validator
            .record_usage(
                &tracker.key_hash,
                input,
                output,
                cached,
                cost,
                &tracker.model,
                &tracker.provider,
            )
            .await;
        
        tracing::debug!(
            key_hash = %tracker.key_hash,
            input_tokens = input,
            output_tokens = output,
            cached_tokens = cached,
            cost_usd = cost,
            "Recorded usage for virtual key"
        );
    }
}

fn apply_injections(json_body: &mut Value, injections: &[Injection]) {
    if let Some(messages) = json_body
        .get_mut("messages")
        .and_then(|m| m.as_array_mut())
    {
        // Insert system messages at the beginning, others at end
        for injection in injections {
            let obj = serde_json::json!({
                "role": injection.role,
                "content": injection.content
            });
            if injection.role == "system" {
                messages.insert(0, obj);
            } else {
                messages.push(obj);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_apply_injections() {
        let mut body = json!({
            "model": "gpt-3.5",
            "messages": [
                {"role": "user", "content": "Hello"}
            ]
        });

        let injections = vec![
            Injection {
                role: "system".to_string(),
                content: "System Prompt".to_string(),
            },
            Injection {
                role: "assistant".to_string(),
                content: "Assistant Response".to_string(),
            },
        ];

        apply_injections(&mut body, &injections);

        let messages = body["messages"].as_array().unwrap();

        // Should have 3 messages now
        assert_eq!(messages.len(), 3);

        // System prompt should be first
        assert_eq!(messages[0]["role"], "system");
        assert_eq!(messages[0]["content"], "System Prompt");

        // User message should be second (preserved)
        assert_eq!(messages[1]["role"], "user");
        assert_eq!(messages[1]["content"], "Hello");

        // Assistant response should be last
        assert_eq!(messages[2]["role"], "assistant");
        assert_eq!(messages[2]["content"], "Assistant Response");
    }

    #[test]
    fn test_apply_injections_no_messages() {
        let mut body = json!({
            "model": "gpt-3.5"
        });

        let injections = vec![Injection {
            role: "system".to_string(),
            content: "System Prompt".to_string(),
        }];

        // Should not panic
        apply_injections(&mut body, &injections);

        // Should stay same
        assert!(body.get("messages").is_none());
    }

    #[test]
    fn test_apply_injections_multiple_system_messages() {
        let mut body = json!({
            "model": "gpt-4",
            "messages": [
                {"role": "user", "content": "Hello"}
            ]
        });

        let injections = vec![
            Injection {
                role: "system".to_string(),
                content: "First system".to_string(),
            },
            Injection {
                role: "system".to_string(),
                content: "Second system".to_string(),
            },
        ];

        apply_injections(&mut body, &injections);

        let messages = body["messages"].as_array().unwrap();

        // Should have 3 messages
        assert_eq!(messages.len(), 3);

        // Both system messages should be at the beginning
        assert_eq!(messages[0]["role"], "system");
        assert_eq!(messages[1]["role"], "system");
        assert_eq!(messages[2]["role"], "user");
    }

    #[test]
    fn test_apply_injections_empty_injections() {
        let mut body = json!({
            "model": "gpt-4",
            "messages": [
                {"role": "user", "content": "Hello"}
            ]
        });

        let injections: Vec<Injection> = vec![];
        apply_injections(&mut body, &injections);

        let messages = body["messages"].as_array().unwrap();
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0]["content"], "Hello");
    }

    #[test]
    fn test_apply_injections_preserves_other_fields() {
        let mut body = json!({
            "model": "gpt-4",
            "temperature": 0.7,
            "max_tokens": 1000,
            "messages": [
                {"role": "user", "content": "Hello"}
            ]
        });

        let injections = vec![Injection {
            role: "system".to_string(),
            content: "Be concise".to_string(),
        }];

        apply_injections(&mut body, &injections);

        // Other fields should be preserved
        assert_eq!(body["model"], "gpt-4");
        assert_eq!(body["temperature"], 0.7);
        assert_eq!(body["max_tokens"], 1000);
    }

    #[test]
    fn test_apply_injections_with_existing_system_message() {
        let mut body = json!({
            "model": "gpt-4",
            "messages": [
                {"role": "system", "content": "Original system"},
                {"role": "user", "content": "Hello"}
            ]
        });

        let injections = vec![Injection {
            role: "system".to_string(),
            content: "Injected system".to_string(),
        }];

        apply_injections(&mut body, &injections);

        let messages = body["messages"].as_array().unwrap();

        // Should have 3 messages now
        assert_eq!(messages.len(), 3);

        // Injected system should be first
        assert_eq!(messages[0]["content"], "Injected system");
        // Original system second
        assert_eq!(messages[1]["content"], "Original system");
        // User message last
        assert_eq!(messages[2]["content"], "Hello");
    }

    #[test]
    fn test_apply_injections_user_and_assistant_roles() {
        let mut body = json!({
            "model": "gpt-4",
            "messages": [
                {"role": "user", "content": "Original user"}
            ]
        });

        let injections = vec![
            Injection {
                role: "user".to_string(),
                content: "Injected user".to_string(),
            },
            Injection {
                role: "assistant".to_string(),
                content: "Injected assistant".to_string(),
            },
        ];

        apply_injections(&mut body, &injections);

        let messages = body["messages"].as_array().unwrap();

        // Should have 3 messages
        assert_eq!(messages.len(), 3);

        // Original user first
        assert_eq!(messages[0]["content"], "Original user");
        // Injected user second (appended)
        assert_eq!(messages[1]["content"], "Injected user");
        // Injected assistant last (appended)
        assert_eq!(messages[2]["content"], "Injected assistant");
    }

    #[test]
    fn test_apply_injections_messages_not_array() {
        let mut body = json!({
            "model": "gpt-4",
            "messages": "not an array"
        });

        let injections = vec![Injection {
            role: "system".to_string(),
            content: "System".to_string(),
        }];

        // Should not panic, just do nothing
        apply_injections(&mut body, &injections);

        // Body should remain unchanged
        assert_eq!(body["messages"], "not an array");
    }

    #[test]
    fn test_apply_injections_null_body() {
        let mut body = Value::Null;

        let injections = vec![Injection {
            role: "system".to_string(),
            content: "System".to_string(),
        }];

        // Should not panic
        apply_injections(&mut body, &injections);

        assert!(body.is_null());
    }
}

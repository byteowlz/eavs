use crate::state::{AnalysisEvent, AppState, Injection};
use crate::transform::{
    build_openai_sse_response, parse_incoming_request, ProviderTransformer,
};
use crate::types::StreamState;
use axum::{
    body::Body,
    extract::{Request, State},
    http::StatusCode,
    response::Response,
};
use bytes::Bytes;
use futures::stream::StreamExt;
use serde_json::Value;
use std::sync::{Arc, Mutex};
use uuid::Uuid;

pub async fn proxy_handler(
    State(state): State<AppState>,
    req: Request<Body>,
) -> Result<Response, StatusCode> {
    // 1. Generate Correlation ID
    let correlation_id = Uuid::new_v4().to_string();

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
        .map_err(|_| StatusCode::BAD_REQUEST)?;

    let mut json_body: Value = if !bytes.is_empty() {
        serde_json::from_slice(&bytes).unwrap_or(Value::Null)
    } else {
        Value::Null
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

    // 3. Get provider configuration
    let provider_config = state
        .config
        .get_provider(&provider_name)
        .ok_or(StatusCode::INTERNAL_SERVER_ERROR)?;

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
            StatusCode::BAD_REQUEST
        })?;
        
        let model = context.model.clone();
        
        // Transform to target provider format
        let transformed = transformer.transform_request(&context).map_err(|e| {
            tracing::error!("Failed to transform request: {}", e);
            StatusCode::INTERNAL_SERVER_ERROR
        })?;
        
        (serde_json::to_vec(&transformed).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?, model)
    } else {
        // Pass through for OpenAI-compatible providers
        let model = json_body["model"].as_str().unwrap_or("unknown").to_string();
        (serde_json::to_vec(&json_body).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?, model)
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
            StatusCode::BAD_GATEWAY
        })?;

    // 5. Stream Response with optional transformation
    let status = upstream_res.status();
    let headers = upstream_res.headers().clone();
    let stream = upstream_res.bytes_stream();

    let analysis_tx = state.analysis_tx.clone();
    let correlation_id_clone = correlation_id.clone();

    if needs_transform {
        // Transform response from provider format back to OpenAI format
        let stream_state = Arc::new(Mutex::new(StreamState::default()));
        let transformer = Arc::new(ProviderTransformer::for_provider(provider_type));
        let model_for_stream = model_name.clone();
        let request_id = correlation_id.clone();
        
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
        
        Ok(response)
    } else {
        // Pass through without transformation
        let stream_with_logging = stream.map(move |chunk_result| match chunk_result {
            Ok(chunk) => {
                let text = String::from_utf8_lossy(&chunk).to_string();
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

        Ok(response)
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

use crate::config::get_api_key;
use crate::state::{AnalysisEvent, AppState, Injection};
use axum::{
    body::Body,
    extract::{State, Request},
    response::Response,
    http::StatusCode,
};
use futures::stream::StreamExt;
use serde_json::Value;
use uuid::Uuid;

pub async fn proxy_handler(
    State(state): State<AppState>,
    req: Request<Body>,
) -> Result<Response, StatusCode> {
    // 1. Generate Correlation ID
    let correlation_id = Uuid::new_v4().to_string();
    let conversation_id = req.headers()
        .get("X-Conversation-ID")
        .and_then(|h| h.to_str().ok())
        .map(|s| s.to_string())
        .unwrap_or_else(|| "default".to_string());

    // 2. Read and modify body if needed (Pre-request Injection)
    let (parts, body) = req.into_parts();
    let bytes = axum::body::to_bytes(body, usize::MAX).await.map_err(|_| StatusCode::BAD_REQUEST)?;
    
    let mut json_body: Value = if !bytes.is_empty() {
         serde_json::from_slice(&bytes).unwrap_or(Value::Null)
    } else {
        Value::Null
    };

    // Check for injections
    if let Some((_, injections)) = state.injections.remove(&conversation_id) {
        apply_injections(&mut json_body, &injections);
    }

    // Log Request
    let _ = state.analysis_tx.send(AnalysisEvent::Request {
        timestamp: chrono::Utc::now().timestamp_millis(),
        id: correlation_id.clone(),
        method: parts.method.to_string(),
        uri: parts.uri.to_string(),
        body: json_body.clone(),
    });

    // Re-serialize body
    let new_body_bytes = serde_json::to_vec(&json_body).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    // 3. Prepare Upstream Request
    // Assuming "default" upstream for MVP
    let upstream_config = state.config.upstream.get("default").ok_or(StatusCode::INTERNAL_SERVER_ERROR)?;
    let api_key = get_api_key(&upstream_config.api_key);
    let url = format!("{}{}", upstream_config.base_url, parts.uri.path());

    let upstream_req = state.client
        .request(parts.method.clone(), url)
        .header("Content-Type", "application/json")
        .header("Authorization", format!("Bearer {}", api_key))
        .body(new_body_bytes);

    // Copy headers (excluding host, auth, etc if needed, but keeping it simple)
    // Ideally we shouldn't blindly copy all headers, but for transparency...
    
    // Execute Upstream Request
    let upstream_res = upstream_req.send().await.map_err(|_| StatusCode::BAD_GATEWAY)?;

    // 4. Stream Response
    let status = upstream_res.status();
    let headers = upstream_res.headers().clone();
    let stream = upstream_res.bytes_stream();

    let analysis_tx = state.analysis_tx.clone();
    let correlation_id_clone = correlation_id.clone();

    let stream_with_logging = stream.map(move |chunk_result| {
        match chunk_result {
            Ok(chunk) => {
                // Log chunk
                // Note: Raw bytes might not be valid UTF-8 string if split mid-character, 
                // but for logging tokens we usually assume it works out or we handle it carefully.
                // For MVP, lossy conversion is acceptable for the "live analysis" log.
                let text = String::from_utf8_lossy(&chunk).to_string();
                let _ = analysis_tx.send(AnalysisEvent::ResponseChunk {
                    timestamp: chrono::Utc::now().timestamp_millis(),
                    id: correlation_id_clone.clone(),
                    chunk: text,
                });
                Ok(chunk)
            },
            Err(e) => Err(std::io::Error::new(std::io::ErrorKind::Other, e)),
        }
    });

    let mut response = Response::new(Body::from_stream(stream_with_logging));
    *response.status_mut() = status;
    // Copy headers from upstream to downstream
    *response.headers_mut() = headers;

    Ok(response)
}

fn apply_injections(json_body: &mut Value, injections: &[Injection]) {
    if let Some(messages) = json_body.get_mut("messages").and_then(|m| m.as_array_mut()) {
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
            Injection { role: "system".to_string(), content: "System Prompt".to_string() },
            Injection { role: "assistant".to_string(), content: "Assistant Response".to_string() },
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
        
        let injections = vec![
            Injection { role: "system".to_string(), content: "System Prompt".to_string() },
        ];
        
        // Should not panic
        apply_injections(&mut body, &injections);
        
        // Should stay same
        assert!(body.get("messages").is_none());
    }
}

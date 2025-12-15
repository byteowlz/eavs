use crate::keys::{is_virtual_key, ValidatedKey};
use crate::aws_sigv4::{sign_request_headers, AwsCredentials};
use crate::provider::{AuthStyle, ProviderType};
use crate::state::{AnalysisEvent, AppState, Injection};
use crate::transform::{
    build_openai_sse_response, parse_incoming_request, ProviderTransformer, TransformError,
};
use crate::types::{ContentBlock, Context, ImageContent, Message, StreamState};
use crate::upstream::{Upstream, UpstreamRequest};
use axum::{
    body::Body,
    extract::{
        ws::{Message as AxumWsMessage, WebSocketUpgrade},
        OriginalUri, Request, State,
    },
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use base64::Engine;
use bytes::Bytes;
use futures::{stream::StreamExt, SinkExt};
use serde::Serialize;
use serde_json::Value;
use std::sync::{Arc, Mutex};
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::Message as TungsteniteMessage;
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

    // Apply policy rules (deny/rewrite/filter) after injection.
    if let Err(err) = state
        .config
        .policy
        .apply(&provider_name, parts.uri.path(), &mut json_body)
    {
        return Err((
            StatusCode::FORBIDDEN,
            Json(ProxyError::new(err.message, "policy_violation")),
        )
            .into_response());
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
    let provider_lookup = state
        .config
        .resolve_provider(&provider_name)
        .ok_or_else(|| {
            let available = state.config.provider_names();
            (
                StatusCode::BAD_REQUEST,
                Json(ProxyError::new(
                    format!(
                        "Unknown provider '{}'. Available providers: {:?}",
                        provider_name, available
                    ),
                    "invalid_provider",
                )),
            )
                .into_response()
        })?;

    let provider_config = provider_lookup.config;
    let resolved_provider = provider_lookup.resolved_name.clone();
    
    // Log if provider name was normalized or fell back
    if provider_lookup.was_fallback {
        tracing::info!(
            requested = %provider_name,
            resolved = %resolved_provider,
            "Provider name was empty, using default"
        );
    } else if provider_name != resolved_provider {
        tracing::debug!(
            requested = %provider_name,
            resolved = %resolved_provider,
            "Provider name normalized"
        );
    }

    // Use real API key from provider config (virtual key was just for auth)
    let api_key = provider_config.resolved_api_key();
    let provider_type = provider_config.provider_type();

    // Check if we need format translation
    let needs_transform = provider_type.needs_transform();
    
    // Get the transformer for this provider
    let transformer = ProviderTransformer::for_provider(provider_type);
    
    // 4. Build request body - transform if needed
    let mut transformed_endpoint_path: Option<String> = None;
    let mut request_stream = false;
    let (request_body, model_name) = if needs_transform {
        // Parse incoming OpenAI-format request to canonical Context
        let mut context = parse_incoming_request(&json_body).map_err(|e| {
            tracing::error!("Failed to parse request: {}", e);
            (
                StatusCode::BAD_REQUEST,
                Json(ProxyError::new(format!("Failed to parse request: {}", e), "invalid_request")),
            )
                .into_response()
        })?;

        request_stream = context.stream;

        // Resolve URL images for providers that require inline/base64 image data.
        if matches!(
            provider_type,
            ProviderType::Anthropic | ProviderType::Google | ProviderType::Bedrock
        ) {
            resolve_image_urls_in_context(state.upstream.as_ref(), &mut context)
                .await
                .map_err(|e| {
                    (
                        StatusCode::BAD_REQUEST,
                        Json(ProxyError::new(
                            format!("Failed to resolve image URL: {}", e),
                            "invalid_request",
                        )),
                    )
                        .into_response()
                })?;
        }
        
        let model = context.model.clone();
        transformed_endpoint_path = Some(transformer.endpoint_path(&context));
        
        // Transform to target provider format
        let transformed = transformer.transform_request(&context).map_err(|e| {
            let status = match e {
                TransformError::InvalidJson(_)
                | TransformError::MissingField(_)
                | TransformError::InvalidValue(_)
                | TransformError::Unsupported(_) => StatusCode::BAD_REQUEST,
            };
            let error_type = match e {
                TransformError::Unsupported(_) => "unsupported",
                _ => "invalid_request",
            };

            tracing::error!("Failed to transform request: {}", e);
            (status, Json(ProxyError::new(format!("{}", e), error_type))).into_response()
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
        transformed_endpoint_path.unwrap_or_else(|| "/v1/chat/completions".to_string())
    } else {
        // For OpenAI-compatible pass-through, strip /v1 prefix when base URL already has it.
        // Azure OpenAI is deployment-based; treat `model` as deployment name when base_url is
        // the resource endpoint (no `/openai/deployments/...` path).
        let request_path = parts.uri.path();
        let stripped_path = if (provider_type == ProviderType::Azure || base.ends_with("/v1"))
            && request_path.starts_with("/v1")
        {
            request_path.strip_prefix("/v1").unwrap_or(request_path)
        } else {
            request_path
        };

        if provider_type == ProviderType::Azure && !base.contains("/openai/deployments/") {
            // Use explicit deployment name if configured, otherwise fall back to model name
            let deployment = provider_config
                .resolved_deployment()
                .unwrap_or_else(|| model_name.clone());
            format!("/openai/deployments/{}{}", deployment, stripped_path)
        } else {
            stripped_path.to_string()
        }
    };
    
    let mut url = format!("{}{}", base, path);

    // Handle Query Parameters (Original + API Version for Azure)
    let mut query_string = if needs_transform {
        String::new() // Transformer includes query params in path if needed
    } else {
        parts.uri.query().map(|s| s.to_string()).unwrap_or_default()
    };

    if let Some(ref ver) = provider_config.resolved_api_version() {
        if !query_string.is_empty() {
            query_string.push('&');
        }
        query_string.push_str(&format!("api-version={}", ver));
    }

    if !query_string.is_empty() && !url.contains('?') {
        url.push('?');
        url.push_str(&query_string);
    }

    let request_body = Bytes::from(request_body);

    // Build upstream request
    let mut upstream_headers = HeaderMap::new();
    upstream_headers.insert(
        http::header::CONTENT_TYPE,
        http::HeaderValue::from_static("application/json"),
    );

    apply_http_auth_headers(&mut upstream_headers, provider_type, &api_key);
    apply_http_extra_headers(&mut upstream_headers, provider_type);

    // Add custom headers from provider config
    for (key, value) in &provider_config.headers {
        let resolved_value = if let Some(var_name) = value.strip_prefix("env:") {
            std::env::var(var_name).unwrap_or_default()
        } else {
            value.clone()
        };
        if let (Ok(name), Ok(val)) = (
            http::header::HeaderName::from_bytes(key.as_bytes()),
            http::header::HeaderValue::from_str(&resolved_value),
        ) {
            upstream_headers.insert(name, val);
        }
    }

    if provider_type == ProviderType::Bedrock {
        let region = provider_config
            .resolved_aws_region()
            .ok_or_else(|| {
                (
                    StatusCode::BAD_REQUEST,
                    Json(ProxyError::new(
                        "Bedrock provider requires aws_region (or AWS_REGION)".to_string(),
                        "invalid_request",
                    )),
                )
                    .into_response()
            })?;

        let access_key_id = provider_config
            .resolved_aws_access_key_id()
            .ok_or_else(|| {
                (
                    StatusCode::BAD_REQUEST,
                    Json(ProxyError::new(
                        "Bedrock provider requires aws_access_key_id (or AWS_ACCESS_KEY_ID)"
                            .to_string(),
                        "invalid_request",
                    )),
                )
                    .into_response()
            })?;

        let secret_access_key = provider_config
            .resolved_aws_secret_access_key()
            .ok_or_else(|| {
                (
                    StatusCode::BAD_REQUEST,
                    Json(ProxyError::new(
                        "Bedrock provider requires aws_secret_access_key (or AWS_SECRET_ACCESS_KEY)"
                            .to_string(),
                        "invalid_request",
                    )),
                )
                    .into_response()
            })?;

        let creds = AwsCredentials {
            access_key_id,
            secret_access_key,
            session_token: provider_config.resolved_aws_session_token(),
        };

        let service = provider_config.resolved_aws_service();

        sign_request_headers(
            &mut upstream_headers,
            &parts.method,
            &url,
            request_body.as_ref(),
            &region,
            &service,
            chrono::Utc::now(),
            &creds,
        )
        .map_err(|e| {
            (
                StatusCode::BAD_REQUEST,
                Json(ProxyError::new(
                    format!("Failed to sign Bedrock request: {}", e),
                    "invalid_request",
                )),
            )
                .into_response()
        })?;
    }

    let upstream_req = UpstreamRequest {
        method: parts.method.clone(),
        url: url.clone(),
        headers: upstream_headers,
        body: request_body,
    };

    tracing::debug!("Upstream URL: {}", url);

    // Execute Upstream Request
    let upstream_res = state.upstream.send(upstream_req).await.map_err(|e| {
        tracing::error!("Upstream request failed: {}", e);
        (
            StatusCode::BAD_GATEWAY,
            Json(ProxyError::new(
                format!("Upstream request failed: {}", e),
                "upstream_error",
            )),
        )
            .into_response()
    })?;

    // 5. Stream Response with optional transformation
    let status = upstream_res.status;
    let headers = upstream_res.headers.clone();
    let stream = upstream_res.body;

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
        if request_stream {
            let stream_state = Arc::new(Mutex::new(StreamState::default()));
            let transformer = Arc::new(ProviderTransformer::for_provider(provider_type));
            let model_for_stream = model_name.clone();
            let request_id = correlation_id.clone();
            let tracker_clone = usage_tracker.clone();
            let analysis_tx_stream = analysis_tx.clone();
            let correlation_id_stream = correlation_id_clone.clone();

            let stream_with_transform = stream.map(move |chunk_result| match chunk_result {
                Ok(chunk) => {
                    let text = String::from_utf8_lossy(&chunk).to_string();

                    // Log original chunk
                    let _ = analysis_tx_stream.send(AnalysisEvent::ResponseChunk {
                        timestamp: chrono::Utc::now().timestamp_millis(),
                        id: correlation_id_stream.clone(),
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
                                output.push_str(&build_openai_sse_response(
                                    &event,
                                    &request_id,
                                    &model_for_stream,
                                ));
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
                Err(e) => Err(e),
            });

            let mut response = Response::new(Body::from_stream(stream_with_transform));
            *response.status_mut() = status;
            response
                .headers_mut()
                .insert("content-type", "text/event-stream".parse().unwrap());
            response
                .headers_mut()
                .insert("cache-control", "no-cache".parse().unwrap());
            response.headers_mut().insert(
                "x-eavs-provider",
                resolved_provider.parse().unwrap(),
            );

            // Schedule usage recording after response completes
            if let Some(tracker) = usage_tracker.clone() {
                let state_clone = state.clone();
                tokio::spawn(async move {
                    tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;
                    record_usage_from_tracker(&state_clone, &tracker).await;
                });
            }

            Ok(response)
        } else {
            // Non-streaming transform: parse full JSON response and translate to OpenAI JSON.
            let body_bytes = collect_stream_bytes(stream).await.map_err(|e| {
                (
                    StatusCode::BAD_GATEWAY,
                    Json(ProxyError::new(
                        format!("Failed to read upstream response: {}", e),
                        "upstream_error",
                    )),
                )
                    .into_response()
            })?;

            let text = String::from_utf8_lossy(&body_bytes).to_string();
            let _ = analysis_tx.send(AnalysisEvent::ResponseChunk {
                timestamp: chrono::Utc::now().timestamp_millis(),
                id: correlation_id_clone.clone(),
                chunk: text,
            });

            let json_body: Value = serde_json::from_slice(&body_bytes).map_err(|e| {
                (
                    StatusCode::BAD_GATEWAY,
                    Json(ProxyError::new(
                        format!("Invalid upstream JSON: {}", e),
                        "upstream_error",
                    )),
                )
                    .into_response()
            })?;

            let transformer = ProviderTransformer::for_provider(provider_type);
            let events = transformer.parse_response(&json_body).map_err(|e| {
                (
                    StatusCode::BAD_GATEWAY,
                    Json(ProxyError::new(
                        format!("Failed to parse upstream response: {}", e),
                        "upstream_error",
                    )),
                )
                    .into_response()
            })?;

            let response_json =
                build_openai_chat_completion_from_events(&events, &correlation_id, &model_name);
            let bytes = serde_json::to_vec(&response_json).map_err(|e| {
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(ProxyError::new(
                        format!("Failed to serialize response: {}", e),
                        "internal_error",
                    )),
                )
                    .into_response()
            })?;

            let mut response = Response::new(Body::from(bytes));
            *response.status_mut() = status;
            response.headers_mut().insert(
                http::header::CONTENT_TYPE,
                http::HeaderValue::from_static("application/json"),
            );
            response.headers_mut().insert(
                "x-eavs-provider",
                resolved_provider.parse().unwrap(),
            );

            if let Some(tracker) = usage_tracker.clone() {
                if let Some(usage) = response_json.get("usage") {
                    if let Some(input) = usage.get("prompt_tokens").and_then(|v| v.as_u64()) {
                        tracker.input_tokens.store(
                            input as u32,
                            std::sync::atomic::Ordering::SeqCst,
                        );
                    }
                    if let Some(output) = usage.get("completion_tokens").and_then(|v| v.as_u64()) {
                        tracker.output_tokens.store(
                            output as u32,
                            std::sync::atomic::Ordering::SeqCst,
                        );
                    }
                }

                let state_clone = state.clone();
                tokio::spawn(async move {
                    record_usage_from_tracker(&state_clone, &tracker).await;
                });
            }

            Ok(response)
        }
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
            Err(e) => Err(e),
        });

        let mut response = Response::new(Body::from_stream(stream_with_logging));
        *response.status_mut() = status;
        *response.headers_mut() = headers;
        // Add header indicating which provider was used
        response.headers_mut().insert(
            "x-eavs-provider",
            resolved_provider.parse().unwrap(),
        );

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

async fn collect_stream_bytes(
    mut stream: futures::stream::BoxStream<'static, Result<Bytes, std::io::Error>>,
) -> Result<Bytes, std::io::Error> {
    let mut collected = Vec::new();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk?;
        collected.extend_from_slice(&chunk);
    }
    Ok(Bytes::from(collected))
}

fn build_openai_chat_completion_from_events(events: &[crate::types::StreamEvent], request_id: &str, model: &str) -> Value {
    use crate::types::{StopReason, StreamEvent};

    let mut assistant = crate::types::AssistantMessage::default();
    for event in events {
        if let StreamEvent::Done { message, .. } = event {
            assistant = message.clone();
        }
    }

    let mut content = String::new();
    let mut tool_calls = Vec::new();

    for block in &assistant.content {
        match block {
            ContentBlock::Text(t) => content.push_str(&t.text),
            ContentBlock::Thinking(t) => {
                content.push_str("<thinking>\n");
                content.push_str(&t.thinking);
                content.push_str("\n</thinking>");
            }
            ContentBlock::ToolCall(tc) => tool_calls.push(serde_json::json!({
                "id": tc.id,
                "type": "function",
                "function": {
                    "name": tc.name,
                    "arguments": tc.arguments.to_string()
                }
            })),
            _ => {}
        }
    }

    let finish_reason = match assistant.stop_reason {
        StopReason::EndTurn => "stop",
        StopReason::StopSequence => "stop",
        StopReason::MaxTokens => "length",
        StopReason::ToolUse => "tool_calls",
        StopReason::ContentFilter => "content_filter",
        StopReason::Other => "stop",
    };

    let mut message_obj = serde_json::json!({
        "role": "assistant",
        "content": content
    });
    if !tool_calls.is_empty() {
        message_obj["tool_calls"] = Value::Array(tool_calls);
    }

    serde_json::json!({
        "id": request_id,
        "object": "chat.completion",
        "created": chrono::Utc::now().timestamp(),
        "model": model,
        "choices": [{
            "index": 0,
            "message": message_obj,
            "finish_reason": finish_reason
        }],
        "usage": {
            "prompt_tokens": assistant.usage.prompt_tokens,
            "completion_tokens": assistant.usage.completion_tokens,
            "total_tokens": assistant.usage.total_tokens
        }
    })
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

pub async fn ws_proxy_handler(
    State(state): State<AppState>,
    ws: WebSocketUpgrade,
    headers: HeaderMap,
    OriginalUri(uri): OriginalUri,
) -> Response {
    let conversation_id = headers
        .get("X-Conversation-ID")
        .and_then(|h| h.to_str().ok())
        .unwrap_or("default")
        .to_string();

    let provider_name = headers
        .get("X-Provider")
        .and_then(|h| h.to_str().ok())
        .unwrap_or("default")
        .to_string();

    let provider_lookup = match state.config.resolve_provider(&provider_name) {
        Some(v) => v,
        None => {
            let available = state.config.provider_names();
            return (
                StatusCode::BAD_REQUEST,
                Json(ProxyError::new(
                    format!(
                        "Unknown provider '{}'. Available providers: {:?}",
                        provider_name, available
                    ),
                    "invalid_provider",
                )),
            )
                .into_response();
        }
    };

    let provider_config = provider_lookup.config.clone();
    let provider_type = provider_config.provider_type();
    let api_key = provider_config.resolved_api_key();

    let upstream_url = match build_ws_upstream_url(
        &provider_config.resolved_base_url(),
        uri.path(),
        uri.query(),
    ) {
        Ok(u) => u,
        Err(e) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(ProxyError::new(e, "invalid_request")),
            )
                .into_response();
        }
    };

    ws.on_upgrade(move |mut client_socket| async move {
        let (session_token, mut injection_rx) = state.ws_sessions.register(&conversation_id);
        let _session_guard = WsSessionGuard {
            conversation_id: conversation_id.clone(),
            token: session_token,
            sessions: state.ws_sessions.clone(),
        };

        let mut request = match upstream_url.into_client_request() {
            Ok(r) => r,
            Err(_) => {
                let _ = client_socket.send(AxumWsMessage::Close(None)).await;
                return;
            }
        };

        apply_ws_auth_headers(&mut request, provider_type, &api_key);
        apply_ws_extra_headers(&mut request, provider_type);

        // Custom provider headers
        for (key, value) in &provider_config.headers {
            let resolved_value = if let Some(var_name) = value.strip_prefix("env:") {
                std::env::var(var_name).unwrap_or_default()
            } else {
                value.clone()
            };
            if let (Ok(name), Ok(val)) = (
                http::header::HeaderName::from_bytes(key.as_bytes()),
                http::header::HeaderValue::from_str(&resolved_value),
            ) {
                request.headers_mut().insert(name, val);
            }
        }

        let (upstream_socket, _) = match tokio_tungstenite::connect_async(request).await {
            Ok(pair) => pair,
            Err(_) => {
                let _ = client_socket.send(AxumWsMessage::Close(None)).await;
                return;
            }
        };

        let (mut client_sender, mut client_receiver) = client_socket.split();
        let (mut upstream_sender, mut upstream_receiver) = upstream_socket.split();

        // Single writer task for upstream.
        let (upstream_tx, mut upstream_rx) = mpsc::unbounded_channel::<TungsteniteMessage>();
        let upstream_write = tokio::spawn(async move {
            while let Some(msg) = upstream_rx.recv().await {
                if upstream_sender.send(msg).await.is_err() {
                    break;
                }
            }
        });

        // Forward client -> upstream.
        let upstream_tx_client = upstream_tx.clone();
        let client_to_upstream = tokio::spawn(async move {
            while let Some(Ok(msg)) = client_receiver.next().await {
                let Some(up_msg) = axum_to_tungstenite(msg) else {
                    continue;
                };
                let is_close = matches!(up_msg, TungsteniteMessage::Close(_));
                let _ = upstream_tx_client.send(up_msg);
                if is_close {
                    break;
                }
            }
        });

        // Forward upstream -> client.
        let upstream_to_client = tokio::spawn(async move {
            while let Some(Ok(msg)) = upstream_receiver.next().await {
                let Some(client_msg) = tungstenite_to_axum(msg) else {
                    continue;
                };
                let is_close = matches!(client_msg, AxumWsMessage::Close(_));
                if client_sender.send(client_msg).await.is_err() {
                    break;
                }
                if is_close {
                    break;
                }
            }
        });

        // Injection -> upstream (OpenAI Realtime semantics).
        let inject_to_upstream = tokio::spawn(async move {
            while let Some(injections) = injection_rx.recv().await {
                for inj in injections {
                    let event = serde_json::json!({
                        "type": "conversation.item.create",
                        "item": {
                            "type": "message",
                            "role": inj.role,
                            "content": [{
                                "type": "input_text",
                                "text": inj.content
                            }]
                        }
                    });
                    let _ = upstream_tx.send(TungsteniteMessage::Text(event.to_string()));
                }
            }
        });

        let _ = tokio::join!(upstream_write, client_to_upstream, upstream_to_client, inject_to_upstream);
    })
    .into_response()
}

struct WsSessionGuard {
    conversation_id: String,
    token: crate::state::WsSessionToken,
    sessions: Arc<crate::state::WsSessionManager>,
}

impl Drop for WsSessionGuard {
    fn drop(&mut self) {
        self.sessions.unregister(&self.conversation_id, self.token);
    }
}

fn axum_to_tungstenite(msg: AxumWsMessage) -> Option<TungsteniteMessage> {
    match msg {
        AxumWsMessage::Text(t) => Some(TungsteniteMessage::Text(t)),
        AxumWsMessage::Binary(b) => Some(TungsteniteMessage::Binary(b)),
        AxumWsMessage::Ping(b) => Some(TungsteniteMessage::Ping(b)),
        AxumWsMessage::Pong(b) => Some(TungsteniteMessage::Pong(b)),
        AxumWsMessage::Close(_) => Some(TungsteniteMessage::Close(None)),
    }
}

fn tungstenite_to_axum(msg: TungsteniteMessage) -> Option<AxumWsMessage> {
    match msg {
        TungsteniteMessage::Text(t) => Some(AxumWsMessage::Text(t)),
        TungsteniteMessage::Binary(b) => Some(AxumWsMessage::Binary(b)),
        TungsteniteMessage::Ping(b) => Some(AxumWsMessage::Ping(b)),
        TungsteniteMessage::Pong(b) => Some(AxumWsMessage::Pong(b)),
        TungsteniteMessage::Close(_) => Some(AxumWsMessage::Close(None)),
        TungsteniteMessage::Frame(_) => None,
    }
}

fn build_ws_upstream_url(
    base_url: &str,
    request_path: &str,
    query: Option<&str>,
) -> Result<String, String> {
    let mut base = base_url.trim_end_matches('/').to_string();

    // Mirror the HTTP behavior: if base already ends with /v1, strip the incoming /v1 prefix.
    let path = if base.ends_with("/v1") && request_path.starts_with("/v1") {
        request_path.strip_prefix("/v1").unwrap_or(request_path)
    } else {
        request_path
    };

    base.push_str(path);

    if let Some(q) = query {
        if !q.is_empty() {
            base.push('?');
            base.push_str(q);
        }
    }

    if base.starts_with("https://") {
        Ok(base.replacen("https://", "wss://", 1))
    } else if base.starts_with("http://") {
        Ok(base.replacen("http://", "ws://", 1))
    } else if base.starts_with("ws://") || base.starts_with("wss://") {
        Ok(base)
    } else {
        Err(format!("Unsupported upstream base_url scheme: {}", base_url))
    }
}

fn apply_ws_auth_headers(
    request: &mut http::Request<()>,
    provider_type: ProviderType,
    api_key: &str,
) {
    if api_key.is_empty() {
        return;
    }

    match provider_type.info().auth_style {
        AuthStyle::BearerToken => {
            let _ = request.headers_mut().insert(
                http::header::AUTHORIZATION,
                http::HeaderValue::from_str(&format!("Bearer {}", api_key))
                    .unwrap_or_else(|_| http::HeaderValue::from_static("")),
            );
        }
        AuthStyle::ApiKeyHeader(name) => {
            let Ok(hname) = http::header::HeaderName::from_bytes(name.as_bytes()) else {
                return;
            };
            let _ = request
                .headers_mut()
                .insert(hname, http::HeaderValue::from_str(api_key).unwrap());
        }
        AuthStyle::AzureApiKey => {
            let _ = request.headers_mut().insert(
                http::header::HeaderName::from_static("api-key"),
                http::HeaderValue::from_str(api_key).unwrap(),
            );
        }
        AuthStyle::QueryParam(_) | AuthStyle::None => {}
    }
}

fn apply_ws_extra_headers(request: &mut http::Request<()>, provider_type: ProviderType) {
    match provider_type {
        ProviderType::Anthropic => {
            let _ = request.headers_mut().insert(
                http::header::HeaderName::from_static("anthropic-version"),
                http::HeaderValue::from_static("2023-06-01"),
            );
        }
        ProviderType::OpenRouter => {
            let _ = request.headers_mut().insert(
                http::header::HeaderName::from_static("http-referer"),
                http::HeaderValue::from_static("https://github.com/eavs-proxy"),
            );
        }
        _ => {}
    }
}

fn apply_http_auth_headers(headers: &mut HeaderMap, provider_type: ProviderType, api_key: &str) {
    if api_key.is_empty() {
        return;
    }

    match provider_type.info().auth_style {
        AuthStyle::BearerToken => {
            let _ = headers.insert(
                http::header::AUTHORIZATION,
                http::HeaderValue::from_str(&format!("Bearer {}", api_key))
                    .unwrap_or_else(|_| http::HeaderValue::from_static("")),
            );
        }
        AuthStyle::ApiKeyHeader(name) => {
            let Ok(hname) = http::header::HeaderName::from_bytes(name.as_bytes()) else {
                return;
            };
            let _ = headers.insert(hname, http::HeaderValue::from_str(api_key).unwrap());
        }
        AuthStyle::AzureApiKey => {
            let _ = headers.insert(
                http::header::HeaderName::from_static("api-key"),
                http::HeaderValue::from_str(api_key).unwrap(),
            );
        }
        AuthStyle::QueryParam(_) | AuthStyle::None => {}
    }
}

fn apply_http_extra_headers(headers: &mut HeaderMap, provider_type: ProviderType) {
    match provider_type {
        ProviderType::Anthropic => {
            let _ = headers.insert(
                http::header::HeaderName::from_static("anthropic-version"),
                http::HeaderValue::from_static("2023-06-01"),
            );
        }
        ProviderType::OpenRouter => {
            let _ = headers.insert(
                http::header::HeaderName::from_static("http-referer"),
                http::HeaderValue::from_static("https://github.com/eavs-proxy"),
            );
        }
        _ => {}
    }
}

async fn resolve_image_urls_in_context(
    upstream: &dyn Upstream,
    context: &mut Context,
) -> Result<(), String> {
    let mut urls = Vec::new();

    for msg in &context.messages {
        let blocks = match msg {
            Message::User(u) => &u.content,
            Message::Assistant(a) => &a.content,
            Message::Tool(t) => &t.content,
            Message::System(_) => continue,
        };

        for block in blocks {
            if let ContentBlock::Image(img) = block {
                if img.is_url {
                    urls.push(img.data.clone());
                }
            }
        }
    }

    if urls.is_empty() {
        return Ok(());
    }

    for url in urls {
        let (mime_type, data) = fetch_image_to_base64(upstream, &url).await?;

        // Rewrite all occurrences of this URL in the context.
        for msg in &mut context.messages {
            let blocks = match msg {
                Message::User(u) => &mut u.content,
                Message::Assistant(a) => &mut a.content,
                Message::Tool(t) => &mut t.content,
                Message::System(_) => continue,
            };

            for block in blocks {
                if let ContentBlock::Image(img) = block {
                    if img.is_url && img.data == url {
                        *img = ImageContent::base64(data.clone(), mime_type.clone());
                    }
                }
            }
        }
    }

    Ok(())
}

async fn fetch_image_to_base64(
    upstream: &dyn Upstream,
    url: &str,
) -> Result<(String, String), String> {
    const MAX_IMAGE_BYTES: usize = 10 * 1024 * 1024;
    const TIMEOUT_SECS: u64 = 10;

    let parsed = url::Url::parse(url).map_err(|e| format!("invalid url: {}", e))?;
    match parsed.scheme() {
        "http" | "https" => {}
        _ => return Err(format!("unsupported image url scheme: {}", parsed.scheme())),
    }

    let (headers, body) = upstream
        .get_bytes(
            url,
            std::time::Duration::from_secs(TIMEOUT_SECS),
            MAX_IMAGE_BYTES,
        )
        .await
        .map_err(|e| e.to_string())?;

    let mime_type = headers
        .get(http::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.split(';').next())
        .map(|v| v.trim().to_string())
        .or_else(|| guess_mime_from_url(url));

    let mime_type = mime_type.unwrap_or_else(|| "application/octet-stream".to_string());
    let encoded = base64::engine::general_purpose::STANDARD.encode(body);
    Ok((mime_type, encoded))
}

fn guess_mime_from_url(url: &str) -> Option<String> {
    let Ok(parsed) = url::Url::parse(url) else {
        return None;
    };
    let path = parsed.path().to_lowercase();
    if path.ends_with(".png") {
        Some("image/png".to_string())
    } else if path.ends_with(".jpg") || path.ends_with(".jpeg") {
        Some("image/jpeg".to_string())
    } else if path.ends_with(".webp") {
        Some("image/webp".to_string())
    } else if path.ends_with(".gif") {
        Some("image/gif".to_string())
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api;
    use crate::config::{AppConfig, ProviderConfig};
    use crate::upstream::{UpstreamResponse};
    use axum::routing::{any, post};
    use axum::Router;
    use bytes::Bytes;
    use futures::{stream, StreamExt};
    use http::{HeaderMap, Method, StatusCode};
    use std::collections::HashMap;
    use std::sync::Arc;
    use tokio::sync::Mutex;
    use tower::util::ServiceExt;
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

    #[derive(Clone)]
    struct MockUpstream {
        requests: Arc<Mutex<Vec<crate::upstream::UpstreamRequest>>>,
        responses: Arc<Mutex<Vec<ResponseSpec>>>,
        images: Arc<HashMap<String, (HeaderMap, Bytes, StatusCode)>>,
    }

    #[derive(Clone)]
    struct ResponseSpec {
        status: StatusCode,
        headers: HeaderMap,
        chunks: Vec<Bytes>,
    }

    impl MockUpstream {
        fn new(responses: Vec<ResponseSpec>) -> Self {
            Self {
                requests: Arc::new(Mutex::new(Vec::new())),
                responses: Arc::new(Mutex::new(responses)),
                images: Arc::new(HashMap::new()),
            }
        }

        fn with_images(mut self, images: HashMap<String, (HeaderMap, Bytes, StatusCode)>) -> Self {
            self.images = Arc::new(images);
            self
        }

        async fn take_requests(&self) -> Vec<crate::upstream::UpstreamRequest> {
            std::mem::take(&mut *self.requests.lock().await)
        }
    }

    impl crate::upstream::Upstream for MockUpstream {
        fn send<'a>(
            &'a self,
            request: crate::upstream::UpstreamRequest,
        ) -> futures::future::BoxFuture<'a, Result<UpstreamResponse, std::io::Error>> {
            Box::pin(async move {
                // Record request for assertions.
                {
                    let mut guard = self.requests.lock().await;
                    guard.push(crate::upstream::UpstreamRequest {
                        method: request.method.clone(),
                        url: request.url.clone(),
                        headers: request.headers.clone(),
                        body: request.body.clone(),
                    });
                }

                // Serve image fetches directly if configured.
                if request.method == Method::GET {
                    if let Some((headers, body, status)) = self.images.get(&request.url) {
                        let chunks = vec![body.clone()];
                        return Ok(UpstreamResponse {
                            status: *status,
                            headers: headers.clone(),
                            body: stream::iter(chunks.into_iter())
                                .then(|chunk| async move {
                                    tokio::task::yield_now().await;
                                    Ok(chunk)
                                })
                                .boxed(),
                        });
                    }
                }

                let mut responses = self.responses.lock().await;
                let spec = responses.remove(0);
                Ok(UpstreamResponse {
                    status: spec.status,
                    headers: spec.headers,
                    body: stream::iter(spec.chunks.into_iter())
                        .then(|chunk| async move {
                            tokio::task::yield_now().await;
                            Ok(chunk)
                        })
                        .boxed(),
                })
            })
        }
    }

    fn make_config(providers: HashMap<String, ProviderConfig>) -> AppConfig {
        AppConfig {
            server: Default::default(),
            providers,
            upstream: HashMap::new(),
            logging: Default::default(),
            analysis: Default::default(),
            policy: Default::default(),
            state: Default::default(),
            keys: Default::default(),
        }
    }

    #[tokio::test]
    async fn e2e_passthrough_and_injection_without_network() {
        let response_json = json!({
            "id": "test",
            "object": "chat.completion",
            "created": 0,
            "model": "gpt-4o-mini",
            "choices": [{
                "index": 0,
                "message": {"role": "assistant", "content": "ok"},
                "finish_reason": "stop"
            }],
            "usage": {"prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2}
        });

        let mock = MockUpstream::new(vec![ResponseSpec {
            status: StatusCode::OK,
            headers: HeaderMap::new(),
            chunks: vec![Bytes::from(serde_json::to_vec(&response_json).unwrap())],
        }]);

        let mut providers = HashMap::new();
        providers.insert(
            "default".to_string(),
            ProviderConfig {
                type_: "openai".to_string(),
                base_url: "http://upstream/v1".to_string(),
                ..Default::default()
            },
        );

        let state = AppState::new_with_upstream(make_config(providers), Arc::new(mock.clone()));
        let app = Router::new()
            .route("/inject/:conversation_id", post(api::inject_handler))
            .route("/v1/*path", any(proxy_handler))
            .with_state(state);

        // Queue injection
        let inj = json!({"messages":[{"role":"system","content":"You are injected"}]});
        let req = http::Request::builder()
            .method("POST")
            .uri("/inject/conv1")
            .header(http::header::CONTENT_TYPE, "application/json")
            .body(Body::from(serde_json::to_vec(&inj).unwrap()))
            .unwrap();
        let resp = app.clone().oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        // Proxy request
        let req_body = json!({
            "model": "gpt-4o-mini",
            "messages": [{"role":"user","content":"hello"}]
        });
        let req = http::Request::builder()
            .method("POST")
            .uri("/v1/chat/completions")
            .header(http::header::CONTENT_TYPE, "application/json")
            .header("X-Conversation-ID", "conv1")
            .body(Body::from(serde_json::to_vec(&req_body).unwrap()))
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        let requests = mock.take_requests().await;
        assert_eq!(requests.len(), 1);
        let sent: serde_json::Value = serde_json::from_slice(&requests[0].body).unwrap();
        let msgs = sent["messages"].as_array().unwrap();
        assert_eq!(msgs[0]["role"], "system");
        assert_eq!(msgs[0]["content"], "You are injected");
        assert_eq!(msgs[1]["role"], "user");
    }

    #[tokio::test]
    async fn e2e_provider_routing_changes_upstream_url() {
        let response_json = json!({"choices":[{"message":{"role":"assistant","content":"ok"}}]});
        let mock = MockUpstream::new(vec![
            ResponseSpec {
                status: StatusCode::OK,
                headers: HeaderMap::new(),
                chunks: vec![Bytes::from(serde_json::to_vec(&response_json).unwrap())],
            },
            ResponseSpec {
                status: StatusCode::OK,
                headers: HeaderMap::new(),
                chunks: vec![Bytes::from(serde_json::to_vec(&response_json).unwrap())],
            },
        ]);

        let mut providers = HashMap::new();
        providers.insert(
            "default".to_string(),
            ProviderConfig {
                type_: "openai".to_string(),
                base_url: "http://up1/v1".to_string(),
                ..Default::default()
            },
        );
        providers.insert(
            "alt".to_string(),
            ProviderConfig {
                type_: "openai".to_string(),
                base_url: "http://up2/v1".to_string(),
                ..Default::default()
            },
        );

        let state = AppState::new_with_upstream(make_config(providers), Arc::new(mock.clone()));
        let app = Router::new()
            .route("/v1/*path", any(proxy_handler))
            .with_state(state);

        let req_body = json!({"model":"gpt-4o-mini","messages":[{"role":"user","content":"hi"}]});

        let req1 = http::Request::builder()
            .method("POST")
            .uri("/v1/chat/completions")
            .header(http::header::CONTENT_TYPE, "application/json")
            .header("X-Provider", "alt")
            .body(Body::from(serde_json::to_vec(&req_body).unwrap()))
            .unwrap();
        let _ = app.clone().oneshot(req1).await.unwrap();

        let req2 = http::Request::builder()
            .method("POST")
            .uri("/v1/chat/completions")
            .header(http::header::CONTENT_TYPE, "application/json")
            .header("X-Provider", "default")
            .body(Body::from(serde_json::to_vec(&req_body).unwrap()))
            .unwrap();
        let _ = app.oneshot(req2).await.unwrap();

        let requests = mock.take_requests().await;
        assert_eq!(requests.len(), 2);
        assert!(requests[0].url.starts_with("http://up2/"));
        assert!(requests[1].url.starts_with("http://up1/"));
    }

    #[tokio::test]
    async fn e2e_streaming_passthrough_forwards_sse_payload() {
        let mut upstream_headers = HeaderMap::new();
        upstream_headers.insert(
            http::header::CONTENT_TYPE,
            http::HeaderValue::from_static("text/event-stream"),
        );

        let mock = MockUpstream::new(vec![ResponseSpec {
            status: StatusCode::OK,
            headers: upstream_headers,
            chunks: vec![
                Bytes::from_static(b"data: {\"choices\":[{\"delta\":{\"content\":\"hi\"}}]}\n\n"),
                Bytes::from_static(b"data: [DONE]\n\n"),
            ],
        }]);

        let mut providers = HashMap::new();
        providers.insert(
            "default".to_string(),
            ProviderConfig {
                type_: "openai".to_string(),
                base_url: "http://up/v1".to_string(),
                ..Default::default()
            },
        );

        let state = AppState::new_with_upstream(make_config(providers), Arc::new(mock));
        let app = Router::new()
            .route("/v1/*path", any(proxy_handler))
            .with_state(state);

        let req_body = json!({
            "model":"gpt-4o-mini",
            "messages":[{"role":"user","content":"hi"}],
            "stream": true
        });
        let req = http::Request::builder()
            .method("POST")
            .uri("/v1/chat/completions")
            .header(http::header::CONTENT_TYPE, "application/json")
            .body(Body::from(serde_json::to_vec(&req_body).unwrap()))
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX).await.unwrap();
        let text = String::from_utf8_lossy(&body);
        assert!(text.contains("data: [DONE]"));
        assert!(text.contains("\"hi\""));
    }

    #[tokio::test]
    async fn e2e_error_forwarding() {
        let mock = MockUpstream::new(vec![ResponseSpec {
            status: StatusCode::TOO_MANY_REQUESTS,
            headers: HeaderMap::new(),
            chunks: vec![Bytes::from_static(b"{\"error\":{\"message\":\"rate limited\"}}")],
        }]);

        let mut providers = HashMap::new();
        providers.insert(
            "default".to_string(),
            ProviderConfig {
                type_: "openai".to_string(),
                base_url: "http://up/v1".to_string(),
                ..Default::default()
            },
        );

        let state = AppState::new_with_upstream(make_config(providers), Arc::new(mock));
        let app = Router::new()
            .route("/v1/*path", any(proxy_handler))
            .with_state(state);

        let req_body = json!({"model":"gpt-4o-mini","messages":[{"role":"user","content":"hi"}]});
        let req = http::Request::builder()
            .method("POST")
            .uri("/v1/chat/completions")
            .header(http::header::CONTENT_TYPE, "application/json")
            .body(Body::from(serde_json::to_vec(&req_body).unwrap()))
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::TOO_MANY_REQUESTS);
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX).await.unwrap();
        assert!(String::from_utf8_lossy(&body).contains("rate limited"));
    }

    #[tokio::test]
    async fn e2e_logging_emits_request_and_chunk() {
        let mock = MockUpstream::new(vec![ResponseSpec {
            status: StatusCode::OK,
            headers: HeaderMap::new(),
            chunks: vec![Bytes::from_static(b"hello")],
        }]);

        let mut providers = HashMap::new();
        providers.insert(
            "default".to_string(),
            ProviderConfig {
                type_: "openai".to_string(),
                base_url: "http://up/v1".to_string(),
                ..Default::default()
            },
        );

        let state = AppState::new_with_upstream(make_config(providers), Arc::new(mock));
        let mut rx = state.analysis_tx.subscribe();

        let app = Router::new()
            .route("/v1/*path", any(proxy_handler))
            .with_state(state);

        let req_body = json!({"model":"gpt-4o-mini","messages":[{"role":"user","content":"hi"}]});
        let req = http::Request::builder()
            .method("POST")
            .uri("/v1/chat/completions")
            .header(http::header::CONTENT_TYPE, "application/json")
            .body(Body::from(serde_json::to_vec(&req_body).unwrap()))
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        let _ = axum::body::to_bytes(resp.into_body(), usize::MAX).await.unwrap();

        let mut saw_request = false;
        let mut saw_chunk = false;
        for _ in 0..10 {
            match tokio::time::timeout(std::time::Duration::from_millis(200), rx.recv()).await {
                Ok(Ok(evt)) => match evt {
                    AnalysisEvent::Request { .. } => saw_request = true,
                    AnalysisEvent::ResponseChunk { .. } => {
                        saw_chunk = true;
                        break;
                    }
                    _ => {}
                },
                _ => {}
            }
        }
        assert!(saw_request);
        assert!(saw_chunk);
    }

    #[tokio::test]
    async fn vision_url_images_are_fetched_for_inline_providers() {
        let img_url = "http://images.test/img.png".to_string();

        let mut img_headers = HeaderMap::new();
        img_headers.insert(
            http::header::CONTENT_TYPE,
            http::HeaderValue::from_static("image/png"),
        );
        let mut images = HashMap::new();
        images.insert(
            img_url.clone(),
            (img_headers, Bytes::from_static(&[1u8, 2, 3]), StatusCode::OK),
        );

        let anthropic_response = json!({
            "id": "msg_123",
            "type": "message",
            "role": "assistant",
            "content": [{"type":"text","text":"ok"}],
            "model": "claude-3-opus",
            "stop_reason": "end_turn",
            "usage": {"input_tokens": 1, "output_tokens": 1}
        });

        let mock = MockUpstream::new(vec![ResponseSpec {
            status: StatusCode::OK,
            headers: HeaderMap::new(),
            chunks: vec![Bytes::from(serde_json::to_vec(&anthropic_response).unwrap())],
        }])
        .with_images(images);

        let mut providers = HashMap::new();
        providers.insert(
            "default".to_string(),
            ProviderConfig {
                type_: "anthropic".to_string(),
                base_url: "http://anthropic.local".to_string(),
                ..Default::default()
            },
        );

        let state = AppState::new_with_upstream(make_config(providers), Arc::new(mock.clone()));
        let app = Router::new()
            .route("/v1/*path", any(proxy_handler))
            .with_state(state);

        let req_body = json!({
            "model": "claude-3-opus",
            "messages": [{
                "role":"user",
                "content":[
                    {"type":"text","text":"Describe this"},
                    {"type":"image_url","image_url":{"url": img_url}}
                ]
            }]
        });
        let req = http::Request::builder()
            .method("POST")
            .uri("/v1/chat/completions")
            .header(http::header::CONTENT_TYPE, "application/json")
            .header("X-Provider", "default")
            .body(Body::from(serde_json::to_vec(&req_body).unwrap()))
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        // Ensure upstream request to Anthropic had base64 inline image data (AQID for [1,2,3]).
        let requests = mock.take_requests().await;
        assert!(requests.iter().any(|r| r.method == Method::GET && r.url == "http://images.test/img.png"));

        let post = requests.iter().find(|r| r.method == Method::POST).unwrap();
        let sent: serde_json::Value = serde_json::from_slice(&post.body).unwrap();
        assert_eq!(sent["messages"][0]["content"][1]["type"], "image");
        assert_eq!(sent["messages"][0]["content"][1]["source"]["data"], "AQID");
    }

    #[tokio::test]
    async fn e2e_non_streaming_transform_returns_openai_json() {
        let anthropic_response = json!({
            "id": "msg_123",
            "type": "message",
            "role": "assistant",
            "content": [{"type":"text","text":"ok"}],
            "model": "claude-3-opus",
            "stop_reason": "end_turn",
            "usage": {"input_tokens": 1, "output_tokens": 1}
        });

        let mock = MockUpstream::new(vec![ResponseSpec {
            status: StatusCode::OK,
            headers: HeaderMap::new(),
            chunks: vec![Bytes::from(serde_json::to_vec(&anthropic_response).unwrap())],
        }]);

        let mut providers = HashMap::new();
        providers.insert(
            "default".to_string(),
            ProviderConfig {
                type_: "anthropic".to_string(),
                base_url: "http://anthropic.local".to_string(),
                ..Default::default()
            },
        );

        let state = AppState::new_with_upstream(make_config(providers), Arc::new(mock));
        let app = Router::new()
            .route("/v1/*path", any(proxy_handler))
            .with_state(state);

        let req_body = json!({
            "model":"claude-3-opus",
            "messages":[{"role":"user","content":"hi"}],
            "stream": false
        });
        let req = http::Request::builder()
            .method("POST")
            .uri("/v1/chat/completions")
            .header(http::header::CONTENT_TYPE, "application/json")
            .header("X-Provider", "default")
            .body(Body::from(serde_json::to_vec(&req_body).unwrap()))
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX).await.unwrap();
        let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(json["choices"][0]["message"]["content"], "ok");
    }

    #[tokio::test]
    async fn e2e_bedrock_request_is_sigv4_signed() {
        let bedrock_response = json!({
            "id": "msg_123",
            "type": "message",
            "role": "assistant",
            "content": [{"type":"text","text":"ok"}],
            "model": "anthropic.claude-3-opus-20240229-v1:0",
            "stop_reason": "end_turn",
            "usage": {"input_tokens": 1, "output_tokens": 1}
        });

        let mock = MockUpstream::new(vec![ResponseSpec {
            status: StatusCode::OK,
            headers: HeaderMap::new(),
            chunks: vec![Bytes::from(serde_json::to_vec(&bedrock_response).unwrap())],
        }]);

        let mut providers = HashMap::new();
        providers.insert(
            "default".to_string(),
            ProviderConfig {
                type_: "bedrock".to_string(),
                aws_region: "us-east-1".to_string(),
                aws_access_key_id: "AKIDEXAMPLE".to_string(),
                aws_secret_access_key: "secret".to_string(),
                ..Default::default()
            },
        );

        let state = AppState::new_with_upstream(make_config(providers), Arc::new(mock.clone()));
        let app = Router::new()
            .route("/v1/*path", any(proxy_handler))
            .with_state(state);

        let req_body = json!({
            "model":"anthropic.claude-3-opus-20240229-v1:0",
            "messages":[{"role":"user","content":"hi"}],
            "stream": false
        });
        let req = http::Request::builder()
            .method("POST")
            .uri("/v1/chat/completions")
            .header(http::header::CONTENT_TYPE, "application/json")
            .header("X-Provider", "default")
            .body(Body::from(serde_json::to_vec(&req_body).unwrap()))
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX).await.unwrap();
        let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(json["choices"][0]["message"]["content"], "ok");

        let requests = mock.take_requests().await;
        assert_eq!(requests.len(), 1);
        assert_eq!(
            requests[0].url,
            "https://bedrock-runtime.us-east-1.amazonaws.com/model/anthropic.claude-3-opus-20240229-v1:0/invoke"
        );

        let auth = requests[0]
            .headers
            .get(http::header::AUTHORIZATION)
            .unwrap()
            .to_str()
            .unwrap();
        assert!(auth.starts_with("AWS4-HMAC-SHA256 "));
        assert!(auth.contains("Credential=AKIDEXAMPLE/"));
        assert!(requests[0]
            .headers
            .contains_key(http::header::HeaderName::from_static("x-amz-date")));
        assert!(requests[0]
            .headers
            .contains_key(http::header::HeaderName::from_static("x-amz-content-sha256")));

        let sent: serde_json::Value = serde_json::from_slice(&requests[0].body).unwrap();
        assert_eq!(sent["anthropic_version"], "bedrock-2023-05-31");
        assert!(sent.get("model").is_none());
    }

    #[tokio::test]
    async fn e2e_bedrock_missing_credentials_returns_400() {
        let mock = MockUpstream::new(vec![]);

        let mut providers = HashMap::new();
        providers.insert(
            "default".to_string(),
            ProviderConfig {
                type_: "bedrock".to_string(),
                aws_region: "us-east-1".to_string(),
                ..Default::default()
            },
        );

        let state = AppState::new_with_upstream(make_config(providers), Arc::new(mock.clone()));
        let app = Router::new()
            .route("/v1/*path", any(proxy_handler))
            .with_state(state);

        let req_body = json!({
            "model":"anthropic.claude-3-opus-20240229-v1:0",
            "messages":[{"role":"user","content":"hi"}],
            "stream": false
        });
        let req = http::Request::builder()
            .method("POST")
            .uri("/v1/chat/completions")
            .header(http::header::CONTENT_TYPE, "application/json")
            .header("X-Provider", "default")
            .body(Body::from(serde_json::to_vec(&req_body).unwrap()))
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);

        let requests = mock.take_requests().await;
        assert!(requests.is_empty());
    }

    #[tokio::test]
    async fn e2e_azure_base_url_env_is_resolved_and_deployment_path_is_added() {
        std::env::set_var("AZURE_OPENAI_ENDPOINT", "https://azure.example.com/");

        let response_json = json!({
            "id": "test",
            "object": "chat.completion",
            "created": 0,
            "model": "gpt-4o-mini",
            "choices": [{
                "index": 0,
                "message": {"role": "assistant", "content": "ok"},
                "finish_reason": "stop"
            }]
        });

        let mock = MockUpstream::new(vec![ResponseSpec {
            status: StatusCode::OK,
            headers: HeaderMap::new(),
            chunks: vec![Bytes::from(serde_json::to_vec(&response_json).unwrap())],
        }]);

        let mut providers = HashMap::new();
        providers.insert(
            "default".to_string(),
            ProviderConfig {
                type_: "azure".to_string(),
                api_key: "azure-key".to_string(),
                base_url: "env:AZURE_OPENAI_ENDPOINT".to_string(),
                api_version: Some("2025-03-01-preview".to_string()),
                ..Default::default()
            },
        );

        let state = AppState::new_with_upstream(make_config(providers), Arc::new(mock.clone()));
        let app = Router::new()
            .route("/v1/*path", any(proxy_handler))
            .with_state(state);

        let req_body = json!({
            "model":"gpt-4o-mini",
            "messages":[{"role":"user","content":"hi"}],
            "stream": false
        });
        let req = http::Request::builder()
            .method("POST")
            .uri("/v1/chat/completions")
            .header(http::header::CONTENT_TYPE, "application/json")
            .header("X-Provider", "default")
            .body(Body::from(serde_json::to_vec(&req_body).unwrap()))
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        let requests = mock.take_requests().await;
        assert_eq!(requests.len(), 1);
        assert_eq!(
            requests[0].url,
            "https://azure.example.com/openai/deployments/gpt-4o-mini/chat/completions?api-version=2025-03-01-preview"
        );
        assert_eq!(
            requests[0]
                .headers
                .get(http::header::HeaderName::from_static("api-key"))
                .unwrap()
                .to_str()
                .unwrap(),
            "azure-key"
        );

        std::env::remove_var("AZURE_OPENAI_ENDPOINT");
    }

    #[tokio::test]
    async fn stress_100_concurrent_non_streaming_requests() {
        let response_json = json!({
            "id": "test",
            "object": "chat.completion",
            "created": 0,
            "model": "gpt-4o-mini",
            "choices": [{
                "index": 0,
                "message": {"role": "assistant", "content": "ok"},
                "finish_reason": "stop"
            }]
        });

        let mut responses = Vec::new();
        for _ in 0..100 {
            responses.push(ResponseSpec {
                status: StatusCode::OK,
                headers: HeaderMap::new(),
                chunks: vec![Bytes::from(serde_json::to_vec(&response_json).unwrap())],
            });
        }

        let mock = MockUpstream::new(responses);
        let mut providers = HashMap::new();
        providers.insert(
            "default".to_string(),
            ProviderConfig {
                type_: "openai".to_string(),
                base_url: "http://up/v1".to_string(),
                ..Default::default()
            },
        );
        let state = AppState::new_with_upstream(make_config(providers), Arc::new(mock.clone()));
        let app = Router::new()
            .route("/v1/*path", any(proxy_handler))
            .with_state(state);

        let req_body = serde_json::to_vec(&json!({
            "model": "gpt-4o-mini",
            "messages": [{"role":"user","content":"hi"}],
            "stream": false
        }))
        .unwrap();

        let tasks = (0..100).map(|_| {
            let app = app.clone();
            let req_body = req_body.clone();
            async move {
                let req = http::Request::builder()
                    .method("POST")
                    .uri("/v1/chat/completions")
                    .header(http::header::CONTENT_TYPE, "application/json")
                    .body(Body::from(req_body))
                    .unwrap();
                let resp = app.oneshot(req).await.unwrap();
                let status = resp.status();
                let _ = axum::body::to_bytes(resp.into_body(), usize::MAX).await.unwrap();
                status
            }
        });

        let statuses = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            futures::future::join_all(tasks),
        )
        .await
        .unwrap();

        assert!(statuses.iter().all(|s| *s == StatusCode::OK));
        let requests = mock.take_requests().await;
        assert_eq!(requests.len(), 100);
    }

    #[tokio::test]
    async fn stress_50_concurrent_streaming_requests() {
        let mut upstream_headers = HeaderMap::new();
        upstream_headers.insert(
            http::header::CONTENT_TYPE,
            http::HeaderValue::from_static("text/event-stream"),
        );

        let mut responses = Vec::new();
        for _ in 0..50 {
            responses.push(ResponseSpec {
                status: StatusCode::OK,
                headers: upstream_headers.clone(),
                chunks: vec![
                    Bytes::from_static(b"data: {\"choices\":[{\"delta\":{\"content\":\"hi\"}}]}\n\n"),
                    Bytes::from_static(b"data: [DONE]\n\n"),
                ],
            });
        }

        let mock = MockUpstream::new(responses);
        let mut providers = HashMap::new();
        providers.insert(
            "default".to_string(),
            ProviderConfig {
                type_: "openai".to_string(),
                base_url: "http://up/v1".to_string(),
                ..Default::default()
            },
        );
        let state = AppState::new_with_upstream(make_config(providers), Arc::new(mock));
        let app = Router::new()
            .route("/v1/*path", any(proxy_handler))
            .with_state(state);

        let req_body = serde_json::to_vec(&json!({
            "model": "gpt-4o-mini",
            "messages": [{"role":"user","content":"hi"}],
            "stream": true
        }))
        .unwrap();

        let tasks = (0..50).map(|_| {
            let app = app.clone();
            let req_body = req_body.clone();
            async move {
                let req = http::Request::builder()
                    .method("POST")
                    .uri("/v1/chat/completions")
                    .header(http::header::CONTENT_TYPE, "application/json")
                    .body(Body::from(req_body))
                    .unwrap();
                let resp = app.oneshot(req).await.unwrap();
                assert_eq!(resp.status(), StatusCode::OK);
                let body = axum::body::to_bytes(resp.into_body(), usize::MAX).await.unwrap();
                let text = String::from_utf8_lossy(&body);
                assert!(text.contains("data: [DONE]"));
            }
        });

        tokio::time::timeout(
            std::time::Duration::from_secs(5),
            futures::future::join_all(tasks),
        )
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn stress_rapid_connect_disconnect_cycles() {
        let mut upstream_headers = HeaderMap::new();
        upstream_headers.insert(
            http::header::CONTENT_TYPE,
            http::HeaderValue::from_static("text/event-stream"),
        );

        let mut responses = Vec::new();
        for _ in 0..200 {
            responses.push(ResponseSpec {
                status: StatusCode::OK,
                headers: upstream_headers.clone(),
                chunks: vec![Bytes::from_static(b"data: [DONE]\n\n")],
            });
        }

        let mock = MockUpstream::new(responses);
        let mut providers = HashMap::new();
        providers.insert(
            "default".to_string(),
            ProviderConfig {
                type_: "openai".to_string(),
                base_url: "http://up/v1".to_string(),
                ..Default::default()
            },
        );
        let state = AppState::new_with_upstream(make_config(providers), Arc::new(mock));
        let app = Router::new()
            .route("/v1/*path", any(proxy_handler))
            .with_state(state);

        let req_body = serde_json::to_vec(&json!({
            "model": "gpt-4o-mini",
            "messages": [{"role":"user","content":"hi"}],
            "stream": true
        }))
        .unwrap();

        for _ in 0..200 {
            let req = http::Request::builder()
                .method("POST")
                .uri("/v1/chat/completions")
                .header(http::header::CONTENT_TYPE, "application/json")
                .body(Body::from(req_body.clone()))
                .unwrap();
            let resp = app.clone().oneshot(req).await.unwrap();
            drop(resp);
        }
    }

    #[tokio::test]
    async fn stress_injection_during_concurrent_streaming() {
        let mut upstream_headers = HeaderMap::new();
        upstream_headers.insert(
            http::header::CONTENT_TYPE,
            http::HeaderValue::from_static("text/event-stream"),
        );

        let mut responses = Vec::new();
        for _ in 0..20 {
            let mut chunks = Vec::new();
            for _ in 0..200 {
                chunks.push(Bytes::from_static(b"data: {\"choices\":[{\"delta\":{\"content\":\".\"}}]}\n\n"));
            }
            chunks.push(Bytes::from_static(b"data: [DONE]\n\n"));
            responses.push(ResponseSpec {
                status: StatusCode::OK,
                headers: upstream_headers.clone(),
                chunks,
            });
        }

        let mock = MockUpstream::new(responses);
        let mut providers = HashMap::new();
        providers.insert(
            "default".to_string(),
            ProviderConfig {
                type_: "openai".to_string(),
                base_url: "http://up/v1".to_string(),
                ..Default::default()
            },
        );
        let state = AppState::new_with_upstream(make_config(providers), Arc::new(mock));

        let (_token, mut rx) = state.ws_sessions.register("conv-stress");
        let app = Router::new()
            .route("/inject/:conversation_id", post(api::inject_handler))
            .route("/v1/*path", any(proxy_handler))
            .with_state(state);

        let stream_body = serde_json::to_vec(&json!({
            "model": "gpt-4o-mini",
            "messages": [{"role":"user","content":"hi"}],
            "stream": true
        }))
        .unwrap();

        let stream_tasks = (0..20).map(|_| {
            let app = app.clone();
            let stream_body = stream_body.clone();
            async move {
                let req = http::Request::builder()
                    .method("POST")
                    .uri("/v1/chat/completions")
                    .header(http::header::CONTENT_TYPE, "application/json")
                    .header("X-Conversation-ID", "conv-stress")
                    .body(Body::from(stream_body))
                    .unwrap();
                let resp = app.oneshot(req).await.unwrap();
                assert_eq!(resp.status(), StatusCode::OK);
                let _ = axum::body::to_bytes(resp.into_body(), usize::MAX).await.unwrap();
            }
        });

        let inject_task = {
            let app = app.clone();
            async move {
                tokio::task::yield_now().await;
                let inj = json!({"messages":[{"role":"system","content":"Injected mid-stream"}]});
                let req = http::Request::builder()
                    .method("POST")
                    .uri("/inject/conv-stress")
                    .header(http::header::CONTENT_TYPE, "application/json")
                    .body(Body::from(serde_json::to_vec(&inj).unwrap()))
                    .unwrap();
                let resp = app.oneshot(req).await.unwrap();
                assert_eq!(resp.status(), StatusCode::OK);
            }
        };

        tokio::time::timeout(
            std::time::Duration::from_secs(5),
            async move {
                tokio::join!(futures::future::join_all(stream_tasks), inject_task);
            },
        )
        .await
        .unwrap();

        let delivered = tokio::time::timeout(std::time::Duration::from_millis(500), rx.recv())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(delivered[0].role, "system");
        assert_eq!(delivered[0].content, "Injected mid-stream");
    }

    #[tokio::test]
    async fn inject_delivers_to_active_ws_sessions() {
        let mock = MockUpstream::new(vec![]);
        let mut providers = HashMap::new();
        providers.insert(
            "default".to_string(),
            ProviderConfig {
                type_: "openai".to_string(),
                base_url: "http://up/v1".to_string(),
                ..Default::default()
            },
        );
        let state = AppState::new_with_upstream(make_config(providers), Arc::new(mock));

        let (_token, mut rx) = state.ws_sessions.register("convws");
        let app = Router::new()
            .route("/inject/:conversation_id", post(api::inject_handler))
            .with_state(state.clone());

        let inj = json!({"messages":[{"role":"system","content":"Injected mid-stream"}]});
        let req = http::Request::builder()
            .method("POST")
            .uri("/inject/convws")
            .header(http::header::CONTENT_TYPE, "application/json")
            .body(Body::from(serde_json::to_vec(&inj).unwrap()))
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        let delivered = tokio::time::timeout(std::time::Duration::from_millis(200), rx.recv())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(delivered[0].role, "system");
        assert_eq!(delivered[0].content, "Injected mid-stream");
    }

    #[test]
    fn ws_upstream_url_builder_converts_http_to_ws() {
        let url = build_ws_upstream_url("https://api.example.com/v1", "/v1/realtime", Some("model=x"))
            .unwrap();
        assert_eq!(url, "wss://api.example.com/v1/realtime?model=x");
    }
}

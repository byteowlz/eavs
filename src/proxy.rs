use crate::aws_sigv4::{sign_request_headers, AwsCredentials};
use crate::keys::{is_virtual_key, ValidatedKey};
use crate::oauth::{
    anthropic as oauth_anthropic, google as oauth_google, openai_codex as oauth_openai,
    OAuthCredentials, OAuthProvider as OAuthProviderKind,
};
use crate::provider::{
    detect_provider_from_host, detect_provider_from_model, AuthStyle, ProviderType,
};
use crate::state::{AnalysisEvent, AppState, Injection};
use crate::transform::{
    build_openai_sse_response, parse_incoming_request, ProviderTransformer, TransformError,
};
use crate::types::{ContentBlock, Context, ImageContent, Message, StreamState};
use crate::upstream::{Upstream, UpstreamRequest, UpstreamResponse};
use axum::{
    body::Body,
    extract::{
        ws::{Message as AxumWsMessage, WebSocketUpgrade},
        OriginalUri, Path, Request, State,
    },
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use base64::Engine;
use bytes::Bytes;
use futures::{stream::StreamExt, SinkExt};
use serde::Serialize;
use serde_json::{json, Value};
use std::collections::BTreeSet;
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

/// Handler for provider-prefixed routes: /{provider}/v1/*
///
/// This allows clients to explicitly select a provider via the URL path:
/// - POST /openai/v1/chat/completions
/// - POST /anthropic/v1/chat/completions
/// - POST /azure/v1/chat/completions
pub async fn provider_proxy_handler(
    State(state): State<AppState>,
    Path((provider, _path)): Path<(String, String)>,
    req: Request<Body>,
) -> Result<Response, Response> {
    proxy_handler_inner(state, req, Some(provider)).await
}

/// Handler for the default /v1/* route.
///
/// Provider selection priority:
/// 1. X-Provider header
/// 2. X-Original-Host header (mitmproxy capture mode)
/// 3. Auto-detect from model name (if configured)
/// 4. Falls back to "default" provider
pub async fn proxy_handler(
    State(state): State<AppState>,
    req: Request<Body>,
) -> Result<Response, Response> {
    proxy_handler_inner(state, req, None).await
}

/// Inner implementation shared by both proxy handlers.
async fn proxy_handler_inner(
    state: AppState,
    req: Request<Body>,
    path_provider: Option<String>,
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

    // Provider selection priority (pre-body):
    // 1. Path parameter (/{provider}/v1/...)
    // 2. X-Provider header
    // 3. X-Original-Host header (mitmproxy capture mode)
    // Model-based detection happens after body parsing
    let header_provider = req
        .headers()
        .get("X-Provider")
        .and_then(|h| h.to_str().ok())
        .map(|s| s.to_string());

    let original_host = req
        .headers()
        .get("X-Original-Host")
        .and_then(|h| h.to_str().ok())
        .map(|s| s.to_string());

    // Initial provider from path/header/host (model-based comes later)
    let pre_body_provider = path_provider
        .as_deref()
        .or(header_provider.as_deref())
        .or_else(|| {
            // Try to detect provider from X-Original-Host (mitmproxy capture mode)
            original_host.as_deref().and_then(detect_provider_from_host)
        });

    // 2. Read and modify body if needed (Pre-request Injection)
    let (parts, body) = req.into_parts();
    let include_claude_code_beta = parts
        .headers
        .get("anthropic-beta")
        .and_then(|h| h.to_str().ok())
        .map(|v| v.split(',').any(|p| p.trim() == "claude-code-20250219"))
        .unwrap_or(false);

    // Use configurable max body size to prevent DoS attacks
    let max_body_size = if state.config.server.max_body_size > 0 {
        state.config.server.max_body_size
    } else {
        usize::MAX // Unlimited if explicitly set to 0
    };

    let bytes = axum::body::to_bytes(body, max_body_size)
        .await
        .map_err(|e| {
            // Check if it's a size limit error
            let error_msg = e.to_string();
            if error_msg.contains("length limit") || error_msg.contains("too large") {
                (
                    StatusCode::PAYLOAD_TOO_LARGE,
                    Json(ProxyError::new(
                        format!(
                            "Request body too large. Maximum size: {} bytes",
                            max_body_size
                        ),
                        "payload_too_large",
                    )),
                )
                    .into_response()
            } else {
                (
                    StatusCode::BAD_REQUEST,
                    Json(ProxyError::new(
                        "Failed to read request body",
                        "invalid_request",
                    )),
                )
                    .into_response()
            }
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

    // Final provider resolution:
    // 1. Path parameter (/{provider}/v1/...)
    // 2. X-Provider header
    // 3. X-Original-Host header (mitmproxy capture mode)
    // 4. Auto-detect from model name
    // 5. Falls back to "default"
    let runtime_default_provider = if pre_body_provider.is_none() {
        crate::runtime_state::load_runtime_state().and_then(|state| state.default_provider)
    } else {
        None
    };

    let model_detected_provider = detect_provider_from_model(&model);

    let provider_name_requested = pre_body_provider
        .or(runtime_default_provider.as_deref())
        .or(model_detected_provider)
        .unwrap_or("default")
        .to_string();

    // Log provider routing for debugging
    if path_provider.is_some() {
        tracing::debug!(
            provider = %provider_name_requested,
            "Request routed via provider-prefixed path"
        );
    } else if header_provider.is_some() {
        tracing::debug!(
            provider = %provider_name_requested,
            "Request routed via X-Provider header"
        );
    } else if original_host.is_some() && pre_body_provider.is_some() {
        tracing::debug!(
            original_host = %original_host.as_deref().unwrap_or(""),
            detected_provider = %provider_name_requested,
            "Request intercepted via mitmproxy capture mode"
        );
    } else if runtime_default_provider.is_some() {
        tracing::debug!(
            provider = %provider_name_requested,
            "Provider selected via runtime default"
        );
    } else if model_detected_provider.is_some() {
        tracing::debug!(
            model = %model,
            detected_provider = %provider_name_requested,
            "Provider auto-detected from model name"
        );
    }

    // Resolve provider config.
    //
    // If the provider was auto-detected from the model but isn't configured, fall back to
    // the configured "default" provider (backwards compatible with older configs).
    let provider_selected_by_model = pre_body_provider.is_none()
        && runtime_default_provider.is_none()
        && model_detected_provider.is_some();
    let provider_lookup = state
        .config
        .resolve_provider(&provider_name_requested)
        .or_else(|| {
            if provider_selected_by_model {
                state.config.resolve_provider("default")
            } else {
                None
            }
        })
        .ok_or_else(|| {
            let available = state.config.provider_names();
            (
                StatusCode::BAD_REQUEST,
                Json(ProxyError::new(
                    format!(
                        "Unknown provider '{}'. Available providers: {:?}",
                        provider_name_requested, available
                    ),
                    "invalid_provider",
                )),
            )
                .into_response()
        })?;

    let provider_config = provider_lookup.config;
    let resolved_provider = provider_lookup.resolved_name.clone();
    let provider_name = resolved_provider.clone();

    if provider_selected_by_model && provider_name_requested != provider_name {
        tracing::debug!(
            requested = %provider_name_requested,
            resolved = %provider_name,
            "Auto-detected provider not configured; using default"
        );
    } else if provider_name_requested != provider_name {
        tracing::debug!(
            requested = %provider_name_requested,
            resolved = %provider_name,
            "Provider name normalized"
        );
    }

    // 3. Validate virtual API key if present (or required)
    // Note: validated_key is used for tracking usage after response completes
    let require_key = state.config.keys.enabled && state.config.keys.require_key;

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
                            StatusCode::from_u16(e.status_code())
                                .unwrap_or(StatusCode::UNAUTHORIZED),
                            Json(
                                ProxyError::new(e.to_string(), "authentication_error")
                                    .with_code(e.error_code()),
                            ),
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
        } else if require_key {
            // Not a virtual key but require_key is enabled - reject
            return Err((
                StatusCode::UNAUTHORIZED,
                Json(
                    ProxyError::new(
                        "A valid virtual API key is required. Keys must start with 'eavs_'",
                        "authentication_error",
                    )
                    .with_code("invalid_api_key"),
                ),
            )
                .into_response());
        } else {
            // Not a virtual key - pass through (backward compatible)
            None
        }
    } else if require_key {
        // No key provided but require_key is enabled - reject
        return Err((
            StatusCode::UNAUTHORIZED,
            Json(
                ProxyError::new(
                    "Authorization header with a valid virtual API key is required",
                    "authentication_error",
                )
                .with_code("missing_api_key"),
            ),
        )
            .into_response());
    } else {
        None
    };

    // Register/update conversation in store if capture_all is enabled
    if state.config.state.capture_all {
        let _ = state.conversations.get_or_create(&conversation_id);
        state
            .conversations
            .update_metadata(&conversation_id, |meta| {
                meta.provider = Some(provider_name.clone());
                meta.model = Some(model.clone());
                meta.request_count += 1;
            });
    }

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

    // Use real API key from provider config (virtual key was just for auth)
    // But if the virtual key has oauth_user, use OAuth token instead
    let mut api_key = provider_config.resolved_api_key();
    let mut provider_type = provider_config.provider_type();

    let mut is_anthropic_oauth = false;
    let mut is_openai_codex_oauth = false;
    if let Some(ref validated) = validated_key {
        if let Some(oauth_user) = validated.oauth_user.as_deref() {
            tracing::info!("OAuth user: {}, provider: {}", oauth_user, provider_name);
            api_key = match resolve_oauth_access_token_with_account(
                &state,
                &provider_name,
                oauth_user,
                validated.oauth_account.as_deref(),
            )
            .await
            {
                Ok(token) => {
                    tracing::info!(
                        "Got OAuth token starting with: {}...",
                        &token[..20.min(token.len())]
                    );
                    // Check if this is an Anthropic OAuth token
                    if provider_type == ProviderType::Anthropic && is_anthropic_oauth_token(&token)
                    {
                        is_anthropic_oauth = true;
                        tracing::info!(
                            "Detected Anthropic OAuth token, will inject Claude Code identity"
                        );
                    }
                    // Check if this is an OpenAI Codex OAuth token
                    if is_openai_codex_oauth_token(&token) {
                        is_openai_codex_oauth = true;
                        // Override provider type to use Codex (ChatGPT backend + Responses API)
                        provider_type = ProviderType::OpenAICodex;
                        tracing::info!(
                            "Detected OpenAI Codex OAuth token, switching to OpenAICodex provider"
                        );
                    }
                    token
                }
                Err(msg) => {
                    tracing::error!("OAuth token resolution failed: {}", msg);
                    return Err((
                        StatusCode::UNAUTHORIZED,
                        Json(ProxyError::new(msg, "oauth_error").with_code("oauth_failed")),
                    )
                        .into_response());
                }
            };
        }
    }

    // Determine the actual API path (strip provider prefix if using provider-prefixed routing)
    let api_path = if let Some(ref provider) = path_provider {
        let prefix = format!("/{}", provider);
        parts
            .uri
            .path()
            .strip_prefix(&prefix)
            .unwrap_or(parts.uri.path())
    } else {
        parts.uri.path()
    };

    // Handle /v1/models endpoint for providers that don't support it natively
    // Return a synthetic response with known models for that provider
    if api_path == "/v1/models" && !provider_type.supports_models_endpoint() {
        let models = provider_type.synthetic_models();
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);

        let models_response = serde_json::json!({
            "object": "list",
            "data": models.iter().map(|m| {
                serde_json::json!({
                    "id": m,
                    "object": "model",
                    "created": now,
                    "owned_by": format!("{:?}", provider_type).to_lowercase()
                })
            }).collect::<Vec<_>>()
        });

        let mut response = Json(models_response).into_response();
        response.headers_mut().insert(
            http::header::HeaderName::from_static("x-eavs-provider"),
            http::HeaderValue::from_str(&provider_name)
                .unwrap_or_else(|_| http::HeaderValue::from_static("unknown")),
        );
        response.headers_mut().insert(
            http::header::HeaderName::from_static("x-eavs-synthetic"),
            http::HeaderValue::from_static("true"),
        );

        return Ok(response);
    }

    // Check if this is a pass-through endpoint that doesn't need transformation
    // (e.g., /v1/models, /v1/embeddings for providers that support them natively)
    let is_passthrough_endpoint = api_path == "/v1/models"
        || api_path.starts_with("/v1/models/")
        || api_path == "/v1/embeddings";

    // Check if client is sending Responses API format directly
    // This allows clients to use the Responses API natively with EAVS just handling OAuth
    let is_responses_api_request =
        api_path == "/v1/responses" || api_path == "/responses" || api_path == "/codex/responses";

    // Check if we need format translation
    // Skip transformation for pass-through endpoints and native Responses API requests
    let needs_transform =
        provider_type.needs_transform() && !is_passthrough_endpoint && !is_responses_api_request;

    // Get the transformer for this provider
    let transformer = ProviderTransformer::for_provider(provider_type);

    // 4. Build request body - transform if needed
    let mut transformed_endpoint_path: Option<String> = None;
    let mut request_stream = false;
    let mut fake_streaming = false;
    let mut beta_scan_body: Option<Value> = None;
    let (request_body, model_name) = if needs_transform {
        // Parse incoming OpenAI-format request to canonical Context
        let mut context = parse_incoming_request(&json_body).map_err(|e| {
            tracing::error!("Failed to parse request: {}", e);
            (
                StatusCode::BAD_REQUEST,
                Json(ProxyError::new(
                    format!("Failed to parse request: {}", e),
                    "invalid_request",
                )),
            )
                .into_response()
        })?;

        let requested_stream = context.stream;
        request_stream = requested_stream;

        // For Anthropic OAuth tokens, inject Claude Code identity into system prompt
        // This is required because OAuth tokens are scoped to Claude Code
        if is_anthropic_oauth {
            inject_claude_code_identity_into_context(&mut context);
        }

        // Fake streaming for providers without native streaming support.
        if provider_type == ProviderType::Bedrock && requested_stream {
            fake_streaming = true;
            context.stream = false;
        }

        // Resolve URL images for providers that require inline/base64 image data.
        if matches!(
            provider_type,
            ProviderType::Anthropic
                | ProviderType::Google
                | ProviderType::GoogleVertex
                | ProviderType::GoogleGeminiCli
                | ProviderType::Bedrock
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
        let mut transformed = transformer.transform_request(&context).map_err(|e| {
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

        if is_anthropic_oauth {
            prefix_anthropic_oauth_tools(&mut transformed);
            apply_anthropic_oauth_body_transforms(&mut transformed);
        }

        beta_scan_body = Some(transformed.clone());

        let body = serde_json::to_vec(&transformed).map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ProxyError::new(
                    format!("Failed to serialize request: {}", e),
                    "internal_error",
                )),
            )
                .into_response()
        })?;
        (body, model)
    } else {
        // Pass through for OpenAI-compatible providers
        if provider_type == ProviderType::Mistral && api_path == "/v1/chat/completions" {
            crate::transform::mistral::transform_openai_request_for_mistral(&mut json_body)
                .map_err(|e| {
                    tracing::error!("Failed to apply Mistral request quirks: {}", e);
                    (
                        StatusCode::BAD_REQUEST,
                        Json(ProxyError::new(
                            format!("Failed to transform request for Mistral: {}", e),
                            "invalid_request",
                        )),
                    )
                        .into_response()
                })?;
        }

        let model = json_body["model"].as_str().unwrap_or("unknown").to_string();
        let body = serde_json::to_vec(&json_body).map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ProxyError::new(
                    format!("Failed to serialize request: {}", e),
                    "internal_error",
                )),
            )
                .into_response()
        })?;
        (body, model)
    };

    // Construct URL with transformer's endpoint path if transforming
    // Override base URL for providers that use dynamic OAuth-derived URLs
    let base = if is_openai_codex_oauth {
        "https://chatgpt.com/backend-api".to_string()
    } else if provider_type == ProviderType::GithubCopilot {
        // GitHub Copilot: use dynamic base URL from token if available
        if let Some(ref validated) = validated_key {
            if let Some(oauth_user) = validated.oauth_user.as_deref() {
                if let Some(store) = state.get_oauth_store() {
                    if let Ok(Some(creds)) = store
                        .get_credentials(oauth_user, crate::oauth::OAuthProvider::GithubCopilot)
                        .await
                    {
                        if let Some(extra) = &creds.extra_data {
                            if let Some(url) = extra.get("base_url").and_then(|v| v.as_str()) {
                                url.to_string()
                            } else {
                                provider_config.resolved_base_url()
                            }
                        } else {
                            provider_config.resolved_base_url()
                        }
                    } else {
                        provider_config.resolved_base_url()
                    }
                } else {
                    provider_config.resolved_base_url()
                }
            } else {
                provider_config.resolved_base_url()
            }
        } else {
            provider_config.resolved_base_url()
        }
    } else {
        provider_config.resolved_base_url()
    };
    // Vertex AI: append project/location path prefix to base URL so the
    // Google transformer's `/models/{model}:{action}` path resolves correctly.
    let base = if provider_type == ProviderType::GoogleVertex {
        let project = provider_config.resolved_gcp_project().unwrap_or_default();
        let location = provider_config.resolved_gcp_location().unwrap_or_default();
        if !project.is_empty() && !location.is_empty() {
            format!(
                "{}/v1beta/projects/{}/locations/{}/publishers/google",
                base.trim_end_matches('/'),
                project,
                location,
            )
        } else {
            tracing::warn!(
                "Vertex AI requires gcp_project and gcp_location config; got project={:?}, location={:?}",
                provider_config.resolved_gcp_project(),
                provider_config.resolved_gcp_location(),
            );
            base
        }
    } else {
        base
    };
    let base = base.trim_end_matches('/');

    let path = if needs_transform {
        // Use transformer's endpoint path for non-OpenAI providers
        transformed_endpoint_path.unwrap_or_else(|| "/v1/chat/completions".to_string())
    } else {
        // For OpenAI-compatible pass-through, strip /v1 prefix when base URL already has it.
        // Azure OpenAI is deployment-based; treat `model` as deployment name when base_url is
        // the resource endpoint (no `/openai/deployments/...` path).
        let request_path = parts.uri.path();

        // If using provider-prefixed routing (e.g., /openai/v1/models), strip the provider prefix
        // to get the actual API path (e.g., /v1/models)
        let request_path = if let Some(ref provider) = path_provider {
            let prefix = format!("/{}", provider);
            request_path.strip_prefix(&prefix).unwrap_or(request_path)
        } else {
            request_path
        };

        let stripped_path = if (provider_type == ProviderType::Azure || base.ends_with("/v1"))
            && request_path.starts_with("/v1")
        {
            request_path.strip_prefix("/v1").unwrap_or(request_path)
        } else {
            request_path
        };

        if provider_type == ProviderType::Azure && !base.contains("/openai/deployments/") {
            // Azure has different paths for different endpoints:
            // - /models -> /openai/models (no deployment needed)
            // - /chat/completions -> /openai/deployments/{deployment}/chat/completions
            if stripped_path == "/models" {
                "/openai/models".to_string()
            } else {
                // Use explicit deployment name if configured, otherwise fall back to model name
                let deployment = provider_config
                    .resolved_deployment()
                    .unwrap_or_else(|| model_name.clone());
                format!("/openai/deployments/{}{}", deployment, stripped_path)
            }
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

    if provider_type == ProviderType::Anthropic && is_anthropic_oauth_token(&api_key) {
        if let Ok(mut parsed) = url::Url::parse(&url) {
            if parsed.path() == "/v1/messages" && !parsed.query_pairs().any(|(k, _)| k == "beta") {
                parsed.query_pairs_mut().append_pair("beta", "true");
                url = parsed.to_string();
            }
        }
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
        if provider_type == ProviderType::Bedrock
            && key.to_ascii_lowercase().starts_with("anthropic-")
            && !bedrock_is_claude_model(&model_name)
        {
            continue;
        }

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

    // Auto-detect Anthropic beta headers based on request content.
    //
    // - Anthropic direct: apply to all requests when needed.
    // - Bedrock: only apply for Claude models (anthropic.*), never for other families.
    if should_apply_anthropic_beta_headers(provider_type, &model_name) {
        let scan = beta_scan_body.as_ref().unwrap_or(&json_body);
        let betas = anthropic_beta_tokens_for_request(scan);
        upsert_csv_header(
            &mut upstream_headers,
            http::header::HeaderName::from_static("anthropic-beta"),
            betas,
        );
    }

    if provider_type == ProviderType::Anthropic
        && is_anthropic_oauth_token(&api_key)
        && include_claude_code_beta
    {
        upsert_csv_header(
            &mut upstream_headers,
            http::header::HeaderName::from_static("anthropic-beta"),
            vec!["claude-code-20250219".to_string()],
        );
    }

    if provider_type == ProviderType::Bedrock {
        let region = provider_config.resolved_aws_region().ok_or_else(|| {
            (
                StatusCode::BAD_REQUEST,
                Json(ProxyError::new(
                    "Bedrock provider requires aws_region (or AWS_REGION)".to_string(),
                    "invalid_request",
                )),
            )
                .into_response()
        })?;

        let creds = resolve_bedrock_aws_credentials(state.upstream.as_ref(), provider_config)
            .await
            .map_err(|msg| {
                (
                    StatusCode::BAD_REQUEST,
                    Json(ProxyError::new(msg, "invalid_request")),
                )
                    .into_response()
            })?;

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

    // Log upstream URL with optional redaction
    if state.config.server.log_redact {
        tracing::debug!("Upstream URL: {}", crate::config::redact_sensitive(&url));
    } else {
        tracing::debug!("Upstream URL: {}", url);
    }

    // Handle mock provider - return synthetic responses without network calls
    if provider_type.is_mock() {
        return handle_mock_response(
            &model_name,
            json_body
                .get("stream")
                .and_then(|v| v.as_bool())
                .unwrap_or(false),
            &correlation_id,
            state.analysis_tx.clone(),
        )
        .await;
    }

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
    // Destructure to take ownership without cloning
    let UpstreamResponse {
        status,
        headers,
        body: stream,
    } = upstream_res;

    let analysis_tx = state.analysis_tx.clone();
    let correlation_id_clone = correlation_id.clone();

    // Normalize non-success upstream errors to OpenAI-compatible error format.
    if !status.is_success() {
        let body_bytes = collect_stream_bytes(stream).await.unwrap_or_default();
        let text = String::from_utf8_lossy(&body_bytes).to_string();

        let _ = analysis_tx.send(AnalysisEvent::ResponseChunk {
            timestamp: chrono::Utc::now().timestamp_millis(),
            id: correlation_id_clone.clone(),
            chunk: text.clone(),
        });

        let (message, error_type, code) =
            normalize_upstream_error(provider_type, status, &text, &body_bytes);

        let err = json!({
            "error": {
                "message": message,
                "type": error_type,
                "param": null,
                "code": code
            }
        });

        let bytes = serde_json::to_vec(&err).unwrap_or_default();
        let mut response = Response::new(Body::from(bytes));
        *response.status_mut() = status;
        response.headers_mut().insert(
            http::header::CONTENT_TYPE,
            http::HeaderValue::from_static("application/json"),
        );
        if let Some(v) = headers.get(http::header::RETRY_AFTER).cloned() {
            response.headers_mut().insert(http::header::RETRY_AFTER, v);
        }
        response
            .headers_mut()
            .insert("x-eavs-provider", resolved_provider.parse().unwrap());
        return Ok(response);
    }

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
        if request_stream && !fake_streaming {
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
            response
                .headers_mut()
                .insert("x-eavs-provider", resolved_provider.parse().unwrap());

            // Record usage asynchronously when stream completes
            // Use a lighter-weight approach than spawning a delayed task for every request
            if let Some(tracker) = usage_tracker.clone() {
                let state_clone = state.clone();
                // Record immediately - the batched KeyStore will handle SQLite writes efficiently
                tokio::spawn(async move {
                    record_usage_from_tracker(&state_clone, &tracker).await;
                });
            }

            Ok(response)
        } else if request_stream && fake_streaming {
            // Fake streaming: buffer full upstream response, transform to canonical events,
            // then emit OpenAI-compatible SSE chunks.
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

            // Track usage from final Done event.
            if let Some(tracker) = usage_tracker.clone() {
                if let Some(done) = events.iter().find_map(|e| match e {
                    crate::types::StreamEvent::Done { message, .. } => Some(message),
                    _ => None,
                }) {
                    tracker.input_tokens.store(
                        done.usage.prompt_tokens,
                        std::sync::atomic::Ordering::SeqCst,
                    );
                    tracker.output_tokens.store(
                        done.usage.completion_tokens,
                        std::sync::atomic::Ordering::SeqCst,
                    );
                    tracker.cached_tokens.store(
                        done.usage.cache_read_input_tokens.unwrap_or(0),
                        std::sync::atomic::Ordering::SeqCst,
                    );
                }
            }

            let chunks = build_fake_openai_sse_from_events(&events, &correlation_id, &model_name);
            let stream = futures::stream::iter(chunks.into_iter().map(Ok::<_, std::io::Error>));

            let mut response = Response::new(Body::from_stream(stream));
            *response.status_mut() = status;
            response
                .headers_mut()
                .insert("content-type", "text/event-stream".parse().unwrap());
            response
                .headers_mut()
                .insert("cache-control", "no-cache".parse().unwrap());
            response
                .headers_mut()
                .insert("x-eavs-provider", resolved_provider.parse().unwrap());

            if let Some(tracker) = usage_tracker.clone() {
                let state_clone = state.clone();
                tokio::spawn(async move {
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
            response
                .headers_mut()
                .insert("x-eavs-provider", resolved_provider.parse().unwrap());

            if let Some(tracker) = usage_tracker.clone() {
                if let Some(usage) = response_json.get("usage") {
                    if let Some(input) = usage.get("prompt_tokens").and_then(|v| v.as_u64()) {
                        tracker
                            .input_tokens
                            .store(input as u32, std::sync::atomic::Ordering::SeqCst);
                    }
                    if let Some(output) = usage.get("completion_tokens").and_then(|v| v.as_u64()) {
                        tracker
                            .output_tokens
                            .store(output as u32, std::sync::atomic::Ordering::SeqCst);
                    }
                }

                // Record immediately - the batched KeyStore handles SQLite writes efficiently
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
        response
            .headers_mut()
            .insert("x-eavs-provider", resolved_provider.parse().unwrap());

        // Record usage asynchronously - batched KeyStore handles SQLite writes efficiently
        if let Some(tracker) = usage_tracker {
            let state_clone = state.clone();
            tokio::spawn(async move {
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

/// Handle mock provider requests - returns synthetic responses without network calls.
/// This is used for benchmarking to measure pure proxy overhead.
async fn handle_mock_response(
    model: &str,
    stream: bool,
    request_id: &str,
    analysis_tx: tokio::sync::broadcast::Sender<AnalysisEvent>,
) -> Result<Response, Response> {
    let timestamp = chrono::Utc::now().timestamp();

    if stream {
        // Return streaming SSE response
        let chunks = vec![
            format!(
                "data: {{\"id\":\"chatcmpl-mock-{}\",\"object\":\"chat.completion.chunk\",\"created\":{},\"model\":\"{}\",\"choices\":[{{\"index\":0,\"delta\":{{\"role\":\"assistant\",\"content\":\"\"}},\"finish_reason\":null}}]}}\n\n",
                request_id, timestamp, model
            ),
            format!(
                "data: {{\"id\":\"chatcmpl-mock-{}\",\"object\":\"chat.completion.chunk\",\"created\":{},\"model\":\"{}\",\"choices\":[{{\"index\":0,\"delta\":{{\"content\":\"This is a mock response for benchmarking.\"}},\"finish_reason\":null}}]}}\n\n",
                request_id, timestamp, model
            ),
            format!(
                "data: {{\"id\":\"chatcmpl-mock-{}\",\"object\":\"chat.completion.chunk\",\"created\":{},\"model\":\"{}\",\"choices\":[{{\"index\":0,\"delta\":{{}},\"finish_reason\":\"stop\"}}],\"usage\":{{\"prompt_tokens\":10,\"completion_tokens\":8,\"total_tokens\":18}}}}\n\n",
                request_id, timestamp, model
            ),
            "data: [DONE]\n\n".to_string(),
        ];

        // Log the mock response
        let _ = analysis_tx.send(AnalysisEvent::ResponseChunk {
            timestamp: chrono::Utc::now().timestamp_millis(),
            id: request_id.to_string(),
            chunk: "[mock streaming response]".to_string(),
        });

        let stream = futures::stream::iter(
            chunks
                .into_iter()
                .map(|c| Ok::<_, std::io::Error>(Bytes::from(c))),
        );

        Ok(Response::builder()
            .status(StatusCode::OK)
            .header("content-type", "text/event-stream")
            .header("cache-control", "no-cache")
            .header("x-mock-response", "true")
            .body(Body::from_stream(stream))
            .unwrap())
    } else {
        // Return non-streaming JSON response
        let response = serde_json::json!({
            "id": format!("chatcmpl-mock-{}", request_id),
            "object": "chat.completion",
            "created": timestamp,
            "model": model,
            "choices": [{
                "index": 0,
                "message": {
                    "role": "assistant",
                    "content": "This is a mock response for benchmarking."
                },
                "finish_reason": "stop"
            }],
            "usage": {
                "prompt_tokens": 10,
                "completion_tokens": 8,
                "total_tokens": 18
            }
        });

        // Log the mock response
        let _ = analysis_tx.send(AnalysisEvent::ResponseChunk {
            timestamp: chrono::Utc::now().timestamp_millis(),
            id: request_id.to_string(),
            chunk: "[mock non-streaming response]".to_string(),
        });

        Ok(Response::builder()
            .status(StatusCode::OK)
            .header("content-type", "application/json")
            .header("x-mock-response", "true")
            .body(Body::from(serde_json::to_vec(&response).unwrap()))
            .unwrap())
    }
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
                        tracker
                            .input_tokens
                            .store(input as u32, std::sync::atomic::Ordering::SeqCst);
                    }
                    if let Some(output) = usage.get("completion_tokens").and_then(|v| v.as_u64()) {
                        tracker
                            .output_tokens
                            .store(output as u32, std::sync::atomic::Ordering::SeqCst);
                    }
                    if let Some(cached) = usage
                        .get("prompt_tokens_details")
                        .and_then(|d| d.get("cached_tokens"))
                        .and_then(|v| v.as_u64())
                    {
                        tracker
                            .cached_tokens
                            .store(cached as u32, std::sync::atomic::Ordering::SeqCst);
                    }
                }
            }
        }
    }
}

/// Record usage from tracker to the key validator.
async fn record_usage_from_tracker(state: &AppState, tracker: &UsageTracker) {
    let input = tracker
        .input_tokens
        .load(std::sync::atomic::Ordering::SeqCst);
    let output = tracker
        .output_tokens
        .load(std::sync::atomic::Ordering::SeqCst);
    let cached = tracker
        .cached_tokens
        .load(std::sync::atomic::Ordering::SeqCst);

    // Only record if we have any usage data
    if input == 0 && output == 0 {
        return;
    }

    // Calculate cost
    let cost = if let Some(calc) = state.get_cost_calculator() {
        calc.calculate_actual_cost(&tracker.model, input, output, cached)
            .await
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

fn build_openai_chat_completion_from_events(
    events: &[crate::types::StreamEvent],
    request_id: &str,
    model: &str,
) -> Value {
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

fn build_fake_openai_sse_from_events(
    events: &[crate::types::StreamEvent],
    request_id: &str,
    model: &str,
) -> Vec<Bytes> {
    use crate::types::{AssistantMessage, StreamEvent};

    let mut done: Option<(crate::types::StopReason, AssistantMessage)> = None;
    for e in events {
        match e {
            StreamEvent::Done { reason, message } => {
                done = Some((reason.clone(), message.clone()));
            }
            StreamEvent::Error { reason, message } => {
                done = Some((reason.clone(), message.clone()));
            }
            _ => {}
        }
    }

    let (stop_reason, message) =
        done.unwrap_or_else(|| (crate::types::StopReason::Other, AssistantMessage::default()));

    let mut out_events: Vec<StreamEvent> = Vec::new();
    out_events.push(StreamEvent::Start {
        partial: AssistantMessage {
            model: model.to_string(),
            ..Default::default()
        },
    });

    let mut tool_index = 0usize;
    for block in &message.content {
        match block {
            ContentBlock::Text(t) => {
                if !t.text.is_empty() {
                    out_events.push(StreamEvent::TextDelta {
                        content_index: 0,
                        delta: t.text.clone(),
                    });
                }
            }
            ContentBlock::Thinking(t) => {
                if !t.thinking.is_empty() {
                    out_events.push(StreamEvent::ThinkingDelta {
                        content_index: 0,
                        delta: t.thinking.clone(),
                    });
                }
            }
            ContentBlock::ToolCall(tc) => {
                out_events.push(StreamEvent::ToolCallStart {
                    content_index: tool_index,
                    id: tc.id.clone(),
                    name: tc.name.clone(),
                });
                let args =
                    serde_json::to_string(&tc.arguments).unwrap_or_else(|_| "{}".to_string());
                out_events.push(StreamEvent::ToolCallDelta {
                    content_index: tool_index,
                    delta: args,
                });
                tool_index += 1;
            }
            _ => {}
        }
    }

    out_events.push(StreamEvent::Usage {
        usage: message.usage.clone(),
    });
    out_events.push(StreamEvent::Done {
        reason: stop_reason,
        message,
    });

    out_events
        .iter()
        .map(|e| Bytes::from(build_openai_sse_response(e, request_id, model)))
        .collect()
}

fn apply_injections(json_body: &mut Value, injections: &[Injection]) {
    if let Some(messages) = json_body.get_mut("messages").and_then(|m| m.as_array_mut()) {
        // Collect system and non-system injections separately to maintain order
        let mut system_injections: Vec<Value> = Vec::new();
        let mut other_injections: Vec<Value> = Vec::new();

        for injection in injections {
            let obj = serde_json::json!({
                "role": injection.role,
                "content": injection.content
            });
            if injection.role == "system" {
                system_injections.push(obj);
            } else {
                other_injections.push(obj);
            }
        }

        // Insert all system messages at the beginning in their original order
        // by using splice to insert all at once
        if !system_injections.is_empty() {
            messages.splice(0..0, system_injections);
        }

        // Append non-system messages at the end
        messages.extend(other_injections);
    }
}

/// Handler for provider-prefixed WebSocket routes: /{provider}/v1/realtime
pub async fn provider_ws_proxy_handler(
    State(state): State<AppState>,
    Path(provider): Path<String>,
    ws: WebSocketUpgrade,
    headers: HeaderMap,
    OriginalUri(uri): OriginalUri,
) -> Response {
    ws_proxy_handler_inner(state, ws, headers, uri, Some(provider)).await
}

/// Handler for the default WebSocket route: /v1/realtime
pub async fn ws_proxy_handler(
    State(state): State<AppState>,
    ws: WebSocketUpgrade,
    headers: HeaderMap,
    OriginalUri(uri): OriginalUri,
) -> Response {
    ws_proxy_handler_inner(state, ws, headers, uri, None).await
}

/// Inner implementation for WebSocket proxy handlers.
async fn ws_proxy_handler_inner(
    state: AppState,
    ws: WebSocketUpgrade,
    headers: HeaderMap,
    uri: http::Uri,
    path_provider: Option<String>,
) -> Response {
    let conversation_id = headers
        .get("X-Conversation-ID")
        .and_then(|h| h.to_str().ok())
        .unwrap_or("default")
        .to_string();

    // Provider selection: path > header > default
    let header_provider = headers
        .get("X-Provider")
        .and_then(|h| h.to_str().ok())
        .map(|s| s.to_string());

    let provider_name = path_provider
        .or(header_provider)
        .unwrap_or_else(|| "default".to_string());

    // Validate virtual API key if present (or required)
    let require_key = state.config.keys.enabled && state.config.keys.require_key;
    let auth_header = headers
        .get("authorization")
        .and_then(|h| h.to_str().ok())
        .and_then(|h| h.strip_prefix("Bearer "))
        .map(|s| s.to_string());

    let mut validated_key: Option<ValidatedKey> = None;

    if let Some(ref key) = auth_header {
        if is_virtual_key(key) {
            if let Some(validator) = state.get_key_validator() {
                // Use a small token estimate for WebSocket rate limiting
                let estimated_tokens = 100; // Conservative estimate per WS connection
                match validator
                    .validate(key, "websocket", &provider_name, Some(estimated_tokens))
                    .await
                {
                    Ok(validated) => {
                        validated_key = Some(validated);
                    }
                    Err(e) => {
                        return (
                            StatusCode::from_u16(e.status_code())
                                .unwrap_or(StatusCode::UNAUTHORIZED),
                            Json(
                                ProxyError::new(e.to_string(), "authentication_error")
                                    .with_code(e.error_code()),
                            ),
                        )
                            .into_response();
                    }
                }
            } else {
                // Keys enabled in request but validator not initialized
                return (
                    StatusCode::SERVICE_UNAVAILABLE,
                    Json(ProxyError::new(
                        "Virtual API keys are not available",
                        "service_unavailable",
                    )),
                )
                    .into_response();
            }
        } else if require_key {
            // Not a virtual key but require_key is enabled - reject
            return (
                StatusCode::UNAUTHORIZED,
                Json(
                    ProxyError::new(
                        "A valid virtual API key is required. Keys must start with 'eavs_'",
                        "authentication_error",
                    )
                    .with_code("invalid_api_key"),
                ),
            )
                .into_response();
        }
    } else if require_key {
        // No key provided but require_key is enabled - reject
        return (
            StatusCode::UNAUTHORIZED,
            Json(
                ProxyError::new(
                    "Authorization header with a valid virtual API key is required",
                    "authentication_error",
                )
                .with_code("missing_api_key"),
            ),
        )
            .into_response();
    }

    // Register/update conversation in store if capture_all is enabled
    if state.config.state.capture_all {
        let _ = state.conversations.get_or_create(&conversation_id);
        state
            .conversations
            .update_metadata(&conversation_id, |meta| {
                meta.provider = Some(provider_name.clone());
                meta.model = Some("websocket".to_string());
                meta.request_count += 1;
            });
    }

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
    let mut api_key = provider_config.resolved_api_key();

    if let Some(ref validated) = validated_key {
        if let Some(oauth_user) = validated.oauth_user.as_deref() {
            api_key = match resolve_oauth_access_token_with_account(&state, &provider_name, oauth_user, validated.oauth_account.as_deref()).await {
                Ok(token) => token,
                Err(msg) => {
                    return (
                        StatusCode::UNAUTHORIZED,
                        Json(ProxyError::new(msg, "oauth_error").with_code("oauth_failed")),
                    )
                        .into_response();
                }
            };
        }
    }

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

        let _ = tokio::join!(
            upstream_write,
            client_to_upstream,
            upstream_to_client,
            inject_to_upstream
        );
    })
    .into_response()
}

/// Handler for provider-prefixed Codex WebSocket routes: /{provider}/v1/codex/responses
pub async fn provider_codex_ws_handler(
    State(state): State<AppState>,
    Path(provider): Path<String>,
    ws: WebSocketUpgrade,
    headers: HeaderMap,
    OriginalUri(uri): OriginalUri,
) -> Response {
    codex_ws_handler_inner(state, ws, headers, uri, Some(provider)).await
}

/// Handler for the default Codex WebSocket route: /v1/codex/responses
pub async fn codex_ws_handler(
    State(state): State<AppState>,
    ws: WebSocketUpgrade,
    headers: HeaderMap,
    OriginalUri(uri): OriginalUri,
) -> Response {
    codex_ws_handler_inner(state, ws, headers, uri, None).await
}

/// Inner implementation for Codex Responses WebSocket proxy.
///
/// Protocol: client sends `{"type": "response.create", ...body}`, server streams
/// back response events as JSON messages. Eavs intercepts the initial message to
/// apply policy rules (e.g., set_field for `store: true`), then relays bidirectionally.
async fn codex_ws_handler_inner(
    state: AppState,
    ws: WebSocketUpgrade,
    headers: HeaderMap,
    uri: http::Uri,
    path_provider: Option<String>,
) -> Response {
    // --- Auth & provider resolution (shared with realtime WS handler) ---

    let header_provider = headers
        .get("X-Provider")
        .and_then(|h| h.to_str().ok())
        .map(|s| s.to_string());

    let provider_name = path_provider
        .or(header_provider)
        .unwrap_or_else(|| "openai-codex".to_string());

    let require_key = state.config.keys.enabled && state.config.keys.require_key;
    let auth_header = headers
        .get("authorization")
        .and_then(|h| h.to_str().ok())
        .and_then(|h| h.strip_prefix("Bearer "))
        .map(|s| s.to_string());

    let mut validated_key: Option<ValidatedKey> = None;

    if let Some(ref key) = auth_header {
        if is_virtual_key(key) {
            if let Some(validator) = state.get_key_validator() {
                let estimated_tokens = 100;
                match validator
                    .validate(key, "codex-ws", &provider_name, Some(estimated_tokens))
                    .await
                {
                    Ok(validated) => {
                        validated_key = Some(validated);
                    }
                    Err(e) => {
                        return (
                            StatusCode::from_u16(e.status_code())
                                .unwrap_or(StatusCode::UNAUTHORIZED),
                            Json(
                                ProxyError::new(e.to_string(), "authentication_error")
                                    .with_code(e.error_code()),
                            ),
                        )
                            .into_response();
                    }
                }
            } else {
                return (
                    StatusCode::SERVICE_UNAVAILABLE,
                    Json(ProxyError::new(
                        "Virtual API keys are not available",
                        "service_unavailable",
                    )),
                )
                    .into_response();
            }
        } else if require_key {
            return (
                StatusCode::UNAUTHORIZED,
                Json(
                    ProxyError::new(
                        "A valid virtual API key is required",
                        "authentication_error",
                    )
                    .with_code("invalid_api_key"),
                ),
            )
                .into_response();
        }
    } else if require_key {
        return (
            StatusCode::UNAUTHORIZED,
            Json(
                ProxyError::new(
                    "Authorization header required",
                    "authentication_error",
                )
                .with_code("missing_api_key"),
            ),
        )
            .into_response();
    }

    let provider_lookup = match state.config.resolve_provider(&provider_name) {
        Some(v) => v,
        None => {
            let available = state.config.provider_names();
            return (
                StatusCode::BAD_REQUEST,
                Json(ProxyError::new(
                    format!(
                        "Unknown provider '{}'. Available: {:?}",
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
    let mut api_key = provider_config.resolved_api_key();

    // Resolve OAuth token if key is bound to an oauth_user
    if let Some(ref validated) = validated_key {
        if let Some(oauth_user) = validated.oauth_user.as_deref() {
            api_key = match resolve_oauth_access_token_with_account(&state, &provider_name, oauth_user, validated.oauth_account.as_deref()).await {
                Ok(token) => token,
                Err(msg) => {
                    return (
                        StatusCode::UNAUTHORIZED,
                        Json(ProxyError::new(msg, "oauth_error").with_code("oauth_failed")),
                    )
                        .into_response();
                }
            };
        }
    }

    // Build upstream WebSocket URL
    // For OpenAI Codex OAuth, override base to chatgpt.com
    let base = if is_openai_codex_oauth_token(&api_key) {
        "https://chatgpt.com/backend-api".to_string()
    } else {
        provider_config.resolved_base_url()
    };

    let upstream_url = match build_ws_upstream_url(&base, "/codex/responses", uri.query()) {
        Ok(u) => u,
        Err(e) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(ProxyError::new(e, "invalid_request")),
            )
                .into_response();
        }
    };

    // Clone what we need for the async upgrade closure
    let policy = state.config.policy.clone();
    let provider_name_for_policy = provider_name.clone();
    let provider_name_for_usage = provider_name.clone();
    let provider_headers = provider_config.headers.clone();
    let validated_key_hash = validated_key.as_ref().map(|k| k.key_hash.clone());
    // We'll reference state inside the closure directly

    ws.on_upgrade(move |mut client_socket| async move {
        // Connect to upstream WebSocket
        let mut request = match upstream_url.into_client_request() {
            Ok(r) => r,
            Err(e) => {
                tracing::error!("Failed to build upstream WS request: {}", e);
                let _ = client_socket.send(AxumWsMessage::Close(None)).await;
                return;
            }
        };

        // Apply auth + extra headers
        apply_ws_auth_headers(&mut request, provider_type, &api_key);

        // Add WebSocket-specific beta header for Codex responses
        request.headers_mut().insert(
            http::header::HeaderName::from_static("openai-beta"),
            http::HeaderValue::from_static("responses_websockets=2026-02-06"),
        );

        // Custom provider headers
        for (key, value) in &provider_headers {
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
            Err(e) => {
                tracing::error!("Failed to connect to upstream Codex WS: {}", e);
                let _ = client_socket.send(AxumWsMessage::Close(None)).await;
                return;
            }
        };

        let (mut client_sender, mut client_receiver) = client_socket.split();
        let (mut upstream_sender, mut upstream_receiver) = upstream_socket.split();

        // Single writer task for upstream
        let (upstream_tx, mut upstream_rx) = mpsc::unbounded_channel::<TungsteniteMessage>();
        let upstream_write = tokio::spawn(async move {
            while let Some(msg) = upstream_rx.recv().await {
                if upstream_sender.send(msg).await.is_err() {
                    break;
                }
            }
        });

        // Forward client -> upstream, intercepting response.create for policy application
        let upstream_tx_client = upstream_tx.clone();
        let client_to_upstream = tokio::spawn(async move {
            while let Some(Ok(msg)) = client_receiver.next().await {
                let processed = match &msg {
                    AxumWsMessage::Text(text) => {
                        // Try to parse as JSON and apply policies to response.create messages
                        match serde_json::from_str::<Value>(text.as_str()) {
                            Ok(mut json) => {
                                let msg_type = json
                                    .get("type")
                                    .and_then(|v| v.as_str())
                                    .unwrap_or_default();

                                if msg_type == "response.create" {
                                    // The body fields are at the top level of the message
                                    // (not nested under "body"), e.g. {"type":"response.create","model":"...","store":false,...}
                                    if let Err(e) = policy.apply(
                                        &provider_name_for_policy,
                                        "/codex/responses",
                                        &mut json,
                                    ) {
                                        tracing::warn!(
                                            "Policy violation on Codex WS: {}",
                                            e.message
                                        );
                                        // Skip sending this message
                                        continue;
                                    }
                                    Some(TungsteniteMessage::Text(json.to_string()))
                                } else {
                                    axum_to_tungstenite(msg)
                                }
                            }
                            Err(_) => axum_to_tungstenite(msg),
                        }
                    }
                    _ => axum_to_tungstenite(msg),
                };

                let Some(up_msg) = processed else {
                    continue;
                };
                let is_close = matches!(up_msg, TungsteniteMessage::Close(_));
                let _ = upstream_tx_client.send(up_msg);
                if is_close {
                    break;
                }
            }
        });

        // Forward upstream -> client, tracking usage from response.completed events
        let key_hash = validated_key_hash.clone();
        let key_validator = state.get_key_validator().cloned();
        let cost_calc = state.get_cost_calculator().cloned();
        let upstream_to_client = tokio::spawn(async move {
            while let Some(Ok(msg)) = upstream_receiver.next().await {
                // Track usage from response.completed events
                if let TungsteniteMessage::Text(ref text) = msg {
                    if let Ok(json) = serde_json::from_str::<Value>(text.as_str()) {
                        let event_type = json
                            .get("type")
                            .and_then(|v| v.as_str())
                            .unwrap_or_default();

                        if event_type == "response.completed" || event_type == "response.done" {
                            if let (Some(ref kh), Some(ref validator)) =
                                (&key_hash, &key_validator)
                            {
                                // Extract usage from response.completed.response.usage
                                let usage = json
                                    .pointer("/response/usage")
                                    .or_else(|| json.get("usage"));
                                let model = json
                                    .pointer("/response/model")
                                    .and_then(|v| v.as_str())
                                    .unwrap_or("codex-ws");
                                if let Some(usage) = usage {
                                    let input_tokens = usage
                                        .get("input_tokens")
                                        .and_then(|v| v.as_u64())
                                        .unwrap_or(0) as u32;
                                    let output_tokens = usage
                                        .get("output_tokens")
                                        .and_then(|v| v.as_u64())
                                        .unwrap_or(0) as u32;
                                    let cached_tokens = usage
                                        .get("input_tokens_details")
                                        .and_then(|d| d.get("cached_tokens"))
                                        .and_then(|v| v.as_u64())
                                        .unwrap_or(0) as u32;

                                    let cost_usd = if let Some(ref cc) = cost_calc {
                                        cc.calculate_actual_cost(
                                            model,
                                            input_tokens,
                                            output_tokens,
                                            cached_tokens,
                                        )
                                        .await
                                    } else {
                                        0.0
                                    };

                                    validator
                                        .record_usage(
                                            kh,
                                            input_tokens,
                                            output_tokens,
                                            cached_tokens,
                                            cost_usd,
                                            model,
                                            &provider_name_for_usage,
                                        )
                                        .await;
                                }
                            }
                        }
                    }
                }

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

        // Wait for all tasks (no injection task for Codex - it's request/response)
        let _ = tokio::join!(upstream_write, client_to_upstream, upstream_to_client);
        drop(upstream_tx); // ensure writer task exits
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
        Err(format!(
            "Unsupported upstream base_url scheme: {}",
            base_url
        ))
    }
}

/// Trait for abstracting over different header map types (HeaderMap vs http::Request<()>).
/// This allows us to use a single implementation for both HTTP and WebSocket headers.
trait HeadersExt {
    fn insert_header(&mut self, name: http::header::HeaderName, value: http::HeaderValue);
}

impl HeadersExt for HeaderMap {
    fn insert_header(&mut self, name: http::header::HeaderName, value: http::HeaderValue) {
        let _ = self.insert(name, value);
    }
}

impl HeadersExt for http::Request<()> {
    fn insert_header(&mut self, name: http::header::HeaderName, value: http::HeaderValue) {
        let _ = self.headers_mut().insert(name, value);
    }
}

/// Check if this is an Anthropic OAuth token (starts with sk-ant-oat)
fn is_anthropic_oauth_token(api_key: &str) -> bool {
    api_key.starts_with("sk-ant-oat")
}

/// Check if this is an OpenAI Codex OAuth token (JWT format)
/// OpenAI OAuth tokens are JWTs that start with "eyJ" (base64 encoded JSON header)
fn is_openai_codex_oauth_token(api_key: &str) -> bool {
    api_key.starts_with("eyJ") && api_key.contains('.')
}

/// Extract account ID from OpenAI OAuth JWT token.
/// The account ID is stored in the JWT claims under "https://api.openai.com/auth"
fn extract_openai_account_id(token: &str) -> Option<String> {
    // JWT format: header.payload.signature
    let parts: Vec<&str> = token.split('.').collect();
    if parts.len() != 3 {
        return None;
    }

    // Decode the payload (second part)
    let payload = parts[1];
    // JWT uses base64url encoding, need to handle padding
    let padded = match payload.len() % 4 {
        2 => format!("{}==", payload),
        3 => format!("{}=", payload),
        _ => payload.to_string(),
    };

    // Replace URL-safe chars with standard base64
    let standard_b64 = padded.replace('-', "+").replace('_', "/");

    let decoded =
        match base64::Engine::decode(&base64::engine::general_purpose::STANDARD, &standard_b64) {
            Ok(bytes) => bytes,
            Err(_) => return None,
        };

    let json: serde_json::Value = match serde_json::from_slice(&decoded) {
        Ok(v) => v,
        Err(_) => return None,
    };

    // Look for account ID in the standard OpenAI claim path
    json.get("https://api.openai.com/auth")
        .and_then(|auth| auth.get("org_id").or_else(|| auth.get("account_id")))
        .and_then(|v| v.as_str())
        .map(String::from)
        .or_else(|| {
            // Fallback: try direct org_id or account_id
            json.get("org_id")
                .or_else(|| json.get("account_id"))
                .and_then(|v| v.as_str())
                .map(String::from)
        })
}

/// Anthropic OAuth tokens are scoped to "Claude Code" - requests must identify as Claude Code.
/// This injects the required system prompt prefix for the token to work.
const CLAUDE_CODE_IDENTITY: &str = "You are Claude Code, Anthropic's official CLI for Claude.";

fn inject_claude_code_identity_into_context(context: &mut crate::types::Context) {
    // Prepend Claude Code identity to the system prompt
    let new_system = match &context.system_prompt {
        Some(existing) => format!("{}\n\n{}", CLAUDE_CODE_IDENTITY, existing),
        None => CLAUDE_CODE_IDENTITY.to_string(),
    };
    context.system_prompt = Some(new_system);
}

fn prefix_anthropic_oauth_tools(body: &mut Value) {
    const TOOL_PREFIX: &str = "mcp_";

    // Prefix tool names in tools[] definitions
    if let Some(tools) = body.get_mut("tools").and_then(|v| v.as_array_mut()) {
        for tool in tools {
            if let Some(name) = tool.get_mut("name").and_then(|v| v.as_str()) {
                if !name.starts_with(TOOL_PREFIX) {
                    *tool.get_mut("name").unwrap() =
                        Value::String(format!("{}{}", TOOL_PREFIX, name));
                }
            }
        }
    }

    // Prefix tool names in messages[].content[].tool_use blocks
    if let Some(messages) = body.get_mut("messages").and_then(|v| v.as_array_mut()) {
        for message in messages {
            if let Some(content) = message.get_mut("content").and_then(|v| v.as_array_mut()) {
                for block in content {
                    let is_tool_use = block
                        .get("type")
                        .and_then(|t| t.as_str())
                        .map(|t| t == "tool_use")
                        .unwrap_or(false);
                    if is_tool_use {
                        if let Some(name) = block.get_mut("name").and_then(|v| v.as_str()) {
                            if !name.starts_with(TOOL_PREFIX) {
                                *block.get_mut("name").unwrap() =
                                    Value::String(format!("{}{}", TOOL_PREFIX, name));
                            }
                        }
                    }
                }
            }
        }
    }

    // Prefix tool names in tool_choice
    if let Some(choice) = body.get_mut("tool_choice") {
        if let Some(obj) = choice.as_object_mut() {
            if let Some(name_value) = obj.get_mut("name") {
                if let Some(name) = name_value.as_str() {
                    if !name.starts_with(TOOL_PREFIX) {
                        *name_value = Value::String(format!("{}{}", TOOL_PREFIX, name));
                    }
                }
            }
        }
    }
}

/// Apply Anthropic OAuth-specific body transformations.
///
/// Sanitizes system prompt text to replace "OpenCode" with "Claude Code" and
/// "opencode" (case-insensitive) with "Claude", matching the opencode-anthropic-auth
/// plugin behavior. The Anthropic server blocks requests containing "OpenCode" in
/// system prompts when using OAuth tokens.
fn apply_anthropic_oauth_body_transforms(body: &mut Value) {
    // Sanitize system prompt - server blocks 'OpenCode' string
    if let Some(system) = body.get_mut("system") {
        if let Some(arr) = system.as_array_mut() {
            for item in arr {
                if item
                    .get("type")
                    .and_then(|t| t.as_str())
                    .map(|t| t == "text")
                    .unwrap_or(false)
                {
                    if let Some(text) = item.get("text").and_then(|t| t.as_str()) {
                        let sanitized = sanitize_system_prompt(text);
                        if sanitized != text {
                            *item.get_mut("text").unwrap() = Value::String(sanitized);
                        }
                    }
                }
            }
        } else if let Some(text) = system.as_str() {
            let sanitized = sanitize_system_prompt(text);
            if sanitized != text {
                *system = Value::String(sanitized);
            }
        }
    }
}

/// Sanitize system prompt for Anthropic OAuth: replace "OpenCode" -> "Claude Code"
/// and "opencode" (case-insensitive) -> "Claude".
fn sanitize_system_prompt(text: &str) -> String {
    // First replace case-sensitive "OpenCode" with "Claude Code"
    let result = text.replace("OpenCode", "Claude Code");
    // Then replace remaining case-insensitive "opencode" with "Claude"
    // Use a simple approach: find and replace case-insensitively
    let mut output = String::with_capacity(result.len());
    let lower = result.to_lowercase();
    let mut i = 0;
    while i < result.len() {
        if i + 8 <= lower.len() && &lower[i..i + 8] == "opencode" {
            output.push_str("Claude");
            i += 8;
        } else {
            output.push(result.as_bytes()[i] as char);
            i += 1;
        }
    }
    output
}

/// Apply authentication headers to any header container.
/// Unified implementation for both HTTP HeaderMap and WebSocket Request headers.
fn apply_auth_headers<H: HeadersExt>(headers: &mut H, provider_type: ProviderType, api_key: &str) {
    if api_key.is_empty() {
        return;
    }

    // Special handling for Anthropic OAuth tokens - they use Bearer auth + specific headers.
    // Matches the opencode-anthropic-auth plugin: only Authorization, anthropic-beta, and
    // user-agent are set. The underlying Anthropic SDK (not us) handles x-stainless-* headers.
    if provider_type == ProviderType::Anthropic && is_anthropic_oauth_token(api_key) {
        if let Ok(value) = http::HeaderValue::from_str(&format!("Bearer {}", api_key)) {
            headers.insert_header(http::header::AUTHORIZATION, value);
        }
        // Required OAuth beta headers
        headers.insert_header(
            http::header::HeaderName::from_static("anthropic-beta"),
            http::HeaderValue::from_static("oauth-2025-04-20, interleaved-thinking-2025-05-14"),
        );
        headers.insert_header(
            http::header::HeaderName::from_static("user-agent"),
            http::HeaderValue::from_static("claude-cli/2.1.2 (external, cli)"),
        );
        return;
    }

    // Special handling for GitHub Copilot tokens - they need specific headers
    if provider_type == ProviderType::GithubCopilot {
        if let Ok(value) = http::HeaderValue::from_str(&format!("Bearer {}", api_key)) {
            headers.insert_header(http::header::AUTHORIZATION, value);
        }
        // Required Copilot headers (mimic VS Code extension)
        headers.insert_header(
            http::header::HeaderName::from_static("user-agent"),
            http::HeaderValue::from_static("GitHubCopilotChat/0.35.0"),
        );
        headers.insert_header(
            http::header::HeaderName::from_static("editor-version"),
            http::HeaderValue::from_static("vscode/1.107.0"),
        );
        headers.insert_header(
            http::header::HeaderName::from_static("editor-plugin-version"),
            http::HeaderValue::from_static("copilot-chat/0.35.0"),
        );
        headers.insert_header(
            http::header::HeaderName::from_static("copilot-integration-id"),
            http::HeaderValue::from_static("vscode-chat"),
        );
        return;
    }

    // Special handling for OpenAI Codex OAuth tokens - they need additional headers
    if provider_type == ProviderType::OpenAICodex && is_openai_codex_oauth_token(api_key) {
        if let Ok(value) = http::HeaderValue::from_str(&format!("Bearer {}", api_key)) {
            headers.insert_header(http::header::AUTHORIZATION, value);
        }

        // Extract and set account ID from JWT
        if let Some(account_id) = extract_openai_account_id(api_key) {
            if let Ok(value) = http::HeaderValue::from_str(&account_id) {
                headers.insert_header(
                    http::header::HeaderName::from_static("chatgpt-account-id"),
                    value,
                );
            }
        }

        // Add required Codex headers
        headers.insert_header(
            http::header::HeaderName::from_static("openai-beta"),
            http::HeaderValue::from_static("responses=experimental"),
        );
        headers.insert_header(
            http::header::HeaderName::from_static("originator"),
            http::HeaderValue::from_static("codex_cli_rs"),
        );
        return;
    }

    match provider_type.info().auth_style {
        AuthStyle::BearerToken => {
            if let Ok(value) = http::HeaderValue::from_str(&format!("Bearer {}", api_key)) {
                headers.insert_header(http::header::AUTHORIZATION, value);
            } else {
                tracing::warn!("Failed to create Authorization header: invalid API key characters");
            }
        }
        AuthStyle::ApiKeyHeader(name) => {
            let Ok(hname) = http::header::HeaderName::from_bytes(name.as_bytes()) else {
                tracing::warn!("Invalid header name for API key: {}", name);
                return;
            };
            if let Ok(value) = http::HeaderValue::from_str(api_key) {
                headers.insert_header(hname, value);
            } else {
                tracing::warn!(
                    "Failed to create {} header: invalid API key characters",
                    name
                );
            }
        }
        AuthStyle::AzureApiKey => {
            if let Ok(value) = http::HeaderValue::from_str(api_key) {
                headers.insert_header(http::header::HeaderName::from_static("api-key"), value);
            } else {
                tracing::warn!("Failed to create api-key header: invalid API key characters");
            }
        }
        AuthStyle::QueryParam(_) | AuthStyle::None => {}
    }
}

/// Apply provider-specific extra headers to any header container.
/// Unified implementation for both HTTP HeaderMap and WebSocket Request headers.
fn apply_extra_headers<H: HeadersExt>(headers: &mut H, provider_type: ProviderType) {
    match provider_type {
        ProviderType::Anthropic => {
            headers.insert_header(
                http::header::HeaderName::from_static("anthropic-version"),
                http::HeaderValue::from_static("2023-06-01"),
            );
        }
        ProviderType::OpenRouter => {
            headers.insert_header(
                http::header::HeaderName::from_static("http-referer"),
                http::HeaderValue::from_static("https://github.com/eavs-proxy"),
            );
        }
        _ => {}
    }
}

// Backwards-compatible wrappers (can be removed once all call sites are updated)
fn apply_ws_auth_headers(
    request: &mut http::Request<()>,
    provider_type: ProviderType,
    api_key: &str,
) {
    apply_auth_headers(request, provider_type, api_key);
}

fn apply_ws_extra_headers(request: &mut http::Request<()>, provider_type: ProviderType) {
    apply_extra_headers(request, provider_type);
}

fn apply_http_auth_headers(headers: &mut HeaderMap, provider_type: ProviderType, api_key: &str) {
    apply_auth_headers(headers, provider_type, api_key);
}

fn apply_http_extra_headers(headers: &mut HeaderMap, provider_type: ProviderType) {
    apply_extra_headers(headers, provider_type);
}

fn oauth_default_redirect_uri() -> String {
    std::env::var("EAVS_OAUTH_REDIRECT_URI").unwrap_or_else(|_| "http://localhost".to_string())
}

async fn resolve_oauth_access_token(
    state: &AppState,
    provider_name: &str,
    oauth_user: &str,
) -> Result<String, String> {
    resolve_oauth_access_token_with_account(state, provider_name, oauth_user, None).await
}

async fn resolve_oauth_access_token_with_account(
    state: &AppState,
    provider_name: &str,
    oauth_user: &str,
    oauth_account: Option<&str>,
) -> Result<String, String> {
    // Try to resolve from provider type first (more reliable when config key
    // differs from provider type, e.g., [providers.my-claude] with type = "anthropic").
    // Falls back to matching on the config section name for backward compatibility.
    let provider = state
        .config
        .resolve_provider(provider_name)
        .and_then(|lookup| OAuthProviderKind::from_str(&lookup.config.type_))
        .or_else(|| OAuthProviderKind::from_str(provider_name))
        .ok_or_else(|| {
            format!(
                "OAuth provider not supported for '{}'. Supported: anthropic, openai-codex, github-copilot, google-gemini-cli, google-antigravity",
                provider_name
            )
        })?;

    let store = state
        .get_oauth_store()
        .ok_or_else(|| "OAuth store not initialized".to_string())?;

    let account_label = oauth_account.unwrap_or("default");
    let mut credentials = store
        .get_credentials_for_account(oauth_user, provider, account_label)
        .await
        .map_err(|e| e.to_string())?
        .ok_or_else(|| {
            if account_label == "default" {
                "OAuth credentials not found".to_string()
            } else {
                format!("OAuth credentials not found for account '{}'", account_label)
            }
        })?;

    if credentials.is_expired(60) {
        if credentials.refresh_token.is_empty() {
            return Err("OAuth token expired and no refresh token available".to_string());
        }

        let client = reqwest::Client::new();
        let redirect_uri = std::env::var("EAVS_OAUTH_REDIRECT_URI")
            .ok()
            .filter(|v| !v.trim().is_empty())
            .unwrap_or_else(oauth_anthropic::default_redirect_uri);

        credentials = match provider {
            OAuthProviderKind::Anthropic => {
                let config =
                    oauth_anthropic::config_from_env(redirect_uri).map_err(|e| e.to_string())?;
                oauth_anthropic::refresh_token(
                    &client,
                    &config,
                    oauth_user,
                    &credentials.refresh_token,
                )
                .await
                .map_err(|e| e.to_string())?
            }
            OAuthProviderKind::OpenAICodex => {
                let config =
                    oauth_openai::config_from_env(redirect_uri).map_err(|e| e.to_string())?;
                oauth_openai::refresh_token(
                    &client,
                    &config,
                    oauth_user,
                    &credentials.refresh_token,
                )
                .await
                .map_err(|e| e.to_string())?
            }
            OAuthProviderKind::GoogleGeminiCli | OAuthProviderKind::GoogleAntigravity => {
                let config = oauth_google::config_from_env(provider, redirect_uri)
                    .map_err(|e| e.to_string())?;
                oauth_google::refresh_token(
                    &client,
                    &config,
                    oauth_user,
                    &credentials.refresh_token,
                )
                .await
                .map_err(|e| e.to_string())?
            }
            OAuthProviderKind::GithubCopilot => {
                // GitHub Copilot uses a two-step flow:
                // 1. refresh_token = GitHub OAuth access token (long-lived)
                // 2. access_token = Copilot API token (short-lived, exchanged on demand)
                // The "refresh" here exchanges the GitHub token for a new Copilot token.
                let github_token = if credentials.refresh_token.is_empty() {
                    // Older credential format: access_token is the GitHub token
                    &credentials.access_token
                } else {
                    &credentials.refresh_token
                };
                exchange_copilot_token(&client, oauth_user, github_token).await?
            }
        };

        // Preserve account_label from original credentials through refresh
        credentials.account_label = account_label.to_string();

        store
            .upsert_credentials(&credentials)
            .await
            .map_err(|e| e.to_string())?;
    }

    Ok(credentials.access_token)
}

/// Exchange a GitHub OAuth access token for a short-lived Copilot API token.
///
/// The Copilot token is obtained from `https://api.github.com/copilot_internal/v2/token`
/// and includes a `proxy-ep` field that determines the actual API base URL.
/// The token is short-lived (typically ~30 minutes) and must be re-exchanged.
async fn exchange_copilot_token(
    client: &reqwest::Client,
    user_id: &str,
    github_access_token: &str,
) -> Result<OAuthCredentials, String> {
    #[derive(serde::Deserialize)]
    struct CopilotTokenResponse {
        token: String,
        expires_at: i64,
    }

    let resp = client
        .get("https://api.github.com/copilot_internal/v2/token")
        .header("Accept", "application/json")
        .header("Authorization", format!("Bearer {}", github_access_token))
        .header("User-Agent", "GitHubCopilotChat/0.35.0")
        .header("Editor-Version", "vscode/1.107.0")
        .header("Editor-Plugin-Version", "copilot-chat/0.35.0")
        .header("Copilot-Integration-Id", "vscode-chat")
        .send()
        .await
        .map_err(|e| format!("Copilot token exchange request failed: {}", e))?;

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp
            .text()
            .await
            .unwrap_or_else(|_| "<unreadable>".to_string());
        return Err(format!(
            "Copilot token exchange failed ({}): {}",
            status, body
        ));
    }

    let token_resp: CopilotTokenResponse = resp
        .json()
        .await
        .map_err(|e| format!("Failed to parse Copilot token response: {}", e))?;

    // Parse proxy-ep from token to determine the actual API base URL.
    // Token format: tid=...;exp=...;proxy-ep=proxy.individual.githubcopilot.com;...
    let base_url = if let Some(proxy_ep) = token_resp
        .token
        .split(';')
        .find_map(|part| part.strip_prefix("proxy-ep="))
    {
        let api_host = proxy_ep.replace("proxy.", "api.");
        Some(format!("https://{}", api_host))
    } else {
        None
    };

    Ok(OAuthCredentials {
        user_id: user_id.to_string(),
        provider: OAuthProviderKind::GithubCopilot,
        account_label: "default".to_string(),
        access_token: token_resp.token,
        refresh_token: github_access_token.to_string(),
        expires_at: token_resp.expires_at,
        extra_data: base_url.map(|url| serde_json::json!({ "base_url": url })),
    })
}

async fn resolve_bedrock_aws_credentials(
    upstream: &dyn Upstream,
    provider_config: &crate::config::ProviderConfig,
) -> Result<AwsCredentials, String> {
    // 1) Explicit keys (flags/config/env) take precedence.
    if let (Some(access_key_id), Some(secret_access_key)) = (
        provider_config.resolved_aws_access_key_id(),
        provider_config.resolved_aws_secret_access_key(),
    ) {
        return Ok(AwsCredentials {
            access_key_id,
            secret_access_key,
            session_token: provider_config.resolved_aws_session_token(),
        });
    }

    // 2) Web identity (IRSA / OIDC).
    if let (Ok(token_file), Ok(role_arn)) = (
        std::env::var("AWS_WEB_IDENTITY_TOKEN_FILE"),
        std::env::var("AWS_ROLE_ARN"),
    ) {
        let token_file = token_file.trim().to_string();
        let role_arn = role_arn.trim().to_string();
        if !token_file.is_empty() && !role_arn.is_empty() {
            let token = std::fs::read_to_string(&token_file)
                .map_err(|e| format!("Failed to read AWS web identity token file: {}", e))?;
            let token = token.trim();
            if token.is_empty() {
                return Err("AWS web identity token file is empty".to_string());
            }

            let session_name = std::env::var("AWS_ROLE_SESSION_NAME")
                .ok()
                .filter(|s| !s.trim().is_empty())
                .unwrap_or_else(|| "eavs".to_string());

            let assumed = crate::aws_credentials::assume_role_with_web_identity(
                upstream,
                &role_arn,
                token,
                &session_name,
            )
            .await?;
            return Ok(assumed.creds);
        }
    }

    // 3) Shared credentials file + profile.
    let profile = crate::aws_credentials::aws_profile();
    if let Some(path) = crate::aws_credentials::default_shared_credentials_path() {
        if let Some(creds) = crate::aws_credentials::load_shared_credentials(&profile, &path) {
            return Ok(creds);
        }
    }

    Err("Bedrock provider requires AWS credentials. Provide aws_access_key_id/aws_secret_access_key, set AWS_ACCESS_KEY_ID/AWS_SECRET_ACCESS_KEY, set AWS_PROFILE with ~/.aws/credentials, or use AWS_ROLE_ARN + AWS_WEB_IDENTITY_TOKEN_FILE.".to_string())
}

fn bedrock_is_claude_model(model_id: &str) -> bool {
    let stripped = model_id
        .strip_prefix("us.")
        .or_else(|| model_id.strip_prefix("eu."))
        .or_else(|| model_id.strip_prefix("apac."))
        .unwrap_or(model_id);
    stripped.starts_with("anthropic.")
}

fn should_apply_anthropic_beta_headers(provider_type: ProviderType, model_id: &str) -> bool {
    match provider_type {
        ProviderType::Anthropic => true,
        ProviderType::Bedrock => bedrock_is_claude_model(model_id),
        _ => false,
    }
}

fn anthropic_beta_tokens_for_request(body: &Value) -> Vec<String> {
    let mut out: BTreeSet<String> = BTreeSet::new();
    anthropic_beta_scan_value(body, &mut out);
    out.into_iter().collect()
}

fn anthropic_beta_scan_value(v: &Value, out: &mut BTreeSet<String>) {
    match v {
        Value::Object(map) => {
            // Prompt caching (cache_control blocks).
            if map.contains_key("cache_control") {
                out.insert("prompt-caching-2024-07-31".to_string());
            }

            // PDFs: look for media_type application/pdf or document blocks.
            if map
                .get("media_type")
                .and_then(|m| m.as_str())
                .map(|m| m.eq_ignore_ascii_case("application/pdf"))
                .unwrap_or(false)
            {
                out.insert("pdfs-2024-09-25".to_string());
            }
            if map
                .get("type")
                .and_then(|t| t.as_str())
                .map(|t| t.eq_ignore_ascii_case("document"))
                .unwrap_or(false)
            {
                out.insert("pdfs-2024-09-25".to_string());
            }

            // Computer use: look for common markers (tool name / type).
            if map
                .get("type")
                .and_then(|t| t.as_str())
                .map(|t| {
                    t.eq_ignore_ascii_case("computer_use") || t.eq_ignore_ascii_case("computer")
                })
                .unwrap_or(false)
            {
                out.insert("computer-use-2024-10-22".to_string());
            }
            if map
                .get("name")
                .and_then(|n| n.as_str())
                .map(|n| n.eq_ignore_ascii_case("computer"))
                .unwrap_or(false)
            {
                out.insert("computer-use-2024-10-22".to_string());
            }

            // Files API: detect file_id/file_ids fields.
            if map.contains_key("file_id") || map.contains_key("file_ids") {
                out.insert("files-api-2025-04-14".to_string());
            }

            // MCP: detect mcp servers fields.
            if map.contains_key("mcp_servers")
                || map.contains_key("mcpServers")
                || map.contains_key("mcp")
            {
                out.insert("mcp-client-2025-04-04".to_string());
            }

            for (_, child) in map {
                anthropic_beta_scan_value(child, out);
            }
        }
        Value::Array(arr) => {
            for child in arr {
                anthropic_beta_scan_value(child, out);
            }
        }
        _ => {}
    }
}

fn upsert_csv_header(headers: &mut HeaderMap, name: http::header::HeaderName, values: Vec<String>) {
    if values.is_empty() {
        return;
    }

    let mut set: BTreeSet<String> = values.into_iter().collect();

    if let Some(existing) = headers.get(&name).and_then(|h| h.to_str().ok()) {
        for part in existing.split(',') {
            let trimmed = part.trim();
            if !trimmed.is_empty() {
                set.insert(trimmed.to_string());
            }
        }
    }

    let joined = set.into_iter().collect::<Vec<_>>().join(", ");
    if let Ok(value) = http::HeaderValue::from_str(&joined) {
        headers.insert(name, value);
    }
}

fn normalize_upstream_error(
    provider_type: ProviderType,
    status: StatusCode,
    body_text: &str,
    body_bytes: &[u8],
) -> (String, &'static str, Option<String>) {
    let mut message: Option<String> = None;
    let mut code: Option<String> = None;

    if let Ok(v) = serde_json::from_slice::<Value>(body_bytes) {
        let candidates = [
            v.pointer("/error/message").and_then(|v| v.as_str()),
            v.pointer("/error/error/message").and_then(|v| v.as_str()),
            v.pointer("/message").and_then(|v| v.as_str()),
            v.pointer("/error").and_then(|v| v.as_str()),
            v.pointer("/Message").and_then(|v| v.as_str()),
        ];
        message = candidates
            .into_iter()
            .flatten()
            .next()
            .map(|s| s.to_string());

        let code_candidates = [
            v.pointer("/error/code").and_then(|v| v.as_str()),
            v.pointer("/code").and_then(|v| v.as_str()),
            v.pointer("/error/type").and_then(|v| v.as_str()),
            v.pointer("/__type").and_then(|v| v.as_str()),
        ];
        code = code_candidates
            .into_iter()
            .flatten()
            .next()
            .map(|s| s.to_string());
    }

    if message
        .as_ref()
        .map(|m| m.trim().is_empty())
        .unwrap_or(true)
        && !body_text.trim().is_empty()
    {
        message = Some(body_text.trim().to_string());
    }

    let mut error_type = openai_error_type_for_status(status);

    if let Some(ref msg) = message {
        let lower = msg.to_lowercase();
        if lower.contains("context_length")
            || lower.contains("context length")
            || lower.contains("maximum context")
            || lower.contains("max context")
        {
            error_type = "context_length_exceeded";
        } else if lower.contains("content policy")
            || lower.contains("safety")
            || lower.contains("policy")
        {
            error_type = "content_policy_violation";
        }
    }

    // Provider-specific refinements.
    let _ = provider_type;

    (
        message.unwrap_or_else(|| status.to_string()),
        error_type,
        code,
    )
}

fn openai_error_type_for_status(status: StatusCode) -> &'static str {
    match status.as_u16() {
        400 | 404 | 409 | 413 | 422 => "invalid_request_error",
        401 | 403 => "authentication_error",
        429 => "rate_limit_error",
        500..=599 => "server_error",
        _ => "api_error",
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

/// Validate that a URL does not target internal/private network resources.
/// This prevents SSRF (Server-Side Request Forgery) attacks.
fn validate_url_not_internal(url: &url::Url) -> Result<(), String> {
    let host_str = url.host_str().ok_or("URL has no host")?;

    // Strip brackets from IPv6 addresses for consistent handling
    // url::Url.host_str() returns IPv6 addresses with brackets like "[::1]"
    let host = host_str.trim_start_matches('[').trim_end_matches(']');

    // Block localhost and loopback
    if host == "localhost" || host == "127.0.0.1" || host == "::1" {
        return Err(format!("blocked internal URL: {}", host));
    }

    // Block common internal hostnames
    let blocked_hostnames = [
        "metadata",
        "metadata.google.internal",
        "instance-data",
        "169.254.169.254", // AWS/GCP/Azure metadata service
        "fd00:ec2::254",   // AWS IMDSv2 IPv6
    ];
    if blocked_hostnames.contains(&host) {
        return Err(format!("blocked internal URL: {}", host));
    }

    // Parse IP address if host is an IP
    if let Ok(ip) = host.parse::<std::net::IpAddr>() {
        // Block private IP ranges (RFC 1918)
        let is_private = match ip {
            std::net::IpAddr::V4(ipv4) => {
                ipv4.is_private()           // 10.0.0.0/8, 172.16.0.0/12, 192.168.0.0/16
                || ipv4.is_loopback()       // 127.0.0.0/8
                || ipv4.is_link_local()     // 169.254.0.0/16 (includes metadata endpoint)
                || ipv4.is_broadcast()      // 255.255.255.255
                || ipv4.is_unspecified()    // 0.0.0.0
                || ipv4.octets()[0] == 100 && (ipv4.octets()[1] & 0xC0) == 64 // 100.64.0.0/10 (CGNAT)
            }
            std::net::IpAddr::V6(ipv6) => {
                ipv6.is_loopback()          // ::1
                || ipv6.is_unspecified()    // ::
                // Check for private/link-local IPv6 ranges
                || {
                    let segments = ipv6.segments();
                    // fc00::/7 (unique local)
                    (segments[0] & 0xfe00) == 0xfc00
                    // fe80::/10 (link-local)
                    || (segments[0] & 0xffc0) == 0xfe80
                }
            }
        };

        if is_private {
            return Err(format!("blocked private/internal IP address: {}", ip));
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

    // SSRF protection: block internal/private network URLs
    validate_url_not_internal(&parsed)?;

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
    use crate::upstream::UpstreamResponse;
    use axum::routing::{any, post};
    use axum::Router;
    use bytes::Bytes;
    use futures::{stream, StreamExt};
    use http::{HeaderMap, Method, StatusCode};
    use serde_json::json;
    use std::collections::HashMap;
    use std::sync::Arc;
    use tokio::sync::Mutex;
    use tower::util::ServiceExt;

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

        // Both system messages should be at the beginning, in original order
        assert_eq!(messages[0]["role"], "system");
        assert_eq!(messages[0]["content"], "First system");
        assert_eq!(messages[1]["role"], "system");
        assert_eq!(messages[1]["content"], "Second system");
        assert_eq!(messages[2]["role"], "user");
    }

    #[test]
    fn test_bedrock_is_claude_model_with_regional_prefix() {
        assert!(bedrock_is_claude_model(
            "anthropic.claude-3-opus-20240229-v1:0"
        ));
        assert!(bedrock_is_claude_model(
            "us.anthropic.claude-3-opus-20240229-v1:0"
        ));
        assert!(bedrock_is_claude_model(
            "eu.anthropic.claude-3-opus-20240229-v1:0"
        ));
        assert!(bedrock_is_claude_model(
            "apac.anthropic.claude-3-opus-20240229-v1:0"
        ));
        assert!(!bedrock_is_claude_model("meta.llama3-70b-instruct-v1:0"));
    }

    #[test]
    fn test_upsert_csv_header_merges_and_dedupes() {
        let mut headers = HeaderMap::new();
        headers.insert(
            http::header::HeaderName::from_static("anthropic-beta"),
            http::HeaderValue::from_static("prompt-caching-2024-07-31"),
        );

        upsert_csv_header(
            &mut headers,
            http::header::HeaderName::from_static("anthropic-beta"),
            vec![
                "prompt-caching-2024-07-31".to_string(),
                "pdfs-2024-09-25".to_string(),
            ],
        );

        let val = headers
            .get("anthropic-beta")
            .unwrap()
            .to_str()
            .unwrap()
            .to_string();
        assert!(val.contains("prompt-caching-2024-07-31"));
        assert!(val.contains("pdfs-2024-09-25"));
    }

    #[test]
    fn test_normalize_upstream_error_rate_limit() {
        let body = br#"{"error":{"message":"rate limited","type":"rate_limit_error"}}"#;
        let (msg, ty, code) = normalize_upstream_error(
            ProviderType::OpenAI,
            StatusCode::TOO_MANY_REQUESTS,
            std::str::from_utf8(body).unwrap(),
            body,
        );
        assert_eq!(msg, "rate limited");
        assert_eq!(ty, "rate_limit_error");
        assert!(code.is_some());
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

    #[test]
    fn test_ssrf_protection_blocks_localhost() {
        let url = url::Url::parse("http://localhost/image.png").unwrap();
        let result = super::validate_url_not_internal(&url);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("blocked internal URL"));
    }

    #[test]
    fn test_ssrf_protection_blocks_loopback() {
        let url = url::Url::parse("http://127.0.0.1/image.png").unwrap();
        let result = super::validate_url_not_internal(&url);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("blocked internal URL"));
    }

    #[test]
    fn test_ssrf_protection_blocks_metadata_endpoint() {
        let url = url::Url::parse("http://169.254.169.254/latest/meta-data/").unwrap();
        let result = super::validate_url_not_internal(&url);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("blocked internal URL"));
    }

    #[test]
    fn test_ssrf_protection_blocks_private_ip_10() {
        let url = url::Url::parse("http://10.0.0.1/image.png").unwrap();
        let result = super::validate_url_not_internal(&url);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("blocked private/internal IP"));
    }

    #[test]
    fn test_ssrf_protection_blocks_private_ip_172() {
        let url = url::Url::parse("http://172.16.0.1/image.png").unwrap();
        let result = super::validate_url_not_internal(&url);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("blocked private/internal IP"));
    }

    #[test]
    fn test_ssrf_protection_blocks_private_ip_192() {
        let url = url::Url::parse("http://192.168.1.1/image.png").unwrap();
        let result = super::validate_url_not_internal(&url);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("blocked private/internal IP"));
    }

    #[test]
    fn test_ssrf_protection_blocks_ipv6_loopback() {
        let url = url::Url::parse("http://[::1]/image.png").unwrap();
        let result = super::validate_url_not_internal(&url);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("blocked internal URL"));
    }

    #[test]
    fn test_ssrf_protection_blocks_ipv6_link_local() {
        let url = url::Url::parse("http://[fe80::1]/image.png").unwrap();
        let result = super::validate_url_not_internal(&url);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("blocked private/internal IP"));
    }

    #[test]
    fn test_ssrf_protection_allows_public_ip() {
        let url = url::Url::parse("https://example.com/image.png").unwrap();
        let result = super::validate_url_not_internal(&url);
        assert!(result.is_ok());
    }

    #[test]
    fn test_ssrf_protection_allows_public_ip_address() {
        let url = url::Url::parse("https://8.8.8.8/image.png").unwrap();
        let result = super::validate_url_not_internal(&url);
        assert!(result.is_ok());
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
            capture: Default::default(),
            transform: Default::default(),
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
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let text = String::from_utf8_lossy(&body);
        assert!(text.contains("data: [DONE]"));
        assert!(text.contains("\"hi\""));
    }

    #[tokio::test]
    async fn e2e_error_forwarding() {
        let mock = MockUpstream::new(vec![ResponseSpec {
            status: StatusCode::TOO_MANY_REQUESTS,
            headers: HeaderMap::new(),
            chunks: vec![Bytes::from_static(
                b"{\"error\":{\"message\":\"rate limited\"}}",
            )],
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
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        assert!(String::from_utf8_lossy(&body).contains("rate limited"));
    }

    #[tokio::test]
    async fn bedrock_fake_streaming_emits_sse() {
        let bedrock_body = json!({
            "id": "msg_1",
            "model": "anthropic.claude-3-opus-20240229-v1:0",
            "content": [{"type": "text", "text": "ok"}],
            "stop_reason": "end_turn",
            "usage": {"input_tokens": 1, "output_tokens": 1}
        });

        let mock = MockUpstream::new(vec![ResponseSpec {
            status: StatusCode::OK,
            headers: HeaderMap::new(),
            chunks: vec![Bytes::from(serde_json::to_vec(&bedrock_body).unwrap())],
        }]);

        let mut providers = HashMap::new();
        providers.insert(
            "default".to_string(),
            ProviderConfig {
                type_: "bedrock".to_string(),
                aws_region: "us-east-1".to_string(),
                aws_access_key_id: "AKIA_TEST".to_string(),
                aws_secret_access_key: "SECRET_TEST".to_string(),
                ..Default::default()
            },
        );

        let state = AppState::new_with_upstream(make_config(providers), Arc::new(mock));
        let app = Router::new()
            .route("/v1/*path", any(proxy_handler))
            .with_state(state);

        let req_body = json!({
            "model":"anthropic.claude-3-opus-20240229-v1:0",
            "messages":[{"role":"user","content":"hi"}],
            "stream": true,
            "max_tokens": 16
        });
        let req = http::Request::builder()
            .method("POST")
            .uri("/v1/chat/completions")
            .header(http::header::CONTENT_TYPE, "application/json")
            .body(Body::from(serde_json::to_vec(&req_body).unwrap()))
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(
            resp.headers()
                .get(http::header::CONTENT_TYPE)
                .and_then(|v| v.to_str().ok())
                .unwrap_or(""),
            "text/event-stream"
        );
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let text = String::from_utf8_lossy(&body);
        assert!(text.contains("[DONE]"));
        assert!(text.contains("\"ok\""));
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
        let _ = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();

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
            (
                img_headers,
                Bytes::from_static(&[1u8, 2, 3]),
                StatusCode::OK,
            ),
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
            chunks: vec![Bytes::from(
                serde_json::to_vec(&anthropic_response).unwrap(),
            )],
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
        assert!(requests
            .iter()
            .any(|r| r.method == Method::GET && r.url == "http://images.test/img.png"));

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
            chunks: vec![Bytes::from(
                serde_json::to_vec(&anthropic_response).unwrap(),
            )],
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
        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
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

        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
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
            .contains_key(http::header::HeaderName::from_static(
                "x-amz-content-sha256"
            )));

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
                let _ = axum::body::to_bytes(resp.into_body(), usize::MAX)
                    .await
                    .unwrap();
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
                    Bytes::from_static(
                        b"data: {\"choices\":[{\"delta\":{\"content\":\"hi\"}}]}\n\n",
                    ),
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
                let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
                    .await
                    .unwrap();
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
                chunks.push(Bytes::from_static(
                    b"data: {\"choices\":[{\"delta\":{\"content\":\".\"}}]}\n\n",
                ));
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
                let _ = axum::body::to_bytes(resp.into_body(), usize::MAX)
                    .await
                    .unwrap();
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

        tokio::time::timeout(std::time::Duration::from_secs(5), async move {
            tokio::join!(futures::future::join_all(stream_tasks), inject_task);
        })
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
        let url = build_ws_upstream_url(
            "https://api.example.com/v1",
            "/v1/realtime",
            Some("model=x"),
        )
        .unwrap();
        assert_eq!(url, "wss://api.example.com/v1/realtime?model=x");
    }

    #[tokio::test]
    async fn e2e_mock_provider_returns_synthetic_response() {
        // Mock upstream won't be called since mock provider handles requests internally
        let mock = MockUpstream::new(vec![]);

        let mut providers = HashMap::new();
        providers.insert(
            "mock".to_string(),
            ProviderConfig {
                type_: "mock".to_string(),
                ..Default::default()
            },
        );

        let state = AppState::new_with_upstream(make_config(providers), Arc::new(mock.clone()));
        let app = Router::new()
            .route("/v1/*path", any(proxy_handler))
            .with_state(state);

        // Non-streaming request
        let req_body = json!({
            "model": "mock-model",
            "messages": [{"role":"user","content":"hello"}],
            "stream": false
        });
        let req = http::Request::builder()
            .method("POST")
            .uri("/v1/chat/completions")
            .header(http::header::CONTENT_TYPE, "application/json")
            .header("X-Provider", "mock")
            .body(Body::from(serde_json::to_vec(&req_body).unwrap()))
            .unwrap();

        let resp = app.clone().oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(resp.headers().get("x-mock-response").unwrap(), "true");

        let body_bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let body: serde_json::Value = serde_json::from_slice(&body_bytes).unwrap();

        assert_eq!(body["object"], "chat.completion");
        assert_eq!(body["model"], "mock-model");
        assert!(body["choices"][0]["message"]["content"]
            .as_str()
            .unwrap()
            .contains("mock response"));

        // Verify no upstream requests were made
        let requests = mock.take_requests().await;
        assert_eq!(
            requests.len(),
            0,
            "Mock provider should not make upstream requests"
        );
    }

    #[tokio::test]
    async fn e2e_mock_provider_returns_streaming_response() {
        let mock = MockUpstream::new(vec![]);

        let mut providers = HashMap::new();
        providers.insert(
            "mock".to_string(),
            ProviderConfig {
                type_: "mock".to_string(),
                ..Default::default()
            },
        );

        let state = AppState::new_with_upstream(make_config(providers), Arc::new(mock.clone()));
        let app = Router::new()
            .route("/v1/*path", any(proxy_handler))
            .with_state(state);

        // Streaming request
        let req_body = json!({
            "model": "mock-model",
            "messages": [{"role":"user","content":"hello"}],
            "stream": true
        });
        let req = http::Request::builder()
            .method("POST")
            .uri("/v1/chat/completions")
            .header(http::header::CONTENT_TYPE, "application/json")
            .header("X-Provider", "mock")
            .body(Body::from(serde_json::to_vec(&req_body).unwrap()))
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(
            resp.headers().get("content-type").unwrap(),
            "text/event-stream"
        );

        let body_bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let body_str = String::from_utf8_lossy(&body_bytes);

        // Should contain SSE data lines
        assert!(body_str.contains("data: "));
        assert!(body_str.contains("mock response"));
        assert!(body_str.contains("[DONE]"));

        // Verify no upstream requests were made
        let requests = mock.take_requests().await;
        assert_eq!(requests.len(), 0);
    }
}

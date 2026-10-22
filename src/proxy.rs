use crate::aws_sigv4::{sign_request_headers, AwsCredentials};
use crate::keys::{is_virtual_key, ValidatedKey};
use crate::oauth::{
    anthropic as oauth_anthropic, openai_codex as oauth_openai, OAuthCredentials,
    OAuthProvider as OAuthProviderKind, OAuthResolveError,
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

/// Native inference operations do not use the chat-completions schema.
/// Keep this an explicit allowlist: provider credentials must not become a
/// general-purpose proxy to account, billing, or administrative endpoints.
fn native_inference_path(provider: ProviderType, path: &str) -> bool {
    let openai_media = path == "/v1/decisions"
        || path == "/v1/embeddings"
        || path == "/v1/images/generations"
        || path == "/v1/images/edits"
        || path == "/v1/images/variations"
        || matches!(
            path,
            "/v1/audio/speech" | "/v1/audio/transcriptions" | "/v1/audio/translations"
        )
        || path == "/v1/videos"
        || path.starts_with("/v1/videos/");
    match provider {
        ProviderType::OpenAI | ProviderType::OpenAIResponses | ProviderType::OpenAICompatible => {
            openai_media
        }
        ProviderType::TypeSafe => path == "/v1/systemone",
        ProviderType::ElevenLabs => {
            path.starts_with("/v1/text-to-speech/")
                || path == "/v1/speech-to-text"
                || path.starts_with("/v1/speech-to-text/")
        }
        ProviderType::Cohere => matches!(
            path,
            "/v1/eavs/rerank" | "/v2/rerank" | "/v2/embed" | "/v1/embed"
        ),
        ProviderType::Voyage => matches!(path, "/v1/eavs/rerank" | "/v1/rerank" | "/v1/embeddings"),
        _ => false,
    }
}

fn has_remote_media_reference(value: &Value) -> bool {
    match value {
        Value::Object(fields) => fields.iter().any(|(key, value)| {
            ((key == "url" || key.ends_with("_url"))
                && value
                    .as_str()
                    .is_some_and(|s| s.starts_with("http://") || s.starts_with("https://")))
                || has_remote_media_reference(value)
        }),
        Value::Array(values) => values.iter().any(has_remote_media_reference),
        _ => false,
    }
}

fn native_inference_method(path: &str, method: &http::Method) -> bool {
    if path.starts_with("/v1/videos/") {
        return *method == http::Method::GET;
    }
    *method == http::Method::POST
}

/// Preserve caller bytes (including meaningful field/choice ordering) unless policy rewrote JSON.
fn native_json_unchanged(is_native_inference: bool, bytes: &[u8], json_body: &Value) -> bool {
    is_native_inference
        && !bytes.is_empty()
        && serde_json::from_slice::<Value>(bytes).is_ok_and(|original| original == *json_body)
}

pub(crate) fn is_inference_path(path: &str) -> bool {
    path == "/v1/systemone"
        || path == "/v1/decisions"
        || path == "/v1/eavs/rerank"
        || path == "/v1/embeddings"
        || path.starts_with("/v1/images/")
        || matches!(
            path,
            "/v1/audio/speech" | "/v1/audio/transcriptions" | "/v1/audio/translations"
        )
        || path == "/v1/videos"
        || path.starts_with("/v1/videos/")
        || matches!(
            path,
            "/v1/rerank" | "/v2/rerank" | "/v1/embed" | "/v2/embed"
        )
        || path.starts_with("/v1/text-to-speech/")
        || path.starts_with("/v1/speech-to-text")
}

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

fn emit_delegated_fetch_audit(
    item: crate::delegated_fetch::StrippedItem,
    analysis_tx: &tokio::sync::broadcast::Sender<AnalysisEvent>,
    correlation_id: &str,
) {
    tracing::warn!(
        audit_event = "delegated_fetch_stripped",
        correlation_id,
        field_path = %item.field_path,
        capability = %item.capability,
        target_host = item.target_host.as_deref().unwrap_or(""),
        "Stripped provider-delegated fetch capability"
    );
    let _ = analysis_tx.send(AnalysisEvent::DelegatedFetchStripped {
        timestamp: chrono::Utc::now().timestamp_millis(),
        id: correlation_id.to_string(),
        field_path: item.field_path,
        capability: item.capability,
        target_host: item.target_host,
    });
}

fn sanitize_delegated_fetch_body(
    body: &mut Value,
    policy: &crate::delegated_fetch::DelegatedFetchPolicy,
    analysis_tx: &tokio::sync::broadcast::Sender<AnalysisEvent>,
    correlation_id: &str,
) {
    for item in crate::delegated_fetch::sanitize(body, policy) {
        emit_delegated_fetch_audit(item, analysis_tx, correlation_id);
    }
}

// Operator rewrites do not grant tenant delegated-fetch permission.
fn apply_policy_and_sanitize(
    body: &mut Value,
    policy: &crate::policy::PolicyConfig,
    provider: &str,
    path: &str,
    delegated_fetch: &crate::delegated_fetch::DelegatedFetchPolicy,
    analysis_tx: &tokio::sync::broadcast::Sender<AnalysisEvent>,
    correlation_id: &str,
) -> Result<(), crate::policy::PolicyViolation> {
    policy.apply(provider, path, body)?;
    sanitize_delegated_fetch_body(body, delegated_fetch, analysis_tx, correlation_id);
    Ok(())
}

fn sanitize_codex_ws_message(
    message: AxumWsMessage,
    policy: &crate::policy::PolicyConfig,
    provider: &str,
    permissions: Option<&crate::keys::KeyPermissions>,
    delegated_fetch: &crate::delegated_fetch::DelegatedFetchPolicy,
    analysis_tx: &tokio::sync::broadcast::Sender<AnalysisEvent>,
) -> Option<TungsteniteMessage> {
    let payload = match &message {
        AxumWsMessage::Text(text) => text.as_bytes(),
        AxumWsMessage::Binary(bytes) => bytes.as_slice(),
        _ => return axum_to_tungstenite(message),
    };
    let mut body = parse_and_sanitize_ws_json(payload, delegated_fetch, analysis_tx)?;
    if body.get("type").and_then(Value::as_str) == Some("response.create") {
        if !codex_frame_model_allowed(&body, permissions) {
            tracing::warn!("Codex WS frame rejected by model scope");
            return None;
        }
        if let Err(error) = apply_policy_and_sanitize(
            &mut body,
            policy,
            provider,
            "/codex/responses",
            delegated_fetch,
            analysis_tx,
            &Uuid::new_v4().to_string(),
        ) {
            tracing::warn!("Policy violation on Codex WS: {}", error.message);
            return None;
        }
        if !codex_frame_model_allowed(&body, permissions) {
            tracing::warn!("Codex WS policy rewrite rejected by model scope");
            return None;
        }
    }
    match message {
        AxumWsMessage::Text(_) => Some(TungsteniteMessage::Text(body.to_string())),
        _ => serde_json::to_vec(&body)
            .ok()
            .map(TungsteniteMessage::Binary),
    }
}

fn codex_frame_model_allowed(
    body: &Value,
    permissions: Option<&crate::keys::KeyPermissions>,
) -> bool {
    let Some(permissions) = permissions else {
        return true;
    };
    // With model restrictions, never inherit an unspecified upstream model.
    // The connection's synthetic `codex-ws` label is not a model authorization.
    let Some(model) = body.get("model").and_then(Value::as_str) else {
        return permissions.allowed_models.is_none();
    };
    permissions.is_model_allowed(model)
}

fn parse_and_sanitize_ws_json(
    payload: &[u8],
    policy: &crate::delegated_fetch::DelegatedFetchPolicy,
    analysis_tx: &tokio::sync::broadcast::Sender<AnalysisEvent>,
) -> Option<Value> {
    let correlation_id = Uuid::new_v4().to_string();
    if let Ok(mut body) = serde_json::from_slice::<Value>(payload) {
        sanitize_delegated_fetch_body(&mut body, policy, analysis_tx, &correlation_id);
        Some(body)
    } else {
        emit_delegated_fetch_audit(
            crate::delegated_fetch::StrippedItem {
                field_path: "/".to_string(),
                capability: "invalid_json_frame_rejected".to_string(),
                target_host: None,
            },
            analysis_tx,
            &correlation_id,
        );
        None
    }
}

/// Namespace conversation/injection state by the authenticated virtual key.
///
/// The client-controlled `X-Conversation-ID` header is never trusted as a
/// tenant boundary. For an authenticated virtual key, state is isolated under
/// `key:<key_hash>:<conversation_id>` so a different key (or the operator's
/// default namespace) cannot steal, consume, or receive another tenant's
/// injections or conversation state. When no virtual key is present
/// (operator/plain use), the raw id is preserved for backward compatibility.
pub fn conversation_namespace(conversation_id: &str, key_hash: Option<&str>) -> String {
    match key_hash {
        Some(kh) => format!("key:{kh}:{conversation_id}"),
        None => conversation_id.to_string(),
    }
}

/// Extract the model a Realtime WebSocket client actually requests.
///
/// The `OpenAI` Realtime protocol carries the model in the connection query
/// string (`?model=...`). Validating against this actual model (rather than a
/// fixed sentinel) is what makes virtual-key model scope apply to realtime.
pub fn extract_realtime_model(uri: &http::Uri) -> String {
    uri.query()
        .and_then(|q| {
            url::form_urlencoded::parse(q.as_bytes())
                .find(|(k, _)| k == "model")
                .map(|(_, v)| v.into_owned())
        })
        .unwrap_or_else(|| "websocket".to_string())
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

/// Handler for POST /:provider/v1/codex/responses (Codex SSE transport).
/// The GET method on the same path is the WebSocket upgrade; pi falls back
/// to SSE POSTs when the WebSocket transport is unavailable or rate-limited.
pub async fn provider_codex_sse_handler(
    State(state): State<AppState>,
    Path(provider): Path<String>,
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
    let state = state.pin_routes();
    let generation = state.file_routing_generation();
    let mut result = if crate::pi_messages::matches(req.uri().path(), path_provider.as_deref()) {
        Ok(crate::pi_messages::handle(state, req, path_provider).await)
    } else {
        proxy_handler_pinned(state, req, path_provider).await
    };
    let response = match &mut result {
        Ok(response) | Err(response) => response,
    };
    if let Ok(value) = generation.parse() {
        response
            .headers_mut()
            .insert("x-eavs-file-routing-generation", value);
    }
    result
}

pub async fn proxy_handler_pinned(
    state: AppState,
    req: Request<Body>,
    path_provider: Option<String>,
) -> Result<Response, Response> {
    let state = state.pin_routes();
    let pi_interface = req
        .extensions()
        .get::<crate::pi_messages::Interface>()
        .is_some();
    // 1. Generate Correlation ID
    let correlation_id = Uuid::new_v4().to_string();

    // Extract Authorization header for key validation
    let auth_header = req
        .headers()
        .get("authorization")
        .and_then(|h| h.to_str().ok())
        .and_then(|h| h.strip_prefix("Bearer "))
        .map(std::string::ToString::to_string);

    // Extract conversation ID and provider selection from headers
    let conversation_id = req
        .headers()
        .get("X-Conversation-ID")
        .and_then(|h| h.to_str().ok())
        .map_or_else(|| "default".to_string(), std::string::ToString::to_string);

    // Provider selection priority (pre-body):
    // 1. Path parameter (/{provider}/v1/...)
    // 2. X-Provider header
    // 3. X-Original-Host header (mitmproxy capture mode)
    // Model-based detection happens after body parsing
    let header_provider = req
        .headers()
        .get("X-Provider")
        .and_then(|h| h.to_str().ok())
        .map(std::string::ToString::to_string);

    let original_host = req
        .headers()
        .get("X-Original-Host")
        .and_then(|h| h.to_str().ok())
        .map(std::string::ToString::to_string);

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
        .is_some_and(|v| v.split(',').any(|p| p.trim() == "claude-code-20250219"));

    // Use configurable max body size to prevent DoS attacks
    let max_body_size = if state.config.server.max_body_size > 0 {
        state.config.server.max_body_size
    } else {
        usize::MAX
    };
    let max_body_size = parts
        .extensions
        .get::<crate::inference_ingress::NativeBodyLimit>()
        .map_or(max_body_size, |limit| max_body_size.min(limit.0));

    let bytes = axum::body::to_bytes(body, max_body_size)
        .await
        .map_err(|e| {
            // Check if it's a size limit error
            let error_msg = e.to_string();
            if error_msg.contains("length limit") || error_msg.contains("too large") {
                (
                    StatusCode::PAYLOAD_TOO_LARGE,
                    Json(ProxyError::new(
                        format!("Request body too large. Maximum size: {max_body_size} bytes"),
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

    // pi's Codex SSE transport sends zstd-compressed request bodies
    // (content-encoding: zstd); decompress before parsing/forwarding.
    let bytes = if parts
        .headers
        .get(http::header::CONTENT_ENCODING)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.trim().eq_ignore_ascii_case("zstd"))
    {
        let decoded = zstd::stream::read::Decoder::new(bytes.as_ref()).and_then(|decoder| {
            let mut decoded = Vec::new();
            std::io::Read::read_to_end(
                &mut std::io::Read::take(decoder, max_body_size.saturating_add(1) as u64),
                &mut decoded,
            )?;
            Ok(decoded)
        });
        match decoded {
            Ok(decoded) if decoded.len() <= max_body_size => Bytes::from(decoded),
            Ok(_) => {
                return Err((
                    StatusCode::PAYLOAD_TOO_LARGE,
                    Json(ProxyError::new(
                        "Decoded request body too large",
                        "request_too_large",
                    )),
                )
                    .into_response())
            }
            Err(e) => {
                return Err((
                    StatusCode::BAD_REQUEST,
                    Json(ProxyError::new(
                        format!("Failed to decompress zstd request body: {e}"),
                        "invalid_request",
                    )),
                )
                    .into_response());
            }
        }
    } else {
        bytes
    };

    let mut json_body: Value = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or(Value::Null)
    };

    // Extract model from request for validation
    // URL-addressed model RPCs must authorize the actual path model before
    // any body model or synthetic unknown label can affect admission.
    let model = google_model_from_path(parts.uri.path())
        .or_else(|| {
            json_body
                .get("model")
                .or_else(|| json_body.get("model_id"))
                .and_then(|m| m.as_str())
                .map(str::to_owned)
        })
        .unwrap_or_else(|| "unknown".to_string());

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
    // The guess names a provider kind. Use the provider of that name, else the
    // single configured provider of that type (e.g. `default` with
    // type = "openai" for a gpt-* model); anything else is rejected below.
    let model_matched_provider = model_detected_provider.map(|guess| {
        if state.resolve_provider(guess).is_some() {
            return guess.to_string();
        }
        let mut of_type = state
            .config
            .providers
            .iter()
            .filter(|(_, config)| config.type_.eq_ignore_ascii_case(guess))
            .map(|(name, _)| name);
        match (of_type.next(), of_type.next()) {
            (Some(name), None) => name.clone(),
            _ => guess.to_string(),
        }
    });

    let provider_name_requested = pre_body_provider
        .or(runtime_default_provider.as_deref())
        .or(model_matched_provider.as_deref())
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

    // Resolve provider config. A model-name guess only selects a provider
    // that is configured; otherwise the client gets the remedy, never a
    // silent fallback to "default".
    let provider_selected_by_model = pre_body_provider.is_none()
        && runtime_default_provider.is_none()
        && model_detected_provider.is_some();
    let Some(provider_lookup) = state.resolve_provider(&provider_name_requested) else {
        return Err(unknown_provider_response(
            &state,
            &parts.headers,
            &provider_name_requested,
            provider_selected_by_model.then_some(model.as_str()),
        )
        .await);
    };

    let provider_config = &provider_lookup.config;
    if matches!(
        provider_config.provider_type(),
        ProviderType::Fal | ProviderType::Replicate
    ) {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(ProxyError::new(
                "Use the tenant-bound /{provider}/v1/eavs/jobs API for asynchronous jobs",
                "unsupported_api_shape",
            )),
        )
            .into_response());
    }
    let api_path = if let Some(ref provider) = path_provider {
        parts
            .uri
            .path()
            .strip_prefix(&format!("/{provider}"))
            .unwrap_or(parts.uri.path())
    } else {
        parts.uri.path()
    };
    // An allowlisted prefix must never be escapable through URL normalization
    // or a backend decoding encoded separators/dot segments.
    if api_path.contains('%')
        || api_path.contains('\\')
        || api_path.split('/').any(|s| s == "." || s == "..")
    {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(ProxyError::new(
                "Encoded or traversal path segments are not allowed",
                "invalid_path",
            )),
        )
            .into_response());
    }
    let is_native_inference = native_inference_path(provider_config.provider_type(), api_path);
    if (is_inference_path(api_path) && !is_native_inference)
        || (matches!(
            provider_config.provider_type(),
            ProviderType::TypeSafe
                | ProviderType::ElevenLabs
                | ProviderType::Cohere
                | ProviderType::Voyage
        ) && !is_native_inference
            && !(api_path == "/v1/models"
                && provider_config.provider_type().supports_models_endpoint()))
    {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(ProxyError::new(
                "This provider does not support the requested native inference API shape",
                "unsupported_api_shape",
            )),
        )
            .into_response());
    }
    if is_native_inference && !native_inference_method(api_path, &parts.method) {
        return Err((
            StatusCode::METHOD_NOT_ALLOWED,
            Json(ProxyError::new(
                "Method not supported for this inference endpoint",
                "unsupported_method",
            )),
        )
            .into_response());
    }
    let is_opaque_inference = is_native_inference
        && !bytes.is_empty()
        && !parts
            .headers
            .get(http::header::CONTENT_TYPE)
            .and_then(|h| h.to_str().ok())
            .is_some_and(|s| s.starts_with("application/json"));
    if is_native_inference && !is_opaque_inference && !bytes.is_empty() && json_body.is_null() {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(ProxyError::new(
                "Invalid JSON inference request",
                "invalid_request",
            )),
        )
            .into_response());
    }
    if is_opaque_inference {
        let allowed_upload = api_path == "/v1/images/edits"
            || api_path == "/v1/images/variations"
            || api_path.starts_with("/v1/speech-to-text")
            || matches!(
                api_path,
                "/v1/audio/transcriptions" | "/v1/audio/translations"
            )
            || api_path.starts_with("/v1/text-to-speech/");
        if !allowed_upload
            || !parts
                .headers
                .get(http::header::CONTENT_TYPE)
                .and_then(|h| h.to_str().ok())
                .is_some_and(|s| {
                    s.starts_with("multipart/form-data;")
                        || s.starts_with("audio/")
                        || s == "application/octet-stream"
                })
        {
            return Err((
                StatusCode::UNSUPPORTED_MEDIA_TYPE,
                Json(ProxyError::new(
                    "Unsupported native inference upload content type",
                    "unsupported_media_type",
                )),
            )
                .into_response());
        }
        if state.config.policy.enabled
            && state
                .config
                .policy
                .rules
                .iter()
                .any(|rule| !matches!(rule, crate::policy::PolicyRule::Deny { .. }))
        {
            return Err((
                StatusCode::BAD_REQUEST,
                Json(ProxyError::new(
                    "Body-mutating policy rules cannot be applied to binary uploads",
                    "unsupported_policy",
                )),
            )
                .into_response());
        }
    }
    let resolved_provider = provider_lookup.resolved_name.clone();
    let provider_name = resolved_provider.clone();

    if provider_name_requested != provider_name {
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
                    .map_or(0, |calc| calc.estimate_request_tokens(&json_body, &model));

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
                        "A valid virtual API key is required. Keys must start with 'eavs-'",
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

    // Opaque uploads cannot prove the physical body model. Never authorize
    // them against the synthetic "unknown" label when a key restricts models.
    if is_opaque_inference
        && validated_key.as_ref().is_some_and(|key| {
            key.permissions.allowed_models.is_some() || key.permissions.blocked_models.is_some()
        })
    {
        return Err((
            StatusCode::FORBIDDEN,
            Json(ProxyError::new(
                "Model-scoped keys require a verifiable native upload model",
                "unsupported_model_scope",
            )),
        )
            .into_response());
    }

    // The OpenAI video job ID is a bearer-like handle at the upstream. With a
    // shared provider credential, virtual tenants must not access each other's
    // jobs by guessing/leaking an ID. Reject the whole lifecycle rather than
    // allowing submission of jobs that this key cannot retrieve safely.
    if (api_path == "/v1/videos" || api_path.starts_with("/v1/videos/")) && validated_key.is_some()
    {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(ProxyError::new(
                "Native video jobs are not tenant-bound; virtual keys cannot use this route",
                "unsupported_api_shape",
            )),
        )
            .into_response());
    }
    // Native providers use incompatible or non-token billing units. Until cost
    // adapters exist, a virtual key's budget/TPM must not silently be bypassed.
    // Canonical rerank is not an exception: reported units are not a verified
    // pricing contract and durable accounting is not implemented (eavs-avm8).
    if is_native_inference
        && validated_key.as_ref().is_some_and(|key| {
            key.permissions.max_budget_usd.is_some() || key.permissions.tpm_limit.is_some()
        })
    {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(ProxyError::new(
                "Native inference cannot enforce this virtual key's budget or token limit yet",
                "unsupported_metering",
            )),
        )
            .into_response());
    }

    // Provider-side URL fetching and hosted tools bypass EAVS's network ACL.
    // Resolve tenant metadata after key validation, then sanitize before any
    // routing transform or passthrough serialization.
    let delegated_fetch_policy = state
        .config
        .delegated_fetch
        .policy_for_key_metadata(validated_key.as_ref().map(|key| &key.metadata));
    sanitize_delegated_fetch_body(
        &mut json_body,
        &delegated_fetch_policy,
        &state.analysis_tx,
        &correlation_id,
    );

    // Namespace conversation/injection state by the authenticated key so a
    // different key using the same client-controlled X-Conversation-ID cannot
    // steal, consume, or receive another tenant's injections.
    let ns_conversation = conversation_namespace(
        &conversation_id,
        validated_key.as_ref().map(|k| k.key_hash.as_str()),
    );

    // Register/update conversation in store if capture_all is enabled
    if state.config.state.capture_all {
        let _ = state.conversations.get_or_create(&ns_conversation);
        state
            .conversations
            .update_metadata(&ns_conversation, |meta| {
                meta.provider = Some(provider_name.clone());
                meta.model = Some(model.clone());
                meta.request_count += 1;
            });
    }

    // Check for injections (new conversation store)
    if !is_native_inference {
        let injections = state.conversations.take_injections(&ns_conversation);
        if !injections.is_empty() {
            apply_injections(&mut json_body, &injections);
        }

        // Legacy fallback: check old injections map
        if let Some((_, legacy_injections)) = state.injections.remove(&ns_conversation) {
            apply_injections(&mut json_body, &legacy_injections);
        }
    }

    // Apply policy rules (deny/rewrite/filter) after injection.
    if let Err(err) = apply_policy_and_sanitize(
        &mut json_body,
        &state.config.policy,
        &provider_name,
        parts.uri.path(),
        &delegated_fetch_policy,
        &state.analysis_tx,
        &correlation_id,
    ) {
        return Err((
            StatusCode::FORBIDDEN,
            Json(ProxyError::new(err.message, "policy_violation")),
        )
            .into_response());
    }

    // Policies can rewrite the model after initial authentication. Authorize
    // the effective body again without charging request/rate limits twice.
    // Native URL-addressed models still use the already-resolved path model.
    if let Some(ref validated) = validated_key {
        let effective_model = json_body
            .get("model")
            .and_then(Value::as_str)
            .unwrap_or(&model);
        if !validated.permissions.is_model_allowed(effective_model) {
            return Err((
                StatusCode::FORBIDDEN,
                Json(
                    ProxyError::new(
                        format!("Model '{effective_model}' is not allowed by this key"),
                        "authentication_error",
                    )
                    .with_code("model_not_allowed"),
                ),
            )
                .into_response());
        }
    }

    // Check the final body: policy rewrites may introduce new remote URLs.
    if is_native_inference
        && !delegated_fetch_policy.allow_remote_content
        && (api_path.starts_with("/v1/images/") || api_path.starts_with("/v1/videos"))
        && has_remote_media_reference(&json_body)
    {
        return Err((
            StatusCode::FORBIDDEN,
            Json(ProxyError::new(
                "Remote media references require delegated fetch permission",
                "network_policy",
            )),
        )
            .into_response());
    }

    // Sanitize empty text content parts in messages.
    // Some providers (e.g. Azure Foundry / Kimi-K2.5) reject requests with
    // `{"text": "", "type": "text"}` content parts (422 "text is not provided").
    // Replace empty text with a single space to keep the message structure intact.
    sanitize_empty_text_parts(&mut json_body);

    // Log Request
    let _ = state.analysis_tx.send(AnalysisEvent::Request {
        timestamp: chrono::Utc::now().timestamp_millis(),
        id: correlation_id.clone(),
        method: parts.method.to_string(),
        uri: parts.uri.to_string(),
        // Native responses can contain large base64 media or sensitive documents.
        // Audit metadata, never the raw content of native inference calls.
        body: if is_native_inference {
            json!({"native_api": api_path, "model": model, "request_bytes": bytes.len()})
        } else {
            json_body.clone()
        },
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
                    let msg = msg.to_string();
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

    // Native-only providers have no OpenAI-format synthetic catalog.
    if api_path == "/v1/models"
        && matches!(provider_type, ProviderType::Cohere | ProviderType::Voyage)
    {
        return Err((
            StatusCode::NOT_FOUND,
            Json(ProxyError::new(
                "This provider does not expose a compatible models endpoint",
                "unsupported_api_shape",
            )),
        )
            .into_response());
    }
    // Handle /v1/models endpoint for providers that don't support it natively
    // Return a synthetic response with known models for that provider
    if api_path == "/v1/models" && !provider_type.supports_models_endpoint() {
        let models = provider_type.synthetic_models();
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_secs());

        let models_response = serde_json::json!({
            "object": "list",
            "data": models.iter().map(|m| {
                serde_json::json!({
                    "id": m,
                    "object": "model",
                    "created": now,
                    "owned_by": format!("{provider_type:?}").to_lowercase()
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

    // Native-format client requests: the client already speaks the upstream's
    // wire format (e.g. pi configured with api=anthropic-messages or
    // api=google-generative-ai), so relay instead of translating from OpenAI
    // chat-completions format. The advertised base URL is {eavs}/{provider}/v1
    // and vendor SDKs append their own path, so Anthropic Messages arrives as
    // /v1/v1/messages and Google as /v1/models/{model}:streamGenerateContent.
    let is_anthropic_native_request = provider_type == ProviderType::Anthropic
        && matches!(api_path, "/messages" | "/v1/messages" | "/v1/v1/messages");
    let is_google_native_request =
        matches!(
            provider_type,
            ProviderType::Google | ProviderType::GoogleVertex
        ) && [":generateContent", ":streamGenerateContent", ":countTokens"]
            .iter()
            .any(|action| api_path.contains(action));
    let is_native_format_request = is_anthropic_native_request || is_google_native_request;

    // Check if this is a pass-through endpoint that doesn't need transformation
    // (e.g., /v1/models, /v1/embeddings for providers that support them natively)
    let is_passthrough_endpoint = !is_native_format_request
        && (api_path == "/v1/models" || api_path.starts_with("/v1/models/") || is_native_inference);

    // Check if client is sending Responses API format directly
    // This allows clients to use the Responses API natively with EAVS just handling OAuth
    let is_responses_api_request = api_path == "/v1/responses"
        || api_path == "/responses"
        || api_path == "/codex/responses"
        || api_path == "/v1/codex/responses";

    // Check if we need format translation
    // Skip transformation for pass-through endpoints, native Responses API
    // requests, and native-format requests
    let needs_transform = pi_interface
        || provider_type.needs_transform()
            && !is_passthrough_endpoint
            && !is_responses_api_request
            && !is_native_format_request;

    // Resolve compat settings for OpenAI-compatible providers
    let resolved_compat = provider_config.resolved_compat();
    if !resolved_compat.supports_developer_role()
        || !resolved_compat.supports_store()
        || !resolved_compat.supports_stream_options()
        || resolved_compat.max_tokens_field() != "max_completion_tokens"
    {
        tracing::debug!(
            provider = %provider_name,
            developer_role = resolved_compat.supports_developer_role(),
            store = resolved_compat.supports_store(),
            stream_options = resolved_compat.supports_stream_options(),
            max_tokens_field = resolved_compat.max_tokens_field(),
            "Using compat settings for provider"
        );
    }

    // Get the transformer for this provider (with compat settings wired in)
    let transformer =
        ProviderTransformer::for_provider_with_compat(provider_type, Some(&resolved_compat));

    // 4. Build request body - transform if needed
    let mut transformed_endpoint_path: Option<String> = None;
    let mut request_stream = false;
    let mut fake_streaming = false;
    let mut beta_scan_body: Option<Value> = None;
    let (request_body, model_name) = if needs_transform {
        // Parse incoming OpenAI-format request to canonical Context
        let parsed = if pi_interface {
            crate::pi_messages::canonical::context(&json_body, &provider_name, provider_type)
        } else {
            parse_incoming_request(&json_body)
        };
        let mut context = parsed.map_err(|e| {
            tracing::error!("Failed to parse request: {}", e);
            (
                StatusCode::BAD_REQUEST,
                Json(ProxyError::new(
                    format!("Failed to parse request: {e}"),
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
                | ProviderType::Bedrock
        ) {
            resolve_image_urls_in_context(state.upstream.as_ref(), &mut context)
                .await
                .map_err(|e| {
                    (
                        StatusCode::BAD_REQUEST,
                        Json(ProxyError::new(
                            format!("Failed to resolve image URL: {e}"),
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
            (status, Json(ProxyError::new(format!("{e}"), error_type))).into_response()
        })?;

        if pi_interface {
            crate::pi_messages::canonical::finish(
                &mut transformed,
                &context,
                &provider_name,
                provider_type,
            )
            .map_err(|_| {
                (
                    StatusCode::BAD_REQUEST,
                    Json(ProxyError::new(
                        "Pi request cannot be represented by this route",
                        "unsupported_pi_request",
                    )),
                )
                    .into_response()
            })?;
        }
        if is_anthropic_oauth {
            prefix_anthropic_oauth_tools(&mut transformed);
            apply_anthropic_oauth_body_transforms(&mut transformed);
        }

        beta_scan_body = Some(transformed.clone());

        let body = serde_json::to_vec(&transformed).map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ProxyError::new(
                    format!("Failed to serialize request: {e}"),
                    "internal_error",
                )),
            )
                .into_response()
        })?;
        (body, model)
    } else {
        // Explicit Eavs rerank contract: Cohere's top_n/results shape, with
        // Voyage's top_k/data translated at the provider boundary.
        if api_path == "/v1/eavs/rerank" && provider_type == ProviderType::Voyage {
            if let Some(body) = json_body.as_object_mut() {
                if let Some(top_n) = body.remove("top_n") {
                    body.insert("top_k".to_string(), top_n);
                }
            }
        }
        // Pass through for OpenAI-compatible providers
        if !is_native_format_request
            && !is_responses_api_request
            && api_path == "/v1/chat/completions"
        {
            crate::transform::openai::apply_compat_to_passthrough_body(
                &mut json_body,
                &resolved_compat,
            );
        }

        if provider_type == ProviderType::Mistral && api_path == "/v1/chat/completions" {
            crate::transform::mistral::transform_openai_request_for_mistral(&mut json_body)
                .map_err(|e| {
                    tracing::error!("Failed to apply Mistral request quirks: {}", e);
                    (
                        StatusCode::BAD_REQUEST,
                        Json(ProxyError::new(
                            format!("Failed to transform request for Mistral: {e}"),
                            "invalid_request",
                        )),
                    )
                        .into_response()
                })?;
        }

        // Native Anthropic Messages requests with OAuth tokens need the same
        // Claude Code identity/tool adjustments the transform path applies.
        if is_anthropic_native_request && is_anthropic_oauth {
            inject_claude_code_identity_into_anthropic_body(&mut json_body);
            prefix_anthropic_oauth_tools(&mut json_body);
            apply_anthropic_oauth_body_transforms(&mut json_body);
        }

        // Native Google requests carry the model in the URL, not the body.
        let model = json_body["model"]
            .as_str()
            .map(str::to_string)
            .or_else(|| {
                if is_google_native_request {
                    google_model_from_path(api_path)
                } else {
                    None
                }
            })
            .unwrap_or_else(|| "unknown".to_string());
        let body = if is_opaque_inference
            || native_json_unchanged(is_native_inference, &bytes, &json_body)
        {
            bytes.to_vec()
        } else if bytes.is_empty() && is_native_inference {
            Vec::new()
        } else {
            serde_json::to_vec(&json_body).map_err(|e| {
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(ProxyError::new(
                        format!("Failed to serialize request: {e}"),
                        "internal_error",
                    )),
                )
                    .into_response()
            })?
        };
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
            let prefix = format!("/{provider}");
            request_path.strip_prefix(&prefix).unwrap_or(request_path)
        } else {
            request_path
        };

        // Canonicalize native-format paths: drop the /v1 routing artifact that
        // comes from advertising {eavs}/{provider}/v1 as the client base URL.
        let request_path = if is_anthropic_native_request {
            "/v1/messages"
        } else if is_google_native_request {
            request_path.strip_prefix("/v1").unwrap_or(request_path)
        } else if request_path == "/v1/eavs/rerank" {
            if provider_type == ProviderType::Cohere {
                "/v2/rerank"
            } else {
                "/v1/rerank"
            }
        } else if request_path == "/v1/codex/responses" {
            // Codex SSE upstream is {base}/codex/responses on chatgpt.com/backend-api
            "/codex/responses"
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
                format!("/openai/deployments/{deployment}{stripped_path}")
            }
        } else {
            stripped_path.to_string()
        }
    };

    let mut url = format!("{base}{path}");

    // Handle Query Parameters (Original + API Version for Azure)
    let mut query_string = if needs_transform {
        String::new() // Transformer includes query params in path if needed
    } else {
        parts
            .uri
            .query()
            .map(std::string::ToString::to_string)
            .unwrap_or_default()
    };

    if let Some(ref ver) = provider_config.resolved_api_version() {
        if !query_string.is_empty() {
            query_string.push('&');
        }
        query_string.push_str(&format!("api-version={ver}"));
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

    // Adjust for upstream behavior already known for this model (accepted
    // reasoning efforts, adaptive thinking), declared or learned from a 400.
    let effort_capable = !is_native_inference && provider_type != ProviderType::Bedrock;
    let (request_body, adjustments) = if effort_capable {
        apply_known_adjustments(
            &state,
            provider_config,
            &provider_name,
            &model_name,
            request_body,
        )
    } else {
        (request_body, Vec::new())
    };
    let request_body = Bytes::from(request_body);

    // Build upstream request
    let mut upstream_headers = HeaderMap::new();
    if !request_body.is_empty() {
        if is_opaque_inference {
            if let Some(content_type) = parts.headers.get(http::header::CONTENT_TYPE) {
                upstream_headers.insert(http::header::CONTENT_TYPE, content_type.clone());
            }
        } else {
            upstream_headers.insert(
                http::header::CONTENT_TYPE,
                http::HeaderValue::from_static("application/json"),
            );
        }
    }

    apply_http_auth_headers(&mut upstream_headers, provider_type, &api_key);
    apply_http_extra_headers(&mut upstream_headers, provider_type);
    apply_opencode_session_headers(&mut upstream_headers, provider_type, &conversation_id);

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

    // Only the isolated inference ingress can supply these allowlisted,
    // key-namespaced retry/request semantics; never copy arbitrary client headers.
    if let Some(headers) = parts
        .extensions
        .get::<crate::inference_ingress::NativeHeaders>()
    {
        for (name, value) in &headers.0 {
            upstream_headers.insert(name.clone(), value.clone());
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

    // Native Anthropic clients (e.g. pi) select beta features via the
    // anthropic-beta header (fine-grained tool streaming, interleaved
    // thinking, ...) that can't all be derived from the body — forward them.
    if is_anthropic_native_request {
        if let Some(client_betas) = parts
            .headers
            .get("anthropic-beta")
            .and_then(|h| h.to_str().ok())
        {
            let tokens: Vec<String> = client_betas
                .split(',')
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect();
            upsert_csv_header(
                &mut upstream_headers,
                http::header::HeaderName::from_static("anthropic-beta"),
                tokens,
            );
        }
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
                    format!("Failed to sign Bedrock request: {e}"),
                    "invalid_request",
                )),
            )
                .into_response()
        })?;
    }

    // Network access control check
    if let Err(reason) = crate::network_acl::check_url_allowed(&state.config.network, &url) {
        tracing::warn!("Network ACL blocked request to {}: {}", url, reason);
        return Err((
            StatusCode::FORBIDDEN,
            Json(ProxyError::new(reason, "network_policy").with_code("network_blocked")),
        )
            .into_response());
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
        let dynamic_trigger = json_body
            .get("messages")
            .and_then(|m| m.as_array())
            .and_then(|messages| {
                messages.iter().rev().find_map(|msg| {
                    if msg.get("role").and_then(|r| r.as_str()) != Some("user") {
                        return None;
                    }
                    msg.get("content")
                        .and_then(|c| {
                            // Handle both string content and array content (OpenAI format)
                            if let Some(s) = c.as_str() {
                                // Plain string content
                                Some(s.to_string())
                            } else if let Some(arr) = c.as_array() {
                                // Array content format: extract text from text parts
                                let text_parts: Vec<String> = arr
                                    .iter()
                                    .filter_map(|part| {
                                        if part.get("type").and_then(|t| t.as_str()) == Some("text")
                                        {
                                            part.get("text")
                                                .and_then(|t| t.as_str())
                                                .map(std::string::ToString::to_string)
                                        } else {
                                            None
                                        }
                                    })
                                    .collect();
                                if text_parts.is_empty() {
                                    None
                                } else {
                                    Some(text_parts.join(""))
                                }
                            } else {
                                None
                            }
                        })
                        .map(|s| s.trim().to_string())
                })
            })
            .and_then(|content| content.strip_prefix("##").map(|s| s.trim().to_lowercase()));

        let mock_req = crate::mock_provider::resolve_mock_params(
            &model_name,
            json_body
                .get("stream")
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(false),
            &correlation_id,
            &parts.headers,
            state.analysis_tx.clone(),
            dynamic_trigger,
            state.config.mock_responses.responses.clone(),
        );
        return crate::mock_provider::handle_mock_response(mock_req).await;
    }

    // Execute Upstream Request
    let pi_pricing = if pi_interface {
        Some(
            state
                .pricing
                .get(&model_name)
                .await
                .filter(crate::pi_messages::valid_pricing)
                .ok_or_else(|| {
                    (
                        StatusCode::BAD_REQUEST,
                        Json(ProxyError::new(
                            "Pi effective pricing unavailable",
                            "pi_pricing_unavailable",
                        )),
                    )
                        .into_response()
                })?,
        )
    } else {
        None
    };
    let upstream_res = send_with_adjustment_retry(
        &state,
        &provider_name,
        &model_name,
        upstream_req,
        effort_capable,
        adjustments,
    )
    .await
    .map_err(|e| {
        tracing::error!("Upstream request failed: {}", e);
        (
            StatusCode::BAD_GATEWAY,
            Json(ProxyError::new(
                format!("Upstream request failed: {e}"),
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

    // Track upstream rate limit quotas from response headers
    if let Some(quota) = crate::upstream_quota::parse_quota_headers(&headers) {
        let account = validated_key
            .as_ref()
            .and_then(|k| k.oauth_account.as_deref())
            .unwrap_or("default")
            .to_string();
        let key = crate::upstream_quota::QuotaKey {
            provider: provider_name.clone(),
            account,
        };
        tracing::debug!(
            "Upstream quota for {}/{}: req {}/{}, tok {}/{}",
            key.provider,
            key.account,
            quota.requests_remaining.unwrap_or(0),
            quota.requests_limit.unwrap_or(0),
            quota.tokens_remaining.unwrap_or(0),
            quota.tokens_limit.unwrap_or(0),
        );
        let tracker = state.quota_tracker.clone();
        tokio::spawn(async move { tracker.update(key, quota).await });
    }

    let analysis_tx = state.analysis_tx.clone();
    let correlation_id_clone = correlation_id.clone();

    // Native inference APIs have their own error schemas. Preserve status,
    // headers and bytes; never text-decode an audio/video error payload.
    if !status.is_success() && is_native_inference {
        let mut response = Response::new(Body::from_stream(stream));
        *response.status_mut() = status;
        *response.headers_mut() = headers;
        return Ok(response);
    }

    // Normalize non-success upstream errors to OpenAI-compatible error format.
    if !status.is_success() {
        // A non-success upstream (e.g. 401) may send a status but never terminate
        // its error body, which would otherwise hang the request forever (eavs-1sfs).
        // Bound the error-body read so we always return a prompt, well-formed error.
        let body_bytes =
            tokio::time::timeout(upstream_error_body_timeout(), collect_stream_bytes(stream))
                .await
                .unwrap_or_else(|_| {
                    tracing::warn!(
                        status = %status,
                        "Timed out reading upstream error body; returning empty error body"
                    );
                    Ok(Bytes::new())
                })
                .unwrap_or_default();
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

    if api_path == "/v1/eavs/rerank" {
        // Rerank results are bounded JSON, unlike media/streaming responses.
        // Limit buffering rather than letting a malicious upstream exhaust RAM.
        let mut body = Vec::new();
        let mut stream = stream;
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|_| {
                (
                    StatusCode::BAD_GATEWAY,
                    Json(ProxyError::new(
                        "Upstream rerank stream failed",
                        "upstream_error",
                    )),
                )
                    .into_response()
            })?;
            if body.len().saturating_add(chunk.len()) > 2 * 1024 * 1024 {
                return Err((
                    StatusCode::BAD_GATEWAY,
                    Json(ProxyError::new(
                        "Rerank response exceeds 2 MiB",
                        "upstream_error",
                    )),
                )
                    .into_response());
            }
            body.extend_from_slice(&chunk);
        }
        let mut result: Value = serde_json::from_slice(&body).map_err(|_| {
            (
                StatusCode::BAD_GATEWAY,
                Json(ProxyError::new(
                    "Invalid rerank response JSON",
                    "upstream_error",
                )),
            )
                .into_response()
        })?;
        if provider_type == ProviderType::Voyage {
            let data = result.as_object_mut().and_then(|r| r.remove("data"));
            if let Some(data) = data {
                result["results"] = data;
            }
        }
        if !result.get("results").is_some_and(Value::is_array) {
            return Err((
                StatusCode::BAD_GATEWAY,
                Json(ProxyError::new(
                    "Rerank response is missing results",
                    "upstream_error",
                )),
            )
                .into_response());
        }

        // Preserve upstream usage verbatim, including absent/unknown usage.
        // Do not reinterpret search units or total_tokens as output tokens or
        // bill them with chat pricing. Capped keys were rejected before dispatch.
        return Ok(Json(result).into_response());
    }

    // Prepare usage tracking state for virtual keys
    let usage_tracker: Option<UsageTracker> = validated_key.as_ref().map(|vk| UsageTracker {
        key_hash: vk.key_hash.clone(),
        model: model.clone(),
        provider: provider_name.clone(),
        input_tokens: Arc::new(std::sync::atomic::AtomicU32::new(0)),
        output_tokens: Arc::new(std::sync::atomic::AtomicU32::new(0)),
        cached_tokens: Arc::new(std::sync::atomic::AtomicU32::new(0)),
        cache_write_tokens: Arc::new(std::sync::atomic::AtomicU32::new(0)),
    });

    if pi_interface {
        let pricing = pi_pricing.ok_or_else(|| {
            (
                StatusCode::BAD_GATEWAY,
                Json(ProxyError::new(
                    "Pi effective pricing unavailable",
                    "pi_pricing_unavailable",
                )),
            )
                .into_response()
        })?;
        return Ok(crate::pi_messages::stream::response(
            crate::pi_messages::stream::Admission {
                state,
                key: validated_key,
                provider: resolved_provider,
                model: model_name,
                kind: provider_type,
                pricing,
                oauth_tools: is_anthropic_oauth,
            },
            stream,
            &headers,
        ));
    }

    if needs_transform {
        // Transform response from provider format back to OpenAI format
        if request_stream && !fake_streaming {
            let stream_state = Arc::new(Mutex::new(StreamState::default()));
            let transformer = Arc::new(ProviderTransformer::for_provider_with_compat(
                provider_type,
                Some(&resolved_compat),
            ));
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
                                        tracker.cache_write_tokens.store(
                                            usage.cache_creation_input_tokens.unwrap_or(0),
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

            let stream_with_transform: futures::stream::BoxStream<
                'static,
                Result<Bytes, std::io::Error>,
            > = if let Some(tracker) = usage_tracker.clone() {
                Box::pin(with_terminal_usage_record(
                    stream_with_transform,
                    state.clone(),
                    tracker,
                ))
            } else {
                Box::pin(stream_with_transform)
            };

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

            Ok(response)
        } else if request_stream && fake_streaming {
            // Fake streaming: buffer full upstream response, transform to canonical events,
            // then emit OpenAI-compatible SSE chunks.
            let body_bytes = collect_stream_bytes(stream).await.map_err(|e| {
                (
                    StatusCode::BAD_GATEWAY,
                    Json(ProxyError::new(
                        format!("Failed to read upstream response: {e}"),
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
                        format!("Invalid upstream JSON: {e}"),
                        "upstream_error",
                    )),
                )
                    .into_response()
            })?;

            let transformer = ProviderTransformer::for_provider_with_compat(
                provider_type,
                Some(&resolved_compat),
            );
            let events = transformer.parse_response(&json_body).map_err(|e| {
                (
                    StatusCode::BAD_GATEWAY,
                    Json(ProxyError::new(
                        format!("Failed to parse upstream response: {e}"),
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
                    tracker.cache_write_tokens.store(
                        done.usage.cache_creation_input_tokens.unwrap_or(0),
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
                        format!("Failed to read upstream response: {e}"),
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
                        format!("Invalid upstream JSON: {e}"),
                        "upstream_error",
                    )),
                )
                    .into_response()
            })?;

            let transformer = ProviderTransformer::for_provider_with_compat(
                provider_type,
                Some(&resolved_compat),
            );
            let events = transformer.parse_response(&json_body).map_err(|e| {
                (
                    StatusCode::BAD_GATEWAY,
                    Json(ProxyError::new(
                        format!("Failed to parse upstream response: {e}"),
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
                        format!("Failed to serialize response: {e}"),
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
                    if let Some(input) = usage
                        .get("prompt_tokens")
                        .and_then(serde_json::Value::as_u64)
                    {
                        tracker
                            .input_tokens
                            .store(input as u32, std::sync::atomic::Ordering::SeqCst);
                    }
                    if let Some(output) = usage
                        .get("completion_tokens")
                        .and_then(serde_json::Value::as_u64)
                    {
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
        // Native media bytes are forwarded unchanged and never decoded into the
        // text analysis stream (audio/video/image payloads can be sensitive).
        let tracker_clone = usage_tracker.clone();
        let stream_with_logging = stream.map(move |chunk_result| match chunk_result {
            Ok(chunk) => {
                if is_native_inference {
                    // Never publish media, embeddings or typed decisions to text audit logs.
                    return Ok(chunk);
                }
                let text = String::from_utf8_lossy(&chunk).to_string();

                // Try to extract usage from streaming responses in the
                // format this passthrough actually relays
                if let Some(tracker) = &tracker_clone {
                    if is_anthropic_native_request {
                        extract_anthropic_usage(&text, tracker);
                    } else if is_google_native_request {
                        extract_google_usage(&text, tracker);
                    } else {
                        extract_openai_usage(&text, tracker);
                    }
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

        let stream_with_logging: futures::stream::BoxStream<
            'static,
            Result<Bytes, std::io::Error>,
        > = if let Some(tracker) = usage_tracker.clone() {
            Box::pin(with_terminal_usage_record(
                stream_with_logging,
                state.clone(),
                tracker,
            ))
        } else {
            Box::pin(stream_with_logging)
        };

        let mut response = Response::new(Body::from_stream(stream_with_logging));
        *response.status_mut() = status;
        *response.headers_mut() = headers;
        // Add header indicating which provider was used
        response
            .headers_mut()
            .insert("x-eavs-provider", resolved_provider.parse().unwrap());

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
    /// Cache-read (cache-hit) input tokens.
    cached_tokens: Arc<std::sync::atomic::AtomicU32>,
    /// Cache-write (cache-creation) input tokens; only Anthropic bills these
    /// separately (others leave it 0).
    cache_write_tokens: Arc<std::sync::atomic::AtomicU32>,
}

// Mock provider handler is in src/mock_provider.rs

/// Extract usage from native Anthropic Messages responses (SSE or plain JSON).
/// Streaming: `message_start` carries input/cache tokens, `message_delta` the
/// final output tokens. Non-streaming: top-level `usage` object.
fn extract_anthropic_usage(chunk: &str, tracker: &UsageTracker) {
    let store = |usage: &Value| {
        if let Some(input) = usage
            .get("input_tokens")
            .and_then(serde_json::Value::as_u64)
        {
            tracker
                .input_tokens
                .store(input as u32, std::sync::atomic::Ordering::SeqCst);
        }
        if let Some(output) = usage
            .get("output_tokens")
            .and_then(serde_json::Value::as_u64)
        {
            tracker
                .output_tokens
                .store(output as u32, std::sync::atomic::Ordering::SeqCst);
        }
        if let Some(cached) = usage
            .get("cache_read_input_tokens")
            .and_then(serde_json::Value::as_u64)
        {
            tracker
                .cached_tokens
                .store(cached as u32, std::sync::atomic::Ordering::SeqCst);
        }
        if let Some(write) = usage
            .get("cache_creation_input_tokens")
            .and_then(serde_json::Value::as_u64)
        {
            tracker
                .cache_write_tokens
                .store(write as u32, std::sync::atomic::Ordering::SeqCst);
        }
    };

    let mut saw_sse = false;
    for line in chunk.lines() {
        if let Some(data) = line.strip_prefix("data: ") {
            saw_sse = true;
            if let Ok(json) = serde_json::from_str::<Value>(data) {
                if let Some(usage) = json.pointer("/message/usage") {
                    store(usage);
                }
                if let Some(usage) = json.get("usage") {
                    store(usage);
                }
            }
        }
    }

    if !saw_sse {
        if let Ok(json) = serde_json::from_str::<Value>(chunk) {
            if let Some(usage) = json.get("usage") {
                store(usage);
            }
        }
    }
}

/// Extract usage from native Google generateContent responses (SSE or JSON).
fn extract_google_usage(chunk: &str, tracker: &UsageTracker) {
    let store = |meta: &Value| {
        if let Some(input) = meta
            .get("promptTokenCount")
            .and_then(serde_json::Value::as_u64)
        {
            tracker
                .input_tokens
                .store(input as u32, std::sync::atomic::Ordering::SeqCst);
        }
        let output = meta
            .get("candidatesTokenCount")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(0)
            + meta
                .get("thoughtsTokenCount")
                .and_then(serde_json::Value::as_u64)
                .unwrap_or(0);
        if output > 0 {
            tracker
                .output_tokens
                .store(output as u32, std::sync::atomic::Ordering::SeqCst);
        }
        if let Some(cached) = meta
            .get("cachedContentTokenCount")
            .and_then(serde_json::Value::as_u64)
        {
            tracker
                .cached_tokens
                .store(cached as u32, std::sync::atomic::Ordering::SeqCst);
        }
    };

    let mut saw_sse = false;
    for line in chunk.lines() {
        if let Some(data) = line.strip_prefix("data: ") {
            saw_sse = true;
            if let Ok(json) = serde_json::from_str::<Value>(data) {
                if let Some(meta) = json.get("usageMetadata") {
                    store(meta);
                }
            }
        }
    }

    if !saw_sse {
        if let Ok(json) = serde_json::from_str::<Value>(chunk) {
            if let Some(meta) = json.get("usageMetadata") {
                store(meta);
            }
        }
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
                    if let Some(input) = usage
                        .get("prompt_tokens")
                        .and_then(serde_json::Value::as_u64)
                    {
                        tracker
                            .input_tokens
                            .store(input as u32, std::sync::atomic::Ordering::SeqCst);
                    }
                    if let Some(output) = usage
                        .get("completion_tokens")
                        .and_then(serde_json::Value::as_u64)
                    {
                        tracker
                            .output_tokens
                            .store(output as u32, std::sync::atomic::Ordering::SeqCst);
                    }
                    if let Some(cached) = usage
                        .get("prompt_tokens_details")
                        .and_then(|d| d.get("cached_tokens"))
                        .and_then(serde_json::Value::as_u64)
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
    let cache_write = tracker
        .cache_write_tokens
        .load(std::sync::atomic::Ordering::SeqCst);

    // Always record the request to update last_request_at
    // Some providers don't return usage data, but we still want to track request timestamps
    if let Some(validator) = state.get_key_validator() {
        if input == 0 && output == 0 {
            // No usage data, but record the request timestamp
            validator
                .record_request(&tracker.key_hash, &tracker.model, &tracker.provider)
                .await;
            tracing::debug!(
                key_hash = %tracker.key_hash,
                "Recorded request (no usage data) for virtual key"
            );
            return;
        }

        // Calculate cost (distinguishing cache reads from cache writes)
        let cost = if let Some(calc) = state.get_cost_calculator() {
            calc.calculate_detailed_cost(
                &tracker.model,
                &tracker.provider,
                input,
                cached,
                cache_write,
                output,
            )
            .await
        } else {
            0.0
        };

        // Record to validator
        validator
            .record_usage(
                &tracker.key_hash,
                input,
                output,
                cached,
                cache_write,
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
            cache_write_tokens = cache_write,
            cost_usd = cost,
            "Recorded usage for virtual key"
        );
    }
}

/// Wrap a byte stream so that virtual-key usage is debited only once the
/// stream is fully consumed (its terminal), not at response-construction time.
///
/// Streaming usage is parsed into the tracker's atomics while the body is being
/// polled, so recording at construction would read all zeros and never debit
/// budget/TPM. Recording on the terminal ensures the debited tokens reflect the
/// streamed traffic. If the stream is dropped early (client disconnect) no
/// debit occurs, which is acceptable since the traffic was never delivered.
fn with_terminal_usage_record(
    stream: impl futures::Stream<Item = Result<Bytes, std::io::Error>> + Send + 'static,
    state: AppState,
    tracker: UsageTracker,
) -> impl futures::Stream<Item = Result<Bytes, std::io::Error>> + Send + 'static {
    use std::task::Poll;

    let mut stream = Box::pin(stream);
    futures::stream::poll_fn(
        move |cx| match futures::Stream::poll_next(stream.as_mut(), cx) {
            Poll::Ready(Some(item)) => Poll::Ready(Some(item)),
            Poll::Ready(None) => {
                let state = state.clone();
                let tracker = tracker.clone();
                tokio::spawn(async move {
                    record_usage_from_tracker(&state, &tracker).await;
                });
                Poll::Ready(None)
            }
            Poll::Pending => Poll::Pending,
        },
    )
}

/// " Providers available to this key: a, b." for a valid virtual key, built
/// exactly like the key-filtered serving catalog. Empty for anyone else, so
/// errors never disclose configuration to unauthenticated callers.
pub async fn provider_hint(state: &AppState, headers: &http::HeaderMap) -> String {
    let token = headers
        .get(http::header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .or_else(|| headers.get("x-api-key").and_then(|v| v.to_str().ok()));
    let Some(token) = token else {
        return String::new();
    };
    match crate::api::serving_catalog::build(state, token).await {
        Ok(catalog) if catalog.providers.is_empty() => {
            " This key has no usable providers.".to_string()
        }
        Ok(catalog) => format!(
            " Providers available to this key: {}.",
            catalog
                .providers
                .iter()
                .map(|p| p.id.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        ),
        Err(_) => String::new(),
    }
}

/// 400 `unknown_provider`: the requested or guessed provider is not configured.
async fn unknown_provider_response(
    state: &AppState,
    headers: &http::HeaderMap,
    requested: &str,
    guessed_from_model: Option<&str>,
) -> Response {
    let remedy = "Select a provider with the /<provider>/v1/... path or the X-Provider header.";
    let message = match guessed_from_model {
        Some(model) => format!(
            "Model '{model}' does not identify a configured provider (guessed '{requested}'). {remedy}"
        ),
        None if requested == "default" => {
            format!("No provider selected and no default provider is configured. {remedy}")
        }
        None => format!("Provider '{requested}' is not configured. {remedy}"),
    };
    let hint = provider_hint(state, headers).await;
    (
        StatusCode::BAD_REQUEST,
        Json(
            ProxyError::new(format!("{message}{hint}"), "invalid_request_error")
                .with_code("unknown_provider"),
        ),
    )
        .into_response()
}

/// 404 `unknown_endpoint` for a provider/API path that is not an inference
/// endpoint, naming the expected shapes.
pub async fn unknown_endpoint_response(
    state: &AppState,
    headers: &http::HeaderMap,
    path: &str,
) -> Response {
    let hint = provider_hint(state, headers).await;
    (
        StatusCode::NOT_FOUND,
        Json(
            ProxyError::new(
                format!(
                    "'{path}' is not an inference endpoint. Expected /<provider>/v1/chat/completions, \
                     /<provider>/v1/responses, /<provider>/v1/messages or /<provider>/v1/models.{hint}"
                ),
                "invalid_request_error",
            )
            .with_code("unknown_endpoint"),
        ),
    )
        .into_response()
}

/// A request adjustment reported to the client as `header: from->to`.
type Adjustment = (&'static str, String, String);

/// Whether `model` is known to require Anthropic adaptive thinking: declared
/// via model compat `forceAdaptiveThinking = true`, or learned.
pub fn requires_adaptive_thinking(
    state: &AppState,
    provider_config: &crate::config::ProviderConfig,
    provider_name: &str,
    model: &str,
) -> bool {
    provider_config
        .models
        .iter()
        .find(|entry| entry.id == model)
        .and_then(|entry| entry.compat.get("forceAdaptiveThinking"))
        .and_then(Value::as_bool)
        .or_else(|| state.learned_adaptive.get(provider_name, model))
        .unwrap_or(false)
}

/// Apply adjustments already known for this upstream model (accepted
/// reasoning efforts, adaptive thinking) before sending.
fn apply_known_adjustments(
    state: &AppState,
    provider_config: &crate::config::ProviderConfig,
    provider_name: &str,
    model_name: &str,
    mut body: Vec<u8>,
) -> (Vec<u8>, Vec<Adjustment>) {
    let mut adjustments = Vec::new();
    if let Some((rewritten, from, to)) =
        reasoning_efforts_for(state, provider_config, provider_name, model_name)
            .and_then(|supported| crate::reasoning_effort::rewrite_bytes(&body, &supported))
    {
        body = rewritten;
        adjustments.push(("x-eavs-reasoning-effort", from, to));
    }
    if requires_adaptive_thinking(state, provider_config, provider_name, model_name) {
        if let Some((rewritten, from, to)) = crate::reasoning_effort::to_adaptive_bytes(&body) {
            body = rewritten;
            adjustments.push(("x-eavs-thinking", from, to));
        }
    }
    for (header, from, to) in &adjustments {
        tracing::info!(provider = %provider_name, model = %model_name, %header, %from, %to, "Adjusted request for known upstream behavior");
    }
    (body, adjustments)
}

/// Whether a request carries something the gateway can adjust after an
/// upstream rejection.
fn adjustable(body: &[u8]) -> bool {
    serde_json::from_slice::<Value>(body).is_ok_and(|body| {
        crate::reasoning_effort::requested(&body).is_some()
            || crate::reasoning_effort::uses_enabled_thinking(&body)
    })
}

/// Send upstream; if the upstream rejects the request with a 400 the gateway
/// understands (unsupported reasoning effort naming the accepted set, or a
/// model requiring adaptive thinking), learn it and resend once, adjusted.
/// Any other response passes through unchanged. Adjustments are reported as
/// `x-eavs-reasoning-effort` / `x-eavs-thinking: from->to` headers.
async fn send_with_adjustment_retry(
    state: &AppState,
    provider_name: &str,
    model_name: &str,
    request: UpstreamRequest,
    capable: bool,
    mut adjustments: Vec<Adjustment>,
) -> Result<UpstreamResponse, std::io::Error> {
    let resend = (capable && adjustable(&request.body)).then(|| {
        (
            request.method.clone(),
            request.url.clone(),
            request.headers.clone(),
            request.body.clone(),
        )
    });
    let mut response = state.upstream.send(request).await?;
    if let Some((method, url, headers, body)) = resend {
        if let Some((rewritten, (header, from, to))) =
            learn_from_upstream_error(state, provider_name, model_name, &mut response, &body).await
        {
            tracing::info!(
                provider = %provider_name,
                model = %model_name,
                %header,
                %from,
                %to,
                "Upstream rejected request; learned its requirement and retrying once"
            );
            record_adjustment(&mut adjustments, (header, from, to));
            response = state
                .upstream
                .send(UpstreamRequest {
                    method,
                    url,
                    headers,
                    body: Bytes::from(rewritten),
                })
                .await?;
        }
    }
    report_adjustments(&mut response.headers, adjustments);
    Ok(response)
}

/// Add an adjustment, or update the target of one already applied (keeping
/// the client's original value as `from`).
fn record_adjustment(adjustments: &mut Vec<Adjustment>, (header, from, to): Adjustment) {
    match adjustments
        .iter_mut()
        .find(|(known, _, _)| *known == header)
    {
        Some(existing) => existing.2 = to,
        None => adjustments.push((header, from, to)),
    }
}

fn report_adjustments(headers: &mut HeaderMap, adjustments: Vec<Adjustment>) {
    for (header, from, to) in adjustments {
        if let Ok(value) = http::HeaderValue::from_str(&format!("{from}->{to}")) {
            headers.insert(header, value);
        }
    }
}

/// For a 400 the gateway understands: record what it teaches and return the
/// adjusted body. Otherwise restore the (already read) error body so the
/// client receives it unchanged, and return `None`.
async fn learn_from_upstream_error(
    state: &AppState,
    provider_name: &str,
    model_name: &str,
    response: &mut UpstreamResponse,
    body: &[u8],
) -> Option<(Vec<u8>, Adjustment)> {
    if response.status != StatusCode::BAD_REQUEST {
        return None;
    }
    let error_stream = std::mem::replace(&mut response.body, futures::stream::empty().boxed());
    let error_bytes = tokio::time::timeout(
        upstream_error_body_timeout(),
        collect_stream_bytes(error_stream),
    )
    .await
    .ok()
    .and_then(Result::ok)
    .unwrap_or_default();
    let text = String::from_utf8_lossy(&error_bytes);
    let retry = if crate::reasoning_effort::requires_adaptive(&text) {
        state
            .learned_adaptive
            .learn(provider_name, model_name, true);
        crate::reasoning_effort::to_adaptive_bytes(body)
            .map(|(rewritten, from, to)| (rewritten, ("x-eavs-thinking", from, to)))
    } else {
        crate::reasoning_effort::parse_supported(&text).and_then(|supported| {
            state
                .learned_efforts
                .learn(provider_name, model_name, supported.clone());
            crate::reasoning_effort::rewrite_bytes(body, &supported)
                .map(|(rewritten, from, to)| (rewritten, ("x-eavs-reasoning-effort", from, to)))
        })
    };
    if retry.is_none() {
        response.body = futures::stream::once(async move { Ok(error_bytes) }).boxed();
    }
    retry
}

/// Reasoning-effort values the upstream accepts for `model`: the model's
/// config declaration wins over values learned from upstream errors.
pub fn reasoning_efforts_for(
    state: &AppState,
    provider_config: &crate::config::ProviderConfig,
    provider_name: &str,
    model: &str,
) -> Option<Vec<String>> {
    provider_config
        .models
        .iter()
        .find(|entry| entry.id == model && !entry.reasoning_efforts.is_empty())
        .map(|entry| entry.reasoning_efforts.clone())
        .or_else(|| state.learned_efforts.get(provider_name, model))
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
            ContentBlock::Text(t) if !t.text.is_empty() => {
                out_events.push(StreamEvent::TextDelta {
                    content_index: 0,
                    delta: t.text.clone(),
                });
            }
            ContentBlock::Thinking(t) if !t.thinking.is_empty() => {
                out_events.push(StreamEvent::ThinkingDelta {
                    content_index: 0,
                    delta: t.thinking.clone(),
                });
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

/// Sanitize empty text content parts in the `messages` array.
///
/// Some providers (Azure Foundry, Kimi) reject `{"text": "", "type": "text"}`
/// with a 422 error because they consider an empty string as "text not provided".
/// This function replaces empty text with a single space to keep message structure
/// intact while satisfying strict validation.
///
/// Handles both array-of-parts content (`"content": [{"type":"text","text":""}]`)
/// and string content (`"content": ""`).
fn sanitize_empty_text_parts(json_body: &mut Value) {
    let Some(messages) = json_body.get_mut("messages").and_then(|m| m.as_array_mut()) else {
        return;
    };

    for msg in messages.iter_mut() {
        let Some(content) = msg.get_mut("content") else {
            continue;
        };

        match content {
            // Array content: [{"type": "text", "text": ""}, ...]
            Value::Array(parts) => {
                for part in parts.iter_mut() {
                    if part.get("type").and_then(|t| t.as_str()) == Some("text") {
                        if let Some(text) = part.get("text").and_then(|t| t.as_str()) {
                            if text.is_empty() {
                                part["text"] = Value::String(" ".to_string());
                            }
                        }
                    }
                }
            }
            // String content: ""
            Value::String(s) if s.is_empty() => {
                *content = Value::String(" ".to_string());
            }
            _ => {}
        }
    }
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

/// Voice-specific `ElevenLabs` WebSocket route.
pub async fn provider_voice_ws_proxy_handler(
    State(state): State<AppState>,
    Path((provider, _voice_id)): Path<(String, String)>,
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
    let state = state.pin_routes();
    let conversation_id = headers
        .get("X-Conversation-ID")
        .and_then(|h| h.to_str().ok())
        .unwrap_or("default")
        .to_string();

    // Provider selection: path > header > default
    let header_provider = headers
        .get("X-Provider")
        .and_then(|h| h.to_str().ok())
        .map(std::string::ToString::to_string);

    let request_path = path_provider
        .as_ref()
        .and_then(|p| uri.path().strip_prefix(&format!("/{p}")))
        .unwrap_or(uri.path());
    let provider_name = path_provider
        .or(header_provider)
        .unwrap_or_else(|| "default".to_string());
    let elevenlabs_ws = request_path == "/v1/speech-to-text/realtime"
        || request_path.starts_with("/v1/text-to-speech/")
            && matches!(
                request_path.rsplit('/').next(),
                Some("stream-input" | "multi-stream-input")
            );

    // Resolve once before authorization; scope and accounting use the effective
    // provider identity, not the caller's casing/alias label.
    let Some(provider_lookup) = state.resolve_provider(&provider_name) else {
        return (
            StatusCode::BAD_REQUEST,
            Json(ProxyError::new(
                "Unknown realtime provider",
                "invalid_provider",
            )),
        )
            .into_response();
    };
    let provider_name = provider_lookup.resolved_name.clone();

    // Determine the actual model the client requested (Realtime carries it in
    // the connection query, e.g. ?model=gpt-4o-realtime). Validating this real
    // model is what makes virtual-key model scope apply to Realtime.
    let ws_model = if elevenlabs_ws {
        uri.query()
            .and_then(|q| {
                url::form_urlencoded::parse(q.as_bytes())
                    .find(|(name, _)| name == "model_id")
                    .map(|(_, value)| value.into_owned())
            })
            .unwrap_or_else(|| {
                if request_path == "/v1/speech-to-text/realtime" {
                    "scribe_v2_realtime".to_string()
                } else {
                    "eleven_multilingual_v2".to_string()
                }
            })
    } else {
        extract_realtime_model(&uri)
    };

    // Validate virtual API key if present (or required)
    let require_key = state.config.keys.enabled && state.config.keys.require_key;
    let auth_header = headers
        .get("authorization")
        .and_then(|h| h.to_str().ok())
        .and_then(|h| h.strip_prefix("Bearer "))
        .map(std::string::ToString::to_string);

    let mut validated_key: Option<ValidatedKey> = None;

    if let Some(ref key) = auth_header {
        if is_virtual_key(key) {
            if let Some(validator) = state.get_key_validator() {
                // Use a small token estimate for WebSocket rate limiting
                let estimated_tokens = 100; // Conservative estimate per WS connection
                match validator
                    .validate(key, &ws_model, &provider_name, Some(estimated_tokens))
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
                        "A valid virtual API key is required. Keys must start with 'eavs-'",
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

    // Namespace conversation/injection state by the authenticated key so a
    // different key using the same client-controlled X-Conversation-ID cannot
    // steal, consume, or receive another tenant's injections.
    let ns_conversation = conversation_namespace(
        &conversation_id,
        validated_key.as_ref().map(|k| k.key_hash.as_str()),
    );

    // Usage accounting for the Realtime session. Token counts are filled from
    // upstream response.done events as the session runs; the request is debited
    // once the session ends so budget/TPM limits apply to Realtime traffic.
    let ws_tracker: Option<UsageTracker> =
        validated_key
            .as_ref()
            .filter(|_| !elevenlabs_ws)
            .map(|vk| UsageTracker {
                key_hash: vk.key_hash.clone(),
                model: ws_model.clone(),
                provider: provider_name.clone(),
                input_tokens: Arc::new(std::sync::atomic::AtomicU32::new(0)),
                output_tokens: Arc::new(std::sync::atomic::AtomicU32::new(0)),
                cached_tokens: Arc::new(std::sync::atomic::AtomicU32::new(0)),
                cache_write_tokens: Arc::new(std::sync::atomic::AtomicU32::new(0)),
            });
    let ws_state = state.clone();

    // Register/update conversation in store if capture_all is enabled
    if state.config.state.capture_all {
        let _ = state.conversations.get_or_create(&ns_conversation);
        state
            .conversations
            .update_metadata(&ns_conversation, |meta| {
                meta.provider = Some(provider_name.clone());
                meta.model = Some(ws_model.clone());
                meta.request_count += 1;
            });
    }

    let provider_config = provider_lookup.config.clone();
    let provider_type = provider_config.provider_type();
    if elevenlabs_ws
        && (request_path.contains('%')
            || request_path.contains('\\')
            || request_path
                .split('/')
                .any(|segment| segment == "." || segment == ".."))
    {
        return (
            StatusCode::BAD_REQUEST,
            Json(ProxyError::new(
                "Encoded or traversal path segments are not allowed",
                "invalid_path",
            )),
        )
            .into_response();
    }
    if elevenlabs_ws
        && state.config.policy.enabled
        && state
            .config
            .policy
            .rules
            .iter()
            .any(|rule| !matches!(rule, crate::policy::PolicyRule::Deny { .. }))
    {
        return (
            StatusCode::BAD_REQUEST,
            Json(ProxyError::new(
                "Body-mutating policies cannot be enforced for native realtime speech",
                "unsupported_policy",
            )),
        )
            .into_response();
    }
    if elevenlabs_ws
        && state
            .config
            .policy
            .apply(&provider_name, uri.path(), &mut json!({"model": ws_model}))
            .is_err()
    {
        return (
            StatusCode::FORBIDDEN,
            Json(ProxyError::new(
                "Realtime speech request denied by policy",
                "policy_violation",
            )),
        )
            .into_response();
    }
    if matches!(provider_type, ProviderType::Fal | ProviderType::Replicate)
        || (elevenlabs_ws && provider_type != ProviderType::ElevenLabs)
        || (!elevenlabs_ws && provider_type == ProviderType::ElevenLabs)
    {
        return (
            StatusCode::BAD_REQUEST,
            Json(ProxyError::new(
                "WebSocket API shape is not supported by this provider",
                "unsupported_api_shape",
            )),
        )
            .into_response();
    }
    // Never relay user-supplied provider tokens in the query string.
    if elevenlabs_ws
        && uri.query().is_some_and(|q| {
            url::form_urlencoded::parse(q.as_bytes()).any(|(name, _)| {
                matches!(
                    name.as_ref(),
                    "token" | "single_use_token" | "authorization"
                )
            })
        })
    {
        return (
            StatusCode::BAD_REQUEST,
            Json(ProxyError::new(
                "Use an Eavs Authorization header, not a provider token query parameter",
                "invalid_request",
            )),
        )
            .into_response();
    }
    if elevenlabs_ws
        && validated_key.as_ref().is_some_and(|key| {
            key.permissions.max_budget_usd.is_some() || key.permissions.tpm_limit.is_some()
        })
    {
        return (
            StatusCode::BAD_REQUEST,
            Json(ProxyError::new(
                "Native realtime speech cannot enforce this key's budget or token limit yet",
                "unsupported_metering",
            )),
        )
            .into_response();
    }
    let mut api_key = provider_config.resolved_api_key();

    if let Some(ref validated) = validated_key {
        if let Some(oauth_user) = validated.oauth_user.as_deref() {
            api_key = match resolve_oauth_access_token_with_account(
                &state,
                &provider_name,
                oauth_user,
                validated.oauth_account.as_deref(),
            )
            .await
            {
                Ok(token) => token,
                Err(msg) => {
                    let msg = msg.to_string();
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
        request_path,
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

    // Network ACL check for WebSocket
    if let Err(reason) =
        crate::network_acl::check_url_allowed(&state.config.network, upstream_url.as_str())
    {
        tracing::warn!("Network ACL blocked WS to {}: {}", upstream_url, reason);
        return (
            StatusCode::FORBIDDEN,
            Json(ProxyError::new(reason, "network_policy").with_code("network_blocked")),
        )
            .into_response();
    }

    let delegated_fetch_policy = state
        .config
        .delegated_fetch
        .policy_for_key_metadata(validated_key.as_ref().map(|key| &key.metadata));
    let delegated_fetch_analysis_tx = state.analysis_tx.clone();

    ws.on_upgrade(move |mut client_socket| async move {
        let (session_token, mut injection_rx) = state.ws_sessions.register(&ns_conversation);
        let _session_guard = WsSessionGuard {
            conversation_id: ns_conversation.clone(),
            token: session_token,
            sessions: state.ws_sessions.clone(),
        };

        let mut request = if let Ok(r) = upstream_url.into_client_request() {
            r
        } else {
            let _ = client_socket.send(AxumWsMessage::Close(None)).await;
            return;
        };

        apply_ws_auth_headers(&mut request, provider_type, &api_key);
        apply_ws_extra_headers(&mut request, provider_type);
        apply_opencode_session_headers(&mut request, provider_type, &conversation_id);

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

        let (upstream_socket, _) = if let Ok(pair) = tokio_tungstenite::connect_async(request).await
        {
            pair
        } else {
            let _ = client_socket.send(AxumWsMessage::Close(None)).await;
            return;
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
                let Some(up_msg) = (if elevenlabs_ws {
                    axum_to_tungstenite(msg)
                } else {
                    sanitize_ws_client_message(
                        msg,
                        &delegated_fetch_policy,
                        &delegated_fetch_analysis_tx,
                    )
                }) else {
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
        let tracker_for_client = ws_tracker.clone();
        let upstream_to_client = tokio::spawn(async move {
            while let Some(Ok(msg)) = upstream_receiver.next().await {
                // Fill session usage from Realtime response.done/completed events.
                if let (Some(tracker), TungsteniteMessage::Text(text)) = (&tracker_for_client, &msg)
                {
                    if let Ok(json) = serde_json::from_str::<Value>(text.as_str()) {
                        let event_type = json
                            .get("type")
                            .and_then(|v| v.as_str())
                            .unwrap_or_default();
                        if event_type == "response.done" || event_type == "response.completed" {
                            let usage = json
                                .pointer("/response/usage")
                                .or_else(|| json.get("usage"));
                            if let Some(usage) = usage {
                                if let Some(input) = usage
                                    .get("input_tokens")
                                    .and_then(serde_json::Value::as_u64)
                                {
                                    tracker
                                        .input_tokens
                                        .store(input as u32, std::sync::atomic::Ordering::SeqCst);
                                }
                                if let Some(output) = usage
                                    .get("output_tokens")
                                    .and_then(serde_json::Value::as_u64)
                                {
                                    tracker
                                        .output_tokens
                                        .store(output as u32, std::sync::atomic::Ordering::SeqCst);
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

        // Injection -> upstream (OpenAI Realtime semantics).
        let inject_to_upstream = tokio::spawn(async move {
            if elevenlabs_ws {
                return;
            }
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

        // Record Realtime session usage now that the session has ended, so
        // budget/token/rate limits are debited (not at construction when the
        // atomics are still zero).
        if let Some(tracker) = ws_tracker {
            record_usage_from_tracker(&ws_state, &tracker).await;
        }
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
/// apply policy rules (e.g., `set_field` for `store: true`), then relays bidirectionally.
async fn codex_ws_handler_inner(
    state: AppState,
    ws: WebSocketUpgrade,
    headers: HeaderMap,
    uri: http::Uri,
    path_provider: Option<String>,
) -> Response {
    let state = state.pin_routes();
    // --- Auth & provider resolution (shared with realtime WS handler) ---

    let header_provider = headers
        .get("X-Provider")
        .and_then(|h| h.to_str().ok())
        .map(std::string::ToString::to_string);

    let provider_name = path_provider
        .or(header_provider)
        .unwrap_or_else(|| "openai-codex".to_string());
    let Some(provider_lookup) = state.resolve_provider(&provider_name) else {
        return (
            StatusCode::BAD_REQUEST,
            Json(ProxyError::new(
                "Unknown Codex provider",
                "invalid_provider",
            )),
        )
            .into_response();
    };
    let provider_name = provider_lookup.resolved_name.clone();

    let require_key = state.config.keys.enabled && state.config.keys.require_key;
    let auth_header = headers
        .get("authorization")
        .and_then(|h| h.to_str().ok())
        .and_then(|h| h.strip_prefix("Bearer "))
        .map(std::string::ToString::to_string);

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
                ProxyError::new("Authorization header required", "authentication_error")
                    .with_code("missing_api_key"),
            ),
        )
            .into_response();
    }

    let provider_config = provider_lookup.config.clone();
    let provider_type = provider_config.provider_type();
    if matches!(
        provider_type,
        ProviderType::Fal | ProviderType::Replicate | ProviderType::ElevenLabs
    ) {
        return (
            StatusCode::BAD_REQUEST,
            Json(ProxyError::new(
                "Codex WebSocket is not supported by this provider",
                "unsupported_api_shape",
            )),
        )
            .into_response();
    }
    let mut api_key = provider_config.resolved_api_key();

    // Resolve OAuth token if key is bound to an oauth_user
    if let Some(ref validated) = validated_key {
        if let Some(oauth_user) = validated.oauth_user.as_deref() {
            api_key = match resolve_oauth_access_token_with_account(
                &state,
                &provider_name,
                oauth_user,
                validated.oauth_account.as_deref(),
            )
            .await
            {
                Ok(token) => token,
                Err(msg) => {
                    let msg = msg.to_string();
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

    // Network ACL check for Codex WebSocket
    if let Err(reason) =
        crate::network_acl::check_url_allowed(&state.config.network, upstream_url.as_str())
    {
        tracing::warn!(
            "Network ACL blocked Codex WS to {}: {}",
            upstream_url,
            reason
        );
        return (
            StatusCode::FORBIDDEN,
            Json(ProxyError::new(reason, "network_policy").with_code("network_blocked")),
        )
            .into_response();
    }

    // Clone what we need for the async upgrade closure
    let policy = state.config.policy.clone();
    let delegated_fetch_policy = state
        .config
        .delegated_fetch
        .policy_for_key_metadata(validated_key.as_ref().map(|key| &key.metadata));
    let delegated_fetch_analysis_tx = state.analysis_tx.clone();
    let provider_name_for_policy = provider_name.clone();
    let provider_name_for_usage = provider_name.clone();
    let provider_headers = provider_config.headers.clone();
    let frame_permissions = validated_key.as_ref().map(|key| key.permissions.clone());
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
                let processed = sanitize_codex_ws_message(
                    msg,
                    &policy,
                    &provider_name_for_policy,
                    frame_permissions.as_ref(),
                    &delegated_fetch_policy,
                    &delegated_fetch_analysis_tx,
                );

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
                            if let (Some(ref kh), Some(ref validator)) = (&key_hash, &key_validator)
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
                                        .and_then(serde_json::Value::as_u64)
                                        .unwrap_or(0)
                                        as u32;
                                    let output_tokens = usage
                                        .get("output_tokens")
                                        .and_then(serde_json::Value::as_u64)
                                        .unwrap_or(0)
                                        as u32;
                                    let cached_tokens = usage
                                        .get("input_tokens_details")
                                        .and_then(|d| d.get("cached_tokens"))
                                        .and_then(serde_json::Value::as_u64)
                                        .unwrap_or(0)
                                        as u32;

                                    let cost_usd = if let Some(ref cc) = cost_calc {
                                        cc.calculate_detailed_cost(
                                            model,
                                            &provider_name_for_usage,
                                            input_tokens,
                                            cached_tokens,
                                            0,
                                            output_tokens,
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
                                            0,
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

fn sanitize_ws_client_message(
    msg: AxumWsMessage,
    policy: &crate::delegated_fetch::DelegatedFetchPolicy,
    analysis_tx: &tokio::sync::broadcast::Sender<AnalysisEvent>,
) -> Option<TungsteniteMessage> {
    match msg {
        AxumWsMessage::Text(text) => {
            let body = parse_and_sanitize_ws_json(text.as_bytes(), policy, analysis_tx)?;
            Some(TungsteniteMessage::Text(body.to_string()))
        }
        AxumWsMessage::Binary(bytes) => {
            let body = parse_and_sanitize_ws_json(&bytes, policy, analysis_tx)?;
            Some(TungsteniteMessage::Binary(serde_json::to_vec(&body).ok()?))
        }
        other => axum_to_tungstenite(other),
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
        Err(format!("Unsupported upstream base_url scheme: {base_url}"))
    }
}

/// Trait for abstracting over different header map types (`HeaderMap` vs `http::Request`<()>).
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

/// Check if this is an `OpenAI` Codex OAuth token (JWT format)
/// `OpenAI` OAuth tokens are JWTs that start with "eyJ" (base64 encoded JSON header)
fn is_openai_codex_oauth_token(api_key: &str) -> bool {
    api_key.starts_with("eyJ") && api_key.contains('.')
}

/// Extract account ID from `OpenAI` OAuth JWT token.
/// The account ID is stored in the JWT claims under `https://api.openai.com/auth`
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
        2 => format!("{payload}=="),
        3 => format!("{payload}="),
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

    // Look for the account ID in the standard OpenAI claim path. The Codex
    // backend keys on chatgpt_account_id (sent as the chatgpt-account-id
    // header); org_id is the platform org and only a last resort.
    json.get("https://api.openai.com/auth")
        .and_then(|auth| {
            auth.get("chatgpt_account_id")
                .or_else(|| auth.get("account_id"))
                .or_else(|| auth.get("org_id"))
        })
        .and_then(|v| v.as_str())
        .map(String::from)
        .or_else(|| {
            // Fallback: try direct claims
            json.get("chatgpt_account_id")
                .or_else(|| json.get("account_id"))
                .or_else(|| json.get("org_id"))
                .and_then(|v| v.as_str())
                .map(String::from)
        })
}

/// Anthropic OAuth tokens are scoped to "Claude Code" - requests must identify as Claude Code.
/// This injects the required system prompt prefix for the token to work.
/// Bounds how long the proxy will wait to read an upstream error body before
/// returning a prompt error (eavs-1sfs). Prevents an upstream that sends a
/// non-success status (e.g. 401) but never terminates its body from hanging
/// the request indefinitely. Defaults to 30s; `EAVS_UPSTREAM_ERROR_BODY_TIMEOUT_MS`
/// overrides it (used by tests to keep the hang-case fast).
fn upstream_error_body_timeout() -> std::time::Duration {
    std::env::var("EAVS_UPSTREAM_ERROR_BODY_TIMEOUT_MS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .map_or(
            std::time::Duration::from_secs(30),
            std::time::Duration::from_millis,
        )
}

const CLAUDE_CODE_IDENTITY: &str = "You are Claude Code, Anthropic's official CLI for Claude.";

fn inject_claude_code_identity_into_context(context: &mut crate::types::Context) {
    // Prepend Claude Code identity to the system prompt
    let new_system = match &context.system_prompt {
        Some(existing) => format!("{CLAUDE_CODE_IDENTITY}\n\n{existing}"),
        None => CLAUDE_CODE_IDENTITY.to_string(),
    };
    context.system_prompt = Some(new_system);
}

/// Inject the Claude Code identity into a native Anthropic Messages body.
/// The `system` field may be a plain string or an array of content blocks.
fn inject_claude_code_identity_into_anthropic_body(body: &mut Value) {
    match body.get_mut("system") {
        Some(Value::String(existing)) => {
            *existing = format!("{CLAUDE_CODE_IDENTITY}\n\n{existing}");
        }
        Some(Value::Array(blocks)) => {
            blocks.insert(
                0,
                serde_json::json!({"type": "text", "text": CLAUDE_CODE_IDENTITY}),
            );
        }
        _ => {
            if let Some(obj) = body.as_object_mut() {
                obj.insert(
                    "system".to_string(),
                    Value::String(CLAUDE_CODE_IDENTITY.to_string()),
                );
            }
        }
    }
}

/// Extract the model id from a native Google generateContent path,
/// e.g. `/v1/models/gemini-2.5-pro:streamGenerateContent`.
pub(crate) fn google_model_from_path(api_path: &str) -> Option<String> {
    let after = api_path.split("/models/").nth(1)?;
    let model = after.split(':').next()?;
    if model.is_empty() {
        None
    } else {
        Some(model.to_string())
    }
}

fn prefix_anthropic_oauth_tools(body: &mut Value) {
    const TOOL_PREFIX: &str = "mcp_";

    // Prefix tool names in tools[] definitions
    if let Some(tools) = body.get_mut("tools").and_then(|v| v.as_array_mut()) {
        for tool in tools {
            if let Some(name) = tool.get_mut("name").and_then(|v| v.as_str()) {
                if !name.starts_with(TOOL_PREFIX) {
                    *tool.get_mut("name").unwrap() = Value::String(format!("{TOOL_PREFIX}{name}"));
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
                        .is_some_and(|t| t == "tool_use");
                    if is_tool_use {
                        if let Some(name) = block.get_mut("name").and_then(|v| v.as_str()) {
                            if !name.starts_with(TOOL_PREFIX) {
                                *block.get_mut("name").unwrap() =
                                    Value::String(format!("{TOOL_PREFIX}{name}"));
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
                        *name_value = Value::String(format!("{TOOL_PREFIX}{name}"));
                    }
                }
            }
        }
    }
}

/// Apply Anthropic OAuth-specific body transformations.
///
/// Sanitizes system prompt text to replace "`OpenCode`" with "Claude Code" and
/// "opencode" (case-insensitive) with "Claude", matching the opencode-anthropic-auth
/// plugin behavior. The Anthropic server blocks requests containing "`OpenCode`" in
/// system prompts when using OAuth tokens.
fn apply_anthropic_oauth_body_transforms(body: &mut Value) {
    // Sanitize system prompt - server blocks 'OpenCode' string
    if let Some(system) = body.get_mut("system") {
        if let Some(arr) = system.as_array_mut() {
            for item in arr {
                if item
                    .get("type")
                    .and_then(|t| t.as_str())
                    .is_some_and(|t| t == "text")
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

/// Sanitize system prompt for Anthropic OAuth: replace "`OpenCode`" -> "Claude Code"
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
/// Unified implementation for both HTTP `HeaderMap` and WebSocket Request headers.
fn apply_auth_headers<H: HeadersExt>(headers: &mut H, provider_type: ProviderType, api_key: &str) {
    if api_key.is_empty() {
        return;
    }

    // Special handling for Anthropic OAuth tokens - they use Bearer auth + specific headers.
    // Matches pi's Claude Code identification: Authorization, anthropic-beta,
    // user-agent, and x-app. The underlying Anthropic SDK (not us) handles
    // x-stainless-* headers.
    if provider_type == ProviderType::Anthropic && is_anthropic_oauth_token(api_key) {
        if let Ok(value) = http::HeaderValue::from_str(&format!("Bearer {api_key}")) {
            headers.insert_header(http::header::AUTHORIZATION, value);
        }
        // Required OAuth beta headers
        headers.insert_header(
            http::header::HeaderName::from_static("anthropic-beta"),
            http::HeaderValue::from_static("oauth-2025-04-20, interleaved-thinking-2025-05-14"),
        );
        headers.insert_header(
            http::header::HeaderName::from_static("user-agent"),
            http::HeaderValue::from_static("claude-cli/2.1.75 (external, cli)"),
        );
        headers.insert_header(
            http::header::HeaderName::from_static("x-app"),
            http::HeaderValue::from_static("cli"),
        );
        return;
    }

    // Special handling for GitHub Copilot tokens - they need specific headers
    if provider_type == ProviderType::GithubCopilot {
        if let Ok(value) = http::HeaderValue::from_str(&format!("Bearer {api_key}")) {
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
        if let Ok(value) = http::HeaderValue::from_str(&format!("Bearer {api_key}")) {
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
            if let Ok(value) = http::HeaderValue::from_str(&format!("Bearer {api_key}")) {
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
/// Unified implementation for both HTTP `HeaderMap` and WebSocket Request headers.
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

pub fn apply_http_auth_headers(
    headers: &mut HeaderMap,
    provider_type: ProviderType,
    api_key: &str,
) {
    apply_auth_headers(headers, provider_type, api_key);
}

pub fn apply_http_extra_headers(headers: &mut HeaderMap, provider_type: ProviderType) {
    apply_extra_headers(headers, provider_type);
}

/// `OpenCode` providers (opencode, opencode-go) route each request to a session via
/// the `x-opencode-session` header, derived from the conversation id.
fn apply_opencode_session_headers<H: HeadersExt>(
    headers: &mut H,
    provider_type: ProviderType,
    session_id: &str,
) {
    if matches!(
        provider_type,
        ProviderType::OpenCode | ProviderType::OpenCodeGo
    ) && !session_id.is_empty()
    {
        if let Ok(value) = http::HeaderValue::from_str(session_id) {
            headers.insert_header(
                http::header::HeaderName::from_static("x-opencode-session"),
                value,
            );
        }
    }
}

async fn resolve_oauth_access_token_with_account(
    state: &AppState,
    provider_name: &str,
    oauth_user: &str,
    oauth_account: Option<&str>,
) -> Result<String, OAuthResolveError> {
    resolve_oauth_access_token_using_client(
        state,
        provider_name,
        oauth_user,
        oauth_account,
        &reqwest::Client::new(),
    )
    .await
}

async fn resolve_oauth_access_token_using_client(
    state: &AppState,
    provider_name: &str,
    oauth_user: &str,
    oauth_account: Option<&str>,
    client: &reqwest::Client,
) -> Result<String, OAuthResolveError> {
    // Try to resolve from provider type first (more reliable when config key
    // differs from provider type, e.g., [providers.my-claude] with type = "anthropic").
    // Falls back to matching on the config section name for backward compatibility.
    let provider = state
        .resolve_provider(provider_name)
        .and_then(|lookup| OAuthProviderKind::from_str(&lookup.config.type_))
        .or_else(|| OAuthProviderKind::from_str(provider_name))
        .ok_or_else(|| {
            OAuthResolveError::UnsupportedProvider(format!(
                "OAuth provider not supported for '{provider_name}'. Supported: anthropic, openai-codex, github-copilot"
            ))
        })?;

    let store = state
        .get_oauth_store()
        .ok_or(OAuthResolveError::StoreNotInitialized)?;

    let account_label = oauth_account.unwrap_or("default");
    let mut credentials = store
        .get_credentials_for_account(oauth_user, provider, account_label)
        .await
        .map_err(|e| OAuthResolveError::Store(e.to_string()))?
        .ok_or_else(|| OAuthResolveError::CredentialsNotFound {
            account: if account_label == "default" {
                None
            } else {
                Some(account_label.to_string())
            },
        })?;

    if credentials.is_expired(60) {
        // Ownership guard: a borrowed credential belongs to an external owner
        // CLI. Eavs must NOT refresh it (rotating refresh tokens logs the owner
        // out) nor write back to the owner's file. Return an actionable typed
        // error naming the owner instead.
        if let crate::oauth::CredentialProvenance::Borrowed { owner } = &credentials.provenance {
            return Err(OAuthResolveError::ExpiredBorrowed {
                owner: owner.clone(),
                provider,
            });
        }

        if credentials.refresh_token.is_empty() {
            return Err(OAuthResolveError::ExpiredNoRefresh);
        }

        let redirect_uri = std::env::var("EAVS_OAUTH_REDIRECT_URI")
            .ok()
            .filter(|v| !v.trim().is_empty())
            .unwrap_or_else(oauth_anthropic::default_redirect_uri);

        credentials = match provider {
            OAuthProviderKind::Anthropic => {
                let config = oauth_anthropic::config_from_env(redirect_uri)
                    .map_err(|e| OAuthResolveError::RefreshFailed(e))?;
                oauth_anthropic::refresh_token(
                    client,
                    &config,
                    oauth_user,
                    &credentials.refresh_token,
                )
                .await
                .map_err(|e| OAuthResolveError::RefreshFailed(e))?
            }
            OAuthProviderKind::OpenAICodex => {
                let config = oauth_openai::config_from_env(redirect_uri)
                    .map_err(|e| OAuthResolveError::RefreshFailed(e))?;
                oauth_openai::refresh_token(client, &config, oauth_user, &credentials.refresh_token)
                    .await
                    .map_err(|e| OAuthResolveError::RefreshFailed(e))?
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
                exchange_copilot_token(client, oauth_user, github_token)
                    .await
                    .map_err(|e| OAuthResolveError::RefreshFailed(e))?
            }
        };

        // Preserve account_label from original credentials through refresh
        credentials.account_label = account_label.to_string();

        store
            .upsert_credentials(&credentials)
            .await
            .map_err(|e| OAuthResolveError::RefreshFailed(e.to_string()))?;
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
        .header("Authorization", format!("Bearer {github_access_token}"))
        .header("User-Agent", "GitHubCopilotChat/0.35.0")
        .header("Editor-Version", "vscode/1.107.0")
        .header("Editor-Plugin-Version", "copilot-chat/0.35.0")
        .header("Copilot-Integration-Id", "vscode-chat")
        .send()
        .await
        .map_err(|e| format!("Copilot token exchange request failed: {e}"))?;

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp
            .text()
            .await
            .unwrap_or_else(|_| "<unreadable>".to_string());
        return Err(format!("Copilot token exchange failed ({status}): {body}"));
    }

    let token_resp: CopilotTokenResponse = resp
        .json()
        .await
        .map_err(|e| format!("Failed to parse Copilot token response: {e}"))?;

    // Parse proxy-ep from token to determine the actual API base URL.
    // Token format: tid=...;exp=...;proxy-ep=proxy.individual.githubcopilot.com;...
    let base_url = if let Some(proxy_ep) = token_resp
        .token
        .split(';')
        .find_map(|part| part.strip_prefix("proxy-ep="))
    {
        let api_host = proxy_ep.replace("proxy.", "api.");
        Some(format!("https://{api_host}"))
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
        provenance: crate::oauth::CredentialProvenance::Owned,
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
                .map_err(|e| format!("Failed to read AWS web identity token file: {e}"))?;
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
                .is_some_and(|m| m.eq_ignore_ascii_case("application/pdf"))
            {
                out.insert("pdfs-2024-09-25".to_string());
            }
            if map
                .get("type")
                .and_then(|t| t.as_str())
                .is_some_and(|t| t.eq_ignore_ascii_case("document"))
            {
                out.insert("pdfs-2024-09-25".to_string());
            }

            // Computer use: look for common markers (tool name / type).
            if map.get("type").and_then(|t| t.as_str()).is_some_and(|t| {
                t.eq_ignore_ascii_case("computer_use") || t.eq_ignore_ascii_case("computer")
            }) {
                out.insert("computer-use-2024-10-22".to_string());
            }
            if map
                .get("name")
                .and_then(|n| n.as_str())
                .is_some_and(|n| n.eq_ignore_ascii_case("computer"))
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
            v.pointer("/detail").and_then(|v| v.as_str()),
            v.pointer("/detail/0/msg").and_then(|v| v.as_str()),
        ];
        message = candidates
            .into_iter()
            .flatten()
            .next()
            .map(std::string::ToString::to_string);

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
            .map(std::string::ToString::to_string);
    }

    if message.as_ref().is_none_or(|m| m.trim().is_empty()) && !body_text.trim().is_empty() {
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

const fn openai_error_type_for_status(status: StatusCode) -> &'static str {
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
        return Err(format!("blocked internal URL: {host}"));
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
        return Err(format!("blocked internal URL: {host}"));
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
            return Err(format!("blocked private/internal IP address: {ip}"));
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

    let parsed = url::Url::parse(url).map_err(|e| format!("invalid url: {e}"))?;
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
#[path = "oauth/proxy_ownership_tests.rs"]
mod oauth_ownership_tests;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api;
    use crate::config::{AppConfig, ProviderConfig, StateConfig};
    use crate::keys::{
        CostCalculator, CreateKeyRequest, KeyPermissions, KeyStore, KeyValidator, RateLimiter,
    };
    use crate::upstream::UpstreamResponse;
    use anyhow::Context;
    use axum::routing::{any, post};
    use axum::Router;
    use bytes::Bytes;
    use futures::{stream, StreamExt};
    use http::{HeaderMap, Method, StatusCode};
    use serde_json::json;
    use std::collections::HashMap;
    use std::sync::atomic::AtomicU32;
    use std::sync::Arc;
    use std::time::Duration;
    use tokio::sync::Mutex;
    use tower::util::ServiceExt;

    #[expect(
        clippy::panic_in_result_fn,
        reason = "assertions are intentional in tests"
    )]
    #[test]
    fn opencode_session_header_is_applied() -> Result<(), anyhow::Error> {
        let mut headers = http::HeaderMap::new();
        apply_opencode_session_headers(&mut headers, ProviderType::OpenCodeGo, "sess-1");
        assert_eq!(
            headers
                .get("x-opencode-session")
                .context("get(...) returned None or Err")?
                .to_str()
                .context("to_str(...) returned None or Err")?,
            "sess-1"
        );

        // OpenCode (Zen) also gets the session header.
        let mut h2 = http::HeaderMap::new();
        apply_opencode_session_headers(&mut h2, ProviderType::OpenCode, "sess-2");
        assert_eq!(
            h2.get("x-opencode-session")
                .context("get(...) returned None or Err")?
                .to_str()
                .context("to_str(...) returned None or Err")?,
            "sess-2"
        );

        // Non-OpenCode providers must NOT receive the header.
        let mut h3 = http::HeaderMap::new();
        apply_opencode_session_headers(&mut h3, ProviderType::OpenAI, "sess-3");
        assert!(h3.get("x-opencode-session").is_none());

        // Empty session id -> no header.
        let mut h4 = http::HeaderMap::new();
        apply_opencode_session_headers(&mut h4, ProviderType::OpenCodeGo, "");
        assert!(h4.get("x-opencode-session").is_none());

        Ok(())
    }

    #[expect(
        clippy::panic_in_result_fn,
        reason = "assertions are intentional in tests"
    )]
    #[test]
    fn websocket_text_messages_are_sanitized_and_audited() -> Result<(), anyhow::Error> {
        let (analysis_tx, mut audit_rx) = tokio::sync::broadcast::channel(8);
        let message = AxumWsMessage::Text(
            json!({
                "type": "response.create",
                "input": [{
                    "content": [{
                        "type": "input_file",
                        "file_url": "https://ws-target.example/private"
                    }]
                }],
                "tools": [{"type": "web_search"}],
                "tool_choice": {"type": "web_search"}
            })
            .to_string(),
        );

        let sanitized = sanitize_ws_client_message(
            message,
            &crate::delegated_fetch::DelegatedFetchPolicy::default(),
            &analysis_tx,
        )
        .context("unexpected None or Err")?;
        let TungsteniteMessage::Text(text) = sanitized else {
            return Err(anyhow::anyhow!("expected text frame"));
        };
        let body: Value =
            serde_json::from_str(text.as_str()).context("as_str(...) returned None or Err")?;
        assert!(!body.to_string().contains("file_url"));
        assert!(body["tools"]
            .as_array()
            .context("as_array(...) returned None or Err")?
            .is_empty());
        assert_eq!(body["tool_choice"], "auto");

        let events: Vec<AnalysisEvent> = std::iter::from_fn(|| audit_rx.try_recv().ok()).collect();
        assert!(events.iter().any(|event| matches!(
            event,
            AnalysisEvent::DelegatedFetchStripped {
                target_host: Some(host),
                ..
            } if host == "ws-target.example"
        )));

        Ok(())
    }

    #[expect(
        clippy::panic_in_result_fn,
        reason = "assertions are intentional in tests"
    )]
    #[test]
    fn websocket_binary_messages_are_sanitized_and_invalid_json_is_rejected(
    ) -> Result<(), anyhow::Error> {
        let (analysis_tx, mut audit_rx) = tokio::sync::broadcast::channel(8);
        let binary = AxumWsMessage::Binary(
            serde_json::to_vec(&json!({
                "tools": [{"type": "web_fetch"}],
                "tool_choice": "web_fetch"
            }))
            .context("unexpected None or Err")?,
        );

        let sanitized = sanitize_ws_client_message(
            binary,
            &crate::delegated_fetch::DelegatedFetchPolicy::default(),
            &analysis_tx,
        )
        .context("unexpected None or Err")?;
        let TungsteniteMessage::Binary(bytes) = sanitized else {
            return Err(anyhow::anyhow!("expected binary frame"));
        };
        let body: Value = serde_json::from_slice(&bytes).context("unexpected None or Err")?;
        assert!(body["tools"]
            .as_array()
            .context("as_array(...) returned None or Err")?
            .is_empty());
        assert_eq!(body["tool_choice"], "auto");

        assert!(sanitize_ws_client_message(
            AxumWsMessage::Text("not-json".to_string()),
            &crate::delegated_fetch::DelegatedFetchPolicy::default(),
            &analysis_tx,
        )
        .is_none());

        let events: Vec<AnalysisEvent> = std::iter::from_fn(|| audit_rx.try_recv().ok()).collect();
        assert!(events.iter().any(|event| matches!(
            event,
            AnalysisEvent::DelegatedFetchStripped { capability, .. }
                if capability == "invalid_json_frame_rejected"
        )));

        Ok(())
    }

    #[expect(
        clippy::panic_in_result_fn,
        reason = "assertions are intentional in tests"
    )]
    #[test]
    fn test_apply_injections() -> Result<(), anyhow::Error> {
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

        let messages = body["messages"]
            .as_array()
            .context("as_array(...) returned None or Err")?;

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

        Ok(())
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

    #[expect(
        clippy::panic_in_result_fn,
        reason = "assertions are intentional in tests"
    )]
    #[test]
    fn test_apply_injections_multiple_system_messages() -> Result<(), anyhow::Error> {
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

        let messages = body["messages"]
            .as_array()
            .context("as_array(...) returned None or Err")?;

        // Should have 3 messages
        assert_eq!(messages.len(), 3);

        // Both system messages should be at the beginning, in original order
        assert_eq!(messages[0]["role"], "system");
        assert_eq!(messages[0]["content"], "First system");
        assert_eq!(messages[1]["role"], "system");
        assert_eq!(messages[1]["content"], "Second system");
        assert_eq!(messages[2]["role"], "user");

        Ok(())
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

    #[expect(
        clippy::panic_in_result_fn,
        reason = "assertions are intentional in tests"
    )]
    #[test]
    fn test_upsert_csv_header_merges_and_dedupes() -> Result<(), anyhow::Error> {
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
            .context("unexpected None or Err")?
            .to_str()
            .context("unexpected None or Err")?
            .to_string();
        assert!(val.contains("prompt-caching-2024-07-31"));
        assert!(val.contains("pdfs-2024-09-25"));

        Ok(())
    }

    #[expect(
        clippy::panic_in_result_fn,
        reason = "assertions are intentional in tests"
    )]
    #[test]
    fn test_normalize_upstream_error_rate_limit() -> Result<(), anyhow::Error> {
        let body = br#"{"error":{"message":"rate limited","type":"rate_limit_error"}}"#;
        let (msg, ty, code) = normalize_upstream_error(
            ProviderType::OpenAI,
            StatusCode::TOO_MANY_REQUESTS,
            std::str::from_utf8(body).context("unexpected None or Err")?,
            body,
        );
        assert_eq!(msg, "rate limited");
        assert_eq!(ty, "rate_limit_error");
        assert!(code.is_some());

        Ok(())
    }

    #[expect(
        clippy::panic_in_result_fn,
        reason = "assertions are intentional in tests"
    )]
    #[test]
    fn test_apply_injections_empty_injections() -> Result<(), anyhow::Error> {
        let mut body = json!({
            "model": "gpt-4",
            "messages": [
                {"role": "user", "content": "Hello"}
            ]
        });

        let injections: Vec<Injection> = vec![];
        apply_injections(&mut body, &injections);

        let messages = body["messages"]
            .as_array()
            .context("as_array(...) returned None or Err")?;
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0]["content"], "Hello");

        Ok(())
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

    #[expect(
        clippy::panic_in_result_fn,
        reason = "assertions are intentional in tests"
    )]
    #[test]
    fn test_apply_injections_with_existing_system_message() -> Result<(), anyhow::Error> {
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

        let messages = body["messages"]
            .as_array()
            .context("as_array(...) returned None or Err")?;

        // Should have 3 messages now
        assert_eq!(messages.len(), 3);

        // Injected system should be first
        assert_eq!(messages[0]["content"], "Injected system");
        // Original system second
        assert_eq!(messages[1]["content"], "Original system");
        // User message last
        assert_eq!(messages[2]["content"], "Hello");

        Ok(())
    }

    #[expect(
        clippy::panic_in_result_fn,
        reason = "assertions are intentional in tests"
    )]
    #[test]
    fn test_apply_injections_user_and_assistant_roles() -> Result<(), anyhow::Error> {
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

        let messages = body["messages"]
            .as_array()
            .context("as_array(...) returned None or Err")?;

        // Should have 3 messages
        assert_eq!(messages.len(), 3);

        // Original user first
        assert_eq!(messages[0]["content"], "Original user");
        // Injected user second (appended)
        assert_eq!(messages[1]["content"], "Injected user");
        // Injected assistant last (appended)
        assert_eq!(messages[2]["content"], "Injected assistant");

        Ok(())
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

    #[expect(
        clippy::panic_in_result_fn,
        reason = "assertions are intentional in tests"
    )]
    #[test]
    fn test_ssrf_protection_blocks_localhost() -> Result<(), anyhow::Error> {
        let url =
            url::Url::parse("http://localhost/image.png").context("unexpected None or Err")?;
        let result = super::validate_url_not_internal(&url);
        assert!(result.is_err());
        assert!(matches!(result, Err(e) if e.contains("blocked internal URL")));

        Ok(())
    }

    #[expect(
        clippy::panic_in_result_fn,
        reason = "assertions are intentional in tests"
    )]
    #[test]
    fn test_ssrf_protection_blocks_loopback() -> Result<(), anyhow::Error> {
        let url =
            url::Url::parse("http://127.0.0.1/image.png").context("unexpected None or Err")?;
        let result = super::validate_url_not_internal(&url);
        assert!(result.is_err());
        assert!(matches!(result, Err(e) if e.contains("blocked internal URL")));

        Ok(())
    }

    #[expect(
        clippy::panic_in_result_fn,
        reason = "assertions are intentional in tests"
    )]
    #[test]
    fn test_ssrf_protection_blocks_metadata_endpoint() -> Result<(), anyhow::Error> {
        let url = url::Url::parse("http://169.254.169.254/latest/meta-data/")
            .context("unexpected None or Err")?;
        let result = super::validate_url_not_internal(&url);
        assert!(result.is_err());
        assert!(matches!(result, Err(e) if e.contains("blocked internal URL")));

        Ok(())
    }

    #[expect(
        clippy::panic_in_result_fn,
        reason = "assertions are intentional in tests"
    )]
    #[test]
    fn test_ssrf_protection_blocks_private_ip_10() -> Result<(), anyhow::Error> {
        let url = url::Url::parse("http://10.0.0.1/image.png").context("unexpected None or Err")?;
        let result = super::validate_url_not_internal(&url);
        assert!(result.is_err());
        assert!(matches!(result, Err(e) if e.contains("blocked private/internal IP")));

        Ok(())
    }

    #[expect(
        clippy::panic_in_result_fn,
        reason = "assertions are intentional in tests"
    )]
    #[test]
    fn test_ssrf_protection_blocks_private_ip_172() -> Result<(), anyhow::Error> {
        let url =
            url::Url::parse("http://172.16.0.1/image.png").context("unexpected None or Err")?;
        let result = super::validate_url_not_internal(&url);
        assert!(result.is_err());
        assert!(matches!(result, Err(e) if e.contains("blocked private/internal IP")));

        Ok(())
    }

    #[expect(
        clippy::panic_in_result_fn,
        reason = "assertions are intentional in tests"
    )]
    #[test]
    fn test_ssrf_protection_blocks_private_ip_192() -> Result<(), anyhow::Error> {
        let url =
            url::Url::parse("http://192.168.1.1/image.png").context("unexpected None or Err")?;
        let result = super::validate_url_not_internal(&url);
        assert!(result.is_err());
        assert!(matches!(result, Err(e) if e.contains("blocked private/internal IP")));

        Ok(())
    }

    #[expect(
        clippy::panic_in_result_fn,
        reason = "assertions are intentional in tests"
    )]
    #[test]
    fn test_ssrf_protection_blocks_ipv6_loopback() -> Result<(), anyhow::Error> {
        let url = url::Url::parse("http://[::1]/image.png").context("unexpected None or Err")?;
        let result = super::validate_url_not_internal(&url);
        assert!(result.is_err());
        assert!(matches!(result, Err(e) if e.contains("blocked internal URL")));

        Ok(())
    }

    #[expect(
        clippy::panic_in_result_fn,
        reason = "assertions are intentional in tests"
    )]
    #[test]
    fn test_ssrf_protection_blocks_ipv6_link_local() -> Result<(), anyhow::Error> {
        let url =
            url::Url::parse("http://[fe80::1]/image.png").context("unexpected None or Err")?;
        let result = super::validate_url_not_internal(&url);
        assert!(result.is_err());
        assert!(matches!(result, Err(e) if e.contains("blocked private/internal IP")));

        Ok(())
    }

    #[expect(
        clippy::panic_in_result_fn,
        reason = "assertions are intentional in tests"
    )]
    #[test]
    fn test_ssrf_protection_allows_public_ip() -> Result<(), anyhow::Error> {
        let url =
            url::Url::parse("https://example.com/image.png").context("unexpected None or Err")?;
        let result = super::validate_url_not_internal(&url);
        assert!(result.is_ok());

        Ok(())
    }

    #[expect(
        clippy::panic_in_result_fn,
        reason = "assertions are intentional in tests"
    )]
    #[test]
    fn test_ssrf_protection_allows_public_ip_address() -> Result<(), anyhow::Error> {
        let url = url::Url::parse("https://8.8.8.8/image.png").context("unexpected None or Err")?;
        let result = super::validate_url_not_internal(&url);
        assert!(result.is_ok());

        Ok(())
    }

    type MockImages = HashMap<String, (HeaderMap, Bytes, StatusCode)>;

    #[derive(Clone)]
    struct MockUpstream {
        requests: Arc<Mutex<Vec<crate::upstream::UpstreamRequest>>>,
        responses: Arc<Mutex<Vec<ResponseSpec>>>,
        images: Arc<MockImages>,
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

        fn with_images(mut self, images: MockImages) -> Self {
            self.images = Arc::new(images);
            self
        }

        async fn take_requests(&self) -> Vec<crate::upstream::UpstreamRequest> {
            std::mem::take(&mut *self.requests.lock().await)
        }
    }

    impl crate::upstream::Upstream for MockUpstream {
        fn send(
            &self,
            request: crate::upstream::UpstreamRequest,
        ) -> futures::future::BoxFuture<'_, Result<UpstreamResponse, std::io::Error>> {
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
                            body: stream::iter(chunks)
                                .then(|chunk| async move {
                                    tokio::task::yield_now().await;
                                    Ok(chunk)
                                })
                                .boxed(),
                        });
                    }
                }

                let spec = {
                    let mut responses = self.responses.lock().await;
                    responses.remove(0)
                };
                Ok(UpstreamResponse {
                    status: spec.status,
                    headers: spec.headers,
                    body: stream::iter(spec.chunks)
                        .then(|chunk| async move {
                            tokio::task::yield_now().await;
                            Ok(chunk)
                        })
                        .boxed(),
                })
            })
        }
    }

    #[expect(
        clippy::panic_in_result_fn,
        reason = "assertions are intentional in tests"
    )]
    #[tokio::test]
    async fn e2e_typesafe_systemone_forwards_caller_key_order_verbatim() -> Result<(), anyhow::Error>
    {
        let body = br#"{"model":"jev-latest", "state":{"task":"x","status":"y"},"questions":{"risk":{"type":"choice","instructions":"How risky?","criteria":{"low":null,"medium":null,"high":null}}}}"#;
        let mock = MockUpstream::new(vec![ResponseSpec {
            status: StatusCode::OK,
            headers: HeaderMap::from_iter([(
                http::header::CONTENT_TYPE,
                http::HeaderValue::from_static("application/json"),
            )]),
            chunks: vec![Bytes::from_static(br#"{"answers":{}}"#)],
        }]);
        let mut providers = HashMap::new();
        providers.insert(
            "typesafe".into(),
            ProviderConfig {
                type_: "typesafe".into(),
                base_url: "http://up/v1".into(),
                api_key: "secret".into(),
                ..Default::default()
            },
        );
        let app = Router::new()
            .route("/:provider/v1/*path", any(provider_proxy_handler))
            .with_state(AppState::new_with_upstream(
                make_config(providers),
                Arc::new(mock.clone()),
            ));
        let request = http::Request::builder()
            .method("POST")
            .uri("/typesafe/v1/systemone")
            .header(http::header::CONTENT_TYPE, "application/json")
            .body(Body::from(body.as_slice()))?;
        let response = app.oneshot(request).await?;
        assert_eq!(response.status(), StatusCode::OK);
        let sent = mock.take_requests().await;
        assert_eq!(sent[0].body.as_ref(), body.as_slice());
        Ok(())
    }

    #[test]
    fn native_json_preservation_does_not_bypass_rewrites() {
        let original = br#"{ "model":"original", "z":1,"a":2 }"#;
        assert!(native_json_unchanged(
            true,
            original,
            &serde_json::json!({"model":"original", "a":2,"z":1})
        ));
        assert!(!native_json_unchanged(
            true,
            original,
            &serde_json::json!({"model":"rewritten", "a":2,"z":1})
        ));
        assert!(!native_json_unchanged(
            false,
            original,
            &serde_json::json!({"model":"original", "a":2,"z":1})
        ));
        assert!(!native_json_unchanged(true, b"", &Value::Null));
    }

    fn make_config(providers: HashMap<String, ProviderConfig>) -> AppConfig {
        AppConfig {
            reload_restart_input: None,
            server: crate::config::ServerConfig::default(),
            inference: Default::default(),
            providers,
            upstream: HashMap::new(),
            logging: crate::config::LoggingConfig::default(),
            analysis: crate::config::AnalysisConfig::default(),
            policy: crate::policy::PolicyConfig::default(),
            delegated_fetch: crate::delegated_fetch::DelegatedFetchConfig::default(),
            state: crate::config::StateConfig::default(),
            keys: crate::config::KeysConfig::default(),
            capture: crate::config::CaptureConfig::default(),
            transform: crate::config::TransformConfig::default(),
            network: crate::config::NetworkConfig::default(),
            egress: crate::config::EgressConfig::default(),
            mock_responses: crate::config::MockResponsesConfig::default(),
            native_pricing: crate::config::NativePricingConfig::default(),
        }
    }

    #[expect(
        clippy::panic_in_result_fn,
        reason = "assertions are intentional in tests"
    )]
    #[tokio::test]
    async fn e2e_delegated_fetch_is_stripped_before_passthrough_and_audited(
    ) -> Result<(), anyhow::Error> {
        let mock = MockUpstream::new(vec![
            ResponseSpec {
                status: StatusCode::OK,
                headers: HeaderMap::new(),
                chunks: vec![Bytes::from_static(b"{}")],
            },
            ResponseSpec {
                status: StatusCode::OK,
                headers: HeaderMap::new(),
                chunks: vec![Bytes::from_static(b"{}")],
            },
        ]);

        let mut providers = HashMap::new();
        providers.insert(
            "default".to_string(),
            ProviderConfig {
                type_: "openai-compatible".to_string(),
                base_url: "http://up/v1".to_string(),
                ..Default::default()
            },
        );

        let state = AppState::new_with_upstream(make_config(providers), Arc::new(mock.clone()));
        let mut audit_rx = state.analysis_tx.subscribe();
        let app = Router::new()
            .route("/v1/*path", any(proxy_handler))
            .with_state(state);

        let unsafe_body = json!({
            "model": "gpt-5",
            "input": [{
                "role": "user",
                "content": [
                    {"type": "input_text", "text": "summarize"},
                    {"type": "input_file", "file_url": "https://metadata.example/secrets"}
                ]
            }],
            "tools": [
                {"type": "function", "function": {"name": "local_tool"}},
                {"type": "web_search"}
            ],
            "tool_choice": {"type": "web_search"}
        });
        let unsafe_request = http::Request::builder()
            .method("POST")
            .uri("/v1/responses")
            .header(http::header::CONTENT_TYPE, "application/json")
            .header("x-provider", "default")
            .body(Body::from(
                serde_json::to_vec(&unsafe_body).context("body(...) returned None or Err")?,
            ))
            .context("unexpected None or Err")?;
        let _ = app
            .clone()
            .oneshot(unsafe_request)
            .await
            .context("oneshot(...) returned None or Err")?;

        let control_body = json!({
            "model": "gpt-5",
            "input": [{
                "role": "user",
                "content": [{"type": "input_text", "text": "hello"}]
            }],
            "tools": [{"type": "function", "function": {"name": "local_tool"}}],
            "tool_choice": "auto"
        });
        let control_bytes = serde_json::to_vec(&control_body).context("unexpected None or Err")?;
        let control_request = http::Request::builder()
            .method("POST")
            .uri("/v1/responses")
            .header(http::header::CONTENT_TYPE, "application/json")
            .header("x-provider", "default")
            .body(Body::from(control_bytes.clone()))
            .context("unexpected None or Err")?;
        let _ = app
            .oneshot(control_request)
            .await
            .context("oneshot(...) returned None or Err")?;

        let requests = mock.take_requests().await;
        assert_eq!(requests.len(), 2);
        let forwarded: Value =
            serde_json::from_slice(&requests[0].body).context("unexpected None or Err")?;
        assert_eq!(
            forwarded["input"][0]["content"]
                .as_array()
                .context("as_array(...) returned None or Err")?
                .len(),
            1
        );
        assert_eq!(
            forwarded["tools"]
                .as_array()
                .context("as_array(...) returned None or Err")?
                .len(),
            1
        );
        assert_eq!(forwarded["tool_choice"], "auto");
        assert!(!String::from_utf8_lossy(&requests[0].body).contains("file_url"));
        assert_eq!(requests[1].body, control_bytes);

        let mut audit_items = Vec::new();
        while let Ok(event) = audit_rx.try_recv() {
            if let AnalysisEvent::DelegatedFetchStripped {
                field_path,
                capability,
                target_host,
                ..
            } = event
            {
                audit_items.push((field_path, capability, target_host));
            }
        }
        assert!(audit_items.iter().any(|(path, capability, host)| {
            path == "/input/0/content/1/file_url"
                && capability == "input_file.file_url"
                && host.as_deref() == Some("metadata.example")
        }));
        assert!(audit_items
            .iter()
            .any(|(_, capability, _)| capability == "server_tool:web_search"));
        assert!(audit_items
            .iter()
            .any(|(path, _, _)| path == "/tool_choice"));

        Ok(())
    }

    #[expect(
        clippy::panic_in_result_fn,
        reason = "assertions are intentional in tests"
    )]
    #[tokio::test]
    async fn e2e_virtual_key_metadata_controls_remote_content_per_tenant(
    ) -> Result<(), anyhow::Error> {
        let mock = MockUpstream::new(vec![
            ResponseSpec {
                status: StatusCode::OK,
                headers: HeaderMap::new(),
                chunks: vec![Bytes::from_static(b"{}")],
            },
            ResponseSpec {
                status: StatusCode::OK,
                headers: HeaderMap::new(),
                chunks: vec![Bytes::from_static(b"{}")],
            },
        ]);
        let mut providers = HashMap::new();
        providers.insert(
            "default".to_string(),
            ProviderConfig {
                type_: "openai-compatible".to_string(),
                base_url: "http://up/v1".to_string(),
                ..Default::default()
            },
        );
        let temp = tempfile::tempdir().context("unexpected None or Err")?;
        let mut config = make_config(providers);
        config.keys.enabled = true;
        config.keys.require_key = true;
        config.keys.database_path = temp.path().join("keys.db").display().to_string();
        // Virtual-key metadata must still default deny even if the process-wide
        // fallback permits remote content for non-key clients.
        config.delegated_fetch.enabled = true;

        let state = AppState::new_with_upstream(config, Arc::new(mock.clone()));
        state
            .init_key_store()
            .await
            .map_err(|e| anyhow::anyhow!("init_key_store: {e}"))?;
        let store = state
            .get_key_store()
            .context("get_key_store(...) returned None or Err")?;
        let denied_key = store
            .create_key(crate::keys::CreateKeyRequest::default())
            .await
            .context("unexpected None or Err")?;
        let allowed_key = store
            .create_key(crate::keys::CreateKeyRequest {
                metadata: json!({
                    "delegated_fetch": {"remote_content": true}
                }),
                ..Default::default()
            })
            .await
            .context("unexpected None or Err")?;

        let app = Router::new()
            .route("/v1/*path", any(proxy_handler))
            .with_state(state);
        let body = json!({
            "model": "gpt-5",
            "input": [{
                "content": [{
                    "type": "input_file",
                    "file_url": "https://tenant.example/private"
                }]
            }]
        });
        for key in [&denied_key.key, &allowed_key.key] {
            let request = http::Request::builder()
                .method("POST")
                .uri("/v1/responses")
                .header(http::header::CONTENT_TYPE, "application/json")
                .header("x-provider", "default")
                .header(http::header::AUTHORIZATION, format!("Bearer {key}"))
                .body(Body::from(
                    serde_json::to_vec(&body).context("body(...) returned None or Err")?,
                ))
                .context("unexpected None or Err")?;
            let response = app
                .clone()
                .oneshot(request)
                .await
                .context("oneshot(...) returned None or Err")?;
            assert_eq!(response.status(), StatusCode::OK);
        }

        let requests = mock.take_requests().await;
        assert_eq!(requests.len(), 2);
        assert!(!String::from_utf8_lossy(&requests[0].body).contains("file_url"));
        assert!(String::from_utf8_lossy(&requests[1].body).contains("file_url"));

        Ok(())
    }

    #[expect(
        clippy::panic_in_result_fn,
        reason = "assertions are intentional in tests"
    )]
    #[tokio::test]
    async fn e2e_passthrough_applies_compat_to_openai_compatible_body() -> Result<(), anyhow::Error>
    {
        let mock = MockUpstream::new(vec![ResponseSpec {
            status: StatusCode::OK,
            headers: HeaderMap::new(),
            chunks: vec![Bytes::from_static(b"{}")],
        }]);

        let mut providers = HashMap::new();
        providers.insert(
            "default".to_string(),
            ProviderConfig {
                type_: "openai-compatible".to_string(),
                base_url: "http://up1/v1".to_string(),
                compat: crate::provider::CompatSettings {
                    supports_developer_role: Some(false),
                    supports_store: Some(false),
                    supports_stream_options: Some(false),
                    max_tokens_field: Some("max_tokens".to_string()),
                },
                ..Default::default()
            },
        );

        let state = AppState::new_with_upstream(make_config(providers), Arc::new(mock.clone()));
        let app = Router::new()
            .route("/v1/*path", any(proxy_handler))
            .with_state(state);

        let req = http::Request::builder()
            .method("POST")
            .uri("/v1/chat/completions")
            .header(http::header::CONTENT_TYPE, "application/json")
            .body(Body::from(
                serde_json::to_vec(&json!({
                    "model": "Kimi-K2.6",
                    "messages": [
                        {"role": "developer", "content": "be terse"},
                        {"role": "user", "content": "hi"}
                    ],
                    "store": true,
                    "stream_options": {"include_usage": true},
                    "max_completion_tokens": 20
                }))
                .context("unexpected None or Err")?,
            ))
            .context("unexpected None or Err")?;
        let _ = app
            .oneshot(req)
            .await
            .context("oneshot(...) returned None or Err")?;

        let requests = mock.take_requests().await;
        assert_eq!(requests.len(), 1);
        let forwarded: Value =
            serde_json::from_slice(&requests[0].body).context("unexpected None or Err")?;
        assert_eq!(forwarded["messages"][0]["role"], "system");
        assert!(forwarded.get("store").is_none());
        assert!(forwarded.get("stream_options").is_none());
        assert_eq!(forwarded["max_tokens"], 20);

        Ok(())
    }

    #[expect(
        clippy::panic_in_result_fn,
        reason = "assertions are intentional in tests"
    )]
    #[tokio::test]
    async fn e2e_passthrough_and_injection_without_network() -> Result<(), anyhow::Error> {
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
            chunks: vec![Bytes::from(
                serde_json::to_vec(&response_json).context("unexpected None or Err")?,
            )],
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
            .body(Body::from(
                serde_json::to_vec(&inj).context("body(...) returned None or Err")?,
            ))
            .context("unexpected None or Err")?;
        let resp = app
            .clone()
            .oneshot(req)
            .await
            .context("oneshot(...) returned None or Err")?;
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
            .body(Body::from(
                serde_json::to_vec(&req_body).context("body(...) returned None or Err")?,
            ))
            .context("unexpected None or Err")?;
        let resp = app
            .oneshot(req)
            .await
            .context("oneshot(...) returned None or Err")?;
        assert_eq!(resp.status(), StatusCode::OK);

        let requests = mock.take_requests().await;
        assert_eq!(requests.len(), 1);
        let sent: serde_json::Value =
            serde_json::from_slice(&requests[0].body).context("unexpected None or Err")?;
        let msgs = sent["messages"]
            .as_array()
            .context("as_array(...) returned None or Err")?;
        assert_eq!(msgs[0]["role"], "system");
        assert_eq!(msgs[0]["content"], "You are injected");
        assert_eq!(msgs[1]["role"], "user");

        Ok(())
    }

    #[expect(
        clippy::panic_in_result_fn,
        reason = "assertions are intentional in tests"
    )]
    #[tokio::test]
    async fn e2e_provider_routing_changes_upstream_url() -> Result<(), anyhow::Error> {
        let response_json = json!({"choices":[{"message":{"role":"assistant","content":"ok"}}]});
        let mock = MockUpstream::new(vec![
            ResponseSpec {
                status: StatusCode::OK,
                headers: HeaderMap::new(),
                chunks: vec![Bytes::from(
                    serde_json::to_vec(&response_json).context("unexpected None or Err")?,
                )],
            },
            ResponseSpec {
                status: StatusCode::OK,
                headers: HeaderMap::new(),
                chunks: vec![Bytes::from(
                    serde_json::to_vec(&response_json).context("unexpected None or Err")?,
                )],
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
            .body(Body::from(
                serde_json::to_vec(&req_body).context("body(...) returned None or Err")?,
            ))
            .context("unexpected None or Err")?;
        let _ = app
            .clone()
            .oneshot(req1)
            .await
            .context("oneshot(...) returned None or Err")?;

        let req2 = http::Request::builder()
            .method("POST")
            .uri("/v1/chat/completions")
            .header(http::header::CONTENT_TYPE, "application/json")
            .header("X-Provider", "default")
            .body(Body::from(
                serde_json::to_vec(&req_body).context("body(...) returned None or Err")?,
            ))
            .context("unexpected None or Err")?;
        let _ = app
            .oneshot(req2)
            .await
            .context("oneshot(...) returned None or Err")?;

        let requests = mock.take_requests().await;
        assert_eq!(requests.len(), 2);
        assert!(requests[0].url.starts_with("http://up2/"));
        assert!(requests[1].url.starts_with("http://up1/"));

        Ok(())
    }

    /// An upstream whose error body never yields and never terminates — the
    /// eavs-1sfs hang scenario. The proxy must bound the error-body read and
    /// still return a prompt 401 rather than hang forever.
    struct HangUpstream;
    impl crate::upstream::Upstream for HangUpstream {
        fn send(
            &self,
            _req: crate::upstream::UpstreamRequest,
        ) -> futures::future::BoxFuture<'_, Result<UpstreamResponse, std::io::Error>> {
            Box::pin(async move {
                let body = futures::stream::pending::<Result<Bytes, std::io::Error>>().boxed();
                Ok(UpstreamResponse {
                    status: StatusCode::UNAUTHORIZED,
                    headers: HeaderMap::new(),
                    body,
                })
            })
        }
    }

    /// eavs-1sfs double-proof: (1) a terminated 401 surfaces promptly as a
    /// well-formed error; (2) a 401 whose error body never terminates is bounded
    /// by `upstream_error_body_timeout()` and does not hang.
    #[expect(
        clippy::panic_in_result_fn,
        reason = "assertions are intentional in tests"
    )]
    #[tokio::test]
    async fn upstream_401_with_non_terminating_body_does_not_hang() -> Result<(), anyhow::Error> {
        std::env::set_var("EAVS_UPSTREAM_ERROR_BODY_TIMEOUT_MS", "50");
        let mut providers = HashMap::new();
        providers.insert(
            "default".to_string(),
            ProviderConfig {
                type_: "openai".to_string(),
                base_url: "http://upstream/v1".to_string(),
                ..Default::default()
            },
        );
        let state = AppState::new_with_upstream(make_config(providers), Arc::new(HangUpstream));
        let app = Router::new()
            .route("/v1/*path", any(proxy_handler))
            .with_state(state);
        let req = http::Request::builder()
            .method("POST")
            .uri("/v1/chat/completions")
            .header(http::header::CONTENT_TYPE, "application/json")
            .body(Body::from(
                json!({"model":"gpt-4o-mini","messages":[{"role":"user","content":"hi"}]})
                    .to_string(),
            ))
            .context("unexpected None or Err")?;
        // Must return well before the 50ms bounded read + margin, and as a 401.
        let result =
            tokio::time::timeout(std::time::Duration::from_secs(5), app.oneshot(req)).await;
        assert!(
            result.is_ok(),
            "upstream 401 with a non-terminating body must not hang"
        );
        let response = result
            .context("unexpected None or Err")?
            .context("unwrap(...) returned None or Err")?;
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        std::env::remove_var("EAVS_UPSTREAM_ERROR_BODY_TIMEOUT_MS");

        Ok(())
    }

    #[expect(
        clippy::panic_in_result_fn,
        reason = "assertions are intentional in tests"
    )]
    #[tokio::test]
    async fn upstream_401_returns_error_promptly() -> Result<(), anyhow::Error> {
        // Repro eavs-1sfs: an upstream 401 must be surfaced as a prompt
        // OpenAI-style error, not leave the request hanging.
        let body = br#"{"error":{"message":"invalid api key","type":"invalid_request_error"}}"#;
        let mock = MockUpstream::new(vec![ResponseSpec {
            status: StatusCode::UNAUTHORIZED,
            headers: HeaderMap::new(),
            chunks: vec![Bytes::from(&body[..])],
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
            .route("/v1/*path", any(proxy_handler))
            .with_state(state);
        let req = http::Request::builder()
            .method("POST")
            .uri("/v1/chat/completions")
            .header(http::header::CONTENT_TYPE, "application/json")
            .body(Body::from(
                json!({"model":"gpt-4o-mini","messages":[{"role":"user","content":"hi"}]})
                    .to_string(),
            ))
            .context("unexpected None or Err")?;
        let result =
            tokio::time::timeout(std::time::Duration::from_secs(3), app.oneshot(req)).await;
        assert!(result.is_ok(), "upstream 401 must not hang");
        let response = result
            .context("unexpected None or Err")?
            .context("unwrap(...) returned None or Err")?;
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);

        Ok(())
    }

    #[test]
    fn native_shapes_are_scoped_to_their_provider() {
        assert!(native_inference_path(
            ProviderType::TypeSafe,
            "/v1/systemone"
        ));
        assert!(native_inference_path(ProviderType::Cohere, "/v2/rerank"));
        assert!(native_inference_path(ProviderType::Voyage, "/v1/rerank"));
        assert!(native_inference_path(
            ProviderType::OpenAI,
            "/v1/videos/vid_1/content"
        ));
        assert!(!native_inference_path(
            ProviderType::Anthropic,
            "/v1/videos"
        ));
        assert!(!native_inference_path(
            ProviderType::ElevenLabs,
            "/v1/admin/keys"
        ));
        assert!(!native_inference_path(
            ProviderType::TypeSafe,
            "/v1/chat/completions"
        ));
        assert!(has_remote_media_reference(
            &json!({"input_reference": {"image_url": "https://example.com/a.png"}})
        ));
        assert!(!has_remote_media_reference(
            &json!({"input_reference": {"image_url": "data:image/png;base64,AAAA"}})
        ));
    }

    #[expect(
        clippy::panic_in_result_fn,
        reason = "assertions are intentional in tests"
    )]
    #[tokio::test]
    async fn e2e_typesafe_systemone_preserves_typed_json_and_errors() -> Result<(), anyhow::Error> {
        let payload = json!({"model":"jev-latest","state":{"ticket":"refund"},
            "questions":{"urgent":{"type":"noul","instructions":"Is this urgent?"}}});
        let mock = MockUpstream::new(vec![ResponseSpec {
            status: StatusCode::UNPROCESSABLE_ENTITY,
            headers: HeaderMap::from_iter([(
                http::header::CONTENT_TYPE,
                http::HeaderValue::from_static("application/json"),
            )]),
            chunks: vec![Bytes::from_static(br#"{"detail":"invalid question"}"#)],
        }]);
        let mut providers = HashMap::new();
        providers.insert(
            "typesafe".into(),
            ProviderConfig {
                type_: "typesafe".into(),
                base_url: "http://up/v1".into(),
                api_key: "secret".into(),
                ..Default::default()
            },
        );
        let app = Router::new()
            .route("/:provider/v1/*path", any(provider_proxy_handler))
            .with_state(AppState::new_with_upstream(
                make_config(providers),
                Arc::new(mock.clone()),
            ));
        let request = http::Request::builder()
            .method("POST")
            .uri("/typesafe/v1/systemone")
            .header(http::header::CONTENT_TYPE, "application/json")
            .body(Body::from(
                serde_json::to_vec(&payload).context("body(...) returned None or Err")?,
            ))
            .context("unexpected None or Err")?;
        let response = app
            .oneshot(request)
            .await
            .context("oneshot(...) returned None or Err")?;
        assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
        let response_body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .context("unexpected None or Err")?;
        assert_eq!(response_body.as_ref(), br#"{"detail":"invalid question"}"#);
        let sent = mock.take_requests().await;
        assert_eq!(sent[0].url, "http://up/v1/systemone");
        assert_eq!(
            serde_json::from_slice::<Value>(&sent[0].body).context("unexpected None or Err")?,
            payload
        );
        assert_eq!(
            sent[0]
                .headers
                .get("authorization")
                .context("get(...) returned None or Err")?,
            "Bearer secret"
        );

        Ok(())
    }

    #[expect(
        clippy::panic_in_result_fn,
        reason = "assertions are intentional in tests"
    )]
    #[tokio::test]
    async fn e2e_elevenlabs_binary_upload_and_download_preserve_bytes() -> Result<(), anyhow::Error>
    {
        let mut audio_headers = HeaderMap::new();
        audio_headers.insert(
            http::header::CONTENT_TYPE,
            http::HeaderValue::from_static("audio/mpeg"),
        );
        let mock = MockUpstream::new(vec![ResponseSpec {
            status: StatusCode::OK,
            headers: audio_headers,
            chunks: vec![Bytes::from_static(b"\xff\xfb\x00\x80")],
        }]);
        let mut providers = HashMap::new();
        providers.insert(
            "elevenlabs".into(),
            ProviderConfig {
                type_: "elevenlabs".into(),
                base_url: "http://up/v1".into(),
                api_key: "secret".into(),
                ..Default::default()
            },
        );
        let app = Router::new()
            .route("/:provider/v1/*path", any(provider_proxy_handler))
            .with_state(AppState::new_with_upstream(
                make_config(providers),
                Arc::new(mock.clone()),
            ));
        let multipart = b"--boundary\r\nContent-Disposition: form-data; name=\"file\"; filename=\"audio.wav\"\r\n\r\n\xff\x00\r\n--boundary--\r\n";
        let request = http::Request::builder()
            .method("POST")
            .uri("/elevenlabs/v1/speech-to-text")
            .header(
                http::header::CONTENT_TYPE,
                "multipart/form-data; boundary=boundary",
            )
            .body(Body::from(multipart.as_slice()))
            .context("unexpected None or Err")?;
        let response = app
            .oneshot(request)
            .await
            .context("oneshot(...) returned None or Err")?;
        assert_eq!(
            response
                .headers()
                .get(http::header::CONTENT_TYPE)
                .context("get(...) returned None or Err")?,
            "audio/mpeg"
        );
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .context("unexpected None or Err")?;
        assert_eq!(body.as_ref(), b"\xff\xfb\x00\x80");
        let sent = mock.take_requests().await;
        assert_eq!(sent[0].body.as_ref(), multipart);
        assert_eq!(
            sent[0]
                .headers
                .get("xi-api-key")
                .context("get(...) returned None or Err")?,
            "secret"
        );
        assert_eq!(
            sent[0]
                .headers
                .get(http::header::CONTENT_TYPE)
                .context("get(...) returned None or Err")?,
            "multipart/form-data; boundary=boundary"
        );

        Ok(())
    }

    #[expect(
        clippy::panic_in_result_fn,
        reason = "assertions are intentional in tests"
    )]
    #[tokio::test]
    async fn remote_video_reference_is_rejected_before_upstream() -> Result<(), anyhow::Error> {
        let mock = MockUpstream::new(vec![]);
        let mut providers = HashMap::new();
        providers.insert(
            "default".into(),
            ProviderConfig {
                type_: "openai".into(),
                base_url: "http://up/v1".into(),
                ..Default::default()
            },
        );
        let app = Router::new()
            .route("/v1/*path", any(proxy_handler))
            .with_state(AppState::new_with_upstream(
                make_config(providers),
                Arc::new(mock.clone()),
            ));
        let payload = json!({"model":"sora-2","prompt":"x","input_reference":{"image_url":"https://example.com/private"}});
        let response = app
            .oneshot(
                http::Request::builder()
                    .method("POST")
                    .uri("/v1/videos")
                    .header(http::header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        serde_json::to_vec(&payload).context("body(...) returned None or Err")?,
                    ))
                    .context("unexpected None or Err")?,
            )
            .await
            .context("unexpected None or Err")?;
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
        assert!(mock.take_requests().await.is_empty());

        Ok(())
    }

    #[expect(
        clippy::panic_in_result_fn,
        reason = "assertions are intentional in tests"
    )]
    #[tokio::test]
    async fn unsupported_native_shape_rejected_before_upstream() -> Result<(), anyhow::Error> {
        let mock = MockUpstream::new(vec![]);
        let mut providers = HashMap::new();
        providers.insert(
            "default".into(),
            ProviderConfig {
                type_: "anthropic".into(),
                base_url: "http://up/v1".into(),
                ..Default::default()
            },
        );
        let app = Router::new()
            .route("/v1/*path", any(proxy_handler))
            .with_state(AppState::new_with_upstream(
                make_config(providers),
                Arc::new(mock.clone()),
            ));
        let response = app
            .oneshot(
                http::Request::builder()
                    .method("POST")
                    .uri("/v1/videos")
                    .body(Body::from("{}"))
                    .context("unexpected None or Err")?,
            )
            .await
            .context("unexpected None or Err")?;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        assert!(mock.take_requests().await.is_empty());

        Ok(())
    }

    #[expect(
        clippy::panic_in_result_fn,
        reason = "assertions are intentional in tests"
    )]
    #[tokio::test]
    async fn e2e_canonical_rerank_translates_voyage_without_changing_native_route(
    ) -> Result<(), anyhow::Error> {
        let response = json!({"object":"list","data":[{"index":1,"relevance_score":0.9}],
            "model":"rerank-2.5","usage":{"total_tokens":12}});
        let mock = MockUpstream::new(vec![ResponseSpec {
            status: StatusCode::OK,
            headers: HeaderMap::from_iter([(
                http::header::CONTENT_TYPE,
                http::HeaderValue::from_static("application/json"),
            )]),
            chunks: vec![Bytes::from(
                serde_json::to_vec(&response).context("unexpected None or Err")?,
            )],
        }]);
        let mut providers = HashMap::new();
        providers.insert(
            "voyage".into(),
            ProviderConfig {
                type_: "voyage".into(),
                base_url: "http://up/v1".into(),
                api_key: "secret".into(),
                ..Default::default()
            },
        );
        let app = Router::new()
            .route("/:provider/v1/*path", any(provider_proxy_handler))
            .with_state(AppState::new_with_upstream(
                make_config(providers),
                Arc::new(mock.clone()),
            ));
        let payload = json!({"model":"rerank-2.5","query":"test","documents":["a","b"],"top_n":1});
        let result = app
            .oneshot(
                http::Request::builder()
                    .method("POST")
                    .uri("/voyage/v1/eavs/rerank")
                    .header(http::header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        serde_json::to_vec(&payload).context("body(...) returned None or Err")?,
                    ))
                    .context("unexpected None or Err")?,
            )
            .await
            .context("unexpected None or Err")?;
        assert_eq!(result.status(), StatusCode::OK);
        let value: Value = serde_json::from_slice(
            &axum::body::to_bytes(result.into_body(), usize::MAX)
                .await
                .context("unexpected None or Err")?,
        )
        .context("unexpected None or Err")?;
        assert_eq!(value["results"][0]["index"], 1);
        assert_eq!(value["usage"]["total_tokens"], 12);
        assert!(value.get("data").is_none());
        let sent = mock.take_requests().await;
        assert_eq!(sent[0].url, "http://up/v1/rerank");
        let upstream: Value =
            serde_json::from_slice(&sent[0].body).context("unexpected None or Err")?;
        assert_eq!(upstream["top_k"], 1);
        assert!(upstream.get("top_n").is_none());

        Ok(())
    }

    #[expect(
        clippy::panic_in_result_fn,
        reason = "assertions are intentional in tests"
    )]
    #[tokio::test]
    async fn e2e_canonical_rerank_keeps_cohere_shape() -> Result<(), anyhow::Error> {
        let value = json!({"results":[{"index":0,"relevance_score":0.9}]});
        let mock = MockUpstream::new(vec![ResponseSpec {
            status: StatusCode::OK,
            headers: HeaderMap::new(),
            chunks: vec![Bytes::from(
                serde_json::to_vec(&value).context("unexpected None or Err")?,
            )],
        }]);
        let mut providers = HashMap::new();
        providers.insert(
            "cohere".into(),
            ProviderConfig {
                type_: "cohere".into(),
                base_url: "http://up".into(),
                ..Default::default()
            },
        );
        let app = Router::new()
            .route("/:provider/v1/*path", any(provider_proxy_handler))
            .with_state(AppState::new_with_upstream(
                make_config(providers),
                Arc::new(mock.clone()),
            ));
        let request = json!({"model":"rerank-v3.5","query":"q","documents":["a"],"top_n":1});
        let response = app
            .oneshot(
                http::Request::builder()
                    .method("POST")
                    .uri("/cohere/v1/eavs/rerank")
                    .header(http::header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        serde_json::to_vec(&request).context("body(...) returned None or Err")?,
                    ))
                    .context("unexpected None or Err")?,
            )
            .await
            .context("unexpected None or Err")?;
        assert_eq!(response.status(), StatusCode::OK);
        let body: Value = serde_json::from_slice(
            &axum::body::to_bytes(response.into_body(), usize::MAX)
                .await
                .context("unexpected None or Err")?,
        )
        .context("unexpected None or Err")?;
        assert_eq!(body, value);
        let sent = mock.take_requests().await;
        assert_eq!(sent[0].url, "http://up/v2/rerank");
        assert_eq!(
            serde_json::from_slice::<Value>(&sent[0].body).context("unexpected None or Err")?,
            request
        );

        Ok(())
    }

    #[expect(
        clippy::panic_in_result_fn,
        reason = "assertions are intentional in tests"
    )]
    #[tokio::test]
    async fn capped_rerank_denied_before_upstream_uncapped_usage_preserved(
    ) -> Result<(), anyhow::Error> {
        use crate::keys::{CreateKeyRequest, KeyPermissions, KeyValidator, RateLimiter};
        for provider in ["cohere", "voyage"] {
            for permissions in [
                KeyPermissions {
                    max_budget_usd: Some(1.0),
                    ..Default::default()
                },
                KeyPermissions {
                    tpm_limit: Some(1000),
                    ..Default::default()
                },
                KeyPermissions::default(),
            ] {
                let capped =
                    permissions.max_budget_usd.is_some() || permissions.tpm_limit.is_some();
                let mut value = json!({
                    "results": [{"index":0,"relevance_score":0.9}],
                    "usage": {"total_tokens": 100},
                    "meta": {"billed_units": {"search_units": 1}}
                });
                if provider == "voyage" {
                    value["data"] = value
                        .as_object_mut()
                        .context("as_object_mut(...) returned None or Err")?
                        .remove("results")
                        .context("remove(...) returned None or Err")?;
                }
                let mock = MockUpstream::new(vec![ResponseSpec {
                    status: StatusCode::OK,
                    headers: HeaderMap::new(),
                    chunks: vec![Bytes::from(
                        serde_json::to_vec(&value).context("unexpected None or Err")?,
                    )],
                }]);
                let mut providers = HashMap::new();
                providers.insert(
                    "cohere".into(),
                    ProviderConfig {
                        type_: "cohere".into(),
                        base_url: "http://up".into(),
                        api_key: "secret".into(),
                        ..Default::default()
                    },
                );
                let mut provider_config = providers
                    .remove("cohere")
                    .context("remove(...) returned None or Err")?;
                provider_config.type_ = provider.into();
                providers.insert(provider.into(), provider_config);
                let state =
                    AppState::new_with_upstream(make_config(providers), Arc::new(mock.clone()));
                let store = Arc::new(
                    KeyStore::in_memory()
                        .await
                        .context("unexpected None or Err")?,
                );
                let validator = KeyValidator::new(store.clone(), Arc::new(RateLimiter::new()));
                state
                    .key_validator
                    .set(Arc::new(validator))
                    .ok()
                    .context("set(...) returned None or Err")?;
                state
                    .key_store
                    .set(store.clone())
                    .ok()
                    .context("clone(...) returned None or Err")?;
                state
                    .cost_calculator
                    .set(CostCalculator::new(state.pricing.clone()))
                    .ok()
                    .context("unexpected None or Err")?;
                let key = store
                    .create_key(CreateKeyRequest {
                        permissions,
                        ..Default::default()
                    })
                    .await
                    .context("unexpected None or Err")?
                    .key;
                let app = Router::new()
                    .route("/:provider/v1/*path", any(provider_proxy_handler))
                    .with_state(state);
                let response = app
                    .oneshot(
                        http::Request::builder()
                            .method("POST")
                            .uri(format!("/{provider}/v1/eavs/rerank"))
                            .header(http::header::CONTENT_TYPE, "application/json")
                            .header("authorization", format!("Bearer {key}"))
                            .body(Body::from(
                                json!({"model":"rerank-v3.5","query":"q","documents":["a"]})
                                    .to_string(),
                            ))
                            .context("unexpected None or Err")?,
                    )
                    .await
                    .context("unexpected None or Err")?;
                assert_eq!(
                    response.status(),
                    if capped {
                        StatusCode::BAD_REQUEST
                    } else {
                        StatusCode::OK
                    }
                );
                let body: Value = serde_json::from_slice(
                    &axum::body::to_bytes(response.into_body(), usize::MAX)
                        .await
                        .context("unexpected None or Err")?,
                )
                .context("unexpected None or Err")?;
                let requests = mock.take_requests().await;
                if capped {
                    assert!(requests.is_empty());
                    assert_eq!(body["error"]["type"], "unsupported_metering");
                } else {
                    assert_eq!(requests.len(), 1);
                    assert_eq!(body["usage"], value["usage"]);
                    assert_eq!(body["meta"], value["meta"]);
                    assert!(body["results"].is_array());
                }
                let settled = store
                    .get_by_key(&key)
                    .context("get_by_key(...) returned None or Err")?;
                assert_eq!(settled.usage.total_input_tokens, 0);
                assert_eq!(settled.usage.total_output_tokens, 0);
                assert_eq!(settled.usage.total_spend_usd, 0.0);
            }
        }

        Ok(())
    }

    #[expect(
        clippy::panic_in_result_fn,
        reason = "assertions are intentional in tests"
    )]
    #[tokio::test]
    async fn e2e_cohere_rerank_uses_v2_native_path() -> Result<(), anyhow::Error> {
        let mock = MockUpstream::new(vec![ResponseSpec {
            status: StatusCode::OK,
            headers: HeaderMap::from_iter([(
                http::header::CONTENT_TYPE,
                http::HeaderValue::from_static("application/json"),
            )]),
            chunks: vec![Bytes::from_static(
                br#"{"results":[{"index":0,"relevance_score":0.9}]}"#,
            )],
        }]);
        let mut providers = HashMap::new();
        providers.insert(
            "cohere".into(),
            ProviderConfig {
                type_: "cohere".into(),
                base_url: "http://up".into(),
                api_key: "secret".into(),
                ..Default::default()
            },
        );
        let app = Router::new()
            .route("/:provider/v2/*path", any(provider_proxy_handler))
            .with_state(AppState::new_with_upstream(
                make_config(providers),
                Arc::new(mock.clone()),
            ));
        let payload = json!({"model":"rerank-v3.5","query":"test","documents":["hello"]});
        let response = app
            .oneshot(
                http::Request::builder()
                    .method("POST")
                    .uri("/cohere/v2/rerank")
                    .header(http::header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        serde_json::to_vec(&payload).context("body(...) returned None or Err")?,
                    ))
                    .context("unexpected None or Err")?,
            )
            .await
            .context("unexpected None or Err")?;
        assert_eq!(response.status(), StatusCode::OK);
        let sent = mock.take_requests().await;
        assert_eq!(sent[0].url, "http://up/v2/rerank");
        assert_eq!(
            serde_json::from_slice::<Value>(&sent[0].body).context("unexpected None or Err")?,
            payload
        );

        Ok(())
    }

    #[expect(
        clippy::panic_in_result_fn,
        reason = "assertions are intentional in tests"
    )]
    #[tokio::test]
    async fn e2e_video_get_has_no_json_null_body() -> Result<(), anyhow::Error> {
        let mock = MockUpstream::new(vec![ResponseSpec {
            status: StatusCode::OK,
            headers: HeaderMap::new(),
            chunks: vec![Bytes::from_static(b"mp4")],
        }]);
        let mut providers = HashMap::new();
        providers.insert(
            "default".into(),
            ProviderConfig {
                type_: "openai".into(),
                base_url: "http://up/v1".into(),
                ..Default::default()
            },
        );
        let app = Router::new()
            .route("/v1/*path", any(proxy_handler))
            .with_state(AppState::new_with_upstream(
                make_config(providers),
                Arc::new(mock.clone()),
            ));
        let response = app
            .oneshot(
                http::Request::builder()
                    .uri("/v1/videos/vid_1/content")
                    .body(Body::empty())
                    .context("unexpected None or Err")?,
            )
            .await
            .context("unexpected None or Err")?;
        assert_eq!(response.status(), StatusCode::OK);
        let sent = mock.take_requests().await;
        assert!(sent[0].body.is_empty());
        assert!(sent[0].headers.get(http::header::CONTENT_TYPE).is_none());
        assert_eq!(sent[0].url, "http://up/v1/videos/vid_1/content");

        Ok(())
    }

    #[expect(
        clippy::panic_in_result_fn,
        reason = "assertions are intentional in tests"
    )]
    #[tokio::test]
    async fn e2e_streaming_passthrough_forwards_sse_payload() -> Result<(), anyhow::Error> {
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
            .body(Body::from(
                serde_json::to_vec(&req_body).context("body(...) returned None or Err")?,
            ))
            .context("unexpected None or Err")?;
        let resp = app
            .oneshot(req)
            .await
            .context("oneshot(...) returned None or Err")?;
        assert_eq!(resp.status(), StatusCode::OK);
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .context("unexpected None or Err")?;
        let text = String::from_utf8_lossy(&body);
        assert!(text.contains("data: [DONE]"));
        assert!(text.contains("\"hi\""));

        Ok(())
    }

    #[expect(
        clippy::panic_in_result_fn,
        reason = "assertions are intentional in tests"
    )]
    #[tokio::test]
    async fn e2e_error_forwarding() -> Result<(), anyhow::Error> {
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
            .body(Body::from(
                serde_json::to_vec(&req_body).context("body(...) returned None or Err")?,
            ))
            .context("unexpected None or Err")?;
        let resp = app
            .oneshot(req)
            .await
            .context("oneshot(...) returned None or Err")?;
        assert_eq!(resp.status(), StatusCode::TOO_MANY_REQUESTS);
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .context("unexpected None or Err")?;
        assert!(String::from_utf8_lossy(&body).contains("rate limited"));

        Ok(())
    }

    #[expect(
        clippy::panic_in_result_fn,
        reason = "assertions are intentional in tests"
    )]
    #[tokio::test]
    async fn bedrock_fake_streaming_emits_sse() -> Result<(), anyhow::Error> {
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
            chunks: vec![Bytes::from(
                serde_json::to_vec(&bedrock_body).context("unexpected None or Err")?,
            )],
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
            .body(Body::from(
                serde_json::to_vec(&req_body).context("body(...) returned None or Err")?,
            ))
            .context("unexpected None or Err")?;
        let resp = app
            .oneshot(req)
            .await
            .context("oneshot(...) returned None or Err")?;
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
            .context("unexpected None or Err")?;
        let text = String::from_utf8_lossy(&body);
        assert!(text.contains("[DONE]"));
        assert!(text.contains("\"ok\""));

        Ok(())
    }

    #[expect(
        clippy::panic_in_result_fn,
        reason = "assertions are intentional in tests"
    )]
    #[tokio::test]
    async fn e2e_logging_emits_request_and_chunk() -> Result<(), anyhow::Error> {
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
            .body(Body::from(
                serde_json::to_vec(&req_body).context("body(...) returned None or Err")?,
            ))
            .context("unexpected None or Err")?;
        let resp = app
            .oneshot(req)
            .await
            .context("oneshot(...) returned None or Err")?;
        let _ = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .context("unexpected None or Err")?;

        let mut saw_request = false;
        let mut saw_chunk = false;
        for _ in 0..10 {
            if let Ok(Ok(evt)) =
                tokio::time::timeout(std::time::Duration::from_millis(200), rx.recv()).await
            {
                match evt {
                    AnalysisEvent::Request { .. } => saw_request = true,
                    AnalysisEvent::ResponseChunk { .. } => {
                        saw_chunk = true;
                        break;
                    }
                    _ => {}
                }
            }
        }
        assert!(saw_request);
        assert!(saw_chunk);

        Ok(())
    }

    #[expect(
        clippy::panic_in_result_fn,
        reason = "assertions are intentional in tests"
    )]
    #[tokio::test]
    async fn vision_url_images_are_fetched_for_inline_providers() -> Result<(), anyhow::Error> {
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
                serde_json::to_vec(&anthropic_response).context("unexpected None or Err")?,
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

        let mut config = make_config(providers);
        config.delegated_fetch.enabled = true;
        let state = AppState::new_with_upstream(config, Arc::new(mock.clone()));
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
            .body(Body::from(
                serde_json::to_vec(&req_body).context("body(...) returned None or Err")?,
            ))
            .context("unexpected None or Err")?;
        let resp = app
            .oneshot(req)
            .await
            .context("oneshot(...) returned None or Err")?;
        assert_eq!(resp.status(), StatusCode::OK);

        // Ensure upstream request to Anthropic had base64 inline image data (AQID for [1,2,3]).
        let requests = mock.take_requests().await;
        assert!(requests
            .iter()
            .any(|r| r.method == Method::GET && r.url == "http://images.test/img.png"));

        let post = requests
            .iter()
            .find(|r| r.method == Method::POST)
            .context("find(...) returned None or Err")?;
        let sent: serde_json::Value =
            serde_json::from_slice(&post.body).context("unexpected None or Err")?;
        assert_eq!(sent["messages"][0]["content"][1]["type"], "image");
        assert_eq!(sent["messages"][0]["content"][1]["source"]["data"], "AQID");

        Ok(())
    }

    #[expect(
        clippy::panic_in_result_fn,
        reason = "assertions are intentional in tests"
    )]
    #[tokio::test]
    async fn e2e_non_streaming_transform_returns_openai_json() -> Result<(), anyhow::Error> {
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
                serde_json::to_vec(&anthropic_response).context("unexpected None or Err")?,
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
            .body(Body::from(
                serde_json::to_vec(&req_body).context("body(...) returned None or Err")?,
            ))
            .context("unexpected None or Err")?;
        let resp = app
            .oneshot(req)
            .await
            .context("oneshot(...) returned None or Err")?;
        assert_eq!(resp.status(), StatusCode::OK);
        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .context("unexpected None or Err")?;
        let json: serde_json::Value =
            serde_json::from_slice(&bytes).context("unexpected None or Err")?;
        assert_eq!(json["choices"][0]["message"]["content"], "ok");

        Ok(())
    }

    #[expect(
        clippy::panic_in_result_fn,
        reason = "assertions are intentional in tests"
    )]
    #[tokio::test]
    async fn e2e_bedrock_request_is_sigv4_signed() -> Result<(), anyhow::Error> {
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
            chunks: vec![Bytes::from(
                serde_json::to_vec(&bedrock_response).context("unexpected None or Err")?,
            )],
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
            .body(Body::from(
                serde_json::to_vec(&req_body).context("body(...) returned None or Err")?,
            ))
            .context("unexpected None or Err")?;
        let resp = app
            .oneshot(req)
            .await
            .context("oneshot(...) returned None or Err")?;
        assert_eq!(resp.status(), StatusCode::OK);

        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .context("unexpected None or Err")?;
        let json: serde_json::Value =
            serde_json::from_slice(&bytes).context("unexpected None or Err")?;
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
            .context("unexpected None or Err")?
            .to_str()
            .context("unexpected None or Err")?;
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

        let sent: serde_json::Value =
            serde_json::from_slice(&requests[0].body).context("unexpected None or Err")?;
        assert_eq!(sent["anthropic_version"], "bedrock-2023-05-31");
        assert!(sent.get("model").is_none());

        Ok(())
    }

    #[expect(
        clippy::panic_in_result_fn,
        reason = "assertions are intentional in tests"
    )]
    #[tokio::test]
    async fn e2e_bedrock_missing_credentials_returns_400() -> Result<(), anyhow::Error> {
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
            .body(Body::from(
                serde_json::to_vec(&req_body).context("body(...) returned None or Err")?,
            ))
            .context("unexpected None or Err")?;
        let resp = app
            .oneshot(req)
            .await
            .context("oneshot(...) returned None or Err")?;
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);

        let requests = mock.take_requests().await;
        assert!(requests.is_empty());

        Ok(())
    }

    #[expect(
        clippy::panic_in_result_fn,
        reason = "assertions are intentional in tests"
    )]
    #[tokio::test]
    async fn e2e_azure_base_url_env_is_resolved_and_deployment_path_is_added(
    ) -> Result<(), anyhow::Error> {
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
            chunks: vec![Bytes::from(
                serde_json::to_vec(&response_json).context("unexpected None or Err")?,
            )],
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
            .body(Body::from(
                serde_json::to_vec(&req_body).context("body(...) returned None or Err")?,
            ))
            .context("unexpected None or Err")?;
        let resp = app
            .oneshot(req)
            .await
            .context("oneshot(...) returned None or Err")?;
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
                .context("unexpected None or Err")?
                .to_str()
                .context("unexpected None or Err")?,
            "azure-key"
        );

        std::env::remove_var("AZURE_OPENAI_ENDPOINT");

        Ok(())
    }

    #[expect(
        clippy::panic_in_result_fn,
        reason = "assertions are intentional in tests"
    )]
    #[tokio::test]
    async fn stress_100_concurrent_non_streaming_requests() -> Result<(), anyhow::Error> {
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
                chunks: vec![Bytes::from(
                    serde_json::to_vec(&response_json).context("unexpected None or Err")?,
                )],
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
        .context("unexpected None or Err")?;

        let tasks = (0..100).map(|_| {
            let app = app.clone();
            let req_body = req_body.clone();
            async move {
                let req = http::Request::builder()
                    .method("POST")
                    .uri("/v1/chat/completions")
                    .header(http::header::CONTENT_TYPE, "application/json")
                    .body(Body::from(req_body))
                    .context("unexpected None or Err")?;
                let resp = app
                    .oneshot(req)
                    .await
                    .context("oneshot(...) returned None or Err")?;
                let status = resp.status();
                let _ = axum::body::to_bytes(resp.into_body(), usize::MAX)
                    .await
                    .context("unexpected None or Err")?;
                Ok::<_, anyhow::Error>(status)
            }
        });

        let statuses = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            futures::future::join_all(tasks),
        )
        .await
        .context("unexpected None or Err")?;

        assert!(statuses.iter().all(|s| matches!(s, Ok(StatusCode::OK))));
        let requests = mock.take_requests().await;
        assert_eq!(requests.len(), 100);

        Ok(())
    }

    #[expect(
        clippy::panic_in_result_fn,
        reason = "assertions are intentional in tests"
    )]
    #[tokio::test]
    async fn stress_50_concurrent_streaming_requests() -> Result<(), anyhow::Error> {
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
        .context("unexpected None or Err")?;

        let tasks = (0..50).map(|_| {
            let app = app.clone();
            let req_body = req_body.clone();
            async move {
                let req = http::Request::builder()
                    .method("POST")
                    .uri("/v1/chat/completions")
                    .header(http::header::CONTENT_TYPE, "application/json")
                    .body(Body::from(req_body))
                    .context("unexpected None or Err")?;
                let resp = app
                    .oneshot(req)
                    .await
                    .context("oneshot(...) returned None or Err")?;
                assert_eq!(resp.status(), StatusCode::OK);
                let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
                    .await
                    .context("unexpected None or Err")?;
                let text = String::from_utf8_lossy(&body);
                assert!(text.contains("data: [DONE]"));
                Ok::<(), anyhow::Error>(())
            }
        });

        tokio::time::timeout(
            std::time::Duration::from_secs(5),
            futures::future::join_all(tasks),
        )
        .await
        .context("unexpected None or Err")?
        .into_iter()
        .collect::<Result<Vec<_>, _>>()?;

        Ok(())
    }

    #[tokio::test]
    async fn stress_rapid_connect_disconnect_cycles() -> Result<(), anyhow::Error> {
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
        .context("unexpected None or Err")?;

        for _ in 0..200 {
            let req = http::Request::builder()
                .method("POST")
                .uri("/v1/chat/completions")
                .header(http::header::CONTENT_TYPE, "application/json")
                .body(Body::from(req_body.clone()))
                .context("unexpected None or Err")?;
            let resp = app
                .clone()
                .oneshot(req)
                .await
                .context("oneshot(...) returned None or Err")?;
            drop(resp);
        }

        Ok(())
    }

    #[expect(
        clippy::panic_in_result_fn,
        reason = "assertions are intentional in tests"
    )]
    #[tokio::test]
    async fn stress_injection_during_concurrent_streaming() -> Result<(), anyhow::Error> {
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
        .context("unexpected None or Err")?;

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
                    .context("unexpected None or Err")?;
                let resp = app
                    .oneshot(req)
                    .await
                    .context("oneshot(...) returned None or Err")?;
                assert_eq!(resp.status(), StatusCode::OK);
                let _ = axum::body::to_bytes(resp.into_body(), usize::MAX)
                    .await
                    .context("unexpected None or Err")?;
                Ok::<(), anyhow::Error>(())
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
                    .body(Body::from(
                        serde_json::to_vec(&inj).context("body(...) returned None or Err")?,
                    ))
                    .context("unexpected None or Err")?;
                let resp = app
                    .oneshot(req)
                    .await
                    .context("oneshot(...) returned None or Err")?;
                assert_eq!(resp.status(), StatusCode::OK);
                Ok::<(), anyhow::Error>(())
            }
        };

        tokio::time::timeout(std::time::Duration::from_secs(5), async move {
            let (stream_results, injected) =
                tokio::join!(futures::future::join_all(stream_tasks), inject_task,);
            injected?;
            stream_results
                .into_iter()
                .collect::<Result<Vec<_>, anyhow::Error>>()?;
            Ok::<(), anyhow::Error>(())
        })
        .await
        .context("unexpected None or Err")??;

        let delivered = tokio::time::timeout(std::time::Duration::from_millis(500), rx.recv())
            .await
            .context("unexpected None or Err")?
            .context("unexpected None or Err")?;
        assert_eq!(delivered[0].role, "system");
        assert_eq!(delivered[0].content, "Injected mid-stream");

        Ok(())
    }

    #[expect(
        clippy::panic_in_result_fn,
        reason = "assertions are intentional in tests"
    )]
    #[tokio::test]
    async fn inject_delivers_to_active_ws_sessions() -> Result<(), anyhow::Error> {
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
            .body(Body::from(
                serde_json::to_vec(&inj).context("body(...) returned None or Err")?,
            ))
            .context("unexpected None or Err")?;
        let resp = app
            .oneshot(req)
            .await
            .context("oneshot(...) returned None or Err")?;
        assert_eq!(resp.status(), StatusCode::OK);

        let delivered = tokio::time::timeout(std::time::Duration::from_millis(200), rx.recv())
            .await
            .context("unexpected None or Err")?
            .context("unexpected None or Err")?;
        assert_eq!(delivered[0].role, "system");
        assert_eq!(delivered[0].content, "Injected mid-stream");

        Ok(())
    }

    #[expect(
        clippy::panic_in_result_fn,
        reason = "assertions are intentional in tests"
    )]
    #[test]
    fn ws_upstream_url_builder_converts_http_to_ws() -> Result<(), anyhow::Error> {
        let url = build_ws_upstream_url(
            "https://api.example.com/v1",
            "/v1/realtime",
            Some("model=x"),
        )
        .map_err(|e| anyhow::anyhow!("build_ws_upstream_url: {e}"))?;
        assert_eq!(url, "wss://api.example.com/v1/realtime?model=x");

        Ok(())
    }

    #[expect(
        clippy::panic_in_result_fn,
        reason = "assertions are intentional in tests"
    )]
    #[tokio::test]
    async fn e2e_mock_provider_returns_synthetic_response() -> Result<(), anyhow::Error> {
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
            .body(Body::from(
                serde_json::to_vec(&req_body).context("body(...) returned None or Err")?,
            ))
            .context("unexpected None or Err")?;

        let resp = app
            .clone()
            .oneshot(req)
            .await
            .context("oneshot(...) returned None or Err")?;
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(
            resp.headers()
                .get("x-mock-response")
                .context("get(...) returned None or Err")?,
            "true"
        );

        let body_bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .context("unexpected None or Err")?;
        let body: serde_json::Value =
            serde_json::from_slice(&body_bytes).context("unexpected None or Err")?;

        assert_eq!(body["object"], "chat.completion");
        assert_eq!(body["model"], "mock-model");
        assert!(body["choices"][0]["message"]["content"]
            .as_str()
            .context("unexpected None or Err")?
            .contains("mock response"));

        // Verify no upstream requests were made
        let requests = mock.take_requests().await;
        assert_eq!(
            requests.len(),
            0,
            "Mock provider should not make upstream requests"
        );

        Ok(())
    }

    #[expect(
        clippy::panic_in_result_fn,
        reason = "assertions are intentional in tests"
    )]
    #[tokio::test]
    async fn e2e_mock_provider_returns_streaming_response() -> Result<(), anyhow::Error> {
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
            .body(Body::from(
                serde_json::to_vec(&req_body).context("body(...) returned None or Err")?,
            ))
            .context("unexpected None or Err")?;

        let resp = app
            .oneshot(req)
            .await
            .context("oneshot(...) returned None or Err")?;
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(
            resp.headers()
                .get("content-type")
                .context("get(...) returned None or Err")?,
            "text/event-stream"
        );

        let body_bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .context("unexpected None or Err")?;
        let body_str = String::from_utf8_lossy(&body_bytes);

        // Should contain SSE data lines with word-by-word streaming
        assert!(body_str.contains("data: "));
        assert!(body_str.contains("mock"));
        assert!(body_str.contains("response"));
        assert!(body_str.contains("[DONE]"));

        // Verify no upstream requests were made
        let requests = mock.take_requests().await;
        assert_eq!(requests.len(), 0);

        Ok(())
    }

    #[test]
    fn test_extract_openai_account_id_prefers_chatgpt_account_id() {
        use base64::Engine;
        let b64 = |v: &serde_json::Value| {
            base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(v.to_string())
        };
        let header = b64(&json!({"alg": "RS256"}));

        // ChatGPT subscription tokens carry chatgpt_account_id; org_id (the
        // platform org) must not win over it.
        let payload = b64(&json!({
            "https://api.openai.com/auth": {
                "chatgpt_account_id": "acct-chatgpt",
                "org_id": "org-platform"
            }
        }));
        let token = format!("{header}.{payload}.sig");
        assert_eq!(
            extract_openai_account_id(&token).as_deref(),
            Some("acct-chatgpt")
        );

        // org_id remains a fallback when nothing better exists
        let payload = b64(&json!({
            "https://api.openai.com/auth": {"org_id": "org-only"}
        }));
        let token = format!("{header}.{payload}.sig");
        assert_eq!(
            extract_openai_account_id(&token).as_deref(),
            Some("org-only")
        );
    }

    #[test]
    fn test_google_model_from_path() {
        assert_eq!(
            google_model_from_path("/v1/models/gemini-2.5-pro:streamGenerateContent"),
            Some("gemini-2.5-pro".to_string())
        );
        assert_eq!(
            google_model_from_path("/models/gemini-2.5-flash:generateContent"),
            Some("gemini-2.5-flash".to_string())
        );
        assert_eq!(google_model_from_path("/v1/chat/completions"), None);
    }

    #[expect(
        clippy::panic_in_result_fn,
        reason = "assertions are intentional in tests"
    )]
    #[test]
    fn test_inject_claude_code_identity_into_anthropic_body() -> Result<(), anyhow::Error> {
        // String system prompt gets prefixed
        let mut body = json!({"system": "You are helpful.", "messages": []});
        inject_claude_code_identity_into_anthropic_body(&mut body);
        let system = body["system"]
            .as_str()
            .context("as_str(...) returned None or Err")?;
        assert!(system.starts_with(CLAUDE_CODE_IDENTITY));
        assert!(system.contains("You are helpful."));

        // Block-array system gets a new leading text block
        let mut body = json!({
            "system": [{"type": "text", "text": "existing", "cache_control": {"type": "ephemeral"}}],
            "messages": []
        });
        inject_claude_code_identity_into_anthropic_body(&mut body);
        let blocks = body["system"]
            .as_array()
            .context("as_array(...) returned None or Err")?;
        assert_eq!(blocks.len(), 2);
        assert_eq!(
            blocks[0]["text"]
                .as_str()
                .context("as_str(...) returned None or Err")?,
            CLAUDE_CODE_IDENTITY
        );
        assert_eq!(
            blocks[1]["text"]
                .as_str()
                .context("as_str(...) returned None or Err")?,
            "existing"
        );

        // Missing system gets created
        let mut body = json!({"messages": []});
        inject_claude_code_identity_into_anthropic_body(&mut body);
        assert_eq!(
            body["system"]
                .as_str()
                .context("as_str(...) returned None or Err")?,
            CLAUDE_CODE_IDENTITY
        );

        Ok(())
    }

    fn make_tracker() -> UsageTracker {
        UsageTracker {
            key_hash: "h".to_string(),
            model: "m".to_string(),
            provider: "p".to_string(),
            input_tokens: Arc::new(std::sync::atomic::AtomicU32::new(0)),
            output_tokens: Arc::new(std::sync::atomic::AtomicU32::new(0)),
            cached_tokens: Arc::new(std::sync::atomic::AtomicU32::new(0)),
            cache_write_tokens: Arc::new(std::sync::atomic::AtomicU32::new(0)),
        }
    }

    #[test]
    fn test_extract_anthropic_usage_from_sse() {
        let tracker = make_tracker();
        extract_anthropic_usage(
            "event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"usage\":{\"input_tokens\":42,\"cache_read_input_tokens\":7}}}\n\n",
            &tracker,
        );
        extract_anthropic_usage(
            "event: message_delta\ndata: {\"type\":\"message_delta\",\"usage\":{\"output_tokens\":13}}\n\n",
            &tracker,
        );
        assert_eq!(
            tracker
                .input_tokens
                .load(std::sync::atomic::Ordering::SeqCst),
            42
        );
        assert_eq!(
            tracker
                .output_tokens
                .load(std::sync::atomic::Ordering::SeqCst),
            13
        );
        assert_eq!(
            tracker
                .cached_tokens
                .load(std::sync::atomic::Ordering::SeqCst),
            7
        );
    }

    #[test]
    fn test_extract_google_usage_from_json() {
        let tracker = make_tracker();
        extract_google_usage(
            "{\"candidates\":[],\"usageMetadata\":{\"promptTokenCount\":100,\"candidatesTokenCount\":20,\"thoughtsTokenCount\":5,\"cachedContentTokenCount\":11}}",
            &tracker,
        );
        assert_eq!(
            tracker
                .input_tokens
                .load(std::sync::atomic::Ordering::SeqCst),
            100
        );
        assert_eq!(
            tracker
                .output_tokens
                .load(std::sync::atomic::Ordering::SeqCst),
            25
        );
        assert_eq!(
            tracker
                .cached_tokens
                .load(std::sync::atomic::Ordering::SeqCst),
            11
        );
    }

    #[expect(
        clippy::panic_in_result_fn,
        reason = "assertions are intentional in tests"
    )]
    #[test]
    fn websocket_remote_images_and_policy_injection_are_sanitized() -> Result<(), anyhow::Error> {
        let input = json!([{"role":"user","content":[
            {"type":"input_text","text":"describe"},
            {"type":"input_image","image_url":"https://private.example/image"},
            {"type":"input_image","image_url":"data:image/png;base64,AQID"}
        ]}]);
        let policy = crate::policy::PolicyConfig {
            enabled: true,
            rules: vec![crate::policy::PolicyRule::SetField {
                provider: None,
                model: None,
                path: None,
                field: "input".into(),
                value: input.clone(),
            }],
        };
        let (tx, mut audit) = tokio::sync::broadcast::channel(32);
        let no_policy = crate::policy::PolicyConfig::default();
        for binary in [false, true] {
            for rewrite in [false, true] {
                let body = json!({"type":"response.create", "input":input});
                let message = if binary {
                    AxumWsMessage::Binary(
                        serde_json::to_vec(&body).context("unexpected None or Err")?,
                    )
                } else {
                    AxumWsMessage::Text(body.to_string())
                };
                let result = sanitize_codex_ws_message(
                    message,
                    if rewrite { &policy } else { &no_policy },
                    "openai-codex",
                    None,
                    &crate::delegated_fetch::DelegatedFetchPolicy::default(),
                    &tx,
                )
                .context("unexpected None or Err")?;
                let value: Value = match result {
                    TungsteniteMessage::Text(text) => {
                        assert!(!binary);
                        serde_json::from_str(&text).context("unexpected None or Err")?
                    }
                    TungsteniteMessage::Binary(bytes) => {
                        assert!(binary);
                        serde_json::from_slice(&bytes).context("unexpected None or Err")?
                    }
                    _ => return Err(anyhow::anyhow!("expected JSON frame")),
                };
                let content = value["input"][0]["content"]
                    .as_array()
                    .context("as_array(...) returned None or Err")?;
                assert_eq!(content.len(), 2);
                assert_eq!(content[0], input[0]["content"][0]);
                assert_eq!(content[1], input[0]["content"][2]);
            }
        }
        let mut stripped = 0;
        while let Ok(event) = audit.try_recv() {
            if let AnalysisEvent::DelegatedFetchStripped { target_host, .. } = event {
                assert_eq!(target_host.as_deref(), Some("private.example"));
                stripped += 1;
            }
        }
        assert_eq!(stripped, 6); // Four inbound strips, two policy reinjections.

        Ok(())
    }

    #[tokio::test]
    #[expect(
        clippy::panic_in_result_fn,
        reason = "Assertions verify admin auth, CAS and actual forwarded credentials; fixture errors propagate"
    )]
    async fn admin_rotation_changes_live_credentials_without_restart() -> Result<(), anyhow::Error>
    {
        let mock = MockUpstream::new(
            (0..3)
                .map(|_| ResponseSpec {
                    status: StatusCode::OK,
                    headers: HeaderMap::new(),
                    chunks: vec![Bytes::from_static(b"{}")],
                })
                .collect(),
        );
        let mut config = make_config(HashMap::new());
        config.keys.master_key = Some("synthetic-admin".into());
        let state = AppState::new_with_upstream(config, Arc::new(mock.clone()));
        let store = Arc::new(crate::provider_store::ProviderStore::in_memory().await?);
        state
            .provider_store
            .set(store.clone())
            .map_err(|_| anyhow::anyhow!("store already initialized"))?;
        let app = Router::new()
            .route("/:provider/v1/*path", any(provider_proxy_handler))
            .route(
                "/admin/providers",
                axum::routing::post(crate::api::upsert_provider_handler),
            )
            .route(
                "/admin/providers/:name/credentials",
                axum::routing::post(crate::api::credentials::rotate),
            )
            .route(
                "/admin/providers/:name/credentials/rollback",
                axum::routing::post(crate::api::credentials::rollback),
            )
            .with_state(state);
        let created = app.clone().oneshot(http::Request::builder().method("POST")
            .uri("/admin/providers").header("content-type", "application/json")
            .header("authorization", "Bearer synthetic-admin")
            .body(Body::from(json!({"name":"managed", "config":{"type":"openai-compatible", "base_url":"http://up/v1", "api_key":"synthetic-old"}}).to_string()))?).await?;
        assert_eq!(created.status(), StatusCode::OK);
        let metadata = axum::body::to_bytes(created.into_body(), usize::MAX).await?;
        assert!(!String::from_utf8_lossy(&metadata).contains("synthetic-old"));
        let metadata: Value = serde_json::from_slice(&metadata)?;
        let revision = metadata["updated_at"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("missing revision"))?;
        for (token, secret, expected) in [
            ("wrong-admin", "synthetic-new", StatusCode::UNAUTHORIZED),
            ("synthetic-admin", "", StatusCode::BAD_REQUEST),
            ("synthetic-admin", "synthetic-new", StatusCode::OK),
        ] {
            let response = app
                .clone()
                .oneshot(
                    http::Request::builder()
                        .method("POST")
                        .uri("/admin/providers/managed/credentials")
                        .header("content-type", "application/json")
                        .header("authorization", format!("Bearer {token}"))
                        .body(Body::from(
                            json!({"expected_revision":revision, "api_key":secret}).to_string(),
                        ))?,
                )
                .await?;
            assert_eq!(response.status(), expected);
            let bytes = axum::body::to_bytes(response.into_body(), usize::MAX).await?;
            assert!(!String::from_utf8_lossy(&bytes).contains("synthetic-new"));
            if expected != StatusCode::OK {
                assert_eq!(
                    store
                        .get_provider("managed")
                        .ok_or_else(|| anyhow::anyhow!("missing provider"))?
                        .config["api_key"],
                    "synthetic-old"
                );
            }
        }
        let conflict_response = app
            .clone()
            .oneshot(
                http::Request::builder()
                    .method("POST")
                    .uri("/admin/providers/managed/credentials")
                    .header("content-type", "application/json")
                    .header("authorization", "Bearer synthetic-admin")
                    .body(Body::from(
                        json!({"expected_revision":revision, "api_key":"synthetic-stale"})
                            .to_string(),
                    ))?,
            )
            .await?;
        assert_eq!(conflict_response.status(), StatusCode::CONFLICT);
        for expected_key in ["synthetic-new", "synthetic-old"] {
            let response = app
                .clone()
                .oneshot(
                    http::Request::builder()
                        .method("POST")
                        .uri("/managed/v1/responses")
                        .header("content-type", "application/json")
                        .body(Body::from(
                            json!({"model":"gpt-fixture", "input":"offline fixture"}).to_string(),
                        ))?,
                )
                .await?;
            assert_eq!(response.status(), StatusCode::OK);
            let requests = mock.take_requests().await;
            assert_eq!(requests.len(), 1);
            assert_eq!(
                requests[0]
                    .headers
                    .get("authorization")
                    .and_then(|header| header.to_str().ok()),
                Some(format!("Bearer {expected_key}").as_str())
            );
            if expected_key == "synthetic-new" {
                let current = store
                    .get_provider("managed")
                    .ok_or_else(|| anyhow::anyhow!("missing provider"))?
                    .updated_at
                    .to_rfc3339();
                let response = app
                    .clone()
                    .oneshot(
                        http::Request::builder()
                            .method("POST")
                            .uri("/admin/providers/managed/credentials/rollback")
                            .header("content-type", "application/json")
                            .header("authorization", "Bearer synthetic-admin")
                            .body(Body::from(json!({"expected_revision":current}).to_string()))?,
                    )
                    .await?;
                assert_eq!(response.status(), StatusCode::OK);
            }
        }
        Ok(())
    }

    #[test]
    #[expect(
        clippy::panic_in_result_fn,
        reason = "Assertions verify scope denial and frame preservation; serialization errors propagate"
    )]
    fn codex_ws_frames_enforce_model_scope_before_and_after_policy() -> Result<(), anyhow::Error> {
        let permissions = KeyPermissions {
            allowed_models: Some(["codex-ws".to_string(), "gpt-allowed".to_string()].into()),
            ..Default::default()
        };
        let (tx, _) = tokio::sync::broadcast::channel(16);
        for binary in [false, true] {
            for (model, rewrite, expected) in [
                (Some("gpt-allowed"), None, true),
                (Some("gpt-forbidden"), None, false),
                (None, None, false),
                (Some("gpt-allowed"), Some("gpt-forbidden"), false),
                (Some("gpt-forbidden"), Some("gpt-allowed"), false),
                (Some("gpt-allowed"), Some("gpt-allowed"), true),
            ] {
                let mut body = json!({"type":"response.create", "input":"fixture"});
                if let Some(model) = model {
                    body["model"] = json!(model);
                }
                let policy = crate::policy::PolicyConfig {
                    enabled: rewrite.is_some(),
                    rules: rewrite.map_or_else(Vec::new, |target| {
                        vec![crate::policy::PolicyRule::RewriteModel {
                            provider: None,
                            model: None,
                            to: target.to_string(),
                        }]
                    }),
                };
                let message = if binary {
                    AxumWsMessage::Binary(serde_json::to_vec(&body)?)
                } else {
                    AxumWsMessage::Text(body.to_string())
                };
                let result = sanitize_codex_ws_message(
                    message,
                    &policy,
                    "openai-codex",
                    Some(&permissions),
                    &crate::delegated_fetch::DelegatedFetchPolicy::default(),
                    &tx,
                );
                assert_eq!(
                    result.is_some(),
                    expected,
                    "binary={binary}, model={model:?}, rewrite={rewrite:?}"
                );
                if let Some(frame) = result {
                    let sent: Value = match frame {
                        TungsteniteMessage::Text(text) => serde_json::from_str(&text)?,
                        TungsteniteMessage::Binary(bytes) => serde_json::from_slice(&bytes)?,
                        _ => return Err(anyhow::anyhow!("unexpected frame type")),
                    };
                    assert_eq!(sent["model"], "gpt-allowed");
                    assert_eq!(sent["input"], "fixture");
                }
            }
        }
        Ok(())
    }

    #[expect(
        clippy::panic_in_result_fn,
        reason = "assertions are intentional in tests"
    )]
    #[tokio::test]
    async fn policy_rewrite_cannot_expand_virtual_key_model_scope() -> Result<(), anyhow::Error> {
        for (target, allowed) in [("gpt-forbidden", false), ("gpt-allowed", true)] {
            let mock = MockUpstream::new(vec![ResponseSpec {
                status: StatusCode::OK,
                headers: HeaderMap::new(),
                chunks: vec![Bytes::from_static(b"{}")],
            }]);
            let providers = HashMap::from([(
                "test".into(),
                ProviderConfig {
                    type_: "openai-compatible".into(),
                    base_url: "http://up/v1".into(),
                    api_key: "synthetic-secret".into(),
                    ..Default::default()
                },
            )]);
            let mut config = make_config(providers);
            config.policy = crate::policy::PolicyConfig {
                enabled: true,
                rules: vec![crate::policy::PolicyRule::RewriteModel {
                    provider: None,
                    model: None,
                    to: target.into(),
                }],
            };
            let state = AppState::new_with_upstream(config, Arc::new(mock.clone()));
            let store = Arc::new(KeyStore::in_memory().await?);
            state
                .key_validator
                .set(Arc::new(KeyValidator::new(
                    store.clone(),
                    Arc::new(RateLimiter::new()),
                )))
                .map_err(|_| anyhow::anyhow!("validator already initialized"))?;
            let key = store
                .create_key(CreateKeyRequest {
                    permissions: KeyPermissions {
                        allowed_models: Some(["gpt-allowed".to_string()].into()),
                        ..Default::default()
                    },
                    ..Default::default()
                })
                .await?
                .key;
            let app = Router::new()
                .route("/:provider/v1/*path", any(provider_proxy_handler))
                .with_state(state);
            let response = app
                .oneshot(
                    http::Request::builder()
                        .method("POST")
                        .uri("/test/v1/responses")
                        .header("content-type", "application/json")
                        .header("authorization", format!("Bearer {key}"))
                        .body(Body::from(
                            json!({"model":"gpt-allowed", "input":"fixture"}).to_string(),
                        ))?,
                )
                .await?;
            let requests = mock.take_requests().await;
            if allowed {
                assert_eq!(response.status(), StatusCode::OK);
                assert_eq!(requests.len(), 1);
                let sent: Value = serde_json::from_slice(&requests[0].body)?;
                assert_eq!(sent["model"], target);
            } else {
                assert_eq!(response.status(), StatusCode::FORBIDDEN);
                assert!(
                    requests.is_empty(),
                    "Forbidden rewrite must not reach upstream"
                );
            }
        }
        Ok(())
    }

    #[tokio::test]
    #[expect(
        clippy::panic_in_result_fn,
        reason = "Assertions verify native media preservation while fixture failures propagate"
    )]
    async fn e2e_remote_media_policy_preserves_inline_native_shapes() -> Result<(), anyhow::Error> {
        use crate::keys::{CreateKeyRequest, KeyValidator, RateLimiter};
        let remote = "https://private.example/media";
        for (provider, path, body, parts_path) in [
            (
                "openai-compatible",
                "chat/completions",
                json!({"model":"gpt-5","messages":[{"role":"user","content":[
                    {"type":"text","text":"describe"},
                    {"type":"image_url","image_url":{"url":remote}},
                    {"type":"image_url","image_url":{"url":"data:image/png;base64,AQID"}}
                ]}]}),
                "/messages/0/content",
            ),
            (
                "openai-compatible",
                "responses",
                json!({"model":"gpt-5","input":[{"role":"user","content":[
                    {"type":"input_text","text":"describe"},
                    {"type":"input_image","image_url":remote},
                    {"type":"input_image","image_url":"data:image/png;base64,AQID"}
                ]}]}),
                "/input/0/content",
            ),
            (
                "anthropic",
                "messages",
                json!({"model":"claude-opus-4-6","max_tokens":32,"messages":[{"role":"user","content":[
                    {"type":"text","text":"describe"},
                    {"type":"image","source":{"type":"url","url":remote}},
                    {"type":"image","source":{"type":"base64","media_type":"image/png","data":"AQID"}}
                ]}]}),
                "/messages/0/content",
            ),
            (
                "google",
                "models/gemini-2.5-pro:generateContent",
                json!({"contents":[{"role":"user","parts":[
                    {"text":"describe"},
                    {"fileData":{"mimeType":"image/png","fileUri":remote}},
                    {"inlineData":{"mimeType":"image/png","data":"AQID"}}
                ]}]}),
                "/contents/0/parts",
            ),
        ] {
            let mock = MockUpstream::new(
                (0..2)
                    .map(|_| ResponseSpec {
                        status: StatusCode::OK,
                        headers: HeaderMap::new(),
                        chunks: vec![Bytes::from_static(b"{}")],
                    })
                    .collect(),
            );
            let providers = HashMap::from([(
                "test".into(),
                ProviderConfig {
                    type_: provider.into(),
                    base_url: "http://up/v1".into(),
                    api_key: "secret".into(),
                    ..Default::default()
                },
            )]);
            let mut config = make_config(providers);
            // A tenant must not inherit the permissive process fallback or
            // media injected by an operator body-mutating policy.
            config.delegated_fetch.enabled = true;
            let field = parts_path
                .split('/')
                .nth(1)
                .context("nth(...) returned None or Err")?;
            config.policy = crate::policy::PolicyConfig {
                enabled: true,
                rules: vec![crate::policy::PolicyRule::SetField {
                    provider: None,
                    model: None,
                    path: None,
                    field: field.into(),
                    value: body[field].clone(),
                }],
            };
            let state = AppState::new_with_upstream(config, Arc::new(mock.clone()));
            let store = Arc::new(
                KeyStore::in_memory()
                    .await
                    .context("unexpected None or Err")?,
            );
            state
                .key_validator
                .set(Arc::new(KeyValidator::new(
                    store.clone(),
                    Arc::new(RateLimiter::new()),
                )))
                .ok()
                .context("unexpected None or Err")?;
            let mut audit = state.analysis_tx.subscribe();
            let app = Router::new()
                .route("/:provider/v1/*path", any(provider_proxy_handler))
                .with_state(state);
            for allowed in [false, true] {
                let key = store
                    .create_key(CreateKeyRequest {
                        metadata: json!({"delegated_fetch":{"remote_content":allowed}}),
                        ..Default::default()
                    })
                    .await
                    .context("unexpected None or Err")?
                    .key;
                let response = app
                    .clone()
                    .oneshot(
                        http::Request::builder()
                            .method("POST")
                            .uri(format!("/test/v1/{path}"))
                            .header("content-type", "application/json")
                            .header("authorization", format!("Bearer {key}"))
                            .body(Body::from(body.to_string()))
                            .context("unexpected None or Err")?,
                    )
                    .await
                    .context("unexpected None or Err")?;
                assert_eq!(response.status(), StatusCode::OK, "{provider}: {allowed}");
                let requests = mock.take_requests().await;
                assert_eq!(requests.len(), 1, "No gateway fetch may occur");
                assert_eq!(requests[0].method, Method::POST);
                let sent: Value =
                    serde_json::from_slice(&requests[0].body).context("unexpected None or Err")?;
                let mut expected = body.clone();
                if !allowed {
                    expected
                        .pointer_mut(parts_path)
                        .context("unexpected None or Err")?
                        .as_array_mut()
                        .context("unexpected None or Err")?
                        .remove(1);
                }
                assert_eq!(sent, expected, "{provider}: {allowed}");
            }
            let mut stripped = false;
            while let Ok(event) = audit.try_recv() {
                if let AnalysisEvent::DelegatedFetchStripped { target_host, .. } = event {
                    stripped |= target_host.as_deref() == Some("private.example");
                }
            }
            assert!(stripped, "{provider}: missing denial audit");
        }

        Ok(())
    }

    #[expect(
        clippy::panic_in_result_fn,
        reason = "assertions are intentional in tests"
    )]
    #[tokio::test]
    async fn e2e_native_anthropic_messages_passthrough() -> Result<(), anyhow::Error> {
        // pi (api=anthropic-messages) posts a native Anthropic body to
        // {eavs}/{provider}/v1 + the SDK's /v1/messages. It must be relayed
        // untouched (system, tools, thinking intact) to {base}/v1/messages
        // and the anthropic-format SSE response returned unmodified.
        let sse = "event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"usage\":{\"input_tokens\":3}}}\n\nevent: message_stop\ndata: {\"type\":\"message_stop\"}\n\n";
        let mut headers = HeaderMap::new();
        headers.insert(
            "content-type",
            "text/event-stream"
                .parse()
                .context("parse(...) returned None or Err")?,
        );
        let mock = MockUpstream::new(vec![ResponseSpec {
            status: StatusCode::OK,
            headers,
            chunks: vec![Bytes::from(sse)],
        }]);

        let mut providers = HashMap::new();
        providers.insert(
            "anthropic".to_string(),
            ProviderConfig {
                type_: "anthropic".to_string(),
                api_key: "sk-test".to_string(),
                ..Default::default()
            },
        );

        let state = AppState::new_with_upstream(make_config(providers), Arc::new(mock.clone()));
        let app = Router::new()
            .route("/:provider/v1/*path", any(provider_proxy_handler))
            .with_state(state);

        let req_body = json!({
            "model": "claude-opus-4-6",
            "max_tokens": 1024,
            "stream": true,
            "system": [{"type": "text", "text": "be brief"}],
            "messages": [{"role": "user", "content": [{"type": "text", "text": "hi"}]}],
            "tools": [{"name": "get_weather", "description": "d", "input_schema": {"type": "object"}}]
        });
        let req = http::Request::builder()
            .method("POST")
            .uri("/anthropic/v1/v1/messages")
            .header(http::header::CONTENT_TYPE, "application/json")
            .header("anthropic-beta", "fine-grained-tool-streaming-2025-05-14")
            .body(Body::from(
                serde_json::to_vec(&req_body).context("body(...) returned None or Err")?,
            ))
            .context("unexpected None or Err")?;

        let resp = app
            .oneshot(req)
            .await
            .context("oneshot(...) returned None or Err")?;
        assert_eq!(resp.status(), StatusCode::OK);
        let body_bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .context("unexpected None or Err")?;
        // Response relayed as-is in Anthropic format
        assert_eq!(String::from_utf8_lossy(&body_bytes), sse);

        let requests = mock.take_requests().await;
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].url, "https://api.anthropic.com/v1/messages");
        let sent: Value =
            serde_json::from_slice(&requests[0].body).context("unexpected None or Err")?;
        assert_eq!(sent, req_body);
        // Auth + client beta features forwarded
        assert_eq!(
            requests[0]
                .headers
                .get("x-api-key")
                .context("get(...) returned None or Err")?,
            "sk-test"
        );
        let beta = requests[0]
            .headers
            .get("anthropic-beta")
            .context("get(...) returned None or Err")?;
        assert!(beta
            .to_str()
            .context("unexpected None or Err")?
            .contains("fine-grained-tool-streaming-2025-05-14"));

        Ok(())
    }

    #[expect(
        clippy::panic_in_result_fn,
        reason = "assertions are intentional in tests"
    )]
    #[tokio::test]
    async fn e2e_native_google_generate_content_passthrough() -> Result<(), anyhow::Error> {
        // pi (api=google-generative-ai) posts native Google bodies to
        // {eavs}/{provider}/v1/models/{model}:streamGenerateContent — the /v1
        // routing artifact must be dropped so the upstream sees
        // {base}/models/{model}:streamGenerateContent.
        let mock = MockUpstream::new(vec![ResponseSpec {
            status: StatusCode::OK,
            headers: HeaderMap::new(),
            chunks: vec![Bytes::from(
                "{\"candidates\":[],\"usageMetadata\":{\"promptTokenCount\":1}}",
            )],
        }]);

        let mut providers = HashMap::new();
        providers.insert(
            "google".to_string(),
            ProviderConfig {
                type_: "google".to_string(),
                api_key: "g-test".to_string(),
                ..Default::default()
            },
        );

        let state = AppState::new_with_upstream(make_config(providers), Arc::new(mock.clone()));
        let app = Router::new()
            .route("/:provider/v1/*path", any(provider_proxy_handler))
            .with_state(state);

        let req_body = json!({
            "contents": [{"role": "user", "parts": [{"text": "hi"}]}]
        });
        let req = http::Request::builder()
            .method("POST")
            .uri("/google/v1/models/gemini-2.5-pro:streamGenerateContent?alt=sse")
            .header(http::header::CONTENT_TYPE, "application/json")
            .body(Body::from(
                serde_json::to_vec(&req_body).context("body(...) returned None or Err")?,
            ))
            .context("unexpected None or Err")?;

        let resp = app
            .oneshot(req)
            .await
            .context("oneshot(...) returned None or Err")?;
        assert_eq!(resp.status(), StatusCode::OK);

        let requests = mock.take_requests().await;
        assert_eq!(requests.len(), 1);
        assert_eq!(
            requests[0].url,
            "https://generativelanguage.googleapis.com/v1beta/models/gemini-2.5-pro:streamGenerateContent?alt=sse"
        );
        let sent: Value =
            serde_json::from_slice(&requests[0].body).context("unexpected None or Err")?;
        assert_eq!(sent, req_body);

        Ok(())
    }

    #[expect(
        clippy::panic_in_result_fn,
        reason = "assertions are intentional in tests"
    )]
    #[tokio::test]
    async fn e2e_codex_sse_post_with_zstd_body() -> Result<(), anyhow::Error> {
        // pi's Codex SSE fallback POSTs a zstd-compressed body to
        // {eavs}/{provider}/v1/codex/responses; it must be routed (not 405),
        // decompressed, and forwarded to {base}/codex/responses.
        let mock = MockUpstream::new(vec![ResponseSpec {
            status: StatusCode::OK,
            headers: HeaderMap::new(),
            chunks: vec![Bytes::from("data: {\"type\":\"response.completed\"}\n\n")],
        }]);

        let mut providers = HashMap::new();
        providers.insert(
            "codex".to_string(),
            ProviderConfig {
                type_: "openai-codex".to_string(),
                api_key: "codex-test".to_string(),
                ..Default::default()
            },
        );

        let state = AppState::new_with_upstream(make_config(providers), Arc::new(mock.clone()));
        let app = Router::new()
            .route(
                "/:provider/v1/codex/responses",
                post(provider_codex_sse_handler),
            )
            .route("/:provider/v1/*path", any(provider_proxy_handler))
            .with_state(state);

        let req_body = json!({
            "model": "gpt-5.2-codex",
            "input": [{"role": "user", "content": [{"type": "input_text", "text": "hi"}]}],
            "stream": true
        });
        let compressed = zstd::stream::encode_all(
            serde_json::to_vec(&req_body)
                .context("unexpected None or Err")?
                .as_slice(),
            0,
        )
        .context("as_slice(...) returned None or Err")?;
        let req = http::Request::builder()
            .method("POST")
            .uri("/codex/v1/codex/responses")
            .header(http::header::CONTENT_TYPE, "application/json")
            .header(http::header::CONTENT_ENCODING, "zstd")
            .body(Body::from(compressed))
            .context("unexpected None or Err")?;

        let resp = app
            .oneshot(req)
            .await
            .context("oneshot(...) returned None or Err")?;
        assert_eq!(resp.status(), StatusCode::OK);

        let requests = mock.take_requests().await;
        assert_eq!(requests.len(), 1);
        assert_eq!(
            requests[0].url,
            "https://chatgpt.com/backend-api/codex/responses"
        );
        let sent: Value =
            serde_json::from_slice(&requests[0].body).context("unexpected None or Err")?;
        assert_eq!(sent, req_body);

        Ok(())
    }

    /// Helper: build an `AppState` with an in-memory key store wired into a
    /// validator and cost calculator, so usage recording debits real state.
    async fn state_with_key_store() -> Result<(AppState, Arc<KeyStore>), anyhow::Error> {
        let mut providers = HashMap::new();
        providers.insert(
            "default".to_string(),
            ProviderConfig {
                type_: "openai".to_string(),
                base_url: "http://upstream/v1".to_string(),
                ..Default::default()
            },
        );
        let state = AppState::new_with_upstream(
            make_config(providers),
            Arc::new(MockUpstream::new(vec![])),
        );
        let store = Arc::new(
            KeyStore::in_memory()
                .await
                .context("unexpected None or Err")?,
        );
        let validator = KeyValidator::new(store.clone(), Arc::new(RateLimiter::new()));
        state
            .key_validator
            .set(Arc::new(validator))
            .ok()
            .context("set(...) returned None or Err")?;
        state
            .cost_calculator
            .set(CostCalculator::new(state.pricing.clone()))
            .ok()
            .context("unexpected None or Err")?;
        Ok((state, store))
    }

    #[expect(
        clippy::panic_in_result_fn,
        reason = "assertions are intentional in tests"
    )]
    #[tokio::test]
    async fn fal_cannot_use_realtime_websocket_route() -> Result<(), anyhow::Error> {
        let mut providers = HashMap::new();
        providers.insert(
            "fal".into(),
            ProviderConfig {
                type_: "fal".into(),
                base_url: "http://up".into(),
                ..Default::default()
            },
        );
        let state = AppState::new_with_upstream(
            make_config(providers),
            Arc::new(MockUpstream::new(vec![])),
        );
        let app = Router::new()
            .route(
                "/:provider/v1/realtime",
                axum::routing::get(provider_ws_proxy_handler),
            )
            .with_state(state);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .context("unexpected None or Err")?;
        let addr = listener
            .local_addr()
            .context("local_addr(...) returned None or Err")?;
        let server =
            tokio::spawn(async move { axum::serve(listener, app).await.context("server failed") });
        let connect_result =
            tokio_tungstenite::connect_async(format!("ws://{addr}/fal/v1/realtime")).await;
        let Err(err) = connect_result else {
            return Err(anyhow::anyhow!("expected connect error, got ok"));
        };
        match err {
            tokio_tungstenite::tungstenite::Error::Http(response) => {
                assert_eq!(response.status(), StatusCode::BAD_REQUEST);
            }
            other => return Err(anyhow::anyhow!("unexpected error: {other}")),
        }
        server.abort();

        Ok(())
    }

    #[expect(
        clippy::panic_in_result_fn,
        reason = "assertions are intentional in tests"
    )]
    #[expect(
        clippy::result_large_err,
        reason = "tungstenite handshake callback error type is fixed by the library"
    )]
    #[tokio::test]
    async fn elevenlabs_realtime_relay_preserves_binary_frames_and_provider_auth(
    ) -> Result<(), anyhow::Error> {
        let upstream_listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .context("unexpected None or Err")?;
        let upstream_addr = upstream_listener
            .local_addr()
            .context("local_addr(...) returned None or Err")?;
        let upstream_task = tokio::spawn(async move {
            let (tcp, _) = upstream_listener
                .accept()
                .await
                .context("accept(...) returned None or Err")?;
            let mut socket = tokio_tungstenite::accept_hdr_async(tcp,
                |request: &tokio_tungstenite::tungstenite::handshake::server::Request,
                 response: tokio_tungstenite::tungstenite::handshake::server::Response| {
                    assert_eq!(request.uri().path(), "/v1/speech-to-text/realtime");
                    let key = request
                        .headers()
                        .get("xi-api-key")
                        .and_then(|v| v.to_str().ok())
                        .unwrap_or("missing");
                    assert_eq!(key, "upstream-secret");
                    Ok(response)
                }).await.context("unexpected None or Err")?;
            let frame = socket
                .next()
                .await
                .context("next(...) returned None or Err")?
                .context("unwrap(...) returned None or Err")?;
            assert_eq!(frame.into_data(), b"\xff\x00");
            socket
                .send(TungsteniteMessage::Binary(vec![0xfe, 0x01]))
                .await
                .context("unexpected None or Err")?;
            socket
                .close(None)
                .await
                .context("close(...) returned None or Err")?;
            Ok::<(), anyhow::Error>(())
        });
        let mut providers = HashMap::new();
        providers.insert(
            "elevenlabs".into(),
            ProviderConfig {
                type_: "elevenlabs".into(),
                base_url: format!("http://{upstream_addr}/v1"),
                api_key: "upstream-secret".into(),
                ..Default::default()
            },
        );
        let state = AppState::new_with_upstream(
            make_config(providers),
            Arc::new(MockUpstream::new(vec![])),
        );
        let app = Router::new()
            .route(
                "/:provider/v1/speech-to-text/realtime",
                axum::routing::get(provider_ws_proxy_handler),
            )
            .with_state(state);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .context("unexpected None or Err")?;
        let addr = listener
            .local_addr()
            .context("local_addr(...) returned None or Err")?;
        let server =
            tokio::spawn(async move { axum::serve(listener, app).await.context("server failed") });
        let exchange = async {
            let (mut socket, _) = tokio_tungstenite::connect_async(format!(
                "ws://{addr}/elevenlabs/v1/speech-to-text/realtime?model_id=scribe_v2_realtime"
            ))
            .await
            .context("unexpected None or Err")?;
            socket
                .send(TungsteniteMessage::Binary(vec![0xff, 0x00]))
                .await
                .context("unexpected None or Err")?;
            let frame = socket
                .next()
                .await
                .context("next(...) returned None or Err")?
                .context("unwrap(...) returned None or Err")?;
            assert_eq!(frame.into_data(), b"\xfe\x01");
            upstream_task.await.context("unexpected None or Err")??;
            Ok::<(), anyhow::Error>(())
        };
        tokio::time::timeout(std::time::Duration::from_secs(5), exchange)
            .await
            .context("unexpected None or Err")?
            .context("unexpected None or Err")?;
        server.abort();

        Ok(())
    }

    #[expect(
        clippy::panic_in_result_fn,
        reason = "assertions are intentional in tests"
    )]
    #[tokio::test]
    async fn virtual_key_cannot_start_unowned_native_video_job() -> Result<(), anyhow::Error> {
        let (state, store) = state_with_key_store().await?;
        let key = store
            .create_key(CreateKeyRequest::default())
            .await
            .context("unexpected None or Err")?
            .key;
        let app = Router::new()
            .route("/v1/*path", any(proxy_handler))
            .with_state(state);
        let response = app
            .oneshot(
                http::Request::builder()
                    .method("POST")
                    .uri("/v1/videos")
                    .header("authorization", format!("Bearer {key}"))
                    .header(http::header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        json!({"model":"sora-2","prompt":"a bird"}).to_string(),
                    ))
                    .context("unexpected None or Err")?,
            )
            .await
            .context("unexpected None or Err")?;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);

        Ok(())
    }

    #[expect(
        clippy::panic_in_result_fn,
        reason = "assertions are intentional in tests"
    )]
    #[tokio::test]
    async fn ws_realtime_model_scope_is_enforced_on_actual_model() -> Result<(), anyhow::Error> {
        let store = Arc::new(
            KeyStore::in_memory()
                .await
                .context("unexpected None or Err")?,
        );
        let validator = KeyValidator::new(store.clone(), Arc::new(RateLimiter::new()));

        let permissions = KeyPermissions {
            allowed_models: Some(["gpt-3.5-*".to_string()].into()),
            ..Default::default()
        };
        let resp = store
            .create_key(CreateKeyRequest {
                name: Some("limited".to_string()),
                permissions,
                metadata: json!(null),
                ..Default::default()
            })
            .await
            .context("unexpected None or Err")?;

        // The Realtime connection URI carries the actual requested model.
        let uri: http::Uri = "/v1/realtime?model=gpt-4o-realtime-preview"
            .parse()
            .context("unexpected None or Err")?;
        let model = extract_realtime_model(&uri);
        assert_eq!(model, "gpt-4o-realtime-preview");

        // Validating the *actual* model rejects a key that may not use it.
        let validate_result = validator
            .validate(&resp.key, &model, "openai", Some(100))
            .await;
        let Err(err) = validate_result else {
            return Err(anyhow::anyhow!("expected validation error, got ok"));
        };
        assert_eq!(err.error_code(), "model_not_allowed");

        // An allowlisted model requested the same way passes.
        let ok_uri: http::Uri = "/v1/realtime?model=gpt-3.5-turbo"
            .parse()
            .context("parse(...) returned None or Err")?;
        let ok_model = extract_realtime_model(&ok_uri);
        assert!(validator
            .validate(&resp.key, &ok_model, "openai", Some(100))
            .await
            .is_ok());

        Ok(())
    }

    #[expect(
        clippy::panic_in_result_fn,
        reason = "assertions are intentional in tests"
    )]
    #[tokio::test]
    async fn ws_realtime_session_records_usage_and_debits_budget() -> Result<(), anyhow::Error> {
        let (state, store) = state_with_key_store().await?;
        let permissions = KeyPermissions {
            max_budget_usd: Some(0.0001),
            ..Default::default()
        };
        let resp = store
            .create_key(CreateKeyRequest {
                name: Some("wskey".to_string()),
                permissions,
                metadata: json!(null),
                ..Default::default()
            })
            .await
            .context("unexpected None or Err")?;
        let hash = resp.key_hash.clone();

        // Simulate a Realtime session whose usage was parsed from response.done
        // and then debited once the session ended.
        let tracker = UsageTracker {
            key_hash: hash.clone(),
            model: "gpt-4o-realtime-preview".to_string(),
            provider: "default".to_string(),
            input_tokens: Arc::new(AtomicU32::new(100)),
            output_tokens: Arc::new(AtomicU32::new(100)),
            cached_tokens: Arc::new(AtomicU32::new(0)),
            cache_write_tokens: Arc::new(AtomicU32::new(0)),
        };

        record_usage_from_tracker(&state, &tracker).await;
        let key = store
            .get_by_hash(&hash)
            .context("get_by_hash(...) returned None or Err")?;
        assert!(
            key.usage.window_spend_usd > 0.0,
            "Realtime session usage must debit the virtual key's budget"
        );

        // A small-budget key is eventually budget-limited by Realtime traffic.
        for _ in 0..8 {
            record_usage_from_tracker(&state, &tracker).await;
        }
        let key = store
            .get_by_hash(&hash)
            .context("get_by_hash(...) returned None or Err")?;
        assert!(
            key.is_over_budget(),
            "Realtime usage must eventually trip a small-budget key"
        );

        Ok(())
    }

    #[expect(
        clippy::panic_in_result_fn,
        reason = "assertions are intentional in tests"
    )]
    #[tokio::test]
    async fn streaming_usage_is_recorded_at_terminal_not_construction() -> Result<(), anyhow::Error>
    {
        let (state, store) = state_with_key_store().await?;
        let resp = store
            .create_key(CreateKeyRequest {
                name: Some("stream".to_string()),
                permissions: KeyPermissions::default(),
                metadata: json!(null),
                ..Default::default()
            })
            .await
            .context("unexpected None or Err")?;
        let hash = resp.key_hash.clone();

        let tracker = UsageTracker {
            key_hash: hash.clone(),
            model: "gpt-4o".to_string(),
            provider: "default".to_string(),
            input_tokens: Arc::new(AtomicU32::new(0)),
            output_tokens: Arc::new(AtomicU32::new(0)),
            cached_tokens: Arc::new(AtomicU32::new(0)),
            cache_write_tokens: Arc::new(AtomicU32::new(0)),
        };

        // Args atomics are populated lazily as the stream is consumed (mimicking
        // extract_openai_usage). At construction time they are still 0, so a
        // construction-time debit would read 0 and never bill.
        let parse_tracker = tracker.clone();
        let stream = stream::iter(vec![Ok::<_, std::io::Error>(Bytes::from_static(
            b"data: {\"usage\":{\"prompt_tokens\":10,\"completion_tokens\":20}}\n\n",
        ))])
        .map(move |chunk: Result<Bytes, std::io::Error>| {
            if let Ok(ref bytes) = &chunk {
                let text = String::from_utf8_lossy(bytes).to_string();
                if let Some(body) = text.strip_prefix("data: ") {
                    if let Ok(json) = serde_json::from_str::<Value>(body) {
                        if let Some(input) = json
                            .pointer("/usage/prompt_tokens")
                            .and_then(serde_json::Value::as_u64)
                        {
                            parse_tracker
                                .input_tokens
                                .store(input as u32, std::sync::atomic::Ordering::SeqCst);
                        }
                        if let Some(output) = json
                            .pointer("/usage/completion_tokens")
                            .and_then(serde_json::Value::as_u64)
                        {
                            parse_tracker
                                .output_tokens
                                .store(output as u32, std::sync::atomic::Ordering::SeqCst);
                        }
                    }
                }
            }
            chunk
        });

        let wrapped = with_terminal_usage_record(stream, state.clone(), tracker);
        let collected: Vec<Result<Bytes, std::io::Error>> = wrapped.collect().await;
        assert_eq!(collected.len(), 1);

        // The debit is issued from the stream terminal; wait for the async task.
        for _ in 0..50 {
            let key = store
                .get_by_hash(&hash)
                .context("get_by_hash(...) returned None or Err")?;
            if key.usage.window_spend_usd > 0.0 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        let key = store
            .get_by_hash(&hash)
            .context("get_by_hash(...) returned None or Err")?;
        assert!(
            key.usage.window_spend_usd > 0.0,
            "Streaming usage must be debited after the stream terminal, not 0"
        );

        Ok(())
    }

    #[test]
    fn conversation_state_is_namespaced_by_key_and_operator_preserved() {
        let store = crate::state::ConversationStore::new(StateConfig {
            enabled: true,
            capture_all: false,
            ttl_secs: 0,
            cleanup_interval_secs: 0,
            max_conversations: 100,
        });

        let key_a = "hashA";
        let key_b = "hashB";
        let conv = "shared-conv";
        let ns_a = conversation_namespace(conv, Some(key_a));
        let ns_b = conversation_namespace(conv, Some(key_b));
        assert_eq!(ns_a, "key:hashA:shared-conv");
        assert_ne!(ns_a, ns_b);

        // Operator injects into tenant A's conversation.
        store.add_injections(
            &ns_a,
            vec![Injection {
                role: "system".to_string(),
                content: "tenant A secret".to_string(),
            }],
        );
        // Tenant A consumes its own injection.
        assert_eq!(store.take_injections(&ns_a).len(), 1);
        // Tenant B, using the same client-controlled id, cannot steal it.
        assert_eq!(store.take_injections(&ns_b).len(), 0);

        // Reinject into A; B still cannot consume it.
        store.add_injections(
            &ns_a,
            vec![Injection {
                role: "system".to_string(),
                content: "again".to_string(),
            }],
        );
        assert_eq!(store.take_injections(&ns_b).len(), 0);
        assert_eq!(store.take_injections(&ns_a).len(), 1);

        // Operator's own (non-multi-tenant) default is preserved.
        assert_eq!(conversation_namespace(conv, None), conv);
    }

    #[expect(
        clippy::panic_in_result_fn,
        reason = "assertions are intentional in tests"
    )]
    #[test]
    fn ws_session_injections_are_isolated_per_key() -> Result<(), anyhow::Error> {
        let mgr = crate::state::WsSessionManager::new();
        let conv = "shared";
        // Tenant A opens a Realtime session on its namespaced conversation.
        let (_tok_a, mut rx_a) = mgr.register(&conversation_namespace(conv, Some("hashA")));

        // Injecting into tenant A's namespace is delivered to A's session.
        assert!(mgr.deliver_injections(
            &conversation_namespace(conv, Some("hashA")),
            vec![Injection {
                role: "system".to_string(),
                content: "mid-stream secret".to_string(),
            }]
        ));
        // Tenant B has no session on its own namespace, so nothing is received.
        assert!(!mgr.deliver_injections(
            &conversation_namespace(conv, Some("hashB")),
            vec![Injection {
                role: "system".to_string(),
                content: "leak".to_string(),
            }]
        ));

        let delivered = rx_a
            .try_recv()
            .context("try_recv(...) returned None or Err")?;
        assert_eq!(delivered[0].content, "mid-stream secret");

        Ok(())
    }

    /// A credential marked `Borrowed` whose token has expired must NOT be
    /// refreshed by eavs. The resolver returns a typed `ExpiredBorrowed` error
    /// (naming the owning CLI) instead of touching the refresh path or writing
    /// back to the store. This exercises the real call site of
    /// `resolve_oauth_access_token_with_account` with an injected OAuth store.
    #[expect(
        clippy::panic_in_result_fn,
        reason = "assertions are intentional in tests"
    )]
    #[tokio::test]
    async fn borrowed_expired_credential_is_not_refreshed() -> Result<(), anyhow::Error> {
        use crate::oauth::{CredentialProvenance, OAuthBackend, OAuthStore};

        // Temp-file SQLite OAuth store so provenance survives a roundtrip.
        let path =
            std::env::temp_dir().join(format!("eavs-proxy-oauth-{}.db", uuid::Uuid::new_v4()));
        let store = OAuthStore::new(&path, OAuthBackend::Sqlite)
            .await
            .context("unexpected None or Err")?;

        let creds = OAuthCredentials {
            user_id: "borrower".to_string(),
            provider: OAuthProviderKind::OpenAICodex,
            account_label: "default".to_string(),
            access_token: "at".to_string(),
            refresh_token: "rt".to_string(),
            expires_at: chrono::Utc::now().timestamp() - 100, // already expired
            extra_data: None,
            provenance: CredentialProvenance::Borrowed {
                owner: "codex".to_string(),
            },
        };
        store
            .upsert_credentials(&creds)
            .await
            .context("upsert_credentials(...) returned None or Err")?;

        let mut providers = HashMap::new();
        providers.insert(
            "codex".to_string(),
            ProviderConfig {
                type_: "openai-codex".to_string(),
                base_url: "http://up/v1".to_string(),
                ..Default::default()
            },
        );
        let state = AppState::new_with_upstream(
            make_config(providers),
            Arc::new(MockUpstream::new(vec![])),
        );
        state
            .oauth_store
            .set(Arc::new(store))
            .context("set(...) returned None or Err")?;

        let result =
            resolve_oauth_access_token_with_account(&state, "codex", "borrower", None).await;

        assert!(
            matches!(
                result,
                Err(OAuthResolveError::ExpiredBorrowed {
                    ref owner,
                    provider: OAuthProviderKind::OpenAICodex,
                }) if owner == "codex"
            ),
            "expected ExpiredBorrowed, got {result:?}"
        );

        // The store must remain untouchable: no refresh, no write-back. Reload and
        // confirm the credential is unchanged and still marked Borrowed.
        let reloaded = state
            .get_oauth_store()
            .context("unexpected None or Err")?
            .get_credentials("borrower", OAuthProviderKind::OpenAICodex)
            .await
            .context("unexpected None or Err")?
            .context("unexpected None or Err")?;
        assert_eq!(reloaded.access_token, "at");
        assert_eq!(reloaded.expires_at, creds.expires_at);
        assert_eq!(
            reloaded.provenance,
            CredentialProvenance::Borrowed {
                owner: "codex".to_string()
            }
        );

        Ok(())
    }

    async fn unprefixed_chat(
        providers: HashMap<String, ProviderConfig>,
        model: &str,
    ) -> Result<(StatusCode, Value, usize), anyhow::Error> {
        let mock = MockUpstream::new(vec![ResponseSpec {
            status: StatusCode::OK,
            headers: HeaderMap::new(),
            chunks: vec![Bytes::from_static(b"{}")],
        }]);
        let state = AppState::new_with_upstream(make_config(providers), Arc::new(mock.clone()));
        let app = Router::new()
            .route("/v1/*path", any(proxy_handler))
            .with_state(state);
        let req = http::Request::builder()
            .method("POST")
            .uri("/v1/chat/completions")
            .header(http::header::CONTENT_TYPE, "application/json")
            .body(Body::from(serde_json::to_vec(&json!({
                "model": model,
                "messages": [{"role": "user", "content": "hi"}]
            }))?))?;
        let response = app.oneshot(req).await?;
        let status = response.status();
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX).await?;
        let body: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
        Ok((status, body, mock.take_requests().await.len()))
    }

    fn fhgenie_only() -> HashMap<String, ProviderConfig> {
        let mut providers = HashMap::new();
        providers.insert(
            "fhgenie".to_string(),
            ProviderConfig {
                type_: "openai-compatible".to_string(),
                base_url: "http://fhgenie/v1".to_string(),
                api_key: "sk-secret-fhgenie".to_string(),
                ..Default::default()
            },
        );
        providers
    }

    #[expect(
        clippy::panic_in_result_fn,
        reason = "assertions are intentional in tests"
    )]
    #[tokio::test]
    async fn unprefixed_v1_rejects_unconfigured_model_guess() -> Result<(), anyhow::Error> {
        let (status, body, calls) =
            unprefixed_chat(fhgenie_only(), "deepseek-ai/DeepSeek-V4.1-Flash").await?;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(body["error"]["code"], "unknown_provider", "body: {body}");
        let message = body["error"]["message"].as_str().unwrap_or_default();
        assert!(message.contains("'deepseek'"), "names candidate: {message}");
        assert!(message.contains("/<provider>/v1"), "path remedy: {message}");
        assert!(message.contains("X-Provider"), "header remedy: {message}");
        assert!(
            !message.contains("fhgenie"),
            "no provider list without a key: {message}"
        );
        assert!(!body.to_string().contains("sk-secret"), "no key material");
        assert_eq!(calls, 0, "upstream must not be called");
        Ok(())
    }

    #[expect(
        clippy::panic_in_result_fn,
        reason = "assertions are intentional in tests"
    )]
    #[tokio::test]
    async fn unprefixed_v1_routes_configured_model_guess() -> Result<(), anyhow::Error> {
        let mut providers = fhgenie_only();
        providers.insert(
            "deepseek".to_string(),
            ProviderConfig {
                type_: "openai-compatible".to_string(),
                base_url: "http://deepseek/v1".to_string(),
                ..Default::default()
            },
        );
        let (status, _, calls) = unprefixed_chat(providers, "deepseek-chat").await?;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(calls, 1);
        Ok(())
    }

    fn effort_provider(declared: &[&str]) -> HashMap<String, ProviderConfig> {
        let mut providers = HashMap::new();
        providers.insert(
            "rtx".to_string(),
            ProviderConfig {
                type_: "openai-compatible".to_string(),
                base_url: "http://rtx/v1".to_string(),
                models: vec![crate::config::ModelShortlistEntry {
                    id: "qwen".to_string(),
                    name: "qwen".to_string(),
                    reasoning: true,
                    input: vec!["text".to_string()],
                    context_window: 1000,
                    max_tokens: 100,
                    cost: crate::config::ModelCost::default(),
                    compat: HashMap::new(),
                    reasoning_efforts: declared.iter().map(|v| (*v).to_string()).collect(),
                }],
                ..Default::default()
            },
        );
        providers
    }

    fn ok_spec() -> ResponseSpec {
        ResponseSpec {
            status: StatusCode::OK,
            headers: HeaderMap::new(),
            chunks: vec![Bytes::from_static(b"{}")],
        }
    }

    fn vllm_effort_400() -> ResponseSpec {
        ResponseSpec {
            status: StatusCode::BAD_REQUEST,
            headers: HeaderMap::new(),
            chunks: vec![Bytes::from(
                json!({"error": {"code": "BadRequestError", "message": "Unexpected reasoning effort high. Supported types are xhigh (default), medium, and low.", "type": "invalid_request_error"}}).to_string(),
            )],
        }
    }

    async fn effort_request(
        app: &Router,
        effort: &str,
    ) -> Result<(StatusCode, HeaderMap, Bytes), anyhow::Error> {
        let req = http::Request::builder()
            .method("POST")
            .uri("/rtx/v1/chat/completions")
            .header(http::header::CONTENT_TYPE, "application/json")
            .body(Body::from(serde_json::to_vec(&json!({
                "model": "qwen",
                "messages": [{"role": "user", "content": "hi"}],
                "reasoning_effort": effort
            }))?))?;
        let response = app.clone().oneshot(req).await?;
        let status = response.status();
        let headers = response.headers().clone();
        let body = axum::body::to_bytes(response.into_body(), usize::MAX).await?;
        Ok((status, headers, body))
    }

    fn sent_effort(request: &crate::upstream::UpstreamRequest) -> Result<String, anyhow::Error> {
        let body: Value = serde_json::from_slice(&request.body)?;
        Ok(body["reasoning_effort"]
            .as_str()
            .unwrap_or_default()
            .to_string())
    }

    fn effort_app(mock: &MockUpstream, declared: &[&str]) -> Router {
        let state = AppState::new_with_upstream(
            make_config(effort_provider(declared)),
            Arc::new(mock.clone()),
        );
        Router::new()
            .route("/:provider/v1/*path", any(provider_proxy_handler))
            .with_state(state)
    }

    #[expect(
        clippy::panic_in_result_fn,
        reason = "assertions are intentional in tests"
    )]
    #[tokio::test]
    async fn declared_reasoning_efforts_rewrite_before_forwarding() -> Result<(), anyhow::Error> {
        let mock = MockUpstream::new(vec![ok_spec(), ok_spec()]);
        let app = effort_app(&mock, &["low", "medium", "xhigh"]);
        let (status, headers, _) = effort_request(&app, "high").await?;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(headers["x-eavs-reasoning-effort"], "high->xhigh");
        let (_, headers, _) = effort_request(&app, "medium").await?;
        assert!(headers.get("x-eavs-reasoning-effort").is_none());
        let requests = mock.take_requests().await;
        assert_eq!(requests.len(), 2, "no retries needed");
        assert_eq!(sent_effort(&requests[0])?, "xhigh");
        assert_eq!(sent_effort(&requests[1])?, "medium");
        Ok(())
    }

    #[expect(
        clippy::panic_in_result_fn,
        reason = "assertions are intentional in tests"
    )]
    #[tokio::test]
    async fn rejected_reasoning_effort_is_learned_and_retried_once() -> Result<(), anyhow::Error> {
        let mock = MockUpstream::new(vec![vllm_effort_400(), ok_spec(), ok_spec()]);
        let app = effort_app(&mock, &[]);
        let (status, headers, _) = effort_request(&app, "high").await?;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(headers["x-eavs-reasoning-effort"], "high->xhigh");
        // Learned: the next request is rewritten up front, without a 400.
        let (status, _, _) = effort_request(&app, "minimal").await?;
        assert_eq!(status, StatusCode::OK);
        let requests = mock.take_requests().await;
        assert_eq!(requests.len(), 3);
        assert_eq!(sent_effort(&requests[0])?, "high");
        assert_eq!(sent_effort(&requests[1])?, "xhigh");
        assert_eq!(sent_effort(&requests[2])?, "low");
        Ok(())
    }

    #[expect(
        clippy::panic_in_result_fn,
        reason = "assertions are intentional in tests"
    )]
    #[tokio::test]
    async fn unrelated_400_passes_through_without_retry() -> Result<(), anyhow::Error> {
        let mock = MockUpstream::new(vec![ResponseSpec {
            status: StatusCode::BAD_REQUEST,
            headers: HeaderMap::new(),
            chunks: vec![Bytes::from(
                json!({"error": {"message": "max_tokens too large", "type": "invalid_request_error"}})
                    .to_string(),
            )],
        }]);
        let app = effort_app(&mock, &[]);
        let (status, _, body) = effort_request(&app, "high").await?;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(String::from_utf8_lossy(&body).contains("max_tokens too large"));
        assert_eq!(mock.take_requests().await.len(), 1);
        Ok(())
    }

    fn chat_provider(base_url: &str) -> ProviderConfig {
        ProviderConfig {
            type_: "openai-compatible".to_string(),
            base_url: base_url.to_string(),
            models: vec![crate::config::ModelShortlistEntry {
                id: "m".to_string(),
                name: "m".to_string(),
                reasoning: false,
                input: vec!["text".to_string()],
                context_window: 1000,
                max_tokens: 100,
                cost: crate::config::ModelCost::default(),
                compat: HashMap::new(),
                reasoning_efforts: Vec::new(),
            }],
            ..Default::default()
        }
    }

    /// App over all inference routes with keys enabled; returns a key scoped
    /// to `allowed` providers.
    async fn hint_app(
        providers: HashMap<String, ProviderConfig>,
        allowed: &[&str],
    ) -> Result<(Router, String, MockUpstream), anyhow::Error> {
        let mock = MockUpstream::new(vec![]);
        let mut config = make_config(providers);
        config.keys.enabled = true;
        let state = AppState::new_with_upstream(config, Arc::new(mock.clone()));
        let store = Arc::new(KeyStore::in_memory().await?);
        state
            .key_validator
            .set(Arc::new(KeyValidator::new(
                store.clone(),
                Arc::new(RateLimiter::new()),
            )))
            .ok()
            .context("validator already set")?;
        state
            .key_store
            .set(store.clone())
            .ok()
            .context("store already set")?;
        let key = store
            .create_key(CreateKeyRequest {
                permissions: KeyPermissions {
                    allowed_providers: Some(allowed.iter().map(|p| (*p).to_string()).collect()),
                    ..Default::default()
                },
                ..Default::default()
            })
            .await?
            .key;
        let app = crate::inference_routes::router(&state).with_state(state);
        Ok((app, key, mock))
    }

    async fn hint_request(
        app: &Router,
        uri: &str,
        key: Option<&str>,
        model: &str,
    ) -> Result<(StatusCode, Value), anyhow::Error> {
        let mut req = http::Request::builder()
            .method("POST")
            .uri(uri)
            .header(http::header::CONTENT_TYPE, "application/json");
        if let Some(key) = key {
            req = req.header("authorization", format!("Bearer {key}"));
        }
        let response = app
            .clone()
            .oneshot(req.body(Body::from(serde_json::to_vec(&json!({
                "model": model,
                "messages": [{"role": "user", "content": "hi"}]
            }))?))?)
            .await?;
        let status = response.status();
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX).await?;
        Ok((
            status,
            serde_json::from_slice(&bytes).unwrap_or(Value::Null),
        ))
    }

    fn three_providers() -> HashMap<String, ProviderConfig> {
        let mut providers = HashMap::new();
        providers.insert("default".to_string(), chat_provider("http://default/v1"));
        providers.insert("fhgenie".to_string(), chat_provider("http://fhgenie/v1"));
        providers.insert("secret-lab".to_string(), chat_provider("http://lab/v1"));
        providers
    }

    #[expect(
        clippy::panic_in_result_fn,
        reason = "assertions are intentional in tests"
    )]
    #[tokio::test]
    async fn unconfigured_model_guess_rejects_even_with_default_and_hints_key_scope(
    ) -> Result<(), anyhow::Error> {
        let (app, key, mock) = hint_app(three_providers(), &["fhgenie", "default"]).await?;
        let (status, body) = hint_request(
            &app,
            "/v1/chat/completions",
            Some(&key),
            "deepseek-ai/DeepSeek-V4.1-Flash",
        )
        .await?;
        assert_eq!(status, StatusCode::BAD_REQUEST, "body: {body}");
        assert_eq!(body["error"]["code"], "unknown_provider");
        let message = body["error"]["message"].as_str().unwrap_or_default();
        assert!(message.contains("'deepseek'"), "{message}");
        assert!(
            message.contains("Providers available to this key: default, fhgenie"),
            "{message}"
        );
        assert!(
            !message.contains("secret-lab"),
            "out-of-scope provider leaked: {message}"
        );
        assert_eq!(mock.take_requests().await.len(), 0);
        Ok(())
    }

    #[expect(
        clippy::panic_in_result_fn,
        reason = "assertions are intentional in tests"
    )]
    #[tokio::test]
    async fn unknown_pinned_provider_names_remedy_and_key_scope() -> Result<(), anyhow::Error> {
        let (app, key, mock) = hint_app(three_providers(), &["fhgenie"]).await?;
        let (status, body) =
            hint_request(&app, "/nope/v1/chat/completions", Some(&key), "m").await?;
        assert_eq!(status, StatusCode::BAD_REQUEST, "body: {body}");
        assert_eq!(body["error"]["code"], "unknown_provider");
        let message = body["error"]["message"].as_str().unwrap_or_default();
        assert!(message.contains("'nope'"), "{message}");
        assert!(
            message.contains("Providers available to this key: fhgenie"),
            "{message}"
        );
        assert!(
            !message.contains("secret-lab") && !message.contains("default,"),
            "{message}"
        );
        assert_eq!(mock.take_requests().await.len(), 0);
        Ok(())
    }

    #[expect(
        clippy::panic_in_result_fn,
        reason = "assertions are intentional in tests"
    )]
    #[tokio::test]
    async fn non_inference_path_names_expected_endpoints() -> Result<(), anyhow::Error> {
        let (app, key, mock) = hint_app(three_providers(), &["fhgenie"]).await?;
        let (status, body) =
            hint_request(&app, "/fhgenie/v1/chat/complete", Some(&key), "m").await?;
        assert_eq!(status, StatusCode::NOT_FOUND, "body: {body}");
        assert_eq!(body["error"]["code"], "unknown_endpoint");
        let message = body["error"]["message"].as_str().unwrap_or_default();
        assert!(
            message.contains("/<provider>/v1/chat/completions"),
            "{message}"
        );
        assert!(
            message.contains("Providers available to this key: fhgenie"),
            "{message}"
        );
        let (status, body) = hint_request(&app, "/fhgenie/v1/chat/complete", None, "m").await?;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert!(
            !body.to_string().contains("fhgenie,")
                && !body.to_string().contains("available to this key"),
            "no list without key: {body}"
        );
        assert_eq!(mock.take_requests().await.len(), 0);
        Ok(())
    }

    #[expect(
        clippy::panic_in_result_fn,
        reason = "assertions are intentional in tests"
    )]
    #[tokio::test]
    async fn anthropic_adaptive_thinking_is_learned_and_retried_once() -> Result<(), anyhow::Error>
    {
        let rejection = ResponseSpec {
            status: StatusCode::BAD_REQUEST,
            headers: HeaderMap::new(),
            chunks: vec![Bytes::from(
                json!({"type": "error", "error": {"type": "invalid_request_error", "message": "\"thinking.type.enabled\" is not supported for this model. Use \"thinking.type.adaptive\" and \"output_config.effort\" to control thinking behavior."}}).to_string(),
            )],
        };
        let mock = MockUpstream::new(vec![rejection, ok_spec(), ok_spec()]);
        let mut providers = HashMap::new();
        providers.insert(
            "anthropic".to_string(),
            ProviderConfig {
                type_: "anthropic".to_string(),
                base_url: "http://anthropic".to_string(),
                api_key: "sk-ant-test".to_string(),
                ..Default::default()
            },
        );
        let state = AppState::new_with_upstream(make_config(providers), Arc::new(mock.clone()));
        let app = Router::new()
            .route("/:provider/v1/*path", any(provider_proxy_handler))
            .with_state(state);
        let send = |budget: u64| {
            let app = app.clone();
            async move {
                let req = http::Request::builder()
                    .method("POST")
                    .uri("/anthropic/v1/messages")
                    .header(http::header::CONTENT_TYPE, "application/json")
                    .body(Body::from(serde_json::to_vec(&json!({
                        "model": "claude-sonnet-5-5",
                        "max_tokens": 32000,
                        "thinking": {"type": "enabled", "budget_tokens": budget},
                        "messages": [{"role": "user", "content": "hi"}]
                    }))?))?;
                let response = app.oneshot(req).await?;
                Ok::<_, anyhow::Error>((response.status(), response.headers().clone()))
            }
        };
        let (status, headers) = send(16384).await?;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(headers["x-eavs-thinking"], "enabled->adaptive(high)");
        let (status, headers) = send(2048).await?;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(headers["x-eavs-thinking"], "enabled->adaptive(low)");
        let requests = mock.take_requests().await;
        assert_eq!(requests.len(), 3, "one retry, then adjusted up front");
        let bodies: Vec<Value> = requests
            .iter()
            .map(|r| serde_json::from_slice(&r.body))
            .collect::<Result<_, _>>()?;
        assert_eq!(bodies[0]["thinking"]["type"], "enabled");
        assert_eq!(bodies[1]["thinking"], json!({"type": "adaptive"}));
        assert_eq!(bodies[1]["output_config"]["effort"], "high");
        assert_eq!(bodies[2]["thinking"], json!({"type": "adaptive"}));
        assert_eq!(bodies[2]["output_config"]["effort"], "low");
        Ok(())
    }

    #[test]
    fn upstream_error_messages_survive_normalization() {
        let fastapi = json!({"detail": "Model 'x' is not loaded"}).to_string();
        let (message, _, _) = normalize_upstream_error(
            ProviderType::OpenAICompatible,
            StatusCode::BAD_REQUEST,
            &fastapi,
            fastapi.as_bytes(),
        );
        assert_eq!(message, "Model 'x' is not loaded");
        let validation =
            json!({"detail": [{"loc": ["body", "max_tokens"], "msg": "must be positive"}]})
                .to_string();
        let (message, _, _) = normalize_upstream_error(
            ProviderType::OpenAICompatible,
            StatusCode::UNPROCESSABLE_ENTITY,
            &validation,
            validation.as_bytes(),
        );
        assert_eq!(message, "must be positive");
    }
}

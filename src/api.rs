use crate::keys::{CreateKeyRequest, CreateKeyResponse, KeyInfo, KeyPermissions};
use crate::oauth::{
    anthropic, github_copilot, openai_codex, pkce, OAuthLoginResponse, OAuthPendingAuth,
    OAuthProvider,
};
use crate::state::{AppState, ConversationMetadata, InjectionPayload};
use axum::{
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    response::sse::{Event, Sse},
    Json,
};
use futures::stream::Stream;
use serde::{Deserialize, Serialize};
use std::convert::Infallible;
use tokio_stream::wrappers::BroadcastStream;
use tokio_stream::StreamExt;
use uuid::Uuid;

/// Maximum content length for injected messages (1 MB)
const MAX_INJECTION_CONTENT_LENGTH: usize = 1024 * 1024;

/// Maximum number of messages per injection request
const MAX_INJECTION_MESSAGES: usize = 100;

/// Valid roles for injected messages
const VALID_INJECTION_ROLES: &[&str] = &["system", "user", "assistant"];

/// Validate injection payload to prevent abuse.
fn validate_injection_payload(
    payload: &InjectionPayload,
) -> Result<(), (StatusCode, &'static str)> {
    // Check total number of messages
    if payload.messages.len() > MAX_INJECTION_MESSAGES {
        return Err((
            StatusCode::BAD_REQUEST,
            "Too many messages in injection payload",
        ));
    }

    for injection in &payload.messages {
        // Validate role is one of the allowed values
        if !VALID_INJECTION_ROLES.contains(&injection.role.as_str()) {
            return Err((
                StatusCode::BAD_REQUEST,
                "Invalid role in injection. Allowed: system, user, assistant",
            ));
        }

        // Validate content length
        if injection.content.len() > MAX_INJECTION_CONTENT_LENGTH {
            return Err((StatusCode::BAD_REQUEST, "Injection content too large"));
        }
    }

    Ok(())
}

/// Inject messages into a conversation.
pub async fn inject_handler(
    State(state): State<AppState>,
    Path(conversation_id): Path<String>,
    Json(payload): Json<InjectionPayload>,
) -> Result<StatusCode, (StatusCode, &'static str)> {
    // Validate the injection payload
    validate_injection_payload(&payload)?;

    // If a WS session is active for this conversation, deliver immediately (mid-stream).
    // Otherwise, queue for the next HTTP request (pre-request injection).
    let delivered = state
        .ws_sessions
        .deliver_injections(&conversation_id, payload.messages.clone());

    if !delivered {
        state
            .conversations
            .add_injections(&conversation_id, payload.messages);
    }
    Ok(StatusCode::OK)
}

/// Clear a conversation's pending injections.
pub async fn clear_handler(
    State(state): State<AppState>,
    Path(conversation_id): Path<String>,
) -> StatusCode {
    state.conversations.clear(&conversation_id);
    // Also clear legacy injections
    state.injections.remove(&conversation_id);
    StatusCode::OK
}

/// Stream analysis logs via SSE.
pub async fn logs_stream_handler(
    State(state): State<AppState>,
) -> Sse<impl Stream<Item = Result<Event, Infallible>> + Send> {
    let rx = state.analysis_tx.subscribe();
    let stream = BroadcastStream::new(rx).map(|res| match res {
        Ok(event) => {
            let data = serde_json::to_string(&event).unwrap_or_default();
            Ok(Event::default().data(data))
        }
        Err(_) => Ok(Event::default().event("error").data("Lagged")),
    });

    Sse::new(stream).keep_alive(axum::response::sse::KeepAlive::default())
}

/// Health check endpoint.
pub async fn health_handler() -> StatusCode {
    StatusCode::OK
}

#[derive(Deserialize)]
pub struct DefaultsQuery {
    pub provider: Option<String>,
}

#[derive(Serialize)]
pub struct DefaultsResponse {
    pub provider: String,
    pub default: String,
    pub fast: String,
    pub reasoning: String,
    pub fallback: String,
}

/// Zero-config model defaults for alias-based clients.
///
/// Supports optional provider override via `?provider=name`.
pub async fn defaults_handler(
    State(state): State<AppState>,
    axum::extract::Query(query): axum::extract::Query<DefaultsQuery>,
) -> Result<Json<DefaultsResponse>, StatusCode> {
    let runtime_default_provider = crate::runtime_state::load_runtime_state()
        .and_then(|s| s.default_provider)
        .unwrap_or_default();

    let defaults = crate::model_defaults::resolve_model_defaults(
        &state.config,
        state.catalog(),
        query.provider.as_deref(),
        if runtime_default_provider.is_empty() {
            None
        } else {
            Some(runtime_default_provider.as_str())
        },
    )
    .ok_or(StatusCode::NOT_FOUND)?;

    Ok(Json(DefaultsResponse {
        provider: defaults.provider,
        default: defaults.default,
        fast: defaults.fast,
        reasoning: defaults.reasoning,
        fallback: defaults.fallback,
    }))
}

/// List available providers.
pub async fn providers_handler(State(state): State<AppState>) -> Json<Vec<String>> {
    let providers: Vec<String> = state.config.providers.keys().cloned().collect();
    Json(providers)
}

/// Detailed provider info for external integrations (e.g., generating models.json for Pi).
#[derive(Serialize)]
pub struct ProviderDetail {
    /// Provider name as configured in eavs (e.g., "openai", "anthropic")
    pub name: String,
    /// Provider type (e.g., "openai", "anthropic", "openai-codex")
    #[serde(rename = "type")]
    pub type_: String,
    /// Pi-compatible API type for models.json
    pub pi_api: Option<&'static str>,
    /// Whether this provider uses OAuth
    pub oauth: bool,
    /// Whether the provider has a resolved API key (not the key itself)
    pub has_api_key: bool,
    /// Custom headers the provider requires (e.g., Azure `api-key`).
    /// Keys/values are NOT resolved (no env: expansion) -- the consuming
    /// integration (oqto) decides how to handle them.
    #[serde(default, skip_serializing_if = "std::collections::HashMap::is_empty")]
    pub headers: std::collections::HashMap<String, String>,
    /// API version string (Azure providers).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_version: Option<String>,
    /// Provider-level compat flags (explicit config merged with URL-detected
    /// defaults). Adapters translate these to the consumer's compat schema —
    /// pi can't auto-detect quirks behind the proxy because the base URL it
    /// sees is eavs, not the real upstream.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub compat: Option<serde_json::Value>,
    /// Model list: config shortlist if set, otherwise full catalog from models.dev
    pub models: Vec<crate::config::ModelShortlistEntry>,
}

/// Whether a provider type authenticates via an eavs-managed OAuth flow.
pub fn provider_uses_oauth(provider_type: &crate::provider::ProviderType) -> bool {
    use crate::provider::ProviderType;
    matches!(
        provider_type,
        ProviderType::OpenAICodex | ProviderType::GithubCopilot
    )
}

/// Serialize a provider's resolved compat settings, omitting unset fields.
pub fn provider_compat_json(config: &crate::config::ProviderConfig) -> Option<serde_json::Value> {
    let compat = config.resolved_compat();
    let mut obj = serde_json::Map::new();
    if let Some(v) = compat.supports_store {
        obj.insert("supports_store".to_string(), serde_json::json!(v));
    }
    if let Some(v) = compat.supports_developer_role {
        obj.insert("supports_developer_role".to_string(), serde_json::json!(v));
    }
    if let Some(v) = compat.max_tokens_field {
        obj.insert("max_tokens_field".to_string(), serde_json::json!(v));
    }
    if let Some(v) = compat.supports_stream_options {
        obj.insert("supports_stream_options".to_string(), serde_json::json!(v));
    }
    if obj.is_empty() {
        None
    } else {
        Some(serde_json::Value::Object(obj))
    }
}

/// Get detailed provider information.
///
/// Returns provider types, Pi API mappings, and model lists for models.json generation.
/// Model list logic: config shortlist non-empty = only those models; empty = full models.dev catalog.
pub async fn providers_detail_handler(State(state): State<AppState>) -> Json<Vec<ProviderDetail>> {
    use crate::model_catalog::eavs_to_catalog_id;
    use crate::provider::ProviderType;

    let catalog = state.catalog();

    let mut details: Vec<ProviderDetail> = state
        .config
        .providers
        .iter()
        .map(|(name, config)| {
            let provider_type = ProviderType::from_str(&config.type_);
            let has_api_key = !config.api_key.is_empty();

            // Resolve models: config shortlist wins, otherwise catalog
            let models = if !config.models.is_empty() {
                config.models.clone()
            } else if let Some(cat) = catalog {
                let catalog_id = eavs_to_catalog_id(name, &config.type_);
                cat.models_for_provider(catalog_id, &config.models)
            } else {
                Vec::new()
            };

            // Collect non-secret headers. For Azure providers, the proxy
            // injects the api-key header itself, so we expose it here as
            // "EAVS_API_KEY" (a placeholder the consumer resolves).
            let mut headers = std::collections::HashMap::new();
            for k in config.headers.keys() {
                // Don't leak actual header values — the eavs proxy handles
                // header injection. But signal to the consumer which headers
                // are required so it can set up the models.json correctly.
                headers.insert(k.clone(), "EAVS_API_KEY".to_string());
            }

            ProviderDetail {
                name: name.clone(),
                type_: config.type_.clone(),
                pi_api: pi_api_for_provider(&provider_type),
                oauth: provider_uses_oauth(&provider_type),
                has_api_key,
                headers,
                api_version: config.api_version.clone(),
                compat: provider_compat_json(config),
                models,
            }
        })
        .collect();

    // Merge admin-added providers from the provider store (config takes precedence)
    if let Some(store) = state.get_provider_store() {
        for entry in store.list_providers() {
            if state.config.providers.contains_key(&entry.name) {
                continue; // config providers take precedence
            }

            let type_str = entry
                .config
                .get("type")
                .and_then(|v| v.as_str())
                .unwrap_or("openai");
            let provider_type = ProviderType::from_str(type_str);
            let has_api_key = entry
                .config
                .get("api_key")
                .and_then(|v| v.as_str())
                .map(|s| !s.is_empty())
                .unwrap_or(false);

            let models = if let Ok(m) = store.get_models(&entry.name).await {
                m
            } else if let Some(cat) = catalog {
                let catalog_id = eavs_to_catalog_id(&entry.name, type_str);
                cat.models_for_provider(catalog_id, &[])
            } else {
                Vec::new()
            };

            let mut headers = std::collections::HashMap::new();
            if let Some(obj) = entry.config.get("headers").and_then(|v| v.as_object()) {
                for k in obj.keys() {
                    headers.insert(k.clone(), "EAVS_API_KEY".to_string());
                }
            }

            let api_version = entry
                .config
                .get("api_version")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());

            let compat =
                serde_json::from_value::<crate::config::ProviderConfig>(entry.config.clone())
                    .ok()
                    .and_then(|c| provider_compat_json(&c));

            details.push(ProviderDetail {
                name: entry.name,
                type_: type_str.to_string(),
                pi_api: pi_api_for_provider(&provider_type),
                oauth: provider_uses_oauth(&provider_type),
                has_api_key,
                headers,
                api_version,
                compat,
                models,
            });
        }
    }

    Json(details)
}

/// GET /providers/templates
///
/// Returns available provider templates for quick setup.
/// No authentication required — this is read-only metadata.
/// Templates are merged from the shipped baseline + models.dev catalog.
pub async fn provider_templates_handler(
    State(state): State<AppState>,
) -> Json<Vec<crate::provider_templates::ProviderTemplate>> {
    let catalog = state.catalog();
    let templates = crate::provider_templates::build_templates(catalog);
    Json(templates)
}

/// POST /admin/providers (extended) — supports `template` field.
///
/// If the request body includes `"template": "provider-id"`, the provider config
/// is pre-filled from the matching template. The caller only needs to supply
/// the provider name and optionally an api_key override.
#[derive(Debug, Clone, serde::Deserialize)]
pub struct ProviderFromTemplateRequest {
    /// Provider name (key in config, e.g., "my-anthropic")
    pub name: String,
    /// Template ID to use (e.g., "anthropic", "groq", "minimax")
    pub template: String,
    /// API key override (optional — defaults to env: syntax from template)
    pub api_key: Option<String>,
    /// Additional config overrides (merged on top of template defaults)
    #[serde(default)]
    pub overrides: serde_json::Map<String, serde_json::Value>,
}

/// POST /admin/providers/from-template
/// Authorization: Bearer <master_key>
///
/// Create a provider from a template with minimal input.
pub async fn provider_from_template_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(payload): Json<ProviderFromTemplateRequest>,
) -> Result<Json<crate::provider_store::ProviderEntry>, (StatusCode, Json<ProviderApiError>)> {
    check_master_key_provider(&headers, &state)?;

    let catalog = state.catalog();
    let templates = crate::provider_templates::build_templates(catalog);

    let template = templates
        .iter()
        .find(|t| t.id == payload.template)
        .ok_or_else(|| {
            (
                StatusCode::NOT_FOUND,
                Json(ProviderApiError::new(
                    format!("Template '{}' not found", payload.template),
                    "template_not_found",
                )),
            )
        })?;

    // Generate config from template
    let mut config =
        crate::provider_templates::template_to_config(template, payload.api_key.as_deref());

    // Apply overrides
    if let serde_json::Value::Object(ref mut map) = config {
        for (k, v) in &payload.overrides {
            map.insert(k.clone(), v.clone());
        }
    }

    let store = state.get_provider_store().ok_or_else(|| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ProviderApiError::new(
                "Provider store not initialized (enable [keys] in config)",
                "internal_error",
            )),
        )
    })?;

    let request = crate::provider_store::ProviderRequest {
        name: payload.name,
        config,
        enabled: true,
    };

    let entry = store.upsert_provider(request).await.map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ProviderApiError::new(
                format!("Failed to create provider: {}", e),
                "upsert_failed",
            )),
        )
    })?;

    Ok(Json(entry))
}

/// Map eavs provider type to Pi's API type string for models.json.
pub fn pi_api_for_provider(provider_type: &crate::provider::ProviderType) -> Option<&'static str> {
    use crate::provider::ProviderType;
    match provider_type {
        ProviderType::OpenAI => Some("openai-responses"),
        ProviderType::OpenAIResponses => Some("openai-responses"),
        ProviderType::OpenAICodex => Some("openai-codex-responses"),
        ProviderType::Anthropic => Some("anthropic-messages"),
        ProviderType::Google | ProviderType::GoogleVertex => Some("google-generative-ai"),
        // Bedrock needs eavs's OpenAI-format translation layer (SigV4 + AWS
        // event streams), so native pi APIs can't be relayed for it.
        ProviderType::Bedrock => Some("openai-completions"),
        ProviderType::Azure => Some("openai-responses"),
        ProviderType::Mistral
        | ProviderType::Groq
        | ProviderType::Cerebras
        | ProviderType::XAI
        | ProviderType::OpenRouter
        | ProviderType::OpenAICompatible => Some("openai-completions"),
        ProviderType::GithubCopilot => Some("openai-responses"),
        ProviderType::Mock => Some("openai-completions"),
    }
}

/// Response for conversation stats.
#[derive(Serialize)]
pub struct ConversationStatsResponse {
    pub active_conversations: usize,
    pub total_created: u64,
    pub total_expired: u64,
    pub total_evicted: u64,
}

/// Get conversation store statistics.
pub async fn stats_handler(State(state): State<AppState>) -> Json<ConversationStatsResponse> {
    let (active, created, expired, evicted) = state.conversations.stats();
    Json(ConversationStatsResponse {
        active_conversations: active,
        total_created: created,
        total_expired: expired,
        total_evicted: evicted,
    })
}

/// List all active conversations.
pub async fn conversations_handler(State(state): State<AppState>) -> Json<Vec<String>> {
    Json(state.conversations.list_conversations())
}

/// Get conversation details.
#[derive(Serialize)]
pub struct ConversationResponse {
    pub id: String,
    pub metadata: ConversationMetadata,
    pub pending_injections: usize,
    pub request_count: u64,
}

pub async fn conversation_handler(
    State(state): State<AppState>,
    Path(conversation_id): Path<String>,
) -> Result<Json<ConversationResponse>, StatusCode> {
    state
        .conversations
        .get(&conversation_id)
        .map(|entry| {
            Json(ConversationResponse {
                id: conversation_id,
                metadata: entry.metadata.clone(),
                pending_injections: entry.injections.len(),
                request_count: entry.metadata.request_count,
            })
        })
        .ok_or(StatusCode::NOT_FOUND)
}

/// Update conversation metadata.
#[derive(serde::Deserialize)]
pub struct UpdateMetadataPayload {
    pub provider: Option<String>,
    pub model: Option<String>,
    #[serde(default)]
    pub tags: Option<Vec<String>>,
}

pub async fn update_conversation_handler(
    State(state): State<AppState>,
    Path(conversation_id): Path<String>,
    Json(payload): Json<UpdateMetadataPayload>,
) -> StatusCode {
    // Ensure conversation exists
    state.conversations.get_or_create(&conversation_id);

    state
        .conversations
        .update_metadata(&conversation_id, |meta| {
            if let Some(provider) = payload.provider {
                meta.provider = Some(provider);
            }
            if let Some(model) = payload.model {
                meta.model = Some(model);
            }
            if let Some(tags) = payload.tags {
                meta.tags = tags;
            }
        });

    StatusCode::OK
}

// ============================================================================
// Virtual API Key Management Endpoints
// ============================================================================

/// Error response for key API.
#[derive(Serialize)]
pub struct KeyApiError {
    pub error: String,
    pub code: String,
}

impl KeyApiError {
    fn new(error: impl Into<String>, code: impl Into<String>) -> Self {
        Self {
            error: error.into(),
            code: code.into(),
        }
    }
}

/// Error response for provider API.
#[derive(Serialize)]
pub struct ProviderApiError {
    pub error: String,
    pub code: String,
}

impl ProviderApiError {
    fn new(error: impl Into<String>, code: impl Into<String>) -> Self {
        Self {
            error: error.into(),
            code: code.into(),
        }
    }
}

/// Constant-time byte comparison to prevent timing attacks.
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }

    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

/// Check master key authorization using constant-time comparison.
fn check_master_key(
    headers: &HeaderMap,
    state: &AppState,
) -> Result<(), (StatusCode, Json<KeyApiError>)> {
    let master_key = state.config.keys.resolved_master_key();

    // If no master key configured, admin API is disabled
    let expected_key = master_key.ok_or_else(|| {
        (
            StatusCode::FORBIDDEN,
            Json(KeyApiError::new(
                "Admin API is disabled (no master key configured)",
                "admin_disabled",
            )),
        )
    })?;

    // Check Authorization header
    let auth_header = headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "));

    match auth_header {
        // Use constant-time comparison to prevent timing attacks
        Some(key) if constant_time_eq(key.as_bytes(), expected_key.as_bytes()) => Ok(()),
        _ => Err((
            StatusCode::UNAUTHORIZED,
            Json(KeyApiError::new("Invalid master key", "unauthorized")),
        )),
    }
}

/// Check if keys feature is enabled.
fn check_keys_enabled(state: &AppState) -> Result<(), (StatusCode, Json<KeyApiError>)> {
    if !state.config.keys.enabled {
        return Err((
            StatusCode::SERVICE_UNAVAILABLE,
            Json(KeyApiError::new(
                "Virtual API keys are not enabled",
                "keys_disabled",
            )),
        ));
    }
    Ok(())
}

/// Create a new virtual API key.
///
/// POST /admin/keys
/// Authorization: Bearer <master_key>
pub async fn create_key_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(payload): Json<CreateKeyApiRequest>,
) -> Result<Json<CreateKeyResponse>, (StatusCode, Json<KeyApiError>)> {
    check_keys_enabled(&state)?;
    check_master_key(&headers, &state)?;

    let store = state.get_key_store().ok_or_else(|| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(KeyApiError::new(
                "Key store not initialized",
                "internal_error",
            )),
        )
    })?;

    // Apply default limits from config if not specified
    let mut permissions = payload.permissions.unwrap_or_default();
    if permissions.rpm_limit.is_none() {
        permissions.rpm_limit = state.config.keys.default_rpm_limit;
    }
    if permissions.max_budget_usd.is_none() {
        permissions.max_budget_usd = state.config.keys.default_budget_usd;
    }

    let request = CreateKeyRequest {
        name: payload.name,
        expires_at: payload.expires_at,
        permissions,
        metadata: payload.metadata.unwrap_or(serde_json::Value::Null),
        oauth_user: payload.oauth_user,
        oauth_account: payload.oauth_account,
    };

    let response = store.create_key(request).await.map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(KeyApiError::new(
                format!("Failed to create key: {}", e),
                "create_failed",
            )),
        )
    })?;

    Ok(Json(response))
}

/// Request body for creating a key.
#[derive(Deserialize)]
pub struct CreateKeyApiRequest {
    pub name: Option<String>,
    pub expires_at: Option<chrono::DateTime<chrono::Utc>>,
    pub permissions: Option<KeyPermissions>,
    pub metadata: Option<serde_json::Value>,
    pub oauth_user: Option<String>,
    pub oauth_account: Option<String>,
}

/// List all virtual API keys.
///
/// GET /admin/keys
/// Authorization: Bearer <master_key>
pub async fn list_keys_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Vec<KeyInfo>>, (StatusCode, Json<KeyApiError>)> {
    check_keys_enabled(&state)?;
    check_master_key(&headers, &state)?;

    let store = state.get_key_store().ok_or_else(|| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(KeyApiError::new(
                "Key store not initialized",
                "internal_error",
            )),
        )
    })?;

    let keys = store.list_keys().await.map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(KeyApiError::new(
                format!("Failed to list keys: {}", e),
                "list_failed",
            )),
        )
    })?;

    Ok(Json(keys))
}

/// Get info about a specific key.
///
/// GET /admin/keys/:key_hash
/// Authorization: Bearer <master_key>
pub async fn get_key_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(key_id_or_hash): Path<String>,
) -> Result<Json<KeyInfo>, (StatusCode, Json<KeyApiError>)> {
    check_keys_enabled(&state)?;
    check_master_key(&headers, &state)?;

    let store = state.get_key_store().ok_or_else(|| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(KeyApiError::new(
                "Key store not initialized",
                "internal_error",
            )),
        )
    })?;

    // Try lookup by human ID first (e.g., "cold-lamp"), then by hash
    let key = store
        .get_by_human_id(&key_id_or_hash)
        .or_else(|| store.get_by_hash(&key_id_or_hash))
        .ok_or_else(|| {
            (
                StatusCode::NOT_FOUND,
                Json(KeyApiError::new("Key not found", "not_found")),
            )
        })?;

    Ok(Json(key.to_info()))
}

/// Disable a virtual API key.
///
/// DELETE /admin/keys/:key_id_or_hash
/// Authorization: Bearer <master_key>
///
/// Accepts either the human-readable key ID (e.g., "cold-lamp") or the key hash.
pub async fn delete_key_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(key_id_or_hash): Path<String>,
) -> Result<StatusCode, (StatusCode, Json<KeyApiError>)> {
    check_keys_enabled(&state)?;
    check_master_key(&headers, &state)?;

    let store = state.get_key_store().ok_or_else(|| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(KeyApiError::new(
                "Key store not initialized",
                "internal_error",
            )),
        )
    })?;

    // Resolve human ID to hash if needed
    let key_hash = if let Some(key) = store.get_by_human_id(&key_id_or_hash) {
        key.key_hash
    } else {
        key_id_or_hash
    };

    let deleted = store.disable_key(&key_hash).await.map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(KeyApiError::new(
                format!("Failed to disable key: {}", e),
                "delete_failed",
            )),
        )
    })?;

    if deleted {
        // Clear rate limiter state for this key
        state.rate_limiter.clear_key(&key_hash);
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err((
            StatusCode::NOT_FOUND,
            Json(KeyApiError::new("Key not found", "not_found")),
        ))
    }
}

/// Get usage history for a key.
///
/// GET /admin/keys/:key_id_or_hash/usage
/// Authorization: Bearer <master_key>
///
/// Accepts either the human-readable key ID (e.g., "cold-lamp") or the key hash.
pub async fn key_usage_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(key_id_or_hash): Path<String>,
) -> Result<Json<Vec<crate::keys::UsageRecord>>, (StatusCode, Json<KeyApiError>)> {
    check_keys_enabled(&state)?;
    check_master_key(&headers, &state)?;

    let store = state.get_key_store().ok_or_else(|| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(KeyApiError::new(
                "Key store not initialized",
                "internal_error",
            )),
        )
    })?;

    // Resolve human ID to hash if needed
    let key_hash = if let Some(key) = store.get_by_human_id(&key_id_or_hash) {
        key.key_hash
    } else {
        key_id_or_hash
    };

    let history = store
        .get_usage_history(&key_hash, Some(100))
        .await
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(KeyApiError::new(
                    format!("Failed to get usage: {}", e),
                    "usage_failed",
                )),
            )
        })?;

    Ok(Json(history))
}

/// Self-provisioning endpoint (if enabled).
///
/// POST /keys/provision
/// No auth required, but subject to config limits.
pub async fn provision_key_handler(
    State(state): State<AppState>,
    Json(payload): Json<ProvisionKeyRequest>,
) -> Result<Json<CreateKeyResponse>, (StatusCode, Json<KeyApiError>)> {
    check_keys_enabled(&state)?;

    if !state.config.keys.allow_self_provisioning {
        return Err((
            StatusCode::FORBIDDEN,
            Json(KeyApiError::new(
                "Self-provisioning is not enabled",
                "provisioning_disabled",
            )),
        ));
    }

    let store = state.get_key_store().ok_or_else(|| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(KeyApiError::new(
                "Key store not initialized",
                "internal_error",
            )),
        )
    })?;

    // Apply default/max limits from config
    let permissions = KeyPermissions {
        rpm_limit: state.config.keys.default_rpm_limit,
        max_budget_usd: state.config.keys.default_budget_usd,
        ..Default::default()
    };

    let request = CreateKeyRequest {
        name: payload.name,
        expires_at: None,
        permissions,
        metadata: payload.metadata.unwrap_or(serde_json::Value::Null),
        oauth_user: payload.oauth_user,
        oauth_account: None,
    };

    let response = store.create_key(request).await.map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(KeyApiError::new(
                format!("Failed to create key: {}", e),
                "create_failed",
            )),
        )
    })?;

    Ok(Json(response))
}

/// Request body for self-provisioning.
#[derive(Deserialize)]
pub struct ProvisionKeyRequest {
    pub name: Option<String>,
    pub metadata: Option<serde_json::Value>,
    pub oauth_user: Option<String>,
}

/// Key stats response.
#[derive(Serialize)]
pub struct KeyStatsResponse {
    pub total_keys: usize,
    pub keys_enabled: bool,
    pub self_provisioning_enabled: bool,
    pub pricing_models: usize,
}

/// Get key system stats.
///
/// GET /admin/keys/stats
/// Authorization: Bearer <master_key>
pub async fn key_stats_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<KeyStatsResponse>, (StatusCode, Json<KeyApiError>)> {
    check_keys_enabled(&state)?;
    check_master_key(&headers, &state)?;

    let store = state.get_key_store().ok_or_else(|| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(KeyApiError::new(
                "Key store not initialized",
                "internal_error",
            )),
        )
    })?;

    let pricing_models = state.pricing.len().await;

    Ok(Json(KeyStatsResponse {
        total_keys: store.active_key_count(),
        keys_enabled: state.config.keys.enabled,
        self_provisioning_enabled: state.config.keys.allow_self_provisioning,
        pricing_models,
    }))
}

/// Get upstream rate limit quotas.
///
/// GET /admin/quotas
/// Authorization: Bearer <master_key>
///
/// Returns the latest observed upstream rate limit quotas for all provider/account
/// pairs that have been seen recently.
pub async fn upstream_quotas_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Vec<crate::upstream_quota::QuotaSnapshot>>, (StatusCode, Json<KeyApiError>)> {
    check_keys_enabled(&state)?;
    check_master_key(&headers, &state)?;

    let quotas = state.quota_tracker.all().await;
    let snapshots: Vec<crate::upstream_quota::QuotaSnapshot> = quotas
        .iter()
        .map(|(key, quota)| crate::upstream_quota::QuotaSnapshot::from_quota(key, quota))
        .collect();

    Ok(Json(snapshots))
}

/// Update pricing from LiteLLM.
///
/// POST /admin/pricing/update
/// Authorization: Bearer <master_key>
pub async fn update_pricing_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<PricingUpdateResponse>, (StatusCode, Json<KeyApiError>)> {
    check_keys_enabled(&state)?;
    check_master_key(&headers, &state)?;

    let count = state.pricing.update_from_litellm().await.map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(KeyApiError::new(
                format!("Failed to update pricing: {}", e),
                "update_failed",
            )),
        )
    })?;

    Ok(Json(PricingUpdateResponse {
        models_updated: count,
        total_models: state.pricing.len().await,
    }))
}

#[derive(Serialize)]
pub struct PricingUpdateResponse {
    pub models_updated: usize,
    pub total_models: usize,
}

// ============================================================================
// Admin Provider/Model Management Endpoints
// ============================================================================

/// Create or update a provider.
///
/// POST /admin/providers
/// Authorization: Bearer <master_key>
pub async fn upsert_provider_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(payload): Json<crate::provider_store::ProviderRequest>,
) -> Result<Json<crate::provider_store::ProviderEntry>, (StatusCode, Json<ProviderApiError>)> {
    check_master_key_provider(&headers, &state)?;

    let store = state.get_provider_store().ok_or_else(|| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ProviderApiError::new(
                "Provider store not initialized",
                "internal_error",
            )),
        )
    })?;

    let config_json = &payload.config;
    if !config_json.is_object() {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(ProviderApiError::new(
                "Invalid config format",
                "invalid_config",
            )),
        ));
    }
    if !config_json
        .get("type")
        .and_then(|v| v.as_str())
        .map(|s| !s.is_empty())
        .unwrap_or(false)
    {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(ProviderApiError::new(
                "Provider type is required",
                "missing_type",
            )),
        ));
    }

    let entry = store.upsert_provider(payload).await.map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ProviderApiError::new(
                format!("Failed to upsert provider: {}", e),
                "upsert_failed",
            )),
        )
    })?;

    Ok(Json(entry))
}

/// Get a provider by name.
///
/// GET /admin/providers/:name
/// Authorization: Bearer <master_key>
pub async fn get_provider_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(name): Path<String>,
) -> Result<Json<crate::provider_store::ProviderEntry>, (StatusCode, Json<ProviderApiError>)> {
    check_master_key_provider(&headers, &state)?;

    let store = state.get_provider_store().ok_or_else(|| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ProviderApiError::new(
                "Provider store not initialized",
                "internal_error",
            )),
        )
    })?;

    let entry = store.get_provider(&name).ok_or_else(|| {
        (
            StatusCode::NOT_FOUND,
            Json(ProviderApiError::new("Provider not found", "not_found")),
        )
    })?;

    Ok(Json(entry))
}

/// List all admin-managed providers.
///
/// GET /admin/providers
/// Authorization: Bearer <master_key>
pub async fn list_providers_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Vec<crate::provider_store::ProviderEntry>>, (StatusCode, Json<ProviderApiError>)> {
    check_master_key_provider(&headers, &state)?;

    let store = state.get_provider_store().ok_or_else(|| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ProviderApiError::new(
                "Provider store not initialized",
                "internal_error",
            )),
        )
    })?;

    Ok(Json(store.list_providers()))
}

/// Delete a provider.
///
/// DELETE /admin/providers/:name
/// Authorization: Bearer <master_key>
pub async fn delete_provider_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(name): Path<String>,
) -> Result<StatusCode, (StatusCode, Json<ProviderApiError>)> {
    check_master_key_provider(&headers, &state)?;

    let store = state.get_provider_store().ok_or_else(|| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ProviderApiError::new(
                "Provider store not initialized",
                "internal_error",
            )),
        )
    })?;

    let deleted = store.delete_provider(&name).await.map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ProviderApiError::new(
                format!("Failed to delete provider: {}", e),
                "delete_failed",
            )),
        )
    })?;

    if deleted {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err((
            StatusCode::NOT_FOUND,
            Json(ProviderApiError::new("Provider not found", "not_found")),
        ))
    }
}

/// Add a model to a provider's shortlist.
///
/// POST /admin/providers/:name/models
/// Authorization: Bearer <master_key>
pub async fn add_model_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(name): Path<String>,
    Json(payload): Json<crate::config::ModelShortlistEntry>,
) -> Result<StatusCode, (StatusCode, Json<ProviderApiError>)> {
    check_master_key_provider(&headers, &state)?;

    let store = state.get_provider_store().ok_or_else(|| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ProviderApiError::new(
                "Provider store not initialized",
                "internal_error",
            )),
        )
    })?;

    store.add_model(&name, payload).await.map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ProviderApiError::new(
                format!("Failed to add model: {}", e),
                "add_model_failed",
            )),
        )
    })?;

    Ok(StatusCode::NO_CONTENT)
}

/// Get all models for a provider.
///
/// GET /admin/providers/:name/models
/// Authorization: Bearer <master_key>
pub async fn get_models_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(name): Path<String>,
) -> Result<Json<Vec<crate::config::ModelShortlistEntry>>, (StatusCode, Json<ProviderApiError>)> {
    check_master_key_provider(&headers, &state)?;

    let store = state.get_provider_store().ok_or_else(|| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ProviderApiError::new(
                "Provider store not initialized",
                "internal_error",
            )),
        )
    })?;

    let models = store.get_models(&name).await.map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ProviderApiError::new(
                format!("Failed to get models: {}", e),
                "get_models_failed",
            )),
        )
    })?;

    Ok(Json(models))
}

/// Remove a model from a provider's shortlist.
///
/// DELETE /admin/providers/:name/models/:model_id
/// Authorization: Bearer <master_key>
pub async fn remove_model_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((name, model_id)): Path<(String, String)>,
) -> Result<StatusCode, (StatusCode, Json<ProviderApiError>)> {
    check_master_key_provider(&headers, &state)?;

    let store = state.get_provider_store().ok_or_else(|| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ProviderApiError::new(
                "Provider store not initialized",
                "internal_error",
            )),
        )
    })?;

    let deleted = store.remove_model(&name, &model_id).await.map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ProviderApiError::new(
                format!("Failed to remove model: {}", e),
                "remove_model_failed",
            )),
        )
    })?;

    if deleted {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err((
            StatusCode::NOT_FOUND,
            Json(ProviderApiError::new("Model not found", "not_found")),
        ))
    }
}

/// Check master key authorization, returning ProviderApiError on failure.
fn check_master_key_provider(
    headers: &HeaderMap,
    state: &AppState,
) -> Result<(), (StatusCode, Json<ProviderApiError>)> {
    check_master_key(headers, state)
        .map_err(|(status, Json(e))| (status, Json(ProviderApiError::new(e.error, e.code))))
}

// ============================================================================
// OAuth Endpoints
// ============================================================================

#[derive(Serialize)]
pub struct OAuthApiError {
    pub error: String,
}

fn oauth_error(
    status: StatusCode,
    message: impl Into<String>,
) -> (StatusCode, Json<OAuthApiError>) {
    (
        status,
        Json(OAuthApiError {
            error: message.into(),
        }),
    )
}

fn check_oauth_available(state: &AppState) -> Result<(), (StatusCode, Json<OAuthApiError>)> {
    if !state.config.keys.enabled {
        return Err(oauth_error(
            StatusCode::SERVICE_UNAVAILABLE,
            "OAuth storage requires keys to be enabled",
        ));
    }
    if state.get_oauth_store().is_none() {
        return Err(oauth_error(
            StatusCode::SERVICE_UNAVAILABLE,
            "OAuth store not initialized",
        ));
    }
    Ok(())
}

fn provider_requires_pkce(provider: OAuthProvider) -> bool {
    matches!(
        provider,
        OAuthProvider::Anthropic | OAuthProvider::OpenAICodex
    )
}

fn resolve_redirect_uri(
    request_uri: Option<String>,
) -> Result<String, (StatusCode, Json<OAuthApiError>)> {
    if let Some(uri) = request_uri {
        return Ok(uri);
    }
    std::env::var("EAVS_OAUTH_REDIRECT_URI")
        .map_err(|_| oauth_error(StatusCode::BAD_REQUEST, "Missing redirect_uri"))
}

fn resolve_anthropic_redirect_uri(request_uri: Option<String>) -> String {
    if let Some(uri) = request_uri {
        return uri;
    }
    std::env::var("EAVS_OAUTH_REDIRECT_URI")
        .ok()
        .filter(|v| !v.trim().is_empty())
        .unwrap_or_else(anthropic::default_redirect_uri)
}

fn split_oauth_code(code: &str) -> (String, Option<String>) {
    let mut parts = code.splitn(2, '#');
    let code_part = parts.next().unwrap_or_default().to_string();
    let state_part = parts.next().map(|s| s.to_string());
    (code_part, state_part)
}

#[derive(Deserialize)]
pub struct OAuthLoginRequest {
    pub user_id: String,
    pub redirect_uri: Option<String>,
    pub extra_data: Option<serde_json::Value>,
}

pub async fn oauth_login_handler(
    State(state): State<AppState>,
    Path(provider): Path<String>,
    Json(payload): Json<OAuthLoginRequest>,
) -> Result<Json<OAuthLoginResponse>, (StatusCode, Json<OAuthApiError>)> {
    check_oauth_available(&state)?;

    let provider = OAuthProvider::from_str(&provider)
        .ok_or_else(|| oauth_error(StatusCode::BAD_REQUEST, "Unknown OAuth provider"))?;

    let client = reqwest::Client::new();
    let state_id = Uuid::new_v4().to_string();
    let now = chrono::Utc::now().timestamp();

    let response = match provider {
        OAuthProvider::GithubCopilot => {
            let config = github_copilot::config_from_env()
                .map_err(|e| oauth_error(StatusCode::BAD_REQUEST, e))?;
            let device = github_copilot::start_device_flow(&client, &config)
                .await
                .map_err(|e| oauth_error(StatusCode::BAD_REQUEST, e))?;
            let instructions = format!(
                "Open {} and enter code {}",
                device.verification_uri, device.user_code
            );
            OAuthLoginResponse {
                auth_url: None,
                instructions,
                verification_uri: Some(device.verification_uri),
                user_code: Some(device.user_code),
                device_code: Some(device.device_code),
                interval: device.interval,
                expires_in: Some(device.expires_in),
                state: None,
                code_verifier: None,
            }
        }
        OAuthProvider::Anthropic => {
            let redirect_uri = resolve_anthropic_redirect_uri(payload.redirect_uri);
            let config = anthropic::config_from_env(redirect_uri.clone())
                .map_err(|e| oauth_error(StatusCode::BAD_REQUEST, e))?;
            let (code_verifier, _) = pkce::generate_pkce_pair();
            let state_id = code_verifier.clone();
            let auth_url = anthropic::build_authorize_url(&config, &state_id, &code_verifier)
                .map_err(|e| oauth_error(StatusCode::BAD_REQUEST, e))?;
            state.oauth_states.insert(
                state_id.clone(),
                OAuthPendingAuth {
                    user_id: payload.user_id.clone(),
                    provider,
                    code_verifier: Some(code_verifier.clone()),
                    redirect_uri: Some(redirect_uri),
                    extra_data: payload.extra_data.clone(),
                    created_at: now,
                },
            );
            OAuthLoginResponse {
                auth_url: Some(auth_url),
                instructions: "Complete the login in your browser and return the code.".to_string(),
                verification_uri: None,
                user_code: None,
                device_code: None,
                interval: None,
                expires_in: None,
                state: Some(state_id),
                code_verifier: Some(code_verifier),
            }
        }
        OAuthProvider::OpenAICodex => {
            let redirect_uri = resolve_redirect_uri(payload.redirect_uri)?;
            let config = openai_codex::config_from_env(redirect_uri.clone())
                .map_err(|e| oauth_error(StatusCode::BAD_REQUEST, e))?;
            let (code_verifier, _) = pkce::generate_pkce_pair();
            let auth_url = openai_codex::build_authorize_url(&config, &state_id, &code_verifier)
                .map_err(|e| oauth_error(StatusCode::BAD_REQUEST, e))?;
            state.oauth_states.insert(
                state_id.clone(),
                OAuthPendingAuth {
                    user_id: payload.user_id.clone(),
                    provider,
                    code_verifier: Some(code_verifier),
                    redirect_uri: Some(redirect_uri),
                    extra_data: payload.extra_data.clone(),
                    created_at: now,
                },
            );
            OAuthLoginResponse {
                auth_url: Some(auth_url),
                instructions: "Complete the login in your browser and return the code.".to_string(),
                verification_uri: None,
                user_code: None,
                device_code: None,
                interval: None,
                expires_in: None,
                state: Some(state_id),
                code_verifier: None,
            }
        }
    };

    Ok(Json(response))
}

#[derive(Deserialize)]
pub struct OAuthCallbackRequest {
    pub code: String,
    pub state: String,
    pub redirect_uri: Option<String>,
}

#[derive(Serialize)]
pub struct OAuthStatusResponse {
    pub status: String,
    pub provider: String,
    pub user_id: String,
}

pub async fn oauth_callback_handler(
    State(state): State<AppState>,
    Json(payload): Json<OAuthCallbackRequest>,
) -> Result<Json<OAuthStatusResponse>, (StatusCode, Json<OAuthApiError>)> {
    check_oauth_available(&state)?;

    let (code, state_override) = split_oauth_code(&payload.code);
    let state_value = state_override.unwrap_or(payload.state);

    let pending = state
        .oauth_states
        .remove(&state_value)
        .map(|(_, v)| v)
        .ok_or_else(|| oauth_error(StatusCode::BAD_REQUEST, "Unknown or expired state"))?;

    let redirect_uri = payload.redirect_uri.or(pending.redirect_uri);
    let client = reqwest::Client::new();
    let store = state.get_oauth_store().unwrap();

    let credentials = match pending.provider {
        OAuthProvider::Anthropic => {
            let redirect_uri = redirect_uri.unwrap_or_else(anthropic::default_redirect_uri);
            let code_verifier = pending
                .code_verifier
                .ok_or_else(|| oauth_error(StatusCode::BAD_REQUEST, "Missing PKCE verifier"))?;
            let config = anthropic::config_from_env(redirect_uri)
                .map_err(|e| oauth_error(StatusCode::BAD_REQUEST, e))?;
            anthropic::exchange_code(
                &client,
                &config,
                &pending.user_id,
                &code,
                &state_value,
                &code_verifier,
            )
            .await
            .map_err(|e| oauth_error(StatusCode::BAD_REQUEST, e))?
        }
        OAuthProvider::OpenAICodex => {
            let redirect_uri = resolve_redirect_uri(redirect_uri)?;
            let code_verifier = pending
                .code_verifier
                .ok_or_else(|| oauth_error(StatusCode::BAD_REQUEST, "Missing PKCE verifier"))?;
            let config = openai_codex::config_from_env(redirect_uri)
                .map_err(|e| oauth_error(StatusCode::BAD_REQUEST, e))?;
            openai_codex::exchange_code(
                &client,
                &config,
                &pending.user_id,
                &payload.code,
                &code_verifier,
            )
            .await
            .map_err(|e| oauth_error(StatusCode::BAD_REQUEST, e))?
        }
        OAuthProvider::GithubCopilot => {
            return Err(oauth_error(
                StatusCode::BAD_REQUEST,
                "GitHub Copilot uses the device code flow",
            ));
        }
    };

    store
        .upsert_credentials(&credentials)
        .await
        .map_err(|e| oauth_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    Ok(Json(OAuthStatusResponse {
        status: "stored".to_string(),
        provider: pending.provider.as_str().to_string(),
        user_id: pending.user_id,
    }))
}

#[derive(Deserialize)]
pub struct OAuthCodeRequest {
    pub user_id: String,
    pub code: String,
    pub state: Option<String>,
    pub redirect_uri: Option<String>,
}

pub async fn oauth_code_handler(
    State(state): State<AppState>,
    Path(provider): Path<String>,
    Json(payload): Json<OAuthCodeRequest>,
) -> Result<Json<OAuthStatusResponse>, (StatusCode, Json<OAuthApiError>)> {
    check_oauth_available(&state)?;

    let provider = OAuthProvider::from_str(&provider)
        .ok_or_else(|| oauth_error(StatusCode::BAD_REQUEST, "Unknown OAuth provider"))?;

    let mut user_id = payload.user_id.clone();
    let mut code_verifier = None;
    let mut redirect_uri = payload.redirect_uri;
    let (code, state_override) = split_oauth_code(&payload.code);

    if let Some(state_id) = state_override.as_ref().or(payload.state.as_ref()) {
        if let Some((_, pending)) = state.oauth_states.remove(state_id) {
            user_id = pending.user_id;
            code_verifier = pending.code_verifier;
            if redirect_uri.is_none() {
                redirect_uri = pending.redirect_uri;
            }
        }
    }

    if provider_requires_pkce(provider) && code_verifier.is_none() {
        return Err(oauth_error(
            StatusCode::BAD_REQUEST,
            "Missing PKCE verifier; use /auth/login to generate state",
        ));
    }

    let client = reqwest::Client::new();
    let store = state.get_oauth_store().unwrap();

    let credentials = match provider {
        OAuthProvider::Anthropic => {
            let redirect_uri = redirect_uri.unwrap_or_else(anthropic::default_redirect_uri);
            let verifier = code_verifier
                .ok_or_else(|| oauth_error(StatusCode::BAD_REQUEST, "Missing PKCE verifier"))?;
            let state_str = state_override
                .as_deref()
                .or(payload.state.as_deref())
                .ok_or_else(|| oauth_error(StatusCode::BAD_REQUEST, "Missing state"))?;
            let config = anthropic::config_from_env(redirect_uri)
                .map_err(|e| oauth_error(StatusCode::BAD_REQUEST, e))?;
            anthropic::exchange_code(&client, &config, &user_id, &code, state_str, &verifier)
                .await
                .map_err(|e| oauth_error(StatusCode::BAD_REQUEST, e))?
        }
        OAuthProvider::OpenAICodex => {
            let redirect_uri = resolve_redirect_uri(redirect_uri)?;
            let verifier = code_verifier
                .ok_or_else(|| oauth_error(StatusCode::BAD_REQUEST, "Missing PKCE verifier"))?;
            let config = openai_codex::config_from_env(redirect_uri)
                .map_err(|e| oauth_error(StatusCode::BAD_REQUEST, e))?;
            openai_codex::exchange_code(&client, &config, &user_id, &code, &verifier)
                .await
                .map_err(|e| oauth_error(StatusCode::BAD_REQUEST, e))?
        }
        OAuthProvider::GithubCopilot => {
            return Err(oauth_error(
                StatusCode::BAD_REQUEST,
                "GitHub Copilot uses the device code flow",
            ));
        }
    };

    store
        .upsert_credentials(&credentials)
        .await
        .map_err(|e| oauth_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    Ok(Json(OAuthStatusResponse {
        status: "stored".to_string(),
        provider: provider.as_str().to_string(),
        user_id,
    }))
}

#[derive(Deserialize)]
pub struct OAuthPollRequest {
    pub user_id: String,
    pub device_code: String,
}

#[derive(Serialize)]
pub struct OAuthPollResponse {
    pub status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub interval: Option<u64>,
}

pub async fn oauth_poll_handler(
    State(state): State<AppState>,
    Path(provider): Path<String>,
    Json(payload): Json<OAuthPollRequest>,
) -> Result<Json<OAuthPollResponse>, (StatusCode, Json<OAuthApiError>)> {
    check_oauth_available(&state)?;

    let provider = OAuthProvider::from_str(&provider)
        .ok_or_else(|| oauth_error(StatusCode::BAD_REQUEST, "Unknown OAuth provider"))?;

    if provider != OAuthProvider::GithubCopilot {
        return Err(oauth_error(
            StatusCode::BAD_REQUEST,
            "Polling is only supported for GitHub Copilot",
        ));
    }

    let client = reqwest::Client::new();
    let config =
        github_copilot::config_from_env().map_err(|e| oauth_error(StatusCode::BAD_REQUEST, e))?;

    match github_copilot::poll_device_flow(&client, &config, &payload.user_id, &payload.device_code)
        .await
        .map_err(|e| oauth_error(StatusCode::BAD_REQUEST, e))?
    {
        github_copilot::DevicePollResult::Pending { interval } => Ok(Json(OAuthPollResponse {
            status: "pending".to_string(),
            interval,
        })),
        github_copilot::DevicePollResult::Authorized(credentials) => {
            let store = state.get_oauth_store().unwrap();
            store
                .upsert_credentials(&credentials)
                .await
                .map_err(|e| oauth_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
            Ok(Json(OAuthPollResponse {
                status: "stored".to_string(),
                interval: None,
            }))
        }
    }
}

pub async fn oauth_status_handler(
    State(state): State<AppState>,
    Path(user_id): Path<String>,
) -> Result<Json<Vec<String>>, (StatusCode, Json<OAuthApiError>)> {
    check_oauth_available(&state)?;
    let store = state.get_oauth_store().unwrap();
    let providers = store
        .list_providers(&user_id)
        .await
        .map_err(|e| oauth_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    Ok(Json(providers))
}

pub async fn oauth_delete_handler(
    State(state): State<AppState>,
    Path((user_id, provider)): Path<(String, String)>,
) -> Result<StatusCode, (StatusCode, Json<OAuthApiError>)> {
    check_oauth_available(&state)?;

    let provider = OAuthProvider::from_str(&provider)
        .ok_or_else(|| oauth_error(StatusCode::BAD_REQUEST, "Unknown OAuth provider"))?;

    let store = state.get_oauth_store().unwrap();
    let deleted = store
        .delete_credentials(&user_id, provider)
        .await
        .map_err(|e| oauth_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    Ok(if deleted {
        StatusCode::NO_CONTENT
    } else {
        StatusCode::NOT_FOUND
    })
}

// ---------------------------------------------------------------------------
// Catalog lookup -- search models.dev for model metadata
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
pub struct CatalogLookupQuery {
    /// Model ID to look up (exact or substring match)
    pub model_id: String,
    /// Optional: restrict to a specific provider in the catalog
    pub provider: Option<String>,
}

/// Catalog model info returned by the lookup endpoint.
#[derive(Serialize)]
pub struct CatalogModelInfo {
    pub id: String,
    pub name: String,
    pub provider: String,
    pub reasoning: bool,
    pub input: Vec<String>,
    pub context_window: u64,
    pub max_tokens: u64,
    pub cost: crate::config::ModelCost,
}

/// `GET /catalog/lookup?model_id=...&provider=...`
///
/// Searches the models.dev catalog for a model by ID.
/// Returns matching model metadata (cost, context window, etc.) that can be
/// used to auto-fill fields when adding models to a provider shortlist.
///
/// Requires master key authentication.
pub async fn catalog_lookup_handler(
    State(state): State<AppState>,
    axum::extract::Query(query): axum::extract::Query<CatalogLookupQuery>,
) -> Result<Json<Vec<CatalogModelInfo>>, (StatusCode, String)> {
    let catalog = match state.catalog() {
        Some(c) => c,
        None => return Ok(Json(Vec::new())),
    };

    let model_id = query.model_id.trim();
    if model_id.is_empty() {
        return Err((StatusCode::BAD_REQUEST, "model_id is required".to_string()));
    }

    let model_id_lower = model_id.to_lowercase();
    let mut results = Vec::new();

    // Iterate all providers and find matching models
    for pid in catalog.provider_ids() {
        if let Some(ref filter_pid) = query.provider {
            if pid != filter_pid.as_str() {
                continue;
            }
        }
        for model in catalog.catalog_models(pid) {
            let id_lower = model.id.to_lowercase();
            if id_lower == model_id_lower || id_lower.contains(&model_id_lower) {
                let input: Vec<String> = if model.modalities.input.is_empty() {
                    vec!["text".to_string()]
                } else {
                    model
                        .modalities
                        .input
                        .iter()
                        .filter(|m| *m == "text" || *m == "image")
                        .cloned()
                        .collect()
                };
                results.push(CatalogModelInfo {
                    id: model.id.clone(),
                    name: if model.name.is_empty() {
                        model.id.clone()
                    } else {
                        model.name.clone()
                    },
                    provider: pid.to_string(),
                    reasoning: model.reasoning,
                    input,
                    context_window: model.limit.context,
                    max_tokens: model.limit.output,
                    cost: crate::config::ModelCost {
                        input: model.cost.input,
                        output: model.cost.output,
                        cache_read: model.cost.cache_read,
                        cache_write: model.cost.cache_write,
                    },
                });
            }
        }
    }

    // Sort: exact match first, then by name
    results.sort_by(|a, b| {
        let a_exact = a.id.to_lowercase() == model_id_lower;
        let b_exact = b.id.to_lowercase() == model_id_lower;
        b_exact.cmp(&a_exact).then(a.name.cmp(&b.name))
    });

    // Limit results
    results.truncate(20);

    Ok(Json(results))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{
        AnalysisConfig, AppConfig, KeysConfig, LoggingConfig, ProviderConfig, ServerConfig,
        StateConfig,
    };
    use crate::state::Injection;
    use std::collections::HashMap;

    fn mock_state() -> AppState {
        let config = AppConfig {
            server: ServerConfig::default(),
            providers: HashMap::new(),
            upstream: HashMap::new(),
            logging: LoggingConfig::default(),
            analysis: AnalysisConfig {
                enabled: true,
                broadcast_channel_size: 10,
                plugins: Vec::new(),
            },
            policy: Default::default(),
            state: StateConfig::default(),
            keys: KeysConfig::default(),
            capture: Default::default(),
            transform: Default::default(),
            network: Default::default(),
            egress: Default::default(),
            mock_responses: Default::default(),
        };
        AppState::new(config)
    }

    fn mock_state_with_providers() -> AppState {
        let mut providers = HashMap::new();
        providers.insert(
            "default".to_string(),
            ProviderConfig {
                type_: "openai".to_string(),
                ..Default::default()
            },
        );
        providers.insert(
            "anthropic".to_string(),
            ProviderConfig {
                type_: "anthropic".to_string(),
                ..Default::default()
            },
        );

        let config = AppConfig {
            server: ServerConfig::default(),
            providers,
            upstream: HashMap::new(),
            logging: LoggingConfig::default(),
            analysis: AnalysisConfig {
                enabled: true,
                broadcast_channel_size: 10,
                plugins: Vec::new(),
            },
            policy: Default::default(),
            state: StateConfig::default(),
            keys: KeysConfig::default(),
            capture: Default::default(),
            transform: Default::default(),
            network: Default::default(),
            egress: Default::default(),
            mock_responses: Default::default(),
        };
        AppState::new(config)
    }

    #[tokio::test]
    async fn test_inject_and_clear() {
        let state = mock_state();
        let conversation_id = "test-conv".to_string();

        // Test injection via conversation store
        let payload = InjectionPayload {
            messages: vec![Injection {
                role: "user".to_string(),
                content: "hi".to_string(),
            }],
        };

        state
            .conversations
            .add_injections(&conversation_id, payload.messages);

        let entry = state.conversations.get(&conversation_id).unwrap();
        assert_eq!(entry.injections.len(), 1);

        // Test clear
        state.conversations.clear(&conversation_id);
        assert!(state.conversations.get(&conversation_id).is_none());
    }

    #[tokio::test]
    async fn test_providers_list() {
        let state = mock_state_with_providers();
        let mut providers: Vec<String> = state.config.providers.keys().cloned().collect();
        providers.sort();

        assert_eq!(providers.len(), 2);
        assert!(providers.contains(&"default".to_string()));
        assert!(providers.contains(&"anthropic".to_string()));
    }

    #[tokio::test]
    async fn test_conversation_stats() {
        let state = mock_state();

        state.conversations.add_injections("conv-1", vec![]);
        state.conversations.add_injections("conv-2", vec![]);

        let (active, created, _, _) = state.conversations.stats();
        assert_eq!(active, 2);
        assert_eq!(created, 2);
    }

    #[tokio::test]
    async fn test_list_conversations() {
        let state = mock_state();

        state.conversations.add_injections("conv-1", vec![]);
        state.conversations.add_injections("conv-2", vec![]);

        let conversations = state.conversations.list_conversations();
        assert_eq!(conversations.len(), 2);
    }

    #[tokio::test]
    async fn test_update_metadata() {
        let state = mock_state();

        state.conversations.add_injections("conv-1", vec![]);
        state.conversations.update_metadata("conv-1", |meta| {
            meta.provider = Some("anthropic".to_string());
            meta.tags.push("test".to_string());
        });

        let entry = state.conversations.get("conv-1").unwrap();
        assert_eq!(entry.metadata.provider, Some("anthropic".to_string()));
        assert!(entry.metadata.tags.contains(&"test".to_string()));
    }

    // ============================================================================
    // Security Tests
    // ============================================================================

    #[test]
    fn test_constant_time_eq_same_length() {
        assert!(constant_time_eq(b"hello", b"hello"));
        assert!(constant_time_eq(b"", b""));
        assert!(constant_time_eq(b"x", b"x"));
    }

    #[test]
    fn test_constant_time_eq_different_content() {
        assert!(!constant_time_eq(b"hello", b"world"));
        assert!(!constant_time_eq(b"hello", b"hellx"));
        assert!(!constant_time_eq(b"a", b"b"));
    }

    #[test]
    fn test_constant_time_eq_different_length() {
        assert!(!constant_time_eq(b"hello", b"hell"));
        assert!(!constant_time_eq(b"", b"x"));
        assert!(!constant_time_eq(b"short", b"much longer string"));
    }

    // ============================================================================
    // Injection Validation Tests
    // ============================================================================

    #[test]
    fn test_validate_injection_valid_roles() {
        let payload = InjectionPayload {
            messages: vec![
                Injection {
                    role: "system".to_string(),
                    content: "Be helpful".to_string(),
                },
                Injection {
                    role: "user".to_string(),
                    content: "Hello".to_string(),
                },
                Injection {
                    role: "assistant".to_string(),
                    content: "Hi there".to_string(),
                },
            ],
        };
        assert!(validate_injection_payload(&payload).is_ok());
    }

    #[test]
    fn test_validate_injection_invalid_role() {
        let payload = InjectionPayload {
            messages: vec![Injection {
                role: "tool".to_string(),
                content: "Bad role".to_string(),
            }],
        };
        let result = validate_injection_payload(&payload);
        assert!(result.is_err());
        assert_eq!(
            result.unwrap_err().1,
            "Invalid role in injection. Allowed: system, user, assistant"
        );
    }

    #[test]
    fn test_validate_injection_too_many_messages() {
        let messages: Vec<Injection> = (0..101)
            .map(|i| Injection {
                role: "user".to_string(),
                content: format!("Message {}", i),
            })
            .collect();
        let payload = InjectionPayload { messages };
        let result = validate_injection_payload(&payload);
        assert!(result.is_err());
        assert_eq!(
            result.unwrap_err().1,
            "Too many messages in injection payload"
        );
    }

    #[test]
    fn test_validate_injection_content_too_large() {
        let large_content = "x".repeat(MAX_INJECTION_CONTENT_LENGTH + 1);
        let payload = InjectionPayload {
            messages: vec![Injection {
                role: "user".to_string(),
                content: large_content,
            }],
        };
        let result = validate_injection_payload(&payload);
        assert!(result.is_err());
        assert_eq!(result.unwrap_err().1, "Injection content too large");
    }

    #[test]
    fn test_validate_injection_empty_payload_ok() {
        let payload = InjectionPayload { messages: vec![] };
        assert!(validate_injection_payload(&payload).is_ok());
    }

    #[test]
    fn test_constant_time_eq_binary_data() {
        let a = vec![0u8, 255, 128, 64, 32];
        let b = vec![0u8, 255, 128, 64, 32];
        let c = vec![0u8, 255, 128, 64, 31];

        assert!(constant_time_eq(&a, &b));
        assert!(!constant_time_eq(&a, &c));
    }
}

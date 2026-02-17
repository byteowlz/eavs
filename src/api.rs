use crate::keys::{CreateKeyRequest, CreateKeyResponse, KeyInfo, KeyPermissions};
use crate::oauth::{
    anthropic, github_copilot, google, openai_codex, pkce, OAuthLoginResponse, OAuthPendingAuth,
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
        /// Model list: config shortlist if set, otherwise full catalog from models.dev
    pub models: Vec<crate::config::ModelShortlistEntry>,
}

/// Get detailed provider information.
///
/// Returns provider types, Pi API mappings, and model lists for models.json generation.
/// Model list logic: config shortlist non-empty = only those models; empty = full models.dev catalog.
pub async fn providers_detail_handler(
    State(state): State<AppState>,
) -> Json<Vec<ProviderDetail>> {
    use crate::model_catalog::eavs_to_catalog_id;
    use crate::provider::ProviderType;

    let catalog = state.catalog();

    let details: Vec<ProviderDetail> = state
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

            ProviderDetail {
                name: name.clone(),
                type_: config.type_.clone(),
                pi_api: pi_api_for_provider(&provider_type),
                oauth: matches!(
                    provider_type,
                    ProviderType::OpenAICodex
                        | ProviderType::GithubCopilot
                        | ProviderType::GoogleGeminiCli
                ),
                has_api_key,
                models,
            }
        })
        .collect();

    Json(details)
}

/// Map eavs provider type to Pi's API type string for models.json.
fn pi_api_for_provider(provider_type: &crate::provider::ProviderType) -> Option<&'static str> {
    use crate::provider::ProviderType;
    match provider_type {
        ProviderType::OpenAI => Some("openai-responses"),
        ProviderType::OpenAIResponses => Some("openai-responses"),
        ProviderType::OpenAICodex => Some("openai-codex-responses"),
        ProviderType::Anthropic => Some("anthropic-messages"),
        ProviderType::Google | ProviderType::GoogleVertex | ProviderType::GoogleGeminiCli => {
            Some("google-generative-ai")
        }
        ProviderType::Azure => Some("openai-responses"),
        ProviderType::Mistral
        | ProviderType::Groq
        | ProviderType::Cerebras
        | ProviderType::XAI
        | ProviderType::OpenRouter
        | ProviderType::OpenAICompatible => Some("openai-completions"),
        ProviderType::GithubCopilot => Some("openai-responses"),
        ProviderType::Bedrock => Some("anthropic-messages"),
        ProviderType::Mock => None,
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
        OAuthProvider::Anthropic
            | OAuthProvider::OpenAICodex
            | OAuthProvider::GoogleGeminiCli
            | OAuthProvider::GoogleAntigravity
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
        OAuthProvider::GoogleGeminiCli | OAuthProvider::GoogleAntigravity => {
            let redirect_uri = resolve_redirect_uri(payload.redirect_uri)?;
            let config = google::config_from_env(provider, redirect_uri.clone())
                .map_err(|e| oauth_error(StatusCode::BAD_REQUEST, e))?;
            let (code_verifier, _) = pkce::generate_pkce_pair();
            let auth_url = google::build_authorize_url(&config, &state_id, &code_verifier)
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
        OAuthProvider::GoogleGeminiCli | OAuthProvider::GoogleAntigravity => {
            let redirect_uri = resolve_redirect_uri(redirect_uri)?;
            let code_verifier = pending
                .code_verifier
                .ok_or_else(|| oauth_error(StatusCode::BAD_REQUEST, "Missing PKCE verifier"))?;
            let config = google::config_from_env(pending.provider, redirect_uri)
                .map_err(|e| oauth_error(StatusCode::BAD_REQUEST, e))?;
            google::exchange_code(
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
        OAuthProvider::GoogleGeminiCli | OAuthProvider::GoogleAntigravity => {
            let redirect_uri = resolve_redirect_uri(redirect_uri)?;
            let verifier = code_verifier
                .ok_or_else(|| oauth_error(StatusCode::BAD_REQUEST, "Missing PKCE verifier"))?;
            let config = google::config_from_env(provider, redirect_uri)
                .map_err(|e| oauth_error(StatusCode::BAD_REQUEST, e))?;
            google::exchange_code(&client, &config, &user_id, &code, &verifier)
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

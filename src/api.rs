use crate::keys::{CreateKeyRequest, CreateKeyResponse, KeyInfo, KeyPermissions};
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

/// Inject messages into a conversation.
pub async fn inject_handler(
    State(state): State<AppState>,
    Path(conversation_id): Path<String>,
    Json(payload): Json<InjectionPayload>,
) -> StatusCode {
    state
        .conversations
        .add_injections(&conversation_id, payload.messages);
    StatusCode::OK
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

/// Check master key authorization.
fn check_master_key(headers: &HeaderMap, state: &AppState) -> Result<(), (StatusCode, Json<KeyApiError>)> {
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
        Some(key) if key == expected_key => Ok(()),
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
            Json(KeyApiError::new("Key store not initialized", "internal_error")),
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
    };

    let response = store.create_key(request).await.map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(KeyApiError::new(format!("Failed to create key: {}", e), "create_failed")),
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
            Json(KeyApiError::new("Key store not initialized", "internal_error")),
        )
    })?;

    let keys = store.list_keys().await.map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(KeyApiError::new(format!("Failed to list keys: {}", e), "list_failed")),
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
            Json(KeyApiError::new("Key store not initialized", "internal_error")),
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
            Json(KeyApiError::new("Key store not initialized", "internal_error")),
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
            Json(KeyApiError::new(format!("Failed to disable key: {}", e), "delete_failed")),
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
            Json(KeyApiError::new("Key store not initialized", "internal_error")),
        )
    })?;

    // Resolve human ID to hash if needed
    let key_hash = if let Some(key) = store.get_by_human_id(&key_id_or_hash) {
        key.key_hash
    } else {
        key_id_or_hash
    };

    let history = store.get_usage_history(&key_hash, Some(100)).await.map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(KeyApiError::new(format!("Failed to get usage: {}", e), "usage_failed")),
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
            Json(KeyApiError::new("Key store not initialized", "internal_error")),
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
    };

    let response = store.create_key(request).await.map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(KeyApiError::new(format!("Failed to create key: {}", e), "create_failed")),
        )
    })?;

    Ok(Json(response))
}

/// Request body for self-provisioning.
#[derive(Deserialize)]
pub struct ProvisionKeyRequest {
    pub name: Option<String>,
    pub metadata: Option<serde_json::Value>,
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
            Json(KeyApiError::new("Key store not initialized", "internal_error")),
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
            Json(KeyApiError::new(format!("Failed to update pricing: {}", e), "update_failed")),
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
            },
            state: StateConfig::default(),
            keys: KeysConfig::default(),
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
            },
            state: StateConfig::default(),
            keys: KeysConfig::default(),
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
}

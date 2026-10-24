use crate::keys::{CreateKeyRequest, CreateKeyResponse, KeyInfo, KeyPermissions};
use crate::oauth::{
    anthropic, github_copilot, openai_codex, pkce, OAuthLoginResponse, OAuthPendingAuth,
    OAuthProvider,
};
use crate::state::{AppState, ConversationMetadata, InjectionPayload};
use axum::{
    body::Body,
    extract::{Path, State},
    http::{HeaderMap, Request, StatusCode},
    middleware::Next,
    response::{
        sse::{Event, Sse},
        IntoResponse, Response,
    },
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

/// Authentication state shared by the listener middleware.
#[derive(Clone)]
pub struct ListenerAuth {
    token: std::sync::Arc<str>,
    /// App handle so `/eavs/maintenance/*` can also accept maintenance-only
    /// virtual keys. Every other route keeps failing closed on them.
    app: Option<std::sync::Arc<AppState>>,
}

impl ListenerAuth {
    pub fn new(token: String) -> Self {
        Self {
            token: token.into(),
            app: None,
        }
    }

    /// Allow maintenance virtual keys on `/eavs/maintenance/*` routes.
    pub fn with_app(mut self, app: std::sync::Arc<AppState>) -> Self {
        self.app = Some(app);
        self
    }
}

/// Exact paths of the embedded console shell. Nothing dynamic, nothing
/// secret — data flows only through authenticated API calls.
const CONSOLE_ASSETS: &[&str] = &[
    "/admin",
    "/admin/app",
    "/admin/manifest.webmanifest",
    "/admin/sw.js",
    "/admin/icon.svg",
];

/// GET /admin — the console shell.
pub async fn console_index_handler() -> impl IntoResponse {
    (
        [(axum::http::header::CONTENT_TYPE, "text/html; charset=utf-8")],
        crate::admin_pwa::INDEX,
    )
}

/// GET /admin/app — console script and styles, inlined into the shell; this
/// path exists so the service worker can cache one URL for offline install.
pub async fn console_app_handler() -> impl IntoResponse {
    (
        [
            (axum::http::header::CONTENT_TYPE, "text/html; charset=utf-8"),
            (axum::http::header::CACHE_CONTROL, "max-age=300"),
        ],
        crate::admin_pwa::INDEX,
    )
}

/// GET /admin/manifest.webmanifest
pub async fn console_manifest_handler() -> impl IntoResponse {
    (
        [(
            axum::http::header::CONTENT_TYPE,
            "application/manifest+json",
        )],
        crate::admin_pwa::MANIFEST,
    )
}

/// GET /admin/sw.js
pub async fn console_sw_handler() -> impl IntoResponse {
    (
        [
            (
                axum::http::header::CONTENT_TYPE,
                "text/javascript; charset=utf-8",
            ),
            (axum::http::header::CACHE_CONTROL, "max-age=300"),
        ],
        crate::admin_pwa::SW,
    )
}

/// GET /admin/icon.svg
pub async fn console_icon_handler() -> impl IntoResponse {
    (
        [(axum::http::header::CONTENT_TYPE, "image/svg+xml")],
        crate::admin_pwa::ICON,
    )
}

/// Identity of a maintenance-key caller, inserted by the listener middleware.
#[derive(Debug, Clone)]
pub struct MaintenanceActor {
    pub key_id: String,
}

/// Resolve `Authorization: Bearer <virtual key>` to a valid maintenance key,
/// or `None`. `X-Eavs-Token` is deliberately not considered here.
async fn maintenance_actor(app: &AppState, headers: &http::HeaderMap) -> Option<MaintenanceActor> {
    let bearer = headers
        .get(http::header::AUTHORIZATION)?
        .to_str()
        .ok()?
        .strip_prefix("Bearer ")?
        .trim();
    if !crate::keys::is_virtual_key(bearer) {
        return None;
    }
    let store = app.get_key_store()?;
    let key = store.lookup_for_auth(bearer).await.ok()??;
    if !key.permissions.maintenance || !key.is_valid() {
        return None;
    }
    Some(MaintenanceActor { key_id: key.key_id })
}

fn listener_token_from_headers(headers: &HeaderMap) -> Option<&str> {
    headers
        .get("x-eavs-token")
        .and_then(|value| value.to_str().ok())
        .or_else(|| {
            headers
                .get("authorization")
                .and_then(|value| value.to_str().ok())
                .and_then(|value| value.strip_prefix("Bearer "))
        })
}

fn listener_request_is_authorized(headers: &HeaderMap, expected: &str) -> bool {
    listener_token_from_headers(headers)
        .is_some_and(|provided| constant_time_eq(provided.as_bytes(), expected.as_bytes()))
}

/// Require the listener capability on every route, including health and metadata.
/// `X-Eavs-Token` avoids colliding with virtual keys carried in `Authorization`;
/// Bearer auth is also accepted for control-plane clients.
pub async fn require_listener_auth(
    State(auth): State<ListenerAuth>,
    mut request: Request<Body>,
    next: Next,
) -> Response {
    if listener_request_is_authorized(request.headers(), &auth.token) {
        return next.run(request).await;
    }
    // The console shell is generic UI code with no data; every request it
    // makes carries the credential and is authenticated above.
    if request.method() == http::Method::GET && CONSOLE_ASSETS.contains(&request.uri().path()) {
        return next.run(request).await;
    }
    if let Some(app) = &auth.app {
        if request.uri().path().starts_with("/eavs/maintenance/") {
            if request.method() == http::Method::OPTIONS {
                return next.run(request).await;
            }
            return match maintenance_actor(app, request.headers()).await {
                Some(actor) => {
                    request.extensions_mut().insert(actor);
                    next.run(request).await
                }
                None => (
                    StatusCode::UNAUTHORIZED,
                    Json(serde_json::json!({
                        "error": "A maintenance key is required for this route",
                        "code": "maintenance_unauthorized"
                    })),
                )
                    .into_response(),
            };
        }
    }

    (
        StatusCode::UNAUTHORIZED,
        Json(serde_json::json!({
            "error": "Listener authentication required",
            "code": "listener_unauthorized"
        })),
    )
        .into_response()
}

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
    pub file_routing_generation: String,
}

/// Zero-config model defaults for alias-based clients.
///
/// Supports optional provider override via `?provider=name`.
pub async fn defaults_handler(
    State(state): State<AppState>,
    axum::extract::Query(query): axum::extract::Query<DefaultsQuery>,
) -> Result<Json<DefaultsResponse>, StatusCode> {
    let state = state.pin_routes();
    let runtime_default_provider = crate::runtime_state::load_runtime_state()
        .and_then(|s| s.default_provider)
        .unwrap_or_default();

    let selected_provider = query
        .provider
        .as_deref()
        .filter(|name| !name.is_empty())
        .map(str::to_owned)
        .or_else(|| (!runtime_default_provider.is_empty()).then_some(runtime_default_provider))
        .or_else(|| state.default_provider_hint())
        .ok_or(StatusCode::NOT_FOUND)?;
    // Missing explicit or removed catalog defaults must not silently select a
    // newly added provider. Selection metadata is not gateway authorization.
    if state.config.resolve_provider(&selected_provider).is_none() {
        return Err(StatusCode::NOT_FOUND);
    }
    let defaults = crate::model_defaults::resolve_model_defaults(
        &state.config,
        state.catalog(),
        Some(&selected_provider),
        None,
    )
    .ok_or(StatusCode::NOT_FOUND)?;

    Ok(Json(DefaultsResponse {
        file_routing_generation: state.file_routing_generation(),
        provider: defaults.provider,
        default: defaults.default,
        fast: defaults.fast,
        reasoning: defaults.reasoning,
        fallback: defaults.fallback,
    }))
}

/// List available providers.
pub async fn providers_handler(State(state): State<AppState>) -> (HeaderMap, Json<Vec<String>>) {
    let state = state.pin_routes();
    let mut providers: Vec<String> = state.config.providers.keys().cloned().collect();
    if let Some(store) = state.get_provider_store() {
        for entry in store.list_providers() {
            if state.config.resolve_provider(&entry.name).is_none()
                && state.resolve_provider(&entry.name).is_some()
            {
                providers.push(entry.name);
            }
        }
    }
    providers.sort();
    providers.dedup();
    let mut headers = HeaderMap::new();
    if let Ok(value) = state.file_routing_generation().parse() {
        headers.insert("x-eavs-file-routing-generation", value);
    }
    (headers, Json(providers))
}

/// Detailed provider info for external integrations (e.g., generating models.json for Pi).
#[derive(Serialize)]
pub struct ProviderDetail {
    /// File declarations only; admin-store/catalog updates are independently live.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub file_routing_generation: Option<String>,
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
pub const fn provider_uses_oauth(provider_type: &crate::provider::ProviderType) -> bool {
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
    let state = state.pin_routes();
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
                file_routing_generation: Some(state.file_routing_generation()),
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
            if state.config.resolve_provider(&entry.name).is_some() {
                continue; // config providers take precedence
            }

            let Ok(config) =
                serde_json::from_value::<crate::config::ProviderConfig>(entry.config.clone())
            else {
                continue;
            };
            let Some(provider_type) = ProviderType::try_from_str(&config.type_) else {
                continue;
            };
            let type_str = config.type_.as_str();
            let has_api_key = entry
                .config
                .get("api_key")
                .and_then(|v| v.as_str())
                .is_some_and(|s| !s.is_empty());

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
                .map(std::string::ToString::to_string);

            let compat =
                serde_json::from_value::<crate::config::ProviderConfig>(entry.config.clone())
                    .ok()
                    .and_then(|c| provider_compat_json(&c));

            details.push(ProviderDetail {
                file_routing_generation: Some(state.file_routing_generation()),
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
/// the provider name and optionally an `api_key` override.
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
/// Authorization: Bearer <`master_key`>
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

    upsert_provider_handler(
        State(state),
        headers,
        Json(crate::provider_store::ProviderRequest {
            name: payload.name,
            config,
            enabled: true,
        }),
    )
    .await
}

/// Map eavs provider type to Pi's API type string for models.json.
pub const fn pi_api_for_provider(
    provider_type: &crate::provider::ProviderType,
) -> Option<&'static str> {
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
        ProviderType::OpenCode | ProviderType::OpenCodeGo => Some("openai-completions"),
        ProviderType::GithubCopilot => Some("openai-responses"),
        ProviderType::TypeSafe
        | ProviderType::ElevenLabs
        | ProviderType::Cohere
        | ProviderType::Voyage
        | ProviderType::Fal
        | ProviderType::Replicate => None,
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
/// Authorization: Bearer <`master_key`>
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
        owner: payload.owner,
        oauth_user: payload.oauth_user,
        oauth_account: payload.oauth_account,
    };

    let response = store.create_key(request).await.map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(KeyApiError::new(
                format!("Failed to create key: {e}"),
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
    #[serde(default)]
    pub owner: Option<String>,
    pub oauth_user: Option<String>,
    pub oauth_account: Option<String>,
}

/// List all virtual API keys.
///
/// GET /admin/keys
/// Authorization: Bearer <`master_key`>
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
                format!("Failed to list keys: {e}"),
                "list_failed",
            )),
        )
    })?;

    Ok(Json(keys))
}

/// Get info about a specific key.
///
/// GET /`admin/keys/:key_hash`
/// Authorization: Bearer <`master_key`>
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
/// DELETE /`admin/keys/:key_id_or_hash`
/// Authorization: Bearer <`master_key`>
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
                format!("Failed to disable key: {e}"),
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
/// GET /`admin/keys/:key_id_or_hash/usage`
/// Authorization: Bearer <`master_key`>
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
                    format!("Failed to get usage: {e}"),
                    "usage_failed",
                )),
            )
        })?;

    Ok(Json(history))
}

/// Body for setting/clearing a key's owner.
#[derive(Deserialize)]
pub struct UpdateOwnerPayload {
    /// New owner label. `null` clears it.
    pub owner: Option<String>,
}

/// Set or clear the organizational owner/tag for a key.
///
/// PUT /`admin/keys/:key_id_or_hash/owner`
/// Authorization: Bearer <`master_key`>
pub async fn update_key_owner_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(key_id_or_hash): Path<String>,
    Json(payload): Json<UpdateOwnerPayload>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<KeyApiError>)> {
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

    let key_hash = if let Some(key) = store.get_by_human_id(&key_id_or_hash) {
        key.key_hash
    } else {
        key_id_or_hash.clone()
    };

    let updated = store
        .update_owner(&key_hash, payload.owner.clone())
        .await
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(KeyApiError::new(
                    format!("Failed to update owner: {e}"),
                    "update_failed",
                )),
            )
        })?;

    if !updated {
        return Err((
            StatusCode::NOT_FOUND,
            Json(KeyApiError::new("Key not found", "not_found")),
        ));
    }

    Ok(Json(serde_json::json!({
        "key_hash": key_hash,
        "owner": payload.owner,
    })))
}

/// Query params for owner usage rollups.
#[derive(Deserialize)]
pub struct OwnerUsageQuery {
    /// Restrict to a single owner. Omit to return every owner bucket.
    pub owner: Option<String>,
    /// Restrict to the last N days. Omit for all-time.
    pub days: Option<u32>,
    /// Optional sub-aggregation within each owner. Accepts `model` and/or
    /// `provider` (comma-separated, e.g. `?breakdown=model,provider`). When
    /// set, each returned row also carries the dimension(s) it was grouped by.
    #[serde(default)]
    pub breakdown: Option<String>,
}

/// Parse the `?breakdown=` query value into `(by_model, by_provider)` flags.
///
/// Tokens are matched case-insensitively and trimmed. Unknown tokens are
/// ignored so the endpoint stays forward-compatible.
fn parse_owner_breakdown(value: Option<&str>) -> (bool, bool) {
    let Some(value) = value else {
        return (false, false);
    };
    let mut by_model = false;
    let mut by_provider = false;
    for tok in value.split(',') {
        match tok.trim().to_ascii_lowercase().as_str() {
            "model" => by_model = true,
            "provider" => by_provider = true,
            _ => {}
        }
    }
    (by_model, by_provider)
}

/// Aggregate usage/cost grouped by owner across all keys.
///
/// `GET /admin/usage/by-owner[?owner=<name>]`
/// Authorization: Bearer <`master_key`>
///
/// Rows with no owner are grouped under the empty-string bucket.
pub async fn owner_usage_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    axum::extract::Query(query): axum::extract::Query<OwnerUsageQuery>,
) -> Result<Json<Vec<crate::keys::OwnerUsage>>, (StatusCode, Json<KeyApiError>)> {
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

    let (by_model, by_provider) = parse_owner_breakdown(query.breakdown.as_deref());
    let rollup = if by_model || by_provider {
        store
            .get_usage_by_owner_breakdown(query.owner.as_deref(), by_model, by_provider, query.days)
            .await
    } else {
        store
            .get_usage_by_owner(query.owner.as_deref(), query.days)
            .await
    };

    let rollup = rollup.map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(KeyApiError::new(
                format!("Failed to get owner usage: {e}"),
                "usage_failed",
            )),
        )
    })?;

    Ok(Json(rollup))
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
        owner: payload.owner,
        oauth_user: payload.oauth_user,
        oauth_account: None,
    };

    let response = store.create_key(request).await.map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(KeyApiError::new(
                format!("Failed to create key: {e}"),
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
    #[serde(default)]
    pub owner: Option<String>,
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
/// Authorization: Bearer <`master_key`>
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
/// Authorization: Bearer <`master_key`>
///
/// Returns the latest observed upstream rate limit quotas for all provider/account
/// pairs that have been seen recently.
/// Maintenance API: announce model availability (e.g. after a GPU loadout
/// switch). Authenticated with the listener token or a maintenance key.
#[derive(serde::Deserialize)]
pub struct MaintenanceAvailabilityRequest {
    pub provider: String,
    /// With `mode: "replace"`: the complete active set. Otherwise: the models
    /// to activate or deactivate.
    #[serde(default)]
    pub models: Vec<String>,
    /// `false` deactivates `models`; `true` re-activates them.
    #[serde(default)]
    pub available: bool,
    /// `"replace"`: `models` is the full active set, everything else on the
    /// provider is deactivated. The default is a targeted update.
    #[serde(default)]
    pub mode: Option<String>,
    #[serde(default)]
    pub reason: Option<String>,
    /// Deactivations lapse after this, e.g. "2h" or "30m".
    #[serde(default)]
    pub ttl: Option<String>,
}

fn valid_model_id(value: &str) -> bool {
    !value.is_empty() && value.len() <= 256 && !value.chars().any(char::is_control)
}

/// Validate the request; `Err` is the response to send.
fn validated_maintenance_request(
    request: &MaintenanceAvailabilityRequest,
) -> Result<Option<chrono::DateTime<chrono::Utc>>, Box<Response>> {
    let invalid = |code: &str, error: &str| {
        (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "error": error, "code": code })),
        )
            .into_response()
    };
    if request.models.len() > 512 || request.models.iter().any(|model| !valid_model_id(model)) {
        return Err(Box::new(invalid(
            "invalid_models",
            "models must be at most 512 non-empty ids without control characters",
        )));
    }
    if request
        .reason
        .as_ref()
        .is_some_and(|reason| reason.len() > 512)
    {
        return Err(Box::new(invalid(
            "invalid_reason",
            "reason must be at most 512 characters",
        )));
    }
    let until = match request.ttl.as_deref() {
        None => None,
        Some(ttl) => match crate::cli::parse_duration_string(ttl) {
            Some(duration) => {
                Some(chrono::Utc::now() + chrono::Duration::from_std(duration).unwrap_or_default())
            }
            None => {
                return Err(Box::new(invalid(
                    "invalid_ttl",
                    "ttl must look like \"30m\", \"2h\" or \"7d\"",
                )))
            }
        },
    };
    Ok(until)
}

fn maintenance_error(code: &str, error: &str, extra: Option<serde_json::Value>) -> Response {
    let mut body = serde_json::json!({ "error": error, "code": code });
    if let Some(extra) = extra {
        body["providers"] = extra;
    }
    (StatusCode::BAD_REQUEST, Json(body)).into_response()
}

/// The model universe a `mode=replace` announce can deactivate: the
/// provider's configured shortlist; when it serves a full catalog instead,
/// the currently announced-active models plus the announced set.
async fn maintenance_universe(state: &AppState, provider: &str, active: &[String]) -> Vec<String> {
    let configured: Vec<String> = state
        .resolve_provider(provider)
        .map(|provider| {
            provider
                .config
                .models
                .iter()
                .map(|model| model.id.clone())
                .collect()
        })
        .unwrap_or_default();
    if !configured.is_empty() {
        return configured;
    }
    let mut universe: Vec<String> = state
        .availability
        .snapshot()
        .await
        .into_iter()
        .filter(|entry| entry.provider == provider)
        .flat_map(|entry| entry.models.into_keys())
        .collect();
    universe.extend(active.iter().cloned());
    universe.sort();
    universe.dedup();
    universe
}

pub async fn maintenance_availability_handler(
    State(state): State<AppState>,
    actor: Option<axum::Extension<MaintenanceActor>>,
    Json(request): Json<MaintenanceAvailabilityRequest>,
) -> Response {
    let set_by = match actor {
        Some(axum::Extension(actor)) => actor.key_id,
        None => "operator".to_owned(),
    };
    let until = match validated_maintenance_request(&request) {
        Ok(until) => until,
        Err(response) => return *response,
    };
    let Some(runtime) = state.resolve_provider(&request.provider) else {
        let configured: Vec<String> = state.config.providers.keys().take(32).cloned().collect();
        return maintenance_error(
            "unknown_provider",
            &format!("Provider '{}' is not configured", request.provider),
            Some(serde_json::Value::from(configured)),
        );
    };
    let replace = request.mode.as_deref() == Some("replace");
    if !replace && !request.available && request.models.is_empty() {
        return maintenance_error(
            "invalid_models",
            "models must not be empty to deactivate; use mode=replace to announce the full active set",
            None,
        );
    }
    let changed = if replace {
        if request.models.is_empty() {
            return maintenance_error(
                "invalid_models",
                "mode=replace needs the complete active model set",
                None,
            );
        }
        let universe = maintenance_universe(&state, &runtime.resolved_name, &request.models).await;
        state
            .availability
            .replace_active(
                &request.provider,
                &request.models,
                &universe,
                request.reason,
                until,
                &set_by,
            )
            .await
    } else if request.available {
        state
            .availability
            .activate(
                &request.provider,
                if request.models.is_empty() {
                    None
                } else {
                    Some(&request.models)
                },
            )
            .await
    } else {
        state
            .availability
            .deactivate(
                &request.provider,
                &request.models,
                request.reason,
                until,
                &set_by,
            )
            .await
    };
    let snapshot = state.availability.snapshot().await;
    (
        StatusCode::OK,
        Json(serde_json::json!({
            "provider": request.provider,
            "changed": changed,
            "availability": snapshot,
        })),
    )
        .into_response()
}

pub async fn maintenance_availability_snapshot_handler(State(state): State<AppState>) -> Response {
    let availability = state.availability.snapshot().await;
    let probe = state.availability.probe_snapshot().await;
    (
        StatusCode::OK,
        Json(serde_json::json!({
            "availability": availability,
            "probe": probe,
            "providers": configured_model_universe(&state),
        })),
    )
        .into_response()
}

/// Every configured provider with its models (id and display name), for
/// maintenance clients and the admin UI. Full-catalog providers carry an
/// empty `models` list; their universe is whatever the probe last saw.
fn configured_model_universe(state: &AppState) -> Vec<serde_json::Value> {
    let mut providers: Vec<serde_json::Value> = state
        .config
        .providers
        .iter()
        .map(|(name, config)| {
            serde_json::json!({
                "provider": name,
                "type": config.type_,
                "probe_enabled": config.probe.as_ref().is_some_and(|probe| probe.enabled),
                "models": config.models.iter().map(|model| serde_json::json!({
                    "id": model.id,
                    "name": model.name,
                })).collect::<Vec<_>>(),
            })
        })
        .collect();
    providers.sort_by(|a, b| {
        a["provider"]
            .as_str()
            .unwrap_or_default()
            .cmp(b["provider"].as_str().unwrap_or_default())
    });
    providers
}

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

/// Update pricing from `LiteLLM`.
///
/// POST /admin/pricing/update
/// Authorization: Bearer <`master_key`>
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
                format!("Failed to update pricing: {e}"),
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

pub mod credentials;
#[cfg(test)]
mod fleet_tests;
pub mod routing;
pub mod serving_catalog;

/// Create or update a provider.
///
/// POST /admin/providers
/// Authorization: Bearer <`master_key`>
pub async fn upsert_provider_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(payload): Json<crate::provider_store::ProviderRequest>,
) -> Result<Json<crate::provider_store::ProviderEntry>, (StatusCode, Json<ProviderApiError>)> {
    save_provider(state, headers, payload, false).await
}

pub async fn create_provider_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(payload): Json<crate::provider_store::ProviderRequest>,
) -> Result<Json<crate::provider_store::ProviderEntry>, (StatusCode, Json<ProviderApiError>)> {
    save_provider(state, headers, payload, true).await
}

async fn save_provider(
    state: AppState,
    headers: HeaderMap,
    payload: crate::provider_store::ProviderRequest,
    create_only: bool,
) -> Result<Json<crate::provider_store::ProviderEntry>, (StatusCode, Json<ProviderApiError>)> {
    check_master_key_provider(&headers, &state)?;
    let coordinator = state.routing.clone();
    let _guard = coordinator.changes.lock().await;
    let file_config = coordinator.snapshot().config;

    let store = state.get_provider_store().ok_or_else(|| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ProviderApiError::new(
                "Provider store not initialized",
                "internal_error",
            )),
        )
    })?;

    if file_config.resolve_provider(&payload.name).is_some() {
        return Err((
            StatusCode::CONFLICT,
            Json(ProviderApiError::new(
                "Provider is TOML-managed; migrate it explicitly before remote updates",
                "provider_source_conflict",
            )),
        ));
    }
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
    if config_json
        .get("type")
        .and_then(|v| v.as_str())
        .is_none_or(str::is_empty)
    {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(ProviderApiError::new(
                "Provider type is required",
                "missing_type",
            )),
        ));
    }

    if payload
        .config
        .get("type")
        .and_then(serde_json::Value::as_str)
        .and_then(crate::provider::ProviderType::try_from_str)
        .is_none()
        || serde_json::from_value::<crate::config::ProviderConfig>(payload.config.clone()).is_err()
    {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(ProviderApiError::new(
                "Invalid provider configuration",
                "invalid_config",
            )),
        ));
    }
    if payload.name.is_empty()
        || payload.name != payload.name.to_ascii_lowercase()
        || !payload
            .name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
    {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(ProviderApiError::new(
                "Provider names must be lowercase letters, digits, hyphens or underscores",
                "invalid_name",
            )),
        ));
    }
    let saved = if create_only {
        store.create_provider(payload).await
    } else {
        store.upsert_provider(payload).await
    };
    let entry = saved.map_err(|error| {
        let status = if matches!(error, crate::provider_store::ProviderStoreError::Conflict) {
            StatusCode::CONFLICT
        } else {
            StatusCode::INTERNAL_SERVER_ERROR
        };
        (
            status,
            Json(ProviderApiError::new(
                "Provider update failed or name already exists",
                "provider_update_failed",
            )),
        )
    })?;

    Ok(Json(entry))
}

/// Probe an unsaved provider config with a bounded, side-effect-free request.
///
/// POST /admin/providers/probe
/// Authorization: Bearer <`master_key`>
///
/// Exercises the real routing/transformation path against the supplied draft
/// config without persisting anything. Fail-closed input validation returns
/// `400`; everything else returns `200` with structured, sanitized diagnostics.
pub async fn probe_provider_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(mut payload): Json<crate::provider_probe::ProbeRequest>,
) -> Result<Json<crate::provider_probe::ProbeResponse>, (StatusCode, Json<ProviderApiError>)> {
    check_master_key_provider(&headers, &state)?;
    let state = state.pin_routes();

    // Edit probes may intentionally omit the secret. Resolve it inside EAVS
    // from the already-active provider so credentials never need to travel
    // back through Oqto or be read from deployment-specific env files.
    if payload.config.api_key.trim().is_empty() {
        if let Some(name) = payload.provider_name.as_deref() {
            if let Some(saved) = state.config.providers.get(name) {
                payload.config.api_key = saved.resolved_api_key();
            }
        }
    }

    match crate::provider_probe::run_probe(&state.upstream, &state.config.network, payload).await {
        Ok(resp) => Ok(Json(resp)),
        Err(rejection) => Err((
            StatusCode::BAD_REQUEST,
            Json(ProviderApiError::new(rejection.error, rejection.code)),
        )),
    }
}

/// Get a provider by name.
///
/// GET /admin/providers/:name
/// Authorization: Bearer <`master_key`>
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
/// Authorization: Bearer <`master_key`>
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
/// Authorization: Bearer <`master_key`>
pub async fn delete_provider_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(name): Path<String>,
) -> Result<StatusCode, (StatusCode, Json<ProviderApiError>)> {
    check_master_key_provider(&headers, &state)?;
    let coordinator = state.routing.clone();
    let _guard = coordinator.changes.lock().await;

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
                format!("Failed to delete provider: {e}"),
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
/// Authorization: Bearer <`master_key`>
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
                format!("Failed to add model: {e}"),
                "add_model_failed",
            )),
        )
    })?;

    Ok(StatusCode::NO_CONTENT)
}

/// Get all models for a provider.
///
/// GET /admin/providers/:name/models
/// Authorization: Bearer <`master_key`>
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
                format!("Failed to get models: {e}"),
                "get_models_failed",
            )),
        )
    })?;

    Ok(Json(models))
}

/// Remove a model from a provider's shortlist.
///
/// DELETE /`admin/providers/:name/models/:model_id`
/// Authorization: Bearer <`master_key`>
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
                format!("Failed to remove model: {e}"),
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

/// Check master key authorization, returning `ProviderApiError` on failure.
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

#[derive(Serialize, Debug)]
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

const fn provider_requires_pkce(provider: OAuthProvider) -> bool {
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

/// Bind an OAuth credential operation to the authenticated operator principal.
///
/// EAVS is a single-user gateway: the listener token identifies one operator
/// principal, which is bound to a single user identity. Every per-user OAuth
/// credential read/write/delete must be scoped to that identity rather than
/// trusting a caller-supplied `user_id` (which would allow cross-user read,
/// overwrite, and delete). If the requested user differs from the bound
/// identity, the operation is rejected.
fn effective_oauth_user(
    state: &AppState,
    requested: &str,
) -> Result<String, (StatusCode, Json<OAuthApiError>)> {
    let effective = state.oauth_default_user();
    if requested != effective {
        return Err(oauth_error(
            StatusCode::FORBIDDEN,
            "Cross-user OAuth operations are not permitted",
        ));
    }
    Ok(effective.to_string())
}

fn split_oauth_code(code: &str) -> (String, Option<String>) {
    let mut parts = code.splitn(2, '#');
    let code_part = parts.next().unwrap_or_default().to_string();
    let state_part = parts.next().map(std::string::ToString::to_string);
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

    // Bind the flow to the authenticated principal rather than trusting the
    // caller-supplied `user_id`.
    let effective_user = effective_oauth_user(&state, &payload.user_id)?;

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
                    user_id: effective_user.clone(),
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
                    user_id: effective_user.clone(),
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

#[derive(Serialize, Debug)]
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

    // The pending auth was bound to the effective user at /auth/login; enforce
    // that here so a stored credential can never be written under another user.
    effective_oauth_user(&state, &pending.user_id)?;

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

    // Bind the write to the authenticated principal: reject a caller-supplied
    // `user_id` that differs from the operator's bound identity.
    let effective_user = effective_oauth_user(&state, &payload.user_id)?;
    let mut user_id = effective_user.clone();
    let mut code_verifier = None;
    let mut redirect_uri = payload.redirect_uri;
    let (code, state_override) = split_oauth_code(&payload.code);

    if let Some(state_id) = state_override.as_ref().or(payload.state.as_ref()) {
        if let Some((_, pending)) = state.oauth_states.remove(state_id) {
            // A pending auth must belong to the same authenticated principal.
            if pending.user_id != effective_user {
                return Err(oauth_error(
                    StatusCode::FORBIDDEN,
                    "Cross-user OAuth operations are not permitted",
                ));
            }
            user_id = effective_user;
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

#[derive(Serialize, Debug)]
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

    // Bind the poll to the authenticated principal rather than trusting the
    // caller-supplied `user_id`.
    let effective_user = effective_oauth_user(&state, &payload.user_id)?;

    let client = reqwest::Client::new();
    let config =
        github_copilot::config_from_env().map_err(|e| oauth_error(StatusCode::BAD_REQUEST, e))?;

    match github_copilot::poll_device_flow(&client, &config, &effective_user, &payload.device_code)
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
    // Reject cross-user reads: only the authenticated principal's own user.
    let effective_user = effective_oauth_user(&state, &user_id)?;
    let store = state.get_oauth_store().unwrap();
    let providers = store
        .list_providers(&effective_user)
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

    // Reject cross-user deletes: only the authenticated principal's own user.
    let effective_user = effective_oauth_user(&state, &user_id)?;

    let store = state.get_oauth_store().unwrap();
    let deleted = store
        .delete_credentials(&effective_user, provider)
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
    use crate::oauth::{OAuthBackend, OAuthStore};
    use crate::state::Injection;
    use axum::{middleware, routing::get, Router};
    use std::collections::HashMap;
    use tower::ServiceExt;

    /// Create a temp-file SQLite-backed OAuth store for handler tests.
    async fn test_oauth_store() -> OAuthStore {
        let path =
            std::env::temp_dir().join(format!("eavs-oauth-test-{}.db", uuid::Uuid::new_v4()));
        OAuthStore::new(&path, OAuthBackend::Sqlite)
            .await
            .expect("temp OAuth store")
    }

    /// An `AppState` with virtual keys enabled and an initialized OAuth store so
    /// the OAuth handlers can pass `check_oauth_available`.
    async fn mock_oauth_state() -> AppState {
        let mut config = AppConfig {
            inference: Default::default(),
            reload_restart_input: None,
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
            delegated_fetch: Default::default(),
            state: StateConfig::default(),
            keys: KeysConfig::default(),
            capture: Default::default(),
            transform: Default::default(),
            network: Default::default(),
            egress: Default::default(),
            mock_responses: Default::default(),
            native_pricing: Default::default(),
        };
        config.keys.enabled = true;
        let state = AppState::new(config);
        state
            .oauth_store
            .set(std::sync::Arc::new(test_oauth_store().await))
            .unwrap();
        state
    }

    /// The operator/auth principal is bound to the configured default user
    /// (`KeysConfig::default()` -> "default"). `effective_oauth_user` rejects a
    /// caller-supplied `user_id` that differs from that bound identity.
    #[test]
    fn effective_oauth_user_binds_to_principal() {
        let state = mock_state();
        assert_eq!(effective_oauth_user(&state, "default").unwrap(), "default");
        let err = effective_oauth_user(&state, "victim").unwrap_err();
        assert_eq!(err.0, StatusCode::FORBIDDEN);
    }

    /// Shared maintenance-API fixture: app router with listener auth, a
    /// maintenance key and an ordinary inference key.
    async fn maintenance_fixture() -> anyhow::Result<(Router, String, String, String)> {
        use crate::keys::{is_virtual_key, CreateKeyRequest, KeyPermissions, KeyStore};

        let mut config = AppConfig {
            inference: crate::inference_ingress::Config::default(),
            reload_restart_input: None,
            server: ServerConfig::default(),
            providers: HashMap::new(),
            upstream: HashMap::new(),
            logging: LoggingConfig::default(),
            analysis: AnalysisConfig::default(),
            policy: crate::policy::PolicyConfig::default(),
            delegated_fetch: crate::delegated_fetch::DelegatedFetchConfig::default(),
            state: StateConfig::default(),
            keys: KeysConfig::default(),
            capture: crate::config::CaptureConfig::default(),
            transform: crate::config::TransformConfig::default(),
            network: crate::config::NetworkConfig::default(),
            egress: crate::config::EgressConfig::default(),
            mock_responses: crate::config::MockResponsesConfig::default(),
            native_pricing: crate::config::NativePricingConfig::default(),
        };
        config.providers.insert(
            "rtx".to_string(),
            ProviderConfig {
                type_: "opencode-go".to_string(),
                base_url: "https://fixture-private-host.invalid/v1".to_string(),
                api_key: "env:FIXTURE_PRIVATE_KEY".to_string(),
                ..Default::default()
            },
        );
        let state = AppState::new(config);
        let store = std::sync::Arc::new(KeyStore::in_memory().await?);
        let maintenance = store
            .create_key(CreateKeyRequest {
                permissions: KeyPermissions {
                    maintenance: true,
                    ..Default::default()
                },
                ..Default::default()
            })
            .await?;
        let inference = store.create_key(CreateKeyRequest::default()).await?;
        assert!(is_virtual_key(&maintenance.key));
        state
            .key_store
            .set(store)
            .map_err(|_| anyhow::anyhow!("key store already set"))?;

        let auth =
            ListenerAuth::new("op-token".to_string()).with_app(std::sync::Arc::new(state.clone()));
        let app = Router::new()
            .route(
                "/eavs/maintenance/availability",
                axum::routing::post(maintenance_availability_handler)
                    .get(maintenance_availability_snapshot_handler),
            )
            .with_state(state)
            .layer(middleware::from_fn_with_state(auth, require_listener_auth));
        Ok((app, maintenance.key, maintenance.key_id, inference.key))
    }

    async fn maintenance_call(
        app: &Router,
        method: &str,
        auth_header: Option<(&str, &str)>,
        body: Option<serde_json::Value>,
    ) -> anyhow::Result<(axum::http::StatusCode, serde_json::Value)> {
        let mut builder = axum::http::Request::builder()
            .method(method)
            .uri("/eavs/maintenance/availability");
        if let Some((name, value)) = auth_header {
            builder = builder.header(name, value);
        }
        if body.is_some() {
            builder = builder.header("content-type", "application/json");
        }
        let request = builder.body(axum::body::Body::from(
            body.map_or_else(String::new, |body| body.to_string()),
        ))?;
        let response = app.clone().oneshot(request).await?;
        let status = response.status();
        let bytes = axum::body::to_bytes(response.into_body(), 64 << 10).await?;
        Ok((status, serde_json::from_slice(&bytes).unwrap_or_default()))
    }

    /// `/eavs/maintenance/*` accepts the operator token and maintenance-only
    /// virtual keys — and nothing else. (Maintenance keys carry no inference
    /// rights; see the validator test.)
    #[tokio::test]
    #[expect(
        clippy::panic_in_result_fn,
        reason = "Behavioral assertions in fallible fixture"
    )]
    async fn maintenance_api_accepts_only_operator_and_maintenance_keys() -> anyhow::Result<()> {
        let (app, maintenance, _maintenance_id, inference) = maintenance_fixture().await?;

        // No credential: fail closed.
        let (status, _) = maintenance_call(&app, "GET", None, None).await?;
        assert_eq!(status, axum::http::StatusCode::UNAUTHORIZED);

        // An ordinary inference key is not a maintenance credential.
        let (status, _) = maintenance_call(
            &app,
            "GET",
            Some(("authorization", &format!("Bearer {inference}"))),
            None,
        )
        .await?;
        assert_eq!(status, axum::http::StatusCode::UNAUTHORIZED);

        // The operator token deactivates; the snapshot reflects it.
        let (status, _) = maintenance_call(
            &app,
            "POST",
            Some(("x-eavs-token", "op-token")),
            Some(serde_json::json!({"provider": "rtx", "models": ["glm-5.3"], "available": false, "reason": "loadout"})),
        )
        .await?;
        assert_eq!(status, axum::http::StatusCode::OK);
        let (status, parsed) =
            maintenance_call(&app, "GET", Some(("x-eavs-token", "op-token")), None).await?;
        assert_eq!(status, axum::http::StatusCode::OK);
        assert_eq!(parsed["availability"][0]["provider"], "rtx");
        assert_eq!(
            parsed["availability"][0]["models"]["glm-5.3"]["reason"],
            "loadout"
        );

        // The maintenance key is accepted as a Bearer credential.
        let (status, _) = maintenance_call(
            &app,
            "GET",
            Some(("authorization", &format!("Bearer {maintenance}"))),
            None,
        )
        .await?;
        assert_eq!(status, axum::http::StatusCode::OK);
        Ok(())
    }

    /// The console shell is reachable without a credential (it holds no
    /// data) while everything behind it stays authenticated.
    #[tokio::test]
    #[expect(clippy::unwrap_used, reason = "router assertions in a plain test")]
    async fn console_shell_is_public_but_data_is_not() {
        let state = mock_state();
        let auth =
            ListenerAuth::new("op-token".to_string()).with_app(std::sync::Arc::new(state.clone()));
        let app = Router::new()
            .route("/admin", get(console_index_handler))
            .route("/admin/keys", get(list_keys_handler))
            .with_state(state)
            .layer(middleware::from_fn_with_state(auth, require_listener_auth));
        let shell = app
            .clone()
            .oneshot(
                axum::http::Request::builder()
                    .uri("/admin")
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(shell.status(), StatusCode::OK);
        assert!(shell
            .headers()
            .get(axum::http::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .is_some_and(|v| v.starts_with("text/html")));

        // Data route without a credential: fail closed.
        let keys = app
            .oneshot(
                axum::http::Request::builder()
                    .uri("/admin/keys")
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(keys.status(), StatusCode::UNAUTHORIZED);
    }

    /// `mode=replace` announces the complete active set of a provider: the
    /// announced models are re-activated (or stay active), everything else
    /// known on the provider is deactivated with the announce's reason/TTL.
    #[tokio::test]
    #[expect(
        clippy::panic_in_result_fn,
        reason = "Behavioral assertions in fallible fixture"
    )]
    async fn maintenance_replace_announces_the_active_set() -> anyhow::Result<()> {
        let (app, maintenance, maintenance_id, _inference) = maintenance_fixture().await?;
        let key = ("authorization", format!("Bearer {maintenance}"));

        // Deactivate glm-5.3 first, then announce kimi-k3 as the active set.
        let (status, _) = maintenance_call(
            &app,
            "POST",
            Some(("x-eavs-token", "op-token")),
            Some(serde_json::json!({"provider": "rtx", "models": ["glm-5.3"], "available": false, "reason": "loadout"})),
        )
        .await?;
        assert_eq!(status, axum::http::StatusCode::OK);

        // A typo'd provider fails loudly (guard for fleet tools).
        let (status, parsed) = maintenance_call(
            &app,
            "POST",
            Some(("x-eavs-token", "op-token")),
            Some(serde_json::json!({"provider": "rtx-typo", "models": ["qwen3.8"], "available": false})),
        )
        .await?;
        assert_eq!(status, axum::http::StatusCode::BAD_REQUEST);
        assert_eq!(parsed["code"], "unknown_provider");

        // Deactivating with no models and no replace is a mistake.
        let (status, parsed) = maintenance_call(
            &app,
            "POST",
            Some((key.0, key.1.as_str())),
            Some(serde_json::json!({"provider": "rtx", "models": [], "available": false})),
        )
        .await?;
        assert_eq!(status, axum::http::StatusCode::BAD_REQUEST);
        assert_eq!(parsed["code"], "invalid_models");

        let (status, parsed) = maintenance_call(
            &app,
            "POST",
            Some((key.0, key.1.as_str())),
            Some(serde_json::json!({
                "provider": "rtx",
                "mode": "replace",
                "models": ["kimi-k3"],
                "available": false,
                "ttl": "2h"
            })),
        )
        .await?;
        assert_eq!(status, axum::http::StatusCode::OK);
        // kimi-k3 is announced active: no deactivation entry. glm-5.3 stays
        // deactivated, refreshed by the announce (new actor, TTL).
        assert_eq!(parsed["changed"], 1);
        assert_eq!(parsed["availability"][0]["provider"], "rtx");
        assert!(parsed["availability"][0]["models"].get("kimi-k3").is_none());
        assert_eq!(
            parsed["availability"][0]["models"]["glm-5.3"]["set_by"],
            maintenance_id
        );
        assert!(parsed["availability"][0]["models"]["glm-5.3"]["until"].is_string());
        Ok(())
    }

    /// A caller cannot read, overwrite/list, or delete credentials for a
    /// `user_id` other than the one bound to its authenticated identity. Every
    /// per-user OAuth endpoint must reject the cross-user operation.
    #[tokio::test]
    async fn oauth_cross_user_operations_are_rejected() {
        let state = mock_oauth_state().await;

        // GET /auth/status/:user_id -- cross-user read rejected.
        let res = oauth_status_handler(State(state.clone()), Path("victim".to_string())).await;
        assert_eq!(res.unwrap_err().0, StatusCode::FORBIDDEN);

        // DELETE /auth/:user_id/:provider -- cross-user delete rejected.
        let res = oauth_delete_handler(
            State(state.clone()),
            Path(("victim".to_string(), "anthropic".to_string())),
        )
        .await;
        assert_eq!(res.unwrap_err().0, StatusCode::FORBIDDEN);

        // POST /auth/login/:provider -- cross-user flow start rejected.
        let req = OAuthLoginRequest {
            user_id: "victim".to_string(),
            redirect_uri: None,
            extra_data: None,
        };
        let res = oauth_login_handler(
            State(state.clone()),
            Path("anthropic".to_string()),
            Json(req),
        )
        .await;
        assert_eq!(res.unwrap_err().0, StatusCode::FORBIDDEN);

        // POST /auth/code/:provider -- cross-user credential write rejected.
        let req = OAuthCodeRequest {
            user_id: "victim".to_string(),
            code: "any".to_string(),
            state: None,
            redirect_uri: None,
        };
        let res = oauth_code_handler(
            State(state.clone()),
            Path("anthropic".to_string()),
            Json(req),
        )
        .await;
        assert_eq!(res.unwrap_err().0, StatusCode::FORBIDDEN);

        // POST /auth/poll/:provider -- cross-user poll rejected.
        let req = OAuthPollRequest {
            user_id: "victim".to_string(),
            device_code: "device".to_string(),
        };
        let res = oauth_poll_handler(
            State(state.clone()),
            Path("github-copilot".to_string()),
            Json(req),
        )
        .await;
        assert_eq!(res.unwrap_err().0, StatusCode::FORBIDDEN);
    }

    /// When the caller uses the authenticated principal's own user id, the
    /// endpoints scope to that identity and proceed (rather than being rejected
    /// as cross-user). /auth/status lists only the principal's providers and
    /// DELETE reaches the store (`NOT_FOUND` on an empty store), proving writes
    /// and deletes are scoped to the authenticated principal.
    #[tokio::test]
    async fn oauth_operations_scope_to_authenticated_principal() {
        let state = mock_oauth_state().await;

        // Same-user status: OK, returns the principal's (empty) provider list.
        let res = oauth_status_handler(State(state.clone()), Path("default".to_string())).await;
        assert_eq!(res.unwrap().0, Vec::<String>::new());

        // Same-user delete: proceeds to the store -> NOT_FOUND (scoped to the
        // principal), not a cross-user FORBIDDEN.
        let res = oauth_delete_handler(
            State(state.clone()),
            Path(("default".to_string(), "anthropic".to_string())),
        )
        .await;
        assert_eq!(res.unwrap(), StatusCode::NOT_FOUND);
    }
    fn mock_state() -> AppState {
        let config = AppConfig {
            inference: Default::default(),
            reload_restart_input: None,
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
            delegated_fetch: Default::default(),
            state: StateConfig::default(),
            keys: KeysConfig::default(),
            capture: Default::default(),
            transform: Default::default(),
            network: Default::default(),
            egress: Default::default(),
            mock_responses: Default::default(),
            native_pricing: Default::default(),
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
            reload_restart_input: None,
            server: ServerConfig::default(),
            inference: Default::default(),
            providers,
            upstream: HashMap::new(),
            logging: LoggingConfig::default(),
            analysis: AnalysisConfig {
                enabled: true,
                broadcast_channel_size: 10,
                plugins: Vec::new(),
            },
            policy: Default::default(),
            delegated_fetch: Default::default(),
            state: StateConfig::default(),
            keys: KeysConfig::default(),
            capture: Default::default(),
            transform: Default::default(),
            network: Default::default(),
            egress: Default::default(),
            mock_responses: Default::default(),
            native_pricing: Default::default(),
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
    async fn listener_auth_denies_provider_and_defaults_disclosure() {
        let state = mock_state_with_providers();
        let app = Router::new()
            .route("/providers", get(providers_handler))
            .route("/defaults", get(defaults_handler))
            .with_state(state)
            .layer(middleware::from_fn_with_state(
                ListenerAuth::new("test-listener-capability".to_string()),
                require_listener_auth,
            ));

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let client = reqwest::Client::new();

        for path in ["/providers", "/defaults"] {
            let response = client
                .get(format!("http://{addr}{path}"))
                .send()
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
            let body = response.text().await.unwrap();
            assert!(!body.contains("anthropic"));
            assert!(!body.contains("default"));
        }

        let response = client
            .get(format!("http://{addr}/providers"))
            .header("X-Eavs-Token", "test-listener-capability")
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let providers: Vec<String> = response.json().await.unwrap();
        assert!(providers.contains(&"anthropic".to_string()));

        server.abort();
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
                content: format!("Message {i}"),
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

    // ============================================================================
    // Owner Breakdown Parsing (eavs-5gf3)
    // ============================================================================

    #[test]
    fn test_parse_owner_breakdown_none() {
        assert_eq!(parse_owner_breakdown(None), (false, false));
        assert_eq!(parse_owner_breakdown(Some("")), (false, false));
    }

    #[test]
    fn test_parse_owner_breakdown_single() {
        assert_eq!(parse_owner_breakdown(Some("model")), (true, false));
        assert_eq!(parse_owner_breakdown(Some("provider")), (false, true));
        // case-insensitive + surrounding whitespace
        assert_eq!(parse_owner_breakdown(Some("  MODEL ")), (true, false));
    }

    #[test]
    fn test_parse_owner_breakdown_both() {
        assert_eq!(parse_owner_breakdown(Some("model,provider")), (true, true));
        assert_eq!(parse_owner_breakdown(Some("provider, model")), (true, true));
    }

    #[test]
    fn test_parse_owner_breakdown_ignores_unknown() {
        // Unknown tokens are silently ignored (forward-compatible).
        assert_eq!(parse_owner_breakdown(Some("model,foo")), (true, false));
        assert_eq!(parse_owner_breakdown(Some("bogus")), (false, false));
    }
}

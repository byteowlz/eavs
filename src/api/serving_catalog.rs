//! Read-only, scope-filtered chat candidates. Never resolves upstream credentials.
use crate::{
    config::{ModelCost, ModelShortlistEntry, ProviderConfig},
    keys::KeyPermissions,
    provider::ProviderType,
    state::AppState,
};
use axum::{
    extract::State,
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};

const MAX_PROVIDERS: usize = 256;
const MAX_MODELS: usize = 4096;
const MAX_BYTES: usize = 4 * 1024 * 1024;

#[derive(Serialize)]
pub struct ServingCatalog {
    pub schema_version: u8,
    pub scope: &'static str,
    pub consistency: &'static str,
    pub file_routing_generation: String,
    pub providers: Vec<ServingProvider>,
}

#[derive(Serialize)]
pub struct ServingProvider {
    pub id: String,
    pub api: &'static str,
    pub base_path: String,
    pub request_path: String,
    pub operation: &'static str,
    pub metadata_source: &'static str,
    pub compat: Option<serde_json::Value>,
    pub models: Vec<ServingModel>,
}

#[derive(Serialize)]
pub struct ServingModel {
    pub id: String,
    pub name: String,
    pub reasoning: bool,
    pub input: Vec<String>,
    pub context_window: u64,
    pub max_tokens: u64,
    pub compat: BTreeMap<String, serde_json::Value>,
    pub pricing: Option<Estimate>,
}

#[derive(Serialize)]
pub struct Estimate {
    pub status: &'static str,
    pub unit: &'static str,
    pub rates: ModelCost,
}

#[derive(Clone, Copy)]
pub enum Failure {
    Unauthorized,
    Unavailable,
    TooLarge,
}
impl Failure {
    fn response(self) -> Response {
        let (status, code) = match self {
            Self::Unauthorized => (StatusCode::UNAUTHORIZED, "catalog_unauthorized"),
            Self::Unavailable => (StatusCode::SERVICE_UNAVAILABLE, "catalog_unavailable"),
            Self::TooLarge => (StatusCode::SERVICE_UNAVAILABLE, "catalog_limit_exceeded"),
        };
        (
            status,
            Json(serde_json::json!({"error": "Serving catalog unavailable", "code": code})),
        )
            .into_response()
    }
}

async fn permissions(state: &AppState, token: &str) -> Result<KeyPermissions, Failure> {
    if !state.config.keys.enabled {
        return Err(Failure::Unavailable);
    }
    let store = state.get_key_store().ok_or(Failure::Unavailable)?;
    // This is metadata authentication, not inference admission: no rate/budget
    // counters are charged, and a spent key may still discover its scope.
    if !crate::keys::is_virtual_key(token) {
        return Err(Failure::Unauthorized);
    }
    let key = store
        .lookup_for_auth(token)
        .await
        .map_err(|_| Failure::Unavailable)?
        .ok_or(Failure::Unauthorized)?;
    if !key.is_valid() {
        return Err(Failure::Unauthorized);
    }
    Ok(key.permissions)
}

fn route(config: &ProviderConfig) -> Option<(&'static str, &'static str)> {
    use ProviderType as P;
    // Google URL-model routing and non-chat operation metadata need dedicated
    // contracts. Do not infer these from a generic Pi API mapping.
    match ProviderType::try_from_str(&config.type_)? {
        P::OpenAI | P::OpenAIResponses | P::Azure | P::GithubCopilot => {
            Some(("openai-responses", "/v1/responses"))
        }
        P::OpenAICodex => Some(("openai-codex-responses", "/v1/responses")),
        P::Anthropic => Some(("anthropic-messages", "/v1/messages")),
        P::Mistral
        | P::Groq
        | P::Cerebras
        | P::XAI
        | P::OpenRouter
        | P::Bedrock
        | P::OpenAICompatible
        | P::OpenCode
        | P::OpenCodeGo
        | P::Mock => Some(("openai-completions", "/v1/chat/completions")),
        _ => None,
    }
}

fn public_text(value: &str, limit: usize) -> bool {
    !value.is_empty() && value.len() <= limit && !value.chars().any(char::is_control)
}
fn provider_id(value: &str) -> bool {
    public_text(value, 128)
        && !matches!(value, "." | ".." | "v1" | "v1beta" | "v1alpha")
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
}
fn estimate(cost: &ModelCost) -> Option<Estimate> {
    let rates = [cost.input, cost.output, cost.cache_read, cost.cache_write];
    if rates.iter().all(|v| v.is_finite() && *v >= 0.0) && rates.iter().any(|v| *v > 0.0) {
        Some(Estimate {
            status: "unverified_estimate",
            unit: "USD_per_million_tokens",
            rates: cost.clone(),
        })
    } else {
        None // missing/default zero does not establish free inference
    }
}

fn model_compat(
    values: &std::collections::HashMap<String, serde_json::Value>,
) -> BTreeMap<String, serde_json::Value> {
    let mut result = BTreeMap::new();
    for name in [
        "supportsStore",
        "supportsDeveloperRole",
        "supportsReasoningEffort",
        "supportsUsageInStreaming",
        "supportsStrictMode",
        "requiresToolResultName",
        "requiresAssistantAfterToolResult",
        "requiresThinkingAsText",
    ] {
        if let Some(value) = values.get(name).and_then(serde_json::Value::as_bool) {
            result.insert(name.to_owned(), serde_json::Value::Bool(value));
        }
    }
    if let Some(value @ ("max_tokens" | "max_completion_tokens")) = values
        .get("maxTokensField")
        .and_then(serde_json::Value::as_str)
    {
        result.insert("maxTokensField".into(), value.into());
    }
    result
}

fn provider_compat(config: &ProviderConfig) -> Option<serde_json::Value> {
    let mut value = super::provider_compat_json(config)?;
    if let Some(fields) = value.as_object_mut() {
        if !fields
            .get("max_tokens_field")
            .and_then(serde_json::Value::as_str)
            .is_some_and(|v| matches!(v, "max_tokens" | "max_completion_tokens"))
        {
            fields.remove("max_tokens_field");
        }
    }
    Some(value)
}

struct Candidate {
    id: String,
    config: ProviderConfig,
    models: Vec<ModelShortlistEntry>,
    metadata_source: &'static str,
}

async fn collect_candidates(state: &AppState) -> Result<Vec<Candidate>, Failure> {
    let mut names: BTreeSet<String> = state.config.providers.keys().cloned().collect();
    if let Some(store) = state.get_provider_store() {
        names.extend(store.list_providers().into_iter().map(|p| p.name));
    }
    if names.len() > MAX_PROVIDERS {
        return Err(Failure::TooLarge);
    }
    let mut candidates = Vec::new();
    let mut seen = BTreeSet::new();
    let mut count = 0;
    for name in names {
        let Some(runtime) = state.resolve_provider(&name) else {
            continue;
        };
        let id = runtime.resolved_name;
        if !provider_id(&id) || !seen.insert(id.to_lowercase()) || route(&runtime.config).is_none()
        {
            continue;
        }
        let mut models = runtime.config.models.clone();
        let mut metadata_source = "operator_declaration";
        if state.config.resolve_provider(&id).is_none() {
            if let Some(store) = state.get_provider_store() {
                let stored = store
                    .get_models(&id)
                    .await
                    .map_err(|_| Failure::Unavailable)?;
                if !stored.is_empty() {
                    models = stored;
                }
            }
        }
        if models.is_empty() {
            metadata_source = "catalog_tool_text_candidate";
            if let Some(catalog) = state.catalog() {
                models = catalog
                    .catalog_models(crate::model_catalog::eavs_to_catalog_id(
                        &id,
                        &runtime.config.type_,
                    ))
                    .into_iter()
                    .filter(|m| m.tool_call && m.modalities.output.iter().any(|v| v == "text"))
                    .map(crate::model_catalog::CatalogModel::to_shortlist_entry)
                    .collect();
            }
        }
        count += models.len();
        if count > MAX_MODELS {
            return Err(Failure::TooLarge);
        }
        candidates.push(Candidate {
            id,
            config: runtime.config,
            models,
            metadata_source,
        });
    }
    Ok(candidates)
}

fn project(
    state: &AppState,
    scope: &KeyPermissions,
    candidates: Vec<Candidate>,
) -> Vec<ServingProvider> {
    let mut providers = Vec::new();
    for candidate in candidates {
        if !scope.is_provider_allowed(&candidate.id) {
            continue;
        }
        let Some((api, path)) = route(&candidate.config) else {
            continue;
        };
        let mut models = BTreeMap::new();
        for model in candidate.models {
            if !public_text(&model.id, 512) || !scope.is_model_allowed(&model.id) {
                continue;
            }
            let mut body = serde_json::json!({"model": model.id});
            if state
                .config
                .policy
                .apply(&candidate.id, path, &mut body)
                .is_err()
                || body.get("model").and_then(serde_json::Value::as_str) != Some(model.id.as_str())
            {
                continue; // no alias/rewrite metadata or inherited capabilities
            }
            let input: Vec<String> = model
                .input
                .into_iter()
                .filter(|v| matches!(v.as_str(), "text" | "image"))
                .collect();
            if !input.iter().any(|v| v == "text") {
                continue;
            }
            let name = if public_text(&model.name, 512) {
                model.name
            } else {
                model.id.clone()
            };
            models.insert(
                model.id.clone(),
                ServingModel {
                    id: model.id,
                    name,
                    reasoning: model.reasoning,
                    input,
                    context_window: model.context_window,
                    max_tokens: model.max_tokens,
                    compat: model_compat(&model.compat),
                    pricing: estimate(&model.cost),
                },
            );
        }
        if !models.is_empty() {
            providers.push(ServingProvider {
                base_path: format!("/{}/v1", candidate.id),
                request_path: format!("/{}{path}", candidate.id),
                compat: provider_compat(&candidate.config),
                id: candidate.id,
                api,
                operation: "chat",
                metadata_source: candidate.metadata_source,
                models: models.into_values().collect(),
            });
        }
    }
    providers.sort_by(|a, b| a.id.cmp(&b.id));
    providers
}

/// Shared operation for future exports/provider libraries. File routes are
/// pinned once; DB and key stores remain independently live, not one revision.
pub async fn build(state: &AppState, token: &str) -> Result<ServingCatalog, Failure> {
    let state = state.pin_routes();
    permissions(&state, token).await?; // authenticate before reading inventories
    let candidates = collect_candidates(&state).await?;
    // Reauthenticate after DB awaits; project with current scope synchronously.
    let scope = permissions(&state, token).await?;
    let providers = project(&state, &scope, candidates);
    let result = ServingCatalog {
        schema_version: 1,
        scope: "key-filtered-chat-candidates",
        consistency: "file-snapshot-with-live-stores",
        file_routing_generation: state.file_routing_generation(),
        providers,
    };
    if serde_json::to_vec(&result)
        .map_err(|_| Failure::Unavailable)?
        .len()
        > MAX_BYTES
    {
        return Err(Failure::TooLarge);
    }
    Ok(result)
}

pub async fn handler(State(state): State<AppState>, headers: HeaderMap) -> Response {
    let token = headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "));
    let result = match token {
        Some(token) => build(&state, token).await,
        None => Err(Failure::Unauthorized),
    };
    let mut response = match result {
        Ok(catalog) => Json(catalog).into_response(),
        Err(error) => error.response(),
    };
    response.headers_mut().insert(
        "cache-control",
        axum::http::HeaderValue::from_static("private, no-store"),
    );
    response.headers_mut().insert(
        "vary",
        axum::http::HeaderValue::from_static("Authorization, X-Eavs-Token"),
    );
    response
}

#[cfg(test)]
mod tests;

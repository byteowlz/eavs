//! Per-instance, write-only credential management for admin-managed providers.
use super::{check_master_key_provider, ProviderApiError};
use crate::provider_store::{CredentialRevision, ProviderStoreError};
use crate::state::AppState;
use axum::{
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    Json,
};
use serde::{Deserialize, Serialize};

#[derive(Serialize)]
pub struct ProviderSource {
    pub name: String,
    pub provider_type: String,
    pub source: &'static str,
    pub revision: Option<String>,
    pub enabled: bool,
}

pub async fn sources(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Vec<ProviderSource>>, (StatusCode, Json<ProviderApiError>)> {
    check_master_key_provider(&headers, &state)?;
    let state = state.pin_routes();
    let mut providers: Vec<_> = state
        .config
        .providers
        .iter()
        .map(|(name, config)| ProviderSource {
            name: name.clone(),
            provider_type: public_provider_type(&config.type_),
            source: "toml",
            revision: None,
            enabled: true,
        })
        .collect();
    if let Some(store) = state.get_provider_store() {
        for entry in store.list_all_providers().await.map_err(|_| {
            error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Provider inventory unavailable",
                "inventory_unavailable",
            )
        })? {
            if state.config.resolve_provider(&entry.name).is_some() {
                continue;
            }
            providers.push(ProviderSource {
                name: entry.name,
                provider_type: entry
                    .config
                    .get("type")
                    .and_then(serde_json::Value::as_str)
                    .map_or_else(|| "unknown".into(), public_provider_type),
                source: "admin",
                revision: Some(entry.updated_at.to_rfc3339()),
                enabled: entry.enabled,
            });
        }
    }
    providers.sort_by(|left, right| left.name.cmp(&right.name));
    Ok(Json(providers))
}

fn public_provider_type(value: &str) -> String {
    if crate::provider::ProviderType::try_from_str(value).is_some() {
        value.to_ascii_lowercase()
    } else {
        "unknown".into()
    }
}

type ApiResult = Result<Json<CredentialRevision>, (StatusCode, Json<ProviderApiError>)>;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RotateCredential {
    pub expected_revision: String,
    pub api_key: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RollbackCredential {
    pub expected_revision: String,
}

pub async fn rotate(
    State(state): State<AppState>,
    Path(name): Path<String>,
    headers: HeaderMap,
    Json(request): Json<RotateCredential>,
) -> ApiResult {
    check_master_key_provider(&headers, &state)?;
    let coordinator = state.routing.clone();
    let _guard = coordinator.changes.lock().await;
    let state = state.pin_routes();
    check_managed(&state, &name)?;
    if request.api_key.trim().is_empty()
        || request.api_key.len() > 16_384
        || request.api_key.chars().any(char::is_control)
        || request.api_key.starts_with("env:")
        || request.api_key.starts_with("keychain:")
    {
        return Err(error(
            StatusCode::BAD_REQUEST,
            "Provide a non-empty literal API key, not a secret reference",
            "invalid_credential",
        ));
    }
    let store = state.get_provider_store().ok_or_else(|| {
        error(
            StatusCode::SERVICE_UNAVAILABLE,
            "Provider store unavailable",
            "store_unavailable",
        )
    })?;
    store
        .rotate_api_key(&name, &request.expected_revision, &request.api_key)
        .await
        .map(Json)
        .map_err(|error| store_error(&error))
}

pub async fn rollback(
    State(state): State<AppState>,
    Path(name): Path<String>,
    headers: HeaderMap,
    Json(request): Json<RollbackCredential>,
) -> ApiResult {
    check_master_key_provider(&headers, &state)?;
    let coordinator = state.routing.clone();
    let _guard = coordinator.changes.lock().await;
    let state = state.pin_routes();
    check_managed(&state, &name)?;
    let store = state.get_provider_store().ok_or_else(|| {
        error(
            StatusCode::SERVICE_UNAVAILABLE,
            "Provider store unavailable",
            "store_unavailable",
        )
    })?;
    store
        .rollback_api_key(&name, &request.expected_revision)
        .await
        .map(Json)
        .map_err(|error| store_error(&error))
}

fn check_managed(state: &AppState, name: &str) -> Result<(), (StatusCode, Json<ProviderApiError>)> {
    if state
        .routing
        .snapshot()
        .config
        .resolve_provider(name)
        .is_some()
    {
        return Err(error(
            StatusCode::CONFLICT,
            "Provider is TOML-managed; migrate it explicitly before remote rotation",
            "provider_source_conflict",
        ));
    }
    Ok(())
}

fn store_error(error: &ProviderStoreError) -> (StatusCode, Json<ProviderApiError>) {
    match error {
        ProviderStoreError::Conflict => self::error(
            StatusCode::CONFLICT,
            "Revision changed or rollback unavailable; fetch current metadata",
            "revision_conflict",
        ),
        ProviderStoreError::NotFound => self::error(
            StatusCode::NOT_FOUND,
            "Provider not found",
            "provider_not_found",
        ),
        _ => self::error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Credential update failed; previous configuration retained",
            "credential_update_failed",
        ),
    }
}

fn error(status: StatusCode, message: &str, code: &str) -> (StatusCode, Json<ProviderApiError>) {
    (status, Json(ProviderApiError::new(message, code)))
}

//! Authenticated adapter over the shared file-provider reload operation.
use super::{check_master_key_provider, ProviderApiError};
use crate::{
    routing::{Failure, ReloadRequest, ReloadResult},
    state::AppState,
};
use axum::{
    extract::{rejection::JsonRejection, State},
    http::{HeaderMap, StatusCode},
    Json,
};
use serde::Serialize;

type Error = (StatusCode, Json<ProviderApiError>);
fn error(failure: Failure) -> Error {
    let status = match failure {
        Failure::GenerationConflict | Failure::CandidateConflict | Failure::SourceConflict => {
            StatusCode::CONFLICT
        }
        Failure::Unavailable => StatusCode::SERVICE_UNAVAILABLE,
        _ => StatusCode::BAD_REQUEST,
    };
    (status, Json(ProviderApiError::new("File routing reload rejected; active generation retained. Check the error code and repair declarations before previewing again.", failure.code())))
}

#[derive(Serialize)]
pub struct Metadata {
    pub scope: &'static str,
    pub generation: String,
    pub reload_configured: bool,
}
pub async fn metadata(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Metadata>, Error> {
    check_master_key_provider(&headers, &state)?;
    Ok(Json(Metadata {
        scope: "file-providers",
        generation: state.routing.snapshot().generation,
        reload_configured: state.routing.configured(),
    }))
}
pub async fn reload(
    State(state): State<AppState>,
    headers: HeaderMap,
    request: Result<Json<ReloadRequest>, JsonRejection>,
) -> Result<Json<ReloadResult>, Error> {
    check_master_key_provider(&headers, &state)?;
    let Json(request) = request.map_err(|_| error(Failure::InvalidConfiguration))?;
    state
        .routing
        .reload(&state, request)
        .await
        .map(Json)
        .map_err(error)
}

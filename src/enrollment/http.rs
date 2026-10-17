//! Bounded POST-only enrollment surface. Listener middleware remains mandatory.
use super::{Administrator, EnrollmentError, TokenRequest};
use crate::state::AppState;
use axum::{
    body::Bytes,
    extract::{rejection::BytesRejection, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use serde::Serialize;

fn response<T: Serialize>(status: StatusCode, value: T) -> Response {
    let mut response = (status, Json(value)).into_response();
    response.headers_mut().insert(
        "cache-control",
        axum::http::HeaderValue::from_static("no-store"),
    );
    response
}
fn failure(error: EnrollmentError) -> Response {
    let (status, code) = match error {
        EnrollmentError::Unauthorized => (StatusCode::UNAUTHORIZED, "unauthorized"),
        EnrollmentError::Invalid => (StatusCode::BAD_REQUEST, "invalid_enrollment"),
        EnrollmentError::Unavailable => (StatusCode::SERVICE_UNAVAILABLE, "enrollment_unavailable"),
    };
    response(status, serde_json::json!({"error":code}))
}
fn admin(headers: &HeaderMap, state: &AppState) -> Result<Administrator, EnrollmentError> {
    let supplied = headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.strip_prefix("Bearer "))
        .unwrap_or("");
    Administrator::authenticate(state.config.keys.resolved_master_key().as_deref(), supplied)
}
fn parse<T: serde::de::DeserializeOwned>(
    body: Result<Bytes, BytesRejection>,
) -> Result<T, EnrollmentError> {
    let body = body.map_err(|_| EnrollmentError::Invalid)?;
    if body.len() > 2048 {
        return Err(EnrollmentError::Invalid);
    }
    serde_json::from_slice(&body).map_err(|_| EnrollmentError::Invalid)
}
pub async fn issue(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Result<Bytes, BytesRejection>,
) -> Response {
    let result = async {
        let admin = admin(&headers, &state)?;
        let request = parse(body)?;
        if !state.config.keys.enabled {
            return Err(EnrollmentError::Unavailable);
        }
        state
            .get_key_store()
            .ok_or(EnrollmentError::Unavailable)?
            .invite_device(&admin, request)
            .await
    }
    .await;
    match result {
        Ok(v) => response(StatusCode::CREATED, v),
        Err(e) => failure(e),
    }
}
pub async fn revoke(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Result<Bytes, BytesRejection>,
) -> Response {
    let result = async {
        let admin = admin(&headers, &state)?;
        let request: TokenRequest = parse(body)?;
        if !state.config.keys.enabled {
            return Err(EnrollmentError::Unavailable);
        }
        state
            .get_key_store()
            .ok_or(EnrollmentError::Unavailable)?
            .revoke_device_invitation(&admin, &request.token)
            .await
    }
    .await;
    match result {
        Ok(()) => response(StatusCode::OK, serde_json::json!({"revoked":true})),
        Err(e) => failure(e),
    }
}
pub async fn redeem(
    State(state): State<AppState>,
    body: Result<Bytes, BytesRejection>,
) -> Response {
    let result = async {
        let request: TokenRequest = parse(body)?;
        if !state.config.keys.enabled {
            return Err(EnrollmentError::Unavailable);
        }
        state
            .get_key_store()
            .ok_or(EnrollmentError::Unavailable)?
            .redeem_device(&request.token)
            .await
    }
    .await;
    match result {
        Ok(v) => response(StatusCode::CREATED, v),
        Err(e) => failure(e),
    }
}

#[cfg(test)]
mod revoke_tests;
#[cfg(test)]
mod tests;

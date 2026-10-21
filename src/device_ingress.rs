//! Optional loopback-only device surface. Operator listener authority is never
//! distributed to phones. Native inference admission remains in pi_messages/proxy.
use crate::{enrollment, state::AppState};
use axum::{
    body::{Body, Bytes},
    extract::{rejection::BytesRejection, DefaultBodyLimit, Path, State},
    http::{header::CACHE_CONTROL, HeaderMap, HeaderValue, Request, StatusCode},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use std::{
    collections::VecDeque,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

const CLAIMS_PER_MINUTE: usize = 60;

pub fn listener_address(value: &str) -> Result<std::net::SocketAddr, String> {
    let address: std::net::SocketAddr = value
        .parse()
        .map_err(|_| "Use an explicit loopback IP:port".to_owned())?;
    if !address.ip().is_loopback() || address.port() == 0 {
        return Err("Device ingress requires a nonzero loopback IP:port behind HTTPS".to_owned());
    }
    Ok(address)
}
#[derive(Clone)]
struct Ingress {
    gateway: AppState,
    claims: Arc<Mutex<VecDeque<Instant>>>,
}
fn denial(status: StatusCode, code: &'static str) -> Response {
    (status, Json(serde_json::json!({"error":code}))).into_response()
}
async fn device_authorized(state: &AppState, headers: &HeaderMap) -> bool {
    if !state.config.keys.enabled {
        return false;
    }
    let Some(token) = headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
    else {
        return false;
    };
    if !crate::keys::is_virtual_key(token) {
        return false;
    }
    let key = match state.get_key_store() {
        Some(store) => store.lookup_for_auth(token).await.ok().flatten(),
        None => None,
    };
    key.is_some_and(|key| {
        key.is_valid()
            && key.metadata.get("kind").and_then(serde_json::Value::as_str)
                == Some("device_enrollment")
    })
}
async fn private_response(request: Request<Body>, next: Next) -> Response {
    let mut response = next.run(request).await;
    response
        .headers_mut()
        .insert(CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
}
async fn redeem(State(ingress): State<Ingress>, body: Result<Bytes, BytesRejection>) -> Response {
    let allowed = ingress
        .claims
        .lock()
        .map(|mut claims| {
            let now = Instant::now();
            while claims
                .front()
                .is_some_and(|at| now.saturating_duration_since(*at) >= Duration::from_secs(60))
            {
                claims.pop_front();
            }
            if claims.len() >= CLAIMS_PER_MINUTE {
                return false;
            }
            claims.push_back(now);
            true
        })
        .unwrap_or(false);
    if !allowed {
        return denial(StatusCode::TOO_MANY_REQUESTS, "device_claim_throttled");
    }
    enrollment::http::redeem(State(ingress.gateway), body).await
}
async fn revoke_self(
    State(ingress): State<Ingress>,
    headers: HeaderMap,
    body: Result<Bytes, BytesRejection>,
) -> Response {
    if !device_authorized(&ingress.gateway, &headers).await {
        return denial(StatusCode::UNAUTHORIZED, "device_unauthorized");
    }
    if body.is_err() {
        return denial(StatusCode::BAD_REQUEST, "invalid_device_request");
    }
    let token = headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "));
    let key = match (token, ingress.gateway.get_key_store()) {
        (Some(token), Some(store)) => store.lookup_for_auth(token).await.ok().flatten(),
        _ => None,
    };
    let Some(key) = key else {
        return denial(StatusCode::UNAUTHORIZED, "device_unauthorized");
    };
    let Some(store) = ingress.gateway.get_key_store() else {
        return denial(StatusCode::SERVICE_UNAVAILABLE, "device_unavailable");
    };
    // No caller-supplied key ID/hash is accepted. This can disable only the
    // authenticated device key; it is not an administrative key operation.
    match store.disable_key(&key.key_hash).await {
        Ok(_) => (
            StatusCode::OK,
            Json(serde_json::json!({"revoked":true,"key_id":key.key_id})),
        )
            .into_response(),
        Err(_) => denial(StatusCode::SERVICE_UNAVAILABLE, "device_unavailable"),
    }
}

async fn catalog(State(ingress): State<Ingress>, headers: HeaderMap) -> Response {
    if !device_authorized(&ingress.gateway, &headers).await {
        return denial(StatusCode::UNAUTHORIZED, "device_unauthorized");
    }
    crate::api::serving_catalog::handler(State(ingress.gateway), headers).await
}
async fn inference(
    State(ingress): State<Ingress>,
    Path(provider): Path<String>,
    request: Request<Body>,
) -> Response {
    if !device_authorized(&ingress.gateway, request.headers()).await {
        return denial(StatusCode::UNAUTHORIZED, "device_unauthorized");
    }
    crate::pi_messages::handle(ingress.gateway, request, Some(provider)).await
}
/// This Router must only be exposed behind the operator-selected HTTPS ingress.
/// It deliberately has no fallback/default proxy or administrative routes.
pub fn router(state: AppState) -> Router {
    Router::new()
        .route(
            "/device-enrollment/redeem",
            post(redeem).layer(DefaultBodyLimit::max(2048)),
        )
        .route(
            "/v1/eavs/device/revoke",
            post(revoke_self).layer(DefaultBodyLimit::max(2048)),
        )
        .route("/v1/eavs/catalog", get(catalog))
        .route("/:provider/v1/pi/messages", post(inference))
        .with_state(Ingress {
            gateway: state,
            claims: Arc::new(Mutex::new(VecDeque::new())),
        })
        .layer(middleware::from_fn(private_response))
}

#[cfg(test)]
mod tests;

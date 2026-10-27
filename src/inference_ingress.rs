//! Default-disabled, ordinary-key-only native inference surface (ADR0020).
use crate::{api::serving_catalog, state::AppState};
use axum::{
    body::Body,
    extract::{Request, State},
    http::{header, HeaderMap, HeaderValue, StatusCode},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use serde::Deserialize;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{net::SocketAddr, sync::Arc};

/// Independent listener and browser policy; omission disables the listener.
#[derive(Debug, Clone, Default, Deserialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub listen: Option<String>,
    pub cors_allowed_origins: Vec<String>,
}
impl Config {
    pub fn address(&self) -> Result<Option<SocketAddr>, &'static str> {
        self.listen
            .as_deref()
            .map(|value| {
                let address: SocketAddr = value
                    .parse()
                    .map_err(|_| "inference.listen requires loopback IP:port")?;
                if !address.ip().is_loopback() || address.port() == 0 {
                    return Err("inference.listen requires nonzero loopback IP:port");
                }
                Ok(address)
            })
            .transpose()
    }
}
/// Allowlisted native semantics supplied only by this isolated ingress.
#[derive(Clone)]
pub struct NativeHeaders(pub HeaderMap);
/// An explicitly unlimited operator body setting must not unbound public ingress.
#[derive(Clone, Copy)]
pub struct NativeBodyLimit(pub usize);

#[derive(Clone)]
struct Ingress {
    state: AppState,
    forbidden: Arc<Vec<String>>,
}
fn error(status: StatusCode, code: &'static str) -> Response {
    (
        status,
        Json(json!({"error":{"message":code,"type":"inference_ingress_error","code":code}})),
    )
        .into_response()
}
fn token(headers: &HeaderMap) -> Option<&str> {
    if headers.contains_key("x-eavs-token") {
        return None;
    }
    let bearer = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "));
    let api_key = headers.get("x-api-key").and_then(|v| v.to_str().ok());
    match (bearer, api_key) {
        (Some(a), Some(b)) if a == b => Some(a),
        (Some(a), None) => Some(a),
        (None, Some(b)) if !headers.contains_key(header::AUTHORIZATION) => Some(b),
        _ => None,
    }
}
async fn authenticate(ingress: &Ingress, headers: &HeaderMap) -> Option<crate::keys::VirtualKey> {
    let token = token(headers)?;
    let fingerprint = crate::keys::hash_key(token);
    if ingress.forbidden.contains(&fingerprint) || !crate::keys::is_virtual_key(token) {
        return None;
    }
    ingress
        .state
        .get_key_store()?
        .lookup_for_auth(token)
        .await
        .ok()
        .flatten()
        .filter(|key| {
            key.is_valid()
                && key.metadata.get("kind").and_then(Value::as_str) != Some("device_enrollment")
        })
}
fn semantic_headers(key_hash: &str, headers: &HeaderMap) -> HeaderMap {
    let mut forwarded = HeaderMap::new();
    if let Some(value) = headers.get("idempotency-key") {
        let mut hash = Sha256::new();
        hash.update(b"eavs-inference-idempotency-v1\0");
        hash.update(key_hash.as_bytes());
        hash.update(b"\0");
        hash.update(value.as_bytes());
        if let Ok(value) = HeaderValue::from_str(&format!("eavs-{}", hex::encode(hash.finalize())))
        {
            forwarded.insert("idempotency-key", value);
        }
    }
    if let Some(value) = headers.get("x-client-request-id") {
        forwarded.insert("x-client-request-id", value.clone());
    }
    forwarded
}
async fn auth(State(ingress): State<Ingress>, mut request: Request, next: Next) -> Response {
    let Some(key) = authenticate(&ingress, request.headers()).await else {
        return error(StatusCode::UNAUTHORIZED, "invalid_api_key");
    };
    let path = crate::inference_routes::api_path(request.uri().path());
    let metadata = matches!(
        *request.method(),
        axum::http::Method::GET | axum::http::Method::HEAD
    ) && (matches!(path, "/v1/models" | "/v1/eavs/catalog")
        || path.starts_with("/v1/models/"));
    // Lifecycle handlers still verify MAC-authenticated owner/model bindings.
    // They must remain reachable for already-created jobs after a cap changes.
    let lifecycle = path.starts_with("/v1/eavs/jobs/")
        && (*request.method() == axum::http::Method::GET
            || (*request.method() == axum::http::Method::POST && path.ends_with("/cancel")));
    if !metadata
        && !lifecycle
        && (key.permissions.max_budget_usd.is_some() || key.permissions.tpm_limit.is_some())
    {
        return error(StatusCode::FORBIDDEN, "durable_accounting_required");
    }
    let semantic = semantic_headers(&key.key_hash, request.headers());
    request.extensions_mut().insert(NativeHeaders(semantic));
    let limit = ingress.state.config.server.max_body_size;
    request
        .extensions_mut()
        .insert(NativeBodyLimit(if limit == 0 {
            10 * 1024 * 1024
        } else {
            limit
        }));
    // Normalize only the caller credential, never the native body, model, path,
    // multipart boundary or WebSocket upgrade. Shared handlers perform effective
    // provider/model authorization and operation-specific accounting admission.
    let Some(token) = token(request.headers()).map(str::to_owned) else {
        return error(StatusCode::UNAUTHORIZED, "invalid_api_key");
    };
    let Ok(value) = HeaderValue::from_str(&format!("Bearer {token}")) else {
        return error(StatusCode::UNAUTHORIZED, "invalid_api_key");
    };
    request.headers_mut().insert(header::AUTHORIZATION, value);
    request.headers_mut().remove("x-api-key");
    next.run(request).await
}
async fn private(request: Request, next: Next) -> Response {
    let mut response = next.run(request).await;
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("private, no-store"),
    );
    response.headers_mut().append(
        header::VARY,
        HeaderValue::from_static("Authorization, X-Api-Key"),
    );
    response
}
fn public_id(provider: &str, model: &str) -> String {
    // Catalog provider IDs exclude '/' so this mapping is injective even for
    // physical model IDs containing slashes. Never infer a route by splitting it.
    format!("{provider}/{model}")
}
fn catalog_error(failure: serving_catalog::Failure) -> Response {
    match failure {
        serving_catalog::Failure::Unauthorized => {
            error(StatusCode::UNAUTHORIZED, "invalid_api_key")
        }
        _ => error(StatusCode::SERVICE_UNAVAILABLE, "catalog_unavailable"),
    }
}
async fn models(State(ingress): State<Ingress>, headers: HeaderMap) -> Response {
    let Some(token) = token(&headers) else {
        return error(StatusCode::UNAUTHORIZED, "invalid_api_key");
    };
    match serving_catalog::build(&ingress.state, token).await {
        Ok(catalog) => Json(
            json!({"object":"list", "data":catalog.providers.iter().filter(|p| ["/v1/chat/completions", "/v1/messages", "/v1/responses"].iter().any(|path| supports(p, path))).flat_map(|provider| {
            provider.models.iter().map(|model| json!({"id":public_id(&provider.id, &model.id),
                "object":"model", "created":0, "owned_by":provider.id, "eavs_api":provider.api}))
        }).collect::<Vec<_>>()}),
        )
        .into_response(),
        Err(failure) => catalog_error(failure),
    }
}
fn supports(provider: &serving_catalog::ServingProvider, path: &str) -> bool {
    match provider.api {
        "anthropic-messages" => path == "/v1/messages",
        "openai-completions" => path == "/v1/chat/completions",
        "openai-responses" => matches!(path, "/v1/chat/completions" | "/v1/responses"),
        // Codex requires a distinct transport path, not this generic endpoint.
        _ => false,
    }
}
async fn inference(State(ingress): State<Ingress>, request: Request) -> Response {
    let state = ingress.state.pin_routes();
    let (mut parts, body) = request.into_parts();
    let Some(token) = token(&parts.headers).map(str::to_owned) else {
        return error(StatusCode::UNAUTHORIZED, "invalid_api_key");
    };
    let limit = state.config.server.max_body_size;
    // Unlike the operator listener, an unbounded public body is not supported.
    let limit = if limit == 0 { 10 * 1024 * 1024 } else { limit };
    let Ok(bytes) = axum::body::to_bytes(body, limit).await else {
        return error(StatusCode::PAYLOAD_TOO_LARGE, "request_too_large");
    };
    let Ok(mut value) = serde_json::from_slice::<Value>(&bytes) else {
        return error(StatusCode::BAD_REQUEST, "invalid_json");
    };
    let Some(model_id) = value.get("model").and_then(Value::as_str) else {
        return error(StatusCode::BAD_REQUEST, "model_required");
    };
    let catalog = match serving_catalog::build(&state, &token).await {
        Ok(catalog) => catalog,
        Err(failure) => return catalog_error(failure),
    };
    let selected = catalog
        .providers
        .iter()
        .filter(|p| supports(p, parts.uri.path()))
        .find_map(|p| {
            p.models
                .iter()
                .find(|m| public_id(&p.id, &m.id) == model_id)
                .map(|m| (&p.id, &m.id))
        });
    let Some((provider, model)) = selected else {
        return error(StatusCode::FORBIDDEN, "model_not_allowed");
    };
    // Reauthenticate after body/catalog awaits. Native admission rechecks again.
    let Some(key) = authenticate(&ingress, &parts.headers).await else {
        return error(StatusCode::UNAUTHORIZED, "invalid_api_key");
    };
    if key.permissions.max_budget_usd.is_some() || key.permissions.tpm_limit.is_some() {
        return error(StatusCode::FORBIDDEN, "durable_accounting_required");
    }
    value["model"] = Value::String(model.clone());
    for name in [
        "x-provider",
        "x-original-host",
        "x-api-key",
        "x-eavs-token",
        "content-length",
        "content-encoding",
    ] {
        parts.headers.remove(name);
    }
    let Ok(auth) = HeaderValue::from_str(&format!("Bearer {token}")) else {
        return error(StatusCode::UNAUTHORIZED, "invalid_api_key");
    };
    let Ok(provider_header) = HeaderValue::from_str(provider) else {
        return error(StatusCode::SERVICE_UNAVAILABLE, "catalog_unavailable");
    };
    parts.headers.insert(header::AUTHORIZATION, auth);
    parts.headers.insert("x-provider", provider_header);
    parts.headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/json"),
    );
    let Ok(body) = serde_json::to_vec(&value) else {
        return error(StatusCode::BAD_REQUEST, "invalid_json");
    };
    let semantic = semantic_headers(&key.key_hash, &parts.headers);
    parts.extensions.insert(NativeHeaders(semantic));
    let request = Request::from_parts(parts, Body::from(body));
    match crate::proxy::proxy_handler_pinned(state, request, None).await {
        Ok(response) | Err(response) => response,
    }
}
/// Build only the inference surface; never merge this router with operator routes.
pub fn router(state: AppState, forbidden: Vec<String>) -> Result<Router, &'static str> {
    if !state.config.keys.enabled
        || state.get_key_store().is_none()
        || state.get_key_validator().is_none()
    {
        return Err("Inference ingress requires enabled initialized KeyStore and validator");
    }
    state.config.inference.address()?;
    let cors = crate::cors::layer(&state.config.inference.cors_allowed_origins)?
        .allow_methods([
            axum::http::Method::GET,
            axum::http::Method::POST,
            axum::http::Method::PUT,
            axum::http::Method::PATCH,
            axum::http::Method::DELETE,
            axum::http::Method::HEAD,
        ])
        .allow_headers([
            header::AUTHORIZATION,
            header::CONTENT_TYPE,
            header::ACCEPT,
            // Firefox preflights the SDK's explicit User-Agent (Chromium drops it).
            header::USER_AGENT,
            axum::http::HeaderName::from_static("x-api-key"),
            axum::http::HeaderName::from_static("x-provider"),
            axum::http::HeaderName::from_static("anthropic-version"),
            axum::http::HeaderName::from_static("anthropic-beta"),
            axum::http::HeaderName::from_static("anthropic-dangerous-direct-browser-access"),
            axum::http::HeaderName::from_static("x-client-request-id"),
            axum::http::HeaderName::from_static("idempotency-key"),
            axum::http::HeaderName::from_static("x-stainless-lang"),
            axum::http::HeaderName::from_static("x-stainless-package-version"),
            axum::http::HeaderName::from_static("x-stainless-os"),
            axum::http::HeaderName::from_static("x-stainless-arch"),
            axum::http::HeaderName::from_static("x-stainless-runtime"),
            axum::http::HeaderName::from_static("x-stainless-runtime-version"),
            axum::http::HeaderName::from_static("x-stainless-retry-count"),
            axum::http::HeaderName::from_static("x-stainless-timeout"),
            axum::http::HeaderName::from_static("x-stainless-helper-method"),
            axum::http::HeaderName::from_static("x-stainless-async"),
            axum::http::HeaderName::from_static("x-stainless-read-timeout"),
            axum::http::HeaderName::from_static("x-stainless-connect-timeout"),
        ]);
    let native = crate::inference_routes::router(&state).with_state(state.clone());
    let ingress = Ingress {
        state,
        forbidden: Arc::new(
            forbidden
                .into_iter()
                .map(|token| crate::keys::hash_key(&token))
                .collect(),
        ),
    };
    Ok(Router::new()
        .route("/v1/models", get(models))
        .route("/v1/chat/completions", post(inference))
        .route("/v1/messages", post(inference))
        .route("/v1/responses", post(inference))
        .with_state(ingress.clone())
        .merge(native)
        .route_layer(middleware::from_fn_with_state(ingress, auth))
        .route_layer(cors)
        .layer(middleware::from_fn(private)))
}

#[cfg(test)]
mod tests;

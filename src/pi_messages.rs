//! Additional Pi client interface over shared inference admission and native streams.
pub mod canonical;
mod framing;
mod native;
mod request;
#[cfg(test)]
mod response;
mod signatures;
pub mod stream;
mod wire;

use crate::{provider::ProviderType, state::AppState};
use axum::{
    body::{to_bytes, Body},
    http::{Request, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use serde_json::json;

#[derive(Clone)]
pub struct Interface;
pub fn valid_pricing(pricing: &crate::keys::pricing::ModelPricing) -> bool {
    [pricing.input_cost_per_token, pricing.output_cost_per_token]
        .iter()
        .all(|v| v.is_finite() && *v > 0.0)
}
const fn supported(kind: ProviderType) -> bool {
    !matches!(
        kind,
        ProviderType::TypeSafe
            | ProviderType::ElevenLabs
            | ProviderType::Cohere
            | ProviderType::Voyage
            | ProviderType::Fal
            | ProviderType::Replicate
            | ProviderType::Bedrock
            | ProviderType::Mock
    )
}
pub fn matches(path: &str, provider: Option<&str>) -> bool {
    let path = provider
        .and_then(|p| path.strip_prefix(&format!("/{p}")))
        .unwrap_or(path);
    path == "/v1/pi/messages"
}
struct Failure {
    status: StatusCode,
    code: &'static str,
}
impl Failure {
    fn response(self) -> Response {
        (self.status,Json(json!({"error":{"message":"Pi interface request rejected; see error code", "code":self.code}}))).into_response()
    }
}
const fn error(status: StatusCode, code: &'static str) -> Failure {
    Failure { status, code }
}
pub async fn handle(state: AppState, req: Request<Body>, provider: Option<String>) -> Response {
    match handle_inner(state, req, provider).await {
        Ok(response) => response,
        Err(failure) => failure.response(),
    }
}
async fn preflight(
    state: &AppState,
    method: &http::Method,
    headers: &http::HeaderMap,
    provider: Option<String>,
) -> Result<String, Failure> {
    if method != http::Method::POST {
        return Err(error(
            StatusCode::METHOD_NOT_ALLOWED,
            "pi_method_not_allowed",
        ));
    }
    if state.config.keys.enabled {
        let token = headers
            .get("authorization")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.strip_prefix("Bearer "));
        let key = match (token, state.get_key_store()) {
            (Some(token), Some(store)) => store.lookup_for_auth(token).await.ok().flatten(),
            _ => None,
        }
        .filter(crate::keys::VirtualKey::is_valid)
        .ok_or_else(|| error(StatusCode::UNAUTHORIZED, "pi_unauthorized"))?;
        if key.permissions.max_budget_usd.is_some() || key.permissions.tpm_limit.is_some() {
            return Err(error(
                StatusCode::BAD_REQUEST,
                "pi_capped_accounting_unavailable",
            ));
        }
    }
    let selected = provider
        .or_else(|| {
            headers
                .get("x-provider")
                .and_then(|v| v.to_str().ok())
                .map(str::to_owned)
        })
        .ok_or_else(|| error(StatusCode::BAD_REQUEST, "pi_explicit_provider_required"))?;
    let runtime = state
        .resolve_provider(&selected)
        .ok_or_else(|| error(StatusCode::BAD_REQUEST, "pi_provider_unavailable"))?;
    if !supported(runtime.config.provider_type()) {
        return Err(error(StatusCode::BAD_REQUEST, "pi_provider_not_supported"));
    }
    Ok(runtime.resolved_name)
}
async fn handle_inner(
    state: AppState,
    req: Request<Body>,
    provider: Option<String>,
) -> Result<Response, Failure> {
    let provider = Some(preflight(&state, req.method(), req.headers(), provider).await?);
    let (mut parts, body) = req.into_parts();
    let limit = state.config.server.max_body_size;
    let bytes = to_bytes(body, if limit == 0 { 10 * 1024 * 1024 } else { limit })
        .await
        .map_err(|_| error(StatusCode::PAYLOAD_TOO_LARGE, "pi_body_limit"))?;
    let value = serde_json::from_slice(&bytes)
        .map_err(|_| error(StatusCode::BAD_REQUEST, "pi_invalid_json"))?;
    let payload = request::convert(&value)
        .map_err(|()| error(StatusCode::BAD_REQUEST, "pi_unsupported_request"))?;
    let model = payload["model"]
        .as_str()
        .ok_or_else(|| error(StatusCode::BAD_REQUEST, "pi_invalid_model"))?;
    if !state
        .pricing
        .get(model)
        .await
        .is_some_and(|p| valid_pricing(&p))
    {
        return Err(error(StatusCode::BAD_REQUEST, "pi_pricing_unavailable"));
    }
    let path = provider.as_ref().map_or_else(
        || "/v1/chat/completions".to_owned(),
        |p| format!("/{p}/v1/chat/completions"),
    );
    parts.uri = path
        .parse()
        .map_err(|_| error(StatusCode::BAD_REQUEST, "pi_invalid_route"))?;
    parts.headers.remove("content-length");
    parts.headers.insert(
        "content-type",
        http::HeaderValue::from_static("application/json"),
    );
    parts.extensions.insert(Interface);
    let bytes = serde_json::to_vec(&payload)
        .map_err(|_| error(StatusCode::BAD_REQUEST, "pi_invalid_request"))?;
    let result = crate::proxy::proxy_handler_pinned(
        state,
        Request::from_parts(parts, Body::from(bytes)),
        provider,
    )
    .await
    .map_err(|response| error(response.status(), "pi_inference_rejected"))?;
    if !result.status().is_success() {
        return Err(error(result.status(), "pi_inference_rejected"));
    }
    Ok(result)
}
#[cfg(test)]
mod native_safety_tests;
#[cfg(test)]
mod stream_tests;
#[cfg(test)]
mod tests;

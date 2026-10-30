//! Shared native inference routes. Neither listener may maintain a partial copy.
//! Operator/configuration/credential routes must never be registered here.
use crate::{api, jobs, proxy, state::AppState};
use axum::{
    extract::{Request, State},
    middleware::{self, Next},
    response::Response,
    routing::{any, get, post},
    Router,
};

// The legacy operator fallback was not an authority boundary. Never turn an
// ordinary inference key into a general upstream account/billing/admin proxy.
pub(crate) fn api_path(path: &str) -> &str {
    if path.starts_with("/v1/") || path.starts_with("/v2/") {
        path
    } else {
        path.strip_prefix('/')
            .and_then(|p| p.find('/').map(|i| &p[i..]))
            .unwrap_or("")
    }
}
async fn inference_path_only(
    State(state): State<AppState>,
    request: Request,
    next: Next,
) -> Response {
    let path = api_path(request.uri().path());
    if !crate::proxy::is_inference_path(path)
        && !matches!(
            path,
            "/v1/models"
                | "/v1/chat/completions"
                | "/v1/completions"
                | "/v1/messages"
                | "/v1/v1/messages"
                | "/v1/messages/count_tokens"
                | "/v1/v1/messages/count_tokens"
                | "/v1/complete"
                | "/v1/responses"
                | "/v1/pi/messages"
        )
        && !path.starts_with("/v1/models/")
        && !(crate::proxy::google_model_from_path(path).is_some()
            && [":generateContent", ":streamGenerateContent", ":countTokens"]
                .iter()
                .any(|action| path.ends_with(action)))
    {
        let path = request.uri().path().to_owned();
        return proxy::unknown_endpoint_response(&state, request.headers(), &path).await;
    }
    next.run(request).await
}

pub fn router(state: &AppState) -> Router<AppState> {
    Router::new()
        .route("/v1/eavs/catalog", get(api::serving_catalog::handler))
        .route("/v1/eavs/pools", get(crate::pools::status_handler))
        .route("/v1/realtime", get(proxy::ws_proxy_handler))
        .route(
            "/:provider/v1/realtime",
            get(proxy::provider_ws_proxy_handler),
        )
        .route(
            "/:provider/v1/speech-to-text/realtime",
            get(proxy::provider_ws_proxy_handler),
        )
        .route(
            "/:provider/v1/text-to-speech/:voice_id/stream-input",
            get(proxy::provider_voice_ws_proxy_handler),
        )
        .route(
            "/:provider/v1/text-to-speech/:voice_id/multi-stream-input",
            get(proxy::provider_voice_ws_proxy_handler),
        )
        .route(
            "/v1/codex/responses",
            get(proxy::codex_ws_handler).post(proxy::proxy_handler),
        )
        .route(
            "/:provider/v1/codex/responses",
            get(proxy::provider_codex_ws_handler).post(proxy::provider_codex_sse_handler),
        )
        .route("/:provider/v1/eavs/jobs", post(jobs::submit))
        .route("/:provider/v1/eavs/jobs/:handle", get(jobs::status))
        .route("/:provider/v1/eavs/jobs/:handle/result", get(jobs::result))
        .route("/:provider/v1/eavs/jobs/:handle/cancel", post(jobs::cancel))
        .route(
            "/:provider/v1/*path",
            any(proxy::provider_proxy_handler).layer(middleware::from_fn_with_state(
                state.clone(),
                inference_path_only,
            )),
        )
        .route(
            "/:provider/v2/*path",
            any(proxy::provider_proxy_handler).layer(middleware::from_fn_with_state(
                state.clone(),
                inference_path_only,
            )),
        )
        .route(
            "/v1/*path",
            any(proxy::proxy_handler).layer(middleware::from_fn_with_state(
                state.clone(),
                inference_path_only,
            )),
        )
        .route(
            "/v2/*path",
            any(proxy::proxy_handler).layer(middleware::from_fn_with_state(
                state.clone(),
                inference_path_only,
            )),
        )
}

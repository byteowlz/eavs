//! Tenant-bound asynchronous inference jobs. The provider credential is shared,
//! so raw upstream IDs must never authorize status/result/cancellation requests.
//! Handles are HMAC-authenticated and bound to an Eavs virtual key and provider.
use crate::keys::{is_virtual_key, ValidatedKey};
use crate::provider::ProviderType;
use crate::state::AppState;
use crate::upstream::UpstreamRequest;
use axum::{
    body::Body,
    extract::{Path, State},
    http::{HeaderMap, Method, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use bytes::Bytes;
use futures::StreamExt;
use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::Sha256;

type HmacSha256 = Hmac<Sha256>;
const MAX_REPLY: usize = 2 * 1024 * 1024;

#[derive(Debug, Serialize, Deserialize)]
struct JobHandle {
    provider: String,
    owner: String,
    model: String,
    job_id: String,
    /// USD cost reserved for this job (0 when the key is not budgeted).
    reserved_cost: f64,
    expires: i64,
}

fn error(status: StatusCode, message: &str) -> Response {
    (
        status,
        Json(json!({"error": {"message": message, "type": "job_error"}})),
    )
        .into_response()
}

fn safe_component(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
}

fn safe_fal_model(value: &str) -> bool {
    value.len() <= 256 && value.split('/').all(safe_component)
}

fn seal(handle: &JobHandle, secret: &str) -> Result<String, Box<Response>> {
    let payload = serde_json::to_vec(handle).map_err(|_| {
        Box::new(error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Could not encode job handle",
        ))
    })?;
    let encoded = URL_SAFE_NO_PAD.encode(payload);
    let mut mac = HmacSha256::new_from_slice(secret.as_bytes()).map_err(|_| {
        Box::new(error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Could not sign job handle",
        ))
    })?;
    mac.update(encoded.as_bytes());
    Ok(format!(
        "v1.{encoded}.{}",
        hex::encode(mac.finalize().into_bytes())
    ))
}

fn open(
    token: &str,
    secret: &str,
    provider: &str,
    owner: &str,
) -> Result<JobHandle, Box<Response>> {
    if token.len() > 2048 {
        return Err(Box::new(error(StatusCode::FORBIDDEN, "Invalid job handle")));
    }
    let mut parts = token.split('.');
    let (Some("v1"), Some(encoded), Some(signature), None) =
        (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        return Err(Box::new(error(StatusCode::FORBIDDEN, "Invalid job handle")));
    };
    let signature = hex::decode(signature)
        .map_err(|_| Box::new(error(StatusCode::FORBIDDEN, "Invalid job handle")))?;
    let mut mac = HmacSha256::new_from_slice(secret.as_bytes()).map_err(|_| {
        Box::new(error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Could not verify job handle",
        ))
    })?;
    mac.update(encoded.as_bytes());
    mac.verify_slice(&signature)
        .map_err(|_| Box::new(error(StatusCode::FORBIDDEN, "Invalid job handle")))?;
    let bytes = URL_SAFE_NO_PAD
        .decode(encoded)
        .map_err(|_| Box::new(error(StatusCode::FORBIDDEN, "Invalid job handle")))?;
    let handle: JobHandle = serde_json::from_slice(&bytes)
        .map_err(|_| Box::new(error(StatusCode::FORBIDDEN, "Invalid job handle")))?;
    if handle.provider != provider
        || handle.owner != owner
        || handle.expires < chrono::Utc::now().timestamp()
        || !safe_component(&handle.job_id)
        || !safe_fal_model(&handle.model)
    {
        return Err(Box::new(error(
            StatusCode::FORBIDDEN,
            "Job handle is not valid for this key/provider",
        )));
    }
    Ok(handle)
}

fn contains_webhook(value: &Value) -> bool {
    match value {
        Value::Object(map) => map.iter().any(|(key, value)| {
            matches!(
                key.as_str(),
                "webhook" | "webhook_url" | "webhook_events_filter"
            ) || contains_webhook(value)
        }),
        Value::Array(values) => values.iter().any(contains_webhook),
        _ => false,
    }
}

fn contains_remote_url(value: &Value) -> bool {
    match value {
        Value::String(text) => {
            let value = text.trim_start().to_ascii_lowercase();
            value.starts_with("http://") || value.starts_with("https://") || value.starts_with("//")
        }
        Value::Object(map) => map.values().any(contains_remote_url),
        Value::Array(values) => values.iter().any(contains_remote_url),
        _ => false,
    }
}

struct JobContext {
    kind: ProviderType,
    provider: String,
    api_key: String,
    base: String,
    owner: ValidatedKey,
}

async fn context(
    state: &AppState,
    provider: &str,
    headers: &HeaderMap,
    model: &str,
    skip_budget: bool,
) -> Result<JobContext, Box<Response>> {
    let lookup = state
        .config
        .resolve_provider(provider)
        .ok_or_else(|| Box::new(error(StatusCode::BAD_REQUEST, "Unknown job provider")))?;
    let kind = lookup.config.provider_type();
    if !matches!(kind, ProviderType::Fal | ProviderType::Replicate) {
        return Err(Box::new(error(
            StatusCode::BAD_REQUEST,
            "Provider does not support Eavs jobs",
        )));
    }
    let key = headers
        .get(http::header::AUTHORIZATION)
        .and_then(|h| h.to_str().ok())
        .and_then(|h| h.strip_prefix("Bearer "))
        .ok_or_else(|| {
            Box::new(error(
                StatusCode::UNAUTHORIZED,
                "A virtual Eavs key is required for jobs",
            ))
        })?;
    if !is_virtual_key(key) {
        return Err(Box::new(error(
            StatusCode::UNAUTHORIZED,
            "A virtual Eavs key is required for jobs",
        )));
    }
    let validator = state.get_key_validator().ok_or_else(|| {
        Box::new(error(
            StatusCode::SERVICE_UNAVAILABLE,
            "Virtual keys are unavailable",
        ))
    })?;
    let owner = if skip_budget {
        validator
            .validate_skip_budget(key, model, &lookup.resolved_name)
            .await
    } else {
        validator
            .validate(key, model, &lookup.resolved_name, Some(0))
            .await
    }
    .map_err(|_| {
        Box::new(error(
            StatusCode::FORBIDDEN,
            "Job key or model is not authorized",
        ))
    })?;
    // Prices alone do not establish verified units or durable settlement.
    // Only submission is denied; authenticated lifecycle access survives caps.
    let budgeted = owner.permissions.max_budget_usd.is_some();
    if !skip_budget && (budgeted || owner.permissions.tpm_limit.is_some()) {
        return Err(Box::new(error(
            StatusCode::BAD_REQUEST,
            "Jobs cannot enforce this virtual key's budget or token limit yet",
        )));
    }
    let api_key = lookup.config.resolved_api_key();
    if api_key.is_empty() {
        return Err(Box::new(error(
            StatusCode::SERVICE_UNAVAILABLE,
            "Provider credential is not configured",
        )));
    }
    Ok(JobContext {
        kind,
        provider: lookup.resolved_name.clone(),
        api_key,
        base: lookup
            .config
            .resolved_base_url()
            .trim_end_matches('/')
            .to_string(),
        owner,
    })
}

async fn call(
    state: &AppState,
    context: &JobContext,
    method: Method,
    path: &str,
    body: Option<Value>,
) -> Result<Value, Box<Response>> {
    let url = format!("{}{}", context.base, path);
    crate::network_acl::check_url_allowed(&state.config.network, &url).map_err(|_| {
        Box::new(error(
            StatusCode::FORBIDDEN,
            "Job upstream blocked by network policy",
        ))
    })?;
    let mut headers = HeaderMap::new();
    let auth = if context.kind == ProviderType::Fal {
        format!("Key {}", context.api_key)
    } else {
        format!("Bearer {}", context.api_key)
    };
    headers.insert(
        http::header::AUTHORIZATION,
        auth.parse().map_err(|_| {
            Box::new(error(
                StatusCode::BAD_REQUEST,
                "Invalid provider credential",
            ))
        })?,
    );
    let body = if let Some(body) = body {
        headers.insert(
            http::header::CONTENT_TYPE,
            http::HeaderValue::from_static("application/json"),
        );
        Bytes::from(
            serde_json::to_vec(&body)
                .map_err(|_| Box::new(error(StatusCode::BAD_REQUEST, "Invalid job request")))?,
        )
    } else {
        Bytes::new()
    };
    let response = state
        .upstream
        .send(UpstreamRequest {
            method,
            url,
            headers,
            body,
        })
        .await
        .map_err(|_| Box::new(error(StatusCode::BAD_GATEWAY, "Job upstream failed")))?;
    let status = response.status;
    let mut stream = response.body;
    let mut bytes = Vec::new();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk
            .map_err(|_| Box::new(error(StatusCode::BAD_GATEWAY, "Job response stream failed")))?;
        if bytes.len().saturating_add(chunk.len()) > MAX_REPLY {
            return Err(Box::new(error(
                StatusCode::BAD_GATEWAY,
                "Job response exceeds size limit",
            )));
        }
        bytes.extend_from_slice(&chunk);
    }
    let value: Value = serde_json::from_slice(&bytes).map_err(|_| {
        Box::new(error(
            StatusCode::BAD_GATEWAY,
            "Invalid upstream job response",
        ))
    })?;
    if !status.is_success() {
        return Err(Box::new((status, Json(value)).into_response()));
    }
    Ok(value)
}

/// POST /:provider/v1/eavs/jobs: submit an async job. No webhook URL is accepted;
/// callers must poll with the returned signed handle.
/// Validate the allowed submit fields and reject webhooks/unknown fields.
fn validate_submit_fields(request: &Value) -> Result<(), Box<Response>> {
    if contains_webhook(request)
        || !request.as_object().is_some_and(|obj| {
            obj.keys()
                .all(|key| matches!(key.as_str(), "model" | "input" | "version"))
        })
    {
        return Err(Box::new(error(
            StatusCode::BAD_REQUEST,
            "Webhook and unknown job fields are not supported",
        )));
    }
    Ok(())
}

/// Resolve the upstream path and body for a submit request.
fn submit_request(
    context: &JobContext,
    request: &Value,
    model: &str,
    input: &Value,
) -> Result<(String, Value), Box<Response>> {
    if context.kind == ProviderType::Fal {
        if !safe_fal_model(model) || request.get("version").is_some() {
            return Err(Box::new(error(
                StatusCode::BAD_REQUEST,
                "Invalid fal model or unsupported version field",
            )));
        }
        Ok((format!("/{model}"), input.clone()))
    } else {
        let Some(version) = request.get("version").and_then(Value::as_str).filter(|v| {
            !v.is_empty()
                && v.len() < 256
                && v.bytes()
                    .all(|b| b.is_ascii_alphanumeric() || matches!(b, b':' | b'/' | b'-' | b'_'))
        }) else {
            return Err(Box::new(error(
                StatusCode::BAD_REQUEST,
                "Replicate version is required",
            )));
        };
        if (context.owner.permissions.allowed_models.is_some()
            || context.owner.permissions.blocked_models.is_some())
            && !version.starts_with(&format!("{model}:"))
        {
            return Err(Box::new(error(
                StatusCode::BAD_REQUEST,
                "Scoped Replicate keys require a model-qualified version (owner/model:hash)",
            )));
        }
        Ok((
            "/predictions".to_string(),
            json!({ "version": version, "input": input }),
        ))
    }
}

pub async fn submit(
    State(state): State<AppState>,
    Path(provider): Path<String>,
    headers: HeaderMap,
    body: Body,
) -> Response {
    let limit = if state.config.server.max_body_size == 0 {
        1024 * 1024
    } else {
        state.config.server.max_body_size.min(1024 * 1024)
    };
    let Ok(bytes) = axum::body::to_bytes(body, limit).await else {
        return error(StatusCode::PAYLOAD_TOO_LARGE, "Job request exceeds 1 MiB");
    };
    let request: Value = match serde_json::from_slice(&bytes) {
        Ok(value) => value,
        Err(_) => return error(StatusCode::BAD_REQUEST, "Invalid JSON job request"),
    };
    let Some(model) = request
        .get("model")
        .and_then(Value::as_str)
        .filter(|m| safe_fal_model(m))
    else {
        return error(
            StatusCode::BAD_REQUEST,
            "Job model must be a safe model identifier",
        );
    };
    let Some(input) = request.get("input").filter(|v| v.is_object()) else {
        return error(StatusCode::BAD_REQUEST, "Job input must be an object");
    };
    let context = match context(&state, &provider, &headers, model, false).await {
        Ok(ctx) => ctx,
        Err(e) => return *e,
    };
    if let Err(e) = validate_submit_fields(&request) {
        return *e;
    }
    let fetch_policy = state
        .config
        .delegated_fetch
        .policy_for_key_metadata(Some(&context.owner.metadata));
    if !fetch_policy.allow_remote_content && contains_remote_url(input) {
        return error(
            StatusCode::FORBIDDEN,
            "Remote input URLs require delegated fetch permission",
        );
    }
    let (path, upstream_body) = match submit_request(&context, &request, model, input) {
        Ok(t) => t,
        Err(e) => return *e,
    };
    let result = match call(&state, &context, Method::POST, &path, Some(upstream_body)).await {
        Ok(result) => result,
        Err(e) => return *e,
    };
    if context.kind == ProviderType::Replicate
        && result
            .get("model")
            .and_then(Value::as_str)
            .is_some_and(|actual| actual != model)
    {
        return error(
            StatusCode::BAD_GATEWAY,
            "Replicate returned a different model than requested",
        );
    }
    let job_id = result
        .get(if context.kind == ProviderType::Fal {
            "request_id"
        } else {
            "id"
        })
        .and_then(Value::as_str)
        .filter(|id| safe_component(id));
    let Some(job_id) = job_id else {
        return error(
            StatusCode::BAD_GATEWAY,
            "Job upstream returned no safe job ID",
        );
    };
    let handle = JobHandle {
        provider: context.provider.clone(),
        owner: context.owner.key_hash.clone(),
        model: model.to_string(),
        job_id: job_id.to_string(),
        reserved_cost: 0.0,
        expires: chrono::Utc::now().timestamp() + 7 * 24 * 3600,
    };
    let token = match seal(&handle, &context.api_key) {
        Ok(v) => v,
        Err(e) => return *e,
    };
    // Do not expose upstream response_url/status_url/cancel_url, which bypass Eavs.
    (StatusCode::ACCEPTED, Json(json!({
        "handle": token, "provider": context.provider, "model": model,
        "status": normalized_status(result.get("status").and_then(Value::as_str).unwrap_or("queued"))
    }))).into_response()
}

fn normalized_status(status: &str) -> &str {
    match status {
        "IN_QUEUE" | "starting" => "queued",
        "IN_PROGRESS" | "processing" => "running",
        "COMPLETED" | "succeeded" => "completed",
        "failed" | "ERROR" => "failed",
        "canceled" | "cancelled" | "CANCELLATION_REQUESTED" => "cancelled",
        _ => status,
    }
}

#[derive(Clone, Copy)]
enum Action {
    Status,
    Result,
    Cancel,
}

/// Resolve the upstream HTTP method and path for a lifecycle action.
fn lifecycle_target(action: Action, kind: ProviderType, handle: &JobHandle) -> (Method, String) {
    let path = match kind {
        ProviderType::Fal => {
            let stem = format!("/{}/requests/{}", handle.model, handle.job_id);
            match action {
                Action::Status => format!("{stem}/status"),
                Action::Result => stem,
                Action::Cancel => format!("{stem}/cancel"),
            }
        }
        ProviderType::Replicate => {
            let stem = format!("/predictions/{}", handle.job_id);
            if matches!(action, Action::Cancel) {
                format!("{stem}/cancel")
            } else {
                stem
            }
        }
        _ => unreachable!(),
    };
    let method = match action {
        Action::Cancel if kind == ProviderType::Fal => Method::PUT,
        Action::Cancel => Method::POST,
        _ => Method::GET,
    };
    (method, path)
}

async fn lifecycle(
    state: AppState,
    provider: String,
    token: String,
    headers: HeaderMap,
    action: Action,
) -> Response {
    // Verify MAC and owner before constructing any upstream URL. `model` is
    // authenticated by the handle and rechecked against current key scopes.
    let lookup = match state.config.resolve_provider(&provider) {
        Some(p)
            if matches!(
                p.config.provider_type(),
                ProviderType::Fal | ProviderType::Replicate
            ) =>
        {
            p
        }
        _ => return error(StatusCode::BAD_REQUEST, "Unknown job provider"),
    };
    let secret = lookup.config.resolved_api_key();
    if secret.is_empty() {
        return error(
            StatusCode::SERVICE_UNAVAILABLE,
            "Provider credential is not configured",
        );
    }
    let Some(key) = headers
        .get(http::header::AUTHORIZATION)
        .and_then(|h| h.to_str().ok())
        .and_then(|h| h.strip_prefix("Bearer "))
        .filter(|k| is_virtual_key(k))
    else {
        return error(
            StatusCode::UNAUTHORIZED,
            "A virtual Eavs key is required for jobs",
        );
    };
    // Use the key hash from the validator, not untrusted payload data.
    let Some(validator) = state.get_key_validator() else {
        return error(
            StatusCode::SERVICE_UNAVAILABLE,
            "Virtual keys are unavailable",
        );
    };
    // Only the MAC-authenticated model may be used for validation.
    let handle = match open(
        &token,
        &secret,
        &lookup.resolved_name,
        &crate::keys::hash_key(key),
    ) {
        Ok(h) => h,
        Err(e) => return *e,
    };
    let context = match context(&state, &provider, &headers, &handle.model, true).await {
        Ok(ctx) => ctx,
        Err(e) => return *e,
    };
    // Check that validated ownership has not changed since handle creation.
    if context.owner.key_hash != handle.owner {
        return error(StatusCode::FORBIDDEN, "Job owner mismatch");
    }
    let _ = validator; // Validation happens in context() with the authenticated model.
    let (method, path) = lifecycle_target(action, context.kind, &handle);
    let result = match call(&state, &context, method, &path, None).await {
        Ok(result) => result,
        Err(e) => return *e,
    };
    let status = normalized_status(result.get("status").and_then(Value::as_str).unwrap_or(
        if matches!(action, Action::Result) {
            "completed"
        } else {
            "unknown"
        },
    ))
    .to_string();
    let output = if matches!(action, Action::Result) {
        if context.kind == ProviderType::Fal {
            result.get("payload").cloned().unwrap_or(result)
        } else {
            result.get("output").cloned().unwrap_or(Value::Null)
        }
    } else {
        Value::Null
    };
    // No accounting mutations on repeatable lifecycle calls. Legacy reservations
    // remain held until durable, operation-bound settlement exists (eavs-avm8).
    // Failure/cancellation does not establish zero cost; process-local guards
    // cannot prevent double release across restarts or replicas.
    Json(
        json!({"handle": token, "provider": context.provider, "model": handle.model,
        "status": status, "output": output}),
    )
    .into_response()
}

pub async fn status(
    State(state): State<AppState>,
    Path((provider, handle)): Path<(String, String)>,
    headers: HeaderMap,
) -> Response {
    lifecycle(state, provider, handle, headers, Action::Status).await
}
pub async fn result(
    State(state): State<AppState>,
    Path((provider, handle)): Path<(String, String)>,
    headers: HeaderMap,
) -> Response {
    lifecycle(state, provider, handle, headers, Action::Result).await
}
pub async fn cancel(
    State(state): State<AppState>,
    Path((provider, handle)): Path<(String, String)>,
    headers: HeaderMap,
) -> Response {
    lifecycle(state, provider, handle, headers, Action::Cancel).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::AppConfig;
    use crate::keys::{CreateKeyRequest, KeyPermissions, KeyStore, KeyValidator, RateLimiter};
    use anyhow::{anyhow, Context};
    use axum::routing::{get, post};
    use axum::Router;
    use std::sync::Arc;

    /// Build a job test app with an in-memory key store.
    async fn job_setup(config_str: &str) -> anyhow::Result<(AppState, MockJobs, Arc<KeyStore>)> {
        let mock = MockJobs::default();
        let config: AppConfig = toml::from_str(config_str)?;
        let state = AppState::new_with_upstream(config, Arc::new(mock.clone()));
        let store = Arc::new(
            KeyStore::in_memory()
                .await
                .map_err(|e| anyhow!("in-memory key store failed: {e:?}"))?,
        );
        state
            .key_validator
            .set(Arc::new(KeyValidator::new(
                store.clone(),
                Arc::new(RateLimiter::new()),
            )))
            .map_err(|_| anyhow!("key validator already set"))?;
        Ok((state, mock, store))
    }

    /// Build a test key with the given permissions.
    async fn create_test_key(
        store: &Arc<KeyStore>,
        permissions: KeyPermissions,
    ) -> anyhow::Result<String> {
        Ok(store
            .create_key(CreateKeyRequest {
                permissions,
                ..Default::default()
            })
            .await
            .map_err(|e| anyhow!("{e:?}"))?
            .key)
    }

    /// Read a response body as JSON.
    async fn read_json(response: axum::response::Response, limit: usize) -> anyhow::Result<Value> {
        Ok(serde_json::from_slice(
            &axum::body::to_bytes(response.into_body(), limit).await?,
        )?)
    }

    /// Reserve an unrelated budget for a test key and assert success.
    async fn reserve(store: &Arc<KeyStore>, key: &str, amount: f64) -> anyhow::Result<()> {
        assert!(store
            .reserve_budget(&crate::keys::hash_key(key), amount)
            .await
            .map_err(|e| anyhow!("{e:?}"))?);
        Ok(())
    }

    /// Set the bearer authorization header on a request.
    fn with_auth(req: http::Request<Body>, key: &str) -> anyhow::Result<http::Request<Body>> {
        let mut req = req;
        req.headers_mut()
            .insert("authorization", format!("Bearer {key}").parse()?);
        Ok(req)
    }

    /// Assert that a request against a router returns the expected status.
    async fn expect_status(
        app: &axum::Router,
        req: http::Request<Body>,
        expected: StatusCode,
    ) -> anyhow::Result<()> {
        use tower::ServiceExt;
        assert_eq!(app.clone().oneshot(req).await?.status(), expected);
        Ok(())
    }

    /// Build the job lifecycle router.
    fn lifecycle_router(state: &AppState) -> axum::Router {
        Router::new()
            .route("/:provider/v1/eavs/jobs/:handle/result", get(result))
            .route("/:provider/v1/eavs/jobs/:handle/status", get(status))
            .route("/:provider/v1/eavs/jobs/:handle/cancel", post(cancel))
            .with_state(state.clone())
    }

    /// Exercise the job lifecycle endpoints and assert reservation accounting.
    async fn assert_lifecycle_reservations(
        result_app: &axum::Router,
        mock: &MockJobs,
        store: &Arc<KeyStore>,
        cases: &[(String, String, f64)],
    ) -> anyhow::Result<()> {
        use tower::ServiceExt;
        for (owner, handle, reserved) in cases {
            for (endpoint, method, status) in [
                ("status", "GET", "failed"),
                ("cancel", "POST", "cancelled"),
                ("result", "GET", "COMPLETED"),
            ] {
                for _ in 0..3 {
                    mock.responses()?
                        .push_back(json!({"status":status,"payload":{"ok":true}}));
                    let response = result_app
                        .clone()
                        .oneshot(
                            http::Request::builder()
                                .method(method)
                                .uri(format!("/fal/v1/eavs/jobs/{handle}/{endpoint}"))
                                .header("authorization", format!("Bearer {owner}"))
                                .body(Body::empty())?,
                        )
                        .await?;
                    assert_eq!(response.status(), StatusCode::OK);
                    let usage = store.get_by_key(owner).context("no usage")?.usage;
                    assert_eq!(usage.reserved_spend_usd, *reserved);
                    assert_eq!(usage.window_spend_usd, 0.0);
                }
            }
        }
        Ok(())
    }

    /// Convert a job-handle `Result` whose `Err` is an axum `Response` into an
    /// `anyhow` error so tests can propagate it with `?`.
    fn job_result<T>(result: Result<T, Box<Response>>) -> anyhow::Result<T> {
        result.map_err(|_| anyhow!("unexpected job-handle error"))
    }

    #[expect(
        clippy::panic_in_result_fn,
        reason = "intentional assertions in job tests"
    )]
    #[test]
    fn signed_handles_bind_owner_provider_and_detect_tampering() -> Result<(), anyhow::Error> {
        let handle = JobHandle {
            provider: "fal".into(),
            owner: "owner1".into(),
            model: "fal-ai/flux".into(),
            job_id: "job123".into(),
            reserved_cost: 0.0,
            expires: chrono::Utc::now().timestamp() + 100,
        };
        let token = job_result(seal(&handle, "secret"))?;
        assert_eq!(
            job_result(open(&token, "secret", "fal", "owner1"))?.job_id,
            "job123"
        );
        assert!(open(&token, "secret", "fal", "owner2").is_err());
        assert!(open(&token, "secret", "replicate", "owner1").is_err());
        assert!(open(&token, "other-secret", "fal", "owner1").is_err());
        assert!(open(&(token + "0"), "secret", "fal", "owner1").is_err());
        Ok(())
    }
    #[test]
    fn unsafe_job_inputs_are_rejected() {
        assert!(!safe_fal_model("fal-ai/../admin"));
        assert!(!safe_fal_model("fal-ai/%2e%2e"));
        assert!(safe_fal_model("fal-ai/flux/schnell"));
        assert!(contains_webhook(
            &json!({"input":{"webhook_url":"https://x"}})
        ));
        assert!(contains_remote_url(
            &json!({"image":"https://internal.example/"})
        ));
    }

    #[derive(Clone, Default)]
    struct MockJobs {
        requests: std::sync::Arc<std::sync::Mutex<Vec<UpstreamRequest>>>,
        responses: std::sync::Arc<std::sync::Mutex<std::collections::VecDeque<Value>>>,
    }
    impl MockJobs {
        fn requests(&self) -> anyhow::Result<std::sync::MutexGuard<'_, Vec<UpstreamRequest>>> {
            self.requests
                .lock()
                .map_err(|_| anyhow!("mock requests lock poisoned"))
        }
        fn responses(
            &self,
        ) -> anyhow::Result<std::sync::MutexGuard<'_, std::collections::VecDeque<Value>>> {
            self.responses
                .lock()
                .map_err(|_| anyhow!("mock responses lock poisoned"))
        }
    }
    impl crate::upstream::Upstream for MockJobs {
        fn send(
            &self,
            request: UpstreamRequest,
        ) -> futures::future::BoxFuture<'_, Result<crate::upstream::UpstreamResponse, std::io::Error>>
        {
            Box::pin(async move {
                self.requests
                    .lock()
                    .map_err(|_| std::io::Error::other("mock lock poisoned"))?
                    .push(request);
                let default = json!({ "status": "ok" });
                let response = self
                    .responses
                    .lock()
                    .map_err(|_| std::io::Error::other("mock lock poisoned"))?
                    .pop_front()
                    .unwrap_or(default);
                Ok(crate::upstream::UpstreamResponse {
                    status: StatusCode::OK,
                    headers: HeaderMap::new(),
                    body: Box::pin(futures::stream::once(async move {
                        Ok(Bytes::from(response.to_string()))
                    })),
                })
            })
        }
    }

    #[expect(
        clippy::panic_in_result_fn,
        reason = "intentional assertions in job tests"
    )]
    #[tokio::test]
    async fn job_webhooks_and_uncalibrated_budgets_fail_closed() -> Result<(), anyhow::Error> {
        use crate::config::AppConfig;
        use crate::keys::{CreateKeyRequest, KeyPermissions, KeyStore, KeyValidator, RateLimiter};
        use axum::{routing::post, Router};
        use std::sync::Arc;
        use tower::ServiceExt;
        let mock = MockJobs::default();
        let config: AppConfig = toml::from_str(
            r#"
            [providers.fal]
            type = "fal"
            base_url = "http://up"
            api_key = "secret"
        "#,
        )?;
        let state = AppState::new_with_upstream(config, Arc::new(mock.clone()));
        let store = Arc::new(
            KeyStore::in_memory()
                .await
                .map_err(|e| anyhow!("in-memory key store failed: {e:?}"))?,
        );
        state
            .key_validator
            .set(Arc::new(KeyValidator::new(
                store.clone(),
                Arc::new(RateLimiter::new()),
            )))
            .map_err(|_| anyhow!("key validator already set"))?;
        let normal = store
            .create_key(CreateKeyRequest::default())
            .await
            .map_err(|e| anyhow!("create key failed: {e:?}"))?
            .key;
        let budgeted = store
            .create_key(CreateKeyRequest {
                permissions: KeyPermissions {
                    max_budget_usd: Some(5.0),
                    ..Default::default()
                },
                ..Default::default()
            })
            .await
            .map_err(|e| anyhow!("create key failed: {e:?}"))?
            .key;
        let app = Router::new()
            .route("/:provider/v1/eavs/jobs", post(submit))
            .with_state(state);
        for (key, input, expected) in [
            (
                &normal,
                json!({"prompt":"cat","webhook_url":"https://external.test"}),
                StatusCode::BAD_REQUEST,
            ),
            (&budgeted, json!({"prompt":"cat"}), StatusCode::BAD_REQUEST),
        ] {
            let response = app
                .clone()
                .oneshot(
                    http::Request::builder()
                        .method("POST")
                        .uri("/fal/v1/eavs/jobs")
                        .header("authorization", format!("Bearer {key}"))
                        .header("content-type", "application/json")
                        .body(Body::from(
                            json!({"model":"fal-ai/flux/schnell","input":input}).to_string(),
                        ))?,
                )
                .await?;
            assert_eq!(response.status(), expected);
        }
        assert!(mock.requests()?.is_empty());
        Ok(())
    }

    #[expect(
        clippy::panic_in_result_fn,
        reason = "intentional assertions in job tests"
    )]
    #[tokio::test]
    async fn job_caps_deny_submission_and_lifecycle_never_mutates_reservations(
    ) -> Result<(), anyhow::Error> {
        use crate::keys::KeyPermissions;
        use axum::{routing::post, Router};
        use tower::ServiceExt;
        // Even configured prices cannot enable capped submissions.
        let (state, mock, store) = job_setup(
            r#"
            [providers.fal]
            type = "fal"
            base_url = "http://up"
            api_key = "secret"

            [native_pricing.jobs]
            'fal-ai/flux/schnell' = 0.005
        "#,
        )
        .await?;
        state
            .key_store
            .set(store.clone())
            .map_err(|_| anyhow!("key store already set"))?;
        let key = create_test_key(
            &store,
            KeyPermissions {
                max_budget_usd: Some(0.01),
                ..Default::default()
            },
        )
        .await?;

        let app = Router::new()
            .route("/:provider/v1/eavs/jobs", post(submit))
            .with_state(state.clone());
        let submit_req = || {
            http::Request::builder()
                .method("POST")
                .uri("/fal/v1/eavs/jobs")
                .header("authorization", format!("Bearer {key}"))
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({"model":"fal-ai/flux/schnell","input":{"prompt":"cat"}}).to_string(),
                ))
        };

        expect_status(&app, submit_req()?, StatusCode::BAD_REQUEST).await?;
        assert!(mock.requests()?.is_empty());

        let tpm_key = create_test_key(
            &store,
            KeyPermissions {
                tpm_limit: Some(100),
                ..Default::default()
            },
        )
        .await?;
        let request = with_auth(submit_req()?, &tpm_key)?;
        expect_status(&app, request, StatusCode::BAD_REQUEST).await?;
        assert!(mock.requests()?.is_empty());

        let normal = create_test_key(&store, KeyPermissions::default()).await?;
        mock.responses()?
            .push_back(json!({"request_id":"job1","status":"IN_QUEUE"}));
        let request = with_auth(submit_req()?, &normal)?;
        let response = app.clone().oneshot(request).await?;
        assert_eq!(response.status(), StatusCode::ACCEPTED);
        let body = read_json(response, MAX_REPLY).await?;
        let normal_handle = body["handle"].as_str().context("missing handle")?;
        assert_eq!(
            job_result(open(
                normal_handle,
                "secret",
                "fal",
                &crate::keys::hash_key(&normal)
            ))?
            .reserved_cost,
            0.0
        );

        // Keep unrelated reservations for both owners. Capped owner is at cap.
        reserve(&store, &key, 0.01).await?;
        reserve(&store, &normal, 0.02).await?;
        expect_status(&app, submit_req()?, StatusCode::FORBIDDEN).await?;
        let legacy = job_result(seal(
            &JobHandle {
                provider: "fal".into(),
                owner: crate::keys::hash_key(&key),
                model: "fal-ai/flux/schnell".into(),
                job_id: "legacy".into(),
                reserved_cost: 0.005,
                expires: chrono::Utc::now().timestamp() + 3600,
            },
            "secret",
        ))?;
        let result_app = lifecycle_router(&state);
        assert_lifecycle_reservations(
            &result_app,
            &mock,
            &store,
            &[
                (key.clone(), legacy, 0.01),
                (normal.clone(), normal_handle.to_string(), 0.02),
            ],
        )
        .await?;
        assert_eq!(mock.requests()?.len(), 19);
        Ok(())
    }

    #[expect(
        clippy::panic_in_result_fn,
        reason = "intentional assertions in job tests"
    )]
    #[tokio::test]
    async fn scoped_replicate_key_cannot_claim_an_unrelated_version() -> Result<(), anyhow::Error> {
        use crate::keys::KeyPermissions;
        use axum::{routing::post, Router};
        use tower::ServiceExt;
        let (state, mock, store) = job_setup(
            r#"
            [providers.replicate]
            type = "replicate"
            base_url = "http://up/v1"
            api_key = "secret"
        "#,
        )
        .await?;
        let key = store
            .create_key(CreateKeyRequest {
                permissions: KeyPermissions {
                    allowed_models: Some(["allowed/model".to_string()].into()),
                    ..Default::default()
                },
                ..Default::default()
            })
            .await
            .map_err(|e| anyhow!("{e:?}"))?
            .key;
        let app = Router::new()
            .route("/:provider/v1/eavs/jobs", post(submit))
            .with_state(state);
        let response = app
            .oneshot(
                http::Request::builder()
                    .method("POST")
                    .uri("/replicate/v1/eavs/jobs")
                    .header("authorization", format!("Bearer {key}"))
                    .header("content-type", "application/json")
                    .body(Body::from(
                        json!({"model":"allowed/model", "version":"other/model:abc",
                "input":{"prompt":"hi"}})
                        .to_string(),
                    ))?,
            )
            .await?;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        assert!(mock.requests()?.is_empty());
        Ok(())
    }

    #[expect(
        clippy::panic_in_result_fn,
        reason = "intentional assertions in job tests"
    )]
    #[tokio::test]
    async fn replicate_job_uses_predictions_and_never_exposes_upstream_urls(
    ) -> Result<(), anyhow::Error> {
        use axum::{
            routing::{get, post},
            Router,
        };
        use tower::ServiceExt;
        let (state, mock, store) = job_setup(
            r#"
            [providers.replicate]
            type = "replicate"
            base_url = "http://up/v1"
            api_key = "replicate-secret"
        "#,
        )
        .await?;
        *mock.responses()? = std::collections::VecDeque::from([
            json!({"id":"prediction123","status":"starting","urls":{"get":"https://api.replicate.com/v1/predictions/prediction123"}}),
            json!({"id":"prediction123","status":"succeeded","output":["https://cdn.example/image.png"]}),
        ]);
        let key = store
            .create_key(CreateKeyRequest::default())
            .await
            .map_err(|e| anyhow!("{e:?}"))?
            .key;
        let app = Router::new()
            .route("/:provider/v1/eavs/jobs", post(submit))
            .route("/:provider/v1/eavs/jobs/:handle/result", get(result))
            .with_state(state);
        let request = http::Request::builder()
            .method("POST")
            .uri("/replicate/v1/eavs/jobs")
            .header("authorization", format!("Bearer {key}"))
            .header("content-type", "application/json")
            .body(Body::from(
                json!({"model":"stability-ai/sdxl", "version":"a12b3c",
                "input":{"prompt":"cat"}})
                .to_string(),
            ))?;
        let response = app.clone().oneshot(request).await?;
        assert_eq!(response.status(), StatusCode::ACCEPTED);
        let body: Value =
            serde_json::from_slice(&axum::body::to_bytes(response.into_body(), usize::MAX).await?)?;
        assert_eq!(body["status"], "queued");
        assert!(body.get("urls").is_none());
        let handle = body["handle"].as_str().context("missing handle")?;
        let response = app
            .oneshot(
                http::Request::builder()
                    .uri(format!("/replicate/v1/eavs/jobs/{handle}/result"))
                    .header("authorization", format!("Bearer {key}"))
                    .body(Body::empty())?,
            )
            .await?;
        assert_eq!(response.status(), StatusCode::OK);
        let body: Value =
            serde_json::from_slice(&axum::body::to_bytes(response.into_body(), usize::MAX).await?)?;
        assert_eq!(body["status"], "completed");
        assert_eq!(body["output"][0], "https://cdn.example/image.png");
        let sent = mock.requests()?;
        assert_eq!(sent[0].url, "http://up/v1/predictions");
        assert_eq!(
            sent[0].headers.get("authorization").context("no auth")?,
            "Bearer replicate-secret"
        );
        assert_eq!(sent[1].url, "http://up/v1/predictions/prediction123");
        Ok(())
    }

    #[expect(
        clippy::panic_in_result_fn,
        reason = "intentional assertions in job tests"
    )]
    #[tokio::test]
    async fn fal_jobs_submit_poll_cancel_and_enforce_tenant_ownership() -> Result<(), anyhow::Error>
    {
        use axum::{
            routing::{get, post},
            Router,
        };
        use tower::ServiceExt;
        let (state, mock, store) = job_setup(
            r#"
            [providers.fal]
            type = "fal"
            api_key = "upstream-secret"
            base_url = "http://up"
        "#,
        )
        .await?;
        *mock.responses()? = std::collections::VecDeque::from([
            json!({"request_id":"job123","status":"IN_QUEUE","status_url":"https://queue.fal.run/bypass"}),
            json!({"status":"IN_PROGRESS"}),
            json!({"status":"CANCELLATION_REQUESTED"}),
        ]);
        let key = store
            .create_key(CreateKeyRequest::default())
            .await
            .map_err(|e| anyhow!("{e:?}"))?
            .key;
        let other = store
            .create_key(CreateKeyRequest::default())
            .await
            .map_err(|e| anyhow!("{e:?}"))?
            .key;
        let app = Router::new()
            .route("/:provider/v1/eavs/jobs", post(submit))
            .route("/:provider/v1/eavs/jobs/:handle", get(status))
            .route("/:provider/v1/eavs/jobs/:handle/cancel", post(cancel))
            .with_state(state);
        let request = http::Request::builder()
            .method("POST")
            .uri("/fal/v1/eavs/jobs")
            .header("authorization", format!("Bearer {key}"))
            .header("content-type", "application/json")
            .body(Body::from(
                json!({"model":"fal-ai/flux/schnell","input":{"prompt":"dog"}}).to_string(),
            ))?;
        let response = app.clone().oneshot(request).await?;
        assert_eq!(response.status(), StatusCode::ACCEPTED);
        let body: Value =
            serde_json::from_slice(&axum::body::to_bytes(response.into_body(), usize::MAX).await?)?;
        assert_eq!(body["status"], "queued");
        assert!(body.get("status_url").is_none());
        let handle = body["handle"].as_str().context("missing handle")?;
        let forbidden = app
            .clone()
            .oneshot(
                http::Request::builder()
                    .uri(format!("/fal/v1/eavs/jobs/{handle}"))
                    .header("authorization", format!("Bearer {other}"))
                    .body(Body::empty())?,
            )
            .await?;
        assert_eq!(forbidden.status(), StatusCode::FORBIDDEN);
        let status_response = app
            .clone()
            .oneshot(
                http::Request::builder()
                    .uri(format!("/fal/v1/eavs/jobs/{handle}"))
                    .header("authorization", format!("Bearer {key}"))
                    .body(Body::empty())?,
            )
            .await?;
        let status_body: Value = serde_json::from_slice(
            &axum::body::to_bytes(status_response.into_body(), usize::MAX).await?,
        )?;
        assert_eq!(status_body["status"], "running");
        let cancel_response = app
            .oneshot(
                http::Request::builder()
                    .method("POST")
                    .uri(format!("/fal/v1/eavs/jobs/{handle}/cancel"))
                    .header("authorization", format!("Bearer {key}"))
                    .body(Body::empty())?,
            )
            .await?;
        assert_eq!(cancel_response.status(), StatusCode::OK);
        let requests = mock.requests()?;
        assert_eq!(requests.len(), 3);
        assert_eq!(requests[0].url, "http://up/fal-ai/flux/schnell");
        assert_eq!(
            requests[0]
                .headers
                .get("authorization")
                .context("no auth")?,
            "Key upstream-secret"
        );
        assert_eq!(
            requests[1].url,
            "http://up/fal-ai/flux/schnell/requests/job123/status"
        );
        assert_eq!(requests[2].method, Method::PUT);
        Ok(())
    }
}

//! Side-effect-free provider draft probe.
//!
//! Exercises the real provider routing/transformation path against an *unsaved*
//! [`ProviderConfig`] with a small, bounded test request, without touching the
//! provider store, config, or restarting EAVS. It returns a stable, sanitized
//! diagnostic schema and can detect OpenAI-compatible endpoints that reject the
//! `developer` role (recommending `supports_developer_role = false`).
//!
//! Invariants:
//! - Credentials are never logged, serialized, or echoed. Any secret that leaks
//!   into an upstream response body is redacted before it appears in a
//!   diagnostic.
//! - Requests are bounded by [`PROBE_TIMEOUT`] and response bodies by
//!   [`MAX_RESPONSE_BYTES`]; excerpts are capped at [`BODY_EXCERPT_CHARS`].
//! - Unknown provider types fail closed (rejected) instead of silently becoming
//!   OpenAI.

use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use futures::StreamExt;
use http::{HeaderMap, Method, StatusCode};
use serde::{Deserialize, Serialize};

use crate::config::ProviderConfig;
use crate::provider::ProviderType;
use crate::proxy::{apply_http_auth_headers, apply_http_extra_headers};
use crate::transform::ProviderTransformer;
use crate::types::{Context, Message};
use crate::upstream::{Upstream, UpstreamRequest};

/// Maximum wall-clock time for a single upstream probe request.
const PROBE_TIMEOUT: Duration = Duration::from_secs(15);
/// Maximum number of response bytes read from upstream before giving up.
const MAX_RESPONSE_BYTES: usize = 8 * 1024;
/// Maximum number of characters retained in a sanitized body excerpt.
const BODY_EXCERPT_CHARS: usize = 512;
/// Tiny output budget for the probe request.
const PROBE_MAX_TOKENS: u32 = 16;
/// System prompt used so the `system`/`developer` role is actually exercised.
const PROBE_SYSTEM_PROMPT: &str = "You are a connectivity probe. Reply with 'ok'.";
/// Default user prompt when the caller does not supply one.
const PROBE_DEFAULT_PROMPT: &str = "ping";

/// Request body for `POST /admin/providers/probe`.
///
/// `config` mirrors the on-disk `[providers.*]` shape; it is used purely
/// in-memory and never persisted.
#[derive(Debug, Deserialize)]
pub struct ProbeRequest {
    /// Existing provider name whose saved credential may be reused when the
    /// draft omits `api_key` (edit flow). New providers leave this unset.
    #[serde(default)]
    pub provider_name: Option<String>,
    /// Unsaved provider configuration to exercise.
    pub config: ProviderConfig,
    /// Model id to send in the test request.
    pub model: String,
    /// Optional user prompt (defaults to a tiny ping).
    #[serde(default)]
    pub prompt: Option<String>,
}

/// A caller-facing rejection produced before any network activity.
///
/// Maps to `400 Bad Request` at the HTTP layer. Fail-closed validation lives
/// here (unknown provider type, empty model, un-buildable request).
#[derive(Debug, Serialize)]
pub struct ProbeRejection {
    pub error: String,
    pub code: String,
}

impl ProbeRejection {
    fn new(error: impl Into<String>, code: impl Into<String>) -> Self {
        Self {
            error: error.into(),
            code: code.into(),
        }
    }
}

/// Structured, sanitized probe diagnostics.
#[derive(Debug, Serialize)]
pub struct ProbeResponse {
    /// Canonical provider type resolved from the config (fail-closed).
    pub provider_type: String,
    /// Resolved base URL (never contains credentials).
    pub base_url: String,
    /// Model that was probed.
    pub model: String,
    /// Overall verdict: the endpoint answered a bounded chat request.
    pub ok: bool,
    /// Detected capabilities.
    pub capabilities: ProbeCapabilities,
    /// Per-stage diagnostics in execution order.
    pub stages: Vec<ProbeStage>,
    /// Actionable configuration remediation.
    pub recommendations: Vec<ProbeRecommendation>,
}

/// Capabilities inferred from the probe.
#[derive(Debug, Default, Serialize)]
pub struct ProbeCapabilities {
    /// Upstream was reachable (TCP/TLS/HTTP responded within the timeout).
    pub reachable: bool,
    /// Credentials were accepted (no 401/403).
    pub authenticated: bool,
    /// Whether the requested model appears usable (`None` if not determined).
    pub model_available: Option<bool>,
    /// Whether the endpoint accepts the `developer` role (`None` when not
    /// applicable, e.g. non-OpenAI-compatible providers).
    pub developer_role_supported: Option<bool>,
}

/// One diagnostic stage (typically a single upstream request).
#[derive(Debug, Serialize)]
pub struct ProbeStage {
    /// Stage name, e.g. `request`, `developer_role`, `system_role`.
    pub name: String,
    /// `ok`, `failed`, or `skipped`.
    pub status: String,
    /// Round-trip latency in milliseconds, when a request was made.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub latency_ms: Option<u64>,
    /// Upstream HTTP status, when the endpoint responded.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub upstream_status: Option<u16>,
    /// Human-readable, sanitized explanation.
    pub detail: String,
    /// Truncated, sanitized upstream body excerpt, when available.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub upstream_body_excerpt: Option<String>,
}

/// A configuration remediation suggestion.
#[derive(Debug, Serialize)]
pub struct ProbeRecommendation {
    /// Config field path, e.g. `compat.supports_developer_role`.
    pub field: String,
    /// Suggested value.
    pub value: serde_json::Value,
    /// Why this change is recommended.
    pub reason: String,
}

/// Wire-level role emitted for the system prompt.
#[derive(Clone, Copy)]
enum WireRole {
    Developer,
    System,
}

/// Outcome of a single bounded upstream attempt.
enum Attempt {
    /// The endpoint responded with an HTTP status.
    Responded {
        status: StatusCode,
        latency_ms: u64,
        body_excerpt: String,
    },
    /// The endpoint could not be reached (DNS/connect/TLS/timeout/read error).
    Unreachable { latency_ms: u64, error: String },
}

/// Run a side-effect-free probe against the supplied draft config.
///
/// Returns `Err(ProbeRejection)` for fail-closed input validation (no network
/// activity), otherwise `Ok(ProbeResponse)` with structured diagnostics.
pub async fn run_probe(
    upstream: &Arc<dyn Upstream>,
    request: ProbeRequest,
) -> Result<ProbeResponse, ProbeRejection> {
    let ProbeRequest {
        provider_name: _,
        config,
        model,
        prompt,
    } = request;

    if model.trim().is_empty() {
        return Err(ProbeRejection::new("Model is required", "missing_model"));
    }

    // Fail closed: unknown/empty provider types are rejected, never coerced to
    // OpenAI the way `ProviderType::from_str` would.
    let provider_type = ProviderType::try_from_str(&config.type_).ok_or_else(|| {
        ProbeRejection::new(
            format!("Unknown provider type '{}'", config.type_),
            "unknown_provider_type",
        )
    })?;

    let base_url = config.resolved_base_url();
    let api_key = config.resolved_api_key();
    let prompt = prompt.unwrap_or_else(|| PROBE_DEFAULT_PROMPT.to_string());

    let mut response = ProbeResponse {
        provider_type: format!("{:?}", provider_type),
        base_url: base_url.clone(),
        model: model.clone(),
        ok: false,
        capabilities: ProbeCapabilities::default(),
        stages: Vec::new(),
        recommendations: Vec::new(),
    };

    // Build the request once up-front so an un-buildable request fails closed
    // before any network activity.
    let build = |role: WireRole| -> Result<UpstreamRequest, ProbeRejection> {
        build_probe_request(
            &config,
            provider_type,
            &base_url,
            &api_key,
            &model,
            &prompt,
            role,
        )
    };

    // OpenAI-compatible providers emit the system prompt as a `developer` or
    // `system` role; others ignore the distinction entirely.
    if provider_type.is_openai_compatible() {
        probe_openai_compatible(upstream, &api_key, build, &mut response).await?;
    } else {
        let req = build(WireRole::System)?;
        let attempt = send_bounded(upstream, req, &api_key).await;
        interpret_primary(&attempt, "request", &mut response);
    }

    Ok(response)
}

/// Two-phase probe for OpenAI-compatible endpoints: try `developer` role, and
/// on a role rejection retry with `system`, recommending
/// `supports_developer_role = false` when the fallback succeeds.
async fn probe_openai_compatible<F>(
    upstream: &Arc<dyn Upstream>,
    api_key: &str,
    build: F,
    response: &mut ProbeResponse,
) -> Result<(), ProbeRejection>
where
    F: Fn(WireRole) -> Result<UpstreamRequest, ProbeRejection>,
{
    let dev_req = build(WireRole::Developer)?;
    let dev_attempt = send_bounded(upstream, dev_req, api_key).await;

    match &dev_attempt {
        Attempt::Unreachable { .. } => {
            interpret_primary(&dev_attempt, "developer_role", response);
        }
        Attempt::Responded {
            status,
            body_excerpt,
            ..
        } => {
            if status.is_success() {
                push_stage(
                    response,
                    "developer_role",
                    &dev_attempt,
                    "ok",
                    "developer role accepted",
                );
                response.capabilities.reachable = true;
                response.capabilities.authenticated = true;
                response.capabilities.model_available = Some(true);
                response.capabilities.developer_role_supported = Some(true);
                response.ok = true;
            } else if is_auth_failure(*status) {
                push_stage(
                    response,
                    "developer_role",
                    &dev_attempt,
                    "failed",
                    "credentials rejected",
                );
                response.capabilities.reachable = true;
                response.capabilities.authenticated = false;
            } else if is_developer_role_rejection(*status, body_excerpt) {
                push_stage(
                    response,
                    "developer_role",
                    &dev_attempt,
                    "failed",
                    "endpoint rejected the developer role; retrying with system role",
                );
                response.capabilities.reachable = true;
                response.capabilities.authenticated = true;

                // Fallback: confirm the endpoint accepts the `system` role.
                let sys_req = build(WireRole::System)?;
                let sys_attempt = send_bounded(upstream, sys_req, api_key).await;
                match &sys_attempt {
                    Attempt::Unreachable { .. } => {
                        interpret_primary(&sys_attempt, "system_role", response);
                    }
                    Attempt::Responded {
                        status,
                        body_excerpt,
                        ..
                    } => {
                        if status.is_success() {
                            push_stage(
                                response,
                                "system_role",
                                &sys_attempt,
                                "ok",
                                "system role accepted",
                            );
                            response.capabilities.model_available = Some(true);
                            response.capabilities.developer_role_supported = Some(false);
                            response.ok = true;
                            response.recommendations.push(ProbeRecommendation {
                                field: "compat.supports_developer_role".to_string(),
                                value: serde_json::Value::Bool(false),
                                reason: "Endpoint rejects the OpenAI `developer` role but accepts \
                                         `system`; set supports_developer_role=false."
                                    .to_string(),
                            });
                        } else if is_auth_failure(*status) {
                            push_stage(
                                response,
                                "system_role",
                                &sys_attempt,
                                "failed",
                                "credentials rejected",
                            );
                            response.capabilities.authenticated = false;
                        } else if is_model_failure(*status, body_excerpt) {
                            push_stage(
                                response,
                                "system_role",
                                &sys_attempt,
                                "failed",
                                "model not available",
                            );
                            response.capabilities.model_available = Some(false);
                        } else {
                            push_stage(
                                response,
                                "system_role",
                                &sys_attempt,
                                "failed",
                                "upstream error",
                            );
                        }
                    }
                }
            } else if is_model_failure(*status, body_excerpt) {
                push_stage(
                    response,
                    "developer_role",
                    &dev_attempt,
                    "failed",
                    "model not available",
                );
                response.capabilities.reachable = true;
                response.capabilities.authenticated = true;
                response.capabilities.model_available = Some(false);
            } else {
                push_stage(
                    response,
                    "developer_role",
                    &dev_attempt,
                    "failed",
                    "upstream error",
                );
                response.capabilities.reachable = true;
                response.capabilities.authenticated = true;
            }
        }
    }

    Ok(())
}

/// Interpret a single primary attempt (non-OpenAI-compatible providers, or the
/// unreachable case) into capabilities and a stage entry.
fn interpret_primary(attempt: &Attempt, stage: &str, response: &mut ProbeResponse) {
    match attempt {
        Attempt::Unreachable { error, .. } => {
            push_stage(
                response,
                stage,
                attempt,
                "failed",
                &format!("unreachable: {error}"),
            );
            response.capabilities.reachable = false;
        }
        Attempt::Responded {
            status,
            body_excerpt,
            ..
        } => {
            response.capabilities.reachable = true;
            if status.is_success() {
                push_stage(
                    response,
                    stage,
                    attempt,
                    "ok",
                    "endpoint responded successfully",
                );
                response.capabilities.authenticated = true;
                response.capabilities.model_available = Some(true);
                response.ok = true;
            } else if is_auth_failure(*status) {
                push_stage(response, stage, attempt, "failed", "credentials rejected");
                response.capabilities.authenticated = false;
            } else if is_model_failure(*status, body_excerpt) {
                push_stage(response, stage, attempt, "failed", "model not available");
                response.capabilities.authenticated = true;
                response.capabilities.model_available = Some(false);
            } else {
                push_stage(response, stage, attempt, "failed", "upstream error");
                response.capabilities.authenticated = true;
            }
        }
    }
}

fn push_stage(
    response: &mut ProbeResponse,
    name: &str,
    attempt: &Attempt,
    status: &str,
    detail: &str,
) {
    let (latency_ms, upstream_status, excerpt) = match attempt {
        Attempt::Responded {
            status,
            latency_ms,
            body_excerpt,
        } => (
            Some(*latency_ms),
            Some(status.as_u16()),
            if body_excerpt.is_empty() {
                None
            } else {
                Some(body_excerpt.clone())
            },
        ),
        Attempt::Unreachable { latency_ms, .. } => (Some(*latency_ms), None, None),
    };
    response.stages.push(ProbeStage {
        name: name.to_string(),
        status: status.to_string(),
        latency_ms,
        upstream_status,
        detail: detail.to_string(),
        upstream_body_excerpt: excerpt,
    });
}

/// A 401/403 indicates the credentials were rejected.
fn is_auth_failure(status: StatusCode) -> bool {
    status == StatusCode::UNAUTHORIZED || status == StatusCode::FORBIDDEN
}

/// A 404, or a 4xx whose body references the model, indicates the model is not
/// usable at this endpoint.
fn is_model_failure(status: StatusCode, body: &str) -> bool {
    if status == StatusCode::NOT_FOUND {
        return true;
    }
    if status.is_client_error() {
        let lower = body.to_lowercase();
        return lower.contains("model") && !lower.contains("developer");
    }
    false
}

/// A 4xx whose body references the `developer` role indicates the endpoint does
/// not accept OpenAI's `developer` message role.
fn is_developer_role_rejection(status: StatusCode, body: &str) -> bool {
    status.is_client_error() && body.to_lowercase().contains("developer")
}

/// Build the upstream request for a probe, mirroring the proxy's routing:
/// transformer-built body, resolved URL (with `/v1` de-duplication), auth,
/// provider extra headers, and env-resolved custom headers.
#[allow(clippy::too_many_arguments)]
fn build_probe_request(
    config: &ProviderConfig,
    provider_type: ProviderType,
    base_url: &str,
    api_key: &str,
    model: &str,
    prompt: &str,
    role: WireRole,
) -> Result<UpstreamRequest, ProbeRejection> {
    let compat = compat_for_role(config, role);
    let transformer = ProviderTransformer::for_provider_with_compat(provider_type, Some(&compat));

    let context = Context::new(model)
        .with_system(PROBE_SYSTEM_PROMPT)
        .with_messages(vec![Message::user(prompt)])
        .with_max_tokens(PROBE_MAX_TOKENS)
        .with_stream(false);

    let body_json = transformer.transform_request(&context).map_err(|e| {
        ProbeRejection::new(
            format!("Failed to build probe request: {e}"),
            "transform_failed",
        )
    })?;
    let body = serde_json::to_vec(&body_json).map_err(|e| {
        ProbeRejection::new(
            format!("Failed to serialize probe request: {e}"),
            "serialize_failed",
        )
    })?;

    let url = join_url(base_url, &transformer.endpoint_path(&context));

    let mut headers = HeaderMap::new();
    headers.insert(
        http::header::CONTENT_TYPE,
        http::HeaderValue::from_static("application/json"),
    );
    apply_http_auth_headers(&mut headers, provider_type, api_key);
    apply_http_extra_headers(&mut headers, provider_type);
    apply_custom_headers(&mut headers, config);

    Ok(UpstreamRequest {
        method: Method::POST,
        url,
        headers,
        body: Bytes::from(body),
    })
}

/// Drive the emitted wire role via compat settings.
///
/// We pin the capability explicitly so the probe controls exactly which role
/// is sent, independent of URL-detected defaults.
fn compat_for_role(config: &ProviderConfig, role: WireRole) -> crate::provider::CompatSettings {
    let mut compat = config.resolved_compat();
    compat.supports_developer_role = Some(match role {
        WireRole::Developer => true,
        WireRole::System => false,
    });
    compat
}

/// Apply env-resolved custom headers from the config, mirroring the proxy.
fn apply_custom_headers(headers: &mut HeaderMap, config: &ProviderConfig) {
    for (key, value) in &config.headers {
        let resolved = if let Some(var) = value.strip_prefix("env:") {
            std::env::var(var).unwrap_or_default()
        } else {
            value.clone()
        };
        if let (Ok(name), Ok(val)) = (
            http::header::HeaderName::from_bytes(key.as_bytes()),
            http::HeaderValue::from_str(&resolved),
        ) {
            headers.insert(name, val);
        }
    }
}

/// Join a base URL and a transformer endpoint path, de-duplicating a shared
/// `/v1` prefix the way the proxy does.
fn join_url(base_url: &str, path: &str) -> String {
    let base = base_url.trim_end_matches('/');
    let path = if base.ends_with("/v1") && path.starts_with("/v1") {
        path.strip_prefix("/v1").unwrap_or(path)
    } else {
        path
    };
    format!("{base}{path}")
}

/// Send a request with a strict timeout and a bounded response body read.
async fn send_bounded(
    upstream: &Arc<dyn Upstream>,
    request: UpstreamRequest,
    secret: &str,
) -> Attempt {
    let started = Instant::now();
    let send = tokio::time::timeout(PROBE_TIMEOUT, upstream.send(request)).await;
    let elapsed = || started.elapsed().as_millis() as u64;

    let mut resp = match send {
        Err(_) => {
            return Attempt::Unreachable {
                latency_ms: elapsed(),
                error: format!("timed out after {}s", PROBE_TIMEOUT.as_secs()),
            }
        }
        Ok(Err(e)) => {
            return Attempt::Unreachable {
                latency_ms: elapsed(),
                error: sanitize(&e.to_string(), secret),
            }
        }
        Ok(Ok(resp)) => resp,
    };

    let status = resp.status;
    let mut collected: Vec<u8> = Vec::new();
    while let Some(chunk) = resp.body.next().await {
        match chunk {
            Ok(bytes) => {
                let remaining = MAX_RESPONSE_BYTES.saturating_sub(collected.len());
                if remaining == 0 {
                    break;
                }
                let take = remaining.min(bytes.len());
                collected.extend_from_slice(&bytes[..take]);
                if collected.len() >= MAX_RESPONSE_BYTES {
                    break;
                }
            }
            Err(e) => {
                return Attempt::Unreachable {
                    latency_ms: elapsed(),
                    error: sanitize(&e.to_string(), secret),
                }
            }
        }
    }

    Attempt::Responded {
        status,
        latency_ms: elapsed(),
        body_excerpt: excerpt(&collected, secret),
    }
}

/// Redact any occurrence of the secret from a string.
fn sanitize(text: &str, secret: &str) -> String {
    if secret.len() >= 4 && text.contains(secret) {
        text.replace(secret, "***")
    } else {
        text.to_string()
    }
}

/// Produce a sanitized, character-bounded excerpt of a response body.
fn excerpt(bytes: &[u8], secret: &str) -> String {
    let text = String::from_utf8_lossy(bytes);
    let sanitized = sanitize(text.trim(), secret);
    if sanitized.chars().count() > BODY_EXCERPT_CHARS {
        let truncated: String = sanitized.chars().take(BODY_EXCERPT_CHARS).collect();
        format!("{truncated}…")
    } else {
        sanitized
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::upstream::UpstreamResponse;
    use futures::stream;
    use std::sync::Mutex;

    /// Minimal scripted upstream: returns queued responses in order and records
    /// the requests it received.
    struct ScriptedUpstream {
        responses: Mutex<Vec<Result<(StatusCode, Vec<u8>), std::io::Error>>>,
        requests: Mutex<Vec<UpstreamRequest>>,
    }

    impl ScriptedUpstream {
        fn new(responses: Vec<Result<(StatusCode, Vec<u8>), std::io::Error>>) -> Arc<Self> {
            Arc::new(Self {
                responses: Mutex::new(responses),
                requests: Mutex::new(Vec::new()),
            })
        }

        fn bodies(&self) -> Vec<serde_json::Value> {
            self.requests
                .lock()
                .unwrap()
                .iter()
                .map(|r| serde_json::from_slice(&r.body).unwrap())
                .collect()
        }
    }

    impl Upstream for ScriptedUpstream {
        fn send<'a>(
            &'a self,
            request: UpstreamRequest,
        ) -> futures::future::BoxFuture<'a, Result<UpstreamResponse, std::io::Error>> {
            Box::pin(async move {
                self.requests.lock().unwrap().push(request);
                let next = self.responses.lock().unwrap().remove(0);
                match next {
                    Ok((status, body)) => Ok(UpstreamResponse {
                        status,
                        headers: HeaderMap::new(),
                        body: stream::once(async move { Ok(Bytes::from(body)) }).boxed(),
                    }),
                    Err(e) => Err(e),
                }
            })
        }
    }

    fn ok_body() -> Vec<u8> {
        serde_json::to_vec(&serde_json::json!({
            "id": "probe",
            "object": "chat.completion",
            "choices": [{"index": 0, "message": {"role": "assistant", "content": "ok"}}]
        }))
        .unwrap()
    }

    fn config(type_: &str, base_url: &str) -> ProviderConfig {
        ProviderConfig {
            type_: type_.to_string(),
            api_key: "sk-probe-secret-key".to_string(),
            base_url: base_url.to_string(),
            ..Default::default()
        }
    }

    fn upstream(scripted: Arc<ScriptedUpstream>) -> Arc<dyn Upstream> {
        scripted
    }

    #[tokio::test]
    async fn rejects_unknown_provider_type_without_network() {
        let scripted = ScriptedUpstream::new(vec![]);
        let up = upstream(scripted.clone());
        let err = run_probe(
            &up,
            ProbeRequest {
                provider_name: None,
                config: config("totally-not-a-provider", "http://up/v1"),
                model: "some-model".to_string(),
                prompt: None,
            },
        )
        .await
        .unwrap_err();
        assert_eq!(err.code, "unknown_provider_type");
        // Fail closed: no request was ever sent.
        assert!(scripted.requests.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn success_reports_reachable_authenticated_and_developer_role() {
        let scripted = ScriptedUpstream::new(vec![Ok((StatusCode::OK, ok_body()))]);
        let up = upstream(scripted.clone());
        let resp = run_probe(
            &up,
            ProbeRequest {
                provider_name: None,
                config: config("openai", "http://up/v1"),
                model: "gpt-4o-mini".to_string(),
                prompt: None,
            },
        )
        .await
        .unwrap();

        assert!(resp.ok);
        assert!(resp.capabilities.reachable);
        assert!(resp.capabilities.authenticated);
        assert_eq!(resp.capabilities.developer_role_supported, Some(true));
        assert!(resp.recommendations.is_empty());
        assert_eq!(resp.stages.len(), 1);
        // The first attempt used the developer role and hit the de-duplicated URL.
        let sent = scripted.bodies();
        assert_eq!(sent[0]["messages"][0]["role"], "developer");
        assert_eq!(
            scripted.requests.lock().unwrap()[0].url,
            "http://up/v1/chat/completions"
        );
    }

    #[tokio::test]
    async fn bad_auth_reports_unauthenticated_and_no_secret_leak() {
        let body = br#"{"error":{"message":"Invalid API key: sk-probe-secret-key"}}"#.to_vec();
        let scripted = ScriptedUpstream::new(vec![Ok((StatusCode::UNAUTHORIZED, body))]);
        let up = upstream(scripted.clone());
        let resp = run_probe(
            &up,
            ProbeRequest {
                provider_name: None,
                config: config("openai", "http://up/v1"),
                model: "gpt-4o-mini".to_string(),
                prompt: None,
            },
        )
        .await
        .unwrap();

        assert!(!resp.ok);
        assert!(resp.capabilities.reachable);
        assert!(!resp.capabilities.authenticated);
        // The secret must be redacted from the echoed upstream body.
        let excerpt = resp.stages[0].upstream_body_excerpt.as_deref().unwrap();
        assert!(excerpt.contains("***"));
        assert!(!excerpt.contains("sk-probe-secret-key"));
        // And it must never appear anywhere in the serialized diagnostics.
        let serialized = serde_json::to_string(&resp).unwrap();
        assert!(!serialized.contains("sk-probe-secret-key"));
    }

    #[tokio::test]
    async fn unreachable_endpoint_reports_not_reachable() {
        let scripted = ScriptedUpstream::new(vec![Err(std::io::Error::new(
            std::io::ErrorKind::ConnectionRefused,
            "connection refused",
        ))]);
        let up = upstream(scripted.clone());
        let resp = run_probe(
            &up,
            ProbeRequest {
                provider_name: None,
                config: config("openai", "http://127.0.0.1:1/v1"),
                model: "gpt-4o-mini".to_string(),
                prompt: None,
            },
        )
        .await
        .unwrap();

        assert!(!resp.ok);
        assert!(!resp.capabilities.reachable);
        assert!(!resp.capabilities.authenticated);
        assert_eq!(resp.stages[0].status, "failed");
    }

    #[tokio::test]
    async fn model_failure_reports_model_unavailable() {
        let body =
            br#"{"error":{"message":"The model 'ghost' does not exist","code":"model_not_found"}}"#
                .to_vec();
        let scripted = ScriptedUpstream::new(vec![Ok((StatusCode::NOT_FOUND, body))]);
        let up = upstream(scripted.clone());
        let resp = run_probe(
            &up,
            ProbeRequest {
                provider_name: None,
                config: config("openai", "http://up/v1"),
                model: "ghost".to_string(),
                prompt: None,
            },
        )
        .await
        .unwrap();

        assert!(!resp.ok);
        assert!(resp.capabilities.reachable);
        assert!(resp.capabilities.authenticated);
        assert_eq!(resp.capabilities.model_available, Some(false));
    }

    #[tokio::test]
    async fn developer_role_rejection_falls_back_to_system_and_recommends_flag() {
        let reject =
            br#"{"error":{"message":"Unsupported role 'developer' in messages"}}"#.to_vec();
        let scripted = ScriptedUpstream::new(vec![
            Ok((StatusCode::BAD_REQUEST, reject)),
            Ok((StatusCode::OK, ok_body())),
        ]);
        let up = upstream(scripted.clone());
        let resp = run_probe(
            &up,
            ProbeRequest {
                provider_name: None,
                config: config("openai-compatible", "http://up:11434/v1"),
                model: "llama3.1".to_string(),
                prompt: None,
            },
        )
        .await
        .unwrap();

        assert!(resp.ok);
        assert!(resp.capabilities.reachable);
        assert!(resp.capabilities.authenticated);
        assert_eq!(resp.capabilities.developer_role_supported, Some(false));
        assert_eq!(resp.capabilities.model_available, Some(true));
        assert_eq!(resp.stages.len(), 2);

        // First attempt developer role, second attempt system role.
        let sent = scripted.bodies();
        assert_eq!(sent[0]["messages"][0]["role"], "developer");
        assert_eq!(sent[1]["messages"][0]["role"], "system");

        // Recommendation targets the compat flag with value false.
        assert_eq!(resp.recommendations.len(), 1);
        assert_eq!(
            resp.recommendations[0].field,
            "compat.supports_developer_role"
        );
        assert_eq!(
            resp.recommendations[0].value,
            serde_json::Value::Bool(false)
        );
    }
}

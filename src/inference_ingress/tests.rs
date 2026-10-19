use super::*;
#[path = "extra_tests.rs"]
mod extra_tests;
use crate::{
    config::{AppConfig, ProviderConfig},
    keys::{CreateKeyRequest, KeyPermissions, KeyStore, KeyValidator},
    upstream::{Upstream, UpstreamRequest, UpstreamResponse},
};
use anyhow::{anyhow, Result};
use bytes::Bytes;
use futures::{future::BoxFuture, StreamExt};
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Mutex,
};
use tower::ServiceExt;

struct Synthetic {
    calls: AtomicUsize,
    requests: Mutex<Vec<(String, Value, HeaderMap)>>,
    status: StatusCode,
}
impl Upstream for Synthetic {
    fn send(&self, request: UpstreamRequest) -> BoxFuture<'_, std::io::Result<UpstreamResponse>> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let body = serde_json::from_slice(&request.body).unwrap_or(Value::Null);
        if let Ok(mut requests) = self.requests.lock() {
            requests.push((request.url, body, request.headers));
        }
        let status = self.status;
        Box::pin(async move {
            let mut headers = HeaderMap::new();
            headers.insert(
                header::CONTENT_TYPE,
                HeaderValue::from_static("text/event-stream"),
            );
            headers.insert(header::RETRY_AFTER, HeaderValue::from_static("12"));
            let body = if status.is_success() {
                futures::stream::once(async {
                    Ok(Bytes::from_static(
                        b"data: {\"choices\":[{\"delta\":{\"content\":\"synthetic\"}}]}\n\n",
                    ))
                })
                .chain(futures::stream::pending())
                .boxed()
            } else {
                futures::stream::once(async {
                    Ok(Bytes::from_static(b"{\"error\":\"synthetic error\"}"))
                })
                .boxed()
            };
            Ok(UpstreamResponse {
                status,
                headers,
                body,
            })
        })
    }
}
struct Fixture {
    state: AppState,
    upstream: Arc<Synthetic>,
    _dir: tempfile::TempDir,
}
async fn fixture(status: StatusCode) -> Result<Fixture> {
    fixture_with_policy(
        status,
        crate::policy::PolicyConfig::default(),
        "openai-compatible",
    )
    .await
}
async fn fixture_with_policy(
    status: StatusCode,
    policy: crate::policy::PolicyConfig,
    alpha_type: &str,
) -> Result<Fixture> {
    let dir = tempfile::tempdir()?;
    let mut config = AppConfig::with_defaults();
    config.keys.enabled = true;
    config.policy = policy;
    config.keys.require_key = false; // ingress must independently require a key
    config.inference.cors_allowed_origins = vec!["https://flash.example".into()];
    for (name, kind) in [("alpha", alpha_type), ("beta", "anthropic")] {
        config.providers.insert(
            name.into(),
            ProviderConfig {
                type_: kind.into(),
                base_url: "https://api.openai.com/v1".into(),
                api_key: "synthetic-upstream".into(),
                models: vec![
                    serde_json::from_value(json!({"id":"allowed","name":"Allowed"}))?,
                    serde_json::from_value(json!({"id":"blocked","name":"Blocked"}))?,
                ],
                ..Default::default()
            },
        );
    }
    let upstream = Arc::new(Synthetic {
        calls: AtomicUsize::new(0),
        requests: Mutex::new(Vec::new()),
        status,
    });
    let state = AppState::new_with_upstream(config, upstream.clone());
    let store = Arc::new(KeyStore::new(&dir.path().join("keys.sqlite")).await?);
    state
        .key_store
        .set(store.clone())
        .map_err(|_| anyhow!("store already set"))?;
    state
        .key_validator
        .set(Arc::new(KeyValidator::new(
            store,
            state.rate_limiter.clone(),
        )))
        .map_err(|_| anyhow!("validator already set"))?;
    Ok(Fixture {
        state,
        upstream,
        _dir: dir,
    })
}
async fn key(state: &AppState, provider: &str) -> Result<String> {
    Ok(state
        .get_key_store()
        .ok_or_else(|| anyhow!("missing store"))?
        .create_key(CreateKeyRequest {
            permissions: KeyPermissions {
                allowed_providers: Some([provider.to_string()].into()),
                allowed_models: Some(["allowed".to_string()].into()),
                ..Default::default()
            },
            ..Default::default()
        })
        .await?
        .key)
}
fn app(state: &AppState) -> Result<Router> {
    router(
        state.clone(),
        vec!["synthetic-operator".into(), "synthetic-master".into()],
    )
    .map_err(|e| anyhow!(e))
}
fn request(method: &str, path: &str, token: &str, body: impl Into<Value>) -> Result<Request> {
    let body = body.into();
    Ok(Request::builder()
        .method(method)
        .uri(path)
        .header("authorization", format!("Bearer {token}"))
        .header("origin", "https://flash.example")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(serde_json::to_vec(&body)?))?)
}
#[tokio::test]
#[expect(
    clippy::panic_in_result_fn,
    reason = "Behavioral assertions with fallible fixture setup"
)]
async fn authentication_revocation_and_surface_isolation() -> Result<()> {
    let f = fixture(StatusCode::OK).await?;
    let token = key(&f.state, "alpha").await?;
    for token in ["", "eavs-invalid", "synthetic-operator", "synthetic-master"] {
        for (method, path) in [
            ("GET", "/v1/models"),
            ("POST", "/v1/chat/completions"),
            ("POST", "/v1/messages"),
            ("POST", "/v1/responses"),
        ] {
            let response = app(&f.state)?
                .oneshot(request(method, path, token, json!({}))?)
                .await?;
            assert_eq!(response.status(), 401);
            assert_eq!(
                response.headers()[header::ACCESS_CONTROL_ALLOW_ORIGIN],
                "https://flash.example"
            );
        }
    }
    for path in [
        "/admin/keys",
        "/admin/providers",
        "/providers",
        "/health",
        "/v1/eavs/catalog",
        "/alpha/v1/chat/completions",
        "/v1/eavs/jobs",
        "/unknown",
        "/models",
    ] {
        for token in ["invalid", token.as_str()] {
            assert_eq!(
                app(&f.state)?
                    .oneshot(request("GET", path, token, json!({}))?)
                    .await?
                    .status(),
                404
            );
        }
    }
    let store = f.state.get_key_store().ok_or_else(|| anyhow!("store"))?;
    let hash = store
        .get_by_key(&token)
        .ok_or_else(|| anyhow!("key"))?
        .key_hash;
    store.disable_key(&hash).await?;
    assert_eq!(
        app(&f.state)?
            .oneshot(request("GET", "/v1/models", &token, json!({}))?)
            .await?
            .status(),
        401
    );
    assert_eq!(f.upstream.calls.load(Ordering::SeqCst), 0);
    Ok(())
}
#[tokio::test]
#[expect(
    clippy::panic_in_result_fn,
    reason = "Behavioral assertions with fallible fixture setup"
)]
async fn cross_key_catalog_and_model_protocol_scope() -> Result<()> {
    let f = fixture(StatusCode::OK).await?;
    for provider in ["alpha", "beta"] {
        let token = key(&f.state, provider).await?;
        let response = app(&f.state)?
            .oneshot(request("GET", "/v1/models", &token, json!({}))?)
            .await?;
        assert_eq!(response.status(), 200);
        assert_eq!(
            response.headers()[header::CACHE_CONTROL],
            "private, no-store"
        );
        let value: Value =
            serde_json::from_slice(&axum::body::to_bytes(response.into_body(), 8192).await?)?;
        assert_eq!(value["data"].as_array().map(Vec::len), Some(1));
        assert_eq!(value["data"][0]["id"], format!("{provider}/allowed"));
        assert_eq!(
            value["data"][0]["eavs_api"],
            if provider == "alpha" {
                "openai-completions"
            } else {
                "anthropic-messages"
            }
        );
        let fields = value["data"][0]
            .as_object()
            .ok_or_else(|| anyhow!("model object"))?;
        assert_eq!(fields.len(), 5);
        assert!(!fields.contains_key("base_url"));
        assert!(!fields.contains_key("api_key"));
        for model in [
            "alpha/blocked",
            "beta/blocked",
            "hidden/allowed",
            "allowed",
            "default",
            "alpha/allowed/../blocked",
        ] {
            let response = app(&f.state)?
                .oneshot(request(
                    "POST",
                    "/v1/chat/completions",
                    &token,
                    json!({"model":model,"messages":[]}),
                )?)
                .await?;
            assert_eq!(response.status(), 403);
        }
        let wrong_protocol = if provider == "alpha" {
            "/v1/messages"
        } else {
            "/v1/chat/completions"
        };
        assert_eq!(
            app(&f.state)?
                .oneshot(request(
                    "POST",
                    wrong_protocol,
                    &token,
                    json!({"model":format!("{provider}/allowed")})
                )?)
                .await?
                .status(),
            403
        );
        let other = if provider == "alpha" {
            "beta/allowed"
        } else {
            "alpha/allowed"
        };
        assert_eq!(
            app(&f.state)?
                .oneshot(request(
                    "POST",
                    "/v1/messages",
                    &token,
                    json!({"model":other})
                )?)
                .await?
                .status(),
            403
        );
    }
    assert_eq!(f.upstream.calls.load(Ordering::SeqCst), 0);
    Ok(())
}
#[tokio::test]
#[expect(
    clippy::panic_in_result_fn,
    reason = "Behavioral assertions with fallible fixture setup"
)]
async fn cors_streaming_and_forced_physical_route() -> Result<()> {
    let f = fixture(StatusCode::OK).await?;
    for origin in ["https://flash.example", "https://evil.example"] {
        let response = app(&f.state)?
            .oneshot(
                Request::builder()
                    .method("OPTIONS")
                    .uri("/v1/chat/completions")
                    .header("origin", origin)
                    .header("access-control-request-method", "POST")
                    .header(
                        "access-control-request-headers",
                        "authorization,x-api-key,content-type",
                    )
                    .body(Body::empty())?,
            )
            .await?;
        assert!(response.status().is_success());
        assert_eq!(
            response
                .headers()
                .contains_key(header::ACCESS_CONTROL_ALLOW_ORIGIN),
            origin == "https://flash.example"
        );
        assert!(!response
            .headers()
            .contains_key(header::ACCESS_CONTROL_ALLOW_CREDENTIALS));
    }
    let token = key(&f.state, "alpha").await?;
    let mut request = request(
        "POST",
        "/v1/chat/completions",
        &token,
        json!({"model":"alpha/allowed","messages":[{"role":"user","content":"synthetic"}],"stream":true}),
    )?;
    request
        .headers_mut()
        .insert("x-provider", HeaderValue::from_static("beta"));
    request.headers_mut().insert(
        "x-original-host",
        HeaderValue::from_static("api.anthropic.com"),
    );
    let response = tokio::time::timeout(
        std::time::Duration::from_secs(2),
        app(&f.state)?.oneshot(request),
    )
    .await??;
    assert_eq!(response.status(), 200);
    assert_eq!(
        response.headers()[header::CONTENT_TYPE],
        "text/event-stream"
    );
    assert_eq!(
        response.headers()[header::ACCESS_CONTROL_ALLOW_ORIGIN],
        "https://flash.example"
    );
    let mut stream = response.into_body().into_data_stream();
    let first = tokio::time::timeout(std::time::Duration::from_secs(2), stream.next())
        .await?
        .ok_or_else(|| anyhow!("missing SSE frame"))??;
    assert!(std::str::from_utf8(&first)?.contains("synthetic"));
    drop(stream); // pending upstream must not prevent response or require buffering
    assert_eq!(f.upstream.calls.load(Ordering::SeqCst), 1);
    let requests = f
        .upstream
        .requests
        .lock()
        .map_err(|_| anyhow!("capture lock"))?;
    assert_eq!(requests[0].1["model"], "allowed");
    assert!(requests[0].0.ends_with("/chat/completions"));
    drop(requests);
    Ok(())
}
#[tokio::test]
#[expect(
    clippy::panic_in_result_fn,
    reason = "Behavioral assertions with fallible fixture setup"
)]
async fn capped_keys_and_anthropic_key_header_and_errors() -> Result<()> {
    let f = fixture(StatusCode::TOO_MANY_REQUESTS).await?;
    let store = f.state.get_key_store().ok_or_else(|| anyhow!("store"))?;
    for permissions in [
        KeyPermissions {
            max_budget_usd: Some(1.0),
            ..Default::default()
        },
        KeyPermissions {
            tpm_limit: Some(100),
            ..Default::default()
        },
    ] {
        let token = store
            .create_key(CreateKeyRequest {
                permissions,
                ..Default::default()
            })
            .await?
            .key;
        let response = app(&f.state)?
            .oneshot(request(
                "POST",
                "/v1/chat/completions",
                &token,
                json!({"model":"alpha/allowed","messages":[]}),
            )?)
            .await?;
        assert_eq!(response.status(), 403);
    }
    assert_eq!(f.upstream.calls.load(Ordering::SeqCst), 0);
    let token = key(&f.state, "beta").await?;
    let mut req = request(
        "POST",
        "/v1/messages",
        &token,
        json!({"model":"beta/allowed","messages":[{"role":"user","content":"synthetic"}],"max_tokens":1}),
    )?;
    req.headers_mut().remove(header::AUTHORIZATION);
    req.headers_mut()
        .insert("x-api-key", HeaderValue::from_str(&token)?);
    let response = app(&f.state)?.oneshot(req).await?;
    assert_eq!(response.status(), 429);
    assert_eq!(
        response.headers()[header::ACCESS_CONTROL_ALLOW_ORIGIN],
        "https://flash.example"
    );
    assert_eq!(
        response.headers()[header::CACHE_CONTROL],
        "private, no-store"
    );
    assert_eq!(f.upstream.calls.load(Ordering::SeqCst), 1);
    Ok(())
}
#[tokio::test]
#[expect(
    clippy::panic_in_result_fn,
    reason = "Behavioral assertions with fallible fixture setup"
)]
async fn startup_prerequisites_and_operator_collision() -> Result<()> {
    assert_eq!(Config::default().address().map_err(|e| anyhow!(e))?, None);
    for value in [
        "0.0.0.0:3035",
        "100.64.0.12:3035",
        "127.0.0.1:0",
        "localhost:3035",
        "garbage",
    ] {
        assert!(Config {
            listen: Some(value.into()),
            ..Default::default()
        }
        .address()
        .is_err());
    }
    assert!(Config {
        listen: Some("[::1]:3035".into()),
        ..Default::default()
    }
    .address()
    .map_err(|e| anyhow!(e))?
    .is_some());
    assert!(router(AppState::new(AppConfig::with_defaults()), vec![]).is_err());
    let mut config = AppConfig::with_defaults();
    config.keys.enabled = true;
    assert!(router(AppState::new(config), vec![]).is_err());
    let f = fixture(StatusCode::OK).await?;
    let token = key(&f.state, "alpha").await?;
    let collision = router(f.state.clone(), vec![token.clone()]).map_err(|e| anyhow!(e))?;
    assert_eq!(
        collision
            .oneshot(request("GET", "/v1/models", &token, json!({}))?)
            .await?
            .status(),
        401
    );
    assert_eq!(f.upstream.calls.load(Ordering::SeqCst), 0);
    Ok(())
}

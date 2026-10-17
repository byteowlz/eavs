use super::*;
use crate::upstream::{Upstream, UpstreamRequest, UpstreamResponse};
use anyhow::{anyhow, Result};
use axum::{
    body::Body,
    http::{Request, StatusCode},
    routing::{get, post},
    Router,
};
use bytes::Bytes;
use futures::{future::BoxFuture, stream::StreamExt};
use std::sync::Mutex as StdMutex;
use tower::ServiceExt;

struct Fake {
    requests: StdMutex<Vec<UpstreamRequest>>,
    release: Arc<tokio::sync::Notify>,
}
impl Upstream for Fake {
    fn send(
        &self,
        request: UpstreamRequest,
    ) -> BoxFuture<'_, Result<UpstreamResponse, std::io::Error>> {
        Box::pin(async move {
            let old = request.url.contains("/a/");
            self.requests
                .lock()
                .map_err(|_| std::io::Error::other("fixture poisoned"))?
                .push(request);
            let mut headers = http::HeaderMap::new();
            let body: futures::stream::BoxStream<'static, Result<Bytes, std::io::Error>> = if old {
                headers.insert(
                    "content-type",
                    http::HeaderValue::from_static("text/event-stream"),
                );
                let release = self.release.clone();
                let first = futures::stream::once(async {
                    Ok(Bytes::from("data: {\"id\":\"old\",\"model\":\"model-a\",\"choices\":[{\"index\":0,\"delta\":{\"role\":\"assistant\",\"content\":\"α\"}}]}\n\n"))
                });
                let last = futures::stream::once(async move {
                    release.notified().await;
                    Ok(Bytes::from("data: {\"id\":\"old\",\"model\":\"model-a\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"β\"},\"finish_reason\":\"stop\"}],\"usage\":{\"prompt_tokens\":3,\"completion_tokens\":2,\"total_tokens\":5}}\n\ndata: [DONE]\n\n"))
                });
                first.chain(last).boxed()
            } else {
                headers.insert(
                    "content-type",
                    http::HeaderValue::from_static("application/json"),
                );
                futures::stream::once(async { Ok(Bytes::from_static(br#"{"id":"new","model":"model-b","choices":[{"message":{"role":"assistant","content":"B"},"finish_reason":"stop"}],"usage":{"prompt_tokens":1,"completion_tokens":1,"total_tokens":2}}"#)) }).boxed()
            };
            Ok(UpstreamResponse {
                status: StatusCode::OK,
                headers,
                body,
            })
        })
    }
}

fn state() -> Result<(AppState, Arc<StdMutex<AppConfig>>)> {
    let mut config = AppConfig::with_defaults();
    config.keys.enabled = false;
    config.keys.master_key = Some("synthetic-admin".into());
    config.providers.insert(
        "a".into(),
        ProviderConfig {
            base_url: "http://127.0.0.1:1/a".into(),
            api_key: "synthetic-a".into(),
            ..ProviderConfig::default()
        },
    );
    let source = Arc::new(StdMutex::new(config.clone()));
    let state = AppState::new(config);
    let loader = source.clone();
    state
        .routing
        .configure(move || {
            loader
                .lock()
                .map(|v| v.clone())
                .map_err(|_| Failure::Unavailable)
        })
        .map_err(|e| anyhow!(e.code()))?;
    Ok((state, source))
}
fn client() -> Result<reqwest::Client> {
    Ok(reqwest::Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(10))
        .build()?)
}

async fn scoped_key(
    store: &crate::keys::KeyStore,
    provider: &str,
    model: Option<&str>,
) -> Result<String> {
    Ok(store
        .create_key(crate::keys::CreateKeyRequest {
            permissions: crate::keys::KeyPermissions {
                allowed_providers: Some([provider.to_owned()].into()),
                allowed_models: model.map(|model| [model.to_owned()].into()),
                ..Default::default()
            },
            ..Default::default()
        })
        .await?
        .key)
}

fn stage_b(source: &StdMutex<AppConfig>) -> Result<()> {
    let mut candidate = source.lock().map_err(|_| anyhow!("fixture poisoned"))?;
    candidate
        .providers
        .get_mut("a")
        .ok_or_else(|| anyhow!("fixture missing"))?
        .api_key = "synthetic-replacement".into();
    candidate.providers.insert(
        "b".into(),
        ProviderConfig {
            base_url: "http://127.0.0.1:1/b".into(),
            api_key: "synthetic-b".into(),
            ..ProviderConfig::default()
        },
    );
    drop(candidate);
    Ok(())
}

fn attach_fake(state: &mut AppState) -> Arc<Fake> {
    let fake = Arc::new(Fake {
        requests: StdMutex::new(Vec::new()),
        release: Arc::new(tokio::sync::Notify::new()),
    });
    state.upstream = fake.clone();
    fake
}

async fn reload_request(
    client: &reqwest::Client,
    base: &str,
    body: serde_json::Value,
) -> Result<serde_json::Value> {
    Ok(client
        .post(format!("{base}/admin/routing/reload"))
        .bearer_auth("synthetic-admin")
        .json(&body)
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?)
}

#[expect(
    clippy::panic_in_result_fn,
    reason = "Fallible assertion helper retains every native routing assertion"
)]
fn assert_native_requests(fake: &Fake, tools: &serde_json::Value) -> Result<()> {
    let requests = fake
        .requests
        .lock()
        .map_err(|_| anyhow!("fixture poisoned"))?;
    assert_eq!(requests.len(), 2, "Old stream never reconnects");
    let a = requests.first().ok_or_else(|| anyhow!("A missing"))?;
    let b = requests.get(1).ok_or_else(|| anyhow!("B missing"))?;
    assert!(a.url.contains("/a/"));
    assert!(b.url.contains("/b/"));
    assert_eq!(
        a.headers.get("authorization").and_then(|v| v.to_str().ok()),
        Some("Bearer synthetic-a")
    );
    assert_eq!(
        b.headers.get("authorization").and_then(|v| v.to_str().ok()),
        Some("Bearer synthetic-b")
    );
    let body: serde_json::Value = serde_json::from_slice(&b.body)?;
    drop(requests);
    assert_eq!(body["model"], "model-b");
    assert_eq!(&body["tools"], tools);
    assert_eq!(body["messages"][0]["content"], "日本 🚀");
    Ok(())
}

fn app(state: AppState) -> Router {
    Router::new()
        .route("/admin/routing", get(crate::api::routing::metadata))
        .route("/admin/routing/reload", post(crate::api::routing::reload))
        .route(
            "/providers/detail",
            get(crate::api::providers_detail_handler),
        )
        .route(
            "/:provider/*path",
            post(crate::proxy::provider_proxy_handler),
        )
        .with_state(state)
}

#[tokio::test]
#[expect(
    clippy::panic_in_result_fn,
    reason = "Behavioral assertions intentionally fail this regression"
)]
async fn reload_http_requires_owner_and_sanitizes_json_rejection() -> Result<()> {
    let (state, _) = state()?;
    let app = app(state);
    let request = Request::builder()
        .method("POST")
        .uri("/admin/routing/reload")
        .header("content-type", "application/json")
        .body(Body::from("{}"))?;
    let response = app.clone().oneshot(request).await?;
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    let request = Request::builder()
        .method("POST")
        .uri("/admin/routing/reload")
        .header("authorization", "Bearer synthetic-admin")
        .header("content-type", "application/json")
        .body(Body::from(
            r#"{"dry_run":"secret-sentinel-json","expected_generation":null}"#,
        ))?;
    let response = app.oneshot(request).await?;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let bytes = axum::body::to_bytes(response.into_body(), 16_384).await?;
    let text = String::from_utf8(bytes.to_vec())?;
    assert!(!text.contains("secret-sentinel"));
    assert!(text.contains("invalid_configuration"));
    Ok(())
}

#[tokio::test]
#[expect(
    clippy::panic_in_result_fn,
    reason = "Behavioral assertions intentionally fail this regression"
)]
async fn one_live_socket_keeps_old_sse_and_serves_new_native_route_after_http_apply() -> Result<()>
{
    let (mut state, source) = state()?;
    let fake = attach_fake(&mut state);
    let release = fake.release.clone();
    let old_generation = state.file_routing_generation();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let process = std::process::id();
    let server = tokio::spawn(async move { axum::serve(listener, app(state)).await });
    let client = client()?;
    let base = format!("http://{address}");
    let response = client.post(format!("{base}/a/v1/chat/completions"))
        .json(&serde_json::json!({"model":"model-a","stream":true,"messages":[{"role":"user","content":"日本 🚀"}]})).send().await?;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response
            .headers()
            .get("x-eavs-file-routing-generation")
            .and_then(|v| v.to_str().ok()),
        Some(old_generation.as_str())
    );
    let mut stream = response.bytes_stream();
    let first = tokio::time::timeout(Duration::from_secs(3), stream.next())
        .await?
        .ok_or_else(|| anyhow!("missing first SSE chunk"))??;
    assert!(String::from_utf8_lossy(&first).contains('α'));
    stage_b(&source)?;
    let preview = reload_request(&client, &base, serde_json::json!({"dry_run":true})).await?;
    assert_eq!(
        fake.requests
            .lock()
            .map_err(|_| anyhow!("fixture poisoned"))?
            .len(),
        1,
        "Preview must not probe upstreams"
    );
    let applied = reload_request(&client, &base, serde_json::json!({"dry_run":false,"expected_generation":preview["current_generation"],"expected_candidate_generation":preview["candidate_generation"]})).await?;
    assert_eq!(applied["status"], "applied");
    let generation = applied["resulting_generation"]
        .as_str()
        .ok_or_else(|| anyhow!("missing generation"))?;
    assert_ne!(generation, old_generation);
    assert_eq!(
        fake.requests
            .lock()
            .map_err(|_| anyhow!("fixture poisoned"))?
            .len(),
        1,
        "Apply must not probe upstreams"
    );
    let tools = serde_json::json!([{"type":"function","function":{"name":"lookup","parameters":{"type":"object"}}}]);
    let response = client.post(format!("{base}/b/v1/chat/completions"))
        .json(&serde_json::json!({"model":"model-b","messages":[{"role":"user","content":"日本 🚀"}],"tools":tools})).send().await?;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response
            .headers()
            .get("x-eavs-file-routing-generation")
            .and_then(|v| v.to_str().ok()),
        Some(generation)
    );
    let body: serde_json::Value = response.json().await?;
    assert_eq!(body["model"], "model-b");
    let metadata: Vec<serde_json::Value> = client
        .get(format!("{base}/providers/detail"))
        .send()
        .await?
        .json()
        .await?;
    assert_eq!(metadata.len(), 2);
    assert!(metadata
        .iter()
        .all(|row| row["file_routing_generation"] == generation));
    release.notify_one();
    let mut rest = Vec::new();
    while let Some(chunk) = tokio::time::timeout(Duration::from_secs(3), stream.next()).await? {
        rest.extend_from_slice(&chunk?);
    }
    let rest = String::from_utf8(rest)?;
    assert!(rest.contains('β'));
    assert!(rest.contains("[DONE]"));
    assert!(rest.contains("prompt_tokens"));
    assert!(rest.contains("stop"));
    assert_native_requests(&fake, &tools)?;
    assert_eq!(std::process::id(), process);
    server.abort();
    Ok(())
}

#[tokio::test]
#[expect(
    clippy::panic_in_result_fn,
    reason = "Behavioral assertions intentionally fail this regression"
)]
async fn new_routes_default_and_policy_rewrites_do_not_expand_virtual_key_authority() -> Result<()>
{
    use crate::keys::{KeyStore, KeyValidator};
    let (_, source) = state()?;
    let mut config = source
        .lock()
        .map_err(|_| anyhow!("fixture poisoned"))?
        .clone();
    config.keys.enabled = true;
    config.keys.require_key = true;
    config.policy = crate::policy::PolicyConfig {
        enabled: true,
        rules: vec![crate::policy::PolicyRule::RewriteModel {
            provider: Some("b".into()),
            model: Some("safe".into()),
            to: "forbidden".into(),
        }],
    };
    let state = AppState::new(config.clone());
    let candidate = Arc::new(StdMutex::new(config));
    let loader = candidate.clone();
    state
        .routing
        .configure(move || {
            loader
                .lock()
                .map(|v| v.clone())
                .map_err(|_| Failure::Unavailable)
        })
        .map_err(|e| anyhow!(e.code()))?;
    let store = Arc::new(KeyStore::in_memory().await?);
    state
        .key_store
        .set(store.clone())
        .map_err(|_| anyhow!("store already set"))?;
    state
        .key_validator
        .set(Arc::new(KeyValidator::new(
            store.clone(),
            state.rate_limiter.clone(),
        )))
        .map_err(|_| anyhow!("validator already set"))?;
    let key_a = scoped_key(&store, "a", None).await?;
    let key_b = scoped_key(&store, "b", Some("safe")).await?;
    {
        let mut config = candidate.lock().map_err(|_| anyhow!("fixture poisoned"))?;
        for name in ["b", "default"] {
            config.providers.insert(
                name.into(),
                ProviderConfig {
                    base_url: "http://127.0.0.1:1/b".into(),
                    api_key: "synthetic-b".into(),
                    ..Default::default()
                },
            );
        }
    }
    let plan = state
        .routing
        .reload(
            &state,
            ReloadRequest {
                dry_run: true,
                expected_generation: None,
                expected_candidate_generation: None,
            },
        )
        .await
        .map_err(|e| anyhow!(e.code()))?;
    state
        .routing
        .reload(
            &state,
            ReloadRequest {
                dry_run: false,
                expected_generation: Some(plan.current_generation),
                expected_candidate_generation: Some(plan.candidate_generation),
            },
        )
        .await
        .map_err(|e| anyhow!(e.code()))?;
    let mut state = state;
    let fake = attach_fake(&mut state);
    let app = app(state.clone()).route(
        "/v1/*path",
        post(crate::proxy::proxy_handler).with_state(state),
    );
    for (path, provider, key) in [
        ("/b/v1/chat/completions", None, &key_a),
        ("/v1/chat/completions", Some("b"), &key_a),
        ("/v1/chat/completions", None, &key_a),
        ("/b/v1/responses", None, &key_a),
        ("/b/v1/embeddings", None, &key_a),
        ("/b/v1/chat/completions", None, &key_b),
    ] {
        let mut request = Request::builder()
            .method("POST")
            .uri(path)
            .header("authorization", format!("Bearer {key}"))
            .header("content-type", "application/json");
        if let Some(provider) = provider {
            request = request.header("x-provider", provider);
        }
        let response = app.clone().oneshot(request.body(Body::from(serde_json::json!({"model":"safe","messages":[{"role":"user","content":"fixture"}],"input":"fixture"}).to_string()))?).await?;
        assert_eq!(
            response.status(),
            StatusCode::FORBIDDEN,
            "Unauthorized route/model via {path} must remain forbidden"
        );
    }
    assert!(fake
        .requests
        .lock()
        .map_err(|_| anyhow!("fixture poisoned"))?
        .is_empty());
    Ok(())
}

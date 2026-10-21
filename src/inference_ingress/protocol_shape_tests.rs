use super::*;

#[tokio::test]
#[expect(
    clippy::panic_in_result_fn,
    reason = "Behavioral assertions with fallible fixture setup"
)]
async fn google_url_models_and_anthropic_sdk_paths_are_preserved() -> Result<()> {
    for (kind, path, payload) in [
        ("anthropic", "/alpha/v1/v1/messages", json!({"model":"allowed","max_tokens":1,"messages":[{"role":"user","content":"synthetic"}]})),
        ("google", "/alpha/v1/models/allowed:generateContent", json!({"contents":[{"role":"user","parts":[{"text":"synthetic"}]}]})),
        ("google", "/alpha/v1/projects/synthetic/locations/global/publishers/google/models/allowed:generateContent", json!({"contents":[{"role":"user","parts":[{"text":"synthetic"}]}]})),
    ] {
        let f = fixture_with_policy(StatusCode::BAD_REQUEST, crate::policy::PolicyConfig::default(), kind).await?;
        let token = key(&f.state, "alpha").await?;
        let response = app(&f.state)?.oneshot(request("POST", path, &token, payload.clone())?).await?;
        assert_eq!(response.status(), 400, "{path}");
        let recorded = f.upstream.requests.lock().map_err(|_| anyhow!("requests"))?;
        assert_eq!(recorded.len(), 1, "{path}");
        assert_eq!(recorded[0].1, payload);
    }
    Ok(())
}

#[tokio::test]
#[expect(
    clippy::panic_in_result_fn,
    reason = "Behavioral assertions with fallible fixture setup"
)]
async fn google_path_model_cannot_be_hidden_behind_allowed_body_model() -> Result<()> {
    let f = fixture_with_policy(
        StatusCode::BAD_REQUEST,
        crate::policy::PolicyConfig::default(),
        "google",
    )
    .await?;
    let token = key(&f.state, "alpha").await?;
    let response = app(&f.state)?
        .oneshot(request(
            "POST",
            "/alpha/v1/models/blocked:generateContent",
            &token,
            json!({"model":"allowed","contents":[{"role":"user","parts":[{"text":"synthetic"}]}]}),
        )?)
        .await?;
    assert_eq!(response.status(), 403);
    assert_eq!(f.upstream.calls.load(Ordering::SeqCst), 0);
    Ok(())
}

struct AbortServer(tokio::task::JoinHandle<std::io::Result<()>>);
impl Drop for AbortServer {
    fn drop(&mut self) {
        self.0.abort();
    }
}
#[tokio::test]
#[expect(
    clippy::panic_in_result_fn,
    reason = "Behavioral assertions with fallible fixture setup"
)]
async fn websocket_scope_uses_resolved_provider_before_connecting_upstream() -> Result<()> {
    // The fake destination is always loopback, even if this regression fails.
    let f = fixture_with_policy_at(
        StatusCode::BAD_REQUEST,
        crate::policy::PolicyConfig::default(),
        "openai-compatible",
        "http://127.0.0.1:9/v1",
    )
    .await?;
    let store = f.state.get_key_store().ok_or_else(|| anyhow!("store"))?;
    let token = store
        .create_key(CreateKeyRequest {
            permissions: KeyPermissions {
                allowed_providers: Some(["ALPHA".to_owned()].into()),
                ..Default::default()
            },
            ..Default::default()
        })
        .await?
        .key;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let router = app(&f.state)?;
    let _server = AbortServer(tokio::spawn(
        async move { axum::serve(listener, router).await },
    ));
    let response = reqwest::Client::builder()
        .no_proxy()
        .timeout(std::time::Duration::from_secs(5))
        .build()?
        .get(format!("http://{address}/ALPHA/v1/realtime?model=allowed"))
        .header(header::AUTHORIZATION, format!("Bearer {token}"))
        .header("connection", "Upgrade")
        .header("upgrade", "websocket")
        .header("sec-websocket-version", "13")
        .header("sec-websocket-key", "dGhlIHNhbXBsZSBub25jZQ==")
        .send()
        .await?;
    assert_eq!(response.status(), 403);
    assert!(response.text().await?.contains("provider_not_allowed"));
    assert_eq!(f.upstream.calls.load(Ordering::SeqCst), 0);
    Ok(())
}

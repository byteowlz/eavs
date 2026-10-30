//! Pools on the public inference ingress: `model: "auto"` on the unprefixed
//! endpoints resolves through pool selection (key scope applied), not the
//! catalog, and keeps the ingress admission rules.
use super::*;

async fn pool_fixture() -> Result<(AppState, Arc<Synthetic>, tempfile::TempDir)> {
    let dir = tempfile::tempdir()?;
    let mut config = AppConfig::with_defaults();
    config.keys.enabled = true;
    config.keys.require_key = false;
    config.providers.insert(
        "alpha".into(),
        ProviderConfig {
            type_: "openai-compatible".into(),
            base_url: "https://api.openai.com/v1".into(),
            api_key: "synthetic-upstream".into(),
            models: vec![
                serde_json::from_value(json!({"id":"allowed","name":"Allowed"}))?,
                serde_json::from_value(json!({"id":"blocked","name":"Blocked"}))?,
            ],
            ..Default::default()
        },
    );
    config.pools.insert(
        "auto".into(),
        crate::pools::PoolConfig {
            members: vec!["alpha/*".into()],
        },
    );
    let upstream = Arc::new(Synthetic {
        calls: AtomicUsize::new(0),
        requests: Mutex::new(Vec::new()),
        raw_bodies: Mutex::new(Vec::new()),
        status: StatusCode::OK,
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
    Ok((state, upstream, dir))
}

#[tokio::test]
#[expect(
    clippy::panic_in_result_fn,
    reason = "Behavioral assertions with fallible fixture setup"
)]
async fn auto_resolves_within_key_scope_on_the_public_ingress() -> Result<()> {
    let (state, upstream, _dir) = pool_fixture().await?;
    // The key may use only model "allowed" on alpha; the pool's wildcard also
    // offers "blocked", which must never be chosen.
    let token = key(&state, "alpha").await?;
    let response = app(&state)?
        .oneshot(request(
            "POST",
            "/v1/chat/completions",
            &token,
            json!({"model": "auto", "stream": true, "messages": [{"role": "user", "content": "hi"}]}),
        )?)
        .await?;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response
            .headers()
            .get("x-eavs-route")
            .and_then(|v| v.to_str().ok()),
        Some("alpha/allowed")
    );
    let sent = upstream
        .requests
        .lock()
        .map_err(|_| anyhow!("poisoned"))?
        .clone();
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0].1["model"], "allowed");
    assert_ne!(
        sent[0].2.get("authorization").and_then(|v| v.to_str().ok()),
        Some(format!("Bearer {token}").as_str()),
        "the caller's key never reaches the upstream"
    );

    // Budget-capped keys still need durable accounting on this surface.
    let capped = state
        .get_key_store()
        .ok_or_else(|| anyhow!("missing store"))?
        .create_key(CreateKeyRequest {
            permissions: KeyPermissions {
                max_budget_usd: Some(1.0),
                ..Default::default()
            },
            ..Default::default()
        })
        .await?
        .key;
    let response = app(&state)?
        .oneshot(request(
            "POST",
            "/v1/chat/completions",
            &capped,
            json!({"model": "auto", "messages": [{"role": "user", "content": "hi"}]}),
        )?)
        .await?;
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    Ok(())
}

use super::*;

#[tokio::test]
#[expect(
    clippy::panic_in_result_fn,
    reason = "Behavioral assertions in fallible offline fixture"
)]
async fn expiry_and_future_activation_are_rejected() -> Result<()> {
    let f = fixture().await?;
    let token = key(&f.state, KeyPermissions::default()).await?;
    let store = f
        .state
        .get_key_store()
        .ok_or_else(|| anyhow!("missing store"))?;
    let hash = store
        .get_by_key(&token)
        .ok_or_else(|| anyhow!("missing key"))?
        .key_hash;
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(
            sqlx::sqlite::SqliteConnectOptions::new().filename(f.dir.path().join("keys.sqlite")),
        )
        .await?;
    sqlx::query("UPDATE virtual_keys SET valid_after = ? WHERE key_hash = ?")
        .bind((chrono::Utc::now() + chrono::Duration::hours(1)).to_rfc3339())
        .bind(&hash)
        .execute(&pool)
        .await?;
    store.refresh_cache().await?;
    assert_eq!(
        request(&f.state, Some(&token), true).await?.0,
        StatusCode::UNAUTHORIZED
    );
    sqlx::query("UPDATE virtual_keys SET valid_after = NULL, expires_at = ? WHERE key_hash = ?")
        .bind((chrono::Utc::now() - chrono::Duration::hours(1)).to_rfc3339())
        .bind(&hash)
        .execute(&pool)
        .await?;
    store.refresh_cache().await?;
    assert_eq!(
        request(&f.state, Some(&token), true).await?.0,
        StatusCode::UNAUTHORIZED
    );
    pool.close().await;
    Ok(())
}

#[tokio::test]
#[expect(
    clippy::panic_in_result_fn,
    reason = "Behavioral assertions in fallible offline fixture"
)]
async fn scope_allow_patterns_empty_scopes_and_path_denial() -> Result<()> {
    let f = fixture().await?;
    let token = key(
        &f.state,
        KeyPermissions {
            allowed_providers: Some(["alpha".into()].into()),
            allowed_models: Some(["allow*".into()].into()),
            ..Default::default()
        },
    )
    .await?;
    assert_eq!(
        model_ids(&request(&f.state, Some(&token), true).await?.2),
        ["allowed"]
    );
    let empty = key(
        &f.state,
        KeyPermissions {
            allowed_models: Some(std::collections::HashSet::default()),
            ..Default::default()
        },
    )
    .await?;
    assert!(model_ids(&request(&f.state, Some(&empty), true).await?.2).is_empty());
    let mut config = (*f.state.config).clone();
    config.policy = serde_json::from_value(
        json!({"enabled":true,"rules":[{"action":"deny","path":"/v1/chat/completions","reason":"private-denial"}]}),
    )?;
    let state = AppState::new_with_upstream(config, f.tripwire.clone());
    state
        .key_store
        .set(
            f.state
                .get_key_store()
                .ok_or_else(|| anyhow!("missing store"))?
                .clone(),
        )
        .map_err(|_| anyhow!("store already set"))?;
    assert!(model_ids(&request(&state, Some(&token), true).await?.2).is_empty());
    Ok(())
}

#[tokio::test]
#[expect(
    clippy::panic_in_result_fn,
    reason = "Behavioral assertions in fallible offline fixture"
)]
async fn rechecks_revocation_after_database_waits() -> Result<()> {
    let f = fixture().await?;
    let provider_store = f
        .state
        .get_provider_store()
        .ok_or_else(|| anyhow!("missing provider store"))?;
    provider_store
        .create_provider(ProviderRequest {
            name: "managed".into(),
            config: serde_json::to_value(provider(&["db-model"])?)?,
            enabled: true,
        })
        .await?;
    let token = key(&f.state, KeyPermissions::default()).await?;
    let store = f
        .state
        .get_key_store()
        .ok_or_else(|| anyhow!("missing key store"))?;
    let hash = store
        .get_by_key(&token)
        .ok_or_else(|| anyhow!("missing key"))?
        .key_hash;
    let mut catalog = Box::pin(build(&f.state, &token));
    assert!(matches!(
        futures::poll!(catalog.as_mut()),
        std::task::Poll::Pending
    ));
    store.disable_key(&hash).await?;
    assert!(matches!(catalog.await, Err(Failure::Unauthorized)));
    assert_eq!(f.tripwire.0.load(Ordering::SeqCst), 0);
    Ok(())
}

#[tokio::test]
#[expect(
    clippy::panic_in_result_fn,
    reason = "Behavioral assertions in fallible offline fixture"
)]
async fn catalog_limits_and_disabled_key_service_fail_closed() -> Result<()> {
    let f = fixture().await?;
    let token = key(&f.state, KeyPermissions::default()).await?;
    let mut config = (*f.state.config).clone();
    config
        .providers
        .get_mut("alpha")
        .ok_or_else(|| anyhow!("missing provider"))?
        .models = (0..=MAX_MODELS)
        .map(|n| model(&format!("m-{n}")))
        .collect::<Result<_>>()?;
    let state = AppState::new_with_upstream(config, f.tripwire.clone());
    state
        .key_store
        .set(
            f.state
                .get_key_store()
                .ok_or_else(|| anyhow!("missing store"))?
                .clone(),
        )
        .map_err(|_| anyhow!("store already set"))?;
    let (status, headers, body) = request(&state, Some(&token), true).await?;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(body["code"], "catalog_limit_exceeded");
    assert_eq!(headers["cache-control"], "private, no-store");
    let mut config = (*f.state.config).clone();
    config.keys.enabled = false;
    let disabled = AppState::new_with_upstream(config, f.tripwire.clone());
    assert_eq!(
        request(&disabled, Some(&token), true).await?.0,
        StatusCode::SERVICE_UNAVAILABLE
    );
    assert_eq!(f.tripwire.0.load(Ordering::SeqCst), 0);
    Ok(())
}

#[test]
#[expect(
    clippy::panic_in_result_fn,
    reason = "Behavioral assertions in fallible offline fixture"
)]
fn compat_has_only_bounded_known_public_values() -> Result<()> {
    let values = serde_json::from_value(
        json!({"supportsDeveloperRole":false,"private":"private-value","maxTokensField":"private-value"}),
    )?;
    assert_eq!(
        serde_json::to_value(model_compat(&values))?,
        json!({"supportsDeveloperRole":false})
    );
    let mut config = provider(&["allowed"])?;
    config.compat.max_tokens_field = Some("private-value".into());
    assert!(!serde_json::to_string(&provider_compat(&config))?.contains("private-value"));
    assert!(!provider_id("../private"));
    assert!(!provider_id("v1"));
    Ok(())
}

use super::*;

#[tokio::test]
#[expect(
    clippy::panic_in_result_fn,
    reason = "Behavioral assertions with fallible fixture setup"
)]
async fn capped_keys_cannot_bypass_ingress_through_native_or_ws_paths() -> Result<()> {
    let f = fixture(StatusCode::BAD_REQUEST).await?;
    let store = f.state.get_key_store().ok_or_else(|| anyhow!("store"))?;
    let token = store
        .create_key(CreateKeyRequest {
            permissions: KeyPermissions {
                max_budget_usd: Some(1.0),
                ..Default::default()
            },
            ..Default::default()
        })
        .await?
        .key;
    for (method, path) in [
        ("GET", "/alpha/v1/realtime"),
        ("GET", "/alpha/v1/codex/responses"),
        ("POST", "/alpha/v1/chat/completions"),
        ("POST", "/alpha/v1/systemone"),
        ("POST", "/alpha/v1/decisions"),
        ("POST", "/alpha/v1/images/generations"),
        ("POST", "/alpha/v1/eavs/jobs"),
    ] {
        let response = app(&f.state)?
            .oneshot(request(method, path, &token, json!({"model":"allowed"}))?)
            .await?;
        assert_eq!(response.status(), 403, "{path}");
        let body = axum::body::to_bytes(response.into_body(), 4096).await?;
        assert!(String::from_utf8_lossy(&body).contains("durable_accounting_required"));
    }
    assert_eq!(
        app(&f.state)?
            .oneshot(request("GET", "/v1/models", &token, json!({}))?)
            .await?
            .status(),
        200
    );
    let response = app(&f.state)?
        .oneshot(request(
            "GET",
            "/alpha/v1/eavs/jobs/invalid/result",
            &token,
            json!({}),
        )?)
        .await?;
    let body = axum::body::to_bytes(response.into_body(), 4096).await?;
    assert!(!String::from_utf8_lossy(&body).contains("durable_accounting_required"));
    assert_eq!(f.upstream.calls.load(Ordering::SeqCst), 0);
    Ok(())
}

#[tokio::test]
#[expect(
    clippy::panic_in_result_fn,
    reason = "Behavioral assertions with fallible fixture setup"
)]
async fn compressed_native_body_limit_is_enforced_after_decoding() -> Result<()> {
    let mut f = fixture(StatusCode::BAD_REQUEST).await?;
    Arc::make_mut(&mut f.state.config).server.max_body_size = 128;
    let token = key(&f.state, "alpha").await?;
    let raw = serde_json::to_vec(&json!({"model":"allowed","input":"x".repeat(4096)}))?;
    let compressed = zstd::stream::encode_all(raw.as_slice(), 1)?;
    assert!(compressed.len() < 128);
    let request = Request::builder()
        .method("POST")
        .uri("/alpha/v1/codex/responses")
        .header(header::AUTHORIZATION, format!("Bearer {token}"))
        .header(header::CONTENT_TYPE, "application/json")
        .header(header::CONTENT_ENCODING, "zstd")
        .body(Body::from(compressed))?;
    assert_eq!(app(&f.state)?.oneshot(request).await?.status(), 413);
    assert_eq!(f.upstream.calls.load(Ordering::SeqCst), 0);
    Ok(())
}

#[tokio::test]
#[expect(
    clippy::panic_in_result_fn,
    reason = "Behavioral assertions with fallible fixture setup"
)]
async fn opaque_model_scope_cannot_authorize_against_unknown_placeholder() -> Result<()> {
    let f = fixture(StatusCode::BAD_REQUEST).await?;
    let store = f.state.get_key_store().ok_or_else(|| anyhow!("store"))?;
    let token = store
        .create_key(CreateKeyRequest {
            permissions: KeyPermissions {
                allowed_models: Some(["unknown".to_owned()].into()),
                ..Default::default()
            },
            ..Default::default()
        })
        .await?
        .key;
    let request = Request::builder().method("POST").uri("/alpha/v1/images/edits")
        .header(header::AUTHORIZATION, format!("Bearer {token}"))
        .header(header::CONTENT_TYPE, "multipart/form-data; boundary=synthetic")
        .body(Body::from("--synthetic\r\nContent-Disposition: form-data; name=\"model\"\r\n\r\nblocked\r\n--synthetic--\r\n"))?;
    assert_eq!(app(&f.state)?.oneshot(request).await?.status(), 403);
    assert_eq!(f.upstream.calls.load(Ordering::SeqCst), 0);
    Ok(())
}

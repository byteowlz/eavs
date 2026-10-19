use super::*;

#[tokio::test]
#[expect(
    clippy::panic_in_result_fn,
    reason = "Behavioral assertions with fallible fixture setup"
)]
async fn effective_policy_scope_cannot_be_bypassed() -> Result<()> {
    let policy = crate::policy::PolicyConfig {
        enabled: true,
        rules: vec![crate::policy::PolicyRule::SetField {
            provider: Some("alpha".into()),
            model: Some("allowed".into()),
            path: Some("/v1/chat/completions".into()),
            field: "model".into(),
            value: json!("blocked"),
        }],
    };
    // OpenAI catalog projects /responses, while this policy mutates /chat only.
    // Discovery therefore succeeds; the native pipeline MUST recheck effective scope.
    let f = fixture_with_policy(StatusCode::OK, policy, "openai").await?;
    let token = key(&f.state, "alpha").await?;
    let response = app(&f.state)?
        .oneshot(request("GET", "/v1/models", &token, json!({}))?)
        .await?;
    let value: Value =
        serde_json::from_slice(&axum::body::to_bytes(response.into_body(), 8192).await?)?;
    assert_eq!(value["data"][0]["id"], "alpha/allowed");
    let response = app(&f.state)?
        .oneshot(request(
            "POST",
            "/v1/chat/completions",
            &token,
            json!({"model":"alpha/allowed","messages":[]}),
        )?)
        .await?;
    assert_eq!(response.status(), 403);
    let value: Value =
        serde_json::from_slice(&axum::body::to_bytes(response.into_body(), 8192).await?)?;
    assert_eq!(value["error"]["code"], "model_not_allowed");
    assert_eq!(f.upstream.calls.load(Ordering::SeqCst), 0);
    Ok(())
}

#[tokio::test]
#[expect(
    clippy::panic_in_result_fn,
    reason = "Behavioral assertions with fallible fixture setup"
)]
async fn sdk_preflight_and_key_scoped_idempotency_are_preserved() -> Result<()> {
    let f = fixture(StatusCode::TOO_MANY_REQUESTS).await?;
    let requested = "authorization,content-type,x-api-key,anthropic-version,anthropic-beta,anthropic-dangerous-direct-browser-access,x-client-request-id,idempotency-key,x-stainless-lang,x-stainless-package-version,x-stainless-os,x-stainless-arch,x-stainless-runtime,x-stainless-runtime-version,x-stainless-retry-count,x-stainless-timeout,x-stainless-helper-method,x-stainless-async,x-stainless-read-timeout,x-stainless-connect-timeout";
    let response = app(&f.state)?
        .oneshot(
            Request::builder()
                .method("OPTIONS")
                .uri("/v1/messages")
                .header("origin", "https://flash.example")
                .header("access-control-request-method", "POST")
                .header("access-control-request-headers", requested)
                .body(Body::empty())?,
        )
        .await?;
    assert_eq!(response.status(), 200);
    let allowed: Vec<_> = response.headers()[header::ACCESS_CONTROL_ALLOW_HEADERS]
        .to_str()?
        .split(',')
        .map(str::trim)
        .collect();
    for name in requested.split(',') {
        assert!(allowed.contains(&name), "missing SDK CORS header {name}");
    }
    let a = key(&f.state, "alpha").await?;
    let b = key(&f.state, "alpha").await?;
    for token in [&a, &a, &b] {
        let mut request = request(
            "POST",
            "/v1/chat/completions",
            token,
            json!({"model":"alpha/allowed","messages":[]}),
        )?;
        request.headers_mut().insert(
            "idempotency-key",
            HeaderValue::from_static("same-client-retry"),
        );
        request.headers_mut().insert(
            "x-client-request-id",
            HeaderValue::from_static("client-request-42"),
        );
        assert_eq!(app(&f.state)?.oneshot(request).await?.status(), 429);
    }
    let requests = f
        .upstream
        .requests
        .lock()
        .map_err(|_| anyhow!("capture lock"))?;
    assert_eq!(requests.len(), 3);
    assert_eq!(
        requests[0].2["idempotency-key"],
        requests[1].2["idempotency-key"]
    );
    assert_ne!(
        requests[0].2["idempotency-key"],
        requests[2].2["idempotency-key"]
    );
    assert_ne!(requests[0].2["idempotency-key"], "same-client-retry");
    assert_eq!(requests[0].2["x-client-request-id"], "client-request-42");
    Ok(())
}

#[tokio::test]
#[expect(
    clippy::panic_in_result_fn,
    reason = "Behavioral assertions with fallible fixture setup"
)]
async fn responses_uses_existing_native_pipeline() -> Result<()> {
    let f = fixture_with_policy(
        StatusCode::TOO_MANY_REQUESTS,
        crate::policy::PolicyConfig::default(),
        "openai",
    )
    .await?;
    let token = key(&f.state, "alpha").await?;
    let response = app(&f.state)?
        .oneshot(request(
            "POST",
            "/v1/responses",
            &token,
            json!({"model":"alpha/allowed","input":"synthetic","max_output_tokens":1}),
        )?)
        .await?;
    assert_eq!(response.status(), 429);
    assert_eq!(response.headers()[header::RETRY_AFTER], "12");
    assert_eq!(
        response.headers()[header::ACCESS_CONTROL_ALLOW_ORIGIN],
        "https://flash.example"
    );
    let requests = f
        .upstream
        .requests
        .lock()
        .map_err(|_| anyhow!("capture lock"))?;
    assert_eq!(requests[0].1["model"], "allowed");
    assert!(requests[0].0.ends_with("/responses"));
    drop(requests);
    Ok(())
}

#[tokio::test]
#[expect(
    clippy::panic_in_result_fn,
    reason = "Behavioral assertions with fallible fixture setup"
)]
async fn expired_device_conflicting_credentials_and_bad_configuration() -> Result<()> {
    let mut f = fixture(StatusCode::OK).await?;
    let store = f.state.get_key_store().ok_or_else(|| anyhow!("store"))?;
    for request_body in [
        CreateKeyRequest {
            expires_at: Some(chrono::Utc::now() - chrono::Duration::seconds(60)),
            ..Default::default()
        },
        CreateKeyRequest {
            metadata: json!({"kind":"device_enrollment"}),
            ..Default::default()
        },
    ] {
        let token = store.create_key(request_body).await?.key;
        assert_eq!(
            app(&f.state)?
                .oneshot(request("GET", "/v1/models", &token, json!({}))?)
                .await?
                .status(),
            401
        );
    }
    let token = key(&f.state, "alpha").await?;
    for (name, value) in [
        ("x-api-key", "different"),
        ("x-eavs-token", "synthetic-operator"),
    ] {
        let mut req = request("GET", "/v1/models", &token, json!({}))?;
        req.headers_mut()
            .insert(name, HeaderValue::from_str(value)?);
        assert_eq!(app(&f.state)?.oneshot(req).await?.status(), 401);
    }
    for method in ["GET", "POST", "OPTIONS"] {
        for path in [
            "/admin/providers",
            "/alpha/v1/chat/completions",
            "/v1/eavs/jobs",
            "/unknown",
        ] {
            assert_eq!(
                app(&f.state)?
                    .oneshot(request(method, path, &token, json!({}))?)
                    .await?
                    .status(),
                404
            );
        }
    }
    assert_eq!(
        app(&f.state)?
            .oneshot(request("HEAD", "/v1/models", &token, json!({}))?)
            .await?
            .status(),
        405
    );
    crate::routing::validate_input(&json!({"inference":{"listen":"127.0.0.1:3035"}}))
        .map_err(|_| anyhow!("inference root config rejected"))?;
    Arc::make_mut(&mut f.state.config)
        .inference
        .cors_allowed_origins = vec!["*".into()];
    assert!(router(f.state.clone(), vec![]).is_err());
    assert!(
        serde_json::from_value::<Config>(json!({"listen":"127.0.0.1:3035","typo":true})).is_err()
    );
    assert_eq!(f.upstream.calls.load(Ordering::SeqCst), 0);
    Ok(())
}

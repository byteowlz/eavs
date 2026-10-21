use super::*;

#[tokio::test]
#[expect(
    clippy::panic_in_result_fn,
    reason = "Behavioral assertions with fallible fixture setup"
)]
async fn upstream_account_and_gateway_admin_paths_are_not_inference() -> Result<()> {
    let f = fixture(StatusCode::BAD_REQUEST).await?;
    let token = key(&f.state, "alpha").await?;
    for path in [
        "/admin/keys",
        "/admin/providers",
        "/auth/status/user",
        "/logs/stream",
        "/providers",
        "/config",
        "/alpha/v1/organization/projects",
        "/alpha/v1/files",
        "/v1/fine_tuning/jobs",
        "/alpha/v2/admin",
    ] {
        let response = app(&f.state)?
            .oneshot(request("POST", path, &token, json!({"model":"allowed"}))?)
            .await?;
        assert_eq!(response.status(), 404, "{path}");
    }
    assert_eq!(f.upstream.calls.load(Ordering::SeqCst), 0);
    Ok(())
}

#[tokio::test]
#[expect(
    clippy::panic_in_result_fn,
    reason = "Behavioral assertions with fallible fixture setup"
)]
async fn multipart_and_anthropic_credential_preserve_native_upload_bytes() -> Result<()> {
    let f = fixture(StatusCode::BAD_REQUEST).await?;
    let store = f.state.get_key_store().ok_or_else(|| anyhow!("store"))?;
    let token = store
        .create_key(CreateKeyRequest {
            permissions: KeyPermissions {
                allowed_providers: Some(["alpha".to_owned()].into()),
                ..Default::default()
            },
            ..Default::default()
        })
        .await?
        .key;
    let bytes = b"--synthetic-boundary\r\nContent-Disposition: form-data; name=\"model\"\r\n\r\nsynthetic-model\r\n--synthetic-boundary\r\nContent-Disposition: form-data; name=\"file\"; filename=\"audio.wav\"\r\nContent-Type: audio/wav\r\n\r\n\x00\xffsynthetic\r\n--synthetic-boundary--\r\n";
    let req = Request::builder()
        .method("POST")
        .uri("/alpha/v1/audio/transcriptions")
        .header("x-api-key", &token)
        .header(
            header::CONTENT_TYPE,
            "multipart/form-data; boundary=synthetic-boundary",
        )
        .body(Body::from(bytes.as_slice()))?;
    let response = app(&f.state)?.oneshot(req).await?;
    assert_eq!(response.status(), 400);
    let recorded = f
        .upstream
        .requests
        .lock()
        .map_err(|_| anyhow!("requests"))?;
    assert_eq!(recorded.len(), 1);
    assert_eq!(
        recorded[0].2[header::CONTENT_TYPE],
        "multipart/form-data; boundary=synthetic-boundary"
    );
    assert!(!recorded[0].2.contains_key("x-api-key"));
    let raw = f
        .upstream
        .raw_bodies
        .lock()
        .map_err(|_| anyhow!("bodies"))?;
    assert_eq!(raw[0].as_ref(), bytes);
    Ok(())
}

#[tokio::test]
#[expect(
    clippy::panic_in_result_fn,
    reason = "Behavioral assertions with fallible fixture setup"
)]
async fn async_video_jobs_reach_same_tenant_bound_handler() -> Result<()> {
    let f = fixture_with_policy(
        StatusCode::BAD_REQUEST,
        crate::policy::PolicyConfig::default(),
        "fal",
    )
    .await?;
    let token = key(&f.state, "alpha").await?;
    let response = app(&f.state)?
        .oneshot(request(
            "POST",
            "/alpha/v1/eavs/jobs",
            &token,
            json!({"model":"allowed","input":{"prompt":"synthetic video"}}),
        )?)
        .await?;
    assert_ne!(response.status(), 404);
    let recorded = f
        .upstream
        .requests
        .lock()
        .map_err(|_| anyhow!("requests"))?;
    assert_eq!(recorded.len(), 1);
    assert_eq!(recorded[0].1, json!({"prompt":"synthetic video"}));
    Ok(())
}

use super::*;

#[tokio::test]
#[expect(
    clippy::panic_in_result_fn,
    reason = "Behavioral assertions with fallible fixture setup"
)]
async fn entire_native_surface_requires_ordinary_keys_before_upstream() -> Result<()> {
    let f = fixture(StatusCode::BAD_REQUEST).await?;
    for (method, path) in [
        ("POST", "/alpha/v1/systemone"),
        ("POST", "/alpha/v1/decisions"),
        ("POST", "/alpha/v1/images/generations"),
        ("POST", "/alpha/v1/images/edits"),
        ("POST", "/alpha/v1/images/variations"),
        ("POST", "/alpha/v1/embeddings"),
        ("POST", "/alpha/v1/videos"),
        ("GET", "/alpha/v1/videos/id/content"),
        ("POST", "/alpha/v1/speech-to-text"),
        ("POST", "/alpha/v1/text-to-speech/voice"),
        ("POST", "/alpha/v2/rerank"),
        ("POST", "/alpha/v2/embed"),
        ("POST", "/alpha/v1/pi/messages"),
        ("POST", "/alpha/v1/codex/responses"),
        ("GET", "/alpha/v1/realtime"),
        ("GET", "/v1/realtime"),
        ("GET", "/alpha/v1/speech-to-text/realtime"),
        ("GET", "/alpha/v1/text-to-speech/voice/stream-input"),
        ("GET", "/alpha/v1/text-to-speech/voice/multi-stream-input"),
        ("POST", "/alpha/v1/eavs/jobs"),
        ("GET", "/alpha/v1/eavs/jobs/handle"),
        ("GET", "/alpha/v1/eavs/jobs/handle/result"),
        ("POST", "/alpha/v1/eavs/jobs/handle/cancel"),
        ("GET", "/v1/eavs/catalog"),
        ("POST", "/v1/images/generations"),
    ] {
        for credential in ["", "synthetic-operator", "synthetic-master", "eavs-invalid"] {
            let response = app(&f.state)?
                .oneshot(request(method, path, credential, json!({}))?)
                .await?;
            assert_eq!(response.status(), 401, "{method} {path}");
        }
    }
    assert_eq!(f.upstream.calls.load(Ordering::SeqCst), 0);
    Ok(())
}

#[tokio::test]
#[expect(
    clippy::panic_in_result_fn,
    reason = "Behavioral assertions with fallible fixture setup"
)]
async fn native_media_and_systemone_reuse_authorized_pipeline_losslessly() -> Result<()> {
    for (kind, path, payload) in [
        (
            "typesafe",
            "/alpha/v1/systemone",
            json!({"model":"allowed","state":{"ticket":"refund"},"questions":{"urgent":{"type":"noul","instructions":"Urgent?"}}}),
        ),
        (
            "openai-compatible",
            "/alpha/v1/images/generations",
            json!({"model":"allowed","prompt":"synthetic image","size":"1024x1024"}),
        ),
        // Official Decisions guide shape; fixture model name is intentionally synthetic.
        (
            "openai-compatible",
            "/alpha/v1/decisions",
            json!({"model":"allowed","input":"The package arrived broken.","questions":[{"type":"predicate","name":"damaged","instructions":"Does this report damage?"}]}),
        ),
        (
            "openai-compatible",
            "/alpha/v1/embeddings",
            json!({"model":"allowed","input":["synthetic"]}),
        ),
        (
            "openai-compatible",
            "/alpha/v1/audio/speech",
            json!({"model":"allowed","input":"synthetic speech","voice":"alloy"}),
        ),
        (
            "cohere",
            "/alpha/v2/rerank",
            json!({"model":"allowed","query":"synthetic","documents":["one","two"]}),
        ),
        (
            "cohere",
            "/alpha/v2/embed",
            json!({"model":"allowed","texts":["synthetic"],"input_type":"search_document"}),
        ),
    ] {
        let f = fixture_with_policy(
            StatusCode::BAD_REQUEST,
            crate::policy::PolicyConfig::default(),
            kind,
        )
        .await?;
        let token = key(&f.state, "alpha").await?;
        let response = app(&f.state)?
            .oneshot(request("POST", path, &token, payload.clone())?)
            .await?;
        assert_eq!(response.status(), 400, "{path}");
        let recorded = f
            .upstream
            .requests
            .lock()
            .map_err(|_| anyhow!("requests poisoned"))?;
        assert_eq!(
            recorded.len(),
            1,
            "{path} must reach shared native pipeline"
        );
        assert_eq!(recorded[0].1, payload);
        assert_eq!(
            recorded[0].0,
            format!(
                "https://api.openai.com{}",
                path.strip_prefix("/alpha")
                    .ok_or_else(|| anyhow!("prefix"))?
            )
        );
        assert_eq!(
            recorded[0].2[header::AUTHORIZATION],
            "Bearer synthetic-upstream"
        );
    }
    Ok(())
}

#[tokio::test]
#[expect(
    clippy::panic_in_result_fn,
    reason = "Behavioral assertions with fallible fixture setup"
)]
async fn native_media_scope_and_caps_fail_before_upstream() -> Result<()> {
    let f = fixture(StatusCode::BAD_REQUEST).await?;
    let token = key(&f.state, "alpha").await?;
    for (path, model) in [
        ("/beta/v1/images/generations", "allowed"),
        ("/alpha/v1/images/generations", "blocked"),
        ("/alpha/v1/videos", "allowed"),
    ] {
        let response = app(&f.state)?
            .oneshot(request(
                "POST",
                path,
                &token,
                json!({"model":model,"prompt":"synthetic"}),
            )?)
            .await?;
        assert!(response.status().is_client_error());
    }
    let store = f.state.get_key_store().ok_or_else(|| anyhow!("store"))?;
    let capped = store
        .create_key(CreateKeyRequest {
            permissions: KeyPermissions {
                max_budget_usd: Some(1.0),
                ..Default::default()
            },
            ..Default::default()
        })
        .await?
        .key;
    let response = app(&f.state)?
        .oneshot(request(
            "POST",
            "/alpha/v1/images/generations",
            &capped,
            json!({"model":"allowed","prompt":"synthetic"}),
        )?)
        .await?;
    assert!(response.status().is_client_error());
    assert_eq!(f.upstream.calls.load(Ordering::SeqCst), 0);
    Ok(())
}

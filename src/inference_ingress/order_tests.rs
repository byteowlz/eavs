use super::*;

#[tokio::test]
#[expect(
    clippy::panic_in_result_fn,
    reason = "Exact-byte regression assertions with fallible setup"
)]
async fn native_ingress_preserves_caller_json_order_and_whitespace() -> Result<()> {
    let raw = br#"{ "model":"allowed", "state":{"task":"x","status":"y"},"questions":{"risk":{"type":"choice","criteria":{"low":null,"medium":null,"high":null}}}}"#;
    for (kind, path) in [
        ("typesafe", "/alpha/v1/systemone"),
        ("openai-compatible", "/alpha/v1/decisions"),
    ] {
        let f = fixture_with_policy(
            StatusCode::BAD_REQUEST,
            crate::policy::PolicyConfig::default(),
            kind,
        )
        .await?;
        let token = key(&f.state, "alpha").await?;
        let request = Request::builder()
            .method("POST")
            .uri(path)
            .header(header::AUTHORIZATION, format!("Bearer {token}"))
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(raw.as_slice()))?;
        assert_eq!(
            app(&f.state)?.oneshot(request).await?.status(),
            StatusCode::BAD_REQUEST
        );
        let bodies = f
            .upstream
            .raw_bodies
            .lock()
            .map_err(|_| anyhow!("bodies"))?;
        assert_eq!(bodies.len(), 1);
        assert_eq!(bodies[0].as_ref(), raw.as_slice(), "{path}");
    }
    Ok(())
}

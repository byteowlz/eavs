use super::*;
use crate::{
    config::AppConfig,
    enrollment::{Administrator, InviteRequest},
    keys::{CreateKeyRequest, KeyStore},
};
use axum::body::to_bytes;
use tower::ServiceExt;

type TestResult = Result<(), Box<dyn std::error::Error + Send + Sync>>;
async fn setup(
) -> Result<(AppState, Arc<KeyStore>, String, String), Box<dyn std::error::Error + Send + Sync>> {
    let mut config = AppConfig::with_defaults();
    config.keys.enabled = true;
    let state = AppState::new(config);
    let store = Arc::new(KeyStore::in_memory().await?);
    state
        .key_store
        .set(store.clone())
        .map_err(|_| "store initialization")?;
    let template = store.create_key(CreateKeyRequest::default()).await?;
    let invitation = store
        .invite_device(
            &Administrator::authenticate(Some("synthetic-root"), "synthetic-root")
                .map_err(|_| "admin")?,
            InviteRequest {
                template_id: template.key_id,
                invitation_ttl_seconds: 60,
                device_ttl_seconds: 120,
            },
        )
        .await
        .map_err(|_| "invitation")?;
    Ok((state, store, template.key, invitation.token))
}
async fn request(
    app: &Router,
    method: &str,
    path: &str,
    key: Option<&str>,
    body: String,
) -> Result<Response, Box<dyn std::error::Error + Send + Sync>> {
    let mut builder = Request::builder().method(method).uri(path);
    if let Some(key) = key {
        builder = builder.header("authorization", format!("Bearer {key}"));
    }
    Ok(app.clone().oneshot(builder.body(Body::from(body))?).await?)
}

#[tokio::test]
#[expect(
    clippy::panic_in_result_fn,
    reason = "fallible fixture setup preserves device ingress authentication and isolation assertions"
)]
async fn only_enrolled_credentials_can_discover_or_infer_and_revocation_is_rechecked() -> TestResult
{
    let (state, store, template, invitation) = setup().await?;
    let app = router(state);
    for key in [None, Some(template.as_str()), Some("synthetic-root")] {
        for (method, path) in [
            ("GET", "/v1/eavs/catalog"),
            ("POST", "/missing/v1/pi/messages"),
        ] {
            let response = request(&app, method, path, key, String::new()).await?;
            assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
            assert_eq!(response.headers()[CACHE_CONTROL], "no-store");
        }
    }
    let response = request(
        &app,
        "POST",
        "/device-enrollment/redeem",
        None,
        serde_json::json!({"token":invitation}).to_string(),
    )
    .await?;
    assert_eq!(response.status(), StatusCode::CREATED);
    assert_eq!(response.headers()[CACHE_CONTROL], "no-store");
    let credential: serde_json::Value =
        serde_json::from_slice(&to_bytes(response.into_body(), 4096).await?)?;
    let key = credential["key"].as_str().ok_or("credential")?;
    assert!(credential.get("listener_token").is_none());
    assert!(credential.get("master_key").is_none());
    assert_eq!(
        request(&app, "GET", "/v1/eavs/catalog", Some(key), String::new())
            .await?
            .status(),
        StatusCode::OK
    );
    // Unknown physical provider is rejected before any upstream request.
    assert_eq!(
        request(
            &app,
            "POST",
            "/missing/v1/pi/messages",
            Some(key),
            "{}".into()
        )
        .await?
        .status(),
        StatusCode::BAD_REQUEST
    );
    let hash = store.get_by_key(key).ok_or("cache publication")?.key_hash;
    store.disable_key(&hash).await?;
    assert_eq!(
        request(&app, "GET", "/v1/eavs/catalog", Some(key), String::new())
            .await?
            .status(),
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        request(
            &app,
            "POST",
            "/missing/v1/pi/messages",
            Some(key),
            "{}".into()
        )
        .await?
        .status(),
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        request(
            &app,
            "POST",
            "/device-enrollment/redeem",
            None,
            serde_json::json!({"token":invitation}).to_string()
        )
        .await?
        .status(),
        StatusCode::BAD_REQUEST
    );
    Ok(())
}

#[tokio::test]
#[expect(
    clippy::panic_in_result_fn,
    reason = "fallible HTTP fixtures preserve exhaustive operator surface isolation assertions"
)]
async fn no_operator_or_generic_proxy_routes_and_errors_are_private() -> TestResult {
    let (state, _, _, _) = setup().await?;
    let app = router(state);
    for path in [
        "/config",
        "/providers",
        "/admin/device-invitations",
        "/keys",
        "/v1/chat/completions",
        "/openai/v1/responses",
        "/v1/eavs/jobs",
        "/oauth/status",
    ] {
        for method in ["GET", "POST"] {
            let response =
                request(&app, method, path, Some("synthetic-root"), String::new()).await?;
            assert_eq!(response.status(), StatusCode::NOT_FOUND, "{method} {path}");
            assert_eq!(response.headers()[CACHE_CONTROL], "no-store");
        }
    }
    for payload in ["private-invalid-token".into(), "x".repeat(2049)] {
        let response = request(&app, "POST", "/device-enrollment/redeem", None, payload).await?;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        assert_eq!(
            to_bytes(response.into_body(), 256).await?.as_ref(),
            br#"{"error":"invalid_enrollment"}"#
        );
    }
    Ok(())
}

#[tokio::test]
#[expect(
    clippy::panic_in_result_fn,
    reason = "fallible HTTP fixtures retain own-key-only revocation and denial assertions"
)]
async fn self_revocation_cannot_disable_a_caller_selected_key() -> TestResult {
    let (state, store, template, invitation) = setup().await?;
    let app = router(state);
    for token in [None, Some(template.as_str()), Some("synthetic-root")] {
        assert_eq!(
            request(&app, "POST", "/v1/eavs/device/revoke", token, "{}".into())
                .await?
                .status(),
            StatusCode::UNAUTHORIZED
        );
    }
    let response = request(
        &app,
        "POST",
        "/device-enrollment/redeem",
        None,
        serde_json::json!({"token":invitation}).to_string(),
    )
    .await?;
    assert_eq!(response.status(), StatusCode::CREATED);
    let credential: serde_json::Value =
        serde_json::from_slice(&to_bytes(response.into_body(), 4096).await?)?;
    let key = credential["key"].as_str().ok_or("credential")?;
    assert_eq!(
        request(
            &app,
            "POST",
            "/v1/eavs/device/revoke",
            Some(key),
            "x".repeat(2049)
        )
        .await?
        .status(),
        StatusCode::BAD_REQUEST
    );
    let foreign = store.get_by_key(&template).ok_or("template publication")?;
    let response = request(
        &app,
        "POST",
        "/v1/eavs/device/revoke",
        Some(key),
        serde_json::json!({"key_id":foreign.key_id,"key_hash":foreign.key_hash}).to_string(),
    )
    .await?;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()[CACHE_CONTROL], "no-store");
    let receipt: serde_json::Value =
        serde_json::from_slice(&to_bytes(response.into_body(), 4096).await?)?;
    assert_eq!(receipt["key_id"], credential["key_id"]);
    assert_eq!(receipt["revoked"], true);
    assert!(store
        .get_by_key(&template)
        .ok_or("template retained")?
        .is_valid());
    for (method, path) in [
        ("GET", "/v1/eavs/catalog"),
        ("POST", "/fixture/v1/pi/messages"),
        ("POST", "/v1/eavs/device/revoke"),
    ] {
        assert_eq!(
            request(&app, method, path, Some(key), "{}".into())
                .await?
                .status(),
            StatusCode::UNAUTHORIZED
        );
    }
    Ok(())
}

#[tokio::test]
#[expect(
    clippy::panic_in_result_fn,
    reason = "fallible HTTP setup with intentional process-local abuse-throttle assertions"
)]
async fn claim_throttle_is_bounded_and_does_not_charge_catalog_access() -> TestResult {
    let (state, _, _, _) = setup().await?;
    let app = router(state);
    for _ in 0..CLAIMS_PER_MINUTE {
        assert_eq!(
            request(&app, "POST", "/device-enrollment/redeem", None, "{}".into())
                .await?
                .status(),
            StatusCode::BAD_REQUEST
        );
    }
    let response = request(&app, "POST", "/device-enrollment/redeem", None, "{}".into()).await?;
    assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(response.headers()[CACHE_CONTROL], "no-store");
    assert_eq!(
        request(&app, "GET", "/v1/eavs/catalog", None, String::new())
            .await?
            .status(),
        StatusCode::UNAUTHORIZED
    );
    Ok(())
}

#[tokio::test]
#[expect(
    clippy::panic_in_result_fn,
    reason = "Behavioral assertions in fallible fixture"
)]
async fn external_writer_device_and_pi_preflight_auth() -> TestResult {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("keys.sqlite");
    let mut config = AppConfig::with_defaults();
    config.keys.enabled = true;
    let state = AppState::new(config);
    let server = Arc::new(KeyStore::new(&path).await?);
    state
        .key_store
        .set(server)
        .map_err(|_| "store initialization")?;
    let writer = KeyStore::new(&path).await?;
    let key = writer
        .create_key(CreateKeyRequest {
            metadata: serde_json::json!({"kind":"device_enrollment"}),
            ..Default::default()
        })
        .await?;
    let mut headers = HeaderMap::new();
    headers.insert(
        "authorization",
        HeaderValue::from_str(&format!("Bearer {}", key.key))?,
    );
    assert!(device_authorized(&state, &headers).await);
    let pi_request = || -> Result<Request<Body>, http::Error> {
        Request::builder()
            .method("POST")
            .uri("/missing/v1/pi/messages")
            .header("authorization", format!("Bearer {}", key.key))
            .body(Body::from("{}"))
    };
    // Authentication passed: the intentionally missing provider prevents any
    // inference and returns the existing explicit-provider failure instead.
    assert_eq!(
        crate::pi_messages::handle(state.clone(), pi_request()?, Some("missing".into()))
            .await
            .status(),
        StatusCode::BAD_REQUEST
    );
    writer.disable_key(&key.key_hash).await?;
    assert!(!device_authorized(&state, &headers).await);
    assert_eq!(
        crate::pi_messages::handle(state.clone(), pi_request()?, Some("missing".into()))
            .await
            .status(),
        StatusCode::UNAUTHORIZED
    );
    writer.delete_key(&key.key_hash).await?;
    assert!(!device_authorized(&state, &headers).await);
    assert_eq!(
        crate::pi_messages::handle(state, pi_request()?, Some("missing".into()))
            .await
            .status(),
        StatusCode::UNAUTHORIZED
    );
    Ok(())
}

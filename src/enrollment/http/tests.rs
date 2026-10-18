use super::{issue, redeem, revoke, Administrator, AppState, StatusCode};
use crate::enrollment::InviteRequest;
use crate::{
    config::AppConfig,
    keys::{CreateKeyRequest, KeyStore},
};
use axum::{
    body::{to_bytes, Body},
    http::Request,
    middleware,
    routing::post,
    Router,
};
use tower::ServiceExt;

fn test_router(state: AppState) -> Router {
    Router::new()
        .route(
            "/admin/device-invitations",
            post(issue).layer(axum::extract::DefaultBodyLimit::max(2048)),
        )
        .route(
            "/admin/device-invitations/revoke",
            post(revoke).layer(axum::extract::DefaultBodyLimit::max(2048)),
        )
        .route(
            "/device-enrollment/redeem",
            post(redeem).layer(axum::extract::DefaultBodyLimit::max(2048)),
        )
        .with_state(state)
        .layer(middleware::from_fn_with_state(
            crate::api::ListenerAuth::new("synthetic-listener".into()),
            crate::api::require_listener_auth,
        ))
}

#[tokio::test]
#[expect(
    clippy::panic_in_result_fn,
    reason = "fallible fixture setup with intentional HTTP privacy and authentication assertions"
)]
async fn root_listener_and_private_bounded_http(
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let mut config = AppConfig::with_defaults();
    config.keys.enabled = true;
    config.keys.master_key = Some("synthetic-root".into());
    let state = AppState::new(config);
    let store = std::sync::Arc::new(KeyStore::in_memory().await?);
    let template = store.create_key(CreateKeyRequest::default()).await?;
    state
        .key_store
        .set(store.clone())
        .map_err(|_| "set store")?;
    let app = test_router(state);
    let payload=serde_json::json!({"template_id":template.key_id,"invitation_ttl_seconds":60,"device_ttl_seconds":120}).to_string();
    for (listener, root, status) in [
        (false, true, StatusCode::UNAUTHORIZED),
        (true, false, StatusCode::UNAUTHORIZED),
        (true, true, StatusCode::CREATED),
    ] {
        let mut req = Request::builder()
            .method("POST")
            .uri("/admin/device-invitations");
        if listener {
            req = req.header("x-eavs-token", "synthetic-listener");
        }
        if root {
            req = req.header("authorization", "Bearer synthetic-root");
        } else {
            req = req.header("authorization", "Bearer synthetic-listener");
        }
        let r = app
            .clone()
            .oneshot(req.body(Body::from(payload.clone()))?)
            .await?;
        assert_eq!(r.status(), status);
        if listener {
            assert_eq!(r.headers()["cache-control"], "no-store");
        }
    }
    let i = store
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
        .map_err(|_| "invite")?;
    let r = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/device-enrollment/redeem")
                .header("x-eavs-token", "synthetic-listener")
                .body(Body::from(serde_json::json!({"token":i.token}).to_string()))?,
        )
        .await?;
    assert_eq!(r.status(), StatusCode::CREATED);
    assert_eq!(r.headers()["cache-control"], "no-store");
    let credential: serde_json::Value =
        serde_json::from_slice(&to_bytes(r.into_body(), 2048).await?)?;
    assert!(store
        .get_by_key(credential["key"].as_str().ok_or("key")?)
        .is_some());
    for body in [
        serde_json::json!({"token":i.token}).to_string(),
        "{secret diagnostic invalid json".into(),
        "x".repeat(3000),
    ] {
        let r = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/device-enrollment/redeem")
                    .header("x-eavs-token", "synthetic-listener")
                    .body(Body::from(body))?,
            )
            .await?;
        assert_eq!(r.status(), StatusCode::BAD_REQUEST);
        assert_eq!(r.headers()["cache-control"], "no-store");
        let bytes = to_bytes(r.into_body(), 128).await?;
        assert_eq!(bytes.as_ref(), br#"{"error":"invalid_enrollment"}"#);
    }
    Ok(())
}

use super::{issue, revoke, Administrator, AppState, StatusCode};
use crate::{
    config::AppConfig,
    enrollment::InviteRequest,
    keys::{CreateKeyRequest, KeyStore},
};
use axum::{
    body::{to_bytes, Body},
    http::Request,
    routing::post,
    Router,
};
use tower::ServiceExt;

#[tokio::test]
#[expect(
    clippy::panic_in_result_fn,
    reason = "fallible fixtures with intentional revoke authorization and private error assertions"
)]
async fn revoke_root_only_and_unavailable_error_is_private(
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let mut config = AppConfig::with_defaults();
    config.keys.enabled = true;
    config.keys.master_key = Some("synthetic-root".into());
    let state = AppState::new(config);
    let store = std::sync::Arc::new(KeyStore::in_memory().await?);
    let source = store.create_key(CreateKeyRequest::default()).await?;
    let admin = Administrator::authenticate(Some("synthetic-root"), "synthetic-root")
        .map_err(|_| "admin")?;
    let invitation = store
        .invite_device(
            &admin,
            InviteRequest {
                template_id: source.key_id,
                invitation_ttl_seconds: 60,
                device_ttl_seconds: 120,
            },
        )
        .await
        .map_err(|_| "invite")?;
    let unavailable = Router::new()
        .route("/issue", post(issue))
        .with_state(state.clone());
    let failure=unavailable.oneshot(Request::builder().method("POST").uri("/issue").header("authorization","Bearer synthetic-root").body(Body::from(r#"{"template_id":"synthetic","invitation_ttl_seconds":60,"device_ttl_seconds":120}"#))?).await?;
    assert_eq!(failure.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(failure.headers()["cache-control"], "no-store");
    assert_eq!(
        to_bytes(failure.into_body(), 128).await?.as_ref(),
        br#"{"error":"enrollment_unavailable"}"#
    );
    state.key_store.set(store.clone()).map_err(|_| "store")?;
    let app = Router::new()
        .route("/revoke", post(revoke))
        .with_state(state);
    for (bearer, status) in [
        ("synthetic-listener", StatusCode::UNAUTHORIZED),
        ("synthetic-root", StatusCode::OK),
        ("synthetic-root", StatusCode::OK),
    ] {
        let reply = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/revoke")
                    .header("authorization", format!("Bearer {bearer}"))
                    .body(Body::from(
                        serde_json::json!({"token":invitation.token}).to_string(),
                    ))?,
            )
            .await?;
        assert_eq!(reply.status(), status);
        assert_eq!(reply.headers()["cache-control"], "no-store");
    }
    assert!(store.redeem_device(&invitation.token).await.is_err());
    Ok(())
}

use super::*;
use crate::{
    config::{AppConfig, ProviderConfig},
    provider_store::{ProviderRequest, ProviderStore},
};
use axum::{
    body::{to_bytes, Body},
    routing::{get, post},
    Router,
};
use std::sync::Arc;
use tower::ServiceExt;

fn request(
    path: &str,
    authenticated: bool,
    body: Option<serde_json::Value>,
) -> Result<http::Request<Body>, http::Error> {
    let mut builder = http::Request::builder()
        .uri(path)
        .header("content-type", "application/json");
    if authenticated {
        builder = builder.header("authorization", "Bearer synthetic-admin");
    }
    match body {
        Some(body) => builder.method("POST").body(Body::from(body.to_string())),
        None => builder.method("GET").body(Body::empty()),
    }
}

#[tokio::test]
#[expect(
    clippy::panic_in_result_fn,
    reason = "Assertions verify authenticated source metadata and race-safe creation; fixture errors propagate"
)]
async fn fleet_sources_and_create_only_preserve_configuration(
) -> Result<(), Box<dyn std::error::Error>> {
    let mut config = AppConfig::with_defaults();
    config.providers.clear();
    config.providers.insert(
        "fixed".into(),
        ProviderConfig {
            type_: "openai".into(),
            api_key: "synthetic-toml-secret".into(),
            ..Default::default()
        },
    );
    config.keys.master_key = Some("synthetic-admin".into());
    let state = AppState::new(config);
    let store = Arc::new(ProviderStore::in_memory().await?);
    store
        .upsert_provider(ProviderRequest {
            name: "disabled".into(),
            config: serde_json::json!({"type":"openai","api_key":"synthetic-disabled-secret"}),
            enabled: false,
        })
        .await?;
    state
        .provider_store
        .set(store.clone())
        .map_err(|_| "already initialized")?;
    let app = Router::new()
        .route("/admin/providers/create", post(create_provider_handler))
        .route("/admin/providers/sources", get(credentials::sources))
        .with_state(state);
    let denied = app
        .clone()
        .oneshot(request("/admin/providers/sources", false, None)?)
        .await?;
    assert_eq!(denied.status(), StatusCode::UNAUTHORIZED);
    let payload = serde_json::json!({"name":"managed","config":{"type":"openai","api_key":"synthetic-new-secret"}});
    let (first, second) = tokio::join!(
        app.clone().oneshot(request(
            "/admin/providers/create",
            true,
            Some(payload.clone())
        )?),
        app.clone()
            .oneshot(request("/admin/providers/create", true, Some(payload))?),
    );
    let statuses = [first?.status(), second?.status()];
    assert_eq!(
        statuses
            .iter()
            .filter(|status| **status == StatusCode::OK)
            .count(),
        1
    );
    assert_eq!(
        statuses
            .iter()
            .filter(|status| **status == StatusCode::CONFLICT)
            .count(),
        1
    );
    let collision = app.clone().oneshot(request("/admin/providers/create", true, Some(serde_json::json!({"name":"fixed","config":{"type":"openai","api_key":"synthetic-overwrite"}})))?).await?;
    assert_eq!(collision.status(), StatusCode::CONFLICT);
    let inventory = app
        .oneshot(request("/admin/providers/sources", true, None)?)
        .await?;
    assert_eq!(inventory.status(), StatusCode::OK);
    let bytes = to_bytes(inventory.into_body(), 1_048_576).await?;
    let text = String::from_utf8_lossy(&bytes);
    assert!(!text.contains("synthetic"));
    let sources: Vec<serde_json::Value> = serde_json::from_slice(&bytes)?;
    assert_eq!(sources.len(), 3);
    assert!(sources.iter().any(|source| source["name"] == "fixed"
        && source["source"] == "toml"
        && source["revision"].is_null()));
    assert!(sources.iter().any(|source| source["name"] == "managed"
        && source["source"] == "admin"
        && source["revision"].is_string()));
    assert!(sources
        .iter()
        .any(|source| source["name"] == "disabled" && source["enabled"] == false));
    assert_eq!(
        store
            .get_provider("managed")
            .ok_or("missing managed provider")?
            .config["api_key"],
        "synthetic-new-secret"
    );
    Ok(())
}

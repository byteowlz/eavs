use super::*;
#[path = "extra_tests.rs"]
mod extra_tests;
use crate::{
    config::AppConfig,
    keys::{CreateKeyRequest, KeyStore},
    provider_store::{ProviderRequest, ProviderStore},
    upstream::{Upstream, UpstreamRequest, UpstreamResponse},
};
use anyhow::{anyhow, Result};
use axum::{
    body::{to_bytes, Body},
    http::Request,
    middleware,
    routing::get,
    Router,
};
use futures::future::BoxFuture;
use serde_json::{json, Value};
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc, Mutex,
};
use tower::ServiceExt;

struct Tripwire(AtomicUsize);
impl Upstream for Tripwire {
    fn send(&self, _: UpstreamRequest) -> BoxFuture<'_, Result<UpstreamResponse, std::io::Error>> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Box::pin(async { Err(std::io::Error::other("catalog must not generate")) })
    }
}
struct Fixture {
    state: AppState,
    dir: tempfile::TempDir,
    tripwire: Arc<Tripwire>,
}
fn model(id: &str) -> Result<ModelShortlistEntry> {
    Ok(serde_json::from_value(
        json!({"id":id, "name":"Public label", "compat":{"privateSecret":"fixture-private-compat"}}),
    )?)
}
fn provider(ids: &[&str]) -> Result<ProviderConfig> {
    Ok(ProviderConfig {
        type_: "openai-compatible".into(),
        base_url: "https://fixture-private-host.invalid".into(),
        api_key: "env:FIXTURE_PRIVATE_KEY".into(),
        headers: [(
            "fixture-private-header".into(),
            "fixture-private-value".into(),
        )]
        .into(),
        models: ids.iter().map(|id| model(id)).collect::<Result<_>>()?,
        ..Default::default()
    })
}
async fn fixture() -> Result<Fixture> {
    let dir = tempfile::tempdir()?;
    let mut config = AppConfig::with_defaults();
    config.providers.clear();
    config.keys.enabled = true;
    config.keys.master_key = Some("fixture-private-master".into());
    config.providers.insert(
        "alpha".into(),
        provider(&["allowed", "blocked", "rewrite", "denied", "field"])?,
    );
    config
        .providers
        .insert("hidden".into(), provider(&["hidden-model"])?);
    let tripwire = Arc::new(Tripwire(AtomicUsize::new(0)));
    let state = AppState::new_with_upstream(config, tripwire.clone());
    let store = Arc::new(KeyStore::new(&dir.path().join("keys.sqlite")).await?);
    state
        .key_store
        .set(store)
        .map_err(|_| anyhow!("fixture key store already set"))?;
    let providers = Arc::new(ProviderStore::new(&dir.path().join("providers.sqlite")).await?);
    state
        .provider_store
        .set(providers)
        .map_err(|_| anyhow!("fixture provider store already set"))?;
    Ok(Fixture {
        state,
        dir,
        tripwire,
    })
}
async fn key(state: &AppState, scope: KeyPermissions) -> Result<String> {
    Ok(state
        .get_key_store()
        .ok_or_else(|| anyhow!("missing fixture store"))?
        .create_key(CreateKeyRequest {
            permissions: scope,
            ..Default::default()
        })
        .await?
        .key)
}
fn app(state: AppState) -> Router {
    Router::new()
        .route("/v1/eavs/catalog", get(handler))
        .with_state(state)
        .layer(middleware::from_fn_with_state(
            crate::api::ListenerAuth::new("fixture-listener".into()),
            crate::api::require_listener_auth,
        ))
}
async fn request(
    state: &AppState,
    token: Option<&str>,
    listener: bool,
) -> Result<(StatusCode, HeaderMap, Value)> {
    let mut request = Request::builder().uri("/v1/eavs/catalog");
    if let Some(token) = token {
        request = request.header("authorization", format!("Bearer {token}"));
    }
    if listener {
        request = request.header("x-eavs-token", "fixture-listener");
    }
    let response = app(state.clone())
        .oneshot(request.body(Body::empty())?)
        .await?;
    let status = response.status();
    let headers = response.headers().clone();
    let value = serde_json::from_slice(&to_bytes(response.into_body(), MAX_BYTES).await?)?;
    Ok((status, headers, value))
}
fn model_ids(value: &Value) -> Vec<&str> {
    value["providers"]
        .as_array()
        .into_iter()
        .flatten()
        .flat_map(|p| p["models"].as_array().into_iter().flatten())
        .filter_map(|m| m["id"].as_str())
        .collect()
}

#[tokio::test]
#[expect(
    clippy::panic_in_result_fn,
    reason = "Behavioral assertions in fallible offline fixture"
)]
async fn authenticates_listener_and_virtual_key_and_revocation() -> Result<()> {
    let f = fixture().await?;
    let token = key(&f.state, KeyPermissions::default()).await?;
    for value in [
        None,
        Some("fixture-private-master"),
        Some("sk-eavs-invalid"),
        Some("fixture-listener"),
    ] {
        let (status, _, body) = request(&f.state, value, true).await?;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        assert!(!body.to_string().contains("private"));
    }
    assert_eq!(
        request(&f.state, Some(&token), false).await?.0,
        StatusCode::UNAUTHORIZED
    );
    let (status, headers, _) = request(&f.state, Some(&token), true).await?;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(headers["cache-control"], "private, no-store");
    let store = f
        .state
        .get_key_store()
        .ok_or_else(|| anyhow!("missing store"))?;
    let hash = store
        .get_by_key(&token)
        .ok_or_else(|| anyhow!("missing key"))?
        .key_hash;
    store.disable_key(&hash).await?;
    assert_eq!(
        request(&f.state, Some(&token), true).await?.0,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(f.tripwire.0.load(Ordering::SeqCst), 0);
    Ok(())
}

#[tokio::test]
#[expect(
    clippy::panic_in_result_fn,
    reason = "Behavioral assertions in fallible offline fixture"
)]
async fn scopes_physical_models_policy_and_allowlisted_metadata() -> Result<()> {
    let mut f = fixture().await?;
    let mut config = (*f.state.config).clone();
    config.policy = serde_json::from_value(json!({"enabled":true,"rules":[
        {"action":"rewrite_model","model":"rewrite","to":"hidden-model"},
        {"action":"deny","model":"denied","reason":"fixture-private-policy"},
        {"action":"set_field","model":"field","field":"model","value":"hidden-model"}
    ]}))?;
    let upstream = f.tripwire.clone();
    let state = AppState::new_with_upstream(config, upstream);
    state
        .key_store
        .set(
            f.state
                .get_key_store()
                .ok_or_else(|| anyhow!("missing store"))?
                .clone(),
        )
        .map_err(|_| anyhow!("store already set"))?;
    f.state = state;
    let token = key(
        &f.state,
        KeyPermissions {
            allowed_providers: Some(["alpha".into()].into()),
            blocked_models: Some(["blocked".into()].into()),
            ..Default::default()
        },
    )
    .await?;
    let (_, _, body) = request(&f.state, Some(&token), true).await?;
    assert_eq!(model_ids(&body), ["allowed"]);
    assert_eq!(body["schema_version"], 1);
    assert_eq!(
        body["providers"][0]["request_path"],
        "/alpha/v1/chat/completions"
    );
    assert!(body["providers"][0]["models"][0]["pricing"].is_null());
    let text = body.to_string();
    for private in [
        "fixture-private",
        "hidden-model",
        "env:",
        "api_key",
        "oauth",
        "key_hash",
    ] {
        assert!(!text.contains(private), "private field leaked");
    }
    assert_eq!(f.tripwire.0.load(Ordering::SeqCst), 0);
    Ok(())
}

#[tokio::test]
#[expect(
    clippy::panic_in_result_fn,
    reason = "Behavioral assertions in fallible offline fixture"
)]
async fn live_store_shortlists_and_file_ownership() -> Result<()> {
    let f = fixture().await?;
    let store = f
        .state
        .get_provider_store()
        .ok_or_else(|| anyhow!("missing providers"))?;
    for (name, enabled) in [("managed", true), ("ALPHA", true), ("disabled", false)] {
        store
            .create_provider(ProviderRequest {
                name: name.into(),
                config: serde_json::to_value(provider(&["db-model"])?)?,
                enabled,
            })
            .await?;
    }
    store
        .create_provider(ProviderRequest {
            name: "invalid-row".into(),
            config: json!({"type":"unknown", "api_key":"fixture-private-invalid"}),
            enabled: true,
        })
        .await?;
    let all = key(&f.state, KeyPermissions::default()).await?;
    let inventory = request(&f.state, Some(&all), true).await?.2;
    assert!(!inventory.to_string().contains("invalid-row"));
    assert!(!inventory.to_string().contains("disabled"));
    let token = key(
        &f.state,
        KeyPermissions {
            allowed_providers: Some(["alpha".into(), "managed".into()].into()),
            ..Default::default()
        },
    )
    .await?;
    let before = request(&f.state, Some(&token), true).await?.2;
    assert_eq!(before["providers"].as_array().map(Vec::len), Some(2));
    assert_eq!(
        model_ids(&before)
            .iter()
            .filter(|id| **id == "db-model")
            .count(),
        1
    );
    store.add_model("managed", model("db-new")?).await?;
    let after = request(&f.state, Some(&token), true).await?.2;
    assert!(model_ids(&after).contains(&"db-new"));
    assert!(!model_ids(&after).contains(&"db-model"));
    assert_eq!(
        before["file_routing_generation"],
        after["file_routing_generation"]
    );
    assert_eq!(f.tripwire.0.load(Ordering::SeqCst), 0);
    Ok(())
}

#[tokio::test]
#[expect(
    clippy::panic_in_result_fn,
    reason = "Behavioral assertions in fallible offline fixture"
)]
async fn reload_changes_candidates_without_rebuilding_runtime() -> Result<()> {
    let f = fixture().await?;
    let source = Arc::new(Mutex::new((*f.state.config).clone()));
    let loader = source.clone();
    f.state
        .routing
        .configure(move || {
            loader
                .lock()
                .map(|c| c.clone())
                .map_err(|_| crate::routing::Failure::Unavailable)
        })
        .map_err(|e| anyhow!(e.code()))?;
    let token = key(&f.state, KeyPermissions::default()).await?;
    let before = request(&f.state, Some(&token), true).await?.2;
    source
        .lock()
        .map_err(|_| anyhow!("fixture poisoned"))?
        .providers
        .get_mut("alpha")
        .ok_or_else(|| anyhow!("missing provider"))?
        .models = vec![model("after-reload")?];
    let preview = f
        .state
        .routing
        .reload(
            &f.state,
            crate::routing::ReloadRequest {
                dry_run: true,
                expected_generation: None,
                expected_candidate_generation: None,
            },
        )
        .await
        .map_err(|e| anyhow!(e.code()))?;
    f.state
        .routing
        .reload(
            &f.state,
            crate::routing::ReloadRequest {
                dry_run: false,
                expected_generation: Some(preview.current_generation),
                expected_candidate_generation: Some(preview.candidate_generation),
            },
        )
        .await
        .map_err(|e| anyhow!(e.code()))?;
    let after = request(&f.state, Some(&token), true).await?.2;
    assert!(model_ids(&after).contains(&"after-reload"));
    assert!(!model_ids(&after).contains(&"allowed"));
    assert_ne!(
        before["file_routing_generation"],
        after["file_routing_generation"]
    );
    assert_eq!(f.tripwire.0.load(Ordering::SeqCst), 0);
    Ok(())
}

#[tokio::test]
#[expect(
    clippy::panic_in_result_fn,
    reason = "Behavioral assertions in fallible offline fixture"
)]
async fn discovery_does_not_charge_inference_quota_or_budget() -> Result<()> {
    let f = fixture().await?;
    let token = key(
        &f.state,
        KeyPermissions {
            rpm_limit: Some(1),
            max_budget_usd: Some(0.0),
            ..Default::default()
        },
    )
    .await?;
    for _ in 0..3 {
        assert_eq!(
            request(&f.state, Some(&token), true).await?.0,
            StatusCode::OK
        );
    }
    let store = f
        .state
        .get_key_store()
        .ok_or_else(|| anyhow!("missing store"))?;
    let info = store
        .get_by_key(&token)
        .ok_or_else(|| anyhow!("missing key"))?;
    assert_eq!(info.usage.total_requests, 0);
    assert!(f
        .state
        .rate_limiter
        .check_request(&info.key_hash, Some(1), None)
        .is_ok());
    assert_eq!(f.tripwire.0.load(Ordering::SeqCst), 0);
    Ok(())
}

#[test]
#[expect(
    clippy::panic_in_result_fn,
    reason = "Behavioral assertions in fallible offline fixture"
)]
fn unsupported_operations_and_price_claims_are_not_inferred() -> Result<()> {
    for kind in [
        "google",
        "google-vertex",
        "fal",
        "replicate",
        "typesafe",
        "cohere",
        "voyage",
        "elevenlabs",
        "unknown",
    ] {
        assert!(route(&ProviderConfig {
            type_: kind.into(),
            ..Default::default()
        })
        .is_none());
    }
    assert!(estimate(&ModelCost::default()).is_none());
    assert!(estimate(&ModelCost {
        input: f64::NAN,
        ..Default::default()
    })
    .is_none());
    assert_eq!(
        serde_json::to_value(estimate(&ModelCost {
            input: 1.0,
            ..Default::default()
        }))?["status"],
        "unverified_estimate"
    );
    Ok(())
}

#[tokio::test]
#[expect(
    clippy::panic_in_result_fn,
    reason = "Behavioral assertions in fallible fixture"
)]
async fn external_writer_catalog_auth() -> Result<()> {
    let f = fixture().await?;
    let path = f.dir.path().join("keys.sqlite");
    let writer = KeyStore::new(&path).await?;
    let key = writer.create_key(CreateKeyRequest::default()).await?;
    let (status, _, value) = request(&f.state, Some(&key.key), true).await?;
    assert_eq!(status, StatusCode::OK);
    assert!(!model_ids(&value).is_empty());
    let pool = sqlx::SqlitePool::connect(&format!("sqlite:{}", path.display())).await?;
    sqlx::query("UPDATE virtual_keys SET permissions='{\"allowed_providers\":[\"other\"]}'")
        .execute(&pool)
        .await?;
    let (status, _, value) = request(&f.state, Some(&key.key), true).await?;
    assert_eq!(status, StatusCode::OK);
    assert!(model_ids(&value).is_empty());
    sqlx::query("UPDATE virtual_keys SET permissions='malformed'")
        .execute(&pool)
        .await?;
    assert_ne!(
        request(&f.state, Some(&key.key), true).await?.0,
        StatusCode::OK
    );
    sqlx::query("DELETE FROM virtual_keys")
        .execute(&pool)
        .await?;
    assert_eq!(
        request(&f.state, Some(&key.key), true).await?.0,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(f.tripwire.0.load(Ordering::SeqCst), 0);
    Ok(())
}

#[tokio::test]
#[expect(
    clippy::panic_in_result_fn,
    reason = "Behavioral assertions in fallible offline fixture"
)]
async fn publishes_declared_and_learned_reasoning_efforts() -> Result<()> {
    let mut fixture = fixture().await?;
    let mut config = (*fixture.state.config).clone();
    let alpha = config
        .providers
        .get_mut("alpha")
        .ok_or_else(|| anyhow!("fixture provider"))?;
    for model in &mut alpha.models {
        if model.id == "allowed" {
            model.reasoning_efforts = vec!["low".into(), "xhigh".into()];
        }
    }
    let learned = fixture.state.learned_efforts.clone();
    fixture.state = AppState::new_with_upstream(config, fixture.tripwire.clone());
    fixture.state.learned_efforts = learned;
    fixture
        .state
        .key_store
        .set(Arc::new(
            KeyStore::new(&fixture.dir.path().join("keys2.sqlite")).await?,
        ))
        .map_err(|_| anyhow!("store already set"))?;
    fixture
        .state
        .learned_efforts
        .learn("alpha", "field", vec!["medium".into()]);
    fixture.state.learned_adaptive.learn("alpha", "field", true);
    let token = key(&fixture.state, KeyPermissions::default()).await?;
    let (status, _, value) = request(&fixture.state, Some(&token), true).await?;
    assert_eq!(status, StatusCode::OK);
    let models: Vec<&Value> = value["providers"]
        .as_array()
        .into_iter()
        .flatten()
        .flat_map(|p| p["models"].as_array().into_iter().flatten())
        .collect();
    let efforts = |id: &str| {
        models
            .iter()
            .find(|m| m["id"] == id)
            .map(|m| m.get("reasoning_efforts").cloned())
    };
    assert_eq!(
        efforts("allowed"),
        Some(Some(serde_json::json!(["low", "xhigh"])))
    );
    assert_eq!(efforts("field"), Some(Some(serde_json::json!(["medium"]))));
    assert_eq!(efforts("blocked"), Some(None), "unknown values are omitted");
    let adaptive = |id: &str| {
        models
            .iter()
            .find(|m| m["id"] == id)
            .and_then(|m| m["compat"].get("forceAdaptiveThinking").cloned())
    };
    assert_eq!(adaptive("field"), Some(serde_json::json!(true)));
    assert_eq!(adaptive("allowed"), None);
    assert_eq!(
        fixture.tripwire.0.load(std::sync::atomic::Ordering::SeqCst),
        0
    );
    Ok(())
}

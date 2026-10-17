use super::*;
use anyhow::{anyhow, Result};
use std::sync::Mutex as StdMutex;

fn config() -> AppConfig {
    let mut config = AppConfig::with_defaults();
    config.keys.enabled = false;
    config.keys.master_key = Some("synthetic-admin".into());
    config.providers.insert(
        "a".into(),
        ProviderConfig {
            base_url: "http://127.0.0.1:1/a".into(),
            api_key: "synthetic-a".into(),
            ..ProviderConfig::default()
        },
    );
    config
}
fn fixture() -> Result<(AppState, Arc<StdMutex<AppConfig>>)> {
    let original = config();
    let candidate = Arc::new(StdMutex::new(original.clone()));
    let source = candidate.clone();
    let state = AppState::new(original);
    state
        .routing
        .configure(move || {
            source
                .lock()
                .map(|config| config.clone())
                .map_err(|_| Failure::Unavailable)
        })
        .map_err(|error| anyhow!(error.code()))?;
    Ok((state, candidate))
}
fn edit(source: &StdMutex<AppConfig>, change: impl FnOnce(&mut AppConfig)) -> Result<()> {
    let mut guard = source
        .lock()
        .map_err(|_| anyhow!("fixture lock poisoned"))?;
    change(&mut guard);
    Ok(())
}
fn preview() -> ReloadRequest {
    ReloadRequest {
        dry_run: true,
        expected_generation: None,
        expected_candidate_generation: None,
    }
}
fn apply(plan: &ReloadResult) -> ReloadRequest {
    ReloadRequest {
        dry_run: false,
        expected_generation: Some(plan.current_generation.clone()),
        expected_candidate_generation: Some(plan.candidate_generation.clone()),
    }
}
async fn plan(state: &AppState) -> Result<ReloadResult> {
    state
        .routing
        .reload(state, preview())
        .await
        .map_err(|e| anyhow!(e.code()))
}
async fn publish(state: &AppState, result: &ReloadResult) -> Result<ReloadResult> {
    state
        .routing
        .reload(state, apply(result))
        .await
        .map_err(|e| anyhow!(e.code()))
}

#[tokio::test]
#[expect(
    clippy::panic_in_result_fn,
    reason = "Behavioral assertions intentionally fail this regression"
)]
async fn publication_pins_old_operations_and_preserves_runtime_objects() -> Result<()> {
    let (state, source) = fixture()?;
    let old = state.pin_routes();
    let generation = old.file_routing_generation();
    edit(&source, |config| {
        config.providers.remove("a");
        config
            .providers
            .insert("b".into(), ProviderConfig::default());
    })?;
    let preview = plan(&state).await?;
    assert_eq!(state.file_routing_generation(), generation);
    assert!(state.resolve_provider("b").is_none());
    let result = publish(&state, &preview).await?;
    assert_ne!(result.resulting_generation, generation);
    assert!(state.resolve_provider("b").is_some());
    assert!(state.resolve_provider("a").is_none());
    assert!(old.resolve_provider("a").is_some());
    assert!(old.resolve_provider("b").is_none());
    assert_eq!(old.pin_routes().file_routing_generation(), generation);
    let new = state.pin_routes();
    assert!(Arc::ptr_eq(&old.key_store, &new.key_store));
    assert!(Arc::ptr_eq(&old.key_validator, &new.key_validator));
    assert!(Arc::ptr_eq(&old.rate_limiter, &new.rate_limiter));
    assert!(Arc::ptr_eq(&old.conversations, &new.conversations));
    assert!(Arc::ptr_eq(&old.oauth_store, &new.oauth_store));
    assert!(Arc::ptr_eq(&old.cost_calculator, &new.cost_calculator));
    assert!(Arc::ptr_eq(&old.ws_sessions, &new.ws_sessions));
    assert!(Arc::ptr_eq(&old.upstream, &new.upstream));
    Ok(())
}

#[tokio::test]
#[expect(
    clippy::panic_in_result_fn,
    reason = "Behavioral assertions intentionally fail this regression"
)]
async fn stale_preview_changed_files_and_missing_cas_do_not_publish() -> Result<()> {
    let (state, source) = fixture()?;
    edit(&source, |config| {
        config
            .providers
            .insert("b".into(), ProviderConfig::default());
    })?;
    let original = state.file_routing_generation();
    let first = plan(&state).await?;
    let second = plan(&state).await?;
    assert_eq!(
        state.routing.reload(&state, apply(&first)).await.err(),
        Some(Failure::CandidateConflict)
    );
    edit(&source, |config| {
        config
            .providers
            .insert("c".into(), ProviderConfig::default());
    })?;
    assert_eq!(
        state.routing.reload(&state, apply(&second)).await.err(),
        Some(Failure::CandidateConflict)
    );
    let mut missing = preview();
    missing.dry_run = false;
    assert_eq!(
        state.routing.reload(&state, missing).await.err(),
        Some(Failure::MissingExpectation)
    );
    assert_eq!(state.file_routing_generation(), original);
    let fresh = plan(&state).await?;
    publish(&state, &fresh).await?;
    assert_eq!(
        state.routing.reload(&state, apply(&fresh)).await.err(),
        Some(Failure::GenerationConflict)
    );
    Ok(())
}

#[tokio::test]
#[expect(
    clippy::panic_in_result_fn,
    reason = "Behavioral assertions intentionally fail this regression"
)]
async fn invalid_and_restart_only_changes_keep_last_good_generation() -> Result<()> {
    let (state, source) = fixture()?;
    let baseline = config();
    let generation = state.file_routing_generation();
    let cases: Vec<(AppConfig, Failure)> = {
        let mut unknown = baseline.clone();
        unknown.providers.insert(
            "bad".into(),
            ProviderConfig {
                type_: "secret-sentinel-unknown".into(),
                ..ProviderConfig::default()
            },
        );
        let mut duplicate = baseline.clone();
        duplicate
            .providers
            .insert("A".into(), ProviderConfig::default());
        let mut listener = baseline.clone();
        listener.server.port += 1;
        let mut destination = baseline.clone();
        destination.providers.insert(
            "bad".into(),
            ProviderConfig {
                base_url: "http://secret-sentinel@localhost:1/".into(),
                ..ProviderConfig::default()
            },
        );
        let mut reference = baseline.clone();
        reference.providers.insert(
            "bad".into(),
            ProviderConfig {
                base_url: "keychain:secret-sentinel".into(),
                ..ProviderConfig::default()
            },
        );
        vec![
            (unknown, Failure::InvalidProvider),
            (duplicate, Failure::InvalidProvider),
            (listener, Failure::RestartRequired),
            (destination, Failure::InvalidDestination),
            (reference, Failure::InvalidDestination),
        ]
    };
    for (candidate, expected) in cases {
        edit(&source, |config| *config = candidate)?;
        assert_eq!(
            state.routing.reload(&state, preview()).await.err(),
            Some(expected)
        );
        assert_eq!(state.file_routing_generation(), generation);
        assert!(state.resolve_provider("a").is_some());
    }
    Ok(())
}

#[tokio::test]
#[expect(
    clippy::panic_in_result_fn,
    reason = "Behavioral assertions intentionally fail this regression"
)]
async fn stored_source_switching_is_explicitly_rejected() -> Result<()> {
    let (state, source) = fixture()?;
    let store = Arc::new(crate::provider_store::ProviderStore::in_memory().await?);
    store
        .create_provider(crate::provider_store::ProviderRequest {
            name: "b".into(),
            config: serde_json::json!({"type":"openai"}),
            enabled: true,
        })
        .await?;
    state
        .provider_store
        .set(store)
        .map_err(|_| anyhow!("store already set"))?;
    edit(&source, |config| {
        config
            .providers
            .insert("b".into(), ProviderConfig::default());
    })?;
    assert_eq!(
        state.routing.reload(&state, preview()).await.err(),
        Some(Failure::SourceConflict)
    );
    assert_eq!(
        state
            .resolve_provider("b")
            .map(|provider| provider.resolved_name),
        Some("b".into())
    );
    Ok(())
}

#[test]
#[expect(
    clippy::panic_in_result_fn,
    reason = "Behavioral assertions intentionally fail this regression"
)]
fn pricing_and_historical_async_route_migration_are_not_reloadable() -> Result<()> {
    let before = config();
    let mut after = before.clone();
    let model: crate::config::ModelShortlistEntry =
        serde_json::from_value(serde_json::json!({"id":"model","cost":{"input":1}}))?;
    after
        .providers
        .get_mut("a")
        .ok_or_else(|| anyhow!("missing fixture"))?
        .models
        .push(model);
    assert_eq!(validate(&before, &after), Err(Failure::PricingChange));
    let mut jobs = before.clone();
    jobs.providers.insert(
        "job".into(),
        ProviderConfig {
            type_: "fal".into(),
            ..ProviderConfig::default()
        },
    );
    assert_eq!(validate(&jobs, &before), Err(Failure::NativeJobChange));
    assert!(validate_input(&serde_json::json!({"aliases":{"hidden":"a"}})).is_err());
    assert!(validate_input(
        &serde_json::json!({"providers":{"a":{"type":"openai","unexpected":"secret-sentinel"}}})
    )
    .is_err());
    Ok(())
}

#[tokio::test]
#[expect(
    clippy::panic_in_result_fn,
    reason = "Behavioral assertions intentionally fail this regression"
)]
async fn file_parse_failure_is_private_and_does_not_replace_the_head() -> Result<()> {
    let file = tempfile::NamedTempFile::new()?;
    let path = file.path().to_path_buf();
    let state = AppState::new(config());
    state
        .routing
        .configure(move || {
            let content =
                std::fs::read_to_string(&path).map_err(|_| Failure::InvalidConfiguration)?;
            toml::from_str(&content).map_err(|_| Failure::InvalidConfiguration)
        })
        .map_err(|e| anyhow!(e.code()))?;
    std::fs::write(
        file.path(),
        "[providers.a]\napi_key = 'secret-sentinel'\n[providers.a]\n",
    )?;
    let original = state.file_routing_generation();
    assert_eq!(
        state.routing.reload(&state, preview()).await.err(),
        Some(Failure::InvalidConfiguration)
    );
    assert_eq!(state.file_routing_generation(), original);
    assert!(state.resolve_provider("a").is_some());
    Ok(())
}

#[tokio::test]
#[expect(
    clippy::panic_in_result_fn,
    reason = "Behavioral assertions intentionally fail this regression"
)]
async fn concurrent_apply_has_one_winner_and_never_replays_a_plan() -> Result<()> {
    let (state, source) = fixture()?;
    edit(&source, |config| {
        config
            .providers
            .insert("b".into(), ProviderConfig::default());
    })?;
    let plan = plan(&state).await?;
    let request = apply(&plan);
    let (left, right) = tokio::join!(
        state.routing.reload(&state, request.clone()),
        state.routing.reload(&state, request)
    );
    let results: [Result<ReloadResult, Failure>; 2] = (left, right).into();
    assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
    assert_eq!(
        results
            .iter()
            .filter(|result| matches!(result, Err(Failure::GenerationConflict)))
            .count(),
        1
    );
    assert!(state.resolve_provider("b").is_some());
    assert_eq!(state.file_routing_generation(), plan.candidate_generation);
    Ok(())
}

#[tokio::test]
#[expect(
    clippy::panic_in_result_fn,
    reason = "Behavioral assertions intentionally fail this regression"
)]
async fn additions_never_switch_the_implicit_catalog_default() -> Result<()> {
    let (state, source) = fixture()?;
    assert_eq!(state.default_provider_hint(), Some("a".into()));
    edit(&source, |config| {
        config
            .providers
            .insert("0-new".into(), ProviderConfig::default());
    })?;
    let candidate = plan(&state).await?;
    publish(&state, &candidate).await?;
    assert_eq!(state.default_provider_hint(), Some("a".into()));
    edit(&source, |config| {
        config.providers.remove("a");
    })?;
    let candidate = plan(&state).await?;
    publish(&state, &candidate).await?;
    assert_eq!(
        state.default_provider_hint(),
        None,
        "Removal must not pick another route implicitly"
    );
    edit(&source, |config| {
        config
            .providers
            .insert("default".into(), ProviderConfig::default());
    })?;
    let candidate = plan(&state).await?;
    publish(&state, &candidate).await?;
    assert_eq!(state.default_provider_hint(), Some("default".into()));
    Ok(())
}

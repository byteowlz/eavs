use super::*;
use anyhow::{anyhow, Result};

const A: &str = "[keys]\nenabled = false\nmaster_key = 'synthetic-admin'\n[server]\nhost = 'localhost'\nport = 3033\n[providers.a]\ntype = 'openai'\napi_key = 'keychain:do-not-read'\nbase_url = 'http://127.0.0.1:1/a'\n";
const B: &str = "\n[providers.b]\ntype = 'openai-compatible'\nbase_url = 'http://127.0.0.1:1/b'\napi_key = 'env:DO_NOT_RESOLVE'\n[providers.b.compat]\nsupports_developer_role = false\n[[providers.b.models]]\nid = 'local-model'\n[providers.b.models.compat]\nsupportsDeveloperRole = false\n";
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

#[tokio::test]
#[expect(
    clippy::panic_in_result_fn,
    reason = "Behavioral assertions intentionally fail this regression"
)]
async fn atomic_file_replacement_uses_strict_parser_and_preserves_cli_precedence() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("gateway.toml");
    std::fs::write(&path, A)?;
    let mut initial = AppConfig::load_isolated_reload(&path)?;
    initial.apply_server_overrides(Some("127.0.0.1"), Some(4040));
    let state = AppState::new(initial);
    let source = path.clone();
    state
        .routing
        .configure(move || {
            let mut config = AppConfig::load_isolated_reload(&source)
                .map_err(|_| Failure::InvalidConfiguration)?;
            config.apply_server_overrides(Some("127.0.0.1"), Some(4040));
            Ok(config)
        })
        .map_err(|e| anyhow!(e.code()))?;
    let replacement = directory.path().join("replacement.toml");
    // CLI-selected host/port still win even when lower-priority file values change.
    std::fs::write(
        &replacement,
        format!("{}{B}", A.replace("port = 3033", "port = 5050")),
    )?;
    std::fs::rename(&replacement, &path)?;
    let plan = state
        .routing
        .reload(&state, preview())
        .await
        .map_err(|e| anyhow!(e.code()))?;
    let result = state
        .routing
        .reload(&state, apply(&plan))
        .await
        .map_err(|e| anyhow!(e.code()))?;
    assert_eq!(result.status, "applied");
    let current = state.pin_routes();
    assert_eq!(current.config.server.port, 4040);
    assert_eq!(current.config.server.host, "127.0.0.1");
    let b = current
        .resolve_provider("b")
        .ok_or_else(|| anyhow!("new route unavailable"))?;
    let model = b
        .config
        .models
        .first()
        .ok_or_else(|| anyhow!("model missing"))?;
    assert_eq!(model.id, "local-model");
    assert_eq!(b.config.compat.supports_developer_role, Some(false));
    assert_eq!(
        model.compat.get("supportsDeveloperRole"),
        Some(&serde_json::json!(false))
    );
    assert_eq!(b.config.api_key, "env:DO_NOT_RESOLVE");
    let generation = current.file_routing_generation();
    std::fs::write(
        &path,
        format!("{A}{B}\n[providers.b]\napi_key = 'secret-sentinel'"),
    )?;
    assert_eq!(
        state.routing.reload(&state, preview()).await.err(),
        Some(Failure::InvalidConfiguration)
    );
    assert_eq!(state.file_routing_generation(), generation);
    assert!(state.resolve_provider("b").is_some());
    Ok(())
}

#[tokio::test]
#[expect(
    clippy::panic_in_result_fn,
    reason = "Behavioral assertions intentionally fail this regression"
)]
async fn unknown_nested_static_changes_and_unsupported_provider_fields_are_not_silently_applied(
) -> Result<()> {
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("gateway.toml");
    std::fs::write(&path, A)?;
    let state = AppState::new(AppConfig::load_isolated_reload(&path)?);
    let source = path.clone();
    state
        .routing
        .configure(move || {
            AppConfig::load_isolated_reload(&source).map_err(|_| Failure::InvalidConfiguration)
        })
        .map_err(|e| anyhow!(e.code()))?;
    std::fs::write(
        &path,
        A.replace(
            "port = 3033",
            "port = 3033\nunsupported_security_setting = 'secret-sentinel'",
        ),
    )?;
    assert_eq!(
        state.routing.reload(&state, preview()).await.err(),
        Some(Failure::RestartRequired)
    );
    std::fs::write(&path, format!("{A}\nenabled = false\n"))?;
    assert_eq!(
        state.routing.reload(&state, preview()).await.err(),
        Some(Failure::InvalidConfiguration)
    );
    assert!(state.resolve_provider("a").is_some());
    Ok(())
}

#[test]
#[expect(
    clippy::panic_in_result_fn,
    reason = "Behavioral assertions intentionally fail this regression"
)]
fn empty_file_snapshot_never_invents_a_default_route() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("gateway.toml");
    std::fs::write(&path, "[providers]\n")?;
    let config = AppConfig::load_isolated_reload(&path)?;
    assert!(config.providers.is_empty());
    assert!(config.resolve_provider("default").is_none());
    Ok(())
}

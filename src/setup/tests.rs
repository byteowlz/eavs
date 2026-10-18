use super::*;

#[test]
#[expect(
    clippy::panic_in_result_fn,
    reason = "Assertions prove cooperative cross-process locking and single-winner creation; fixture/process errors propagate"
)]
fn concurrent_setup_processes_cannot_duplicate_a_provider() -> Result<(), Box<dyn std::error::Error>>
{
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("config.toml");
    let executable = std::env::current_exe()?;
    let spawn = || {
        let mut command = std::process::Command::new(&executable);
        command
            .env_clear()
            .env("EAVS_FIXTURE_CONFIG_WRITE", &path)
            .args(["--exact", "setup::tests::config_write_child", "--nocapture"])
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());
        if let Some(root) = std::env::var_os("SystemRoot") {
            command.env("SystemRoot", root);
        }
        command.spawn()
    };
    let first = spawn()?;
    let second = spawn()?;
    let outputs = [first.wait_with_output()?, second.wait_with_output()?];
    assert!(outputs.iter().all(|output| output.status.success()));
    assert_eq!(
        outputs
            .iter()
            .filter(|output| String::from_utf8_lossy(&output.stdout).contains("WRITE_SUCCESS"))
            .count(),
        1
    );
    assert_eq!(
        outputs
            .iter()
            .filter(|output| String::from_utf8_lossy(&output.stdout).contains("WRITE_CONFLICT"))
            .count(),
        1
    );
    let parsed: toml::Value = std::fs::read_to_string(path)?.parse()?;
    assert_eq!(
        parsed["providers"]
            .as_table()
            .ok_or("missing providers")?
            .len(),
        1
    );
    Ok(())
}

#[test]
fn config_write_child() -> Result<(), Box<dyn std::error::Error>> {
    let Some(path) = std::env::var_os("EAVS_FIXTURE_CONFIG_WRITE") else {
        return Ok(());
    };
    let config = SetupProviderConfig {
        type_: "anthropic".into(),
        api_key: "synthetic-child".into(),
        ..Default::default()
    };
    match save_provider_to_config(&PathBuf::from(path), "anthropic", &config) {
        Ok(()) => println!("WRITE_SUCCESS"),
        Err(error) if error.to_string().contains("already exists") => println!("WRITE_CONFLICT"),
        Err(error) => return Err(error.into()),
    }
    Ok(())
}

#[test]
fn explicit_test_model_is_not_replaced_by_a_default() {
    let config = ProviderConfig {
        type_: "anthropic".into(),
        test_model: "operator-chosen-model".into(),
        ..Default::default()
    };
    assert_eq!(resolve_test_model(&config), "operator-chosen-model");
}

#[test]
fn modern_openai_probe_uses_responses_without_legacy_max_tokens() {
    let config = ProviderConfig::default();
    let (url, body, _) = build_test_request(
        ProviderType::OpenAI,
        "https://example.test/v1",
        "synthetic",
        "gpt-5.4-mini",
        &config,
        "test",
    );
    assert_eq!(url, "https://example.test/v1/responses");
    assert_eq!(body["model"], "gpt-5.4-mini");
    assert_eq!(body["max_output_tokens"], 64);
    assert!(body.get("max_tokens").is_none());
}

#[test]
#[expect(
    clippy::panic_in_result_fn,
    reason = "Assertions prove import and default writes remain idempotent; fixture failures propagate"
)]
fn imports_skip_semantic_duplicates_and_preserve_escaped_values(
) -> Result<(), Box<dyn std::error::Error>> {
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("config.toml");
    let source = directory.path().join("import.toml");
    std::fs::write(
        &path,
        "# keep\n[providers.'anthropic']\ntype='anthropic'\napi_key='synthetic-original'\n",
    )?;
    std::fs::write(&source, "[providers.anthropic]\ntype='anthropic'\napi_key='synthetic-replacement'\n[providers.\"openai.prod\"]\ntype='openai'\napi_key='synthetic-\"key'\n")?;
    let source_name = source.to_str().ok_or("invalid fixture path")?;
    import_providers_from_toml(&path, source_name)?;
    let first = std::fs::read_to_string(&path)?;
    let parsed: toml::Value = first.parse()?;
    assert!(first.starts_with("# keep\n"));
    assert_eq!(
        parsed["providers"]["anthropic"]["api_key"].as_str(),
        Some("synthetic-original")
    );
    assert_eq!(
        parsed["providers"]["openai.prod"]["api_key"].as_str(),
        Some("synthetic-\"key")
    );
    assert_eq!(
        parsed["providers"]["default"]["api_key"].as_str(),
        Some("synthetic-\"key")
    );
    import_providers_from_toml(&path, source_name)?;
    set_default_provider(&path, "anthropic")?;
    assert_eq!(std::fs::read_to_string(&path)?, first);
    Ok(())
}

#[cfg(unix)]
#[test]
#[expect(
    clippy::panic_in_result_fn,
    reason = "Assertions prove private creation, permission preservation and symlink target updates; fixture failures propagate"
)]
fn atomic_config_writes_preserve_permissions_and_symlinks() -> Result<(), Box<dyn std::error::Error>>
{
    use std::os::unix::fs::{symlink, PermissionsExt as _};
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("config.toml");
    let alias = directory.path().join("linked.toml");
    let config = SetupProviderConfig {
        type_: "anthropic".into(),
        api_key: "synthetic".into(),
        ..Default::default()
    };
    save_provider_to_config(&path, "first", &config)?;
    assert_eq!(
        std::fs::metadata(&path)?.permissions().mode() & 0o777,
        0o600
    );
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o640))?;
    symlink(&path, &alias)?;
    save_provider_to_config(&alias, "second", &config)?;
    assert!(std::fs::symlink_metadata(&alias)?.file_type().is_symlink());
    assert_eq!(
        std::fs::metadata(&path)?.permissions().mode() & 0o777,
        0o640
    );
    let parsed: toml::Value = std::fs::read_to_string(path)?.parse()?;
    assert_eq!(
        parsed["providers"]
            .as_table()
            .ok_or("missing providers")?
            .len(),
        2
    );
    Ok(())
}

#[test]
fn recent_test_defaults_use_real_api_ids() {
    assert_eq!(
        suggest_test_model_default(ProviderType::Anthropic),
        "claude-haiku-5-5"
    );
    assert_eq!(
        suggest_test_model_default(ProviderType::OpenAI),
        "gpt-5.4-mini"
    );
    assert_eq!(
        suggest_test_model_default(ProviderType::Google),
        "gemini-3.8-flash"
    );
    assert_eq!(
        suggest_test_model_default(ProviderType::Cerebras),
        "gpt-oss-120b"
    );
}

#[test]
#[expect(
    clippy::panic_in_result_fn,
    reason = "Assertions prove semantic duplicate rejection and unchanged bytes; fixture failures propagate"
)]
fn duplicate_provider_names_never_change_config() -> Result<(), Box<dyn std::error::Error>> {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("config.toml");
    let config = SetupProviderConfig {
        type_: "anthropic".into(),
        api_key: "synthetic-new".into(),
        ..Default::default()
    };
    for header in [
        "[providers.anthropic]",
        "[providers.\"anthropic\"]",
        "[providers]\nanthropic = { type = \"anthropic\", api_key = \"synthetic-old\" }",
    ] {
        let existing = if header.starts_with("[providers]\n") {
            header.to_string()
        } else {
            format!("{header}\ntype = \"anthropic\"\napi_key = \"synthetic-old\"\n")
        };
        std::fs::write(&path, &existing)?;
        assert!(save_provider_to_config(&path, "anthropic", &config).is_err());
        assert_eq!(std::fs::read_to_string(&path)?, existing);
    }
    Ok(())
}

#[test]
#[expect(
    clippy::panic_in_result_fn,
    reason = "Assertions prove invalid existing TOML is never overwritten; fixture failures propagate"
)]
fn malformed_config_is_not_modified() -> Result<(), Box<dyn std::error::Error>> {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("config.toml");
    let original =
        "[providers.anthropic]\ntype = 'anthropic'\n[providers.anthropic]\ntype = 'anthropic'\n";
    std::fs::write(&path, original)?;
    let config = SetupProviderConfig {
        type_: "openai".into(),
        ..Default::default()
    };
    assert!(save_provider_to_config(&path, "openai", &config).is_err());
    assert_eq!(std::fs::read_to_string(&path)?, original);
    Ok(())
}

#[test]
#[expect(
    clippy::panic_in_result_fn,
    reason = "Assertions prove TOML escaping and comment preservation; fixture failures propagate"
)]
fn new_provider_is_escaped_and_preserves_other_configuration(
) -> Result<(), Box<dyn std::error::Error>> {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("config.toml");
    let original = "# preserve this comment\n[server]\nport = 9999\n";
    std::fs::write(&path, original)?;
    let config = SetupProviderConfig {
        type_: "anthropic".into(),
        api_key: "synthetic-\"\\\nkey".into(),
        compat_supports_store: Some(false),
        ..Default::default()
    };
    save_provider_to_config(&path, "anthropic.prod", &config)?;
    let output = std::fs::read_to_string(&path)?;
    let parsed: toml::Value = output.parse()?;
    assert!(output.starts_with(original));
    assert_eq!(
        parsed["providers"]["anthropic.prod"]["api_key"].as_str(),
        Some(config.api_key.as_str())
    );
    assert_eq!(
        parsed["providers"]["anthropic.prod"]["compat"]["supports_store"].as_bool(),
        Some(false)
    );
    Ok(())
}

#[test]
#[expect(
    clippy::panic_in_result_fn,
    reason = "Assertions prove scaffold creation never overwrites a valid providers-only config; fixtures propagate errors"
)]
fn scaffolding_preserves_existing_config_without_server_table(
) -> Result<(), Box<dyn std::error::Error>> {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("config.toml");
    let original = "[providers.anthropic]\ntype = 'anthropic'\napi_key = 'synthetic-existing'\n";
    std::fs::write(&path, original)?;
    ensure_base_config(&path)?;
    assert_eq!(std::fs::read_to_string(&path)?, original);
    Ok(())
}

//! Point Claude Code on this machine at a gateway's Claude subscription
//! provider (ADR-0023), through its settings file's `env` block.
//!
//! Claude Code reads `env` from `settings.json` before authenticating, and a
//! `CLAUDE_CODE_OAUTH_TOKEN` there takes precedence over a local login
//! (verified against Claude Code 2.1.296). Three variables are managed:
//!
//! - `ANTHROPIC_BASE_URL`: `<gateway>/<provider>`;
//! - `CLAUDE_CODE_OAUTH_TOKEN`: the connection's scoped eavs key (omitted with
//!   `--no-store-key`, then exported from the shell instead);
//! - `CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC`: without it Claude Code sends
//!   the same bearer straight to `api.anthropic.com` (bootstrap, flags,
//!   telemetry), i.e. the eavs key would leave for Anthropic.
//!
//! `ANTHROPIC_API_KEY` / `ANTHROPIC_AUTH_TOKEN` in the same block would take
//! precedence and are removed. Everything else in the file is preserved. The
//! published file is private (0600) because it then holds the eavs key.

use std::path::{Path, PathBuf};

use anyhow::{bail, Context as _, Result};
use serde_json::{json, Map, Value};

use super::{revision, storage, Connection};

const BASE_URL: &str = "ANTHROPIC_BASE_URL";
const TOKEN: &str = "CLAUDE_CODE_OAUTH_TOKEN";
const QUIET: &str = "CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC";
/// These would win over the OAuth token and send Anthropic API-key auth.
const OVERRIDES: [&str; 2] = ["ANTHROPIC_API_KEY", "ANTHROPIC_AUTH_TOKEN"];

/// What to write and where.
pub(super) struct Request<'a> {
    pub provider: &'a str,
    pub target: &'a Path,
    /// Write the key into the file; otherwise the shell must export it.
    pub store_key: bool,
    /// Remove this connection's settings instead of writing them.
    pub remove: bool,
    pub registry_path: &'a Path,
    pub registry_before: &'a [u8],
    pub expected: Option<&'a str>,
    pub candidate: Option<&'a str>,
    pub apply: bool,
}

/// `$CLAUDE_CONFIG_DIR/settings.json`, else `~/.claude/settings.json`.
pub(super) fn default_target() -> Result<PathBuf> {
    if let Some(dir) = std::env::var_os("CLAUDE_CONFIG_DIR").map(PathBuf::from) {
        if dir.is_absolute() {
            return Ok(dir.join("settings.json"));
        }
    }
    let home = std::env::var_os("HOME").context("HOME is not set; pass --output")?;
    Ok(PathBuf::from(home).join(".claude").join("settings.json"))
}

/// The catalog provider must be a native Anthropic Messages route this key may use.
fn check_provider(catalog: &Value, provider: &str) -> Result<()> {
    let providers = catalog
        .get("providers")
        .and_then(Value::as_array)
        .context("catalog invalid")?;
    let anthropic: Vec<&str> = providers
        .iter()
        .filter(|p| p.get("api").and_then(Value::as_str) == Some("anthropic-messages"))
        .filter_map(|p| p.get("id").and_then(Value::as_str))
        .collect();
    if anthropic.contains(&provider) {
        return Ok(());
    }
    if anthropic.is_empty() {
        bail!("this key may use no Anthropic Messages provider on the gateway; scope a key to the subscription provider");
    }
    bail!(
        "provider {provider:?} is not an Anthropic Messages provider for this key; available: {}",
        anthropic.join(", ")
    )
}

fn settings(bytes: &[u8]) -> Result<Map<String, Value>> {
    if bytes.is_empty() {
        return Ok(Map::new());
    }
    match serde_json::from_slice(bytes) {
        Ok(Value::Object(map)) => Ok(map),
        _ => bail!("Claude Code settings are not a JSON object; repair before retrying"),
    }
}

/// Which variables an edit set and removed, and what the operator must know.
struct Edit {
    set: Vec<&'static str>,
    removed: Vec<&'static str>,
    warnings: Vec<String>,
}

fn edit_env(
    env: &mut Map<String, Value>,
    connection: &Connection,
    catalog: &Value,
    request: &Request<'_>,
    base_url: &str,
) -> Result<Edit> {
    let mut set = Vec::new();
    let mut removed = Vec::new();
    let mut warnings = Vec::new();

    if request.remove {
        // Only undo what points at this connection.
        if env.get(BASE_URL).and_then(Value::as_str) == Some(base_url) {
            for name in [BASE_URL, TOKEN, QUIET] {
                if env.remove(name).is_some() {
                    removed.push(name);
                }
            }
        } else {
            warnings.push(format!(
                "{BASE_URL} does not point at {base_url}; nothing removed"
            ));
        }
    } else {
        check_provider(catalog, request.provider)?;
        env.insert(BASE_URL.into(), Value::String(base_url.to_owned()));
        env.insert(QUIET.into(), Value::String("1".into()));
        set.extend([BASE_URL, QUIET]);
        if request.store_key {
            let key = std::env::var(&connection.key_env)
                .map_err(|_| anyhow::anyhow!("device credential unavailable"))?;
            if !crate::keys::is_virtual_key(&key) {
                bail!("scoped virtual key required");
            }
            env.insert(TOKEN.into(), Value::String(key));
            set.push(TOKEN);
        } else {
            if env.remove(TOKEN).is_some() {
                removed.push(TOKEN);
            }
            warnings.push(format!(
                "export {TOKEN}=\"${}\" in the shell that starts Claude Code",
                connection.key_env
            ));
        }
        for name in OVERRIDES {
            if env.remove(name).is_some() {
                removed.push(name);
            }
            if std::env::var_os(name).is_some() {
                warnings.push(format!(
                    "{name} is set in this shell; unset it, it would take precedence over the subscription"
                ));
            }
        }
    }
    Ok(Edit {
        set,
        removed,
        warnings,
    })
}

pub(super) fn configure(
    connection: &Connection,
    catalog: &Value,
    request: &Request<'_>,
) -> Result<Value> {
    if !request.target.is_absolute()
        || request
            .target
            .file_name()
            .is_none_or(|name| name != "settings.json")
    {
        bail!("Claude Code target must be an absolute settings.json path");
    }
    if connection.listener_key_env.is_some() {
        bail!(
            "Claude Code cannot send a listener token; connect through the gateway's inference ingress (no --listener-key-env)"
        );
    }
    let base_url = format!(
        "{}/{}",
        connection.url.trim_end_matches('/'),
        request.provider
    );
    let old = storage::read(request.target)?;
    let mut root = settings(&old)?;
    let mut env = match root.remove("env") {
        Some(Value::Object(env)) => env,
        None => Map::new(),
        Some(_) => bail!("Claude Code settings `env` is not an object; repair before retrying"),
    };
    let Edit {
        set: changed,
        removed,
        warnings,
    } = edit_env(&mut env, connection, catalog, request, &base_url)?;
    if !env.is_empty() {
        root.insert("env".into(), Value::Object(env));
    }
    let candidate = serde_json::to_vec_pretty(&root)?;
    if request.apply {
        if request.candidate != Some(revision(&candidate).as_str()) {
            bail!(
                "candidate changed or not approved; use --candidate-revision from a fresh preview"
            );
        }
        storage::with_snapshot(request.registry_path, request.registry_before, || {
            storage::apply(request.target, &old, &candidate, request.expected)
        })?;
        restrict(request.target)?;
    }
    // Never echo the document: it may hold this and other credentials.
    Ok(json!({
        "schema_version": 1,
        "operation": "claude_code_config",
        "applied": request.apply,
        "target": request.target,
        "base_url": base_url,
        "revision": revision(&old),
        "candidate_revision": revision(&candidate),
        "set_env": changed,
        "removed_env": removed,
        "key_stored": request.store_key && !request.remove,
        "warnings": warnings,
    }))
}

/// The settings file holds the eavs key: owner-only.
fn restrict(path: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    }
    let _ = path;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn catalog() -> Value {
        json!({"providers": [
            {"id": "claude", "api": "anthropic-messages"},
            {"id": "fhgenie", "api": "openai-completions"}
        ]})
    }

    fn connection(key_env: &str) -> Connection {
        Connection {
            name: "studio".into(),
            url: "https://eavs.example.ts.net".into(),
            key_env: key_env.into(),
            listener_key_env: None,
            allow_loopback_http: false,
        }
    }

    struct Fixture {
        _dir: tempfile::TempDir,
        target: PathBuf,
        registry: PathBuf,
    }

    fn fixture(existing: Option<&str>) -> Result<Fixture> {
        let dir = tempfile::tempdir()?;
        let target = dir.path().join("settings.json");
        if let Some(text) = existing {
            std::fs::write(&target, text)?;
        }
        let registry = dir.path().join("device.json");
        std::fs::write(&registry, b"{\"connections\":[]}")?;
        Ok(Fixture {
            _dir: dir,
            target,
            registry,
        })
    }

    fn request<'a>(f: &'a Fixture, before: &'a [u8], remove: bool, store: bool) -> Request<'a> {
        Request {
            provider: "claude",
            target: &f.target,
            store_key: store,
            remove,
            registry_path: &f.registry,
            registry_before: before,
            expected: None,
            candidate: None,
            apply: false,
        }
    }

    /// Preview, then apply with the preview's revisions.
    fn preview_and_apply(f: &Fixture, key_env: &str, remove: bool) -> Result<Value> {
        let before = std::fs::read(&f.registry)?;
        let preview = configure(
            &connection(key_env),
            &catalog(),
            &request(f, &before, remove, true),
        )?;
        let expected = preview["revision"].as_str().context("revision")?.to_owned();
        let candidate = preview["candidate_revision"]
            .as_str()
            .context("candidate")?
            .to_owned();
        let mut apply = request(f, &before, remove, true);
        apply.expected = Some(&expected);
        apply.candidate = Some(&candidate);
        apply.apply = true;
        configure(&connection(key_env), &catalog(), &apply)?;
        Ok(preview)
    }

    fn written(f: &Fixture) -> Result<Value> {
        Ok(serde_json::from_slice(&std::fs::read(&f.target)?)?)
    }

    #[test]
    #[expect(
        clippy::panic_in_result_fn,
        reason = "assertions are intentional in tests"
    )]
    fn preview_then_apply_merges_env_and_keeps_other_settings() -> Result<()> {
        let key_env = "EAVS_TEST_CLAUDE_CODE_KEY_A";
        set_env(key_env, "eavs-testkey-0123456789abcdef0123456789abcdef");
        let f = fixture(Some(
            r#"{"model":"opus","env":{"ANTHROPIC_API_KEY":"sk-old","KEEP":"1"},"permissions":{"allow":["Bash"]}}"#,
        ))?;
        let preview = preview_and_apply(&f, key_env, false)?;
        assert_eq!(preview["base_url"], "https://eavs.example.ts.net/claude");
        assert!(
            !preview.to_string().contains("eavs-testkey"),
            "key is never echoed"
        );

        let settings = written(&f)?;
        assert_eq!(settings["model"], "opus");
        assert_eq!(settings["permissions"]["allow"][0], "Bash");
        assert_eq!(settings["env"]["KEEP"], "1");
        assert_eq!(
            settings["env"][BASE_URL],
            "https://eavs.example.ts.net/claude"
        );
        assert_eq!(settings["env"][QUIET], "1");
        assert_eq!(
            settings["env"][TOKEN],
            "eavs-testkey-0123456789abcdef0123456789abcdef"
        );
        assert!(settings["env"].get("ANTHROPIC_API_KEY").is_none());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let mode = std::fs::metadata(&f.target)?.permissions().mode() & 0o777;
            assert_eq!(mode, 0o600);
        }

        // Removal undoes exactly what points at this connection.
        preview_and_apply(&f, key_env, true)?;
        let settings = written(&f)?;
        assert!(settings["env"].get(TOKEN).is_none());
        assert!(settings["env"].get(BASE_URL).is_none());
        assert_eq!(settings["env"]["KEEP"], "1");
        Ok(())
    }

    #[test]
    #[expect(
        clippy::panic_in_result_fn,
        reason = "assertions are intentional in tests"
    )]
    fn refuses_non_anthropic_providers_and_listener_connections() -> Result<()> {
        let f = fixture(None)?;
        let before = std::fs::read(&f.registry)?;
        let mut req = request(&f, &before, false, false);
        req.provider = "fhgenie";
        let error = configure(&connection("UNUSED"), &catalog(), &req)
            .err()
            .context("non-Anthropic provider accepted")?
            .to_string();
        assert!(error.contains("available: claude"), "{error}");

        let mut listener = connection("UNUSED");
        listener.listener_key_env = Some("EAVS_LISTENER".into());
        assert!(configure(&listener, &catalog(), &request(&f, &before, false, false)).is_err());
        Ok(())
    }

    #[test]
    #[expect(
        clippy::panic_in_result_fn,
        reason = "assertions are intentional in tests"
    )]
    fn without_storing_the_key_the_shell_must_export_it() -> Result<()> {
        let f = fixture(None)?;
        let before = std::fs::read(&f.registry)?;
        let preview = configure(
            &connection("EAVS_DEVICE_KEY"),
            &catalog(),
            &request(&f, &before, false, false),
        )?;
        assert_eq!(preview["key_stored"], false);
        assert!(preview["warnings"][0]
            .as_str()
            .is_some_and(|w| w.contains("export CLAUDE_CODE_OAUTH_TOKEN=\"$EAVS_DEVICE_KEY\"")));
        Ok(())
    }

    /// Environment writes are process-global; each test uses its own name.
    fn set_env(name: &str, value: &str) {
        // SAFETY: no other test reads or writes this variable name.
        #[expect(unsafe_code, reason = "test-only environment fixture")]
        unsafe {
            std::env::set_var(name, value);
        }
    }
}

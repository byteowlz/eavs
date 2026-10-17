use super::*;
use anyhow::ensure;
struct Environment(String);
impl Drop for Environment {
    fn drop(&mut self) {
        std::env::remove_var(&self.0);
    }
}
struct Server(tokio::task::JoinHandle<std::io::Result<()>>);
impl Drop for Server {
    fn drop(&mut self) {
        self.0.abort();
    }
}
fn connection() -> Connection {
    Connection {
        name: "central".into(),
        url: "https://eavs.example.test".into(),
        key_env: "EAVS_KEY".into(),
        listener_key_env: Some("EAVS_LISTENER".into()),
        allow_loopback_http: false,
    }
}
pub(in crate::device) fn fixture() -> Value {
    json!({"schema_version":1,"scope":"key-filtered-chat-candidates","providers":[{"id":"alpha","api":"anthropic-messages","base_path":"/alpha/v1","request_path":"/alpha/v1/messages","operation":"chat","metadata_source":"shortlist","models":[{"id":"model-a","name":"Model A","reasoning":true,"input":["text","image"],"context_window":200_000,"max_tokens":4096,"compat":{},"pricing":{"status":"unverified_estimate","unit":"USD_per_million_tokens","rates":{"input":3.0,"output":15.0,"cache_read":0.3,"cache_write":3.75}}}]}]})
}
fn receipt<'a>(output: &'a Path, registry: &'a Path, before: &'a [u8]) -> Publication<'a> {
    Publication {
        output,
        registry_path: registry,
        registry_before: before,
        expected: None,
        candidate: None,
        apply: false,
    }
}
#[test]
fn preview_apply_preserves_unrelated_secrets_without_echoing_or_executing() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("models.json");
    let registry = dir.path().join("device.json");
    std::fs::write(&registry, b"registry")?;
    let c = connection();
    let live = fixture();
    let original = json!({"custom":{"keep":true},"providers":{"native":{"apiKey":"private-unrelated-secret","models":[]},"eavs-other":{"models":[]}}});
    std::fs::write(&path, serde_json::to_vec(&original)?)?;
    let mut request = receipt(&path, &registry, b"registry");
    let preview = configure(&c, &live, &request)?;
    ensure!(serde_json::from_slice::<Value>(&std::fs::read(&path)?)? == original);
    ensure!(!preview.to_string().contains("private-unrelated-secret"));
    request.apply = true;
    request.expected = preview["revision"].as_str();
    request.candidate = preview["candidate_revision"].as_str();
    configure(&c, &live, &request)?;
    let output: Value = serde_json::from_slice(&std::fs::read(&path)?)?;
    ensure!(
        output["custom"] == original["custom"]
            && output["providers"]["native"] == original["providers"]["native"]
            && output["providers"]["eavs-other"] == original["providers"]["eavs-other"]
    );
    let name = preview["managed_providers"][0]
        .as_str()
        .context("managed name")?;
    ensure!(output["providers"][name]["headers"]["Authorization"] == "Bearer ${EAVS_KEY}");
    ensure!(output["providers"][name]["headers"]["X-Eavs-Token"] == "${EAVS_LISTENER}");
    ensure!(output["providers"][name]["apiKey"] == "${EAVS_KEY}");
    if let Some(directory) = std::env::var_os("EAVS_PI_CONFIG_FIXTURES") {
        std::fs::create_dir_all(&directory)?;
        std::fs::write(
            std::path::PathBuf::from(directory).join("models.json"),
            serde_json::to_vec_pretty(&output)?,
        )?;
    }
    ensure!(
        std::fs::read(path.with_extension("json.last-good"))? == serde_json::to_vec(&original)?
    );
    Ok(())
}
#[test]
fn malformed_targets_and_unpriced_models_never_replace_existing_data() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("models.json");
    let registry = dir.path().join("device.json");
    let c = connection();
    let live = fixture();
    for bytes in [
        b"{private-secret".as_slice(),
        b"null",
        b"[]",
        b"{\"providers\":null}",
        b"{\"providers\":[]}",
        b"{\"providers\":{\"custom\":null}}",
        b"",
    ] {
        std::fs::write(&path, bytes)?;
        ensure!(configure(&c, &live, &receipt(&path, &registry, b"")).is_err());
        ensure!(std::fs::read(&path)? == bytes);
    }
    std::fs::write(&path, b"{}")?;
    let mut unpriced = live.clone();
    unpriced["providers"][0]["models"][0]["pricing"] = Value::Null;
    ensure!(configure(&c, &unpriced, &receipt(&path, &registry, b"")).is_err());
    ensure!(std::fs::read(&path)? == b"{}");
    let mut hostile = live.clone();
    hostile["providers"][0]["base_path"] = "/admin/keys".into();
    ensure!(configure(&c, &hostile, &receipt(&path, &registry, b"")).is_err());
    let wrong = dir.path().join("auth.json");
    ensure!(configure(&c, &live, &receipt(&wrong, &registry, b"")).is_err());
    Ok(())
}
#[test]
fn config_cas_and_registry_changes_refuse_and_empty_scope_prunes_only_owned_entries() -> Result<()>
{
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("models.json");
    let registry = dir.path().join("device.json");
    std::fs::write(&registry, b"registry")?;
    let c = connection();
    let live = fixture();
    let mut request = receipt(&path, &registry, b"registry");
    let preview = configure(&c, &live, &request)?;
    request.apply = true;
    request.expected = preview["revision"].as_str();
    request.candidate = Some("wrong");
    ensure!(configure(&c, &live, &request).is_err() && !path.exists());
    request.candidate = preview["candidate_revision"].as_str();
    std::fs::write(&registry, b"changed")?;
    ensure!(configure(&c, &live, &request).is_err() && !path.exists());
    std::fs::write(&registry, b"registry")?;
    configure(&c, &live, &request)?;
    let old = std::fs::read(&path)?;
    ensure!(configure(&c, &live, &request).is_err() && std::fs::read(&path)? == old);
    let empty = json!({"schema_version":1,"scope":"key-filtered-chat-candidates","providers":[]});
    request.apply = false;
    let removal = configure(&c, &empty, &request)?;
    request.apply = true;
    request.expected = removal["revision"].as_str();
    request.candidate = removal["candidate_revision"].as_str();
    configure(&c, &empty, &request)?;
    let result: Value = serde_json::from_slice(&std::fs::read(path)?)?;
    ensure!(result["providers"] == json!({}));
    Ok(())
}
#[test]
fn codex_uses_gateway_native_responses_without_client_oauth_or_untrusted_metadata() -> Result<()> {
    let c = connection();
    let mut live = fixture();
    let p = &mut live["providers"][0];
    p["api"] = "openai-codex-responses".into();
    p["request_path"] = "/alpha/v1/responses".into();
    p["headers"] = json!({"Authorization":"private-upstream"});
    p["models"][0]["compat"] = json!({"command":"!private-command","supportsStore":false});
    let (providers, skipped) = generated(&c, &live)?;
    ensure!(skipped == 0);
    let entry = providers.values().next().context("entry")?;
    ensure!(
        entry["api"] == "openai-responses"
            && !entry.to_string().contains("private-upstream")
            && !entry.to_string().contains("private-command")
    );
    ensure!(entry["models"][0]["compat"]["supportsStore"] == false);
    let mut other = c.clone();
    other.name = "central.alpha".into();
    ensure!(namespace(&c) != namespace(&other));
    Ok(())
}

#[tokio::test]
async fn live_apply_refetches_scope_and_refuses_revoked_or_changed_candidates() -> Result<()> {
    use crate::device::{execute, DeviceArgs, DeviceCommand, Registry};
    use std::sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    };
    let live = Arc::new(tokio::sync::RwLock::new(fixture()));
    let denied = Arc::new(AtomicBool::new(false));
    let state = live.clone();
    let revoked = denied.clone();
    let router = axum::Router::new().route(
        "/v1/eavs/catalog",
        axum::routing::get(move |headers: http::HeaderMap| {
            let state = state.clone();
            let revoked = revoked.clone();
            async move {
                if revoked.load(Ordering::SeqCst)
                    || headers
                        .get("authorization")
                        .and_then(|value| value.to_str().ok())
                        != Some("Bearer eavs-synthetic000000000000000000000000000000")
                {
                    return (
                        http::StatusCode::UNAUTHORIZED,
                        axum::Json(json!({"private":"do-not-echo-provider-error"})),
                    );
                }
                (http::StatusCode::OK, axum::Json(state.read().await.clone()))
            }
        }),
    );
    let socket = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let mut c = connection();
    c.url = format!("http://{}", socket.local_addr()?);
    c.allow_loopback_http = true;
    c.listener_key_env = None;
    c.key_env = format!("EAVS_PI_CONFIG_TEST_{}", uuid::Uuid::new_v4().simple());
    let _environment = Environment(c.key_env.clone());
    std::env::set_var(&c.key_env, "eavs-synthetic000000000000000000000000000000");
    let server = tokio::spawn(async move { axum::serve(socket, router).await });
    let _server = Server(server);
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("models.json");
    let registry = dir.path().join("device.json");
    std::fs::write(
        &registry,
        serde_json::to_vec(&Registry {
            connections: vec![c],
        })?,
    )?;
    let args = |expected_revision: Option<String>,
                candidate_revision: Option<String>,
                yes: bool| DeviceArgs {
        config: Some(registry.clone()),
        candidate_revision,
        command: DeviceCommand::Pi {
            name: "central".into(),
            output: path.clone(),
            expected_revision,
            yes,
        },
    };
    let preview = execute(args(None, None, false)).await?;
    ensure!(!path.exists());
    let current = preview["revision"].as_str().context("revision")?.to_owned();
    let candidate = preview["candidate_revision"]
        .as_str()
        .context("candidate")?
        .to_owned();
    live.write().await["providers"][0]["models"][0]["name"] = "Changed model".into();
    ensure!(
        execute(args(Some(current.clone()), Some(candidate.clone()), true))
            .await
            .is_err()
            && !path.exists()
    );
    *live.write().await = fixture();
    execute(args(Some(current.clone()), Some(candidate.clone()), true)).await?;
    let before = std::fs::read(&path)?;
    denied.store(true, Ordering::SeqCst);
    let error = execute(args(Some(current), Some(candidate), true))
        .await
        .err()
        .context("revocation must fail")?;
    ensure!(
        !error.to_string().contains("do-not-echo-provider-error")
            && std::fs::read(&path)? == before
    );
    Ok(())
}

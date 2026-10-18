use super::*;
use anyhow::ensure;

#[tokio::test]
async fn explicit_enroll_preview_apply_status_and_sync_offline_retention() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("device.json");
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let url = format!("http://{}", listener.local_addr()?);
    let app = axum::Router::new().route("/v1/eavs/catalog", axum::routing::get(|| async {
        axum::Json(serde_json::json!({"schema_version":1,"scope":"key-filtered-chat-candidates","providers":[{"id":"central","models":[{"id":"synthetic","name":"Synthetic"}]}]}))
    }));
    let server = tokio::spawn(async move { axum::serve(listener, app).await });
    let env = format!("EAVS_SYNTHETIC_{}", uuid::Uuid::new_v4().simple());
    std::env::set_var(&env, "eavs-synthetic000000000000000000000000000000");
    let connect = |trust, expected_revision, candidate_revision, yes| DeviceArgs {
        config: Some(path.clone()),
        candidate_revision,
        command: DeviceCommand::Connect {
            name: "central".into(),
            url: url.clone(),
            key_env: env.clone(),
            listener_key_env: None,
            trust,
            allow_loopback_http: true,
            expected_revision,
            yes,
        },
    };
    ensure!(execute(connect(false, None, None, false)).await.is_err());
    ensure!(!path.exists());
    let preview = execute(connect(true, None, None, false)).await?;
    ensure!(!path.exists());
    let rev = preview["revision"]
        .as_str()
        .context("missing revision")?
        .to_owned();
    let candidate = preview["candidate_revision"]
        .as_str()
        .context("missing candidate")?
        .to_owned();
    ensure!(
        execute(connect(true, Some(rev.clone()), Some("wrong".into()), true))
            .await
            .is_err()
    );
    execute(connect(true, Some(rev), Some(candidate), true)).await?;
    ensure!(!String::from_utf8(storage::read(&path)?)?.contains("eavs-synthetic"));
    let status = execute(DeviceArgs {
        config: Some(path.clone()),
        candidate_revision: None,
        command: DeviceCommand::Status { json: true },
    })
    .await?;
    ensure!(status["connections"][0]["state"] == "ready");
    let sync = |expected_revision, candidate_revision, yes| DeviceArgs {
        config: Some(path.clone()),
        candidate_revision,
        command: DeviceCommand::Sync {
            name: "central".into(),
            expected_revision,
            yes,
        },
    };
    let preview = execute(sync(None, None, false)).await?;
    execute(sync(
        Some(
            preview["revision"]
                .as_str()
                .context("missing revision")?
                .into(),
        ),
        Some(
            preview["candidate_revision"]
                .as_str()
                .context("missing candidate")?
                .into(),
        ),
        true,
    ))
    .await?;
    let inventory = path.with_file_name("device-inventory-central.json");
    let good = storage::read(&inventory)?;
    server.abort();
    let _ = server.await;
    ensure!(execute(sync(None, None, false)).await.is_err());
    ensure!(storage::read(&inventory)? == good);
    let status = execute(DeviceArgs {
        config: Some(path),
        candidate_revision: None,
        command: DeviceCommand::Status { json: true },
    })
    .await?;
    ensure!(status["connections"][0]["state"] == "unreachable");
    ensure!(status["connections"][0]["providers"]
        .as_array()
        .is_some_and(Vec::is_empty));
    std::env::remove_var(env);
    Ok(())
}

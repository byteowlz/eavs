use super::*;
use anyhow::ensure;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

fn connection(url: &str) -> Connection {
    Connection {
        name: "central".into(),
        url: url.into(),
        key_env: "EAVS_DEVICE_TEST_UNSET_9B77".into(),
        listener_key_env: None,
        allow_loopback_http: true,
    }
}
fn fixture() -> Vec<u8> {
    br#"{"schema_version":1,"scope":"key-filtered-chat-candidates","providers":[{"id":"public","api_key":"never-copy","models":[{"id":"model","name":"Model","secret":"omit"}]}]}"#.to_vec()
}
#[test]
fn rejects_unsafe_destinations() -> Result<()> {
    for url in [
        "http://100.64.0.1",
        "http://localhost",
        "https://user:pass@central",
        "https://central/path",
        "https://central?key=secret",
        "https://central/#fragment",
        "file:///tmp/key",
    ] {
        ensure!(validate(&connection(url)).is_err(), "unsafe URL accepted");
    }
    validate(&connection("https://central.example"))?;
    validate(&connection("http://127.0.0.1:1234"))?;
    let mut c = connection("http://127.0.0.1:1234");
    c.allow_loopback_http = false;
    ensure!(validate(&c).is_err());
    Ok(())
}
#[test]
fn catalog_projection_discards_unknowns() -> Result<()> {
    let p = project(&fixture())?;
    ensure!(p.len() == 1 && p[0].models[0].id == "model");
    let rendered = serde_json::to_string(&p)?;
    ensure!(!rendered.contains("never-copy") && !rendered.contains("secret"));
    ensure!(project(br#"{"schema_version":2,"providers":[]}"#).is_err());
    ensure!(project(br#"{"schema_version":1,"scope":"key-filtered-chat-candidates","providers":[{"id":"../bad","models":[]}]}"#).is_err());
    Ok(())
}
#[test]
fn publication_requires_cas_and_preserves_last_good() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("device.json");
    let first = br#"{"connections":[]}"#;
    ensure!(storage::apply(&path, b"", first, None).is_err());
    storage::apply(&path, b"", first, Some(&revision(b"")))?;
    ensure!(storage::apply(&path, b"", b"bad", Some(&revision(b""))).is_err());
    ensure!(storage::read(&path)? == first);
    let next = br#"{"connections":[] }"#;
    storage::apply(&path, first, next, Some(&revision(first)))?;
    ensure!(storage::read(&path.with_extension("json.last-good"))? == first);
    ensure!(storage::read(&path)? == next);
    std::fs::write(&path, b"malformed secret")?;
    ensure!(load(&path).is_err());
    Ok(())
}
#[test]
fn dirty_repository_and_lock_conflicts_stop_publication() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let output = std::process::Command::new("git")
        .args(["init", "--quiet"])
        .arg(dir.path())
        .output()?;
    ensure!(output.status.success());
    std::fs::write(dir.path().join("unrelated"), "dirty")?;
    let path = dir.path().join("device.json");
    ensure!(storage::apply(&path, b"", b"{}", Some(&revision(b""))).is_err());
    ensure!(!path.exists());
    let other = tempfile::tempdir()?;
    let path = other.path().join("device.json");
    std::fs::write(path.with_extension("json.lock"), "occupied")?;
    ensure!(storage::apply(&path, b"", b"{}", Some(&revision(b""))).is_err());
    ensure!(!path.exists());
    Ok(())
}
#[cfg(unix)]
#[test]
fn symlink_files_are_rejected() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let target = dir.path().join("source");
    std::fs::write(&target, "do not read or overwrite")?;
    let path = dir.path().join("device.json");
    std::os::unix::fs::symlink(&target, &path)?;
    ensure!(load(&path).is_err());
    ensure!(storage::apply(&path, b"", b"{}", Some(&revision(b""))).is_err());
    ensure!(std::fs::read_to_string(target)? == "do not read or overwrite");
    Ok(())
}
#[tokio::test]
async fn missing_credentials_never_contacts_gateway() -> Result<()> {
    let status = observe(&connection("http://127.0.0.1:1")).await?;
    ensure!(status.state == "unconfigured" && status.providers.is_empty());
    Ok(())
}
async fn synthetic_response(code: u16, body: Vec<u8>) -> Result<Status> {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let c = connection(&format!("http://{}", listener.local_addr()?));
    let task = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await?;
        let mut request = vec![0; 8192];
        let n = stream.read(&mut request).await?;
        let request = String::from_utf8_lossy(&request[..n]);
        ensure!(request.starts_with("GET /v1/eavs/catalog "));
        ensure!(!request.contains("provider_probe"));
        let headers = format!("HTTP/1.1 {code} Test\r\nContent-Length: {}\r\nLocation: http://127.0.0.1:1/secret\r\nConnection: close\r\n\r\n", body.len());
        stream.write_all(headers.as_bytes()).await?;
        stream.write_all(&body).await?;
        Ok::<(), anyhow::Error>(())
    });
    let seed = Status {
        name: c.name.clone(),
        url: c.url.clone(),
        state: "unconfigured",
        diagnostic: "credential_missing",
        providers: vec![],
    };
    let result = observe_key(&c, "eavs-synthetic000000000000000000000000000000", seed).await?;
    task.await??;
    Ok(result)
}
#[tokio::test]
async fn synthetic_http_classification_and_no_redirects() -> Result<()> {
    let ready = synthetic_response(200, fixture()).await?;
    ensure!(ready.state == "ready" && ready.providers.len() == 1);
    for (code, state, diagnostic) in [
        (401, "unauthorized", "authentication_rejected"),
        (403, "unauthorized", "authentication_rejected"),
        (429, "unavailable", "rate_limited"),
        (503, "unavailable", "server_failure"),
        (302, "unavailable", "unexpected_http_status"),
    ] {
        let s = synthetic_response(code, b"upstream secret error".to_vec()).await?;
        ensure!(s.state == state && s.diagnostic == diagnostic && s.providers.is_empty());
        ensure!(!serde_json::to_string(&s)?.contains("upstream secret"));
    }
    let bad = synthetic_response(200, b"not a catalog".to_vec()).await?;
    ensure!(bad.diagnostic == "invalid_catalog");
    let large = synthetic_response(200, vec![b' '; 4 * 1024 * 1024 + 1]).await?;
    ensure!(large.diagnostic == "catalog_limit");
    Ok(())
}

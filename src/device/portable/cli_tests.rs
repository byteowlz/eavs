use super::{
    connectivity, parse, tests::fixture, Connectivity, Observation, Observations, Reachability,
};
use anyhow::Result;
use anyhow::{ensure, Context};
use clap::Parser;
use std::path::Path;
#[derive(Parser)]
struct TestCli {
    #[command(flatten)]
    args: crate::device::DeviceArgs,
}
async fn cli(args: &[&str]) -> Result<serde_json::Value> {
    let mut command_line = vec!["device"];
    command_line.extend_from_slice(args);
    crate::device::execute(TestCli::try_parse_from(command_line)?.args).await
}
fn path(p: &Path) -> Result<&str> {
    p.to_str().context("test path encoding")
}
fn pin<'a>(value: &'a serde_json::Value, key: &str) -> Result<&'a str> {
    value["review"][key].as_str().context("missing review pin")
}
#[tokio::test]
async fn declaration_cli_preview_apply_conflicts_without_registry() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let candidate = dir.path().join("candidate.json");
    let output = dir.path().join("declaration.json");
    std::fs::write(&candidate, fixture())?;
    let (c, o) = (path(&candidate)?, path(&output)?);
    ensure!(
        cli(&["portable", "inspect", "--declaration", c]).await?["declaration"]["version"] == 1
    );
    let preview = cli(&["portable", "preview", "--candidate", c, "--output", o]).await?;
    ensure!(!output.exists());
    let expected = pin(&preview, "expected_revision")?;
    let approved = pin(&preview, "candidate_revision")?;
    ensure!(cli(&[
        "portable",
        "apply",
        "--candidate",
        c,
        "--output",
        o,
        "--expected-revision",
        expected,
        "--yes"
    ])
    .await
    .is_err());
    let apply = [
        "--candidate-revision",
        approved,
        "portable",
        "apply",
        "--candidate",
        c,
        "--output",
        o,
        "--expected-revision",
        expected,
        "--yes",
    ];
    ensure!(cli(&apply).await?["serving_changed"] == false);
    ensure!(cli(&apply).await.is_err());
    ensure!(std::fs::read(&output)? == fixture());
    let p = cli(&["portable", "preview", "--candidate", c, "--output", o]).await?;
    std::fs::write(&candidate, b"{\"secret\":\"TOP_SECRET\"}")?;
    let error = cli(&[
        "--candidate-revision",
        pin(&p, "candidate_revision")?,
        "portable",
        "apply",
        "--candidate",
        c,
        "--output",
        o,
        "--expected-revision",
        pin(&p, "expected_revision")?,
        "--yes",
    ])
    .await
    .err()
    .context("rejection required")?;
    ensure!(!error.to_string().contains("TOP_SECRET"));
    ensure!(std::fs::read(&output)? == fixture());
    Ok(())
}
#[tokio::test]
async fn binding_cli_publishes_only_refs_with_cas_and_last_good() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let declaration = dir.path().join("declaration.json");
    let output = dir.path().join("machine-local.json");
    std::fs::write(&declaration, fixture())?;
    let (d, o) = (path(&declaration)?, path(&output)?);
    let base = [
        "portable",
        "bind",
        "--declaration",
        d,
        "--output",
        o,
        "--provider",
        "web",
        "--env",
        "EXPLICIT_LOCAL_KEY",
    ];
    let p = cli(&base).await?;
    ensure!(!output.exists());
    let mut args = vec!["--candidate-revision", pin(&p, "candidate_revision")?];
    args.extend(base);
    args.extend([
        "--declaration-revision",
        p["declaration_revision"]
            .as_str()
            .context("declaration pin")?,
        "--expected-revision",
        pin(&p, "expected_revision")?,
        "--yes",
    ]);
    cli(&args).await?;
    ensure!(cli(&args).await.is_err());
    let original = std::fs::read(&output)?;
    let p = cli(&base).await?;
    let mut args = vec!["--candidate-revision", pin(&p, "candidate_revision")?];
    args.extend(base);
    args.extend([
        "--declaration-revision",
        p["declaration_revision"]
            .as_str()
            .context("declaration pin")?,
        "--expected-revision",
        pin(&p, "expected_revision")?,
        "--yes",
    ]);
    cli(&args).await?;
    ensure!(std::fs::read(output.with_extension("portable.last-good"))? == original);
    ensure!(cli(&[
        "portable",
        "bind",
        "--declaration",
        d,
        "--output",
        o,
        "--provider",
        "web",
        "--env",
        "$(cat secret)"
    ])
    .await
    .is_err());
    ensure!(cli(&[
        "portable",
        "bind",
        "--declaration",
        d,
        "--output",
        d,
        "--provider",
        "web",
        "--env",
        "KEY"
    ])
    .await
    .is_err());
    ensure!(std::fs::read(&declaration)? == fixture());
    std::fs::write(
        &output,
        br#"{"version":1,"bindings":[{"provider":"web","env":"KEY","secret":"TOP_SECRET"}]}"#,
    )?;
    let error = cli(&base).await.err().context("secret field rejection")?;
    ensure!(!error.to_string().contains("TOP_SECRET"));
    ensure!(std::fs::read(output.with_extension("portable.last-good"))? == original);
    Ok(())
}
#[tokio::test]
async fn head_only_observation_and_plan_cli() -> Result<()> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let origin = format!("http://{}", listener.local_addr()?);
    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await?;
        let mut request = vec![0; 4096];
        let n = socket.read(&mut request).await?;
        let request = String::from_utf8(request[..n].to_vec())?;
        ensure!(request.starts_with("HEAD / HTTP/1.1\r\n"));
        ensure!(!request.to_ascii_lowercase().contains("authorization:"));
        socket.write_all(b"HTTP/1.1 302 Found\r\nLocation: http://127.0.0.1:1/never-follow\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").await?;
        anyhow::Ok(())
    });
    let dir = tempfile::tempdir()?;
    let declaration = dir.path().join("declaration.json");
    let observation_file = dir.path().join("observations.json");
    let mut d = parse(&fixture())?;
    d.providers[0].origin = origin;
    d.providers[0].connectivity = Connectivity::Local;
    std::fs::write(&declaration, serde_json::to_vec(&d)?)?;
    let obs = cli(&[
        "portable",
        "observe",
        "--declaration",
        path(&declaration)?,
        "--provider",
        "web",
        "--ttl-seconds",
        "30",
    ])
    .await?;
    server.await??;
    let mut observations: Observations = serde_json::from_value(obs)?;
    ensure!(observations.items[0].state == Reachability::Reachable);
    observations.items.push(Observation {
        provider: "mesh".into(),
        state: Reachability::Unreachable,
        observed_at: connectivity::now()?,
    });
    std::fs::write(&observation_file, serde_json::to_vec(&observations)?)?;
    let args = [
        "portable",
        "plan",
        "--declaration",
        path(&declaration)?,
        "--profile",
        "laptop",
        "--route",
        "private",
        "--route",
        "paid",
        "--observations",
        path(&observation_file)?,
    ];
    let result = cli(&args).await?;
    ensure!(result["selections"][0]["state"] == "unreachable");
    ensure!(result["selections"][1]["state"] == "reachable");
    ensure!(result["authority_granted"] == false && result["serving_changed"] == false);
    observations.items[0].observed_at = 1;
    std::fs::write(&observation_file, serde_json::to_vec(&observations)?)?;
    ensure!(cli(&args).await?["selections"][1]["state"] == "unknown");
    let failed = cli(&[
        "portable",
        "observe",
        "--declaration",
        path(&declaration)?,
        "--provider",
        "web",
    ])
    .await?;
    ensure!(failed["items"][0]["state"] == "unreachable");
    ensure!(!serde_json::to_string(&failed)?.contains("tailscale"));
    Ok(())
}

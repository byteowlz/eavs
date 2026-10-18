use super::*;
use anyhow::ensure;
pub(super) fn fixture() -> Vec<u8> {
    br#"{"version":1,"providers":[{"id":"web","origin":"https://example.com","connectivity":"public"},{"id":"mesh","origin":"https://node.example","connectivity":"tailnet"}],"routes":[{"id":"paid","provider":"web","model":"model"},{"id":"private","provider":"mesh","model":"model"}],"loadouts":[{"id":"work","routes":["paid","private"]}],"profiles":[{"id":"laptop","loadout":"work"}]}"#.to_vec()
}
#[test]
fn strict_and_redacted() -> Result<()> {
    for bytes in [
        b"{".to_vec(),
        vec![b' '; MAX_BYTES + 1],
        fixture()
            .iter()
            .copied()
            .chain(b" trailing".iter().copied())
            .collect(),
    ] {
        ensure!(parse(&bytes).is_err());
    }
    let mut value: serde_json::Value = serde_json::from_slice(&fixture())?;
    value["credential_command"] = "TOP_SECRET".into();
    let error = parse(&serde_json::to_vec(&value)?)
        .err()
        .ok_or_else(|| anyhow::anyhow!("expected rejection"))?;
    ensure!(!error.to_string().contains("TOP_SECRET"));
    value
        .as_object_mut()
        .ok_or_else(|| anyhow::anyhow!("object"))?
        .remove("credential_command");
    value["providers"][0]["origin"] = "https://user:TOP_SECRET@example.com/".into();
    ensure!(parse(&serde_json::to_vec(&value)?).is_err());
    let d = parse(&fixture())?;
    for env in ["$(cat secret)", "/tmp/key", "sk-secret-value", "${KEY}"] {
        let b = serde_json::json!({"version":1,"bindings":[{"provider":"web","env":env}]});
        ensure!(parse_bindings(&serde_json::to_vec(&b)?, &d).is_err());
    }
    ensure!(parse_bindings(
        br#"{"version":1,"bindings":[{"provider":"web","env":"EAVS_WEB_KEY"}]}"#,
        &d
    )
    .is_ok());
    Ok(())
}
#[test]
fn cas_and_last_good() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("portable.json");
    let bytes = fixture();
    let p = preview(&path, &bytes)?;
    apply(&path, &bytes, &p)?;
    ensure!(apply(&path, &bytes, &p).is_err());
    let next = preview(&path, &bytes)?;
    let mut changed = bytes.clone();
    changed.push(b' ');
    ensure!(apply(&path, &changed, &next).is_err());
    apply(&path, &bytes, &next)?;
    ensure!(std::fs::read(path.with_extension("portable.last-good"))? == bytes);
    std::fs::write(&path, b"malformed")?;
    ensure!(preview(&path, &bytes).is_err());
    ensure!(apply(&path, &bytes, &next).is_err());
    std::fs::write(&path, b"")?;
    ensure!(preview(&path, &bytes).is_err());
    Ok(())
}
#[test]
fn internet_up_tailnet_down_is_not_global_disconnect() -> Result<()> {
    let bytes = fixture();
    let d = parse(&bytes)?;
    let mut observations = Observations::empty(&d)?;
    observations.items = vec![
        Observation {
            provider: "web".into(),
            state: Reachability::Reachable,
            observed_at: 100,
        },
        Observation {
            provider: "mesh".into(),
            state: Reachability::Unreachable,
            observed_at: 100,
        },
    ];
    let result = plan(
        &d,
        "laptop",
        &["private".into(), "paid".into()],
        &observations,
        100,
    )?;
    ensure!(result[0].state == Reachability::Unreachable);
    ensure!(result[1].state == Reachability::Reachable);
    ensure!(plan(&d, "laptop", &["private".into()], &observations, 100)?.len() == 1);
    ensure!(
        plan(
            &d,
            "laptop",
            &["private".into()],
            &Observations::empty(&d)?,
            100
        )?[0]
            .state
            == Reachability::Unknown
    );
    ensure!(plan(&d, "laptop", &["not-approved".into()], &observations, 100).is_err());
    ensure!(serde_json::to_vec(&d)? == serde_json::to_vec(&parse(&bytes)?)?);
    Ok(())
}
#[test]
fn observations_expire_and_pin_origins() -> Result<()> {
    let mut d = parse(&fixture())?;
    let mut observations = Observations::empty(&d)?;
    observations.items.push(Observation {
        provider: "web".into(),
        state: Reachability::Reachable,
        observed_at: 100,
    });
    let selected = ["paid".into()];
    ensure!(plan(&d, "laptop", &selected, &observations, 130)?[0].state == Reachability::Reachable);
    ensure!(plan(&d, "laptop", &selected, &observations, 131)?[0].state == Reachability::Unknown);
    ensure!(plan(&d, "laptop", &selected, &observations, 99)?[0].state == Reachability::Unknown);
    observations.ttl_seconds = 61;
    ensure!(plan(&d, "laptop", &selected, &observations, 100).is_err());
    observations.ttl_seconds = 30;
    d.providers[0].origin = "https://other.example".into();
    ensure!(plan(&d, "laptop", &selected, &observations, 100).is_err());
    Ok(())
}

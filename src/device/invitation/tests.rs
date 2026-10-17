use super::*;
use axum::{
    extract::State,
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::post,
    Json, Router,
};
use clap::Parser;
use std::sync::{Arc, Mutex};
type TestResult = Result<(), Box<dyn std::error::Error + Send + Sync>>;
fn args(directory: &Path) -> InviteArgs {
    InviteArgs {
        client: InviteClient::Oqto,
        connection_type: ConnectionType::PiMessages,
        template: "scoped-template-id".into(),
        admin_url: "http://127.0.0.1:1".into(),
        admin_key_env: "TEST_ADMIN_REFERENCE".into(),
        listener_key_env: Some("TEST_LISTENER_REFERENCE".into()),
        allow_loopback_http: true,
        origin: "https://gateway.example:9443".into(),
        certificate_sha256: "ab".repeat(32),
        invitation_ttl_seconds: 60,
        device_ttl_seconds: 3600,
        output: directory.join("new-invitation"),
        qr: true,
        dry_run: false,
        yes: true,
        json: true,
    }
}
fn resolve(name: &str) -> Result<HeaderValue> {
    match name {
        "TEST_ADMIN_REFERENCE" => Ok(HeaderValue::from_static("synthetic-operator-root")),
        "TEST_LISTENER_REFERENCE" => Ok(HeaderValue::from_static("synthetic-listener-capability")),
        _ => bail!("unexpected reference"),
    }
}
fn refuse_resolution(_: &str) -> Result<HeaderValue> {
    bail!("dry-run must not resolve credentials")
}
#[derive(Clone)]
struct Server {
    calls: Arc<Mutex<Vec<(HeaderMap, serde_json::Value)>>>,
    failure: bool,
    redirect: bool,
    output_collision: Option<PathBuf>,
}
async fn issue_handler(
    State(state): State<Server>,
    headers: HeaderMap,
    Json(body): Json<serde_json::Value>,
) -> Response {
    if let Ok(mut calls) = state.calls.lock() {
        calls.push((headers, body));
    } else {
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    }
    if let Some(path) = state.output_collision {
        let _ = fs::write(path, b"foreign file must remain");
    }
    if state.redirect {
        return (
            StatusCode::TEMPORARY_REDIRECT,
            [("location", "/unexpected")],
        )
            .into_response();
    }
    if state.failure {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            "private-server-body synthetic-operator-root",
        )
            .into_response();
    }
    (StatusCode::CREATED,Json(serde_json::json!({"token":format!("enroll_{}","a".repeat(64)),"expires_at":2_000_000_000}))).into_response()
}
async fn revoke_handler(
    State(state): State<Server>,
    headers: HeaderMap,
    Json(body): Json<serde_json::Value>,
) -> Response {
    if let Ok(mut calls) = state.calls.lock() {
        calls.push((headers, body));
    }
    (StatusCode::OK, Json(serde_json::json!({"revoked":true}))).into_response()
}
async fn server(state: Server) -> Result<(String, tokio::task::JoinHandle<()>)> {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let router = Router::new()
        .route("/admin/device-invitations", post(issue_handler))
        .route("/admin/device-invitations/revoke", post(revoke_handler))
        .with_state(state);
    let task = tokio::spawn(async move {
        let _ = axum::serve(listener, router).await;
    });
    Ok((format!("http://{address}"), task))
}
fn fixture() -> Server {
    Server {
        calls: Arc::new(Mutex::new(Vec::new())),
        failure: false,
        redirect: false,
        output_collision: None,
    }
}
#[tokio::test]
#[expect(
    clippy::panic_in_result_fn,
    reason = "Fallible CLI fixture retains offline dry-run and no-output assertions"
)]
async fn dry_run_is_credential_blind_network_free_and_has_no_secret_artifacts() -> TestResult {
    let dir = tempfile::tempdir()?;
    let mut options = args(dir.path());
    options.dry_run = true;
    let receipt = execute_with_resolver(&options, refuse_resolution).await?;
    assert_eq!(receipt["state"], "dry-run");
    assert_eq!(receipt["connection_type"], "pi-messages");
    assert!(!options.output.exists());
    assert!(!receipt.to_string().contains("enroll_"));
    options.dry_run = false;
    options.yes = false;
    if !std::io::stdin().is_terminal() {
        assert!(execute_with_resolver(&options, refuse_resolution)
            .await
            .is_err());
        assert!(!options.output.exists());
    }
    Ok(())
}
#[tokio::test]
#[expect(
    clippy::panic_in_result_fn,
    reason = "HTTP fixture verifies exact authority forwarding and private typed QR publication"
)]
async fn issuance_writes_private_typed_files_but_receipt_contains_no_credentials() -> TestResult {
    let dir = tempfile::tempdir()?;
    let mut options = args(dir.path());
    let state = fixture();
    let (url, task) = server(state.clone()).await?;
    options.admin_url = url;
    let receipt = execute_with_resolver(&options, resolve).await?;
    task.abort();
    assert_eq!(receipt["state"], "issued");
    let data = fs::read(options.output.join("invitation.json"))?;
    let descriptor: serde_json::Value = serde_json::from_slice(&data)?;
    assert_eq!(descriptor["version"], 2);
    assert_eq!(descriptor["connection_type"], "pi-messages");
    assert_eq!(descriptor.as_object().ok_or("descriptor")?.len(), 5);
    let token = format!("enroll_{}", "a".repeat(64));
    assert_eq!(descriptor["token"], token);
    let image = fs::read_to_string(options.output.join("invitation.svg"))?;
    let expected = QrCode::with_error_correction_level(&data, EcLevel::M)?
        .render::<svg::Color<'_>>()
        .min_dimensions(512, 512)
        .quiet_zone(true)
        .build();
    assert_eq!(image, expected);
    assert!(image.contains("<svg"));
    for secret in ["synthetic-operator-root", "synthetic-listener-capability"] {
        assert!(!String::from_utf8_lossy(&data).contains(secret));
        assert!(!image.contains(secret));
        assert!(!receipt.to_string().contains(secret));
    }
    assert!(!receipt.to_string().contains(&token));
    {
        let calls = state.calls.lock().map_err(|_| "calls mutex")?;
        assert_eq!(calls.len(), 1);
        assert_eq!(
            calls[0].0["authorization"],
            "Bearer synthetic-operator-root"
        );
        assert_eq!(calls[0].0["x-eavs-token"], "synthetic-listener-capability");
        assert_eq!(
            calls[0].1,
            serde_json::json!({"template_id":"scoped-template-id","invitation_ttl_seconds":60,"device_ttl_seconds":3600})
        );
        drop(calls);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            fs::metadata(&options.output)?.permissions().mode() & 0o777,
            0o700
        );
        for name in ["invitation.json", "invitation.svg"] {
            assert_eq!(
                fs::metadata(options.output.join(name))?
                    .permissions()
                    .mode()
                    & 0o777,
                0o600
            );
        }
    }
    assert!(execute_with_resolver(&options, refuse_resolution)
        .await
        .is_err());
    // Optional synthetic artifact for independent macOS Vision QR decoding.
    if let Some(path) = std::env::var_os("EAVS_INVITE_SYNTHETIC_FIXTURE") {
        let path = PathBuf::from(path);
        private_directory(&path)?;
        private_file(&path.join("invitation.json"), &data)?;
        private_file(&path.join("invitation.svg"), image.as_bytes())?;
    }
    Ok(())
}
#[tokio::test]
#[expect(
    clippy::panic_in_result_fn,
    reason = "Private HTTP errors and redirects must never disclose or replay administrator credentials"
)]
async fn failures_do_not_echo_private_bodies_or_follow_redirects() -> TestResult {
    for redirect in [false, true] {
        let dir = tempfile::tempdir()?;
        let mut options = args(dir.path());
        let mut state = fixture();
        state.failure = !redirect;
        state.redirect = redirect;
        let (url, task) = server(state.clone()).await?;
        options.admin_url = url;
        let error = execute_with_resolver(&options, resolve)
            .await
            .err()
            .ok_or("expected failure")?
            .to_string();
        task.abort();
        assert!(!error.contains("private-server-body"));
        assert!(!error.contains("synthetic-operator-root"));
        assert!(error.contains("uncertain"));
        assert_eq!(state.calls.lock().map_err(|_| "calls mutex")?.len(), 1);
        assert!(!options.output.join("invitation.json").exists());
    }
    Ok(())
}
#[tokio::test]
#[expect(
    clippy::panic_in_result_fn,
    reason = "Publication collision must preserve existing bytes and revoke the known invitation"
)]
async fn publication_failure_revokes_without_overwriting_existing_bytes() -> TestResult {
    let dir = tempfile::tempdir()?;
    let mut options = args(dir.path());
    let mut state = fixture();
    state.output_collision = Some(options.output.join("invitation.json"));
    let (url, task) = server(state.clone()).await?;
    options.admin_url = url;
    let error = execute_with_resolver(&options, resolve)
        .await
        .err()
        .ok_or("expected failure")?
        .to_string();
    task.abort();
    assert!(error.contains("invitation revoked"));
    assert_eq!(
        fs::read(options.output.join("invitation.json"))?,
        b"foreign file must remain"
    );
    let calls = state.calls.lock().map_err(|_| "calls mutex")?;
    assert_eq!(calls.len(), 2);
    assert!(calls[1].1["token"]
        .as_str()
        .is_some_and(|v| v.starts_with("enroll_")));
    drop(calls);
    Ok(())
}
#[test]
#[expect(
    clippy::panic_in_result_fn,
    reason = "Unsafe public destinations and command variants must fail before issuance"
)]
fn validation_and_parser_reject_unsafe_connection_types_and_origins() -> TestResult {
    let dir = tempfile::tempdir()?;
    for origin in [
        "http://127.0.0.1:9443",
        "https://name:secret@gateway.example",
        "https://gateway.example/path",
        "https://gateway.example?secret=value",
        "https://gateway.example#fragment",
    ] {
        let mut options = args(dir.path());
        options.origin = origin.into();
        assert!(validate_args(&options).is_err());
    }
    let mut options = args(dir.path());
    options.admin_url = "http://100.100.100.1:3033".into();
    assert!(validate_args(&options).is_err());
    options = args(dir.path());
    options.certificate_sha256 = "not-a-pin".into();
    assert!(validate_args(&options).is_err());
    assert!(crate::cli::Cli::try_parse_from([
        "eavs",
        "device",
        "invite",
        "--client",
        "oqto",
        "--connection-type",
        "openai"
    ])
    .is_err());
    assert!(
        crate::cli::Cli::try_parse_from(["eavs", "device", "invite", "--client", "untrusted"])
            .is_err()
    );
    Ok(())
}

//! Explicit operator issuance. QR is a connection descriptor, never authorization.
use super::{environment_reference, validate, Connection};
use anyhow::{bail, Context, Result};
use clap::{Args, ValueEnum};
use qrcode::{render::svg, EcLevel, QrCode};
use reqwest::{
    header::{HeaderMap, HeaderValue, AUTHORIZATION},
    redirect::Policy,
    Client,
};
use serde::{Deserialize, Serialize};
use std::{
    fs,
    io::{IsTerminal, Write},
    path::{Path, PathBuf},
    time::Duration,
};

#[derive(Clone, Copy, ValueEnum)]
pub enum InviteClient {
    Oqto,
}
#[derive(Clone, Copy, ValueEnum)]
pub enum ConnectionType {
    PiMessages,
}
#[derive(Args)]
#[command(
    after_help = "Example (no credentials in arguments):\n  eavs device invite --client oqto --template KEY_ID --admin-url http://127.0.0.1:3033 --allow-loopback-http --admin-key-env EAVS_MASTER_KEY --origin https://gateway.example:9443 --certificate-sha256 HEX64 --output /private/invitations/phone --qr --dry-run\n\nRemove --dry-run and approve (or pass --yes) to issue once. JSON/QR contain a short-lived secret; they are written only to a NEW private directory, never stdout. No inference, discovery or certificate download occurs."
)]
pub struct InviteArgs {
    #[arg(long, value_enum)]
    pub client: InviteClient,
    #[arg(long, value_enum, default_value = "pi-messages")]
    pub connection_type: ConnectionType,
    /// Existing scoped virtual-key ID, not a display name; authority is inherited server-side.
    #[arg(long, value_name = "KEY_ID")]
    pub template: String,
    /// Operator listener origin, distinct from the phone's HTTPS device ingress.
    #[arg(long)]
    pub admin_url: String,
    /// Environment NAME holding root authorization; never a literal credential.
    #[arg(long)]
    pub admin_key_env: String,
    /// Optional environment NAME holding the operator listener capability.
    #[arg(long)]
    pub listener_key_env: Option<String>,
    /// Allow HTTP only for a literal loopback operator listener, never the phone origin.
    #[arg(long)]
    pub allow_loopback_http: bool,
    /// Explicit externally reachable HTTPS device-ingress origin.
    #[arg(long)]
    pub origin: String,
    /// Operator-verified leaf certificate SHA-256, 64 hexadecimal characters.
    #[arg(long)]
    pub certificate_sha256: String,
    #[arg(long, default_value_t=300, value_parser=clap::value_parser!(u32).range(1..=600))]
    pub invitation_ttl_seconds: u32,
    #[arg(long, default_value_t=3600, value_parser=clap::value_parser!(u32).range(1..=31_536_000))]
    pub device_ttl_seconds: u32,
    /// New absolute private directory in an existing trusted parent; never overwritten.
    #[arg(long)]
    pub output: PathBuf,
    /// Also write invitation.svg (QR); no terminal or stdout secret rendering.
    #[arg(long)]
    pub qr: bool,
    /// Validate/describe only: no environment credential reads, files or HTTP requests.
    #[arg(long)]
    pub dry_run: bool,
    #[arg(long, short = 'y')]
    pub yes: bool,
    /// Secret-free receipt only; invitation JSON is always a private file.
    #[arg(long)]
    pub json: bool,
}
#[derive(Serialize)]
struct Descriptor<'a> {
    version: u8,
    connection_type: &'static str,
    origin: &'a str,
    certificate_sha256: String,
    token: &'a str,
}
#[derive(Deserialize)]
struct Issued {
    token: String,
    expires_at: i64,
}
fn validate_args(a: &InviteArgs) -> Result<()> {
    for (name, url, key, listener, allow) in [
        (
            "operator",
            a.admin_url.clone(),
            a.admin_key_env.clone(),
            a.listener_key_env.clone(),
            a.allow_loopback_http,
        ),
        (
            "device",
            a.origin.clone(),
            "UNUSED_REFERENCE".into(),
            None,
            false,
        ),
    ] {
        validate(&Connection {
            name: name.into(),
            url,
            key_env: key,
            listener_key_env: listener,
            allow_loopback_http: allow,
        })?;
    }
    if !environment_reference(&a.admin_key_env)
        || a.template.is_empty()
        || a.template.len() > 128
        || a.template.chars().any(char::is_control)
        || a.origin.len() > 512
        || a.origin.chars().any(char::is_control)
        || a.admin_url.chars().any(char::is_control)
        || a.certificate_sha256.len() != 64
        || !a.certificate_sha256.bytes().all(|b| b.is_ascii_hexdigit())
        || !(1..=600).contains(&a.invitation_ttl_seconds)
        || !(1..=31_536_000).contains(&a.device_ttl_seconds)
    {
        bail!("invalid template, credential reference, origin, TTL or certificate SHA-256");
    }
    if !a.output.is_absolute() || a.output.file_name().is_none() || a.output.to_str().is_none() {
        bail!("--output requires a new absolute directory");
    }
    if fs::symlink_metadata(&a.output).is_ok() {
        bail!("output already exists; choose a new private directory (nothing issued)");
    }
    let parent = a.output.parent().context("missing output parent")?;
    let metadata = fs::metadata(parent)
        .context("output parent must already exist; create a private directory first")?;
    if !metadata.is_dir() {
        bail!("output parent must be a trusted directory");
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if metadata.permissions().mode() & 0o022 != 0 {
            bail!("output parent must not be group/world writable; create a private parent first");
        }
        Ok(())
    }
    #[cfg(not(unix))]
    {
        bail!("private invitation output requires Unix permissions; this platform's ACL publication is not implemented");
    }
}
fn credential(name: &str) -> Result<HeaderValue> {
    let value = std::env::var(name)
        .map_err(|_| anyhow::anyhow!("credential environment reference is unset"))?;
    if value.is_empty() {
        bail!("credential environment reference is empty");
    }
    let mut header =
        HeaderValue::from_str(&value).map_err(|_| anyhow::anyhow!("invalid credential header"))?;
    header.set_sensitive(true);
    Ok(header)
}
fn operator_client(a: &InviteArgs, resolve: fn(&str) -> Result<HeaderValue>) -> Result<Client> {
    let raw = resolve(&a.admin_key_env)?;
    let text = raw
        .to_str()
        .map_err(|_| anyhow::anyhow!("invalid administrator credential"))?;
    let mut authorization = HeaderValue::from_str(&format!("Bearer {text}"))
        .map_err(|_| anyhow::anyhow!("invalid administrator credential"))?;
    authorization.set_sensitive(true);
    let mut headers = HeaderMap::new();
    headers.insert(AUTHORIZATION, authorization);
    if let Some(name) = &a.listener_key_env {
        headers.insert("x-eavs-token", resolve(name)?);
    }
    Client::builder()
        .no_proxy()
        .redirect(Policy::none())
        .http1_only()
        .pool_max_idle_per_host(0)
        .timeout(Duration::from_secs(10))
        .default_headers(headers)
        .build()
        .map_err(|_| anyhow::anyhow!("could not initialize verified operator transport"))
}
fn endpoint(a: &InviteArgs, path: &str) -> Result<reqwest::Url> {
    reqwest::Url::parse(&a.admin_url)?
        .join(path)
        .map_err(Into::into)
}
async fn issue(a: &InviteArgs, client: &Client) -> Result<Issued> {
    let result=async {
        let mut response=client.post(endpoint(a,"/admin/device-invitations")?).json(&serde_json::json!({
            "template_id":a.template,"invitation_ttl_seconds":a.invitation_ttl_seconds,"device_ttl_seconds":a.device_ttl_seconds
        })).send().await?;
        if response.status()!=reqwest::StatusCode::CREATED || response.content_length().is_some_and(|n|n>2048) {bail!("issuance rejected");}
        let mut bytes=Vec::new();
        while let Some(chunk)=response.chunk().await? {if bytes.len()+chunk.len()>2048 {bail!("issuance response too large");} bytes.extend_from_slice(&chunk);}
        let issued:Issued=serde_json::from_slice(&bytes)?;
        if issued.token.len()!=71 || !issued.token.starts_with("enroll_") || !issued.token[7..].bytes().all(|b|b.is_ascii_hexdigit()) || issued.expires_at<=0 {bail!("invalid issuance response");}
        Ok::<_,anyhow::Error>(issued)
    }.await;
    result.map_err(|_|anyhow::anyhow!("issuance failed or outcome uncertain; no automatic retry. Inspect the operator and let any outstanding invitation expire before replacement"))
}
fn descriptor(a: &InviteArgs, issued: &Issued) -> Result<Vec<u8>> {
    let bytes = serde_json::to_vec(&Descriptor {
        version: 2,
        connection_type: "pi-messages",
        origin: &a.origin,
        certificate_sha256: a.certificate_sha256.to_ascii_lowercase(),
        token: &issued.token,
    })?;
    if bytes.len() > 2048 {
        bail!("invitation descriptor exceeds client limit");
    }
    Ok(bytes)
}
fn private_directory(path: &Path) -> Result<()> {
    let mut builder = fs::DirBuilder::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder
        .create(path)
        .context("could not reserve a new private output directory; nothing issued")
}
fn private_file(path: &Path, bytes: &[u8]) -> Result<()> {
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options
        .open(path)
        .context("could not create private invitation file")?;
    file.write_all(bytes)?;
    file.sync_all()?;
    Ok(())
}
fn publish(a: &InviteArgs, issued: &Issued) -> Result<()> {
    let bytes = descriptor(a, issued)?;
    let svg = if a.qr {
        Some(
            QrCode::with_error_correction_level(&bytes, EcLevel::M)?
                .render::<svg::Color<'_>>()
                .min_dimensions(512, 512)
                .quiet_zone(true)
                .build(),
        )
    } else {
        None
    };
    private_file(&a.output.join("invitation.json"), &bytes)?;
    if let Some(image) = svg {
        private_file(&a.output.join("invitation.svg"), image.as_bytes())?;
    }
    Ok(())
}
async fn execute(a: &InviteArgs) -> Result<serde_json::Value> {
    execute_with_resolver(a, credential).await
}
async fn execute_with_resolver(
    a: &InviteArgs,
    resolve: fn(&str) -> Result<HeaderValue>,
) -> Result<serde_json::Value> {
    validate_args(a)?;
    let mut receipt = serde_json::json!({"schema_version":1,"state":"dry-run","client":"oqto","connection_type":"pi-messages","origin":a.origin,"certificate_sha256":a.certificate_sha256.to_ascii_lowercase(),"directory":a.output,"invitation_file":a.output.join("invitation.json"),"qr_file":if a.qr {Some(a.output.join("invitation.svg"))}else{None},"invitation_ttl_seconds":a.invitation_ttl_seconds,"device_ttl_seconds":a.device_ttl_seconds});
    if a.dry_run {
        return Ok(receipt);
    }
    if !a.yes {
        if !std::io::stdin().is_terminal() || !std::io::stderr().is_terminal() {
            bail!("issuance needs --yes outside a terminal; use --dry-run first");
        }
        eprint!("Issue one scoped invitation (no inference)? [y/N] ");
        std::io::stderr().flush()?;
        let mut answer = String::new();
        std::io::stdin().read_line(&mut answer)?;
        if !matches!(answer.trim().to_ascii_lowercase().as_str(), "y" | "yes") {
            bail!("issuance canceled; nothing sent");
        }
    }
    let client = operator_client(a, resolve)?;
    private_directory(&a.output)?;
    let issued = issue(a, &client).await?;
    if publish(a, &issued).is_err() {
        let revoked = client
            .post(endpoint(a, "/admin/device-invitations/revoke")?)
            .json(&serde_json::json!({"token":issued.token}))
            .send()
            .await
            .is_ok_and(|response| response.status() == reqwest::StatusCode::OK);
        if revoked {
            bail!("publication failed; invitation revoked. Private partial files may remain; inspect output before removing");
        }
        bail!("publication failed and revocation is uncertain; inspect private output or let invitation expire. Do not retry automatically");
    }
    receipt["state"] = "issued".into();
    receipt["expires_at"] = issued.expires_at.into();
    Ok(receipt)
}
pub async fn run_cli(a: InviteArgs, registry_options_supplied: bool) -> Result<()> {
    let result = if registry_options_supplied {
        Err(anyhow::anyhow!("device invite does not use registry --config or --candidate-revision; select --admin-url explicitly and use --dry-run"))
    } else {
        execute(&a).await
    };
    match result {
        Ok(receipt) => {
            if a.json {
                println!("{}", serde_json::to_string(&receipt)?);
            } else {
                if let Some(expiry) = receipt["expires_at"].as_i64() {
                    println!("Invitation expires at Unix time {expiry}");
                }
                println!("{}: Oqto / pi-messages\nPrivate invitation: {}\nQR: {}\nNo inference performed. Treat the files as a short-lived secret.",receipt["state"].as_str().unwrap_or("unknown"),a.output.join("invitation.json").display(),if a.qr {a.output.join("invitation.svg").display().to_string()}else{"not requested".into()});
            }
            Ok(())
        }
        Err(error) => {
            if a.json {
                println!(
                    "{}",
                    serde_json::json!({"schema_version":1,"error":{"code":"device_invitation_failed","message":error.to_string()}})
                );
            }
            Err(error)
        }
    }
}
#[cfg(test)]
mod tests;

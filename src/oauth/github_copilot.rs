use crate::oauth::types::{OAuthCredentials, OAuthProvider};
use chrono::Utc;
use reqwest::Client;
use serde::Deserialize;

const DEVICE_CODE_URL: &str = "https://github.com/login/device/code";
const TOKEN_URL: &str = "https://github.com/login/oauth/access_token";

pub struct GitHubCopilotConfig {
    pub client_id: String,
    pub scope: String,
}

pub fn config_from_env() -> Result<GitHubCopilotConfig, String> {
    let client_id = std::env::var("EAVS_OAUTH_GITHUB_COPILOT_CLIENT_ID")
        .map_err(|_| "Missing EAVS_OAUTH_GITHUB_COPILOT_CLIENT_ID".to_string())?;
    let scope = std::env::var("EAVS_OAUTH_GITHUB_COPILOT_SCOPE")
        .unwrap_or_else(|_| "read:user".to_string());

    Ok(GitHubCopilotConfig { client_id, scope })
}

pub async fn start_device_flow(
    client: &Client,
    config: &GitHubCopilotConfig,
) -> Result<DeviceCodeResponse, String> {
    let params = vec![
        ("client_id", config.client_id.clone()),
        ("scope", config.scope.clone()),
    ];

    let resp = client
        .post(DEVICE_CODE_URL)
        .header("Accept", "application/json")
        .form(&params)
        .send()
        .await
        .map_err(|e| format!("Device code request failed: {}", e))?;

    let status = resp.status();
    let body = resp
        .json::<DeviceCodeResponse>()
        .await
        .map_err(|e| format!("Device code parse failed: {}", e))?;

    if !status.is_success() {
        return Err(format!("Device code request failed with status {}", status));
    }

    Ok(body)
}

pub async fn poll_device_flow(
    client: &Client,
    config: &GitHubCopilotConfig,
    user_id: &str,
    device_code: &str,
) -> Result<DevicePollResult, String> {
    let params = vec![
        ("client_id", config.client_id.clone()),
        (
            "grant_type",
            "urn:ietf:params:oauth:grant-type:device_code".to_string(),
        ),
        ("device_code", device_code.to_string()),
    ];

    let resp = client
        .post(TOKEN_URL)
        .header("Accept", "application/json")
        .form(&params)
        .send()
        .await
        .map_err(|e| format!("Token poll failed: {}", e))?;

    let status = resp.status();
    let body = resp
        .json::<DeviceTokenResponse>()
        .await
        .map_err(|e| format!("Token poll parse failed: {}", e))?;

    if status.is_success() {
        return Ok(DevicePollResult::Authorized(token_to_credentials(
            user_id, body,
        )));
    }

    if let Some(error) = body.error.as_deref() {
        match error {
            "authorization_pending" => {
                return Ok(DevicePollResult::Pending {
                    interval: body.interval,
                })
            }
            "slow_down" => {
                let interval = body.interval.unwrap_or(5) + 5;
                return Ok(DevicePollResult::Pending {
                    interval: Some(interval),
                });
            }
            "expired_token" => return Err("Device code expired".to_string()),
            _ => return Err(format!("Device flow error: {}", error)),
        }
    }

    Err(format!("Device flow failed with status {}", status))
}

#[derive(Debug, Deserialize)]
pub struct DeviceCodeResponse {
    pub device_code: String,
    pub user_code: String,
    pub verification_uri: String,
    #[allow(dead_code)]
    #[serde(default)]
    pub verification_uri_complete: Option<String>,
    pub expires_in: u64,
    #[serde(default)]
    pub interval: Option<u64>,
}

#[derive(Debug)]
pub enum DevicePollResult {
    Pending { interval: Option<u64> },
    Authorized(OAuthCredentials),
}

#[derive(Deserialize)]
struct DeviceTokenResponse {
    #[serde(default)]
    access_token: String,
    #[serde(default)]
    refresh_token: Option<String>,
    #[serde(default)]
    expires_in: Option<i64>,
    #[serde(default)]
    scope: Option<String>,
    #[serde(default)]
    token_type: Option<String>,
    #[serde(default)]
    error: Option<String>,
    #[serde(default)]
    interval: Option<u64>,
}

fn token_to_credentials(user_id: &str, token: DeviceTokenResponse) -> OAuthCredentials {
    let expires_at = if let Some(expires_in) = token.expires_in {
        Utc::now().timestamp() + expires_in
    } else {
        Utc::now().timestamp() + 315360000
    };

    OAuthCredentials {
        user_id: user_id.to_string(),
        provider: OAuthProvider::GithubCopilot,
        access_token: token.access_token,
        refresh_token: token.refresh_token.unwrap_or_default(),
        expires_at,
        extra_data: Some(serde_json::json!({
            "scope": token.scope,
            "token_type": token.token_type,
        })),
    }
}

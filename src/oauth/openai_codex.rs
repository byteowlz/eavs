use crate::oauth::pkce::code_challenge;
use crate::oauth::types::{OAuthCredentials, OAuthProvider};
use chrono::Utc;
use reqwest::Client;
use serde::Deserialize;

const AUTH_URL: &str = "https://auth.openai.com/oauth/authorize";
const TOKEN_URL: &str = "https://auth.openai.com/oauth/token";
const DEFAULT_CLIENT_ID: &str = "app_EMoamEEZ73f0CkXaXp7hrann";

pub struct OpenAICodexConfig {
    pub client_id: String,
    pub client_secret: Option<String>,
    pub redirect_uri: String,
    pub scope: String,
}

pub fn config_from_env(redirect_uri: String) -> Result<OpenAICodexConfig, String> {
    let client_id = std::env::var("EAVS_OAUTH_OPENAI_CLIENT_ID")
        .ok()
        .filter(|v| !v.trim().is_empty())
        .unwrap_or_else(|| DEFAULT_CLIENT_ID.to_string());
    let client_secret = std::env::var("EAVS_OAUTH_OPENAI_CLIENT_SECRET").ok();
    let scope = std::env::var("EAVS_OAUTH_OPENAI_SCOPE")
        .unwrap_or_else(|_| "openid profile email offline_access".to_string());

    Ok(OpenAICodexConfig {
        client_id,
        client_secret,
        redirect_uri,
        scope,
    })
}

pub fn build_authorize_url(
    config: &OpenAICodexConfig,
    state: &str,
    code_verifier: &str,
) -> Result<String, String> {
    let challenge = code_challenge(code_verifier)?;

    let mut url = url::Url::parse(AUTH_URL)
        .unwrap_or_else(|_| url::Url::parse("https://auth.openai.com/oauth/authorize").unwrap());
    url.query_pairs_mut()
        .append_pair("response_type", "code")
        .append_pair("client_id", &config.client_id)
        .append_pair("redirect_uri", &config.redirect_uri)
        .append_pair("scope", &config.scope)
        .append_pair("code_challenge", &challenge)
        .append_pair("code_challenge_method", "S256")
        .append_pair("state", state);

    Ok(url.to_string())
}

pub async fn exchange_code(
    client: &Client,
    config: &OpenAICodexConfig,
    user_id: &str,
    code: &str,
    code_verifier: &str,
) -> Result<OAuthCredentials, String> {
    let mut params = vec![
        ("grant_type", "authorization_code".to_string()),
        ("client_id", config.client_id.clone()),
        ("code", code.to_string()),
        ("redirect_uri", config.redirect_uri.clone()),
        ("code_verifier", code_verifier.to_string()),
    ];

    if let Some(secret) = &config.client_secret {
        params.push(("client_secret", secret.clone()));
    }

    let resp = client
        .post(TOKEN_URL)
        .form(&params)
        .send()
        .await
        .map_err(|e| format!("Token request failed: {}", e))?;

    let status = resp.status();
    let body = resp
        .json::<TokenResponse>()
        .await
        .map_err(|e| format!("Token parse failed: {}", e))?;

    if !status.is_success() {
        return Err(format!("Token exchange failed with status {}", status));
    }

    Ok(token_to_credentials(user_id, body))
}

pub async fn refresh_token(
    client: &Client,
    config: &OpenAICodexConfig,
    user_id: &str,
    refresh_token: &str,
) -> Result<OAuthCredentials, String> {
    let mut params = vec![
        ("grant_type", "refresh_token".to_string()),
        ("client_id", config.client_id.clone()),
        ("refresh_token", refresh_token.to_string()),
    ];

    if let Some(secret) = &config.client_secret {
        params.push(("client_secret", secret.clone()));
    }

    let resp = client
        .post(TOKEN_URL)
        .form(&params)
        .send()
        .await
        .map_err(|e| format!("Refresh request failed: {}", e))?;

    let status = resp.status();
    let body = resp
        .json::<TokenResponse>()
        .await
        .map_err(|e| format!("Refresh parse failed: {}", e))?;

    if !status.is_success() {
        return Err(format!("Token refresh failed with status {}", status));
    }

    Ok(token_to_credentials(user_id, body))
}

#[derive(Deserialize)]
struct TokenResponse {
    access_token: String,
    #[serde(default)]
    refresh_token: Option<String>,
    #[serde(default)]
    expires_in: Option<i64>,
    #[serde(default)]
    id_token: Option<String>,
}

fn token_to_credentials(user_id: &str, token: TokenResponse) -> OAuthCredentials {
    let expires_in = token.expires_in.unwrap_or(3600);
    let expires_at = Utc::now().timestamp() + expires_in;

    OAuthCredentials {
        user_id: user_id.to_string(),
        provider: OAuthProvider::OpenAICodex,
        access_token: token.access_token,
        refresh_token: token.refresh_token.unwrap_or_default(),
        expires_at,
        extra_data: token.id_token.map(|id| serde_json::json!({"id_token": id})),
    }
}

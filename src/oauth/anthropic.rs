use crate::oauth::pkce::code_challenge;
use crate::oauth::types::{OAuthCredentials, OAuthProvider};
use chrono::Utc;
use reqwest::Client;
use serde::Deserialize;

const AUTH_URL: &str = "https://claude.ai/oauth/authorize";
const TOKEN_URL: &str = "https://console.anthropic.com/v1/oauth/token";

pub struct AnthropicOAuthConfig {
    pub client_id: String,
    pub client_secret: Option<String>,
    pub redirect_uri: String,
    pub scope: String,
}

pub fn config_from_env(redirect_uri: String) -> Result<AnthropicOAuthConfig, String> {
    let client_id = std::env::var("EAVS_OAUTH_ANTHROPIC_CLIENT_ID")
        .map_err(|_| "Missing EAVS_OAUTH_ANTHROPIC_CLIENT_ID".to_string())?;
    let client_secret = std::env::var("EAVS_OAUTH_ANTHROPIC_CLIENT_SECRET").ok();
    let scope = std::env::var("EAVS_OAUTH_ANTHROPIC_SCOPE")
        .unwrap_or_else(|_| "org:create_api_key user:profile user:inference".to_string());

    Ok(AnthropicOAuthConfig {
        client_id,
        client_secret,
        redirect_uri,
        scope,
    })
}

pub fn build_authorize_url(
    config: &AnthropicOAuthConfig,
    state: &str,
    code_verifier: &str,
) -> Result<String, String> {
    let challenge = code_challenge(code_verifier)?;
    let mut url = url::Url::parse(AUTH_URL)
        .unwrap_or_else(|_| url::Url::parse("https://claude.ai/oauth/authorize").unwrap());
    url.query_pairs_mut()
        .append_pair("response_type", "code")
        .append_pair("client_id", &config.client_id)
        .append_pair("redirect_uri", &config.redirect_uri)
        .append_pair("scope", &config.scope)
        .append_pair("state", state)
        .append_pair("code_challenge", &challenge)
        .append_pair("code_challenge_method", "S256");
    Ok(url.to_string())
}

pub async fn exchange_code(
    client: &Client,
    config: &AnthropicOAuthConfig,
    user_id: &str,
    code: &str,
    state: &str,
    code_verifier: &str,
) -> Result<OAuthCredentials, String> {
    let mut body = serde_json::json!({
        "grant_type": "authorization_code",
        "client_id": config.client_id,
        "code": code,
        "state": state,
        "redirect_uri": config.redirect_uri,
        "code_verifier": code_verifier,
    });

    if let Some(secret) = &config.client_secret {
        body["client_secret"] = serde_json::Value::String(secret.clone());
    }

    let resp = client
        .post(TOKEN_URL)
        .header("Content-Type", "application/json")
        .json(&body)
        .send()
        .await
        .map_err(|e| format!("Token request failed: {}", e))?;

    let status = resp.status();
    let text = resp.text().await.map_err(|e| format!("Failed to read response: {}", e))?;
    
    if !status.is_success() {
        return Err(format!("Token exchange failed ({}): {}", status, text));
    }

    let token: TokenResponse = serde_json::from_str(&text)
        .map_err(|e| format!("Token parse failed: {} - body: {}", e, text))?;

    Ok(token_to_credentials(user_id, token))
}

pub async fn refresh_token(
    client: &Client,
    config: &AnthropicOAuthConfig,
    user_id: &str,
    refresh_token: &str,
) -> Result<OAuthCredentials, String> {
    let mut params = vec![
        ("grant_type", "refresh_token".to_string()),
        ("refresh_token", refresh_token.to_string()),
        ("client_id", config.client_id.clone()),
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
    token_type: Option<String>,
}

fn token_to_credentials(user_id: &str, token: TokenResponse) -> OAuthCredentials {
    let expires_in = token.expires_in.unwrap_or(3600);
    let expires_at = Utc::now().timestamp() + expires_in;

    OAuthCredentials {
        user_id: user_id.to_string(),
        provider: OAuthProvider::Anthropic,
        access_token: token.access_token,
        refresh_token: token.refresh_token.unwrap_or_default(),
        expires_at,
        extra_data: token
            .token_type
            .map(|t| serde_json::json!({"token_type": t})),
    }
}

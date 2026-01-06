use crate::oauth::pkce::code_challenge;
use crate::oauth::types::{OAuthCredentials, OAuthProvider};
use chrono::Utc;
use reqwest::Client;
use serde::Deserialize;

const AUTH_URL: &str = "https://accounts.google.com/o/oauth2/v2/auth";
const TOKEN_URL: &str = "https://oauth2.googleapis.com/token";

pub struct GoogleOAuthConfig {
    pub client_id: String,
    pub client_secret: Option<String>,
    pub redirect_uri: String,
    pub scope: String,
    pub provider: OAuthProvider,
}

pub fn config_from_env(provider: OAuthProvider, redirect_uri: String) -> Result<GoogleOAuthConfig, String> {
    let (client_id_var, secret_var, scope_var, default_scope) = match provider {
        OAuthProvider::GoogleGeminiCli => (
            "EAVS_OAUTH_GOOGLE_GEMINI_CLIENT_ID",
            "EAVS_OAUTH_GOOGLE_GEMINI_CLIENT_SECRET",
            "EAVS_OAUTH_GOOGLE_GEMINI_SCOPE",
            "https://www.googleapis.com/auth/generative-language",
        ),
        OAuthProvider::GoogleAntigravity => (
            "EAVS_OAUTH_GOOGLE_ANTIGRAVITY_CLIENT_ID",
            "EAVS_OAUTH_GOOGLE_ANTIGRAVITY_CLIENT_SECRET",
            "EAVS_OAUTH_GOOGLE_ANTIGRAVITY_SCOPE",
            "https://www.googleapis.com/auth/cloud-platform",
        ),
        _ => return Err("Unsupported Google provider".to_string()),
    };

    let client_id = std::env::var(client_id_var)
        .map_err(|_| format!("Missing {}", client_id_var))?;
    let client_secret = std::env::var(secret_var).ok();
    let scope = std::env::var(scope_var).unwrap_or_else(|_| default_scope.to_string());

    Ok(GoogleOAuthConfig {
        client_id,
        client_secret,
        redirect_uri,
        scope,
        provider,
    })
}

pub fn build_authorize_url(
    config: &GoogleOAuthConfig,
    state: &str,
    code_verifier: &str,
) -> Result<String, String> {
    let challenge = code_challenge(code_verifier)?;

    let mut url = url::Url::parse(AUTH_URL)
        .unwrap_or_else(|_| url::Url::parse("https://accounts.google.com/o/oauth2/v2/auth").unwrap());
    url.query_pairs_mut()
        .append_pair("response_type", "code")
        .append_pair("client_id", &config.client_id)
        .append_pair("redirect_uri", &config.redirect_uri)
        .append_pair("scope", &config.scope)
        .append_pair("code_challenge", &challenge)
        .append_pair("code_challenge_method", "S256")
        .append_pair("access_type", "offline")
        .append_pair("prompt", "consent")
        .append_pair("state", state);

    Ok(url.to_string())
}

pub async fn exchange_code(
    client: &Client,
    config: &GoogleOAuthConfig,
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

    Ok(token_to_credentials(user_id, config.provider, body))
}

pub async fn refresh_token(
    client: &Client,
    config: &GoogleOAuthConfig,
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

    Ok(token_to_credentials(user_id, config.provider, body))
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
    #[serde(default)]
    scope: Option<String>,
    #[serde(default)]
    token_type: Option<String>,
}

fn token_to_credentials(
    user_id: &str,
    provider: OAuthProvider,
    token: TokenResponse,
) -> OAuthCredentials {
    let expires_in = token.expires_in.unwrap_or(3600);
    let expires_at = Utc::now().timestamp() + expires_in;

    OAuthCredentials {
        user_id: user_id.to_string(),
        provider,
        access_token: token.access_token,
        refresh_token: token.refresh_token.unwrap_or_default(),
        expires_at,
        extra_data: Some(serde_json::json!({
            "id_token": token.id_token,
            "scope": token.scope,
            "token_type": token.token_type,
        })),
    }
}

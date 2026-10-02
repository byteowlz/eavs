use crate::oauth::pkce::code_challenge;
use crate::oauth::types::{OAuthCredentials, OAuthProvider};
use chrono::Utc;
use reqwest::Client;
use serde::Deserialize;

const AUTH_URL: &str = "https://claude.ai/oauth/authorize";
const TOKEN_URL: &str = "https://platform.claude.com/v1/oauth/token";
const DEFAULT_CLIENT_ID: &str = "9d1c250a-e61b-44d9-88ed-5944d1962f5e";
const DEFAULT_REDIRECT_URI: &str = "https://console.anthropic.com/oauth/code/callback";

pub struct AnthropicOAuthConfig {
    pub client_id: String,
    pub client_secret: Option<String>,
    pub redirect_uri: String,
    pub scope: String,
}

pub fn config_from_env(redirect_uri: String) -> Result<AnthropicOAuthConfig, String> {
    let client_id = std::env::var("EAVS_OAUTH_ANTHROPIC_CLIENT_ID")
        .ok()
        .filter(|v| !v.trim().is_empty())
        .unwrap_or_else(|| DEFAULT_CLIENT_ID.to_string());
    let client_secret = std::env::var("EAVS_OAUTH_ANTHROPIC_CLIENT_SECRET").ok();
    let scope = std::env::var("EAVS_OAUTH_ANTHROPIC_SCOPE").unwrap_or_else(|_| {
        "org:create_api_key user:profile user:inference user:sessions:claude_code \
         user:mcp_servers user:file_upload"
            .to_string()
    });
    let redirect_uri = if redirect_uri.trim().is_empty() {
        DEFAULT_REDIRECT_URI.to_string()
    } else {
        redirect_uri
    };

    Ok(AnthropicOAuthConfig {
        client_id,
        client_secret,
        redirect_uri,
        scope,
    })
}

pub fn default_redirect_uri() -> String {
    DEFAULT_REDIRECT_URI.to_string()
}

pub fn build_authorize_url(
    config: &AnthropicOAuthConfig,
    state: &str,
    code_verifier: &str,
) -> Result<String, String> {
    let challenge = code_challenge(code_verifier)?;
    let mut url = url::Url::parse(AUTH_URL)
        .map_err(|err| format!("invalid Anthropic authorization URL: {err}"))?;
    url.query_pairs_mut()
        .append_pair("code", "true")
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
        .map_err(|e| format!("Token request failed: {e}"))?;

    let status = resp.status();
    let text = resp
        .text()
        .await
        .map_err(|e| format!("Failed to read response: {e}"))?;

    if !status.is_success() {
        return Err(format!("Token exchange failed ({status}): {text}"));
    }

    let token: TokenResponse = serde_json::from_str(&text)
        .map_err(|e| format!("Token parse failed: {e} - body: {text}"))?;

    Ok(token_to_credentials(user_id, token))
}

pub async fn refresh_token(
    client: &Client,
    config: &AnthropicOAuthConfig,
    user_id: &str,
    refresh_token: &str,
) -> Result<OAuthCredentials, String> {
    let mut body = serde_json::json!({
        "grant_type": "refresh_token",
        "refresh_token": refresh_token,
        "client_id": config.client_id,
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
        .map_err(|e| format!("Refresh request failed: {e}"))?;

    let status = resp.status();
    let text = resp
        .text()
        .await
        .map_err(|e| format!("Failed to read refresh response: {e}"))?;

    if !status.is_success() {
        return Err(format!("Token refresh failed ({status}): {text}"));
    }

    let token: TokenResponse = serde_json::from_str(&text)
        .map_err(|e| format!("Refresh parse failed: {e} - body: {text}"))?;

    Ok(token_to_credentials(user_id, token))
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
        account_label: "default".to_string(),
        access_token: token.access_token,
        refresh_token: token.refresh_token.unwrap_or_default(),
        expires_at,
        extra_data: token
            .token_type
            .map(|t| serde_json::json!({"token_type": t})),
        provenance: crate::oauth::types::CredentialProvenance::Owned,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn authorization_url_retains_oauth_parameters() -> Result<(), String> {
        let config = AnthropicOAuthConfig {
            client_id: "client id".to_string(),
            client_secret: None,
            redirect_uri: "http://localhost/callback".to_string(),
            scope: "scope one".to_string(),
        };
        let authorized = build_authorize_url(&config, "state value", "verifier")?;
        let url = url::Url::parse(&authorized).map_err(|err| err.to_string())?;
        assert_eq!(url.host_str(), Some("claude.ai"));
        let pairs: std::collections::HashMap<_, _> = url.query_pairs().into_owned().collect();
        assert_eq!(
            pairs.get("client_id").map(String::as_str),
            Some("client id")
        );
        assert_eq!(pairs.get("state").map(String::as_str), Some("state value"));
        assert_eq!(
            pairs.get("code_challenge_method").map(String::as_str),
            Some("S256")
        );
        assert_eq!(
            pairs.get("code_challenge").map(String::as_str),
            Some(code_challenge("verifier")?.as_str())
        );
        Ok(())
    }
}

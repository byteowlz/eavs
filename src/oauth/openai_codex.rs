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
        .map_err(|err| format!("invalid OpenAI Codex authorization URL: {err}"))?;
    url.query_pairs_mut()
        .append_pair("response_type", "code")
        .append_pair("client_id", &config.client_id)
        .append_pair("redirect_uri", &config.redirect_uri)
        .append_pair("scope", &config.scope)
        .append_pair("code_challenge", &challenge)
        .append_pair("code_challenge_method", "S256")
        .append_pair("state", state)
        // Extra params the Codex login flow expects (match pi/codex-cli):
        .append_pair("id_token_add_organizations", "true")
        .append_pair("codex_cli_simplified_flow", "true")
        .append_pair("originator", "codex_cli_rs");

    Ok(url.to_string())
}

pub async fn exchange_code(
    client: &Client,
    config: &OpenAICodexConfig,
    user_id: &str,
    code: &str,
    code_verifier: &str,
) -> Result<OAuthCredentials, String> {
    exchange_code_at(client, config, user_id, code, code_verifier, TOKEN_URL).await
}

// Private injection seam: production callers cannot override the token endpoint.
async fn exchange_code_at(
    client: &Client,
    config: &OpenAICodexConfig,
    user_id: &str,
    code: &str,
    code_verifier: &str,
    endpoint: &str,
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
        .post(endpoint)
        .form(&params)
        .send()
        .await
        .map_err(|e| format!("Token request failed: {e}"))?;

    let status = resp.status();
    if !status.is_success() {
        return Err(format!("Token exchange failed with status {status}"));
    }
    let body = resp
        .json::<TokenResponse>()
        .await
        .map_err(|e| format!("Token parse failed: {e}"))?;

    Ok(token_to_credentials(user_id, body))
}

pub async fn refresh_token(
    client: &Client,
    config: &OpenAICodexConfig,
    user_id: &str,
    refresh_token: &str,
) -> Result<OAuthCredentials, String> {
    refresh_token_at(client, config, user_id, refresh_token, TOKEN_URL).await
}

async fn refresh_token_at(
    client: &Client,
    config: &OpenAICodexConfig,
    user_id: &str,
    refresh_token: &str,
    endpoint: &str,
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
        .post(endpoint)
        .form(&params)
        .send()
        .await
        .map_err(|e| format!("Refresh request failed: {e}"))?;

    let status = resp.status();
    if !status.is_success() {
        return Err(format!("Token refresh failed with status {status}"));
    }
    let mut body = resp
        .json::<TokenResponse>()
        .await
        .map_err(|e| format!("Refresh parse failed: {e}"))?;
    // OAuth refresh responses may omit a replacement refresh token.
    if body.refresh_token.is_none() {
        body.refresh_token = Some(refresh_token.to_string());
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
        account_label: "default".to_string(),
        access_token: token.access_token,
        refresh_token: token.refresh_token.unwrap_or_default(),
        expires_at,
        extra_data: token.id_token.map(|id| serde_json::json!({"id_token": id})),
        provenance: crate::oauth::types::CredentialProvenance::Owned,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn mocked_exchange_and_refresh() -> Result<(), Box<dyn std::error::Error>> {
        use axum::{routing::post, Form, Json, Router};
        use std::collections::HashMap;
        let app = Router::new().route("/token", post(|Form(params): Form<HashMap<String, String>>| async move {
            assert_eq!(params.get("client_id").map(String::as_str), Some("fixture-client"));
            assert_eq!(params.get("client_secret").map(String::as_str), Some("fixture-secret"));
            match params.get("grant_type").map(String::as_str) {
                Some("authorization_code") => {
                    assert_eq!(params.get("code").map(String::as_str), Some("fixture-code"));
                    assert_eq!(params.get("code_verifier").map(String::as_str), Some("fixture-verifier"));
                    assert_eq!(params.get("redirect_uri").map(String::as_str), Some("http://localhost/callback"));
                    Json(serde_json::json!({"access_token":"access", "refresh_token":"refresh", "expires_in":60, "id_token":"id"}))
                }
                Some("refresh_token") => {
                    assert_eq!(params.get("refresh_token").map(String::as_str), Some("refresh"));
                    Json(serde_json::json!({"access_token":"renewed", "expires_in":120}))
                }
                _ => panic!("unexpected grant"),
            }
        })).route("/error", post(|| async { (axum::http::StatusCode::UNAUTHORIZED, "not token JSON") }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let base = format!("http://{}", listener.local_addr()?);
        let server = tokio::spawn(async move { axum::serve(listener, app).await });
        let client = Client::builder()
            .no_proxy()
            .timeout(std::time::Duration::from_secs(2))
            .build()?;
        let config = OpenAICodexConfig {
            client_id: "fixture-client".to_string(),
            client_secret: Some("fixture-secret".to_string()),
            redirect_uri: "http://localhost/callback".to_string(),
            scope: "scope".to_string(),
        };
        let credentials = exchange_code_at(
            &client,
            &config,
            "user",
            "fixture-code",
            "fixture-verifier",
            &format!("{base}/token"),
        )
        .await?;
        assert_eq!(credentials.access_token, "access");
        assert_eq!(credentials.refresh_token, "refresh");
        assert_eq!(credentials.user_id, "user");
        assert_eq!(
            credentials.extra_data,
            Some(serde_json::json!({"id_token":"id"}))
        );
        let refreshed = refresh_token_at(
            &client,
            &config,
            "user",
            "refresh",
            &format!("{base}/token"),
        )
        .await?;
        assert_eq!(refreshed.access_token, "renewed");
        assert_eq!(refreshed.refresh_token, "refresh");
        assert!(refreshed.expires_at >= Utc::now().timestamp() + 115);
        let error = exchange_code_at(
            &client,
            &config,
            "user",
            "code",
            "verifier",
            &format!("{base}/error"),
        )
        .await;
        assert!(error.is_err_and(|error| error.contains("401")));
        let error = refresh_token_at(
            &client,
            &config,
            "user",
            "refresh",
            &format!("{base}/error"),
        )
        .await;
        assert!(error.is_err_and(|error| error.contains("401")));
        server.abort();
        Ok(())
    }

    #[test]
    fn authorization_url_retains_oauth_parameters() -> Result<(), String> {
        let config = OpenAICodexConfig {
            client_id: "client id".to_string(),
            client_secret: None,
            redirect_uri: "http://localhost/callback".to_string(),
            scope: "scope one".to_string(),
        };
        let authorized = build_authorize_url(&config, "state value", "verifier")?;
        let url = url::Url::parse(&authorized).map_err(|err| err.to_string())?;
        assert_eq!(url.host_str(), Some("auth.openai.com"));
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

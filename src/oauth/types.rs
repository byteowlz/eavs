use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OAuthCredentials {
    pub user_id: String,
    pub provider: OAuthProvider,
    /// Account label for multi-account support.
    /// Multiple accounts for the same provider are distinguished by this label.
    /// Defaults to "default" for single-account usage.
    #[serde(default = "default_account_label")]
    pub account_label: String,
    pub access_token: String,
    pub refresh_token: String,
    pub expires_at: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extra_data: Option<serde_json::Value>,
}

fn default_account_label() -> String {
    "default".to_string()
}

impl OAuthCredentials {
    pub fn is_expired(&self, leeway_secs: i64) -> bool {
        let now = chrono::Utc::now().timestamp();
        now + leeway_secs >= self.expires_at
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum OAuthProvider {
    Anthropic,
    GithubCopilot,
    OpenAICodex,
}

impl OAuthProvider {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Anthropic => "anthropic",
            Self::GithubCopilot => "github-copilot",
            Self::OpenAICodex => "openai-codex",
        }
    }

    pub fn from_str(value: &str) -> Option<Self> {
        match value.to_lowercase().as_str() {
            "anthropic" => Some(Self::Anthropic),
            "github-copilot" | "copilot" => Some(Self::GithubCopilot),
            "openai-codex" | "openai" | "chatgpt" => Some(Self::OpenAICodex),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OAuthLoginResponse {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auth_url: Option<String>,
    pub instructions: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verification_uri: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user_code: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub device_code: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub interval: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_in: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code_verifier: Option<String>,
}

#[derive(Debug, Clone)]
pub struct OAuthPendingAuth {
    pub user_id: String,
    pub provider: OAuthProvider,
    pub code_verifier: Option<String>,
    pub redirect_uri: Option<String>,
    #[allow(dead_code)]
    pub extra_data: Option<serde_json::Value>,
    #[allow(dead_code)]
    pub created_at: i64,
}

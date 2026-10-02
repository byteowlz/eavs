use serde::{Deserialize, Serialize};

/// Who owns a credential and may refresh / persist it.
///
/// Eavs only ever creates `Owned` credentials through its own login flow and
/// stores them in its own SQLite/keychain store. There is currently **no**
/// import of foreign Codex/Claude credential files; this marker is a
/// future-proof guard so that any credential explicitly marked `Borrowed` is
/// never refreshed or written back by eavs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum CredentialProvenance {
    /// Eavs owns this credential; eavs may refresh and persist it.
    Owned,
    /// Borrowed from an external owner CLI (e.g. `codex` or `claude`). Eavs
    /// must never refresh it or write to the owner's file; on expiry it returns
    /// an actionable error naming the owner instead.
    Borrowed { owner: String },
}

impl Default for CredentialProvenance {
    fn default() -> Self {
        CredentialProvenance::Owned
    }
}

impl CredentialProvenance {
    /// True when eavs owns the credential and may refresh it.
    pub fn is_owned(&self) -> bool {
        matches!(self, CredentialProvenance::Owned)
    }
}

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
    /// Backward-compatible ownership marker. Existing rows without this field
    /// deserialize to `Owned`, so legacy SQLite/keychain data is unaffected.
    #[serde(default, skip_serializing_if = "CredentialProvenance::is_owned")]
    pub provenance: CredentialProvenance,
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
    pub const fn as_str(&self) -> &'static str {
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

/// Typed error for OAuth access-token resolution in the proxy.
///
/// Carrying structure instead of a bare string lets callers distinguish a
/// borrowed-token expiry (which must be fixed by the owner CLI) from a
/// refreshable eavs-owned token or a plain missing/store failure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OAuthResolveError {
    /// The provider is not configured for OAuth resolution.
    UnsupportedProvider(String),
    /// The OAuth store was not initialized in app state.
    StoreNotInitialized,
    /// Credential store read failed.
    Store(String),
    /// No credential found for the requested user/account.
    CredentialsNotFound {
        /// The account label that was requested, when not "default".
        account: Option<String>,
    },
    /// Eavs-owned credential expired with no refresh token available.
    ExpiredNoRefresh,
    /// A borrowed credential expired; it must be refreshed by its owner.
    ExpiredBorrowed {
        /// The owner CLI that must refresh the token (e.g. `codex`, `claude`).
        owner: String,
        provider: OAuthProvider,
    },
    /// An owned credential's refresh (or its store write-back) failed.
    RefreshFailed(String),
}

impl std::fmt::Display for OAuthResolveError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnsupportedProvider(msg) => write!(f, "{msg}"),
            Self::StoreNotInitialized => write!(f, "OAuth store not initialized"),
            Self::Store(msg) => write!(f, "OAuth store error: {msg}"),
            Self::CredentialsNotFound { account } => match account {
                Some(label) => write!(
                    f,
                    "OAuth credentials not found for account '{label}'; a new eavs login is required"
                ),
                None => write!(
                    f,
                    "OAuth credentials not found; a new eavs login is required"
                ),
            },
            Self::ExpiredNoRefresh => write!(
                f,
                "OAuth token expired and no refresh token available; re-run the eavs login flow"
            ),
            Self::ExpiredBorrowed { owner, provider } => write!(
                f,
                "OAuth token for {} is expired. It is borrowed from '{owner}' and eavs must not refresh it; refresh it via the owning '{owner}' CLI, then retry.",
                provider.as_str()
            ),
            Self::RefreshFailed(msg) => write!(f, "OAuth token refresh failed: {msg}"),
        }
    }
}

impl std::error::Error for OAuthResolveError {}

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn legacy_credentials_default_to_owned() {
        // A stored credential that predates the provenance field must deserialise
        // as Owned (backward compatibility), never Borrowed.
        let json = serde_json::json!({
            "user_id": "u",
            "provider": "anthropic",
            "account_label": "default",
            "access_token": "at",
            "refresh_token": "rt",
            "expires_at": 123,
            "extra_data": null,
        });
        let creds: OAuthCredentials = serde_json::from_value(json).unwrap();
        assert_eq!(creds.provenance, CredentialProvenance::Owned);
    }

    #[test]
    fn borrowed_provenance_roundtrips_json() {
        let creds = OAuthCredentials {
            user_id: "u".to_string(),
            provider: OAuthProvider::OpenAICodex,
            account_label: "default".to_string(),
            access_token: "at".to_string(),
            refresh_token: "rt".to_string(),
            expires_at: 123,
            extra_data: None,
            provenance: CredentialProvenance::Borrowed {
                owner: "codex".to_string(),
            },
        };
        let round = serde_json::to_string(&creds).unwrap();
        let back: OAuthCredentials = serde_json::from_str(&round).unwrap();
        assert_eq!(back.provenance, creds.provenance);
    }

    #[test]
    fn expired_borrowed_error_names_owner() {
        let err = OAuthResolveError::ExpiredBorrowed {
            owner: "claude".to_string(),
            provider: OAuthProvider::Anthropic,
        };
        let msg = err.to_string();
        assert!(msg.contains("claude"), "msg={msg}");
        assert!(msg.contains("must not refresh"), "msg={msg}");
    }
}

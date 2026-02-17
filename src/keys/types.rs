//! Core types for virtual API keys.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;

/// A virtual API key with associated permissions and limits.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VirtualKey {
    /// Human-readable key identifier (e.g., "cold-lamp", "blue-frog")
    pub key_id: String,

    /// Hash of the key for secure storage (SHA-256)
    pub key_hash: String,

    /// Human-readable name for this key
    pub name: Option<String>,

    /// When the key was created
    pub created_at: DateTime<Utc>,

    /// When the key expires (None = never)
    pub expires_at: Option<DateTime<Utc>>,

    /// When the key becomes valid (None = immediately)
    pub valid_after: Option<DateTime<Utc>>,

    /// Whether the key is disabled
    pub disabled: bool,

    /// Key permissions and scopes
    pub permissions: KeyPermissions,

    /// Usage tracking
    pub usage: KeyUsage,

    /// Arbitrary metadata
    #[serde(default)]
    pub metadata: serde_json::Value,

    /// Optional OAuth user binding for token-based auth
    pub oauth_user: Option<String>,
    /// Optional OAuth account label for multi-account support (defaults to "default")
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub oauth_account: Option<String>,
}

/// Permissions and scopes for a virtual key.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct KeyPermissions {
    // ===== Model Access =====
    /// Allowed model patterns (glob syntax: "gpt-*", "claude-3-*")
    /// None = all models allowed
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub allowed_models: Option<HashSet<String>>,

    /// Blocked model patterns (takes precedence over allowed)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blocked_models: Option<HashSet<String>>,

    // ===== Provider Access =====
    /// Allowed providers (e.g., ["openai", "anthropic"])
    /// None = all providers allowed
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub allowed_providers: Option<HashSet<String>>,

    // ===== Rate Limits =====
    /// Maximum requests per minute
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rpm_limit: Option<u32>,

    /// Maximum tokens per minute (input + output combined)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tpm_limit: Option<u32>,

    /// Maximum requests per day
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rpd_limit: Option<u32>,

    // ===== Budget Limits =====
    /// Maximum budget in USD
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_budget_usd: Option<f64>,

    /// Budget reset window
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub budget_window: Option<BudgetWindow>,
}

/// Budget reset window options.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
#[derive(Default)]
pub enum BudgetWindow {
    /// Never resets - lifetime budget
    Total,
    /// Resets daily at midnight UTC
    Daily,
    /// Resets weekly on Sunday midnight UTC
    Weekly,
    /// Resets monthly on 1st at midnight UTC
    #[default]
    Monthly,
}

/// Usage tracking for a virtual key.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct KeyUsage {
    /// Total requests made
    pub total_requests: u64,

    /// Total input tokens consumed
    pub total_input_tokens: u64,

    /// Total output tokens consumed
    pub total_output_tokens: u64,

    /// Total spend in USD (approximate)
    pub total_spend_usd: f64,

    /// Spend in current budget window
    pub window_spend_usd: f64,

    /// When the current budget window started
    pub window_start: Option<DateTime<Utc>>,

    /// Last request timestamp
    pub last_request_at: Option<DateTime<Utc>>,
}

/// Request to create a new virtual key.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CreateKeyRequest {
    /// Human-readable name
    pub name: Option<String>,

    /// Expiration time
    pub expires_at: Option<DateTime<Utc>>,

    /// Permissions
    #[serde(default)]
    pub permissions: KeyPermissions,

    /// Arbitrary metadata
    #[serde(default)]
    pub metadata: serde_json::Value,

    /// Optional OAuth user binding
    pub oauth_user: Option<String>,
    /// Optional OAuth account label for multi-account support
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub oauth_account: Option<String>,
}

/// Response after creating a key (includes the actual key value).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateKeyResponse {
    /// The actual key value (only returned once!)
    pub key: String,

    /// Human-readable key ID (e.g., "cold-lamp")
    pub key_id: String,

    /// Hash of the key for lookups
    pub key_hash: String,

    /// Human-readable name
    pub name: Option<String>,

    /// When the key was created
    pub created_at: DateTime<Utc>,

    /// When the key expires
    pub expires_at: Option<DateTime<Utc>>,

    /// Permissions summary
    pub permissions: KeyPermissions,

    /// Optional OAuth user binding
    pub oauth_user: Option<String>,
    /// Optional OAuth account label
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub oauth_account: Option<String>,
}

/// Key info for listing (does not include the actual key).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KeyInfo {
    /// Human-readable key ID (e.g., "cold-lamp")
    pub key_id: String,

    /// Hash of the key for operations
    pub key_hash: String,

    /// Human-readable name
    pub name: Option<String>,

    /// When the key was created
    pub created_at: DateTime<Utc>,

    /// When the key expires
    pub expires_at: Option<DateTime<Utc>>,

    /// Whether the key is disabled
    pub disabled: bool,

    /// Permissions
    pub permissions: KeyPermissions,

    /// Usage stats
    pub usage: KeyUsage,

    /// Optional OAuth user binding
    pub oauth_user: Option<String>,
    /// Optional OAuth account label for multi-account support
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub oauth_account: Option<String>,
}

impl VirtualKey {
    /// Create a new virtual key with the given ID, hash, and permissions.
    #[allow(dead_code)]
    pub fn new(key_id: String, key_hash: String, permissions: KeyPermissions) -> Self {
        Self {
            key_id,
            key_hash,
            name: None,
            created_at: Utc::now(),
            expires_at: None,
            valid_after: None,
            disabled: false,
            permissions,
            usage: KeyUsage::default(),
            metadata: serde_json::Value::Null,
            oauth_user: None,
            oauth_account: None,
        }
    }

    /// Check if the key is currently valid (not expired, not before valid_after).
    pub fn is_valid(&self) -> bool {
        if self.disabled {
            return false;
        }

        let now = Utc::now();

        if let Some(expires) = self.expires_at {
            if now >= expires {
                return false;
            }
        }

        if let Some(valid_after) = self.valid_after {
            if now < valid_after {
                return false;
            }
        }

        true
    }

    /// Check if the key has exceeded its budget.
    pub fn is_over_budget(&self) -> bool {
        if let Some(max_budget) = self.permissions.max_budget_usd {
            self.usage.window_spend_usd >= max_budget
        } else {
            false
        }
    }

    /// Get the key ID for display.
    ///
    /// Since key_id is now a human-readable ID (e.g., "cold-lamp"),
    /// we return it as-is without masking.
    pub fn display_key_id(&self) -> String {
        self.key_id.clone()
    }

    /// Convert to KeyInfo for listing.
    pub fn to_info(&self) -> KeyInfo {
        KeyInfo {
            key_id: self.display_key_id(),
            key_hash: self.key_hash.clone(),
            name: self.name.clone(),
            created_at: self.created_at,
            expires_at: self.expires_at,
            disabled: self.disabled,
            permissions: self.permissions.clone(),
            usage: self.usage.clone(),
            oauth_user: self.oauth_user.clone(),
            oauth_account: self.oauth_account.clone(),
        }
    }
}

impl KeyPermissions {
    /// Check if a model is allowed by this key's permissions.
    pub fn is_model_allowed(&self, model: &str) -> bool {
        // Check blocked patterns first (they take precedence)
        if let Some(ref blocked) = self.blocked_models {
            for pattern in blocked {
                if glob_match::glob_match(pattern, model) {
                    return false;
                }
            }
        }

        // Check allowed patterns
        if let Some(ref allowed) = self.allowed_models {
            for pattern in allowed {
                if glob_match::glob_match(pattern, model) {
                    return true;
                }
            }
            // If allowed list is specified but model doesn't match any, deny
            return false;
        }

        // No allowed list = all models allowed (unless blocked)
        true
    }

    /// Check if a provider is allowed by this key's permissions.
    pub fn is_provider_allowed(&self, provider: &str) -> bool {
        if let Some(ref allowed) = self.allowed_providers {
            allowed.contains(provider) || allowed.contains(&provider.to_lowercase())
        } else {
            true
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_model_allowed_no_restrictions() {
        let perms = KeyPermissions::default();
        assert!(perms.is_model_allowed("gpt-4"));
        assert!(perms.is_model_allowed("claude-3-opus"));
    }

    #[test]
    fn test_model_allowed_with_allowlist() {
        let mut perms = KeyPermissions::default();
        perms.allowed_models = Some(["gpt-*".to_string(), "claude-3-*".to_string()].into());

        assert!(perms.is_model_allowed("gpt-4"));
        assert!(perms.is_model_allowed("gpt-4-turbo"));
        assert!(perms.is_model_allowed("claude-3-opus"));
        assert!(!perms.is_model_allowed("gemini-pro"));
        assert!(!perms.is_model_allowed("mistral-large"));
    }

    #[test]
    fn test_model_blocked_takes_precedence() {
        let mut perms = KeyPermissions::default();
        perms.allowed_models = Some(["gpt-*".to_string()].into());
        perms.blocked_models = Some(["gpt-4-turbo".to_string()].into());

        assert!(perms.is_model_allowed("gpt-4"));
        assert!(!perms.is_model_allowed("gpt-4-turbo"));
    }

    #[test]
    fn test_provider_allowed() {
        let mut perms = KeyPermissions::default();
        perms.allowed_providers = Some(["openai".to_string(), "anthropic".to_string()].into());

        assert!(perms.is_provider_allowed("openai"));
        assert!(perms.is_provider_allowed("anthropic"));
        assert!(!perms.is_provider_allowed("google"));
    }

    #[test]
    fn test_key_validity() {
        let mut key = VirtualKey::new(
            "eavs-test".to_string(),
            "hash".to_string(),
            KeyPermissions::default(),
        );

        assert!(key.is_valid());

        key.disabled = true;
        assert!(!key.is_valid());

        key.disabled = false;
        key.expires_at = Some(Utc::now() - chrono::Duration::hours(1));
        assert!(!key.is_valid());
    }

    #[test]
    fn test_display_key_id() {
        let key = VirtualKey::new(
            "cold-lamp".to_string(),
            "hash".to_string(),
            KeyPermissions::default(),
        );

        // Human-readable IDs are returned as-is
        assert_eq!(key.display_key_id(), "cold-lamp");
    }
}

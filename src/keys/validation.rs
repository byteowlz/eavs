//! Key validation for incoming requests.
//!
//! This module provides the main validation logic that checks:
//! - Key existence and validity
//! - Model/provider permissions
//! - Rate limits
//! - Budget limits

use crate::keys::generation::is_virtual_key;
use crate::keys::rate_limit::{RateLimitError, RateLimiter};
use crate::keys::store::KeyStore;

use std::sync::Arc;

/// Key validator that performs all validation checks.
pub struct KeyValidator {
    store: Arc<KeyStore>,
    rate_limiter: Arc<RateLimiter>,
}

impl KeyValidator {
    pub fn new(store: Arc<KeyStore>, rate_limiter: Arc<RateLimiter>) -> Self {
        Self {
            store,
            rate_limiter,
        }
    }

    /// Validate an API key for a request.
    ///
    /// Returns the validated key if successful, or an error describing why
    /// validation failed.
    pub async fn validate(
        &self,
        api_key: &str,
        model: &str,
        provider: &str,
        estimated_tokens: Option<u32>,
    ) -> Result<ValidatedKey, KeyValidationError> {
        // Check if it's a virtual key
        if !is_virtual_key(api_key) {
            return Err(KeyValidationError::NotVirtualKey);
        }

        // Look up the key
        let key = self
            .store
            .get_by_key(api_key)
            .ok_or(KeyValidationError::InvalidKey)?;

        // Check if key is valid (not expired, not disabled, etc.)
        if !key.is_valid() {
            if key.disabled {
                return Err(KeyValidationError::KeyDisabled);
            }
            if let Some(expires) = key.expires_at {
                if chrono::Utc::now() >= expires {
                    return Err(KeyValidationError::KeyExpired);
                }
            }
            return Err(KeyValidationError::InvalidKey);
        }

        // Check model permissions
        if !key.permissions.is_model_allowed(model) {
            return Err(KeyValidationError::ModelNotAllowed {
                model: model.to_string(),
            });
        }

        // Check provider permissions
        if !key.permissions.is_provider_allowed(provider) {
            return Err(KeyValidationError::ProviderNotAllowed {
                provider: provider.to_string(),
            });
        }

        // Check budget
        if key.is_over_budget() {
            return Err(KeyValidationError::BudgetExceeded {
                limit: key.permissions.max_budget_usd.unwrap_or(0.0),
                used: key.usage.window_spend_usd,
            });
        }

        // Check rate limits
        self.rate_limiter
            .check_request(
                &key.key_hash,
                key.permissions.rpm_limit,
                key.permissions.rpd_limit,
            )
            .map_err(KeyValidationError::RateLimited)?;

        // Check token limits if estimated tokens provided
        if let Some(tokens) = estimated_tokens {
            self.rate_limiter
                .check_tokens(&key.key_hash, tokens, key.permissions.tpm_limit)
                .map_err(KeyValidationError::RateLimited)?;
        }

        Ok(ValidatedKey {
            key_hash: key.key_hash.clone(),
            key_id: key.key_id.clone(),
            name: key.name.clone(),
            permissions: key.permissions.clone(),
            oauth_user: key.oauth_user.clone(),
            oauth_account: key.oauth_account.clone(),
        })
    }

    /// Record usage after a request completes.
    #[allow(clippy::too_many_arguments)]
    pub async fn record_usage(
        &self,
        key_hash: &str,
        input_tokens: u32,
        output_tokens: u32,
        cached_tokens: u32,
        cache_write_tokens: u32,
        cost_usd: f64,
        model: &str,
        provider: &str,
    ) {
        // Update rate limiter with actual token usage
        if let Some(key) = self.store.get_by_hash(key_hash) {
            self.rate_limiter.record_tokens(
                key_hash,
                input_tokens + output_tokens,
                key.permissions.tpm_limit,
            );
        }

        // Update store (async, non-blocking)
        if let Err(e) = self
            .store
            .update_usage(
                key_hash,
                input_tokens,
                output_tokens,
                cached_tokens,
                cache_write_tokens,
                cost_usd,
                model,
                provider,
            )
            .await
        {
            tracing::warn!("Failed to update usage for key {}: {}", key_hash, e);
        }
    }

    /// Record a request without usage data (for providers that don't return usage).
    ///
    /// This updates last_request_at even when we don't have token counts.
    pub async fn record_request(&self, key_hash: &str, model: &str, provider: &str) {
        if let Err(e) = self
            .store
            .update_usage(key_hash, 0, 0, 0, 0, 0.0, model, provider)
            .await
        {
            tracing::warn!("Failed to record request for key {}: {}", key_hash, e);
        }
    }
}

/// A successfully validated key with relevant info for the request.
#[allow(dead_code)]
#[derive(Debug, Clone)]
pub struct ValidatedKey {
    /// Hash of the key (for tracking)
    pub key_hash: String,
    /// Key ID (for logging, masked)
    pub key_id: String,
    /// Key name if set
    pub name: Option<String>,
    /// Key permissions
    pub permissions: crate::keys::types::KeyPermissions,

    /// Optional OAuth user binding
    pub oauth_user: Option<String>,
    /// Optional OAuth account label for multi-account support
    pub oauth_account: Option<String>,
}

/// Errors that can occur during key validation.
#[derive(Debug, Clone)]
pub enum KeyValidationError {
    /// The provided key doesn't look like a virtual key
    NotVirtualKey,
    /// The key doesn't exist or is invalid
    InvalidKey,
    /// The key has been disabled
    KeyDisabled,
    /// The key has expired
    KeyExpired,
    /// The requested model is not allowed
    ModelNotAllowed { model: String },
    /// The requested provider is not allowed
    ProviderNotAllowed { provider: String },
    /// The key's budget has been exceeded
    BudgetExceeded { limit: f64, used: f64 },
    /// Rate limit exceeded
    RateLimited(RateLimitError),
}

impl std::fmt::Display for KeyValidationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotVirtualKey => write!(f, "Not a virtual API key"),
            Self::InvalidKey => write!(f, "Invalid API key"),
            Self::KeyDisabled => write!(f, "API key has been disabled"),
            Self::KeyExpired => write!(f, "API key has expired"),
            Self::ModelNotAllowed { model } => {
                write!(f, "Model '{}' is not allowed for this key", model)
            }
            Self::ProviderNotAllowed { provider } => {
                write!(f, "Provider '{}' is not allowed for this key", provider)
            }
            Self::BudgetExceeded { limit, used } => {
                write!(
                    f,
                    "Budget exceeded: ${:.4} used of ${:.4} limit",
                    used, limit
                )
            }
            Self::RateLimited(e) => write!(f, "{}", e),
        }
    }
}

impl std::error::Error for KeyValidationError {}

impl KeyValidationError {
    /// Get the HTTP status code for this error.
    pub fn status_code(&self) -> u16 {
        match self {
            Self::NotVirtualKey => 401,
            Self::InvalidKey => 401,
            Self::KeyDisabled => 401,
            Self::KeyExpired => 401,
            Self::ModelNotAllowed { .. } => 403,
            Self::ProviderNotAllowed { .. } => 403,
            Self::BudgetExceeded { .. } => 429,
            Self::RateLimited(_) => 429,
        }
    }

    /// Get a machine-readable error code.
    pub fn error_code(&self) -> &'static str {
        match self {
            Self::NotVirtualKey => "invalid_api_key",
            Self::InvalidKey => "invalid_api_key",
            Self::KeyDisabled => "key_disabled",
            Self::KeyExpired => "key_expired",
            Self::ModelNotAllowed { .. } => "model_not_allowed",
            Self::ProviderNotAllowed { .. } => "provider_not_allowed",
            Self::BudgetExceeded { .. } => "budget_exceeded",
            Self::RateLimited(_) => "rate_limit_exceeded",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keys::types::{CreateKeyRequest, KeyPermissions};

    async fn setup() -> (Arc<KeyStore>, Arc<RateLimiter>, KeyValidator) {
        let store = Arc::new(KeyStore::in_memory().await.unwrap());
        let rate_limiter = Arc::new(RateLimiter::new());
        let validator = KeyValidator::new(store.clone(), rate_limiter.clone());
        (store, rate_limiter, validator)
    }

    #[tokio::test]
    async fn test_validate_valid_key() {
        let (store, _, validator) = setup().await;

        let response = store
            .create_key(CreateKeyRequest {
                name: Some("Test".to_string()),
                expires_at: None,
                permissions: KeyPermissions::default(),
                metadata: serde_json::Value::Null,
                oauth_user: None,
                ..Default::default()
            })
            .await
            .unwrap();

        let result = validator
            .validate(&response.key, "gpt-4", "openai", None)
            .await;

        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_validate_invalid_key() {
        let (_, _, validator) = setup().await;

        let result = validator
            .validate(
                "eavs-invalidkey123456789012345678901234",
                "gpt-4",
                "openai",
                None,
            )
            .await;

        assert!(matches!(result, Err(KeyValidationError::InvalidKey)));
    }

    #[tokio::test]
    async fn test_validate_non_virtual_key() {
        let (_, _, validator) = setup().await;

        let result = validator
            .validate("sk-openai-key", "gpt-4", "openai", None)
            .await;

        assert!(matches!(result, Err(KeyValidationError::NotVirtualKey)));
    }

    #[tokio::test]
    async fn test_validate_model_not_allowed() {
        let (store, _, validator) = setup().await;

        let mut permissions = KeyPermissions::default();
        permissions.allowed_models = Some(["gpt-3.5-*".to_string()].into());

        let response = store
            .create_key(CreateKeyRequest {
                name: Some("Limited".to_string()),
                expires_at: None,
                permissions,
                metadata: serde_json::Value::Null,
                oauth_user: None,
                ..Default::default()
            })
            .await
            .unwrap();

        let result = validator
            .validate(&response.key, "gpt-4", "openai", None)
            .await;

        assert!(matches!(
            result,
            Err(KeyValidationError::ModelNotAllowed { .. })
        ));
    }

    #[tokio::test]
    async fn test_validate_rate_limited() {
        let (store, _, validator) = setup().await;

        let mut permissions = KeyPermissions::default();
        permissions.rpm_limit = Some(2);

        let response = store
            .create_key(CreateKeyRequest {
                name: Some("Rate Limited".to_string()),
                expires_at: None,
                permissions,
                metadata: serde_json::Value::Null,
                oauth_user: None,
                ..Default::default()
            })
            .await
            .unwrap();

        // First two requests should succeed
        assert!(validator
            .validate(&response.key, "gpt-4", "openai", None)
            .await
            .is_ok());
        assert!(validator
            .validate(&response.key, "gpt-4", "openai", None)
            .await
            .is_ok());

        // Third should be rate limited
        let result = validator
            .validate(&response.key, "gpt-4", "openai", None)
            .await;

        assert!(matches!(result, Err(KeyValidationError::RateLimited(_))));
    }
}

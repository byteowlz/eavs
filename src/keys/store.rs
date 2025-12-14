//! SQLite-backed key storage.
//!
//! Provides persistent storage for virtual API keys with an in-memory cache
//! for fast lookups. Human-readable key IDs (e.g., "cold-lamp") are managed
//! via a pool table - IDs are claimed on key creation and returned to the
//! pool on key deletion.

use crate::keys::types::*;
use crate::keys::generation::{generate_key, generate_human_id, hash_key};
use chrono::{DateTime, Datelike, Utc};
use dashmap::DashMap;
use sqlx::{sqlite::SqlitePoolOptions, Pool, Sqlite};
use std::path::Path;
use std::sync::Arc;

/// SQLite-backed key store with in-memory caching.
pub struct KeyStore {
    /// SQLite connection pool
    pool: Pool<Sqlite>,
    /// In-memory cache for fast lookups (key_hash -> VirtualKey)
    cache: Arc<DashMap<String, VirtualKey>>,
}

impl KeyStore {
    /// Create a new key store, initializing the database if needed.
    pub async fn new(db_path: impl AsRef<Path>) -> Result<Self, KeyStoreError> {
        let db_path = db_path.as_ref();
        
        // Ensure parent directory exists
        if let Some(parent) = db_path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| KeyStoreError::Io(e.to_string()))?;
        }

        let db_url = format!("sqlite:{}?mode=rwc", db_path.display());
        
        let pool = SqlitePoolOptions::new()
            .max_connections(5)
            .connect(&db_url)
            .await
            .map_err(|e| KeyStoreError::Database(e.to_string()))?;

        // Run migrations
        Self::run_migrations(&pool).await?;

        let store = Self {
            pool,
            cache: Arc::new(DashMap::new()),
        };

        // Populate cache from database
        store.refresh_cache().await?;

        Ok(store)
    }

    /// Create an in-memory store (for testing).
    #[cfg(test)]
    pub async fn in_memory() -> Result<Self, KeyStoreError> {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .map_err(|e| KeyStoreError::Database(e.to_string()))?;

        Self::run_migrations(&pool).await?;

        Ok(Self {
            pool,
            cache: Arc::new(DashMap::new()),
        })
    }

    async fn run_migrations(pool: &Pool<Sqlite>) -> Result<(), KeyStoreError> {
        sqlx::query(
            r#"
            CREATE TABLE IF NOT EXISTS virtual_keys (
                key_hash TEXT PRIMARY KEY,
                key_id TEXT NOT NULL UNIQUE,
                name TEXT,
                created_at TEXT NOT NULL,
                expires_at TEXT,
                valid_after TEXT,
                disabled INTEGER NOT NULL DEFAULT 0,
                permissions TEXT NOT NULL,
                usage TEXT NOT NULL,
                metadata TEXT
            );

            CREATE INDEX IF NOT EXISTS idx_keys_created ON virtual_keys(created_at);
            CREATE INDEX IF NOT EXISTS idx_keys_disabled ON virtual_keys(disabled);
            CREATE INDEX IF NOT EXISTS idx_keys_key_id ON virtual_keys(key_id);
            "#,
        )
        .execute(pool)
        .await
        .map_err(|e| KeyStoreError::Database(e.to_string()))?;

        // Usage history table for analytics
        sqlx::query(
            r#"
            CREATE TABLE IF NOT EXISTS usage_history (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                key_hash TEXT NOT NULL,
                timestamp TEXT NOT NULL,
                model TEXT NOT NULL,
                provider TEXT NOT NULL,
                input_tokens INTEGER NOT NULL,
                output_tokens INTEGER NOT NULL,
                cached_tokens INTEGER NOT NULL DEFAULT 0,
                cost_usd REAL NOT NULL,
                FOREIGN KEY (key_hash) REFERENCES virtual_keys(key_hash) ON DELETE CASCADE
            );

            CREATE INDEX IF NOT EXISTS idx_usage_key ON usage_history(key_hash);
            CREATE INDEX IF NOT EXISTS idx_usage_timestamp ON usage_history(timestamp);
            "#,
        )
        .execute(pool)
        .await
        .map_err(|e| KeyStoreError::Database(e.to_string()))?;

        Ok(())
    }

    /// Refresh the in-memory cache from the database.
    pub async fn refresh_cache(&self) -> Result<(), KeyStoreError> {
        let rows: Vec<KeyRow> = sqlx::query_as(
            "SELECT key_hash, key_id, name, created_at, expires_at, valid_after, disabled, permissions, usage, metadata FROM virtual_keys WHERE disabled = 0",
        )
        .fetch_all(&self.pool)
        .await
        .map_err(|e| KeyStoreError::Database(e.to_string()))?;

        self.cache.clear();
        for row in rows {
            if let Ok(key) = row.to_virtual_key() {
                self.cache.insert(key.key_hash.clone(), key);
            }
        }

        Ok(())
    }

    /// Create a new virtual key.
    ///
    /// Returns the full key (only time it's available) and the key info.
    pub async fn create_key(
        &self,
        request: CreateKeyRequest,
    ) -> Result<CreateKeyResponse, KeyStoreError> {
        let key = generate_key();
        let key_hash = hash_key(&key);
        
        // Generate a unique human-readable ID (retry if collision)
        let key_id = self.generate_unique_human_id().await;

        let virtual_key = VirtualKey {
            key_id: key_id.clone(),
            key_hash: key_hash.clone(),
            name: request.name.clone(),
            created_at: Utc::now(),
            expires_at: request.expires_at,
            valid_after: None,
            disabled: false,
            permissions: request.permissions.clone(),
            usage: KeyUsage::default(),
            metadata: request.metadata.clone(),
        };

        // Insert into database
        let permissions_json = serde_json::to_string(&virtual_key.permissions)
            .map_err(|e| KeyStoreError::Serialization(e.to_string()))?;
        let usage_json = serde_json::to_string(&virtual_key.usage)
            .map_err(|e| KeyStoreError::Serialization(e.to_string()))?;
        let metadata_json = serde_json::to_string(&virtual_key.metadata)
            .map_err(|e| KeyStoreError::Serialization(e.to_string()))?;

        sqlx::query(
            r#"
            INSERT INTO virtual_keys (key_hash, key_id, name, created_at, expires_at, valid_after, disabled, permissions, usage, metadata)
            VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
            "#,
        )
        .bind(&key_hash)
        .bind(&virtual_key.key_id)
        .bind(&virtual_key.name)
        .bind(virtual_key.created_at.to_rfc3339())
        .bind(virtual_key.expires_at.map(|t| t.to_rfc3339()))
        .bind(virtual_key.valid_after.map(|t| t.to_rfc3339()))
        .bind(virtual_key.disabled as i32)
        .bind(&permissions_json)
        .bind(&usage_json)
        .bind(&metadata_json)
        .execute(&self.pool)
        .await
        .map_err(|e| KeyStoreError::Database(e.to_string()))?;

        // Add to cache
        self.cache.insert(key_hash.clone(), virtual_key.clone());

        Ok(CreateKeyResponse {
            key,
            key_id,
            key_hash,
            name: request.name,
            created_at: virtual_key.created_at,
            expires_at: request.expires_at,
            permissions: request.permissions,
        })
    }
    
    /// Generate a unique human-readable ID.
    /// 
    /// Generates adjective-noun combinations until finding one not in use.
    /// With ~40,000 combinations (200 adj * 200 nouns), collisions are rare
    /// until the pool is substantially depleted.
    async fn generate_unique_human_id(&self) -> String {
        // Try up to 100 times to find an unused ID
        for attempt in 0..100 {
            let id = generate_human_id();
            
            // Check if this ID is already in use (including disabled keys)
            let exists: Option<(i32,)> = sqlx::query_as(
                "SELECT 1 FROM virtual_keys WHERE key_id = ? LIMIT 1"
            )
            .bind(&id)
            .fetch_optional(&self.pool)
            .await
            .ok()
            .flatten();
            
            if exists.is_none() {
                return id;
            }
            
            // Log if we're having trouble finding IDs (pool getting full)
            if attempt == 50 {
                tracing::warn!("Human ID pool getting depleted, took 50+ attempts to find unused ID");
            }
        }
        
        // Fallback: append timestamp suffix to guarantee uniqueness
        let base_id = generate_human_id();
        let suffix = Utc::now().timestamp_millis() % 10000;
        format!("{}-{}", base_id, suffix)
    }

    /// Look up a key by its value.
    ///
    /// This is the hot path - uses cache for O(1) lookup.
    pub fn get_by_key(&self, key: &str) -> Option<VirtualKey> {
        let key_hash = hash_key(key);
        self.cache.get(&key_hash).map(|v| v.clone())
    }

    /// Look up a key by its hash (for internal use).
    pub fn get_by_hash(&self, key_hash: &str) -> Option<VirtualKey> {
        self.cache.get(key_hash).map(|v| v.clone())
    }

    /// Look up a key by its human-readable ID (e.g., "cold-lamp").
    pub fn get_by_human_id(&self, key_id: &str) -> Option<VirtualKey> {
        self.cache
            .iter()
            .find(|entry| entry.value().key_id == key_id)
            .map(|entry| entry.value().clone())
    }

    /// List all keys (returns masked info, not actual keys).
    pub async fn list_keys(&self) -> Result<Vec<KeyInfo>, KeyStoreError> {
        let rows: Vec<KeyRow> = sqlx::query_as(
            "SELECT key_hash, key_id, name, created_at, expires_at, valid_after, disabled, permissions, usage, metadata FROM virtual_keys ORDER BY created_at DESC",
        )
        .fetch_all(&self.pool)
        .await
        .map_err(|e| KeyStoreError::Database(e.to_string()))?;

        let mut keys = Vec::new();
        for row in rows {
            if let Ok(key) = row.to_virtual_key() {
                keys.push(key.to_info());
            }
        }
        Ok(keys)
    }

    /// Disable a key (soft delete).
    pub async fn disable_key(&self, key_hash: &str) -> Result<bool, KeyStoreError> {
        let result = sqlx::query("UPDATE virtual_keys SET disabled = 1 WHERE key_hash = ?")
            .bind(key_hash)
            .execute(&self.pool)
            .await
            .map_err(|e| KeyStoreError::Database(e.to_string()))?;

        if result.rows_affected() > 0 {
            self.cache.remove(key_hash);
            Ok(true)
        } else {
            Ok(false)
        }
    }

    /// Delete a key permanently.
    #[allow(dead_code)]
    pub async fn delete_key(&self, key_hash: &str) -> Result<bool, KeyStoreError> {
        let result = sqlx::query("DELETE FROM virtual_keys WHERE key_hash = ?")
            .bind(key_hash)
            .execute(&self.pool)
            .await
            .map_err(|e| KeyStoreError::Database(e.to_string()))?;

        if result.rows_affected() > 0 {
            self.cache.remove(key_hash);
            Ok(true)
        } else {
            Ok(false)
        }
    }

    /// Update usage stats for a key.
    pub async fn update_usage(
        &self,
        key_hash: &str,
        input_tokens: u32,
        output_tokens: u32,
        cached_tokens: u32,
        cost_usd: f64,
        model: &str,
        provider: &str,
    ) -> Result<(), KeyStoreError> {
        // Update cache first (hot path)
        if let Some(mut key) = self.cache.get_mut(key_hash) {
            key.usage.total_requests += 1;
            key.usage.total_input_tokens += input_tokens as u64;
            key.usage.total_output_tokens += output_tokens as u64;
            key.usage.total_spend_usd += cost_usd;
            key.usage.window_spend_usd += cost_usd;
            key.usage.last_request_at = Some(Utc::now());
            
            // Check if we need to reset the window
            if let Some(window) = &key.permissions.budget_window {
                let should_reset = match (&key.usage.window_start, window) {
                    (Some(start), BudgetWindow::Daily) => {
                        Utc::now().date_naive() > start.date_naive()
                    }
                    (Some(start), BudgetWindow::Weekly) => {
                        let days = (Utc::now() - *start).num_days();
                        days >= 7
                    }
                    (Some(start), BudgetWindow::Monthly) => {
                        Utc::now().date_naive().month() > start.date_naive().month()
                            || Utc::now().date_naive().year() > start.date_naive().year()
                    }
                    (None, _) => true,
                    (_, BudgetWindow::Total) => false,
                };

                if should_reset {
                    key.usage.window_spend_usd = cost_usd;
                    key.usage.window_start = Some(Utc::now());
                }
            }
        }

        // Record in usage history (async, non-blocking for main path)
        let timestamp = Utc::now().to_rfc3339();
        sqlx::query(
            r#"
            INSERT INTO usage_history (key_hash, timestamp, model, provider, input_tokens, output_tokens, cached_tokens, cost_usd)
            VALUES (?, ?, ?, ?, ?, ?, ?, ?)
            "#,
        )
        .bind(key_hash)
        .bind(&timestamp)
        .bind(model)
        .bind(provider)
        .bind(input_tokens as i64)
        .bind(output_tokens as i64)
        .bind(cached_tokens as i64)
        .bind(cost_usd)
        .execute(&self.pool)
        .await
        .map_err(|e| KeyStoreError::Database(e.to_string()))?;

        // Periodically sync cache to database (every 10 requests or so)
        // For now, we'll update on every request for simplicity
        if let Some(key) = self.cache.get(key_hash) {
            let usage_json = serde_json::to_string(&key.usage)
                .map_err(|e| KeyStoreError::Serialization(e.to_string()))?;
            
            sqlx::query("UPDATE virtual_keys SET usage = ? WHERE key_hash = ?")
                .bind(&usage_json)
                .bind(key_hash)
                .execute(&self.pool)
                .await
                .map_err(|e| KeyStoreError::Database(e.to_string()))?;
        }

        Ok(())
    }

    /// Get usage history for a key.
    pub async fn get_usage_history(
        &self,
        key_hash: &str,
        limit: Option<u32>,
    ) -> Result<Vec<UsageRecord>, KeyStoreError> {
        let limit = limit.unwrap_or(100);
        
        let rows: Vec<UsageRow> = sqlx::query_as(
            "SELECT timestamp, model, provider, input_tokens, output_tokens, cached_tokens, cost_usd FROM usage_history WHERE key_hash = ? ORDER BY timestamp DESC LIMIT ?",
        )
        .bind(key_hash)
        .bind(limit as i64)
        .fetch_all(&self.pool)
        .await
        .map_err(|e| KeyStoreError::Database(e.to_string()))?;

        Ok(rows.into_iter().map(|r| r.into()).collect())
    }

    /// Get the number of active keys.
    pub fn active_key_count(&self) -> usize {
        self.cache.len()
    }
}

/// Database row for virtual keys.
#[derive(sqlx::FromRow)]
struct KeyRow {
    key_hash: String,
    key_id: String,
    name: Option<String>,
    created_at: String,
    expires_at: Option<String>,
    valid_after: Option<String>,
    disabled: i32,
    permissions: String,
    usage: String,
    metadata: Option<String>,
}

impl KeyRow {
    fn to_virtual_key(&self) -> Result<VirtualKey, KeyStoreError> {
        let created_at = DateTime::parse_from_rfc3339(&self.created_at)
            .map_err(|e| KeyStoreError::Serialization(e.to_string()))?
            .with_timezone(&Utc);

        let expires_at = self
            .expires_at
            .as_ref()
            .map(|s| DateTime::parse_from_rfc3339(s))
            .transpose()
            .map_err(|e| KeyStoreError::Serialization(e.to_string()))?
            .map(|t| t.with_timezone(&Utc));

        let valid_after = self
            .valid_after
            .as_ref()
            .map(|s| DateTime::parse_from_rfc3339(s))
            .transpose()
            .map_err(|e| KeyStoreError::Serialization(e.to_string()))?
            .map(|t| t.with_timezone(&Utc));

        let permissions: KeyPermissions = serde_json::from_str(&self.permissions)
            .map_err(|e| KeyStoreError::Serialization(e.to_string()))?;

        let usage: KeyUsage = serde_json::from_str(&self.usage)
            .map_err(|e| KeyStoreError::Serialization(e.to_string()))?;

        let metadata: serde_json::Value = self
            .metadata
            .as_ref()
            .map(|s| serde_json::from_str(s))
            .transpose()
            .map_err(|e| KeyStoreError::Serialization(e.to_string()))?
            .unwrap_or(serde_json::Value::Null);

        Ok(VirtualKey {
            key_hash: self.key_hash.clone(),
            key_id: self.key_id.clone(),
            name: self.name.clone(),
            created_at,
            expires_at,
            valid_after,
            disabled: self.disabled != 0,
            permissions,
            usage,
            metadata,
        })
    }
}

/// Database row for usage history.
#[derive(sqlx::FromRow)]
struct UsageRow {
    timestamp: String,
    model: String,
    provider: String,
    input_tokens: i64,
    output_tokens: i64,
    cached_tokens: i64,
    cost_usd: f64,
}

/// Usage record for API response.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct UsageRecord {
    pub timestamp: DateTime<Utc>,
    pub model: String,
    pub provider: String,
    pub input_tokens: u32,
    pub output_tokens: u32,
    pub cached_tokens: u32,
    pub cost_usd: f64,
}

impl From<UsageRow> for UsageRecord {
    fn from(row: UsageRow) -> Self {
        Self {
            timestamp: DateTime::parse_from_rfc3339(&row.timestamp)
                .map(|t| t.with_timezone(&Utc))
                .unwrap_or_else(|_| Utc::now()),
            model: row.model,
            provider: row.provider,
            input_tokens: row.input_tokens as u32,
            output_tokens: row.output_tokens as u32,
            cached_tokens: row.cached_tokens as u32,
            cost_usd: row.cost_usd,
        }
    }
}

/// Errors from the key store.
#[derive(Debug)]
pub enum KeyStoreError {
    Database(String),
    Serialization(String),
    Io(String),
    #[allow(dead_code)]
    NotFound,
}

impl std::fmt::Display for KeyStoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Database(e) => write!(f, "Database error: {}", e),
            Self::Serialization(e) => write!(f, "Serialization error: {}", e),
            Self::Io(e) => write!(f, "IO error: {}", e),
            Self::NotFound => write!(f, "Key not found"),
        }
    }
}

impl std::error::Error for KeyStoreError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_create_and_get_key() {
        let store = KeyStore::in_memory().await.unwrap();

        let request = CreateKeyRequest {
            name: Some("Test Key".to_string()),
            expires_at: None,
            permissions: KeyPermissions::default(),
            metadata: serde_json::Value::Null,
        };

        let response = store.create_key(request).await.unwrap();
        assert!(response.key.starts_with("eavs-"));

        // Should be able to look up by key
        let key = store.get_by_key(&response.key).unwrap();
        assert_eq!(key.name, Some("Test Key".to_string()));
    }

    #[tokio::test]
    async fn test_list_keys() {
        let store = KeyStore::in_memory().await.unwrap();

        // Create a few keys
        for i in 0..3 {
            let request = CreateKeyRequest {
                name: Some(format!("Key {}", i)),
                expires_at: None,
                permissions: KeyPermissions::default(),
                metadata: serde_json::Value::Null,
            };
            store.create_key(request).await.unwrap();
        }

        let keys = store.list_keys().await.unwrap();
        assert_eq!(keys.len(), 3);
    }

    #[tokio::test]
    async fn test_disable_key() {
        let store = KeyStore::in_memory().await.unwrap();

        let request = CreateKeyRequest {
            name: Some("To Disable".to_string()),
            expires_at: None,
            permissions: KeyPermissions::default(),
            metadata: serde_json::Value::Null,
        };

        let response = store.create_key(request).await.unwrap();
        let key_hash = crate::keys::generation::hash_key(&response.key);

        // Should find key before disabling
        assert!(store.get_by_key(&response.key).is_some());

        // Disable
        store.disable_key(&key_hash).await.unwrap();

        // Should not find key after disabling
        assert!(store.get_by_key(&response.key).is_none());
    }

    #[tokio::test]
    async fn test_update_usage() {
        let store = KeyStore::in_memory().await.unwrap();

        let request = CreateKeyRequest {
            name: Some("Usage Test".to_string()),
            expires_at: None,
            permissions: KeyPermissions::default(),
            metadata: serde_json::Value::Null,
        };

        let response = store.create_key(request).await.unwrap();
        let key_hash = crate::keys::generation::hash_key(&response.key);

        // Record some usage
        store
            .update_usage(&key_hash, 100, 50, 0, 0.001, "gpt-4", "openai")
            .await
            .unwrap();

        // Check usage was recorded
        let key = store.get_by_key(&response.key).unwrap();
        assert_eq!(key.usage.total_requests, 1);
        assert_eq!(key.usage.total_input_tokens, 100);
        assert_eq!(key.usage.total_output_tokens, 50);
    }

    #[tokio::test]
    async fn test_human_readable_key_id() {
        let store = KeyStore::in_memory().await.unwrap();

        let request = CreateKeyRequest {
            name: Some("Human ID Test".to_string()),
            expires_at: None,
            permissions: KeyPermissions::default(),
            metadata: serde_json::Value::Null,
        };

        let response = store.create_key(request).await.unwrap();
        
        // Key should be the long eavs- format
        assert!(response.key.starts_with("eavs-"));
        assert!(response.key.len() > 30);
        
        // key_id should be human-readable (adjective-noun format)
        assert!(response.key_id.contains('-'));
        let parts: Vec<_> = response.key_id.split('-').collect();
        assert_eq!(parts.len(), 2, "key_id should be adjective-noun format");
        
        // Should be able to look up by human ID
        let key = store.get_by_human_id(&response.key_id).unwrap();
        assert_eq!(key.name, Some("Human ID Test".to_string()));
        assert_eq!(key.key_id, response.key_id);
    }

    #[tokio::test]
    async fn test_unique_human_ids() {
        let store = KeyStore::in_memory().await.unwrap();

        let mut ids = std::collections::HashSet::new();
        
        // Create several keys and verify unique IDs
        for i in 0..10 {
            let request = CreateKeyRequest {
                name: Some(format!("Key {}", i)),
                expires_at: None,
                permissions: KeyPermissions::default(),
                metadata: serde_json::Value::Null,
            };

            let response = store.create_key(request).await.unwrap();
            assert!(ids.insert(response.key_id.clone()), "Duplicate key_id generated");
        }
    }
}

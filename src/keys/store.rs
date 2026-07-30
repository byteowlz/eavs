//! SQLite-backed key storage.
//!
//! Provides persistent storage for virtual API keys with an in-memory cache
//! for fast lookups. Human-readable key IDs (e.g., "cold-lamp") are managed
//! via a pool table - IDs are claimed on key creation and returned to the
//! pool on key deletion.
//!
//! Performance optimizations:
//! - In-memory cache for O(1) lookups
//! - Batched SQLite writes via background task
//! - Configurable sync intervals

use crate::keys::generation::{generate_human_id, generate_key, hash_key};
use crate::keys::types::*;
use chrono::{DateTime, Datelike, Utc};
use dashmap::DashMap;
use sqlx::{sqlite::SqlitePoolOptions, Pool, Row, Sqlite};
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use tokio::sync::mpsc;

/// Message type for the background sync task.
#[derive(Debug)]
enum SyncMessage {
    /// Sync a specific key's usage to the database
    SyncUsage(String),
    /// Shutdown the sync task
    Shutdown,
}

/// SQLite-backed key store with in-memory caching and batched writes.
pub struct KeyStore {
    /// SQLite connection pool
    pool: Pool<Sqlite>,
    /// In-memory cache for fast lookups (key_hash -> VirtualKey)
    cache: Arc<DashMap<String, VirtualKey>>,
    /// Secondary index for O(1) lookup by human ID (key_id -> key_hash)
    human_id_index: Arc<DashMap<String, String>>,
    /// Pending usage updates counter (for batch syncing)
    pending_updates: Arc<AtomicU64>,
    /// Channel to send sync requests to background task
    sync_tx: Option<mpsc::UnboundedSender<SyncMessage>>,
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

        let cache = Arc::new(DashMap::new());
        let human_id_index = Arc::new(DashMap::new());
        let pending_updates = Arc::new(AtomicU64::new(0));

        // Start background sync task
        let (sync_tx, sync_rx) = mpsc::unbounded_channel();
        let pool_clone = pool.clone();
        let cache_clone = cache.clone();
        let pending_clone = pending_updates.clone();

        tokio::spawn(async move {
            Self::background_sync_task(pool_clone, cache_clone, pending_clone, sync_rx).await;
        });

        let store = Self {
            pool,
            cache,
            human_id_index,
            pending_updates,
            sync_tx: Some(sync_tx),
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
            human_id_index: Arc::new(DashMap::new()),
            pending_updates: Arc::new(AtomicU64::new(0)),
            sync_tx: None, // No background task for tests (sync is immediate)
        })
    }

    /// Background task that batches and syncs usage updates to SQLite.
    async fn background_sync_task(
        pool: Pool<Sqlite>,
        cache: Arc<DashMap<String, VirtualKey>>,
        pending_updates: Arc<AtomicU64>,
        mut rx: mpsc::UnboundedReceiver<SyncMessage>,
    ) {
        use std::collections::HashSet;
        use std::time::Duration;

        let mut pending_keys: HashSet<String> = HashSet::new();
        let mut interval = tokio::time::interval(Duration::from_secs(5)); // Sync every 5 seconds

        loop {
            tokio::select! {
                // Receive sync requests
                msg = rx.recv() => {
                    match msg {
                        Some(SyncMessage::SyncUsage(key_hash)) => {
                            pending_keys.insert(key_hash);
                        }
                        Some(SyncMessage::Shutdown) | None => {
                            // Sync any remaining keys before shutdown
                            if !pending_keys.is_empty() {
                                Self::sync_keys_to_db(&pool, &cache, &pending_keys).await;
                            }
                            break;
                        }
                    }
                }
                // Periodic sync
                _ = interval.tick() => {
                    if !pending_keys.is_empty() {
                        Self::sync_keys_to_db(&pool, &cache, &pending_keys).await;
                        pending_updates.store(0, Ordering::Relaxed);
                        pending_keys.clear();
                    }
                }
            }
        }

        tracing::debug!("Background sync task shutting down");
    }

    /// Sync a batch of keys to the database.
    async fn sync_keys_to_db(
        pool: &Pool<Sqlite>,
        cache: &DashMap<String, VirtualKey>,
        key_hashes: &std::collections::HashSet<String>,
    ) {
        for key_hash in key_hashes {
            if let Some(key) = cache.get(key_hash) {
                let usage_json = match serde_json::to_string(&key.usage) {
                    Ok(json) => json,
                    Err(e) => {
                        tracing::warn!("Failed to serialize usage for {}: {}", key_hash, e);
                        continue;
                    }
                };

                if let Err(e) = sqlx::query("UPDATE virtual_keys SET usage = ? WHERE key_hash = ?")
                    .bind(&usage_json)
                    .bind(key_hash)
                    .execute(pool)
                    .await
                {
                    tracing::warn!("Failed to sync usage for {}: {}", key_hash, e);
                }
            }
        }

        tracing::debug!("Synced {} keys to database", key_hashes.len());
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
                metadata TEXT,
                oauth_user TEXT,
                owner TEXT
            );

            CREATE INDEX IF NOT EXISTS idx_keys_created ON virtual_keys(created_at);
            CREATE INDEX IF NOT EXISTS idx_keys_disabled ON virtual_keys(disabled);
            CREATE INDEX IF NOT EXISTS idx_keys_key_id ON virtual_keys(key_id);
            "#,
        )
        .execute(pool)
        .await
        .map_err(|e| KeyStoreError::Database(e.to_string()))?;

        // Ensure oauth_user column exists for pre-existing databases.
        let columns = sqlx::query("PRAGMA table_info(virtual_keys)")
            .fetch_all(pool)
            .await
            .map_err(|e| KeyStoreError::Database(e.to_string()))?;
        let mut has_oauth_user = false;
        for row in columns {
            let name: String = row
                .try_get("name")
                .map_err(|e| KeyStoreError::Database(e.to_string()))?;
            if name == "oauth_user" {
                has_oauth_user = true;
                break;
            }
        }
        if !has_oauth_user {
            sqlx::query("ALTER TABLE virtual_keys ADD COLUMN oauth_user TEXT")
                .execute(pool)
                .await
                .map_err(|e| KeyStoreError::Database(e.to_string()))?;
        }

        // Ensure oauth_account column exists (multi-account support).
        let has_oauth_account: bool = sqlx::query_scalar::<_, i32>(
            "SELECT COUNT(*) FROM pragma_table_info('virtual_keys') WHERE name = 'oauth_account'",
        )
        .fetch_one(pool)
        .await
        .map_err(|e| KeyStoreError::Database(e.to_string()))?
            > 0;
        if !has_oauth_account {
            sqlx::query("ALTER TABLE virtual_keys ADD COLUMN oauth_account TEXT")
                .execute(pool)
                .await
                .map_err(|e| KeyStoreError::Database(e.to_string()))?;
        }

        // Ensure the organizational owner/tag column exists.
        let has_owner: bool = sqlx::query_scalar::<_, i32>(
            "SELECT COUNT(*) FROM pragma_table_info('virtual_keys') WHERE name = 'owner'",
        )
        .fetch_one(pool)
        .await
        .map_err(|e| KeyStoreError::Database(e.to_string()))?
            > 0;
        if !has_owner {
            sqlx::query("ALTER TABLE virtual_keys ADD COLUMN owner TEXT")
                .execute(pool)
                .await
                .map_err(|e| KeyStoreError::Database(e.to_string()))?;
        }

        // Usage history table for analytics.
        //
        // IMPORTANT: only create the table here. Indexes that reference the
        // optional columns (`owner`, `cache_write_tokens`) are created *after*
        // the ALTER TABLE blocks below. CREATE TABLE IF NOT EXISTS is a no-op
        // on a pre-existing table, so on databases created by older EAVS
        // versions the optional columns are absent until the ALTERs run.
        // Creating an index that references a not-yet-existing column aborts
        // initialization ("no such column: owner"), leaving the key store
        // non-functional. See eavs-64qn.
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
                cache_write_tokens INTEGER NOT NULL DEFAULT 0,
                cost_usd REAL NOT NULL,
                owner TEXT,
                FOREIGN KEY (key_hash) REFERENCES virtual_keys(key_hash) ON DELETE CASCADE
            );
            "#,
        )
        .execute(pool)
        .await
        .map_err(|e| KeyStoreError::Database(e.to_string()))?;

        // Ensure the owner column exists on pre-existing usage_history tables.
        let has_usage_owner: bool = sqlx::query_scalar::<_, i32>(
            "SELECT COUNT(*) FROM pragma_table_info('usage_history') WHERE name = 'owner'",
        )
        .fetch_one(pool)
        .await
        .map_err(|e| KeyStoreError::Database(e.to_string()))?
            > 0;
        if !has_usage_owner {
            sqlx::query("ALTER TABLE usage_history ADD COLUMN owner TEXT")
                .execute(pool)
                .await
                .map_err(|e| KeyStoreError::Database(e.to_string()))?;
        }

        // Ensure the cache_write_tokens column exists on pre-existing tables.
        let has_cache_write: bool = sqlx::query_scalar::<_, i32>(
            "SELECT COUNT(*) FROM pragma_table_info('usage_history') WHERE name = 'cache_write_tokens'",
        )
        .fetch_one(pool)
        .await
        .map_err(|e| KeyStoreError::Database(e.to_string()))?
            > 0;
        if !has_cache_write {
            sqlx::query(
                "ALTER TABLE usage_history ADD COLUMN cache_write_tokens INTEGER NOT NULL DEFAULT 0",
            )
            .execute(pool)
            .await
            .map_err(|e| KeyStoreError::Database(e.to_string()))?;
        }

        // Now that the optional columns are guaranteed to exist on both fresh
        // and pre-existing tables, create the indexes. Order matters: any
        // index/constraint referencing a column must run after the ADD COLUMN
        // that guarantees it (eavs-64qn).
        sqlx::query(
            r#"
            CREATE INDEX IF NOT EXISTS idx_usage_key ON usage_history(key_hash);
            CREATE INDEX IF NOT EXISTS idx_usage_timestamp ON usage_history(timestamp);
            CREATE INDEX IF NOT EXISTS idx_usage_owner ON usage_history(owner);
            "#,
        )
        .execute(pool)
        .await
        .map_err(|e| KeyStoreError::Database(e.to_string()))?;

        // OAuth credentials table
        sqlx::query(
            r#"
            CREATE TABLE IF NOT EXISTS oauth_credentials (
                user_id TEXT NOT NULL,
                provider TEXT NOT NULL,
                access_token TEXT NOT NULL,
                refresh_token TEXT NOT NULL,
                expires_at INTEGER NOT NULL,
                extra_data TEXT,
                PRIMARY KEY (user_id, provider)
            );
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
            "SELECT key_hash, key_id, name, created_at, expires_at, valid_after, disabled, permissions, usage, metadata, owner, oauth_user, oauth_account FROM virtual_keys WHERE disabled = 0",
        )
        .fetch_all(&self.pool)
        .await
        .map_err(|e| KeyStoreError::Database(e.to_string()))?;

        self.cache.clear();
        self.human_id_index.clear();
        for row in rows {
            if let Ok(key) = row.to_virtual_key() {
                // Update secondary index for O(1) human ID lookups
                self.human_id_index
                    .insert(key.key_id.clone(), key.key_hash.clone());
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
            owner: request.owner.clone(),
            oauth_user: request.oauth_user.clone(),
            oauth_account: request.oauth_account.clone(),
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
            INSERT INTO virtual_keys (key_hash, key_id, name, created_at, expires_at, valid_after, disabled, permissions, usage, metadata, owner, oauth_user, oauth_account)
            VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
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
        .bind(&virtual_key.owner)
        .bind(&virtual_key.oauth_user)
        .bind(&virtual_key.oauth_account)
        .execute(&self.pool)
        .await
        .map_err(|e| KeyStoreError::Database(e.to_string()))?;

        // Add to cache and secondary index
        self.human_id_index.insert(key_id.clone(), key_hash.clone());
        self.cache.insert(key_hash.clone(), virtual_key.clone());

        Ok(CreateKeyResponse {
            key,
            key_id,
            key_hash,
            name: request.name,
            created_at: virtual_key.created_at,
            expires_at: request.expires_at,
            permissions: request.permissions,
            owner: request.owner,
            oauth_user: request.oauth_user,
            oauth_account: request.oauth_account,
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
            let exists: Option<(i32,)> =
                sqlx::query_as("SELECT 1 FROM virtual_keys WHERE key_id = ? LIMIT 1")
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
                tracing::warn!(
                    "Human ID pool getting depleted, took 50+ attempts to find unused ID"
                );
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
    ///
    /// Uses a secondary index for O(1) lookup instead of O(n) iteration.
    pub fn get_by_human_id(&self, key_id: &str) -> Option<VirtualKey> {
        // First look up the key_hash via secondary index
        let key_hash = self.human_id_index.get(key_id)?;
        // Then look up the full key via primary cache
        self.cache.get(key_hash.value()).map(|v| v.clone())
    }

    /// List all keys (returns masked info, not actual keys).
    ///
    /// Returns real-time data from the in-memory cache, not the database.
    /// Usage stats are updated immediately in memory after each request.
    pub async fn list_keys(&self) -> Result<Vec<KeyInfo>, KeyStoreError> {
        let mut keys = Vec::new();
        for entry in self.cache.iter() {
            keys.push(entry.value().to_info());
        }
        // Sort by created_at descending (newest first)
        keys.sort_by_key(|k| std::cmp::Reverse(k.created_at));
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
            // Remove from cache and secondary index
            if let Some((_, key)) = self.cache.remove(key_hash) {
                self.human_id_index.remove(&key.key_id);
            }
            Ok(true)
        } else {
            Ok(false)
        }
    }

    /// Update OAuth user binding for a key.
    pub async fn update_oauth_user(
        &self,
        key_hash: &str,
        oauth_user: Option<String>,
    ) -> Result<bool, KeyStoreError> {
        let result = sqlx::query("UPDATE virtual_keys SET oauth_user = ? WHERE key_hash = ?")
            .bind(&oauth_user)
            .bind(key_hash)
            .execute(&self.pool)
            .await
            .map_err(|e| KeyStoreError::Database(e.to_string()))?;

        if result.rows_affected() > 0 {
            if let Some(mut key) = self.cache.get_mut(key_hash) {
                key.oauth_user = oauth_user;
            }
            Ok(true)
        } else {
            Ok(false)
        }
    }

    /// Update the organizational owner/tag for a key.
    pub async fn update_owner(
        &self,
        key_hash: &str,
        owner: Option<String>,
    ) -> Result<bool, KeyStoreError> {
        let result = sqlx::query("UPDATE virtual_keys SET owner = ? WHERE key_hash = ?")
            .bind(&owner)
            .bind(key_hash)
            .execute(&self.pool)
            .await
            .map_err(|e| KeyStoreError::Database(e.to_string()))?;

        if result.rows_affected() > 0 {
            if let Some(mut key) = self.cache.get_mut(key_hash) {
                key.owner = owner;
            }
            Ok(true)
        } else {
            Ok(false)
        }
    }

    /// Aggregate usage/cost grouped by owner across all keys.
    ///
    /// When `owner` is `Some`, only that owner's totals are returned. Rows with a
    /// NULL owner are grouped under the `""` (unassigned) bucket.
    pub async fn get_usage_by_owner(
        &self,
        owner: Option<&str>,
        days: Option<u32>,
    ) -> Result<Vec<OwnerUsage>, KeyStoreError> {
        let cutoff = cutoff_timestamp(days);
        let rows: Vec<OwnerUsageRow> = if let Some(owner) = owner {
            sqlx::query_as(
                r#"
                SELECT COALESCE(owner, '') AS owner,
                       COUNT(*) AS requests,
                       COALESCE(SUM(input_tokens), 0) AS input_tokens,
                       COALESCE(SUM(output_tokens), 0) AS output_tokens,
                       COALESCE(SUM(cached_tokens), 0) AS cached_tokens,
                       COALESCE(SUM(cache_write_tokens), 0) AS cache_write_tokens,
                       COALESCE(SUM(cost_usd), 0.0) AS cost_usd
                FROM usage_history
                WHERE COALESCE(owner, '') = ? AND timestamp >= ?
                GROUP BY COALESCE(owner, '')
                "#,
            )
            .bind(owner)
            .bind(&cutoff)
            .fetch_all(&self.pool)
            .await
        } else {
            sqlx::query_as(
                r#"
                SELECT COALESCE(owner, '') AS owner,
                       COUNT(*) AS requests,
                       COALESCE(SUM(input_tokens), 0) AS input_tokens,
                       COALESCE(SUM(output_tokens), 0) AS output_tokens,
                       COALESCE(SUM(cached_tokens), 0) AS cached_tokens,
                       COALESCE(SUM(cache_write_tokens), 0) AS cache_write_tokens,
                       COALESCE(SUM(cost_usd), 0.0) AS cost_usd
                FROM usage_history
                WHERE timestamp >= ?
                GROUP BY COALESCE(owner, '')
                ORDER BY cost_usd DESC
                "#,
            )
            .bind(&cutoff)
            .fetch_all(&self.pool)
            .await
        }
        .map_err(|e| KeyStoreError::Database(e.to_string()))?;

        Ok(rows.into_iter().map(|r| r.into()).collect())
    }

    /// Aggregate usage grouped by virtual key, newest cost first.
    pub async fn get_usage_by_key(&self, days: Option<u32>) -> Result<Vec<KeyCost>, KeyStoreError> {
        let cutoff = cutoff_timestamp(days);
        let rows: Vec<KeyCostRow> = sqlx::query_as(
            r#"
            SELECT h.key_hash AS key_hash,
                   COALESCE(k.key_id, '') AS key_id,
                   COALESCE(k.name, '') AS name,
                   COALESCE(h.owner, '') AS owner,
                   COUNT(*) AS requests,
                   COALESCE(SUM(h.input_tokens), 0) AS input_tokens,
                   COALESCE(SUM(h.output_tokens), 0) AS output_tokens,
                   COALESCE(SUM(h.cached_tokens), 0) AS cached_tokens,
                   COALESCE(SUM(h.cache_write_tokens), 0) AS cache_write_tokens,
                   COALESCE(SUM(h.cost_usd), 0.0) AS cost_usd
            FROM usage_history h
            LEFT JOIN virtual_keys k ON k.key_hash = h.key_hash
            WHERE h.timestamp >= ?
            GROUP BY h.key_hash
            ORDER BY cost_usd DESC
            "#,
        )
        .bind(&cutoff)
        .fetch_all(&self.pool)
        .await
        .map_err(|e| KeyStoreError::Database(e.to_string()))?;

        Ok(rows.into_iter().map(|r| r.into()).collect())
    }

    /// Gracefully shutdown the key store, flushing any pending writes.
    #[allow(dead_code)]
    pub fn shutdown(&self) {
        if let Some(ref tx) = self.sync_tx {
            let _ = tx.send(SyncMessage::Shutdown);
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
            // Remove from cache and secondary index
            if let Some((_, key)) = self.cache.remove(key_hash) {
                self.human_id_index.remove(&key.key_id);
            }
            Ok(true)
        } else {
            Ok(false)
        }
    }

    /// Update usage stats for a key.
    ///
    /// This is optimized for high-throughput:
    /// - Cache is updated immediately (in-memory, O(1))
    /// - Usage history is recorded asynchronously
    /// - Key usage sync to SQLite is batched via background task
    #[allow(clippy::too_many_arguments)]
    pub async fn update_usage(
        &self,
        key_hash: &str,
        input_tokens: u32,
        output_tokens: u32,
        cached_tokens: u32,
        cache_write_tokens: u32,
        cost_usd: f64,
        model: &str,
        provider: &str,
    ) -> Result<(), KeyStoreError> {
        // Update cache first (hot path - O(1))
        if let Some(mut key) = self.cache.get_mut(key_hash) {
            key.usage.total_requests += 1;
            key.usage.total_input_tokens += input_tokens as u64;
            key.usage.total_output_tokens += output_tokens as u64;
            key.usage.total_spend_usd += cost_usd;
            key.usage.window_spend_usd += cost_usd;
            key.usage.last_request_at = Some(Utc::now());

            // Check if we need to reset the window
            // Use UTC timestamps consistently to avoid timezone edge cases
            if let Some(window) = &key.permissions.budget_window {
                let now = Utc::now();
                let should_reset = match (&key.usage.window_start, window) {
                    (Some(start), BudgetWindow::Daily) => {
                        // Compare UTC days using duration-based comparison
                        // This handles year boundaries correctly
                        let start_utc_day = start.timestamp() / 86400;
                        let now_utc_day = now.timestamp() / 86400;
                        now_utc_day > start_utc_day
                    }
                    (Some(start), BudgetWindow::Weekly) => {
                        let duration = now.signed_duration_since(*start);
                        duration.num_days() >= 7
                    }
                    (Some(start), BudgetWindow::Monthly) => {
                        // For monthly, compare year and month using signed duration
                        // If it's been at least 28 days, check if we've crossed a month boundary
                        let duration = now.signed_duration_since(*start);
                        if duration.num_days() >= 28 {
                            // Use year*12+month to handle year boundaries correctly
                            let start_ym = start.year() * 12 + start.month() as i32;
                            let now_ym = now.year() * 12 + now.month() as i32;
                            now_ym > start_ym
                        } else {
                            false
                        }
                    }
                    (None, _) => true,
                    (_, BudgetWindow::Total) => false,
                };

                if should_reset {
                    key.usage.window_spend_usd = cost_usd;
                    key.usage.window_start = Some(now);
                }
            }
        }

        // Record in usage history (async, non-blocking for main path).
        // Denormalize the owner so rollups need no join and survive key deletion.
        let owner = self.cache.get(key_hash).and_then(|k| k.owner.clone());
        let timestamp = Utc::now().to_rfc3339();
        sqlx::query(
            r#"
            INSERT INTO usage_history (key_hash, timestamp, model, provider, input_tokens, output_tokens, cached_tokens, cache_write_tokens, cost_usd, owner)
            VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
            "#,
        )
        .bind(key_hash)
        .bind(&timestamp)
        .bind(model)
        .bind(provider)
        .bind(input_tokens as i64)
        .bind(output_tokens as i64)
        .bind(cached_tokens as i64)
        .bind(cache_write_tokens as i64)
        .bind(cost_usd)
        .bind(&owner)
        .execute(&self.pool)
        .await
        .map_err(|e| KeyStoreError::Database(e.to_string()))?;

        // Schedule batched sync via background task (if available)
        // This avoids synchronous SQLite writes on every request
        if let Some(ref tx) = self.sync_tx {
            self.pending_updates.fetch_add(1, Ordering::Relaxed);
            let _ = tx.send(SyncMessage::SyncUsage(key_hash.to_string()));
        } else {
            // Fallback for tests: sync immediately
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
            "SELECT timestamp, model, provider, input_tokens, output_tokens, cached_tokens, cache_write_tokens, cost_usd FROM usage_history WHERE key_hash = ? ORDER BY timestamp DESC LIMIT ?",
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
    #[sqlx(default)]
    owner: Option<String>,
    oauth_user: Option<String>,
    #[sqlx(default)]
    oauth_account: Option<String>,
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
            owner: self.owner.clone(),
            oauth_user: self.oauth_user.clone(),
            oauth_account: self.oauth_account.clone(),
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
    #[sqlx(default)]
    cache_write_tokens: i64,
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
    #[serde(default)]
    pub cache_write_tokens: u32,
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
            cache_write_tokens: row.cache_write_tokens as u32,
            cost_usd: row.cost_usd,
        }
    }
}

/// Database row for per-owner usage aggregation.
#[derive(sqlx::FromRow)]
struct OwnerUsageRow {
    owner: String,
    requests: i64,
    input_tokens: i64,
    output_tokens: i64,
    cached_tokens: i64,
    cache_write_tokens: i64,
    cost_usd: f64,
}

/// Aggregated usage/cost for a single owner across all their keys.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct OwnerUsage {
    /// Owner label (empty string = unassigned).
    pub owner: String,
    pub requests: u64,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cached_tokens: u64,
    pub cache_write_tokens: u64,
    pub cost_usd: f64,
}

impl From<OwnerUsageRow> for OwnerUsage {
    fn from(row: OwnerUsageRow) -> Self {
        Self {
            owner: row.owner,
            requests: row.requests as u64,
            input_tokens: row.input_tokens as u64,
            output_tokens: row.output_tokens as u64,
            cached_tokens: row.cached_tokens as u64,
            cache_write_tokens: row.cache_write_tokens as u64,
            cost_usd: row.cost_usd,
        }
    }
}

/// Database row for per-key usage aggregation.
#[derive(sqlx::FromRow)]
struct KeyCostRow {
    key_hash: String,
    key_id: String,
    name: String,
    owner: String,
    requests: i64,
    input_tokens: i64,
    output_tokens: i64,
    cached_tokens: i64,
    cache_write_tokens: i64,
    cost_usd: f64,
}

/// Aggregated usage/cost for a single virtual key.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct KeyCost {
    pub key_hash: String,
    pub key_id: String,
    pub name: String,
    /// Owner label (empty string = unassigned).
    pub owner: String,
    pub requests: u64,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cached_tokens: u64,
    pub cache_write_tokens: u64,
    pub cost_usd: f64,
}

impl From<KeyCostRow> for KeyCost {
    fn from(row: KeyCostRow) -> Self {
        Self {
            key_hash: row.key_hash,
            key_id: row.key_id,
            name: row.name,
            owner: row.owner,
            requests: row.requests as u64,
            input_tokens: row.input_tokens as u64,
            output_tokens: row.output_tokens as u64,
            cached_tokens: row.cached_tokens as u64,
            cache_write_tokens: row.cache_write_tokens as u64,
            cost_usd: row.cost_usd,
        }
    }
}

/// RFC3339 lower bound for a `--days` window (epoch when `None`).
fn cutoff_timestamp(days: Option<u32>) -> String {
    match days {
        Some(d) => (Utc::now() - chrono::Duration::days(d as i64)).to_rfc3339(),
        None => "1970-01-01T00:00:00+00:00".to_string(),
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
            oauth_user: None,
            ..Default::default()
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
                oauth_user: None,
                ..Default::default()
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
            oauth_user: None,
            ..Default::default()
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
            oauth_user: None,
            ..Default::default()
        };

        let response = store.create_key(request).await.unwrap();
        let key_hash = crate::keys::generation::hash_key(&response.key);

        // Record some usage
        store
            .update_usage(&key_hash, 100, 50, 0, 0, 0.001, "gpt-4", "openai")
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
            oauth_user: None,
            ..Default::default()
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
                oauth_user: None,
                ..Default::default()
            };

            let response = store.create_key(request).await.unwrap();
            assert!(
                ids.insert(response.key_id.clone()),
                "Duplicate key_id generated"
            );
        }
    }

    #[tokio::test]
    async fn test_owner_usage_rollup() {
        let store = KeyStore::in_memory().await.unwrap();

        // Two keys owned by "alice", one by "bob", one unassigned.
        let mut hashes = std::collections::HashMap::new();
        for (name, owner) in [
            ("a1", Some("alice")),
            ("a2", Some("alice")),
            ("b1", Some("bob")),
            ("u1", None),
        ] {
            let request = CreateKeyRequest {
                name: Some(name.to_string()),
                owner: owner.map(str::to_string),
                ..Default::default()
            };
            let resp = store.create_key(request).await.unwrap();
            hashes.insert(name, resp.key_hash);
        }

        // Record usage: alice across both keys, bob once, unassigned once.
        store
            .update_usage(&hashes["a1"], 100, 50, 0, 0, 0.10, "gpt-4", "openai")
            .await
            .unwrap();
        store
            .update_usage(&hashes["a2"], 200, 80, 0, 0, 0.20, "gpt-4", "openai")
            .await
            .unwrap();
        store
            .update_usage(&hashes["b1"], 10, 5, 0, 0, 0.01, "gpt-4", "openai")
            .await
            .unwrap();
        store
            .update_usage(&hashes["u1"], 1, 1, 0, 0, 0.001, "gpt-4", "openai")
            .await
            .unwrap();

        // Filtered to alice: rolled up across both her keys.
        let alice = store.get_usage_by_owner(Some("alice"), None).await.unwrap();
        assert_eq!(alice.len(), 1);
        assert_eq!(alice[0].owner, "alice");
        assert_eq!(alice[0].requests, 2);
        assert_eq!(alice[0].input_tokens, 300);
        assert_eq!(alice[0].output_tokens, 130);
        assert!((alice[0].cost_usd - 0.30).abs() < 1e-9);

        // All owners, including the empty-string (unassigned) bucket.
        let all = store.get_usage_by_owner(None, None).await.unwrap();
        assert_eq!(all.len(), 3);
        assert!(all.iter().any(|o| o.owner == "alice" && o.requests == 2));
        assert!(all.iter().any(|o| o.owner == "bob" && o.requests == 1));
        assert!(all.iter().any(|o| o.owner.is_empty() && o.requests == 1));

        // Per-key rollup: one bucket per key, carrying owner + cost.
        let by_key = store.get_usage_by_key(None).await.unwrap();
        assert_eq!(by_key.len(), 4);
        let a1 = by_key
            .iter()
            .find(|k| k.key_hash == hashes["a1"])
            .expect("a1 present");
        assert_eq!(a1.owner, "alice");
        assert_eq!(a1.requests, 1);
        assert_eq!(a1.input_tokens, 100);
        assert!((a1.cost_usd - 0.10).abs() < 1e-9);
    }

    /// Regression test for eavs-64qn.
    ///
    /// Simulates upgrading a keys.db created by an older EAVS version whose
    /// `usage_history` table predates the `owner` and `cache_write_tokens`
    /// columns. Previously the migration created `idx_usage_owner` (which
    /// references `owner`) *before* the ALTER TABLE that adds `owner` to a
    /// pre-existing table, so init aborted with "no such column: owner" and
    /// the key store was left uninitialized. This test fails on 0.8.1 and
    /// passes once the column-additions run before the index creation.
    #[tokio::test]
    async fn test_migration_handles_legacy_usage_history() {
        // A unique temp file for this test.
        let tmp_dir = std::env::temp_dir();
        let db_path = tmp_dir.join(format!(
            "eavs-64qn-test-{}-{}.db",
            std::process::id(),
            unique_suffix()
        ));
        let _ = std::fs::remove_file(&db_path);

        // Hand-craft a legacy database: the schema that 0.8.0-era EAVS would
        // have written, i.e. WITHOUT the `owner` and `cache_write_tokens`
        // columns on usage_history.
        {
            let pool = SqlitePoolOptions::new()
                .max_connections(1)
                .connect(&format!("sqlite:{}?mode=rwc", db_path.display()))
                .await
                .unwrap();
            sqlx::query(
                r#"
                CREATE TABLE virtual_keys (
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
                CREATE TABLE usage_history (
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
                "#,
            )
            .execute(&pool)
            .await
            .unwrap();
            // Insert one legacy row so we exercise a populated table.
            sqlx::query(
                "INSERT INTO virtual_keys (key_hash, key_id, name, created_at, permissions, usage) \
                 VALUES ('legacy-hash', 'legacy-id', 'Legacy', '2026-01-01T00:00:00Z', '{}', '{}')",
            )
            .execute(&pool)
            .await
            .unwrap();
            sqlx::query(
                "INSERT INTO usage_history (key_hash, timestamp, model, provider, input_tokens, output_tokens, cached_tokens, cost_usd) \
                 VALUES ('legacy-hash', '2026-01-01T00:00:00Z', 'gpt-4', 'openai', 10, 5, 0, 0.01)",
            )
            .execute(&pool)
            .await
            .unwrap();
            pool.close().await;
        }

        // Now run the real migration on the legacy database.
        let store = KeyStore::new(&db_path)
            .await
            .expect("migration must succeed on a legacy usage_history table (eavs-64qn)");

        // Both optional columns must now exist on usage_history.
        let has_owner: i32 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM pragma_table_info('usage_history') WHERE name = 'owner'",
        )
        .fetch_one(&store.pool)
        .await
        .unwrap();
        assert_eq!(has_owner, 1, "owner column should have been added");

        let has_cache_write: i32 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM pragma_table_info('usage_history') WHERE name = 'cache_write_tokens'",
        )
        .fetch_one(&store.pool)
        .await
        .unwrap();
        assert_eq!(has_cache_write, 1, "cache_write_tokens column should have been added");

        // The owner-referencing index must exist.
        let idx_count: i32 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM sqlite_master WHERE type = 'index' AND name = 'idx_usage_owner'",
        )
        .fetch_one(&store.pool)
        .await
        .unwrap();
        assert_eq!(idx_count, 1, "idx_usage_owner should have been created");

        // The legacy row must still be present (migration is non-destructive).
        let row_count: i32 =
            sqlx::query_scalar("SELECT COUNT(*) FROM usage_history WHERE key_hash = 'legacy-hash'")
                .fetch_one(&store.pool)
                .await
                .unwrap();
        assert_eq!(row_count, 1, "legacy usage_history row must survive migration");

        // Functional sanity: the store should be usable end-to-end (create +
        // record usage with the new owner column populated).
        let resp = store
            .create_key(CreateKeyRequest {
                name: Some("Post-Upgrade Key".to_string()),
                owner: Some("post-upgrade".to_string()),
                ..Default::default()
            })
            .await
            .expect("create_key must work after migration");
        store
            .update_usage(&resp.key_hash, 7, 3, 0, 4, 0.02, "gpt-4", "openai")
            .await
            .expect("update_usage must work after migration");
        let rollup = store.get_usage_by_owner(None, None).await.unwrap();
        assert!(
            rollup.iter().any(|o| o.owner == "post-upgrade"),
            "post-upgrade owner bucket should be present in usage rollup"
        );

        // Cleanup.
        drop(store);
        let _ = std::fs::remove_file(&db_path);
    }

    /// Monotonic unique suffix for temp file names (avoids pulling in an
    /// extra uuid dependency just for tests).
    fn unique_suffix() -> String {
        use std::sync::atomic::{AtomicU64, Ordering as AOrdering};
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let n = COUNTER.fetch_add(1, AOrdering::SeqCst);
        format!("{}", n)
    }
}

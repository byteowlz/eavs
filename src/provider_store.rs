//! Persistent storage for providers and models.
//!
//! Provides CRUD operations for provider configurations and model shortlists.
//! Uses SQLite for persistence with in-memory caching for fast lookups.

use crate::config::ModelShortlistEntry;
use chrono::{DateTime, Utc};
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use sqlx::{sqlite::SqlitePoolOptions, Pool, Sqlite};
use std::path::Path;
use std::sync::Arc;

/// Provider entry with metadata.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderEntry {
    /// Provider name (e.g., "openai", "anthropic")
    pub name: String,
    /// Provider configuration (JSON)
    pub config: serde_json::Value,
    /// When this provider was added/modified
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    /// Whether this provider is enabled
    pub enabled: bool,
}

/// Create/update provider request.
#[derive(Debug, Clone, Deserialize)]
pub struct ProviderRequest {
    /// Provider name
    pub name: String,
    /// Provider configuration
    pub config: serde_json::Value,
    /// Whether to enable the provider (default: true)
    #[serde(default = "default_enabled")]
    pub enabled: bool,
}

fn default_enabled() -> bool {
    true
}

/// Add model to provider shortlist request.
#[derive(Debug, Clone, Deserialize)]
pub struct AddModelRequest {
    /// Provider name
    pub provider: String,
    /// Model entry to add
    pub model: ModelShortlistEntry,
}

/// Remove model from provider shortlist request.
#[derive(Debug, Clone, Deserialize)]
pub struct RemoveModelRequest {
    /// Provider name
    pub provider: String,
    /// Model ID to remove
    pub model_id: String,
}

/// Errors from the provider store.
#[derive(Debug)]
pub enum ProviderStoreError {
    Database(String),
    Serialization(String),
    Io(String),
    NotFound,
    Conflict(String),
}

impl std::fmt::Display for ProviderStoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Database(e) => write!(f, "Database error: {}", e),
            Self::Serialization(e) => write!(f, "Serialization error: {}", e),
            Self::Io(e) => write!(f, "IO error: {}", e),
            Self::NotFound => write!(f, "Provider not found"),
            Self::Conflict(e) => write!(f, "Conflict: {}", e),
        }
    }
}

impl std::error::Error for ProviderStoreError {}

/// Persistent provider store with in-memory caching.
pub struct ProviderStore {
    /// SQLite connection pool
    pool: Pool<Sqlite>,
    /// In-memory cache for fast lookups (provider name -> ProviderEntry)
    cache: Arc<DashMap<String, ProviderEntry>>,
}

impl ProviderStore {
    /// Create a new provider store, initializing the database if needed.
    pub async fn new(db_path: impl AsRef<Path>) -> Result<Self, ProviderStoreError> {
        let db_path = db_path.as_ref();

        // Ensure parent directory exists
        if let Some(parent) = db_path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| ProviderStoreError::Io(e.to_string()))?;
        }

        let db_url = format!("sqlite:{}?mode=rwc", db_path.display());

        let pool = SqlitePoolOptions::new()
            .max_connections(5)
            .connect(&db_url)
            .await
            .map_err(|e| ProviderStoreError::Database(e.to_string()))?;

        // Run migrations
        Self::run_migrations(&pool).await?;

        let cache = Arc::new(DashMap::new());

        let store = Self { pool, cache };

        // Populate cache from database
        store.refresh_cache().await?;

        Ok(store)
    }

    /// Create an in-memory store (for testing).
    #[cfg(test)]
    pub async fn in_memory() -> Result<Self, ProviderStoreError> {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .map_err(|e| ProviderStoreError::Database(e.to_string()))?;

        Self::run_migrations(&pool).await?;

        Ok(Self {
            pool,
            cache: Arc::new(DashMap::new()),
        })
    }

    async fn run_migrations(pool: &Pool<Sqlite>) -> Result<(), ProviderStoreError> {
        sqlx::query(
            r#"
            CREATE TABLE IF NOT EXISTS providers (
                name TEXT PRIMARY KEY,
                config TEXT NOT NULL,
                created_at TEXT NOT NULL,
                updated_at TEXT NOT NULL,
                enabled INTEGER NOT NULL DEFAULT 1
            );

            CREATE INDEX IF NOT EXISTS idx_providers_enabled ON providers(enabled);
            "#,
        )
        .execute(pool)
        .await
        .map_err(|e| ProviderStoreError::Database(e.to_string()))?;

        // Model shortlist table
        sqlx::query(
            r#"
            CREATE TABLE IF NOT EXISTS provider_models (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                provider_name TEXT NOT NULL,
                model_id TEXT NOT NULL,
                model_data TEXT NOT NULL,
                created_at TEXT NOT NULL,
                FOREIGN KEY (provider_name) REFERENCES providers(name) ON DELETE CASCADE,
                UNIQUE(provider_name, model_id)
            );

            CREATE INDEX IF NOT EXISTS idx_provider_models_provider ON provider_models(provider_name);
            "#,
        )
        .execute(pool)
        .await
        .map_err(|e| ProviderStoreError::Database(e.to_string()))?;

        Ok(())
    }

    /// Refresh the in-memory cache from the database.
    pub async fn refresh_cache(&self) -> Result<(), ProviderStoreError> {
        let rows: Vec<ProviderRow> = sqlx::query_as(
            "SELECT name, config, created_at, updated_at, enabled FROM providers WHERE enabled = 1",
        )
        .fetch_all(&self.pool)
        .await
        .map_err(|e| ProviderStoreError::Database(e.to_string()))?;

        self.cache.clear();
        for row in rows {
            if let Ok(entry) = row.to_entry() {
                self.cache.insert(entry.name.clone(), entry);
            }
        }

        Ok(())
    }

    /// Create or update a provider.
    pub async fn upsert_provider(
        &self,
        request: ProviderRequest,
    ) -> Result<ProviderEntry, ProviderStoreError> {
        let now = Utc::now();

        // Check if provider already exists
        let existing =
            sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM providers WHERE name = ?")
                .bind(&request.name)
                .fetch_one(&self.pool)
                .await
                .map_err(|e| ProviderStoreError::Database(e.to_string()))?;

        let config_json = serde_json::to_string(&request.config)
            .map_err(|e| ProviderStoreError::Serialization(e.to_string()))?;

        if existing > 0 {
            // Update existing provider
            sqlx::query(
                r#"
                UPDATE providers
                SET config = ?, updated_at = ?, enabled = ?
                WHERE name = ?
                "#,
            )
            .bind(&config_json)
            .bind(now.to_rfc3339())
            .bind(request.enabled as i32)
            .bind(&request.name)
            .execute(&self.pool)
            .await
            .map_err(|e: sqlx::Error| ProviderStoreError::Database(e.to_string()))?;
        } else {
            // Insert new provider
            sqlx::query(
                r#"
                INSERT INTO providers (name, config, created_at, updated_at, enabled)
                VALUES (?, ?, ?, ?, ?)
                "#,
            )
            .bind(&request.name)
            .bind(&config_json)
            .bind(now.to_rfc3339())
            .bind(now.to_rfc3339())
            .bind(request.enabled as i32)
            .execute(&self.pool)
            .await
            .map_err(|e| ProviderStoreError::Database(e.to_string()))?;
        }

        let entry = ProviderEntry {
            name: request.name.clone(),
            config: request.config,
            created_at: if existing > 0 {
                // Get original created_at
                sqlx::query_scalar::<_, String>("SELECT created_at FROM providers WHERE name = ?")
                    .bind(&request.name)
                    .fetch_one(&self.pool)
                    .await
                    .map_err(|e| ProviderStoreError::Database(e.to_string()))?
                    .parse::<DateTime<Utc>>()
                    .map_err(|e: chrono::ParseError| {
                        ProviderStoreError::Serialization(e.to_string())
                    })?
            } else {
                now
            },
            updated_at: now,
            enabled: request.enabled,
        };

        // Update cache
        if entry.enabled {
            self.cache.insert(entry.name.clone(), entry.clone());
        } else {
            self.cache.remove(&entry.name);
        }

        Ok(entry)
    }

    /// Get a provider by name.
    pub fn get_provider(&self, name: &str) -> Option<ProviderEntry> {
        self.cache.get(name).map(|v| v.clone())
    }

    /// List all providers.
    pub fn list_providers(&self) -> Vec<ProviderEntry> {
        self.cache
            .iter()
            .map(|entry| entry.value().clone())
            .collect()
    }

    /// Delete a provider.
    pub async fn delete_provider(&self, name: &str) -> Result<bool, ProviderStoreError> {
        let result = sqlx::query("DELETE FROM providers WHERE name = ?")
            .bind(name)
            .execute(&self.pool)
            .await
            .map_err(|e| ProviderStoreError::Database(e.to_string()))?;

        if result.rows_affected() > 0 {
            self.cache.remove(name);
            Ok(true)
        } else {
            Ok(false)
        }
    }

    /// Add a model to a provider's shortlist.
    pub async fn add_model(
        &self,
        provider_name: &str,
        model: ModelShortlistEntry,
    ) -> Result<(), ProviderStoreError> {
        // Check if provider exists
        let exists: Option<i64> =
            sqlx::query_scalar("SELECT COUNT(*) FROM providers WHERE name = ?")
                .bind(provider_name)
                .fetch_optional(&self.pool)
                .await
                .map_err(|e| ProviderStoreError::Database(e.to_string()))?;

        if exists.is_none() || exists == Some(0) {
            return Err(ProviderStoreError::NotFound);
        }

        let model_data_json = serde_json::to_string(&model)
            .map_err(|e| ProviderStoreError::Serialization(e.to_string()))?;

        sqlx::query(
            r#"
            INSERT OR REPLACE INTO provider_models (provider_name, model_id, model_data, created_at)
            VALUES (?, ?, ?, ?)
            "#,
        )
        .bind(provider_name)
        .bind(&model.id)
        .bind(&model_data_json)
        .bind(Utc::now().to_rfc3339())
        .execute(&self.pool)
        .await
        .map_err(|e| ProviderStoreError::Database(e.to_string()))?;

        Ok(())
    }

    /// Remove a model from a provider's shortlist.
    pub async fn remove_model(
        &self,
        provider_name: &str,
        model_id: &str,
    ) -> Result<bool, ProviderStoreError> {
        let result =
            sqlx::query("DELETE FROM provider_models WHERE provider_name = ? AND model_id = ?")
                .bind(provider_name)
                .bind(model_id)
                .execute(&self.pool)
                .await
                .map_err(|e| ProviderStoreError::Database(e.to_string()))?;

        Ok(result.rows_affected() > 0)
    }

    /// Get all models for a provider.
    pub async fn get_models(
        &self,
        provider_name: &str,
    ) -> Result<Vec<ModelShortlistEntry>, ProviderStoreError> {
        let rows: Vec<ModelRow> = sqlx::query_as(
            "SELECT model_data FROM provider_models WHERE provider_name = ? ORDER BY created_at",
        )
        .bind(provider_name)
        .fetch_all(&self.pool)
        .await
        .map_err(|e| ProviderStoreError::Database(e.to_string()))?;

        let mut models = Vec::new();
        for row in rows {
            if let Ok(model) = row.to_model() {
                models.push(model);
            }
        }

        Ok(models)
    }
}

/// Database row for providers.
#[derive(sqlx::FromRow)]
struct ProviderRow {
    name: String,
    config: String,
    created_at: String,
    updated_at: String,
    enabled: i32,
}

impl ProviderRow {
    fn to_entry(self) -> Result<ProviderEntry, ProviderStoreError> {
        let config: serde_json::Value = serde_json::from_str(&self.config)
            .map_err(|e| ProviderStoreError::Serialization(e.to_string()))?;

        let created_at = DateTime::parse_from_rfc3339(&self.created_at)
            .map_err(|e| ProviderStoreError::Serialization(e.to_string()))?
            .with_timezone(&Utc);

        let updated_at = DateTime::parse_from_rfc3339(&self.updated_at)
            .map_err(|e| ProviderStoreError::Serialization(e.to_string()))?
            .with_timezone(&Utc);

        Ok(ProviderEntry {
            name: self.name,
            config,
            created_at,
            updated_at,
            enabled: self.enabled != 0,
        })
    }
}

/// Database row for models.
#[derive(sqlx::FromRow)]
struct ModelRow {
    model_data: String,
}

impl ModelRow {
    fn to_model(self) -> Result<ModelShortlistEntry, ProviderStoreError> {
        serde_json::from_str(&self.model_data)
            .map_err(|e| ProviderStoreError::Serialization(e.to_string()))
    }
}

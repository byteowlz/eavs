use crate::oauth::types::{OAuthCredentials, OAuthProvider};
use sqlx::{sqlite::SqlitePoolOptions, Pool, Sqlite};
use std::path::Path;

pub struct OAuthStore {
    pool: Pool<Sqlite>,
}

impl OAuthStore {
    pub async fn new(db_path: impl AsRef<Path>) -> Result<Self, OAuthStoreError> {
        let db_path = db_path.as_ref();
        if let Some(parent) = db_path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| OAuthStoreError::Io(e.to_string()))?;
        }

        let db_url = format!("sqlite:{}?mode=rwc", db_path.display());
        let pool = SqlitePoolOptions::new()
            .max_connections(5)
            .connect(&db_url)
            .await
            .map_err(|e| OAuthStoreError::Database(e.to_string()))?;

        Self::run_migrations(&pool).await?;

        Ok(Self { pool })
    }

    #[cfg(test)]
    pub async fn in_memory() -> Result<Self, OAuthStoreError> {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .map_err(|e| OAuthStoreError::Database(e.to_string()))?;

        Self::run_migrations(&pool).await?;

        Ok(Self { pool })
    }

    async fn run_migrations(pool: &Pool<Sqlite>) -> Result<(), OAuthStoreError> {
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
        .map_err(|e| OAuthStoreError::Database(e.to_string()))?;

        Ok(())
    }

    pub async fn upsert_credentials(
        &self,
        credentials: &OAuthCredentials,
    ) -> Result<(), OAuthStoreError> {
        let extra_data = credentials
            .extra_data
            .as_ref()
            .map(serde_json::to_string)
            .transpose()
            .map_err(|e| OAuthStoreError::Serialization(e.to_string()))?;

        sqlx::query(
            r#"
            INSERT INTO oauth_credentials (user_id, provider, access_token, refresh_token, expires_at, extra_data)
            VALUES (?, ?, ?, ?, ?, ?)
            ON CONFLICT(user_id, provider) DO UPDATE SET
                access_token = excluded.access_token,
                refresh_token = excluded.refresh_token,
                expires_at = excluded.expires_at,
                extra_data = excluded.extra_data
            "#,
        )
        .bind(&credentials.user_id)
        .bind(credentials.provider.as_str())
        .bind(&credentials.access_token)
        .bind(&credentials.refresh_token)
        .bind(credentials.expires_at)
        .bind(extra_data)
        .execute(&self.pool)
        .await
        .map_err(|e| OAuthStoreError::Database(e.to_string()))?;

        Ok(())
    }

    pub async fn get_credentials(
        &self,
        user_id: &str,
        provider: OAuthProvider,
    ) -> Result<Option<OAuthCredentials>, OAuthStoreError> {
        let row: Option<OAuthRow> = sqlx::query_as(
            "SELECT user_id, provider, access_token, refresh_token, expires_at, extra_data FROM oauth_credentials WHERE user_id = ? AND provider = ?",
        )
        .bind(user_id)
        .bind(provider.as_str())
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| OAuthStoreError::Database(e.to_string()))?;

        Ok(row.map(|r| r.into_credentials()))
    }

    pub async fn delete_credentials(
        &self,
        user_id: &str,
        provider: OAuthProvider,
    ) -> Result<bool, OAuthStoreError> {
        let result =
            sqlx::query("DELETE FROM oauth_credentials WHERE user_id = ? AND provider = ?")
                .bind(user_id)
                .bind(provider.as_str())
                .execute(&self.pool)
                .await
                .map_err(|e| OAuthStoreError::Database(e.to_string()))?;

        Ok(result.rows_affected() > 0)
    }

    pub async fn list_providers(&self, user_id: &str) -> Result<Vec<String>, OAuthStoreError> {
        let rows: Vec<(String,)> = sqlx::query_as(
            "SELECT provider FROM oauth_credentials WHERE user_id = ? ORDER BY provider",
        )
        .bind(user_id)
        .fetch_all(&self.pool)
        .await
        .map_err(|e| OAuthStoreError::Database(e.to_string()))?;

        Ok(rows.into_iter().map(|r| r.0).collect())
    }
}

#[derive(sqlx::FromRow)]
struct OAuthRow {
    user_id: String,
    provider: String,
    access_token: String,
    refresh_token: String,
    expires_at: i64,
    extra_data: Option<String>,
}

impl OAuthRow {
    fn into_credentials(self) -> OAuthCredentials {
        let extra_data = self
            .extra_data
            .as_ref()
            .and_then(|s| serde_json::from_str::<serde_json::Value>(s).ok());

        OAuthCredentials {
            user_id: self.user_id,
            provider: OAuthProvider::from_str(&self.provider).unwrap_or(OAuthProvider::OpenAICodex),
            access_token: self.access_token,
            refresh_token: self.refresh_token,
            expires_at: self.expires_at,
            extra_data,
        }
    }
}

#[derive(Debug)]
pub enum OAuthStoreError {
    Database(String),
    Serialization(String),
    Io(String),
}

impl std::fmt::Display for OAuthStoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Database(e) => write!(f, "Database error: {}", e),
            Self::Serialization(e) => write!(f, "Serialization error: {}", e),
            Self::Io(e) => write!(f, "IO error: {}", e),
        }
    }
}

impl std::error::Error for OAuthStoreError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_upsert_and_get_credentials() {
        let store = OAuthStore::in_memory().await.unwrap();
        let creds = OAuthCredentials {
            user_id: "user-1".to_string(),
            provider: OAuthProvider::Anthropic,
            access_token: "access".to_string(),
            refresh_token: "refresh".to_string(),
            expires_at: 123,
            extra_data: Some(serde_json::json!({"scope": "basic"})),
        };

        store.upsert_credentials(&creds).await.unwrap();
        let fetched = store
            .get_credentials("user-1", OAuthProvider::Anthropic)
            .await
            .unwrap()
            .unwrap();

        assert_eq!(fetched.access_token, "access");
        assert_eq!(fetched.refresh_token, "refresh");
        assert_eq!(fetched.expires_at, 123);
    }

    #[tokio::test]
    async fn test_list_and_delete_credentials() {
        let store = OAuthStore::in_memory().await.unwrap();
        let creds = OAuthCredentials {
            user_id: "user-2".to_string(),
            provider: OAuthProvider::GithubCopilot,
            access_token: "access".to_string(),
            refresh_token: "refresh".to_string(),
            expires_at: 999,
            extra_data: None,
        };

        store.upsert_credentials(&creds).await.unwrap();
        let providers = store.list_providers("user-2").await.unwrap();
        assert_eq!(providers, vec!["github-copilot".to_string()]);

        let deleted = store
            .delete_credentials("user-2", OAuthProvider::GithubCopilot)
            .await
            .unwrap();
        assert!(deleted);
    }
}

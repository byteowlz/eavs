use crate::oauth::types::{OAuthCredentials, OAuthProvider};
use sqlx::{sqlite::SqlitePoolOptions, Pool, Sqlite};
use std::path::Path;

// ---------------------------------------------------------------------------
// Error type
// ---------------------------------------------------------------------------

#[derive(Debug)]
pub enum OAuthStoreError {
    Database(String),
    Serialization(String),
    Io(String),
    Keychain(String),
}

impl std::fmt::Display for OAuthStoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Database(e) => write!(f, "Database error: {}", e),
            Self::Serialization(e) => write!(f, "Serialization error: {}", e),
            Self::Io(e) => write!(f, "IO error: {}", e),
            Self::Keychain(e) => write!(f, "Keychain error: {}", e),
        }
    }
}

impl std::error::Error for OAuthStoreError {}

// ---------------------------------------------------------------------------
// Backend configuration
// ---------------------------------------------------------------------------

/// Which credential storage backend to use for OAuth tokens.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum OAuthBackend {
    /// System keychain (macOS Keychain, libsecret, Windows Credential Manager).
    /// Falls back to SQLite if the keychain is unavailable.
    #[default]
    Keychain,
    /// SQLite database (plaintext, same file as virtual API keys).
    Sqlite,
}

impl OAuthBackend {
    pub fn from_str(s: &str) -> Option<Self> {
        match s.to_lowercase().as_str() {
            "keychain" | "keyring" | "system" => Some(Self::Keychain),
            "sqlite" | "database" | "db" => Some(Self::Sqlite),
            _ => None,
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Keychain => "keychain",
            Self::Sqlite => "sqlite",
        }
    }
}

// ---------------------------------------------------------------------------
// OAuthStore -- public facade
// ---------------------------------------------------------------------------

/// Credential store for OAuth tokens.
///
/// Dispatches to either a system keychain backend or a SQLite backend,
/// depending on configuration. When `keychain` is requested but unavailable,
/// falls back to SQLite with a warning.
pub struct OAuthStore {
    backend: Box<dyn CredentialBackend + Send + Sync>,
    backend_name: &'static str,
}

impl OAuthStore {
    /// Create a new store with the specified backend.
    ///
    /// `db_path` is required even for the keychain backend because it is used
    /// as a fallback when the system keychain is not available.
    pub async fn new(
        db_path: impl AsRef<Path>,
        backend: OAuthBackend,
    ) -> Result<Self, OAuthStoreError> {
        match backend {
            OAuthBackend::Keychain => match KeychainBackend::new() {
                Ok(kb) => {
                    tracing::info!("OAuth credential storage: system keychain");
                    Ok(Self {
                        backend: Box::new(kb),
                        backend_name: "keychain",
                    })
                }
                Err(e) => {
                    tracing::warn!(
                        "System keychain unavailable ({}), falling back to SQLite",
                        e
                    );
                    let sb = SqliteBackend::new(db_path).await?;
                    Ok(Self {
                        backend: Box::new(sb),
                        backend_name: "sqlite (fallback)",
                    })
                }
            },
            OAuthBackend::Sqlite => {
                let sb = SqliteBackend::new(db_path).await?;
                tracing::info!("OAuth credential storage: SQLite");
                Ok(Self {
                    backend: Box::new(sb),
                    backend_name: "sqlite",
                })
            }
        }
    }

    /// Create a store that always uses SQLite (convenience shorthand).
    #[allow(dead_code)]
    pub async fn new_sqlite(db_path: impl AsRef<Path>) -> Result<Self, OAuthStoreError> {
        Self::new(db_path, OAuthBackend::Sqlite).await
    }

    /// Which backend is actually in use (for diagnostics / status display).
    pub fn backend_name(&self) -> &'static str {
        self.backend_name
    }

    pub async fn upsert_credentials(
        &self,
        credentials: &OAuthCredentials,
    ) -> Result<(), OAuthStoreError> {
        self.backend.upsert_credentials(credentials).await
    }

    pub async fn get_credentials(
        &self,
        user_id: &str,
        provider: OAuthProvider,
    ) -> Result<Option<OAuthCredentials>, OAuthStoreError> {
        self.backend
            .get_credentials(user_id, provider, "default")
            .await
    }

    /// Get credentials for a specific account label.
    pub async fn get_credentials_for_account(
        &self,
        user_id: &str,
        provider: OAuthProvider,
        account_label: &str,
    ) -> Result<Option<OAuthCredentials>, OAuthStoreError> {
        self.backend
            .get_credentials(user_id, provider, account_label)
            .await
    }

    pub async fn delete_credentials(
        &self,
        user_id: &str,
        provider: OAuthProvider,
    ) -> Result<bool, OAuthStoreError> {
        self.backend
            .delete_credentials(user_id, provider, "default")
            .await
    }

    /// Delete credentials for a specific account label.
    pub async fn delete_credentials_for_account(
        &self,
        user_id: &str,
        provider: OAuthProvider,
        account_label: &str,
    ) -> Result<bool, OAuthStoreError> {
        self.backend
            .delete_credentials(user_id, provider, account_label)
            .await
    }

    pub async fn list_providers(&self, user_id: &str) -> Result<Vec<String>, OAuthStoreError> {
        self.backend.list_providers(user_id).await
    }
}

// ---------------------------------------------------------------------------
// Credential backend trait
// ---------------------------------------------------------------------------

/// Abstraction over different credential storage mechanisms.
#[async_trait::async_trait]
trait CredentialBackend {
    async fn upsert_credentials(
        &self,
        credentials: &OAuthCredentials,
    ) -> Result<(), OAuthStoreError>;

    async fn get_credentials(
        &self,
        user_id: &str,
        provider: OAuthProvider,
        account_label: &str,
    ) -> Result<Option<OAuthCredentials>, OAuthStoreError>;

    async fn delete_credentials(
        &self,
        user_id: &str,
        provider: OAuthProvider,
        account_label: &str,
    ) -> Result<bool, OAuthStoreError>;

    async fn list_providers(&self, user_id: &str) -> Result<Vec<String>, OAuthStoreError>;
}

// ---------------------------------------------------------------------------
// SQLite backend (existing implementation, extracted)
// ---------------------------------------------------------------------------

struct SqliteBackend {
    pool: Pool<Sqlite>,
}

impl SqliteBackend {
    async fn new(db_path: impl AsRef<Path>) -> Result<Self, OAuthStoreError> {
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

    async fn run_migrations(pool: &Pool<Sqlite>) -> Result<(), OAuthStoreError> {
        // Initial schema
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

        // Migration: add account_label column for multi-account support.
        // ALTER TABLE ADD COLUMN is idempotent in SQLite (fails silently if column exists).
        let has_account_label: bool = sqlx::query_scalar::<_, i32>(
            "SELECT COUNT(*) FROM pragma_table_info('oauth_credentials') WHERE name = 'account_label'"
        )
        .fetch_one(pool)
        .await
        .map_err(|e| OAuthStoreError::Database(e.to_string()))? > 0;

        if !has_account_label {
            // Add the column with a default
            sqlx::query(
                "ALTER TABLE oauth_credentials ADD COLUMN account_label TEXT NOT NULL DEFAULT 'default'"
            )
            .execute(pool)
            .await
            .map_err(|e| OAuthStoreError::Database(e.to_string()))?;

            // Recreate the table with the new primary key.
            // SQLite doesn't support ALTER TABLE to change PK, so we migrate.
            sqlx::query(
                r#"
                CREATE TABLE IF NOT EXISTS oauth_credentials_new (
                    user_id TEXT NOT NULL,
                    provider TEXT NOT NULL,
                    account_label TEXT NOT NULL DEFAULT 'default',
                    access_token TEXT NOT NULL,
                    refresh_token TEXT NOT NULL,
                    expires_at INTEGER NOT NULL,
                    extra_data TEXT,
                    PRIMARY KEY (user_id, provider, account_label)
                );
                INSERT OR REPLACE INTO oauth_credentials_new
                    SELECT user_id, provider, account_label, access_token, refresh_token, expires_at, extra_data
                    FROM oauth_credentials;
                DROP TABLE oauth_credentials;
                ALTER TABLE oauth_credentials_new RENAME TO oauth_credentials;
                "#,
            )
            .execute(pool)
            .await
            .map_err(|e| OAuthStoreError::Database(e.to_string()))?;
        }

        Ok(())
    }
}

#[derive(sqlx::FromRow)]
struct OAuthRow {
    user_id: String,
    provider: String,
    #[sqlx(default)]
    account_label: String,
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
            account_label: if self.account_label.is_empty() {
                "default".to_string()
            } else {
                self.account_label
            },
            access_token: self.access_token,
            refresh_token: self.refresh_token,
            expires_at: self.expires_at,
            extra_data,
        }
    }
}

#[async_trait::async_trait]
impl CredentialBackend for SqliteBackend {
    async fn upsert_credentials(
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
            INSERT INTO oauth_credentials (user_id, provider, account_label, access_token, refresh_token, expires_at, extra_data)
            VALUES (?, ?, ?, ?, ?, ?, ?)
            ON CONFLICT(user_id, provider, account_label) DO UPDATE SET
                access_token = excluded.access_token,
                refresh_token = excluded.refresh_token,
                expires_at = excluded.expires_at,
                extra_data = excluded.extra_data
            "#,
        )
        .bind(&credentials.user_id)
        .bind(credentials.provider.as_str())
        .bind(&credentials.account_label)
        .bind(&credentials.access_token)
        .bind(&credentials.refresh_token)
        .bind(credentials.expires_at)
        .bind(extra_data)
        .execute(&self.pool)
        .await
        .map_err(|e| OAuthStoreError::Database(e.to_string()))?;

        Ok(())
    }

    async fn get_credentials(
        &self,
        user_id: &str,
        provider: OAuthProvider,
        account_label: &str,
    ) -> Result<Option<OAuthCredentials>, OAuthStoreError> {
        let row: Option<OAuthRow> = sqlx::query_as(
            "SELECT user_id, provider, account_label, access_token, refresh_token, expires_at, extra_data FROM oauth_credentials WHERE user_id = ? AND provider = ? AND account_label = ?",
        )
        .bind(user_id)
        .bind(provider.as_str())
        .bind(account_label)
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| OAuthStoreError::Database(e.to_string()))?;

        Ok(row.map(|r| r.into_credentials()))
    }

    async fn delete_credentials(
        &self,
        user_id: &str,
        provider: OAuthProvider,
        account_label: &str,
    ) -> Result<bool, OAuthStoreError> {
        let result = sqlx::query(
            "DELETE FROM oauth_credentials WHERE user_id = ? AND provider = ? AND account_label = ?",
        )
        .bind(user_id)
        .bind(provider.as_str())
        .bind(account_label)
        .execute(&self.pool)
        .await
        .map_err(|e| OAuthStoreError::Database(e.to_string()))?;

        Ok(result.rows_affected() > 0)
    }

    async fn list_providers(&self, user_id: &str) -> Result<Vec<String>, OAuthStoreError> {
        let rows: Vec<(String,)> = sqlx::query_as(
            "SELECT DISTINCT provider FROM oauth_credentials WHERE user_id = ? ORDER BY provider",
        )
        .bind(user_id)
        .fetch_all(&self.pool)
        .await
        .map_err(|e| OAuthStoreError::Database(e.to_string()))?;

        Ok(rows.into_iter().map(|r| r.0).collect())
    }
}

// ---------------------------------------------------------------------------
// Keychain backend (system keychain via `keyring` crate)
// ---------------------------------------------------------------------------

/// Service name used to identify eavs entries in the system keychain.
const KEYCHAIN_SERVICE: &str = "eavs-oauth";

/// Stores OAuth credentials in the OS keychain.
///
/// Each `(user_id, provider)` pair maps to a single keychain entry whose
/// "username" is `{user_id}/{provider}` and whose "password" is the
/// JSON-serialised `OAuthCredentials`.
///
/// To support `list_providers`, a separate index entry is maintained under the
/// username `{user_id}/__index__` containing a JSON array of provider strings.
struct KeychainBackend {
    // Marker to prove we successfully probed the keychain at construction time.
    _probe_ok: (),
}

impl KeychainBackend {
    /// Create a new keychain backend, verifying the system keychain is accessible.
    fn new() -> Result<Self, OAuthStoreError> {
        // Probe: try to access a dummy entry to check the keychain is reachable.
        // `get_password` returning NoEntry is fine -- it means the keychain works.
        let entry = keyring::Entry::new(KEYCHAIN_SERVICE, "__probe__")
            .map_err(|e| OAuthStoreError::Keychain(format!("Cannot access keychain: {}", e)))?;
        match entry.get_password() {
            Ok(_) => {}
            Err(keyring::Error::NoEntry) => {}
            Err(keyring::Error::PlatformFailure(ref e)) => {
                let msg = e.to_string();
                if msg.contains("org.freedesktop.DBus.Error") || msg.contains("Secret Service") {
                    return Err(OAuthStoreError::Keychain(format!(
                        "No secret service (DBus) available: {}",
                        msg
                    )));
                }
                // Other platform failures at probe time -- keychain is present
                // but may have transient issues; proceed optimistically.
            }
            Err(keyring::Error::NoStorageAccess(_)) => {
                return Err(OAuthStoreError::Keychain(
                    "No credential storage accessible".to_string(),
                ));
            }
            // Other errors (e.g. Ambiguous) are unexpected at probe time but
            // don't indicate the keychain is unreachable.
            Err(_) => {}
        }

        Ok(Self { _probe_ok: () })
    }

    /// Build the keychain "username" for a credential entry.
    fn entry_key(user_id: &str, provider: OAuthProvider) -> String {
        format!("{}/{}/default", user_id, provider.as_str())
    }

    fn entry_key_with_label(user_id: &str, provider: OAuthProvider, account_label: &str) -> String {
        if account_label.is_empty() || account_label == "default" {
            format!("{}/{}/default", user_id, provider.as_str())
        } else {
            format!("{}/{}/{}", user_id, provider.as_str(), account_label)
        }
    }

    /// Build the keychain "username" for the per-user provider index.
    fn index_key(user_id: &str) -> String {
        format!("{}/__index__", user_id)
    }

    /// Read the provider index for a user. Returns an empty vec on missing/corrupt data.
    fn read_index(&self, user_id: &str) -> Vec<String> {
        let key = Self::index_key(user_id);
        let entry = match keyring::Entry::new(KEYCHAIN_SERVICE, &key) {
            Ok(e) => e,
            Err(_) => return Vec::new(),
        };
        match entry.get_password() {
            Ok(json) => serde_json::from_str::<Vec<String>>(&json).unwrap_or_default(),
            Err(_) => Vec::new(),
        }
    }

    /// Write the provider index for a user.
    fn write_index(&self, user_id: &str, providers: &[String]) -> Result<(), OAuthStoreError> {
        let key = Self::index_key(user_id);
        let entry = keyring::Entry::new(KEYCHAIN_SERVICE, &key)
            .map_err(|e| OAuthStoreError::Keychain(e.to_string()))?;
        let json = serde_json::to_string(providers)
            .map_err(|e| OAuthStoreError::Serialization(e.to_string()))?;
        entry
            .set_password(&json)
            .map_err(|e| OAuthStoreError::Keychain(e.to_string()))?;
        Ok(())
    }

    /// Add a provider to the index (idempotent).
    fn index_add(&self, user_id: &str, provider: &str) -> Result<(), OAuthStoreError> {
        let mut providers = self.read_index(user_id);
        let p = provider.to_string();
        if !providers.contains(&p) {
            providers.push(p);
            providers.sort();
            self.write_index(user_id, &providers)?;
        }
        Ok(())
    }

    /// Remove a provider from the index.
    fn index_remove(&self, user_id: &str, provider: &str) -> Result<(), OAuthStoreError> {
        let mut providers = self.read_index(user_id);
        let before = providers.len();
        providers.retain(|p| p != provider);
        if providers.len() != before {
            if providers.is_empty() {
                // Clean up the index entry entirely
                let key = Self::index_key(user_id);
                if let Ok(entry) = keyring::Entry::new(KEYCHAIN_SERVICE, &key) {
                    let _ = entry.delete_credential();
                }
            } else {
                self.write_index(user_id, &providers)?;
            }
        }
        Ok(())
    }
}

#[async_trait::async_trait]
impl CredentialBackend for KeychainBackend {
    async fn upsert_credentials(
        &self,
        credentials: &OAuthCredentials,
    ) -> Result<(), OAuthStoreError> {
        let key = Self::entry_key_with_label(
            &credentials.user_id,
            credentials.provider,
            &credentials.account_label,
        );
        let json = serde_json::to_string(credentials)
            .map_err(|e| OAuthStoreError::Serialization(e.to_string()))?;

        let entry = keyring::Entry::new(KEYCHAIN_SERVICE, &key)
            .map_err(|e| OAuthStoreError::Keychain(e.to_string()))?;
        entry
            .set_password(&json)
            .map_err(|e| OAuthStoreError::Keychain(e.to_string()))?;

        // Update the provider index
        self.index_add(&credentials.user_id, credentials.provider.as_str())?;

        Ok(())
    }

    async fn get_credentials(
        &self,
        user_id: &str,
        provider: OAuthProvider,
        account_label: &str,
    ) -> Result<Option<OAuthCredentials>, OAuthStoreError> {
        let key = Self::entry_key_with_label(user_id, provider, account_label);
        let entry = keyring::Entry::new(KEYCHAIN_SERVICE, &key)
            .map_err(|e| OAuthStoreError::Keychain(e.to_string()))?;

        match entry.get_password() {
            Ok(json) => {
                let creds: OAuthCredentials = serde_json::from_str(&json)
                    .map_err(|e| OAuthStoreError::Serialization(e.to_string()))?;
                Ok(Some(creds))
            }
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(e) => Err(OAuthStoreError::Keychain(e.to_string())),
        }
    }

    async fn delete_credentials(
        &self,
        user_id: &str,
        provider: OAuthProvider,
        account_label: &str,
    ) -> Result<bool, OAuthStoreError> {
        let key = Self::entry_key_with_label(user_id, provider, account_label);
        let entry = keyring::Entry::new(KEYCHAIN_SERVICE, &key)
            .map_err(|e| OAuthStoreError::Keychain(e.to_string()))?;

        let deleted = match entry.delete_credential() {
            Ok(()) => true,
            Err(keyring::Error::NoEntry) => false,
            Err(e) => return Err(OAuthStoreError::Keychain(e.to_string())),
        };

        if deleted {
            self.index_remove(user_id, provider.as_str())?;
        }

        Ok(deleted)
    }

    async fn list_providers(&self, user_id: &str) -> Result<Vec<String>, OAuthStoreError> {
        Ok(self.read_index(user_id))
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// Create an in-memory SQLite-backed store for tests.
    async fn test_store() -> OAuthStore {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        SqliteBackend::run_migrations(&pool).await.unwrap();
        OAuthStore {
            backend: Box::new(SqliteBackend { pool }),
            backend_name: "sqlite (test)",
        }
    }

    #[tokio::test]
    async fn test_upsert_and_get_credentials() {
        let store = test_store().await;
        let creds = OAuthCredentials {
            user_id: "user-1".to_string(),
            provider: OAuthProvider::Anthropic,
            account_label: "default".to_string(),
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
        let store = test_store().await;
        let creds = OAuthCredentials {
            user_id: "user-2".to_string(),
            provider: OAuthProvider::GithubCopilot,
            account_label: "default".to_string(),
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

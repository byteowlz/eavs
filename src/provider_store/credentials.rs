//! Write-only credential rotation. One active gateway process owns each store.
use super::{ProviderRow, ProviderStore, ProviderStoreError};
use chrono::Utc;
use serde::Serialize;

#[derive(Serialize)]
pub struct CredentialRevision {
    pub name: String,
    pub revision: String,
}

impl ProviderStore {
    pub async fn rotate_api_key(
        &self,
        name: &str,
        expected: &str,
        api_key: &str,
    ) -> Result<CredentialRevision, ProviderStoreError> {
        self.change_credential(name, expected, Some(api_key)).await
    }

    pub async fn rollback_api_key(
        &self,
        name: &str,
        expected: &str,
    ) -> Result<CredentialRevision, ProviderStoreError> {
        self.change_credential(name, expected, None).await
    }

    async fn change_credential(
        &self,
        name: &str,
        expected: &str,
        api_key: Option<&str>,
    ) -> Result<CredentialRevision, ProviderStoreError> {
        // Serde's DateTime metadata uses `Z`; stored revisions use `+00:00`.
        // Compare the same instant after canonicalizing, not presentation bytes.
        let expected = expected
            .parse::<chrono::DateTime<Utc>>()
            .map_err(|_| ProviderStoreError::Conflict)?
            .to_rfc3339();
        let expected = expected.as_str();
        let _guard = self.writes.lock().await;
        let mut transaction = self.pool.begin().await.map_err(database_error)?;
        let row: ProviderRow = sqlx::query_as(
            "SELECT name, config, created_at, updated_at, enabled FROM providers WHERE name = ?",
        )
        .bind(name)
        .fetch_optional(&mut *transaction)
        .await
        .map_err(database_error)?
        .ok_or(ProviderStoreError::NotFound)?;
        if row.updated_at != expected {
            return Err(ProviderStoreError::Conflict);
        }
        let previous_config = row.config.clone();
        let mut entry = row.to_entry()?;
        entry.config =
            credential_config(&mut transaction, name, expected, entry.config, api_key).await?;
        // Ensure the new CAS revision differs even if clock precision repeats
        // or the wall clock moves backwards.
        entry.updated_at = Utc::now().max(entry.updated_at + chrono::Duration::nanoseconds(1));
        let revision = entry.updated_at.to_rfc3339();
        let config = serde_json::to_string(&entry.config).map_err(|_| {
            ProviderStoreError::Serialization("Cannot encode provider configuration".into())
        })?;
        let result = sqlx::query(
            "UPDATE providers SET config = ?, updated_at = ? WHERE name = ? AND updated_at = ?",
        )
        .bind(config)
        .bind(&revision)
        .bind(name)
        .bind(expected)
        .execute(&mut *transaction)
        .await
        .map_err(database_error)?;
        if result.rows_affected() != 1 {
            return Err(ProviderStoreError::Conflict);
        }
        if api_key.is_some() {
            sqlx::query("INSERT INTO provider_credential_rollback (name, current_revision, previous_config) VALUES (?, ?, ?) ON CONFLICT(name) DO UPDATE SET current_revision = excluded.current_revision, previous_config = excluded.previous_config")
                .bind(name).bind(&revision).bind(previous_config).execute(&mut *transaction).await.map_err(database_error)?;
        } else {
            sqlx::query("DELETE FROM provider_credential_rollback WHERE name = ?")
                .bind(name)
                .execute(&mut *transaction)
                .await
                .map_err(database_error)?;
        }
        transaction.commit().await.map_err(database_error)?;
        // Serialized writers prevent an older completed update from replacing
        // a newer snapshot. In-flight requests retain their original snapshot.
        if entry.enabled {
            self.cache.insert(name.to_string(), entry);
        }
        log_credential_change(name, &revision, api_key.is_none());
        Ok(CredentialRevision {
            name: name.to_string(),
            revision,
        })
    }
}

fn log_credential_change(name: &str, revision: &str, rollback: bool) {
    tracing::info!(
        provider = name,
        revision,
        rollback,
        "Provider credential revision activated"
    );
}

async fn credential_config(
    transaction: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    name: &str,
    expected: &str,
    mut config: serde_json::Value,
    api_key: Option<&str>,
) -> Result<serde_json::Value, ProviderStoreError> {
    if !config.is_object() {
        return Err(ProviderStoreError::Serialization(
            "Invalid provider configuration".into(),
        ));
    }
    if let Some(api_key) = api_key {
        config["api_key"] = serde_json::Value::String(api_key.to_string());
        return Ok(config);
    }
    let previous: Option<String> = sqlx::query_scalar("SELECT previous_config FROM provider_credential_rollback WHERE name = ? AND current_revision = ?")
        .bind(name).bind(expected).fetch_optional(&mut **transaction).await.map_err(database_error)?;
    serde_json::from_str(&previous.ok_or(ProviderStoreError::Conflict)?)
        .map_err(|_| ProviderStoreError::Serialization("Invalid rollback configuration".into()))
}

fn database_error(_: sqlx::Error) -> ProviderStoreError {
    // SQL diagnostics must never disclose bound credentials/configuration.
    ProviderStoreError::Database("Credential transaction failed".to_string())
}

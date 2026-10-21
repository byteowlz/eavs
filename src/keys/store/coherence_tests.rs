//! Cross-process metadata authority is independent of process-local metering.
use super::*;
use anyhow::Result;

#[tokio::test]
#[expect(
    clippy::panic_in_result_fn,
    reason = "Behavioral assertions in fallible fixture"
)]
async fn external_metadata_and_malformed_rows_fail_closed() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("keys.sqlite");
    let server = KeyStore::new(&path).await?;
    let writer = KeyStore::new(&path).await?;
    let response = writer.create_key(CreateKeyRequest::default()).await?;
    assert!(server.lookup_for_auth(&response.key).await?.is_some());
    for assignment in [
        "permissions='invalid'",
        "metadata='invalid'",
        "usage='invalid'",
        "expires_at='invalid'",
        "valid_after='invalid'",
        "created_at='invalid'",
    ] {
        let mut tx = writer.pool.begin().await?;
        sqlx::query(&format!("UPDATE virtual_keys SET {assignment}"))
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        assert!(
            server.lookup_for_auth(&response.key).await.is_err(),
            "{assignment}"
        );
        // Restore the entire fixture from the known writer snapshot, without
        // using any production auth-metadata update helper.
        sqlx::query("UPDATE virtual_keys SET permissions='{}', metadata='null', usage=?, expires_at=NULL, valid_after=NULL, created_at=?")
            .bind(serde_json::to_string(&KeyUsage::default())?)
            .bind(Utc::now().to_rfc3339())
            .execute(&writer.pool).await?;
    }
    writer.disable_key(&response.key_hash).await?;
    assert!(!server
        .lookup_for_auth(&response.key)
        .await?
        .ok_or_else(|| anyhow::anyhow!("row"))?
        .is_valid());
    writer.delete_key(&response.key_hash).await?;
    assert!(server.lookup_for_auth(&response.key).await?.is_none());
    server.pool.close().await;
    assert!(server.lookup_for_auth(&response.key).await.is_err());
    Ok(())
}

#[tokio::test(flavor = "current_thread")]
#[expect(
    clippy::panic_in_result_fn,
    reason = "Behavioral assertions in fallible fixture"
)]
async fn concurrent_auth_usage_and_flush_preserve_counters_and_authority() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("keys.sqlite");
    let server = KeyStore::new(&path).await?;
    let writer = KeyStore::new(&path).await?;
    let response = writer.create_key(CreateKeyRequest::default()).await?;
    server
        .lookup_for_auth(&response.key)
        .await?
        .ok_or_else(|| anyhow::anyhow!("row"))?;
    // External narrowing/revocation must survive both partial admin updates
    // and the usage-only flush, even though the server cached broader authority.
    sqlx::query(
        "UPDATE virtual_keys SET permissions='{\"allowed_providers\":[\"narrow\"]}', disabled=1",
    )
    .execute(&writer.pool)
    .await?;
    server
        .update_owner(&response.key_hash, Some("new owner".into()))
        .await?;
    server
        .update_oauth_user(&response.key_hash, Some("new user".into()))
        .await?;
    let hashes = std::iter::once(response.key_hash.clone()).collect();
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        let auth = async {
            for _ in 0..40 {
                server.lookup_for_auth(&response.key).await?;
            }
            Ok::<_, KeyStoreError>(())
        };
        let usage = async {
            for _ in 0..40 {
                server
                    .update_usage(&response.key_hash, 2, 3, 0, 0, 0.5, "m", "p")
                    .await?;
            }
            Ok::<_, KeyStoreError>(())
        };
        let flush = async {
            for _ in 0..40 {
                KeyStore::sync_keys_to_db(&server.pool, &server.cache, &hashes).await;
            }
        };
        let (a, u, ()) = tokio::join!(auth, usage, flush);
        a?;
        u?;
        Ok::<_, KeyStoreError>(())
    })
    .await??;
    let key = server
        .lookup_for_auth(&response.key)
        .await?
        .ok_or_else(|| anyhow::anyhow!("row"))?;
    assert_eq!(key.usage.total_requests, 40);
    assert_eq!(key.usage.total_input_tokens, 80);
    assert!((key.usage.total_spend_usd - 20.0).abs() < f64::EPSILON);
    assert!(!key.is_valid());
    assert!(!key.permissions.is_provider_allowed("p"));
    assert!(key.permissions.is_provider_allowed("narrow"));
    assert_eq!(key.owner.as_deref(), Some("new owner"));
    KeyStore::sync_keys_to_db(&server.pool, &server.cache, &hashes).await;
    let row: KeyRow = sqlx::query_as("SELECT * FROM virtual_keys")
        .fetch_one(&writer.pool)
        .await?;
    let stored = row.to_virtual_key()?;
    assert!(!stored.is_valid());
    assert!(!stored.permissions.is_provider_allowed("p"));
    assert_eq!(stored.usage.total_requests, 40);
    writer.delete_key(&response.key_hash).await?;
    KeyStore::sync_keys_to_db(&server.pool, &server.cache, &hashes).await;
    assert!(
        server.lookup_for_auth(&response.key).await?.is_none(),
        "usage flush must not resurrect a deleted row"
    );
    Ok(())
}

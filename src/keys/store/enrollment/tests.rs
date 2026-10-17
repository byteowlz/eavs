use super::{hash_key, Administrator, Invitation, InviteRequest, KeyStore, KeyStoreError, Utc, E};
use crate::keys::{CreateKeyRequest, CreateKeyResponse, KeyPermissions};
type TestError = Box<dyn std::error::Error + Send + Sync>;
fn admin() -> Result<Administrator, TestError> {
    Administrator::authenticate(Some("synthetic-root"), "synthetic-root")
        .map_err(|_| "admin".into())
}
async fn template(store: &KeyStore) -> Result<CreateKeyResponse, KeyStoreError> {
    store
        .create_key(CreateKeyRequest {
            owner: Some("owner".into()),
            metadata: serde_json::json!({"secret":"never-copy"}),
            ..Default::default()
        })
        .await
}
async fn invite(store: &KeyStore, id: String) -> Result<Invitation, TestError> {
    let authorization = admin()?;
    store
        .invite_device(
            &authorization,
            InviteRequest {
                template_id: id,
                invitation_ttl_seconds: 60,
                device_ttl_seconds: 120,
            },
        )
        .await
        .map_err(|_| "invite".into())
}
#[tokio::test]
#[expect(
    clippy::panic_in_result_fn,
    reason = "fallible fixture setup with intentional behavioral assertions"
)]
async fn independent_connections_single_winner_cache_and_restart() -> Result<(), TestError> {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("keys.db");
    let first = KeyStore::new(&path).await?;
    let source = template(&first).await?;
    let invitation = invite(&first, source.key_id.clone()).await?;
    let second = KeyStore::new(&path).await?;
    let (left, right) = tokio::join!(
        first.redeem_device(&invitation.token),
        second.redeem_device(&invitation.token)
    );
    assert_eq!(usize::from(left.is_ok()) + usize::from(right.is_ok()), 1);
    let (winner, credential) = match (left, right) {
        (Ok(child), Err(_)) => (&first, child),
        (Err(_), Ok(child)) => (&second, child),
        _ => return Err("not single winner".into()),
    };
    let child = winner.get_by_key(&credential.key).ok_or("not cached")?;
    assert!(winner.get_by_human_id(&credential.key_id).is_some());
    assert_ne!(credential.key, source.key);
    assert_eq!(child.owner, Some("owner".into()));
    assert_eq!(child.metadata.get("secret"), None);
    assert!(child
        .expires_at
        .is_some_and(|time| time <= Utc::now() + chrono::Duration::seconds(120)));
    let reopened = KeyStore::new(&path).await?;
    assert!(reopened.get_by_key(&credential.key).is_some());
    assert!(reopened.redeem_device(&invitation.token).await.is_err());
    winner.disable_key(&source.key_hash).await?;
    assert!(
        winner.get_by_key(&credential.key).is_some(),
        "template is not a parent grant"
    );
    winner.disable_key(&child.key_hash).await?;
    assert!(winner.get_by_key(&credential.key).is_none());
    first.shutdown();
    second.shutdown();
    reopened.shutdown();
    Ok(())
}
#[tokio::test]
#[expect(
    clippy::panic_in_result_fn,
    reason = "fallible fixture setup with intentional rollback and authority assertions"
)]
async fn rollback_no_ghost_and_live_authority() -> Result<(), TestError> {
    let store = KeyStore::in_memory().await?;
    let source = template(&store).await?;
    let invitation = invite(&store, source.key_id.clone()).await?;
    sqlx::query("CREATE TRIGGER deny_child BEFORE INSERT ON virtual_keys BEGIN SELECT RAISE(ABORT,'synthetic secret error'); END").execute(&store.pool).await?;
    assert!(matches!(
        store.redeem_device(&invitation.token).await,
        Err(E::Unavailable)
    ));
    assert_eq!(store.active_key_count(), 1);
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM virtual_keys")
        .fetch_one(&store.pool)
        .await?;
    assert_eq!(count, 1);
    sqlx::query("DROP TRIGGER deny_child")
        .execute(&store.pool)
        .await?;
    assert!(
        store.redeem_device(&invitation.token).await.is_ok(),
        "failed INSERT must roll back consumption"
    );
    for field in [
        "owner='changed'",
        "oauth_user='changed'",
        "oauth_account='changed'",
        "permissions='{\"allowed_models\":[]}'",
        "valid_after='2099-01-01T00:00:00Z'",
        "expires_at='2000-01-01T00:00:00Z'",
        "disabled=1",
    ] {
        let source = template(&store).await?;
        let invitation = invite(&store, source.key_id).await?;
        // Fixed, synthetic SQL fields only.
        sqlx::query(&format!("UPDATE virtual_keys SET {field} WHERE key_hash=?"))
            .bind(&source.key_hash)
            .execute(&store.pool)
            .await?;
        assert!(
            store.redeem_device(&invitation.token).await.is_err(),
            "{field}"
        );
    }
    Ok(())
}
#[tokio::test]
#[expect(
    clippy::panic_in_result_fn,
    reason = "fallible fixture setup with intentional expiry, revocation and cap assertions"
)]
async fn expiry_revoke_caps_and_hash_only() -> Result<(), TestError> {
    let store = KeyStore::in_memory().await?;
    let source = template(&store).await?;
    let invitation = invite(&store, source.key_id.clone()).await?;
    let stored: String = sqlx::query_scalar("SELECT token_hash FROM device_enrollment_v1")
        .fetch_one(&store.pool)
        .await?;
    assert_eq!(stored, hash_key(&invitation.token));
    assert_ne!(stored, invitation.token);
    sqlx::query("UPDATE device_enrollment_v1 SET expires_at=0")
        .execute(&store.pool)
        .await?;
    assert!(store.redeem_device(&invitation.token).await.is_err());
    let invitation = invite(&store, source.key_id).await?;
    let authorization = admin()?;
    store
        .revoke_device_invitation(&authorization, &invitation.token)
        .await
        .map_err(|_| "revoke")?;
    assert!(store.redeem_device(&invitation.token).await.is_err());
    for permissions in [
        KeyPermissions {
            rpm_limit: Some(1),
            ..Default::default()
        },
        KeyPermissions {
            tpm_limit: Some(1),
            ..Default::default()
        },
        KeyPermissions {
            rpd_limit: Some(1),
            ..Default::default()
        },
        KeyPermissions {
            max_budget_usd: Some(1.0),
            ..Default::default()
        },
        KeyPermissions {
            budget_window: Some(crate::keys::BudgetWindow::Daily),
            ..Default::default()
        },
    ] {
        let source = store
            .create_key(CreateKeyRequest {
                permissions,
                ..Default::default()
            })
            .await?;
        assert!(invite(&store, source.key_id).await.is_err());
    }
    assert!(Administrator::authenticate(None, "synthetic-root").is_err());
    assert!(Administrator::authenticate(Some("root"), "listener").is_err());
    Ok(())
}

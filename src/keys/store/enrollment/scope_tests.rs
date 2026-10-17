use super::{Administrator, InviteRequest, KeyStore, Utc};
use crate::keys::{CreateKeyRequest, KeyPermissions};

#[tokio::test]
#[expect(
    clippy::panic_in_result_fn,
    reason = "fallible fixture setup with intentional exact scope and expiry assertions"
)]
async fn exact_scope_oauth_binding_and_template_expiry(
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let store = KeyStore::in_memory().await?;
    let expiry = Utc::now() + chrono::Duration::seconds(90);
    let permissions = KeyPermissions {
        allowed_models: Some(
            ["model-a", "model-b", "alias-c"]
                .map(String::from)
                .into_iter()
                .collect(),
        ),
        blocked_models: Some(
            ["blocked-a", "blocked-b"]
                .map(String::from)
                .into_iter()
                .collect(),
        ),
        allowed_providers: Some(
            ["provider-a", "provider-b"]
                .map(String::from)
                .into_iter()
                .collect(),
        ),
        ..Default::default()
    };
    let source = store
        .create_key(CreateKeyRequest {
            permissions: permissions.clone(),
            expires_at: Some(expiry),
            oauth_user: Some("synthetic-account-owner".into()),
            oauth_account: Some("synthetic-account".into()),
            ..Default::default()
        })
        .await?;
    let admin = Administrator::authenticate(Some("synthetic-root"), "synthetic-root")
        .map_err(|_| "admin")?;
    // Repeated DB decoding must not spuriously invalidate randomized HashSets.
    for _ in 0..8 {
        let invitation = store
            .invite_device(
                &admin,
                InviteRequest {
                    template_id: source.key_id.clone(),
                    invitation_ttl_seconds: 600,
                    device_ttl_seconds: 300,
                },
            )
            .await
            .map_err(|_| "invite")?;
        let credential = store
            .redeem_device(&invitation.token)
            .await
            .map_err(|_| "redeem")?;
        let child = store.get_by_key(&credential.key).ok_or("cache")?;
        assert_eq!(child.permissions.allowed_models, permissions.allowed_models);
        assert_eq!(child.permissions.blocked_models, permissions.blocked_models);
        assert_eq!(
            child.permissions.allowed_providers,
            permissions.allowed_providers
        );
        assert_eq!(child.expires_at, Some(expiry));
        assert_eq!(child.oauth_user, source.oauth_user);
        assert_eq!(child.oauth_account, source.oauth_account);
    }
    for (invitation_ttl_seconds, device_ttl_seconds) in [(0, 1), (601, 1), (1, 0), (1, 31_536_001)]
    {
        assert!(store
            .invite_device(
                &admin,
                InviteRequest {
                    template_id: source.key_id.clone(),
                    invitation_ttl_seconds,
                    device_ttl_seconds
                }
            )
            .await
            .is_err());
    }
    Ok(())
}

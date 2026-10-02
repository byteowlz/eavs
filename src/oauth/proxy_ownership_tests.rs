//! Real resolver proof with a loopback-only refresh transport tripwire.
use super::*;
use crate::oauth::{CredentialProvenance, OAuthBackend, OAuthStore};
use std::sync::atomic::{AtomicUsize, Ordering};
use tokio::io::AsyncWriteExt;

#[tokio::test]
#[expect(
    clippy::panic_in_result_fn,
    reason = "Assertions verify security boundaries while fixture failures propagate"
)]
async fn borrowed_credentials_never_contact_refresh_transport(
) -> Result<(), Box<dyn std::error::Error>> {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let requests = Arc::new(AtomicUsize::new(0));
    let counted = requests.clone();
    let tripwire = tokio::spawn(async move {
        while let Ok((mut socket, _)) = listener.accept().await {
            counted.fetch_add(1, Ordering::SeqCst);
            let _ = socket
                .write_all(
                    b"HTTP/1.1 502 Bad Gateway\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                )
                .await;
        }
    });
    // Explicit proxy intercepts every HTTP/HTTPS refresh attempt. Even if the
    // ownership guard regresses, synthetic credentials cannot reach a provider.
    let client = reqwest::Client::builder()
        .proxy(reqwest::Proxy::all(format!("http://{address}"))?)
        .timeout(std::time::Duration::from_secs(2))
        .build()?;
    let directory = tempfile::tempdir()?;
    let store =
        OAuthStore::new(&directory.path().join("oauth.sqlite"), OAuthBackend::Sqlite).await?;
    let mut config = crate::config::AppConfig::with_defaults();
    config.providers.clear();
    for provider in [
        OAuthProviderKind::Anthropic,
        OAuthProviderKind::OpenAICodex,
        OAuthProviderKind::GithubCopilot,
    ] {
        config.providers.insert(
            provider.as_str().to_string(),
            crate::config::ProviderConfig {
                type_: provider.as_str().to_string(),
                ..Default::default()
            },
        );
    }
    let state = AppState::new(config);
    state
        .oauth_store
        .set(Arc::new(store))
        .map_err(|_| "store already initialized")?;
    let store = state.get_oauth_store().ok_or("missing test store")?;
    for provider in [
        OAuthProviderKind::Anthropic,
        OAuthProviderKind::OpenAICodex,
        OAuthProviderKind::GithubCopilot,
    ] {
        for expires_at in [0, chrono::Utc::now().timestamp() + 3600] {
            let credential = OAuthCredentials {
                user_id: "fixture".to_string(),
                provider,
                account_label: "default".to_string(),
                access_token: "synthetic-access".to_string(),
                refresh_token: "synthetic-refresh".to_string(),
                expires_at,
                extra_data: None,
                provenance: CredentialProvenance::Borrowed {
                    owner: "owner-cli".to_string(),
                },
            };
            store.upsert_credentials(&credential).await?;
            let before = serde_json::to_vec(&credential)?;
            let result = resolve_oauth_access_token_using_client(
                &state,
                provider.as_str(),
                "fixture",
                None,
                &client,
            )
            .await;
            if expires_at == 0 {
                assert!(
                    matches!(result, Err(OAuthResolveError::ExpiredBorrowed { owner, provider: actual }) if owner == "owner-cli" && actual == provider)
                );
            } else {
                assert_eq!(result?, "synthetic-access");
            }
            let after = store
                .get_credentials("fixture", provider)
                .await?
                .ok_or("missing credential after resolution")?;
            assert_eq!(serde_json::to_vec(&after)?, before);
            assert_eq!(requests.load(Ordering::SeqCst), 0);
        }
    }
    tripwire.abort();
    Ok(())
}

use super::*;

#[tokio::test]
#[expect(
    clippy::panic_in_result_fn,
    reason = "Assertions verify persistent CAS and rollback while fixture errors propagate"
)]
async fn credential_rotation_cas_redaction_and_restart() -> Result<(), Box<dyn std::error::Error>> {
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("providers.sqlite");
    let store = Arc::new(ProviderStore::new(&path).await?);
    let entry = store.upsert_provider(ProviderRequest {
        name: "fixture".into(),
        config: serde_json::json!({"type":"openai", "api_key":"synthetic-old", "headers":{"x-secret":"synthetic-header"}, "base_url":"https://user:synthetic-url@example.test"}),
        enabled: true,
    }).await?;
    let encoded = serde_json::to_string(&entry)?;
    for secret in ["synthetic-old", "synthetic-header", "synthetic-url"] {
        assert!(!encoded.contains(secret));
        assert!(!format!("{entry:?}").contains(secret));
    }
    let initial = entry.updated_at.to_rfc3339();
    let (left, right) = tokio::join!(
        store.rotate_api_key("fixture", &initial, "synthetic-left"),
        store.rotate_api_key("fixture", &initial, "synthetic-right"),
    );
    assert_eq!(usize::from(left.is_ok()) + usize::from(right.is_ok()), 1);
    assert!(
        matches!(&left, Err(ProviderStoreError::Conflict))
            || matches!(&right, Err(ProviderStoreError::Conflict))
    );
    let winner = left.or(right)?;
    assert!(!serde_json::to_string(&winner)?.contains("synthetic"));
    assert!(matches!(
        store.rollback_api_key("fixture", &initial).await,
        Err(ProviderStoreError::Conflict)
    ));
    drop(store);
    let reopened = ProviderStore::new(&path).await?;
    let rotated = reopened.get_provider("fixture").ok_or("missing provider")?;
    assert_ne!(rotated.config["api_key"], "synthetic-old");
    assert_eq!(rotated.updated_at.to_rfc3339(), winner.revision);
    let restored = reopened
        .rollback_api_key("fixture", &winner.revision)
        .await?;
    assert_ne!(restored.revision, winner.revision);
    assert_eq!(
        reopened
            .get_provider("fixture")
            .ok_or("missing restored provider")?
            .config,
        entry.config
    );
    assert!(matches!(
        reopened
            .rollback_api_key("fixture", &restored.revision)
            .await,
        Err(ProviderStoreError::Conflict)
    ));
    drop(reopened);
    assert_eq!(
        ProviderStore::new(&path)
            .await?
            .get_provider("fixture")
            .ok_or("missing persisted rollback")?
            .config["api_key"],
        "synthetic-old"
    );
    Ok(())
}

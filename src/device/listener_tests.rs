use super::*;
use anyhow::ensure;
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};

#[tokio::test]
async fn listener_capability_is_separate_private_and_explicit() -> Result<()> {
    let calls = Arc::new(AtomicUsize::new(0));
    let counter = calls.clone();
    let app=axum::Router::new().route("/v1/eavs/catalog",axum::routing::get(move |headers:http::HeaderMap| {
        let counter=counter.clone();
        async move {
            counter.fetch_add(1,Ordering::SeqCst);
            if headers.get("x-eavs-token").and_then(|h|h.to_str().ok())!=Some("synthetic-listener-secret") || headers.get("authorization").and_then(|h|h.to_str().ok())!=Some("Bearer eavs-synthetic000000000000000000000000000000") {
                return (http::StatusCode::UNAUTHORIZED,axum::Json(serde_json::json!({"error":"private gateway failure"})));
            }
            (http::StatusCode::OK,axum::Json(serde_json::json!({"schema_version":1,"scope":"key-filtered-chat-candidates","providers":[{"id":"fixture","models":[]}]})))
        }
    }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let url = format!("http://{}", listener.local_addr()?);
    let server = tokio::spawn(async move { axum::serve(listener, app).await });
    let key_ref = format!("EAVS_TEST_KEY_{}", uuid::Uuid::new_v4().simple());
    let listener_ref = format!("EAVS_TEST_LISTENER_{}", uuid::Uuid::new_v4().simple());
    std::env::set_var(&key_ref, "eavs-synthetic000000000000000000000000000000");
    let mut c = Connection {
        name: "fixture".into(),
        url,
        key_env: key_ref.clone(),
        listener_key_env: Some(listener_ref.clone()),
        allow_loopback_http: true,
    };
    let missing = observe(&c).await?;
    ensure!(missing.state == "unconfigured" && calls.load(Ordering::SeqCst) == 0);
    std::env::set_var(&listener_ref, "bad\nheader");
    let invalid = observe(&c).await?;
    ensure!(invalid.state == "unconfigured" && calls.load(Ordering::SeqCst) == 0);
    std::env::set_var(&listener_ref, "synthetic-listener-secret");
    let ready = observe(&c).await?;
    ensure!(ready.state == "ready" && calls.load(Ordering::SeqCst) == 1);
    let public = serde_json::to_string(&ready)?;
    ensure!(
        !public.contains("synthetic-listener-secret")
            && !public.contains(&listener_ref)
            && !public.contains(&key_ref)
    );
    c.listener_key_env = None;
    let denied = observe(&c).await?;
    ensure!(denied.state == "unauthorized" && calls.load(Ordering::SeqCst) == 2);
    let public = serde_json::to_string(&denied)?;
    ensure!(!public.contains("private gateway failure"));
    std::env::remove_var(&key_ref);
    std::env::remove_var(&listener_ref);
    server.abort();
    Ok(())
}

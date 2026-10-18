use super::*;
#[path = "extra_tests.rs"]
mod extra_tests;
use crate::{
    config::{AppConfig, ProviderConfig},
    keys::{CreateKeyRequest, KeyPermissions},
    upstream::{Upstream, UpstreamRequest, UpstreamResponse},
};
use anyhow::{anyhow, Result};
use bytes::Bytes;
use futures::{future::BoxFuture, StreamExt};
use serde_json::{json, Value};
use std::sync::{Arc, Mutex};

pub(super) struct Fake {
    requests: Mutex<Vec<UpstreamRequest>>,
}
impl Upstream for Fake {
    fn send(
        &self,
        req: UpstreamRequest,
    ) -> BoxFuture<'_, Result<UpstreamResponse, std::io::Error>> {
        Box::pin(async move {
            self.requests
                .lock()
                .map_err(|_| std::io::Error::other("fixture poisoned"))?
                .push(req);
            let value = json!({"id":"response-α","model":"gpt-4o","choices":[{"message":{"role":"assistant","content":"Hello α 🦉","tool_calls":[{"id":"call-1","type":"function","function":{"name":"read","arguments":"{\"path\":\"α.txt\"}"}}]},"finish_reason":"tool_calls"}],"usage":{"prompt_tokens":3,"completion_tokens":2,"total_tokens":5}});
            let bytes = Bytes::from(value.to_string());
            Ok(UpstreamResponse {
                status: StatusCode::OK,
                headers: http::HeaderMap::new(),
                body: futures::stream::once(async move { Ok(bytes) }).boxed(),
            })
        })
    }
}
pub(super) async fn fixture() -> Result<(AppState, Arc<Fake>, tempfile::TempDir, String)> {
    let dir = tempfile::tempdir()?;
    let mut config = AppConfig::with_defaults();
    config.providers.clear();
    config.providers.insert(
        "alpha".into(),
        ProviderConfig {
            type_: "openai-compatible".into(),
            api_key: "synthetic-upstream".into(),
            base_url: "https://fixture.invalid/v1".into(),
            ..Default::default()
        },
    );
    config.keys.enabled = true;
    config.keys.database_path = dir
        .path()
        .join("keys.sqlite")
        .to_string_lossy()
        .into_owned();
    config.keys.update_pricing_on_startup = false;
    let fake = Arc::new(Fake {
        requests: Mutex::new(Vec::new()),
    });
    let state = AppState::new_with_upstream(config, fake.clone());
    state.init_key_store().await.map_err(|e| anyhow!(e))?;
    let token = state
        .get_key_store()
        .ok_or_else(|| anyhow!("missing fixture store"))?
        .create_key(CreateKeyRequest {
            permissions: KeyPermissions {
                allowed_providers: Some(["alpha".into()].into()),
                allowed_models: Some(["gpt-4o".into()].into()),
                ..Default::default()
            },
            ..Default::default()
        })
        .await?
        .key;
    Ok((state, fake, dir, token))
}
pub(super) fn payload() -> Value {
    json!({"model":"gpt-4o","context":{"messages":[
        {"role":"system","content":"Read carefully","toolsAdded":[{"name":"read","description":"Read a file","parameters":{"type":"object","properties":{"path":{"type":"string"}}}}]},
        {"role":"user","content":[{"type":"text","text":"Hello α 🦉"}]}
    ]},"options":{"maxTokens":32,"temperature":0.2}})
}
pub(super) async fn call(
    state: AppState,
    token: &str,
    value: Value,
    uri: &str,
) -> Result<Response> {
    let request = Request::builder()
        .method("POST")
        .uri(uri)
        .header("authorization", format!("Bearer {token}"))
        .header("content-type", "application/json")
        .body(Body::from(value.to_string()))?;
    Ok(crate::proxy::provider_proxy_handler(
        axum::extract::State(state),
        axum::extract::Path(("alpha".into(), "pi/messages".into())),
        request,
    )
    .await
    .unwrap_or_else(|response| response))
}
pub(super) async fn body(response: Response) -> Result<String> {
    Ok(String::from_utf8(
        to_bytes(response.into_body(), 16 * 1024 * 1024)
            .await?
            .to_vec(),
    )?)
}

#[tokio::test]
#[expect(
    clippy::panic_in_result_fn,
    reason = "Assertions intentionally verify fallible offline fixtures"
)]
async fn pi_wire_text_tools_and_existing_native_interface() -> Result<()> {
    let (state, fake, _dir, token) = fixture().await?;
    let response = call(state.clone(), &token, payload(), "/alpha/v1/pi/messages").await?;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()["content-type"], "text/event-stream");
    assert_eq!(
        response.headers()["x-eavs-pi-delivery"],
        "buffered-upstream"
    );
    let text = body(response).await?;
    let events: Vec<Value> = text
        .split("\n\n")
        .filter_map(|frame| frame.strip_prefix("data: "))
        .map(serde_json::from_str)
        .collect::<Result<_, _>>()?;
    assert_eq!(
        events.first().map(|v| v["type"].as_str()),
        Some(Some("start"))
    );
    assert_eq!(
        events.last().map(|v| v["type"].as_str()),
        Some(Some("done"))
    );
    assert!(text.contains("Hello α 🦉"));
    assert!(text.contains("toolcall_end"));
    assert_eq!(
        events.last().map(|v| &v["usage"]["totalTokens"]),
        Some(&json!(5))
    );
    if let Some(path) = std::env::var_os("EAVS_PI_WIRE_FIXTURE") {
        std::fs::write(path, &text)?;
    }
    let native = call(
        state,
        &token,
        json!({"model":"gpt-4o","messages":[{"role":"user","content":"native"}],"stream":false}),
        "/alpha/v1/chat/completions",
    )
    .await?;
    assert_eq!(native.status(), StatusCode::OK);
    assert!(!native.headers().contains_key("x-eavs-pi-delivery"));
    let requests = fake
        .requests
        .lock()
        .map_err(|_| anyhow!("fixture poisoned"))?;
    assert_eq!(requests.len(), 2);
    let first: Value = serde_json::from_slice(
        &requests
            .first()
            .ok_or_else(|| anyhow!("missing request"))?
            .body,
    )?;
    assert_eq!(first["model"], "gpt-4o");
    assert_eq!(first["stream"], true);
    assert_eq!(first["tools"][0]["function"]["name"], "read");
    assert!(first.get("context").is_none());
    drop(requests);
    Ok(())
}

#[tokio::test]
#[expect(
    clippy::panic_in_result_fn,
    reason = "Assertions intentionally verify fallible offline fixtures"
)]
async fn unsupported_features_and_invalid_scope_never_generate() -> Result<()> {
    let (state, fake, _dir, token) = fixture().await?;
    let mut unsupported = payload();
    unsupported["options"]["reasoning"] = "invalid".into();
    assert_eq!(
        call(state.clone(), &token, unsupported, "/alpha/v1/pi/messages")
            .await?
            .status(),
        StatusCode::BAD_REQUEST
    );
    let mut wrong_model = payload();
    wrong_model["model"] = "gpt-4".into();
    assert_eq!(
        call(state.clone(), &token, wrong_model, "/alpha/v1/pi/messages")
            .await?
            .status(),
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        call(state.clone(), "invalid", payload(), "/alpha/v1/pi/messages")
            .await?
            .status(),
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        fake.requests
            .lock()
            .map_err(|_| anyhow!("fixture poisoned"))?
            .len(),
        0
    );
    Ok(())
}

#[tokio::test]
#[expect(
    clippy::panic_in_result_fn,
    reason = "Assertions intentionally verify fallible offline fixtures"
)]
async fn policy_rewrites_still_enforce_effective_model_scope() -> Result<()> {
    let (original, fake, _dir, token) = fixture().await?;
    let mut config = (*original.config).clone();
    config.policy = serde_json::from_value(
        json!({"enabled":true,"rules":[{"action":"rewrite_model","model":"gpt-4o","to":"gpt-4"}]}),
    )?;
    let state = AppState::new_with_upstream(config, fake.clone());
    state.init_key_store().await.map_err(|e| anyhow!(e))?;
    assert_eq!(
        call(state, &token, payload(), "/alpha/v1/pi/messages")
            .await?
            .status(),
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        fake.requests
            .lock()
            .map_err(|_| anyhow!("fixture poisoned"))?
            .len(),
        0
    );
    Ok(())
}

#[test]
#[expect(
    clippy::panic_in_result_fn,
    reason = "Assertions intentionally verify fallible offline fixtures"
)]
fn converts_history_images_and_rejects_lossy_features() -> Result<()> {
    let mut value = payload();
    value["context"]["messages"].as_array_mut().ok_or_else(||anyhow!("missing fixture array"))?.extend([
        json!({"role":"assistant","content":[{"type":"toolCall","id":"call-1","name":"read","arguments":{"path":"α.txt"}}]}),
        json!({"role":"toolResult","toolCallId":"call-1","toolName":"read","content":[{"type":"text","text":"file content"}],"isError":false}),
        json!({"role":"user","content":[{"type":"image","mimeType":"image/png","data":"SGVsbG8="}]})
    ]);
    let converted = request::convert(&value).map_err(|()| anyhow!("conversion failed"))?;
    assert_eq!(converted["messages"][3]["role"], "tool");
    assert_eq!(
        converted["messages"][4]["content"][0]["image_url"]["url"],
        "data:image/png;base64,SGVsbG8="
    );
    value["context"]["messages"][2]["content"][0] =
        json!({"type":"thinking","thinking":"private","thinkingSignature":"sig"});
    let signed = request::convert(&value).map_err(|()| anyhow!("surrogate conversion failed"))?;
    assert!(canonical::context(
        &signed,
        "alpha",
        crate::provider::ProviderType::OpenAICompatible
    )
    .is_err());
    assert!(request::convert(
        &json!({"model":"gpt-4o","context":{"messages":[]},"options":{"env":{"SECRET":"private"}}})
    )
    .is_err());
    Ok(())
}

use super::tests::{body, call, fixture, payload};
use super::*;
use crate::{
    provider::ProviderType,
    upstream::{Upstream, UpstreamRequest, UpstreamResponse},
};
use anyhow::{anyhow, Result};
use bytes::Bytes;
use futures::{future::BoxFuture, StreamExt};
use serde_json::{json, Value};
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc, Mutex,
};
use std::time::Duration;

struct NativeFake {
    frames: Vec<u8>,
    requests: Mutex<Vec<UpstreamRequest>>,
    release: Option<Arc<tokio::sync::Notify>>,
    drops: Arc<AtomicUsize>,
}
struct Watch {
    stream: futures::stream::BoxStream<'static, Result<Bytes, std::io::Error>>,
    drops: Arc<AtomicUsize>,
}
impl futures::Stream for Watch {
    type Item = Result<Bytes, std::io::Error>;
    fn poll_next(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Self::Item>> {
        self.stream.as_mut().poll_next(cx)
    }
}
impl Drop for Watch {
    fn drop(&mut self) {
        self.drops.fetch_add(1, Ordering::SeqCst);
    }
}
impl Upstream for NativeFake {
    fn send(
        &self,
        request: UpstreamRequest,
    ) -> BoxFuture<'_, Result<UpstreamResponse, std::io::Error>> {
        Box::pin(async move {
            self.requests
                .lock()
                .map_err(|_| std::io::Error::other("fixture poisoned"))?
                .push(request);
            let mut headers = http::HeaderMap::new();
            headers.insert(
                "content-type",
                http::HeaderValue::from_static("text/event-stream"),
            );
            let chunks: Vec<_> = self
                .frames
                .iter()
                .map(|byte| Ok(Bytes::from(vec![*byte])))
                .collect();
            let stream = futures::stream::iter(chunks);
            let stream = if let Some(release) = &self.release {
                let release = release.clone();
                stream
                    .chain(futures::stream::once(async move {
                        release.notified().await;
                        Ok(Bytes::from_static(b"data: [DONE]\n\n"))
                    }))
                    .boxed()
            } else {
                stream.boxed()
            };
            Ok(UpstreamResponse {
                status: StatusCode::OK,
                headers,
                body: Box::pin(Watch {
                    stream,
                    drops: self.drops.clone(),
                }),
            })
        })
    }
}
fn sse(kind: &str, value: impl std::fmt::Display) -> String {
    let value = value.to_string();
    if kind.is_empty() {
        format!("data: {value}\r\n\r\n")
    } else {
        format!("event: {kind}\r\ndata: {value}\r\n\r\n")
    }
}
fn anthropic() -> String {
    [
        sse("message_start",json!({"message":{"id":"response-α","usage":{"input_tokens":3,"output_tokens":0,"cache_read_input_tokens":2,"cache_creation_input_tokens":1}}})),
        sse("content_block_start",json!({"index":0,"content_block":{"type":"thinking","thinking":""}})),
        sse("content_block_delta",json!({"index":0,"delta":{"type":"thinking_delta","thinking":"Think α"}})),
        sse("content_block_delta",json!({"index":0,"delta":{"type":"signature_delta","signature":"opaque-"}})),
        sse("content_block_delta",json!({"index":0,"delta":{"type":"signature_delta","signature":"sig"}})),
        sse("content_block_stop",json!({"index":0})),
        sse("content_block_start",json!({"index":1,"content_block":{"type":"text","text":""}})),
        sse("content_block_delta",json!({"index":1,"delta":{"type":"text_delta","text":"Hello α 🦉"}})),
        sse("content_block_stop",json!({"index":1})),
        sse("content_block_start",json!({"index":2,"content_block":{"type":"tool_use","id":"call-1","name":"read","input":{}}})),
        sse("content_block_delta",json!({"index":2,"delta":{"type":"input_json_delta","partial_json":"{\"path\":\"α.txt\"}"}})),
        sse("content_block_stop",json!({"index":2})),
        sse("message_delta",json!({"delta":{"stop_reason":"tool_use"},"usage":{"output_tokens":2}})),
        sse("message_stop",json!({})),
    ].concat()
}
fn chat(terminal: bool) -> String {
    let mut text = [
        sse("",json!({"id":"response-α","choices":[{"index":0,"delta":{"role":"assistant","reasoning_content":"Think α"},"finish_reason":null}]})),
        sse("",json!({"id":"response-α","choices":[{"index":0,"delta":{"content":"Hello α 🦉","tool_calls":[{"index":0,"id":"call-1","function":{"name":"read","arguments":"{\"path\":\"α.txt\"}"}}]},"finish_reason":null}]})),
        sse("",json!({"choices":[{"index":0,"delta":{},"finish_reason":"tool_calls"}]})),
        sse("",json!({"choices":[],"usage":{"prompt_tokens":5,"completion_tokens":2,"prompt_tokens_details":{"cached_tokens":2}}})),
    ].concat();
    if terminal {
        text.push_str("data: [DONE]\r\n\r\n");
    }
    text
}
fn google() -> String {
    sse(
        "",
        json!({"responseId":"response-α","candidates":[{"content":{"parts":[{"thought":true,"text":"Think α","thoughtSignature":"google-think"},{"text":"Hello α 🦉","thoughtSignature":"google-text"},{"functionCall":{"name":"read","args":{"path":"α.txt"}},"thoughtSignature":"google-tool"}]},"finishReason":"STOP"}],"usageMetadata":{"promptTokenCount":5,"candidatesTokenCount":1,"thoughtsTokenCount":1,"cachedContentTokenCount":2}}),
    )
}
fn responses() -> String {
    let think =
        json!({"type":"reasoning","id":"rs-1","summary":[],"encrypted_content":"ciphertext"});
    [
        sse("",json!({"type":"response.created","response":{"id":"response-α"}})),
        sse("",json!({"type":"response.output_item.added","item":think})),
        sse("",json!({"type":"response.reasoning_summary_text.delta","item_id":"rs-1","delta":"Think α"})),
        sse("",json!({"type":"response.output_text.delta","item_id":"msg-1","content_index":0,"delta":"Hello α 🦉"})),
        sse("",json!({"type":"response.output_item.done","item":{"type":"message","id":"msg-1","role":"assistant","content":[{"type":"output_text","text":"Hello α 🦉"}]}})),
        sse("",json!({"type":"response.output_item.added","item":{"type":"function_call","id":"fc-1","call_id":"call-1","name":"read","arguments":""}})),
        sse("",json!({"type":"response.function_call_arguments.delta","item_id":"fc-1","delta":"{\"path\":\"α.txt\"}"})),
        sse("",json!({"type":"response.completed","response":{"id":"response-α","status":"completed","usage":{"input_tokens":5,"output_tokens":2,"input_tokens_details":{"cached_tokens":2}}}})),
    ].concat()
}
async fn native_fixture(
    kind: &str,
    frames: String,
    release: Option<Arc<tokio::sync::Notify>>,
) -> Result<(AppState, Arc<NativeFake>, tempfile::TempDir, String)> {
    let (original, _, dir, token) = fixture().await?;
    let mut config = (*original.config).clone();
    config
        .providers
        .get_mut("alpha")
        .ok_or_else(|| anyhow!("missing provider"))?
        .type_ = kind.into();
    let fake = Arc::new(NativeFake {
        frames: frames.into_bytes(),
        requests: Mutex::new(Vec::new()),
        release,
        drops: Arc::new(AtomicUsize::new(0)),
    });
    let state = AppState::new_with_upstream(config, fake.clone());
    state.init_key_store().await.map_err(|e| anyhow!(e))?;
    let mut pricing = state
        .pricing
        .get("gpt-4o")
        .await
        .ok_or_else(|| anyhow!("missing fixture pricing"))?;
    pricing.cached_input_cost_per_token = Some(0.000_001);
    pricing.cache_write_cost_per_token = Some(0.000_002);
    state.pricing.load_pricing_entries(vec![pricing]).await;
    Ok((state, fake, dir, token))
}
fn events(text: &str) -> Result<Vec<Value>> {
    Ok(text
        .split("\n\n")
        .filter_map(|v| v.strip_prefix("data: "))
        .map(serde_json::from_str)
        .collect::<Result<_, _>>()?)
}

#[tokio::test]
#[expect(
    clippy::panic_in_result_fn,
    reason = "Behavioral assertions in fallible native protocol fixtures"
)]
async fn all_native_formats_stream_unicode_thinking_tools_and_cache() -> Result<()> {
    for (kind, frames) in [
        ("openai-compatible", chat(true)),
        ("anthropic", anthropic()),
        ("google", google()),
        ("openai-responses", responses()),
        ("openai-codex", responses()),
    ] {
        let (state, fake, _dir, token) = native_fixture(kind, frames, None).await?;
        let response = call(state.clone(), &token, payload(), "/alpha/v1/pi/messages").await?;
        assert_eq!(response.status(), StatusCode::OK, "{kind}");
        assert_eq!(response.headers()["x-eavs-pi-delivery"], "streaming");
        let text = body(response).await?;
        let parsed = events(&text)?;
        assert_eq!(
            parsed.last().map(|v| &v["type"]),
            Some(&json!("done")),
            "{kind}: {text}"
        );
        assert!(text.contains("Hello α 🦉"));
        assert!(text.contains("thinking_end"));
        assert!(text.contains("toolcall_end"));
        let usage = &parsed.last().ok_or_else(|| anyhow!("missing terminal"))?["usage"];
        assert_eq!(usage["input"], 3);
        assert_eq!(usage["cacheRead"], 2);
        assert_eq!(usage["cacheWrite"], i32::from(kind == "anthropic"));
        if let Some(dir) = std::env::var_os("EAVS_PI_NATIVE_FIXTURES") {
            std::fs::create_dir_all(&dir)?;
            std::fs::write(
                std::path::PathBuf::from(dir).join(format!("{kind}.sse")),
                &text,
            )?;
        }
        let requests = fake
            .requests
            .lock()
            .map_err(|_| anyhow!("fixture poisoned"))?;
        assert_eq!(requests.len(), 1);
        let request = requests.first().ok_or_else(|| anyhow!("missing request"))?;
        let sent: Value = serde_json::from_slice(&request.body)?;
        assert!(sent.get("_pi_blocks").is_none());
        drop(requests);
    }
    Ok(())
}

#[tokio::test]
#[expect(
    clippy::panic_in_result_fn,
    reason = "Behavioral assertions in fallible native protocol fixtures"
)]
async fn streaming_is_incremental_and_drop_releases_upstream() -> Result<()> {
    let release = Arc::new(tokio::sync::Notify::new());
    let (state, fake, _dir, token) =
        native_fixture("openai-compatible", chat(false), Some(release)).await?;
    let response = call(state, &token, payload(), "/alpha/v1/pi/messages").await?;
    let mut body = response.into_body().into_data_stream();
    let mut text = String::new();
    while !text.contains("Hello α 🦉") {
        let chunk = tokio::time::timeout(Duration::from_secs(2), body.next())
            .await?
            .ok_or_else(|| anyhow!("early EOF"))??;
        text.push_str(std::str::from_utf8(&chunk)?);
    }
    assert!(!text.contains("\"type\":\"done\""));
    drop(body);
    assert_eq!(fake.drops.load(Ordering::SeqCst), 1);
    Ok(())
}

#[tokio::test]
#[expect(
    clippy::panic_in_result_fn,
    reason = "Behavioral assertions in fallible native protocol fixtures"
)]
async fn truncated_streams_have_error_not_done_and_private_errors_are_bounded() -> Result<()> {
    for frames in [
        chat(false),
        "data: {\"error\":{\"message\":\"private-upstream-secret\"}}\n\n".into(),
        "data: {unfinished".into(),
    ] {
        let (state, _, _dir, token) = native_fixture("openai-compatible", frames, None).await?;
        let text = body(call(state, &token, payload(), "/alpha/v1/pi/messages").await?).await?;
        let parsed = events(&text)?;
        assert_eq!(parsed.last().map(|v| &v["type"]), Some(&json!("error")));
        assert!(!text.contains("private-upstream-secret"));
        assert!(!parsed.iter().any(|v| v["type"] == "done"));
    }
    Ok(())
}

#[test]
#[expect(
    clippy::panic_in_result_fn,
    reason = "Behavioral assertions in fallible native protocol fixtures"
)]
fn signed_histories_restore_native_fields_and_reject_route_or_model_change() -> Result<()> {
    for kind in [
        ProviderType::Anthropic,
        ProviderType::Google,
        ProviderType::OpenAIResponses,
    ] {
        let raw = if kind == ProviderType::OpenAIResponses {
            json!({"type":"reasoning","id":"rs-1","encrypted_content":"ciphertext","summary":[{"type":"summary_text","text":"Think"}]})
        } else {
            json!("opaque-sig")
        };
        let signature = signatures::seal(&raw, "alpha", "gpt-4o", canonical::api(kind));
        let mut value = payload();
        value["context"]["messages"].as_array_mut().ok_or_else(||anyhow!("missing messages"))?.push(json!({"role":"assistant","content":[{"type":"thinking","thinking":"Think","thinkingSignature":signature},{"type":"text","text":"Hello"}]}));
        let surrogate = request::convert(&value).map_err(|()| anyhow!("conversion failed"))?;
        let context = canonical::context(&surrogate, "alpha", kind)?;
        let transformer = crate::transform::ProviderTransformer::for_provider(kind);
        let mut body = transformer.transform_request(&context)?;
        canonical::finish(&mut body, &context, "alpha", kind)?;
        assert!(body
            .to_string()
            .contains(if kind == ProviderType::OpenAIResponses {
                "ciphertext"
            } else {
                "opaque-sig"
            }));
        assert!(canonical::context(&surrogate, "different", kind).is_err());
        let mut rewritten = surrogate;
        rewritten["model"] = "gpt-4".into();
        assert!(canonical::context(&rewritten, "alpha", kind).is_err());
    }
    Ok(())
}

#[tokio::test]
async fn native_policy_rewrites_and_caps_fail_closed() -> Result<()> {
    use crate::keys::{CreateKeyRequest, KeyPermissions};
    use anyhow::ensure;
    for (kind, frames) in [
        ("anthropic", anthropic()),
        ("google", google()),
        ("openai-responses", responses()),
        ("openai-codex", responses()),
    ] {
        let (state, fake, _dir, token) = native_fixture(kind, frames, None).await?;
        for permissions in [
            KeyPermissions {
                max_budget_usd: Some(1.0),
                ..Default::default()
            },
            KeyPermissions {
                tpm_limit: Some(100),
                ..Default::default()
            },
            KeyPermissions {
                allowed_providers: Some(["other".into()].into()),
                ..Default::default()
            },
        ] {
            let capped = state
                .get_key_store()
                .ok_or_else(|| anyhow!("store"))?
                .create_key(CreateKeyRequest {
                    permissions,
                    ..Default::default()
                })
                .await?
                .key;
            ensure!(
                !call(state.clone(), &capped, payload(), "/alpha/v1/pi/messages")
                    .await?
                    .status()
                    .is_success(),
                "cap or provider scope bypass"
            );
        }
        let mut config = (*state.config).clone();
        config.policy = serde_json::from_value(
            json!({"enabled":true,"rules":[{"action":"rewrite_model","model":"gpt-4o","to":"gpt-4"}]}),
        )?;
        let rewritten = AppState::new_with_upstream(config, fake.clone());
        rewritten.init_key_store().await.map_err(|e| anyhow!(e))?;
        ensure!(
            !call(rewritten, &token, payload(), "/alpha/v1/pi/messages")
                .await?
                .status()
                .is_success(),
            "policy model bypass"
        );
        ensure!(
            fake.requests
                .lock()
                .map_err(|_| anyhow!("poisoned"))?
                .is_empty(),
            "denied request generated"
        );
    }
    Ok(())
}

#[tokio::test]
async fn signed_replay_and_native_denials_cross_shared_admission() -> Result<()> {
    use anyhow::ensure;
    for (kind, api, block, needle, frames) in [
        (
            "google",
            "google-generative-ai",
            json!({"type":"text","text":"hello","textSignature":"PLACEHOLDER"}),
            "text-sig",
            google(),
        ),
        (
            "google",
            "google-generative-ai",
            json!({"type":"toolCall","id":"call-1","name":"read","arguments":{},"thoughtSignature":"PLACEHOLDER"}),
            "tool-sig",
            google(),
        ),
        (
            "anthropic",
            "anthropic-messages",
            json!({"type":"thinking","thinking":"","redacted":true,"thinkingSignature":"PLACEHOLDER"}),
            "redacted-cipher",
            anthropic(),
        ),
        (
            "openai-responses",
            "openai-responses",
            json!({"type":"text","text":"hello","textSignature":"PLACEHOLDER"}),
            "msg-signed",
            responses(),
        ),
    ] {
        let (state, fake, _dir, token) = native_fixture(kind, frames, None).await?;
        let raw = if kind == "openai-responses" {
            json!({"id":needle,"phase":"final_answer"})
        } else {
            json!(needle)
        };
        let signature = signatures::seal(&raw, "alpha", "gpt-4o", api);
        let mut block = block;
        for key in ["textSignature", "thoughtSignature", "thinkingSignature"] {
            if block.get(key).is_some() {
                block[key] = signature.clone().into();
            }
        }
        let mut value = payload();
        value["context"]["messages"]
            .as_array_mut()
            .ok_or_else(|| anyhow!("messages"))?
            .push(json!({"role":"assistant","content":[block]}));
        let denied = call(
            state.clone(),
            "invalid",
            value.clone(),
            "/alpha/v1/pi/messages",
        )
        .await?;
        ensure!(
            denied.status() == StatusCode::UNAUTHORIZED,
            "invalid key accepted"
        );
        let mut rewritten = value.clone();
        rewritten["model"] = "gpt-4".into();
        ensure!(
            !call(state.clone(), &token, rewritten, "/alpha/v1/pi/messages")
                .await?
                .status()
                .is_success(),
            "model rewrite accepted"
        );
        ensure!(
            fake.requests
                .lock()
                .map_err(|_| anyhow!("poisoned"))?
                .is_empty(),
            "denial generated"
        );
        let response = call(state, &token, value, "/alpha/v1/pi/messages").await?;
        if !response.status().is_success() {
            return Err(anyhow!(
                "signed {kind} request rejected: {}",
                body(response).await?
            ));
        }
        let output = events(&body(response).await?)?;
        ensure!(
            output.last().is_some_and(|v| v["type"] == "done"),
            "replay did not complete"
        );
        let requests = fake.requests.lock().map_err(|_| anyhow!("poisoned"))?;
        ensure!(requests.len() == 1, "unexpected generation count");
        let sent: Value = serde_json::from_slice(&requests[0].body)?;
        drop(requests);
        ensure!(
            sent.to_string().contains(needle),
            "signed field lost: {sent}"
        );
    }
    Ok(())
}

use super::*;

#[test]
#[expect(
    clippy::panic_in_result_fn,
    reason = "Actual Swift SDK fixture must preserve gateway shape and explicit generation limits"
)]
fn actual_iphone_embedded_sdk_request_is_representable() -> Result<()> {
    // Captured from the actual pinned PiDurableKit 1.2 runtime's synthetic
    // integration test, after its explicit phone transport output default.
    let value: Value = serde_json::from_str(include_str!("fixtures/iphone-sdk-request.json"))?;
    let converted = request::convert(&value).map_err(|()| anyhow!("Swift SDK request rejected"))?;
    assert_eq!(converted["model"], "fixture-model");
    assert_eq!(converted["max_tokens"], 512);
    assert_eq!(converted["_pi_cache"], "none");
    assert!(converted.get("prompt_cache_key").is_none());
    assert!(converted["messages"]
        .as_array()
        .is_some_and(|messages| messages.iter().any(
            |message| message["role"] == "user" && message["content"] == "Explicit eavs prompt"
        )));
    Ok(())
}

#[test]
#[expect(
    clippy::panic_in_result_fn,
    reason = "Assertions intentionally verify fallible offline fixtures"
)]
fn terminal_and_cache_shapes_never_fabricate_success() -> Result<()> {
    let pricing = crate::keys::pricing::PricingTable::with_embedded_data()
        .get("gpt-4o")
        .ok_or_else(|| anyhow!("missing fixture price"))?
        .clone();
    assert!(valid_pricing(&pricing));
    let mut unknown = pricing.clone();
    unknown.input_cost_per_token = 0.0;
    assert!(!valid_pricing(&unknown));
    let valid = json!({"id":"r","model":"gpt-4o","choices":[{"message":{"role":"assistant","content":"α","tool_calls":null},"finish_reason":"stop"}],"usage":{"prompt_tokens":3,"completion_tokens":2}});
    assert!(response::convert(&valid, &pricing).is_ok());
    for path in [
        "finish",
        "cache",
        "thinking",
        "missing-usage",
        "tool-reason",
    ] {
        let mut value = valid.clone();
        match path {
            "finish" => value["choices"][0]["finish_reason"] = Value::Null,
            "cache" => value["usage"]["prompt_tokens_details"] = json!({"cached_tokens":1}),
            "thinking" => {
                value["choices"][0]["message"]["reasoning_details"] =
                    json!([{"signature":"private"}]);
            }
            "missing-usage" => value["usage"] = Value::Null,
            "tool-reason" => value["choices"][0]["finish_reason"] = "tool_calls".into(),
            _ => return Err(anyhow!("unknown fixture")),
        }
        assert!(response::convert(&value, &pricing).is_err());
    }
    Ok(())
}

#[test]
#[expect(
    clippy::panic_in_result_fn,
    reason = "Assertions intentionally verify fallible offline fixtures"
)]
fn options_and_dynamic_tools_are_explicit() -> Result<()> {
    let mut value = payload();
    value["options"] =
        json!({"sessionId":"fixture-session","cacheRetention":"short","toolChoice":"none"});
    let converted = request::convert(&value).map_err(|()| anyhow!("conversion failed"))?;
    assert_eq!(converted["prompt_cache_key"], "fixture-session");
    assert_eq!(converted["tool_choice"], "none");
    value["options"]["cacheRetention"] = "none".into();
    assert!(request::convert(&value)
        .map_err(|()| anyhow!("conversion failed"))?
        .get("prompt_cache_key")
        .is_none());
    value["context"]["messages"]
        .as_array_mut()
        .ok_or_else(|| anyhow!("missing fixture array"))?
        .push(json!({"role":"system","content":"changed tools","toolsRemoved":[{"name":"read"}]}));
    let normalized =
        request::convert(&value).map_err(|()| anyhow!("tool delta conversion failed"))?;
    assert!(normalized.get("tools").is_none());
    assert!(normalized["messages"][0]["content"]
        .as_str()
        .is_some_and(|v| v.contains("changed tools")));
    assert!(matches("/alpha/v1/pi/messages", Some("alpha")));
    assert!(!matches("/alpha/v1/messages", Some("alpha")));
    Ok(())
}

#[tokio::test]
#[expect(
    clippy::panic_in_result_fn,
    reason = "Assertions intentionally verify fallible offline fixtures"
)]
async fn unsupported_operations_are_rejected_before_upstream() -> Result<()> {
    let (original, fake, _dir, token) = fixture().await?;
    let mut config = (*original.config).clone();
    config
        .providers
        .get_mut("alpha")
        .ok_or_else(|| anyhow!("missing fixture provider"))?
        .type_ = "fal".into();
    let state = AppState::new_with_upstream(config, fake.clone());
    state.init_key_store().await.map_err(|e| anyhow!(e))?;
    let result = call(state, &token, payload(), "/alpha/v1/pi/messages").await?;
    assert_eq!(result.status(), StatusCode::BAD_REQUEST);
    assert!(body(result).await?.contains("pi_provider_not_supported"));
    assert_eq!(
        fake.requests
            .lock()
            .map_err(|_| anyhow!("fixture poisoned"))?
            .len(),
        0
    );
    Ok(())
}

use super::*;
use anyhow::{anyhow, ensure, Result};
use serde_json::{json, Value};

#[test]
fn framing_and_signature_memory_are_bounded() -> Result<()> {
    let mut framer = framing::Framer::default();
    ensure!(
        framer.push(&vec![b'x'; 1024 * 1024]).is_err(),
        "unbounded frame"
    );
    let mut wire = wire::Wire::new("alpha".into(), "gpt-4o".into(), "google-generative-ai");
    let mut output = Vec::new();
    let index = wire
        .block("text".into(), wire::Kind::Text, "", "", &mut output)
        .map_err(|()| anyhow!("block"))?;
    ensure!(
        wire.signature(index, json!("x".repeat(1024 * 1024 + 1)), false)
            .is_err(),
        "unbounded signature"
    );
    ensure!(
        wire.block("k".repeat(1025), wire::Kind::Text, "", "", &mut output)
            .is_err(),
        "unbounded key"
    );
    Ok(())
}

fn reconstruct(value: &Value, kind: ProviderType) -> Result<Value> {
    let surrogate = request::convert(value).map_err(|()| anyhow!("conversion"))?;
    let context = canonical::context(&surrogate, "alpha", kind)?;
    let mut native =
        crate::transform::ProviderTransformer::for_provider(kind).transform_request(&context)?;
    canonical::finish(&mut native, &context, "alpha", kind)?;
    Ok(native)
}

#[test]
fn signed_text_tool_and_redacted_replay_are_lossless_or_rejected() -> Result<()> {
    for (kind, block, needle) in [
        (
            ProviderType::Google,
            json!({"type":"text","text":"hello","textSignature":signatures::seal(&json!("text-sig"),"alpha","gpt-4o","google-generative-ai")}),
            "text-sig",
        ),
        (
            ProviderType::Google,
            json!({"type":"toolCall","id":"call-1","name":"read","arguments":{},"thoughtSignature":signatures::seal(&json!("tool-sig"),"alpha","gpt-4o","google-generative-ai")}),
            "tool-sig",
        ),
        (
            ProviderType::Anthropic,
            json!({"type":"thinking","thinking":"","redacted":true,"thinkingSignature":signatures::seal(&json!("redacted-cipher"),"alpha","gpt-4o","anthropic-messages")}),
            "redacted-cipher",
        ),
        (
            ProviderType::OpenAIResponses,
            json!({"type":"text","text":"hello","textSignature":signatures::seal(&json!({"id":"msg-signed","phase":"final_answer"}),"alpha","gpt-4o","openai-responses")}),
            "msg-signed",
        ),
    ] {
        let mut value = tests::payload();
        value["context"]["messages"] = json!([{"role":"assistant","content":[block]}]);
        let native = reconstruct(&value, kind)?;
        ensure!(
            native.to_string().contains(needle),
            "signature lost: {native}"
        );
        value["model"] = "gpt-4".into();
        ensure!(reconstruct(&value, kind).is_err(), "model rewrite accepted");
        value["model"] = "gpt-4o".into();
        ensure!(
            reconstruct(&value, ProviderType::OpenAICompatible).is_err(),
            "cross protocol accepted"
        );
    }
    Ok(())
}

#[test]
fn exact_native_options_are_not_clamped_rounded_or_misshaped() -> Result<()> {
    let mut value = tests::payload();
    value["options"] = json!({"maxTokens":32,"temperature":0.123_456_789,"reasoning":"off","toolChoice":{"type":"function","function":{"name":"read"}}});
    let native = reconstruct(&value, ProviderType::OpenAIResponses)?;
    ensure!(native["max_output_tokens"] == 32, "tokens clamped");
    ensure!(
        native["temperature"] == value["options"]["temperature"],
        "temperature rounded"
    );
    ensure!(
        native["reasoning"]["effort"] == "none",
        "off misrepresented"
    );
    ensure!(
        native["tool_choice"] == json!({"type":"function","name":"read"}),
        "tool choice shape"
    );
    Ok(())
}

#[test]
fn unrepresentable_options_and_tool_errors_fail_before_upstream() -> Result<()> {
    for (kind, options) in [
        (ProviderType::Google, json!({"toolChoice":"required"})),
        (ProviderType::OpenAIResponses, json!({"maxTokens":15})),
        (
            ProviderType::OpenAIResponses,
            json!({"toolChoice":{"function":{"name":"read"},"extra":true}}),
        ),
        (ProviderType::Google, json!({"cacheRetention":"short"})),
        (ProviderType::Anthropic, json!({"cacheRetention":"long"})),
        (ProviderType::Anthropic, json!({"sessionId":"session"})),
        (
            ProviderType::Anthropic,
            json!({"reasoning":"high","temperature":0.5}),
        ),
    ] {
        let mut value = tests::payload();
        value["options"] = options;
        ensure!(
            reconstruct(&value, kind).is_err(),
            "option silently dropped"
        );
    }
    for kind in [
        ProviderType::Google,
        ProviderType::OpenAICompatible,
        ProviderType::OpenAIResponses,
    ] {
        let mut value = tests::payload();
        value["context"]["messages"] = json!([{"role":"toolResult","toolCallId":"call-1","toolName":"read","content":[{"type":"text","text":"failed"}],"isError":true}]);
        ensure!(
            reconstruct(&value, kind).is_err(),
            "tool error silently dropped"
        );
    }
    Ok(())
}

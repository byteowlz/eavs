use super::super::wire::{count, optional_count, reason, Counts, Kind, Wire};
use serde_json::{json, Value};
fn usage(wire: &mut Wire, value: &Value) -> Result<(), ()> {
    let mut counts = wire.counts.unwrap_or_default();
    for (key, slot) in [
        ("input_tokens", &mut counts.input),
        ("output_tokens", &mut counts.output),
        ("cache_read_input_tokens", &mut counts.read),
        ("cache_creation_input_tokens", &mut counts.write),
    ] {
        if let Some(value) = value.get(key) {
            *slot = count(value)?;
        }
    }
    wire.counts = Some(counts);
    Ok(())
}
fn start_block(
    wire: &mut Wire,
    index: usize,
    block: &Value,
    output: &mut Vec<Value>,
) -> Result<(), ()> {
    let key = format!("a:{index}");
    let kind = match block["type"].as_str().ok_or(())? {
        "text" => Kind::Text,
        "thinking" | "redacted_thinking" => Kind::Thinking,
        "tool_use" => Kind::Tool,
        _ => return Err(()),
    };
    let name = block["name"].as_str().unwrap_or_default();
    let name = if wire.oauth_tools {
        name.strip_prefix("mcp_").unwrap_or(name)
    } else {
        name
    };
    let id = wire.block(
        key,
        kind,
        block["id"].as_str().unwrap_or_default(),
        name,
        output,
    )?;
    match block["type"].as_str() {
        Some("text") => {
            if let Some(text) = block["text"].as_str().filter(|v| !v.is_empty()) {
                wire.delta(id, text, output)?;
            }
        }
        Some("thinking") => {
            if let Some(text) = block["thinking"].as_str().filter(|v| !v.is_empty()) {
                wire.delta(id, text, output)?;
            }
            if let Some(signature) = block.get("signature") {
                wire.signature(id, signature.clone(), false)?;
            }
        }
        Some("redacted_thinking") => {
            wire.redacted(id)?;
            wire.delta(id, "[Reasoning redacted]", output)?;
            wire.signature(
                id,
                block
                    .get("data")
                    .filter(|v| v.is_string())
                    .ok_or(())?
                    .clone(),
                false,
            )?;
        }
        Some("tool_use") => {
            if block["input"].as_object().is_some_and(|v| !v.is_empty()) {
                wire.delta(id, &block["input"].to_string(), output)?;
            }
        }
        _ => return Err(()),
    }
    Ok(())
}
pub(super) fn parse(
    wire: &mut Wire,
    kind: &str,
    value: &Value,
    output: &mut Vec<Value>,
) -> Result<(), ()> {
    match kind {
        "message_start" => {
            wire.start(output);
            wire.response_id = value["message"]["id"].as_str().map(str::to_owned);
            let raw = &value["message"]["usage"];
            if !raw["input_tokens"].is_number() || !raw["output_tokens"].is_number() {
                return Err(());
            }
            usage(wire, raw)?;
        }
        "content_block_start" => start_block(
            wire,
            usize::try_from(count(&value["index"])?).map_err(|_| ())?,
            &value["content_block"],
            output,
        )?,
        "content_block_delta" => {
            let id = wire.index(&format!("a:{}", count(&value["index"])?))?;
            let delta = &value["delta"];
            match delta["type"].as_str().ok_or(())? {
                "text_delta" => wire.delta(id, delta["text"].as_str().ok_or(())?, output)?,
                "thinking_delta" => {
                    wire.delta(id, delta["thinking"].as_str().ok_or(())?, output)?;
                }
                "input_json_delta" => {
                    wire.delta(id, delta["partial_json"].as_str().ok_or(())?, output)?;
                }
                "signature_delta" => wire.signature(id, delta["signature"].clone(), true)?,
                _ => return Err(()),
            }
        }
        "content_block_stop" => wire.end(
            wire.index(&format!("a:{}", count(&value["index"])?))?,
            output,
        )?,
        "message_delta" => {
            if let Some(raw) = value.get("usage") {
                usage(wire, raw)?;
            }
            if let Some(stop) = value["delta"]["stop_reason"].as_str() {
                wire.reason = Some(reason(stop)?);
            }
        }
        "message_stop" => wire.terminal = true,
        "ping" => {}
        _ => return Err(()),
    }
    Ok(())
}
pub(super) fn complete(wire: &mut Wire, value: &Value, output: &mut Vec<Value>) -> Result<(), ()> {
    wire.start(output);
    wire.response_id = value["id"].as_str().map(str::to_owned);
    let raw = &value["usage"];
    wire.counts = Some(Counts {
        input: count(&raw["input_tokens"])?,
        output: count(&raw["output_tokens"])?,
        read: optional_count(&raw["cache_read_input_tokens"])?,
        write: optional_count(&raw["cache_creation_input_tokens"])?,
    });
    for (index, block) in value["content"].as_array().ok_or(())?.iter().enumerate() {
        start_block(wire, index, block, output)?;
        if block["type"] == "tool_use" && block["input"] == json!({}) {
            wire.delta(wire.index(&format!("a:{index}"))?, "{}", output)?;
        }
        wire.end(wire.index(&format!("a:{index}"))?, output)?;
    }
    wire.reason = Some(reason(value["stop_reason"].as_str().ok_or(())?)?);
    Ok(())
}

use super::super::wire::{count, optional_count, reason, Counts, Kind, Wire};
use serde_json::{json, Value};
fn usage(wire: &mut Wire, usage: &Value) -> Result<(), ()> {
    wire.counts = Some(Counts {
        input: count(&usage["prompt_tokens"])?,
        output: count(&usage["completion_tokens"])?,
        read: optional_count(&usage["prompt_tokens_details"]["cached_tokens"])?,
        write: 0,
    });
    Ok(())
}
fn delta(wire: &mut Wire, value: &Value, output: &mut Vec<Value>) -> Result<(), ()> {
    if value.get("refusal").is_some_and(|v| !v.is_null())
        || value.get("reasoning_details").is_some_and(|v| !v.is_null())
    {
        return Err(());
    }
    for (field, key, kind) in [
        ("reasoning_content", "c:thinking", Kind::Thinking),
        ("reasoning", "c:thinking", Kind::Thinking),
        ("content", "c:text", Kind::Text),
    ] {
        if let Some(text) = value.get(field).filter(|v| !v.is_null()) {
            let index = wire.block(key.into(), kind, "", "", output)?;
            wire.delta(index, text.as_str().ok_or(())?, output)?;
        }
    }
    if let Some(calls) = value.get("tool_calls").filter(|v| !v.is_null()) {
        for call in calls.as_array().ok_or(())? {
            let index = count(&call["index"])?;
            let key = format!("c:tool:{index}");
            let id = wire.index(&key).or_else(|()| {
                wire.block(
                    key,
                    Kind::Tool,
                    call["id"].as_str().ok_or(())?,
                    call["function"]["name"].as_str().ok_or(())?,
                    output,
                )
            })?;
            if let Some(arguments) = call["function"]["arguments"].as_str() {
                wire.delta(id, arguments, output)?;
            }
        }
    }
    Ok(())
}
pub(super) fn parse(wire: &mut Wire, value: &Value, output: &mut Vec<Value>) -> Result<(), ()> {
    if value.get("error").is_some() {
        return Err(());
    }
    wire.start(output);
    if let Some(id) = value["id"].as_str() {
        wire.response_id = Some(id.to_owned());
    }
    let choices = value["choices"].as_array().ok_or(())?;
    if choices.len() > 1 {
        return Err(());
    }
    for choice in choices {
        if choice["index"].as_u64().is_some_and(|v| v != 0) {
            return Err(());
        }
        delta(wire, &choice["delta"], output)?;
        if let Some(stop) = choice["finish_reason"].as_str() {
            wire.reason = Some(reason(stop)?);
        }
    }
    if let Some(raw) = value.get("usage").filter(|v| !v.is_null()) {
        usage(wire, raw)?;
    }
    Ok(())
}
pub(super) fn complete(wire: &mut Wire, value: &Value, output: &mut Vec<Value>) -> Result<(), ()> {
    let choices = value["choices"].as_array().ok_or(())?;
    if choices.len() != 1 {
        return Err(());
    }
    let choice = choices.first().ok_or(())?;
    let mut message = choice["message"].clone();
    if let Some(calls) = message.get_mut("tool_calls").and_then(Value::as_array_mut) {
        for (index, call) in calls.iter_mut().enumerate() {
            call["index"] = index.into();
        }
    }
    parse(
        wire,
        &json!({"id":value["id"],"choices":[{"index":0,"delta":message,"finish_reason":choice["finish_reason"]}],"usage":value["usage"]}),
        output,
    )
}

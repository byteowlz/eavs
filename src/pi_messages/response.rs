use crate::keys::pricing::ModelPricing;
use serde_json::{json, Value};

fn tokens(value: &Value) -> Result<u32, ()> {
    u32::try_from(value.as_u64().ok_or(())?).map_err(|_| ())
}
fn usage(value: &Value, pricing: &ModelPricing) -> Result<Value, ()> {
    let usage = &value["usage"];
    let input = tokens(&usage["prompt_tokens"])?;
    let output = tokens(&usage["completion_tokens"])?;
    // Cache conventions vary across the existing translation layer. Never
    // claim exact Pi cache accounting until this path has dedicated fixtures.
    for value in [
        &usage["prompt_tokens_details"]["cached_tokens"],
        &usage["cache_read_input_tokens"],
        &usage["cache_creation_input_tokens"],
    ] {
        if !value.is_null() && tokens(value)? != 0 {
            return Err(());
        }
    }
    let input_cost = f64::from(input) * pricing.input_cost_per_token;
    let output_cost = f64::from(output) * pricing.output_cost_per_token;
    let total_cost = input_cost + output_cost;
    if ![input_cost, output_cost, total_cost]
        .iter()
        .all(|v| v.is_finite() && *v >= 0.0)
    {
        return Err(());
    }
    let total = input.checked_add(output).ok_or(())?;
    Ok(
        json!({"input":input,"output":output,"cacheRead":0,"cacheWrite":0,"totalTokens":total,"cost":{"input":input_cost,"output":output_cost,"cacheRead":0,"cacheWrite":0,"total":total_cost}}),
    )
}

pub(super) fn convert(value: &Value, pricing: &ModelPricing) -> Result<Vec<Value>, ()> {
    let choices = value["choices"].as_array().ok_or(())?;
    if choices.len() != 1 {
        return Err(());
    }
    let choice = choices.first().ok_or(())?;
    let message = &choice["message"];
    if message["role"] != "assistant" {
        return Err(());
    }
    if [
        "reasoning_content",
        "reasoning",
        "reasoning_details",
        "refusal",
    ]
    .iter()
    .any(|key| message.get(key).is_some_and(|v| !v.is_null()))
    {
        return Err(());
    }
    let reason = match choice["finish_reason"].as_str().ok_or(())? {
        "stop" => "stop",
        "length" => "length",
        "tool_calls" => "toolUse",
        _ => return Err(()),
    };
    let mut events = vec![json!({"type":"start"})];
    let mut index = 0;
    if let Some(content) = message["content"].as_str().filter(|v| !v.is_empty()) {
        events.push(json!({"type":"text_start","contentIndex":index}));
        events.push(json!({"type":"text_delta","contentIndex":index,"delta":content}));
        events.push(json!({"type":"text_end","contentIndex":index,"content":content}));
        index += 1;
    } else if !message["content"].is_null() && message["content"] != "" {
        return Err(());
    }
    let mut call_count = 0;
    if let Some(calls) = message.get("tool_calls").filter(|value| !value.is_null()) {
        let calls = calls.as_array().ok_or(())?;
        call_count = calls.len();
        if calls.len() > 1024 {
            return Err(());
        }
        for call in calls {
            if call["type"] != "function" {
                return Err(());
            }
            let id = call["id"].as_str().ok_or(())?;
            let name = call["function"]["name"].as_str().ok_or(())?;
            let raw = call["function"]["arguments"].as_str().ok_or(())?;
            let arguments: Value = serde_json::from_str(raw).map_err(|_| ())?;
            if !arguments.is_object() {
                return Err(());
            }
            events.push(
                json!({"type":"toolcall_start","contentIndex":index,"id":id,"toolName":name}),
            );
            events.push(json!({"type":"toolcall_delta","contentIndex":index,"delta":raw}));
            events.push(json!({"type":"toolcall_end","contentIndex":index,"toolCall":{"type":"toolCall","id":id,"name":name,"arguments":arguments}}));
            index += 1;
        }
    }
    if (reason == "toolUse") != (call_count > 0) {
        return Err(());
    }
    let mut done = json!({"type":"done","reason":reason,"usage":usage(value,pricing)?});
    if let Some(id) = value["id"].as_str() {
        done["responseId"] = id.into();
    }
    events.push(done);
    Ok(events)
}

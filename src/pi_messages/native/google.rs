use super::super::wire::{count, optional_count, reason, Counts, Kind, Wire};
use serde_json::Value;
fn part(wire: &mut Wire, part: &Value, output: &mut Vec<Value>) -> Result<(), ()> {
    if let Some(call) = part.get("functionCall") {
        let name = call["name"].as_str().ok_or(())?;
        let key = format!("g:call:{}", wire.len());
        let id = call["id"].as_str().map_or_else(
            || format!("{}-call-{}", wire.request_id, wire.len()),
            str::to_owned,
        );
        let index = wire.block(key, Kind::Tool, &id, name, output)?;
        let args = &call["args"];
        if !args.is_object() {
            return Err(());
        }
        wire.delta(index, &args.to_string(), output)?;
        if let Some(signature) = part.get("thoughtSignature") {
            wire.signature(index, signature.clone(), false)?;
        }
    } else if let Some(text) = part["text"].as_str() {
        let kind = if part["thought"] == true {
            Kind::Thinking
        } else {
            Kind::Text
        };
        let index = wire.last(kind).map_or_else(
            || wire.block(format!("g:part:{}", wire.len()), kind, "", "", output),
            Ok,
        )?;
        wire.delta(index, text, output)?;
        if let Some(signature) = part.get("thoughtSignature") {
            wire.signature(index, signature.clone(), false)?;
        }
    } else if let Some(signature) = part.get("thoughtSignature") {
        let index = wire.len().checked_sub(1).ok_or(())?;
        wire.signature(index, signature.clone(), false)?;
    } else {
        return Err(());
    }
    Ok(())
}
pub(super) fn parse(wire: &mut Wire, value: &Value, output: &mut Vec<Value>) -> Result<(), ()> {
    if value.get("error").is_some() {
        return Err(());
    }
    wire.start(output);
    if let Some(id) = value["responseId"].as_str() {
        wire.response_id = Some(id.to_owned());
    }
    if let Some(candidates) = value.get("candidates") {
        let candidates = candidates.as_array().ok_or(())?;
        if candidates.len() > 1 {
            return Err(());
        }
        for candidate in candidates {
            if let Some(parts) = candidate["content"]["parts"].as_array() {
                for value in parts {
                    part(wire, value, output)?;
                }
            }
            if let Some(stop) = candidate["finishReason"].as_str() {
                wire.reason = Some(if wire.has_tools() {
                    "toolUse"
                } else {
                    reason(stop)?
                });
                wire.terminal = true;
            }
        }
    }
    if let Some(raw) = value.get("usageMetadata") {
        wire.counts = Some(Counts {
            input: count(&raw["promptTokenCount"])?,
            output: optional_count(&raw["candidatesTokenCount"])?
                .checked_add(optional_count(&raw["thoughtsTokenCount"])?)
                .ok_or(())?,
            read: optional_count(&raw["cachedContentTokenCount"])?,
            write: 0,
        });
    }
    Ok(())
}

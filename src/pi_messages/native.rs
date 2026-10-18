mod anthropic;
mod chat;
mod google;
mod responses;
use super::wire::Wire;
use serde_json::Value;

pub fn parse(wire: &mut Wire, kind: &str, data: &str, output: &mut Vec<Value>) -> Result<(), ()> {
    if data == "[DONE]" {
        if wire.reason.is_none() {
            return Err(());
        }
        wire.terminal = true;
        return Ok(());
    }
    let value: Value = serde_json::from_str(data).map_err(|_| ())?;
    match wire.api {
        "anthropic-messages" => anthropic::parse(
            wire,
            if kind.is_empty() {
                value["type"].as_str().ok_or(())?
            } else {
                kind
            },
            &value,
            output,
        ),
        "google-generative-ai" => google::parse(wire, &value, output),
        "openai-responses" => {
            responses::parse(wire, value["type"].as_str().ok_or(())?, &value, output)
        }
        _ => chat::parse(wire, &value, output),
    }
}
pub fn complete(wire: &mut Wire, value: &Value, output: &mut Vec<Value>) -> Result<(), ()> {
    match wire.api {
        "openai-completions" => chat::complete(wire, value, output)?,
        "anthropic-messages" => anthropic::complete(wire, value, output)?,
        "google-generative-ai" => google::parse(wire, value, output)?,
        "openai-responses" => responses::complete(wire, value, output)?,
        _ => return Err(()),
    }
    wire.terminal = true;
    Ok(())
}

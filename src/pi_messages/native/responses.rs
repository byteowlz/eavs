use super::super::wire::{count, optional_count, Counts, Kind, Wire};
use serde_json::Value;
fn key(value: &Value, kind: &str) -> String {
    format!(
        "r:{kind}:{}:{}",
        value["item_id"].as_str().unwrap_or_default(),
        value["content_index"].as_u64().unwrap_or(0)
    )
}
fn item(wire: &mut Wire, item: &Value, output: &mut Vec<Value>) -> Result<(), ()> {
    let id = item["id"].as_str().ok_or(())?;
    match item["type"].as_str().ok_or(())? {
        "reasoning" => {
            let index = wire.block(format!("r:thinking:{id}:0"), Kind::Thinking, "", "", output)?;
            if wire.empty(index)? {
                for part in item["summary"].as_array().into_iter().flatten() {
                    if let Some(text) = part["text"].as_str() {
                        wire.delta(index, text, output)?;
                    }
                }
            }
            if item.get("encrypted_content").is_some_and(|v| !v.is_null()) {
                wire.signature(index, item.clone(), false)?;
            }
        }
        "function_call" => {
            let index = wire.block(
                format!("r:tool:{id}:0"),
                Kind::Tool,
                item["call_id"].as_str().ok_or(())?,
                item["name"].as_str().ok_or(())?,
                output,
            )?;
            if wire.empty(index)? {
                if let Some(args) = item["arguments"].as_str().filter(|v| !v.is_empty()) {
                    wire.delta(index, args, output)?;
                }
            }
        }
        "message" => {
            if item["role"] != "assistant" {
                return Err(());
            }
            for (part_index, part) in item["content"].as_array().ok_or(())?.iter().enumerate() {
                if part["type"] != "output_text" {
                    return Err(());
                }
                let index = wire.block(
                    format!("r:text:{id}:{part_index}"),
                    Kind::Text,
                    "",
                    "",
                    output,
                )?;
                if wire.empty(index)? {
                    if let Some(text) = part["text"].as_str() {
                        wire.delta(index, text, output)?;
                    }
                }
                wire.signature(
                    index,
                    serde_json::json!({"id":id,"phase":item["phase"]}),
                    false,
                )?;
            }
        }
        _ => return Err(()),
    }
    Ok(())
}
fn terminal(wire: &mut Wire, response: &Value, output: &mut Vec<Value>) -> Result<(), ()> {
    if let Some(items) = response["output"].as_array() {
        for value in items {
            item(wire, value, output)?;
        }
    }
    wire.response_id = response["id"].as_str().map(str::to_owned);
    let raw = &response["usage"];
    wire.counts = Some(Counts {
        input: count(&raw["input_tokens"])?,
        output: count(&raw["output_tokens"])?,
        read: optional_count(&raw["input_tokens_details"]["cached_tokens"])?,
        write: 0,
    });
    wire.reason = Some(match response["status"].as_str().ok_or(())? {
        "completed" => {
            if wire.has_tools() {
                "toolUse"
            } else {
                "stop"
            }
        }
        "incomplete" if response["incomplete_details"]["reason"] == "max_output_tokens" => "length",
        _ => return Err(()),
    });
    wire.terminal = true;
    Ok(())
}
pub(super) fn parse(
    wire: &mut Wire,
    kind: &str,
    value: &Value,
    output: &mut Vec<Value>,
) -> Result<(), ()> {
    wire.start(output);
    match kind {
        "response.created" | "response.in_progress" => {
            if let Some(id) = value["response"]["id"].as_str() {
                wire.response_id = Some(id.to_owned());
            }
        }
        "response.output_item.added" => {
            let item_value = &value["item"];
            // Message content may be empty until output_text deltas arrive.
            if item_value["type"] != "message" {
                item(wire, item_value, output)?;
            }
        }
        "response.output_item.done" => item(wire, &value["item"], output)?,
        "response.output_text.delta" => {
            let index = wire.block(key(value, "text"), Kind::Text, "", "", output)?;
            wire.delta(index, value["delta"].as_str().ok_or(())?, output)?;
        }
        "response.reasoning_summary_text.delta" | "response.reasoning.delta" => {
            let index = wire.block(
                format!("r:thinking:{}:0", value["item_id"].as_str().ok_or(())?),
                Kind::Thinking,
                "",
                "",
                output,
            )?;
            wire.delta(index, value["delta"].as_str().ok_or(())?, output)?;
        }
        "response.function_call_arguments.delta" => wire.delta(
            wire.index(&format!(
                "r:tool:{}:0",
                value["item_id"].as_str().ok_or(())?
            ))?,
            value["delta"].as_str().ok_or(())?,
            output,
        )?,
        "response.completed" | "response.incomplete" => terminal(wire, &value["response"], output)?,
        "response.content_part.added"
        | "response.content_part.done"
        | "response.output_text.done"
        | "response.reasoning_summary_part.added"
        | "response.reasoning_summary_part.done"
        | "response.reasoning_summary_text.done"
        | "response.function_call_arguments.done" => {}
        _ => return Err(()),
    }
    Ok(())
}
pub(super) fn complete(wire: &mut Wire, value: &Value, output: &mut Vec<Value>) -> Result<(), ()> {
    wire.start(output);
    terminal(wire, value, output)
}

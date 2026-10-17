use base64::Engine;
use serde_json::{json, Value};
use std::collections::BTreeMap;

fn text(value: &Value) -> Result<String, ()> {
    if let Some(value) = value.as_str() {
        return Ok(value.to_owned());
    }
    value
        .as_array()
        .ok_or(())?
        .iter()
        .map(|block| {
            if block["type"] != "text" {
                return Err(());
            }
            Ok(block["text"].as_str().ok_or(())?.to_owned())
        })
        .collect()
}
fn user_content(value: &Value) -> Result<Value, ()> {
    if value.is_string() {
        return Ok(value.clone());
    }
    let mut content = Vec::new();
    for block in value.as_array().ok_or(())? {
        match block["type"].as_str().ok_or(())? {
            "text" => content.push(json!({"type":"text","text":block["text"].as_str().ok_or(())?})),
            "image" => {
                let mime = block["mimeType"].as_str().ok_or(())?;
                if !matches!(
                    mime,
                    "image/png" | "image/jpeg" | "image/webp" | "image/gif"
                ) {
                    return Err(());
                }
                let data = block["data"].as_str().ok_or(())?;
                base64::engine::general_purpose::STANDARD
                    .decode(data)
                    .map_err(|_| ())?;
                content.push(json!({"type":"image_url","image_url":{"url":format!("data:{mime};base64,{data}")}}));
            }
            _ => return Err(()),
        }
    }
    Ok(content.into())
}
fn assistant(message: &Value) -> Result<Value, ()> {
    let mut content = String::new();
    let mut calls = Vec::new();
    for block in message["content"].as_array().ok_or(())? {
        match block["type"].as_str().ok_or(())? {
            "text" => content.push_str(block["text"].as_str().ok_or(())?),
            "thinking" => {
                block["thinking"].as_str().ok_or(())?;
            }
            "toolCall" => {
                if !block["arguments"].is_object() {
                    return Err(());
                }
                calls.push(json!({"id":block["id"].as_str().ok_or(())?,"type":"function","function":{"name":block["name"].as_str().ok_or(())?,"arguments":serde_json::to_string(&block["arguments"]).map_err(|_|())?}}));
            }
            _ => return Err(()),
        }
    }
    // Shared admission normalizes empty text to a space. Keep the surrogate
    // stable through that pass; the canonical blocks remain lossless.
    if content.is_empty() {
        content.push(' ');
    }
    let mut value = json!({"role":"assistant","content":content,"_pi_plain":content,"_pi_blocks":message["content"]});
    if !calls.is_empty() {
        value["tool_calls"] = calls.clone().into();
        value["_pi_calls"] = calls.into();
    }
    Ok(value)
}
#[derive(Default)]
struct System {
    content: Vec<String>,
    sections: BTreeMap<String, String>,
    tools: BTreeMap<String, Value>,
}
impl System {
    fn apply(&mut self, message: &Value) -> Result<(), ()> {
        let content = text(&message["content"])?;
        if !content.is_empty() {
            self.content.push(content);
        }
        if let Some(sections) = message.get("sections") {
            for (key, value) in sections.as_object().ok_or(())? {
                if value.is_null() {
                    self.sections.remove(key);
                } else {
                    self.sections
                        .insert(key.clone(), value.as_str().ok_or(())?.to_owned());
                }
            }
        }
        if let Some(removed) = message.get("toolsRemoved") {
            for tool in removed.as_array().ok_or(())? {
                self.tools.remove(tool["name"].as_str().ok_or(())?);
            }
        }
        if let Some(added) = message.get("toolsAdded") {
            for tool in added.as_array().ok_or(())? {
                if !tool["parameters"].is_object()
                    || tool
                        .get("constrainedSampling")
                        .is_some_and(|v| v != &Value::Bool(false))
                {
                    return Err(());
                }
                let name = tool["name"].as_str().ok_or(())?;
                self.tools.insert(name.to_owned(),json!({"type":"function","function":{"name":name,"description":tool["description"].as_str().ok_or(())?,"parameters":tool["parameters"]}}));
            }
        }
        Ok(())
    }
    fn prompt(&self) -> String {
        self.content
            .iter()
            .chain(self.sections.values())
            .cloned()
            .collect::<Vec<_>>()
            .join("\n\n")
    }
}
fn options(result: &mut Value, options: &Value) -> Result<(), ()> {
    for (key, value) in options.as_object().ok_or(())? {
        match key.as_str() {
            "temperature"
                if value
                    .as_f64()
                    .is_some_and(|v| v.is_finite() && (0.0..=2.0).contains(&v)) =>
            {
                result["temperature"] = value.clone();
            }
            "maxTokens"
                if value
                    .as_u64()
                    .is_some_and(|v| v > 0 && u32::try_from(v).is_ok()) =>
            {
                result["max_tokens"] = value.clone();
            }
            "reasoning"
                if value.as_str().is_some_and(|v| {
                    matches!(
                        v,
                        "off" | "minimal" | "low" | "medium" | "high" | "xhigh" | "max"
                    )
                }) =>
            {
                result["reasoning_effort"] = value.clone();
            }
            "toolChoice"
                if value == "auto"
                    || value == "none"
                    || value == "required"
                    || (value["type"] == "function"
                        && value.as_object().is_some_and(|v| v.len() == 2)
                        && value["function"].as_object().is_some_and(|v| v.len() == 1)
                        && value["function"]["name"]
                            .as_str()
                            .is_some_and(|v| !v.is_empty())) =>
            {
                result["tool_choice"] = value.clone();
            }
            "sessionId"
                if value.as_str().is_some_and(|v| {
                    !v.is_empty() && v.len() <= 256 && !v.chars().any(char::is_control)
                }) =>
            {
                result["prompt_cache_key"] = value.clone();
            }
            "cacheRetention" if value == "short" || value == "none" || value == "long" => {
                result["_pi_cache"] = value.clone();
                if value == "long" {
                    result["prompt_cache_retention"] = "24h".into();
                }
            }
            _ => return Err(()),
        }
    }
    if options["cacheRetention"] == "none" {
        result.as_object_mut().ok_or(())?.remove("prompt_cache_key");
    }
    Ok(())
}
pub(super) fn convert(value: &Value) -> Result<Value, ()> {
    if value
        .as_object()
        .ok_or(())?
        .keys()
        .any(|key| !matches!(key.as_str(), "model" | "context" | "options"))
    {
        return Err(());
    }
    let model = value["model"]
        .as_str()
        .filter(|v| !v.is_empty() && v.len() <= 512)
        .ok_or(())?;
    if value["context"]
        .as_object()
        .ok_or(())?
        .keys()
        .any(|key| key != "messages")
    {
        return Err(());
    }
    let mut messages = Vec::new();
    let mut system = System::default();
    for message in value["context"]["messages"].as_array().ok_or(())? {
        match message["role"].as_str().ok_or(())? {
            "system" => system.apply(message)?,
            "user" => messages.push(json!({"role":"user","content":user_content(&message["content"])?})),
            "assistant" => messages.push(assistant(message)?),
            "toolResult" if message.get("isError").is_none_or(Value::is_boolean) => messages.push(json!({"role":"tool","tool_call_id":message["toolCallId"].as_str().ok_or(())?,"name":message["toolName"].as_str().ok_or(())?,"content":text(&message["content"])?,"_pi_error":message["isError"].as_bool().unwrap_or(false)})),
            _ => return Err(()),
        }
    }
    if messages.is_empty() {
        return Err(());
    }
    let prompt = system.prompt();
    if !prompt.is_empty() {
        messages.insert(0, json!({"role":"system","content":prompt}));
    }
    let mut result = json!({"model":model,"messages":messages,"stream":true});
    if !system.tools.is_empty() {
        result["tools"] = system.tools.into_values().collect::<Vec<_>>().into();
    }
    if let Some(value) = value.get("options") {
        options(&mut result, value)?;
    }
    Ok(result)
}

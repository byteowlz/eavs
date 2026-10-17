//! Typed reconstruction after gateway policy/sanitization, not a second admission path.
use crate::{
    provider::ProviderType,
    transform::{parse_incoming_request, TransformError},
    types::{ContentBlock, Context, Message, TextContent, ThinkingContent, ToolCall},
};
use serde_json::Value;

fn failure() -> TransformError {
    TransformError::Unsupported("Pi history cannot be represented by this route".into())
}
pub const fn api(provider: ProviderType) -> &'static str {
    match provider {
        ProviderType::Anthropic => "anthropic-messages",
        ProviderType::Google | ProviderType::GoogleVertex => "google-generative-ai",
        ProviderType::OpenAICodex | ProviderType::OpenAIResponses => "openai-responses",
        _ => "openai-completions",
    }
}
fn signature(
    block: &Value,
    key: &str,
    route: &str,
    model: &str,
    provider: ProviderType,
) -> Result<Option<Value>, TransformError> {
    block
        .get(key)
        .map(|v| {
            super::signatures::open(v.as_str().ok_or_else(failure)?, route, model, api(provider))
                .map_err(|()| failure())
        })
        .transpose()
}
fn blocks(
    source: &Value,
    route: &str,
    model: &str,
    provider: ProviderType,
) -> Result<Vec<ContentBlock>, TransformError> {
    let mut result = Vec::new();
    for block in source.as_array().ok_or_else(failure)? {
        validate_signatures(block, route, model, provider)?;
        match block["type"].as_str().ok_or_else(failure)? {
            "text" => result.push(ContentBlock::Text(TextContent::new(
                block["text"].as_str().ok_or_else(failure)?,
            ))),
            "thinking" => {
                let raw = signature(block, "thinkingSignature", route, model, provider)?;
                if block["redacted"] == true && provider != ProviderType::Anthropic {
                    return Err(failure());
                }
                result.push(ContentBlock::Thinking(ThinkingContent {
                    thinking: block["thinking"].as_str().ok_or_else(failure)?.to_owned(),
                    signature: raw.map(|v| v.as_str().map_or_else(|| v.to_string(), str::to_owned)),
                }));
            }
            "toolCall" => result.push(ContentBlock::ToolCall(ToolCall::new(
                block["id"].as_str().ok_or_else(failure)?,
                block["name"].as_str().ok_or_else(failure)?,
                block["arguments"].clone(),
            ))),
            _ => return Err(failure()),
        }
    }
    Ok(result)
}

fn validate_signatures(
    block: &Value,
    route: &str,
    model: &str,
    provider: ProviderType,
) -> Result<(), TransformError> {
    for key in ["textSignature", "thinkingSignature", "thoughtSignature"] {
        if block.get(key).is_none() {
            continue;
        }
        let supported = match (block["type"].as_str(), key) {
            (Some("text"), "textSignature") => matches!(
                provider,
                ProviderType::Google
                    | ProviderType::GoogleVertex
                    | ProviderType::OpenAIResponses
                    | ProviderType::OpenAICodex
            ),
            (Some("thinking"), "thinkingSignature") => matches!(
                provider,
                ProviderType::Anthropic
                    | ProviderType::Google
                    | ProviderType::GoogleVertex
                    | ProviderType::OpenAIResponses
                    | ProviderType::OpenAICodex
            ),
            (Some("toolCall"), "thoughtSignature") => {
                matches!(provider, ProviderType::Google | ProviderType::GoogleVertex)
            }
            _ => false,
        };
        if !supported {
            return Err(failure());
        }
        let raw = signature(block, key, route, model, provider)?.ok_or_else(failure)?;
        if matches!(
            provider,
            ProviderType::Anthropic | ProviderType::Google | ProviderType::GoogleVertex
        ) && !raw.is_string()
        {
            return Err(failure());
        }
        if key == "textSignature"
            && matches!(
                provider,
                ProviderType::OpenAIResponses | ProviderType::OpenAICodex
            )
            && (!raw["id"].is_string()
                || raw
                    .as_object()
                    .is_none_or(|v| v.keys().any(|k| k != "id" && k != "phase"))
                || raw
                    .get("phase")
                    .is_some_and(|v| !v.is_null() && v != "commentary" && v != "final_answer"))
        {
            return Err(failure());
        }
    }
    if block.get("redacted").is_some_and(|v| !v.is_boolean())
        || (block["redacted"] == true
            && (provider != ProviderType::Anthropic
                || block["type"] != "thinking"
                || block.get("thinkingSignature").is_none()))
    {
        return Err(failure());
    }
    Ok(())
}

pub fn context(
    body: &Value,
    route: &str,
    provider: ProviderType,
) -> Result<Context, TransformError> {
    let mut context = parse_incoming_request(body)?;
    let mut sources = body["messages"]
        .as_array()
        .ok_or_else(failure)?
        .iter()
        .filter(|m| !matches!(m["role"].as_str(), Some("system" | "developer")));
    for message in &mut context.messages {
        let source = sources.next().ok_or_else(failure)?;
        if let Message::Assistant(assistant) = message {
            if let Some(value) = source.get("_pi_blocks") {
                if source["content"] != source["_pi_plain"]
                    || source["tool_calls"] != source["_pi_calls"]
                {
                    return Err(failure());
                }
                assistant.content = blocks(value, route, &context.model, provider)?;
            }
        }
        if let Message::Tool(tool) = message {
            source["name"]
                .as_str()
                .unwrap_or_default()
                .clone_into(&mut tool.tool_name);
            tool.is_error = source["_pi_error"] == true;
            if tool.is_error && provider != ProviderType::Anthropic {
                return Err(failure());
            }
        }
    }
    Ok(context)
}

/// Restore signature-bearing provider fields and options that the legacy
/// canonical serializer does not carry. Runs after policy, before credentials.
pub fn finish(
    body: &mut Value,
    context: &Context,
    route: &str,
    provider: ProviderType,
) -> Result<(), TransformError> {
    let original = context.original_request.as_ref().ok_or_else(failure)?;
    let sources: Vec<&Value> = original["messages"]
        .as_array()
        .ok_or_else(failure)?
        .iter()
        .filter(|m| m["role"] == "assistant")
        .collect();
    match provider {
        ProviderType::Anthropic => restore_anthropic(body, &sources, route, &context.model)?,
        ProviderType::Google | ProviderType::GoogleVertex => {
            restore_google(body, &sources, route, &context.model, provider)?;
        }
        ProviderType::OpenAIResponses | ProviderType::OpenAICodex => {
            restore_responses(body, &sources, route, &context.model, provider)?;
        }
        _ => restore_chat(body, &sources)?,
    }
    options(body, original, provider)?;
    Ok(())
}
fn restore_anthropic(
    body: &mut Value,
    sources: &[&Value],
    route: &str,
    model: &str,
) -> Result<(), TransformError> {
    let mut sources = sources.iter();
    for message in body["messages"].as_array_mut().ok_or_else(failure)? {
        if message["role"] != "assistant" {
            continue;
        }
        let source = sources.next().ok_or_else(failure)?;
        let Some(blocks) = source.get("_pi_blocks").and_then(Value::as_array) else {
            continue;
        };
        let content = message["content"].as_array_mut().ok_or_else(failure)?;
        if content.len() != blocks.len() {
            return Err(failure());
        }
        for (target, block) in content.iter_mut().zip(blocks) {
            if block["type"] == "thinking" && block["redacted"] == true {
                let raw = signature(
                    block,
                    "thinkingSignature",
                    route,
                    model,
                    ProviderType::Anthropic,
                )?
                .ok_or_else(failure)?;
                *target = serde_json::json!({"type":"redacted_thinking","data":raw});
            }
            if block.get("textSignature").is_some() {
                return Err(failure());
            }
        }
    }
    Ok(())
}
fn restore_google(
    body: &mut Value,
    sources: &[&Value],
    route: &str,
    model: &str,
    provider: ProviderType,
) -> Result<(), TransformError> {
    let mut sources = sources.iter();
    for message in body["contents"].as_array_mut().ok_or_else(failure)? {
        if message["role"] != "model" {
            continue;
        }
        let source = sources.next().ok_or_else(failure)?;
        let Some(blocks) = source.get("_pi_blocks").and_then(Value::as_array) else {
            continue;
        };
        let parts = message["parts"].as_array_mut().ok_or_else(failure)?;
        if parts.len() != blocks.len() {
            return Err(failure());
        }
        for (part, block) in parts.iter_mut().zip(blocks) {
            let key = match block["type"].as_str() {
                Some("thinking") => "thinkingSignature",
                Some("text") => "textSignature",
                Some("toolCall") => "thoughtSignature",
                _ => return Err(failure()),
            };
            if let Some(raw) = signature(block, key, route, model, provider)? {
                part["thoughtSignature"] = raw;
            }
        }
    }
    Ok(())
}
fn restore_responses(
    body: &mut Value,
    sources: &[&Value],
    route: &str,
    model: &str,
    provider: ProviderType,
) -> Result<(), TransformError> {
    let mut signed = Vec::new();
    let mut texts = Vec::new();
    for source in sources {
        for block in source["_pi_blocks"].as_array().into_iter().flatten() {
            if block["type"] == "thinking" {
                signed.push(signature(
                    block,
                    "thinkingSignature",
                    route,
                    model,
                    provider,
                )?);
            } else if block["type"] == "text" {
                texts.push(signature(block, "textSignature", route, model, provider)?);
            }
        }
    }
    let mut texts = texts.into_iter();
    let mut signed = signed.into_iter();
    for item in body["input"].as_array_mut().ok_or_else(failure)? {
        if item["type"] == "message" && item["role"] == "assistant" {
            if let Some(Some(raw)) = texts.next() {
                item["id"] = raw
                    .get("id")
                    .filter(|v| v.is_string())
                    .ok_or_else(failure)?
                    .clone();
                if !raw["phase"].is_null() {
                    item["phase"] = raw["phase"].clone();
                }
            }
        }
        if item["type"] == "reasoning" {
            if let Some(Some(raw)) = signed.next() {
                if !raw.is_object() || raw["type"] != "reasoning" {
                    return Err(failure());
                }
                *item = raw;
            }
        }
    }
    Ok(())
}
fn restore_chat(body: &mut Value, sources: &[&Value]) -> Result<(), TransformError> {
    let mut sources = sources.iter();
    for message in body["messages"].as_array_mut().ok_or_else(failure)? {
        if message["role"] != "assistant" {
            continue;
        }
        let source = sources.next().ok_or_else(failure)?;
        let Some(blocks) = source.get("_pi_blocks").and_then(Value::as_array) else {
            continue;
        };
        if blocks
            .iter()
            .any(|b| b.get("thinkingSignature").is_some() || b.get("textSignature").is_some())
        {
            return Err(failure());
        }
        let text: String = blocks
            .iter()
            .filter(|b| b["type"] == "text")
            .filter_map(|b| b["text"].as_str())
            .collect();
        let thinking: String = blocks
            .iter()
            .filter(|b| b["type"] == "thinking")
            .filter_map(|b| b["thinking"].as_str())
            .collect();
        message["content"] = text.into();
        if !thinking.is_empty() {
            message["reasoning_content"] = thinking.into();
        }
    }
    Ok(())
}
fn validate_options(original: &Value, provider: ProviderType) -> Result<(), TransformError> {
    if matches!(
        provider,
        ProviderType::OpenAIResponses | ProviderType::OpenAICodex
    ) && original["max_tokens"].as_u64().is_some_and(|v| v < 16)
    {
        return Err(failure());
    }
    let reasoning = original["reasoning_effort"].as_str();
    let native_cache = matches!(
        provider,
        ProviderType::Anthropic | ProviderType::Google | ProviderType::GoogleVertex
    );
    if native_cache && original.get("prompt_cache_key").is_some() {
        return Err(failure());
    }
    if native_cache && original["_pi_cache"] == "long" {
        return Err(failure());
    }
    if matches!(provider, ProviderType::Google | ProviderType::GoogleVertex)
        && (original.get("tool_choice").is_some() || original.get("_pi_cache").is_some())
    {
        return Err(failure());
    }
    if provider == ProviderType::Anthropic
        && reasoning.is_some_and(|v| v != "off")
        && original.get("temperature").is_some()
    {
        return Err(failure());
    }
    Ok(())
}
fn options(
    body: &mut Value,
    original: &Value,
    provider: ProviderType,
) -> Result<(), TransformError> {
    validate_options(original, provider)?;
    if let Some(temperature) = original.get("temperature") {
        if matches!(provider, ProviderType::Google | ProviderType::GoogleVertex) {
            body["generationConfig"]["temperature"] = temperature.clone();
        } else {
            body["temperature"] = temperature.clone();
        }
    }
    let reasoning = original["reasoning_effort"].as_str();
    match provider {
        ProviderType::Anthropic => {
            if let Some(level) = reasoning.filter(|v| *v != "off") {
                let budget = match level {
                    "minimal" | "low" => 1024,
                    "medium" => 4096,
                    "high" | "xhigh" | "max" => 8192,
                    _ => return Err(failure()),
                };
                if body["max_tokens"].as_u64().is_some_and(|v| v <= budget) {
                    return Err(failure());
                }
                body["thinking"] = serde_json::json!({"type":"enabled","budget_tokens":budget});
                body.as_object_mut()
                    .ok_or_else(failure)?
                    .remove("temperature");
            }
            if original["_pi_cache"] == "none" {
                for value in ["system", "messages", "tools"] {
                    if let Some(items) = body.get_mut(value).and_then(Value::as_array_mut) {
                        for item in items {
                            strip_cache(item);
                        }
                    }
                }
            }
        }
        ProviderType::Google | ProviderType::GoogleVertex => {
            if let Some(level) = reasoning {
                let budget = match level {
                    "off" => 0,
                    "minimal" | "low" => 1024,
                    "medium" => 4096,
                    "high" | "xhigh" | "max" => 8192,
                    _ => return Err(failure()),
                };
                body["generationConfig"]["thinkingConfig"] =
                    serde_json::json!({"thinkingBudget":budget,"includeThoughts":budget > 0});
            }
        }
        _ => openai_options(body, original, provider),
    }
    Ok(())
}
fn openai_options(body: &mut Value, original: &Value, provider: ProviderType) {
    let reasoning = original["reasoning_effort"].as_str();
    for name in ["tool_choice", "prompt_cache_key", "prompt_cache_retention"] {
        if let Some(value) = original.get(name) {
            body[name] = value.clone();
        }
    }
    if matches!(
        provider,
        ProviderType::OpenAIResponses | ProviderType::OpenAICodex
    ) && original["tool_choice"].is_object()
    {
        body["tool_choice"] = serde_json::json!({"type":"function","name":original["tool_choice"]["function"]["name"]});
    }
    if let Some(level) = reasoning {
        if matches!(
            provider,
            ProviderType::OpenAICodex | ProviderType::OpenAIResponses
        ) {
            body["reasoning"] = if level == "off" {
                serde_json::json!({"effort":"none"})
            } else {
                serde_json::json!({"effort":level,"summary":"auto"})
            };
            if level != "off" {
                body["include"] = serde_json::json!(["reasoning.encrypted_content"]);
            }
        } else {
            body["reasoning_effort"] = if level == "off" { "none" } else { level }.into();
        }
    }
}
fn strip_cache(value: &mut Value) {
    match value {
        Value::Object(fields) => {
            fields.remove("cache_control");
            for value in fields.values_mut() {
                strip_cache(value);
            }
        }
        Value::Array(items) => {
            for value in items {
                strip_cache(value);
            }
        }
        _ => {}
    }
}

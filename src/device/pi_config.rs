//! Live scoped catalog -> Pi config. Never executes/echoes existing credential fields.
use super::{check_candidate, identifier, revision, storage, Connection};
use anyhow::{bail, Context, Result};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use serde_json::{json, Map, Value};
use std::path::Path;

pub(super) struct Publication<'a> {
    pub output: &'a Path,
    pub registry_path: &'a Path,
    pub registry_before: &'a [u8],
    pub expected: Option<&'a str>,
    pub candidate: Option<&'a str>,
    pub apply: bool,
}
fn namespace(connection: &Connection) -> String {
    format!("eavs-device-{}.", URL_SAFE_NO_PAD.encode(&connection.name))
}
fn existing(bytes: &[u8]) -> Result<Map<String, Value>> {
    if bytes.is_empty() {
        return Ok(Map::new());
    }
    let value: Value = serde_json::from_slice(bytes)
        .map_err(|_| anyhow::anyhow!("invalid Pi JSON; repair before retrying"))?;
    let object = value
        .as_object()
        .context("Pi configuration must be an object")?;
    if object.contains_key("connections") {
        bail!("device registry is not a Pi configuration");
    }
    if let Some(providers) = object.get("providers") {
        if !providers
            .as_object()
            .is_some_and(|map| map.values().all(Value::is_object))
        {
            bail!("Pi providers must be an object map of objects");
        }
    }
    Ok(object.clone())
}
fn route(provider: &Value) -> Result<&'static str> {
    let id = provider["id"]
        .as_str()
        .filter(|id| identifier(id))
        .context("invalid catalog provider")?;
    let (api, path) = match provider["api"].as_str() {
        Some("openai-completions") => ("openai-completions", "/chat/completions"),
        Some("openai-responses" | "openai-codex-responses") => ("openai-responses", "/responses"),
        Some("anthropic-messages") => ("anthropic-messages", "/messages"),
        _ => bail!("catalog API not representable by Pi native config"),
    };
    if provider["operation"] != "chat"
        || provider["base_path"] != format!("/{id}/v1")
        || provider["request_path"] != format!("/{id}/v1{path}")
    {
        bail!("catalog route does not match the supported native contract");
    }
    Ok(api)
}
fn compat(value: &Value) -> Value {
    let mut out = Map::new();
    for name in [
        "supportsStore",
        "supportsDeveloperRole",
        "supportsReasoningEffort",
        "supportsUsageInStreaming",
        "supportsToolChoice",
        "supportsStrictMode",
        "supportsImagesInToolResults",
    ] {
        if let Some(value) = value.get(name).filter(|value| value.is_boolean()) {
            out.insert(name.into(), value.clone());
        }
    }
    if matches!(
        value["maxTokensField"].as_str(),
        Some("max_tokens" | "max_completion_tokens")
    ) {
        out.insert("maxTokensField".into(), value["maxTokensField"].clone());
    }
    Value::Object(out)
}
fn model(value: &Value) -> Option<Value> {
    let pricing = &value["pricing"];
    if pricing["status"] != "unverified_estimate" || pricing["unit"] != "USD_per_million_tokens" {
        return None;
    }
    let rates = &pricing["rates"];
    let mut costs = Map::new();
    for (source, target) in [
        ("input", "input"),
        ("output", "output"),
        ("cache_read", "cacheRead"),
        ("cache_write", "cacheWrite"),
    ] {
        let rate = rates[source]
            .as_f64()
            .filter(|rate| rate.is_finite() && *rate >= 0.0)?;
        if matches!(source, "input" | "output") && rate <= 0.0 {
            return None;
        }
        costs.insert(target.into(), json!(rate));
    }
    let context = value["context_window"]
        .as_u64()
        .filter(|v| *v > 0 && *v <= 10_000_000)?;
    let max = value["max_tokens"]
        .as_u64()
        .filter(|v| *v > 0 && *v <= context)?;
    let input = value["input"].as_array()?;
    if input.is_empty() || !input.iter().all(|v| v == "text" || v == "image") {
        return None;
    }
    Some(
        json!({"id":value["id"],"name":value["name"],"reasoning":value["reasoning"].as_bool()?,"input":input,"contextWindow":context,"maxTokens":max,"cost":costs,"compat":compat(&value["compat"])}),
    )
}
fn generated(connection: &Connection, catalog: &Value) -> Result<(Map<String, Value>, usize)> {
    super::project(&serde_json::to_vec(catalog)?)?;
    let mut providers = Map::new();
    let mut skipped = 0;
    for provider in catalog["providers"]
        .as_array()
        .context("catalog providers missing")?
    {
        let api = route(provider)?;
        let mut models = Vec::new();
        for entry in provider["models"]
            .as_array()
            .context("catalog models missing")?
        {
            if let Some(model) = model(entry) {
                models.push(model);
            } else {
                skipped += 1;
            }
        }
        if models.is_empty() {
            continue;
        }
        let id = provider["id"]
            .as_str()
            .context("catalog provider missing")?;
        let mut headers = json!({"Authorization":format!("Bearer ${{{}}}",connection.key_env)});
        if let Some(reference) = &connection.listener_key_env {
            headers["X-Eavs-Token"] = format!("${{{reference}}}").into();
        }
        let mut entry = json!({"baseUrl":format!("{}{base}",connection.url.trim_end_matches('/'),base=provider["base_path"].as_str().context("catalog base path missing")?),"api":api,"apiKey":format!("${{{}}}",connection.key_env),"headers":headers,"models":models});
        if api == "openai-completions" {
            entry["compat"] = compat(&provider["compat"]);
        }
        providers.insert(
            format!(
                "{}{id}",
                namespace(connection),
                id = URL_SAFE_NO_PAD.encode(id)
            ),
            entry,
        );
    }
    if providers.is_empty() && skipped > 0 {
        bail!("no Pi-exportable priced chat candidates; existing config unchanged");
    }
    Ok((providers, skipped))
}
pub(super) fn export(connection: &Connection, catalog: &Value) -> Result<Value> {
    let (providers, skipped) = generated(connection, catalog)?;
    Ok(
        json!({"schema_version":1,"providers":providers,"skipped_models":skipped,"pricing":"unverified_estimate"}),
    )
}
pub(super) fn configure(
    connection: &Connection,
    catalog: &Value,
    publication: &Publication<'_>,
) -> Result<Value> {
    if !publication.output.is_absolute()
        || publication
            .output
            .file_name()
            .is_none_or(|name| name != "models.json")
    {
        bail!("Pi target must be an explicit absolute models.json path");
    }
    let old = storage::read(publication.output)?;
    if old.is_empty() && publication.output.exists() {
        bail!("empty Pi configuration; repair before retrying");
    }
    let mut root = existing(&old)?;
    let mut providers = root
        .remove("providers")
        .and_then(|v| v.as_object().cloned())
        .unwrap_or_default();
    let (generated, skipped) = generated(connection, catalog)?;
    let prefix = namespace(connection);
    let removed: Vec<_> = providers
        .keys()
        .filter(|key| key.starts_with(&prefix))
        .cloned()
        .collect();
    for name in &removed {
        providers.remove(name);
    }
    let names: Vec<_> = generated.keys().cloned().collect();
    providers.extend(generated);
    root.insert("providers".into(), Value::Object(providers));
    let candidate = serde_json::to_vec_pretty(&root)?;
    if publication.apply {
        check_candidate(&candidate, publication.candidate)?;
        storage::with_snapshot(
            publication.registry_path,
            publication.registry_before,
            || storage::apply(publication.output, &old, &candidate, publication.expected),
        )?;
    }
    // Do not echo the merged document: unrelated providers may contain secrets/commands.
    Ok(
        json!({"schema_version":1,"operation":"pi_config","applied":publication.apply,"target":publication.output,"revision":revision(&old),"candidate_revision":revision(&candidate),"managed_providers":names,"replaced_providers":removed,"skipped_models":skipped,"pricing":"unverified_estimate","scope":"live-key-filtered-chat-candidates"}),
    )
}

#[cfg(test)]
mod tests;

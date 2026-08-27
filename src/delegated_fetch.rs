use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashSet;

/// Effective policy for capabilities that make the provider fetch or execute
/// content outside EAVS's network boundary.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DelegatedFetchPolicy {
    pub allow_remote_content: bool,
    pub allowed_server_tools: Vec<String>,
}

/// User-facing configuration. Remote content and provider-hosted tools are
/// denied unless explicitly enabled/allowlisted.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct DelegatedFetchConfig {
    /// Permit remote `input_file.file_url` content.
    pub enabled: bool,
    /// Provider-hosted tool types allowed through, for example `web_search`.
    pub allowed_server_tools: Vec<String>,
}

impl DelegatedFetchConfig {
    pub fn policy(&self) -> DelegatedFetchPolicy {
        DelegatedFetchPolicy {
            allow_remote_content: self.enabled,
            allowed_server_tools: self.allowed_server_tools.clone(),
        }
    }

    /// Resolve tenant policy from virtual-key metadata. For an authenticated
    /// virtual key, absence or malformation is deliberately deny-by-default
    /// even when the process-wide setting allows remote content.
    pub fn policy_for_key_metadata(&self, metadata: Option<&Value>) -> DelegatedFetchPolicy {
        let mut policy = self.policy();
        if let Some(metadata) = metadata {
            policy.allow_remote_content = metadata
                .pointer("/delegated_fetch/remote_content")
                .and_then(Value::as_bool)
                .unwrap_or(false);
        }
        policy
    }
}

/// One capability removed from a request body for structured audit logging.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct StrippedItem {
    pub field_path: String,
    pub capability: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target_host: Option<String>,
}

/// Remove delegated-fetch capabilities from an OpenAI-compatible request.
///
/// The function deliberately mutates only known capability surfaces. Bodies
/// without those surfaces remain byte-for-byte equivalent when serialized.
pub fn sanitize(body: &mut Value, policy: &DelegatedFetchPolicy) -> Vec<StrippedItem> {
    let mut stripped = Vec::new();

    if !policy.allow_remote_content {
        sanitize_remote_input_files(body, &mut stripped);
    }

    let removed_tool_types = sanitize_server_tools(body, policy, &mut stripped);
    normalize_tool_choice(body, &removed_tool_types, &mut stripped);

    stripped
}

fn sanitize_remote_input_files(body: &mut Value, stripped: &mut Vec<StrippedItem>) {
    let Some(input) = body.get_mut("input").and_then(Value::as_array_mut) else {
        return;
    };

    let original = std::mem::take(input);
    for (input_index, mut item) in original.into_iter().enumerate() {
        if is_remote_input_file(&item) {
            stripped.push(remote_file_audit_item(
                &item,
                format!("/input/{input_index}/file_url"),
            ));
            continue;
        }

        if let Some(content) = item.get_mut("content").and_then(Value::as_array_mut) {
            let original_content = std::mem::take(content);
            for (content_index, content_item) in original_content.into_iter().enumerate() {
                if is_remote_input_file(&content_item) {
                    stripped.push(remote_file_audit_item(
                        &content_item,
                        format!("/input/{input_index}/content/{content_index}/file_url"),
                    ));
                } else {
                    content.push(content_item);
                }
            }
        }

        input.push(item);
    }
}

fn is_remote_input_file(value: &Value) -> bool {
    value.get("type").and_then(Value::as_str) == Some("input_file")
        && value.get("file_url").and_then(Value::as_str).is_some()
}

fn remote_file_audit_item(value: &Value, field_path: String) -> StrippedItem {
    let file_url = value
        .get("file_url")
        .and_then(Value::as_str)
        .unwrap_or_default();
    StrippedItem {
        field_path,
        capability: "input_file.file_url".to_string(),
        target_host: url::Url::parse(file_url)
            .ok()
            .and_then(|parsed| parsed.host_str().map(ToString::to_string)),
    }
}

fn sanitize_server_tools(
    body: &mut Value,
    policy: &DelegatedFetchPolicy,
    stripped: &mut Vec<StrippedItem>,
) -> HashSet<String> {
    let Some(tools) = body.get_mut("tools").and_then(Value::as_array_mut) else {
        return HashSet::new();
    };

    let allowed: HashSet<String> = policy
        .allowed_server_tools
        .iter()
        .map(|tool| tool.to_ascii_lowercase())
        .collect();
    let mut removed = HashSet::new();
    let original = std::mem::take(tools);

    for (index, tool) in original.into_iter().enumerate() {
        let tool_type = tool.get("type").and_then(Value::as_str);
        if let Some(tool_type) = tool_type {
            if is_server_side_tool(tool_type) && !allowed.contains(&tool_type.to_ascii_lowercase())
            {
                removed.insert(tool_type.to_ascii_lowercase());
                stripped.push(StrippedItem {
                    field_path: format!("/tools/{index}"),
                    capability: format!("server_tool:{tool_type}"),
                    target_host: None,
                });
                continue;
            }
        }
        tools.push(tool);
    }

    removed
}

/// Function/custom tools execute client-side. Typed tools outside that set are
/// provider-hosted and therefore bypass EAVS's egress controls unless allowed.
fn is_server_side_tool(tool_type: &str) -> bool {
    !tool_type.eq_ignore_ascii_case("function") && !tool_type.eq_ignore_ascii_case("custom")
}

fn normalize_tool_choice(
    body: &mut Value,
    removed_tool_types: &HashSet<String>,
    stripped: &mut Vec<StrippedItem>,
) {
    if removed_tool_types.is_empty() {
        return;
    }

    let remaining_tools = body
        .get("tools")
        .and_then(Value::as_array)
        .map_or(0, Vec::len);
    let Some(choice) = body.get("tool_choice") else {
        return;
    };

    let references_removed = match choice {
        Value::String(choice) => removed_tool_types.contains(&choice.to_ascii_lowercase()),
        Value::Object(choice) => ["type", "name"]
            .iter()
            .filter_map(|field| choice.get(*field).and_then(Value::as_str))
            .any(|value| removed_tool_types.contains(&value.to_ascii_lowercase())),
        _ => false,
    };
    let requires_missing_tool =
        remaining_tools == 0 && !matches!(choice.as_str(), Some("auto") | Some("none"));

    if references_removed || requires_missing_tool {
        body["tool_choice"] = Value::String("auto".to_string());
        stripped.push(StrippedItem {
            field_path: "/tool_choice".to_string(),
            capability: "tool_choice_normalized".to_string(),
            target_host: None,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn deny_policy() -> DelegatedFetchPolicy {
        DelegatedFetchPolicy::default()
    }

    #[test]
    fn strips_remote_input_file_and_records_host() {
        let mut body = json!({
            "model": "gpt-5",
            "input": [{
                "role": "user",
                "content": [
                    {"type": "input_text", "text": "summarize"},
                    {"type": "input_file", "file_url": "https://metadata.example/private.pdf"}
                ]
            }]
        });

        let stripped = sanitize(&mut body, &deny_policy());

        assert_eq!(body["input"][0]["content"].as_array().unwrap().len(), 1);
        assert_eq!(stripped.len(), 1);
        assert_eq!(stripped[0].field_path, "/input/0/content/1/file_url");
        assert_eq!(stripped[0].target_host.as_deref(), Some("metadata.example"));
    }

    #[test]
    fn strips_server_tools_and_normalizes_removed_tool_choice() {
        let mut body = json!({
            "tools": [
                {"type": "function", "function": {"name": "local_tool"}},
                {"type": "web_search"},
                {"type": "web_fetch"}
            ],
            "tool_choice": {"type": "web_search"}
        });

        let stripped = sanitize(&mut body, &deny_policy());

        assert_eq!(body["tools"].as_array().unwrap().len(), 1);
        assert_eq!(body["tools"][0]["type"], "function");
        assert_eq!(body["tool_choice"], "auto");
        assert_eq!(stripped.len(), 3);
    }

    #[test]
    fn allows_explicit_remote_content_and_server_tool() {
        let original = json!({
            "input": [{"type": "input_file", "file_url": "https://example.com/a.pdf"}],
            "tools": [{"type": "web_search"}],
            "tool_choice": {"type": "web_search"}
        });
        let mut body = original.clone();
        let policy = DelegatedFetchPolicy {
            allow_remote_content: true,
            allowed_server_tools: vec!["web_search".to_string()],
        };

        assert!(sanitize(&mut body, &policy).is_empty());
        assert_eq!(body, original);
    }

    #[test]
    fn virtual_key_metadata_overrides_global_remote_content_with_default_deny() {
        let config = DelegatedFetchConfig {
            enabled: true,
            allowed_server_tools: vec!["web_search".to_string()],
        };

        let absent = config.policy_for_key_metadata(Some(&json!({})));
        assert!(!absent.allow_remote_content);
        assert_eq!(absent.allowed_server_tools, ["web_search"]);

        let denied = config.policy_for_key_metadata(Some(&json!({
            "delegated_fetch": {"remote_content": false}
        })));
        assert!(!denied.allow_remote_content);

        let allowed = config.policy_for_key_metadata(Some(&json!({
            "delegated_fetch": {"remote_content": true}
        })));
        assert!(allowed.allow_remote_content);

        assert!(config.policy_for_key_metadata(None).allow_remote_content);
    }

    #[test]
    fn safe_control_body_is_unchanged() {
        let original = json!({
            "model": "gpt-5",
            "input": [{
                "role": "user",
                "content": [{"type": "input_text", "text": "hello"}]
            }],
            "tools": [{"type": "function", "function": {"name": "local_tool"}}],
            "tool_choice": "auto"
        });
        let original_bytes = serde_json::to_vec(&original).unwrap();
        let mut body = original;

        assert!(sanitize(&mut body, &deny_policy()).is_empty());
        assert_eq!(serde_json::to_vec(&body).unwrap(), original_bytes);
    }
}

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
#[derive(PartialEq)]
pub struct DelegatedFetchConfig {
    /// Permit provider-delegated remote file, image, and document content.
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

/// Remove known delegated-fetch capabilities from provider request bodies.
///
/// Covers `OpenAI` Chat/Responses, Anthropic and Google media content, including
/// HTTP and Realtime/Codex envelopes. Hosted-tool handling covers top-level
/// tools and Realtime session.update. This is not a general URL scanner.
///
/// The function deliberately mutates only known capability surfaces. Bodies
/// without those surfaces remain byte-for-byte equivalent when serialized.
pub fn sanitize(body: &mut Value, policy: &DelegatedFetchPolicy) -> Vec<StrippedItem> {
    let mut stripped = Vec::new();

    if !policy.allow_remote_content {
        sanitize_remote_media(body, &mut stripped, "");
    }

    let removed_tool_types = sanitize_server_tools(body, policy, &mut stripped);
    normalize_tool_choice(body, &removed_tool_types, &mut stripped);

    // Realtime WebSocket protocol surfaces.
    if body.get("type").and_then(Value::as_str) == Some("session.update") {
        sanitize_realtime_session_update(body, policy, &mut stripped)
    }

    stripped
}

/// Walk only protocol content/envelope fields, never arbitrary metadata,
/// tool schemas, arguments, or text. Original array indices are retained in
/// audit paths even when earlier siblings are removed.
fn sanitize_remote_media(body: &mut Value, stripped: &mut Vec<StrippedItem>, prefix: &str) {
    sanitize_media_arrays(
        body,
        stripped,
        prefix,
        &["input", "messages", "contents", "content", "parts"],
    );
    // Realtime item/response envelopes and wrapped HTTP/Codex requests.
    for field in ["item", "response", "request", "session"] {
        if let Some(envelope) = body.get_mut(field).filter(|value| value.is_object()) {
            sanitize_remote_media(envelope, stripped, &format!("{prefix}/{field}"));
        }
    }
}

fn sanitize_media_arrays(
    body: &mut Value,
    stripped: &mut Vec<StrippedItem>,
    prefix: &str,
    fields: &[&str],
) {
    for field in fields {
        if let Some(items) = body.get_mut(*field).and_then(Value::as_array_mut) {
            let original = std::mem::take(items);
            for (index, mut item) in original.into_iter().enumerate() {
                let path = format!("{prefix}/{field}/{index}");
                let references = remote_media_references(&item);
                if references.is_empty() {
                    // Only message/content containers may nest content. In
                    // particular, never interpret function-call JSON as a request.
                    if matches!(
                        item.get("type").and_then(Value::as_str),
                        None | Some("message" | "tool_result")
                    ) {
                        sanitize_media_arrays(&mut item, stripped, &path, &["content", "parts"]);
                    }
                    items.push(item);
                } else {
                    for (suffix, capability, uri) in references {
                        stripped.push(StrippedItem {
                            field_path: format!("{path}{suffix}"),
                            capability: capability.to_string(),
                            target_host: url::Url::parse(uri)
                                .ok()
                                .and_then(|url| url.host_str().map(ToString::to_string)),
                        });
                    }
                }
            }
        }
    }
}

/// Fail closed for non-inline URI strings, including non-HTTP provider storage
/// schemes and malformed URLs. URL parsing is for audit hosts, not permission.
fn remote_media_references(value: &Value) -> Vec<(&'static str, &'static str, &str)> {
    let mut references = Vec::new();
    let mut add = |path, capability| {
        if let Some(uri) = value.pointer(path).and_then(Value::as_str) {
            if !uri
                .trim_start()
                .get(..5)
                .is_some_and(|scheme| scheme.eq_ignore_ascii_case("data:"))
            {
                references.push((path, capability, uri));
            }
        }
    };
    match value.get("type").and_then(Value::as_str) {
        Some("input_file") => add("/file_url", "input_file.file_url"),
        Some("input_image") => {
            add("/image_url", "input_image.image_url");
            add("/image_url/url", "input_image.image_url");
        }
        Some("image_url") => {
            add("/image_url/url", "image_url.url");
            add("/image_url", "image_url.url");
        }
        Some("image" | "document")
            if value.pointer("/source/type").and_then(Value::as_str) == Some("url") =>
        {
            add("/source/url", "source.url");
        }
        _ => {}
    }
    for (path, capability) in [
        ("/fileData/fileUri", "fileData.fileUri"),
        ("/fileData/file_uri", "fileData.file_uri"),
        ("/file_data/file_uri", "file_data.file_uri"),
        ("/file_data/fileUri", "file_data.fileUri"),
    ] {
        add(path, capability);
    }
    references
}

fn sanitize_server_tools(
    body: &mut Value,
    policy: &DelegatedFetchPolicy,
    stripped: &mut Vec<StrippedItem>,
) -> HashSet<String> {
    let Some(tools) = body.get_mut("tools").and_then(Value::as_array_mut) else {
        return HashSet::new();
    };

    strip_server_tools_from_array(tools, policy, stripped, "/tools")
}

/// Strip provider-hosted tools from a tools array, recording each one.
///
/// Used for both the top-level `tools` array (`OpenAI` Responses) and the
/// Realtime `session.update` `session.tools` array.
fn strip_server_tools_from_array(
    tools: &mut Vec<Value>,
    policy: &DelegatedFetchPolicy,
    stripped: &mut Vec<StrippedItem>,
    field_prefix: &str,
) -> HashSet<String> {
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
                    field_path: format!("{field_prefix}/{index}"),
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

/// Strip provider-hosted tools under `session.tools` on a Realtime
/// `session.update` frame, and normalize `session.tool_choice` if needed.
fn sanitize_realtime_session_update(
    body: &mut Value,
    policy: &DelegatedFetchPolicy,
    stripped: &mut Vec<StrippedItem>,
) {
    let Some(session) = body.get_mut("session") else {
        return;
    };

    let removed = session
        .get_mut("tools")
        .and_then(Value::as_array_mut)
        .map_or_else(HashSet::new, |tools| {
            strip_server_tools_from_array(tools, policy, stripped, "/session/tools")
        });

    if !removed.is_empty() {
        normalize_tool_choice(session, &removed, stripped);
    }
}

/// Function/custom tools execute client-side. Typed tools outside that set are
/// provider-hosted and therefore bypass EAVS's egress controls unless allowed.
const fn is_server_side_tool(tool_type: &str) -> bool {
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
        remaining_tools == 0 && !matches!(choice.as_str(), Some("auto" | "none"));

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

    #[test]
    fn remote_media_matrix_across_http_realtime_and_codex_envelopes() {
        let cases = [
            (
                json!({"type":"image_url","image_url":{"url":"https://media.example/a?secret=x"}}),
                "/image_url/url",
                "image_url.url",
            ),
            (
                json!({"type":"input_image","image_url":"https://media.example/a"}),
                "/image_url",
                "input_image.image_url",
            ),
            (
                json!({"type":"image","source":{"type":"url","url":"https://media.example/a"}}),
                "/source/url",
                "source.url",
            ),
            (
                json!({"type":"document","source":{"type":"url","url":"https://media.example/a"}}),
                "/source/url",
                "source.url",
            ),
            (
                json!({"fileData":{"fileUri":"gs://media.example/a"}}),
                "/fileData/fileUri",
                "fileData.fileUri",
            ),
            (
                json!({"file_data":{"file_uri":"https://media.example/a"}}),
                "/file_data/file_uri",
                "file_data.file_uri",
            ),
        ];
        for (part, suffix, capability) in cases {
            for (mut body, prefix) in [
                (
                    json!({"messages":[{"content":[part.clone(), part.clone()]}]}),
                    "/messages/0/content",
                ),
                (
                    json!({"input":[{"content":[part.clone(), part.clone()]}]}),
                    "/input/0/content",
                ),
                (
                    json!({"contents":[{"parts":[part.clone(), part.clone()]}]}),
                    "/contents/0/parts",
                ),
                (
                    json!({"type":"conversation.item.create","item":{"content":[part.clone(), part.clone()]}}),
                    "/item/content",
                ),
                (
                    json!({"type":"response.create","response":{"input":[part.clone(), part.clone()]}}),
                    "/response/input",
                ),
                (
                    json!({"type":"response.create","input":[part.clone(), part.clone()]}),
                    "/input",
                ),
                (
                    json!({"request":{"contents":[{"parts":[part.clone(), part.clone()]}]}}),
                    "/request/contents/0/parts",
                ),
            ] {
                let original = body.clone();
                let allowed = DelegatedFetchPolicy {
                    allow_remote_content: true,
                    ..Default::default()
                };
                assert!(sanitize(&mut body, &allowed).is_empty());
                assert_eq!(body, original);
                let audit = sanitize(&mut body, &deny_policy());
                assert_eq!(audit.len(), 2, "{original}");
                for (index, entry) in audit.iter().enumerate() {
                    assert_eq!(entry.field_path, format!("{prefix}/{index}{suffix}"));
                    assert_eq!(entry.capability, capability);
                    assert_eq!(entry.target_host.as_deref(), Some("media.example"));
                }
                assert_eq!(body.pointer(prefix), Some(&json!([])));
                assert!(sanitize(&mut body, &deny_policy()).is_empty());
            }
        }
    }

    #[test]
    fn preserves_inline_media_and_unrelated_url_shaped_data() {
        let original = json!({
            "messages": [{"content": [
                {"type":"image_url","image_url":{"url":"data:image/png;base64,YQ=="}},
                {"type":"input_image","image_url":"DATA:image/png;base64,YQ=="},
                {"type":"input_file","file_url":"data:application/pdf;base64,YQ=="},
                {"type":"input_file","file_data":"YQ=="},
                {"type":"image","source":{"type":"base64","data":"YQ=="}},
                {"type":"document","source":{"type":"base64","data":"YQ=="}},
                {"inlineData":{"mimeType":"image/png","data":"YQ=="}},
                {"inline_data":{"mime_type":"image/png","data":"YQ=="}},
                {"type":"text","text":"https://media.example/a"},
                null, 42, "https://media.example/a",
                {"type":"input_image","image_url":42},
                {"type":"image","source":null},
                {"type":"function_call","arguments":{"content":[{"type":"input_image","image_url":"https://media.example/a"}]},"content":[{"type":"input_image","image_url":"https://media.example/a"}]},
                {"type":"tool_use","input":{"contents":[{"fileData":{"fileUri":"https://media.example/a"}}]}},
                {"type":"function_call","arguments":"{\"image_url\":\"https://media.example/a\"}"}
            ]}],
            "metadata":{"input":[{"type":"input_image","image_url":"https://media.example/a"}]},
            "tools":[{"type":"function","parameters":{"contents":[{"fileData":{"fileUri":"https://media.example/a"}}]}}]
        });
        let mut body = original.clone();
        assert!(sanitize(&mut body, &deny_policy()).is_empty());
        assert_eq!(body, original);
    }

    #[test]
    #[expect(
        clippy::panic_in_result_fn,
        reason = "Assertions fail the test while serialization propagates fixture errors"
    )]
    fn nested_tool_result_media_is_stripped_and_invalid_uri_fails_closed(
    ) -> Result<(), serde_json::Error> {
        let mut body = json!({"messages":[{"content":[{"type":"tool_result","content":[
            {"type":"text","text":"keep"},
            {"type":"image","source":{"type":"url","url":"not a valid URL"}},
            {"type":"image","source":{"type":"url","url":"https://user:password@media.example/private?token=secret"}}
        ]}]}]});
        let audit = sanitize(&mut body, &deny_policy());
        assert_eq!(audit.len(), 2);
        assert_eq!(
            audit[0].field_path,
            "/messages/0/content/0/content/1/source/url"
        );
        assert_eq!(audit[0].target_host, None);
        assert_eq!(audit[1].target_host.as_deref(), Some("media.example"));
        assert!(!serde_json::to_string(&audit)?.contains("secret"));
        assert_eq!(
            body["messages"][0]["content"][0]["content"],
            json!([{"type":"text","text":"keep"}])
        );
        Ok(())
    }

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

        assert_eq!(
            body["input"][0]["content"].as_array().map(Vec::len),
            Some(1)
        );
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

        assert_eq!(body["tools"].as_array().map(Vec::len), Some(1));
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
    #[expect(
        clippy::panic_in_result_fn,
        reason = "Assertions verify unchanged wire bytes while serialization errors propagate"
    )]
    fn safe_control_body_is_unchanged() -> Result<(), serde_json::Error> {
        let original = json!({
            "model": "gpt-5",
            "input": [{
                "role": "user",
                "content": [{"type": "input_text", "text": "hello"}]
            }],
            "tools": [{"type": "function", "function": {"name": "local_tool"}}],
            "tool_choice": "auto"
        });
        let original_bytes = serde_json::to_vec(&original)?;
        let mut body = original;

        assert!(sanitize(&mut body, &deny_policy()).is_empty());
        assert_eq!(serde_json::to_vec(&body)?, original_bytes);
        Ok(())
    }

    #[test]
    fn realtime_session_update_strips_hosted_tools_and_preserves_allowlist() {
        let mut body = json!({
            "type": "session.update",
            "session": {
                "instructions": "be helpful",
                "tools": [
                    {"type": "function", "name": "local_fn"},
                    {"type": "web_search"},
                    {"type": "web_fetch"},
                    {"type": "computer_use"}
                ],
                "tool_choice": {"type": "web_search"}
            }
        });

        let stripped = sanitize(&mut body, &deny_policy());

        // function tool preserved, all hosted tools stripped
        assert_eq!(body["session"]["tools"].as_array().map(Vec::len), Some(1));
        assert_eq!(body["session"]["tools"][0]["type"], "function");
        // tool_choice referencing a removed tool is normalized
        assert_eq!(body["session"]["tool_choice"], "auto");
        assert!(stripped
            .iter()
            .any(|s| s.capability == "server_tool:web_search"));
        assert!(stripped
            .iter()
            .any(|s| s.capability == "server_tool:web_fetch"));
        assert!(stripped
            .iter()
            .any(|s| s.capability == "server_tool:computer_use"));
        // instructions untouched
        assert_eq!(body["session"]["instructions"], "be helpful");
    }

    #[test]
    fn realtime_session_update_keeps_allowlisted_hosted_tool() {
        let policy = DelegatedFetchPolicy {
            allow_remote_content: false,
            allowed_server_tools: vec!["web_search".to_string()],
        };
        let original = json!({
            "type": "session.update",
            "session": {
                "tools": [{"type": "web_search"}, {"type": "function", "name": "local"}],
                "tool_choice": {"type": "web_search"}
            }
        });
        let mut body = original.clone();

        assert!(sanitize(&mut body, &policy).is_empty());
        assert_eq!(body, original);
    }

    #[test]
    fn realtime_item_create_strips_remote_input_file() {
        let mut body = json!({
            "type": "conversation.item.create",
            "item": {
                "type": "message",
                "role": "user",
                "content": [
                    {"type": "input_text", "text": "summarize this"},
                    {"type": "input_file", "file_url": "https://realtime.example/private.pdf"}
                ]
            }
        });

        let stripped = sanitize(&mut body, &deny_policy());

        assert_eq!(body["item"]["content"].as_array().map(Vec::len), Some(1));
        assert_eq!(body["item"]["content"][0]["type"], "input_text");
        assert_eq!(stripped.len(), 1);
        assert_eq!(stripped[0].field_path, "/item/content/1/file_url");
        assert_eq!(stripped[0].target_host.as_deref(), Some("realtime.example"));
    }

    #[test]
    #[expect(
        clippy::panic_in_result_fn,
        reason = "Assertions verify unchanged wire bytes while serialization errors propagate"
    )]
    fn realtime_safe_control_body_is_unchanged() -> Result<(), serde_json::Error> {
        let original = json!({
            "type": "session.update",
            "session": {
                "instructions": "no tools here",
                "tools": [{"type": "function", "name": "local_fn"}],
                "tool_choice": "auto"
            }
        });
        let original_bytes = serde_json::to_vec(&original)?;
        let mut body = original;

        assert!(sanitize(&mut body, &deny_policy()).is_empty());
        assert_eq!(serde_json::to_vec(&body)?, original_bytes);
        Ok(())
    }
}

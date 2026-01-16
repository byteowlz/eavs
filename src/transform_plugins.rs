//! Transform plugin system for modifying requests/responses.
//!
//! Transform plugins are external scripts that can modify request/response JSON
//! before/after it's sent to upstream providers. This is useful for:
//! - Provider-specific quirks (e.g., Anthropic OAuth token restrictions)
//! - Custom header injection
//! - Request/response logging and modification
//!
//! ## Plugin Protocol
//!
//! Plugins receive a JSON object on stdin with the following structure:
//! ```json
//! {
//!   "type": "request" | "response",
//!   "provider": "anthropic" | "openai" | ...,
//!   "is_oauth": true | false,
//!   "headers": { "header-name": "value", ... },
//!   "body": { ... request/response body ... }
//! }
//! ```
//!
//! Plugins should output a JSON object on stdout with the modified data:
//! ```json
//! {
//!   "headers": { "header-name": "value", ... },
//!   "body": { ... modified body ... }
//! }
//! ```
//!
//! If the plugin outputs nothing or invalid JSON, the original data is used.
//!
//! ## Configuration
//!
//! ```toml
//! [transform]
//! enabled = true
//!
//! [[transform.plugins]]
//! name = "anthropic-oauth-fix"
//! command = "my-transform-script"
//! args = ["--mode", "transform"]
//! env = { MY_API_KEY = "env:MY_API_KEY" }
//! providers = ["anthropic"]  # Only run for Anthropic
//! oauth_only = true          # Only run for OAuth requests
//! timeout_ms = 5000          # 5 second timeout
//! ```

use crate::config::TransformPluginConfig;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use std::process::Stdio;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::Command;

/// Input sent to transform plugins.
#[derive(Debug, Clone, Serialize)]
pub struct TransformInput {
    /// Type of transform: "request" or "response"
    #[serde(rename = "type")]
    pub transform_type: String,
    /// Provider name (e.g., "anthropic", "openai")
    pub provider: String,
    /// Whether this is an OAuth request
    pub is_oauth: bool,
    /// HTTP headers as key-value pairs
    pub headers: HashMap<String, String>,
    /// Request/response body
    pub body: Value,
}

/// Output from transform plugins.
#[derive(Debug, Clone, Deserialize, Default)]
pub struct TransformOutput {
    /// Modified headers (optional)
    #[serde(default)]
    pub headers: Option<HashMap<String, String>>,
    /// Modified body (optional)
    #[serde(default)]
    pub body: Option<Value>,
}

/// Run transform plugins on a request.
///
/// Returns the modified headers and body, or the original if no plugins match
/// or all plugins fail.
pub async fn run_request_transforms(
    plugins: &[TransformPluginConfig],
    provider: &str,
    is_oauth: bool,
    headers: HashMap<String, String>,
    body: Value,
) -> (HashMap<String, String>, Value) {
    let input = TransformInput {
        transform_type: "request".to_string(),
        provider: provider.to_string(),
        is_oauth,
        headers: headers.clone(),
        body: body.clone(),
    };

    run_transforms(plugins, provider, is_oauth, input, headers, body).await
}

/// Run transform plugins on a response.
///
/// Returns the modified headers and body, or the original if no plugins match
/// or all plugins fail.
#[allow(dead_code)]
pub async fn run_response_transforms(
    plugins: &[TransformPluginConfig],
    provider: &str,
    is_oauth: bool,
    headers: HashMap<String, String>,
    body: Value,
) -> (HashMap<String, String>, Value) {
    let input = TransformInput {
        transform_type: "response".to_string(),
        provider: provider.to_string(),
        is_oauth,
        headers: headers.clone(),
        body: body.clone(),
    };

    run_transforms(plugins, provider, is_oauth, input, headers, body).await
}

async fn run_transforms(
    plugins: &[TransformPluginConfig],
    provider: &str,
    is_oauth: bool,
    mut input: TransformInput,
    mut headers: HashMap<String, String>,
    mut body: Value,
) -> (HashMap<String, String>, Value) {
    for plugin in plugins {
        // Check provider filter
        if !plugin.providers.is_empty()
            && !plugin
                .providers
                .iter()
                .any(|p| p.eq_ignore_ascii_case(provider))
        {
            continue;
        }

        // Check OAuth filter
        if plugin.oauth_only && !is_oauth {
            continue;
        }

        match run_single_plugin(plugin, &input).await {
            Ok(output) => {
                // Apply modifications
                if let Some(new_headers) = output.headers {
                    headers = new_headers;
                    input.headers = headers.clone();
                }
                if let Some(new_body) = output.body {
                    body = new_body;
                    input.body = body.clone();
                }
                tracing::debug!(
                    plugin = %plugin.name,
                    "Transform plugin applied successfully"
                );
            }
            Err(e) => {
                tracing::warn!(
                    plugin = %plugin.name,
                    error = %e,
                    "Transform plugin failed, continuing with original data"
                );
            }
        }
    }

    (headers, body)
}

async fn run_single_plugin(
    plugin: &TransformPluginConfig,
    input: &TransformInput,
) -> Result<TransformOutput, String> {
    let mut cmd = Command::new(&plugin.command);
    cmd.args(&plugin.args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    // Set environment variables
    for (key, value) in &plugin.env {
        let resolved = resolve_env_value(value);
        cmd.env(key, resolved);
    }

    let mut child = cmd
        .spawn()
        .map_err(|e| format!("Failed to spawn plugin: {}", e))?;

    // Write input to stdin
    let stdin = child.stdin.take().ok_or("Failed to get stdin")?;
    let input_json =
        serde_json::to_string(input).map_err(|e| format!("Failed to serialize input: {}", e))?;

    let write_task = async move {
        let mut stdin = stdin;
        stdin
            .write_all(input_json.as_bytes())
            .await
            .map_err(|e| format!("Failed to write to stdin: {}", e))?;
        stdin
            .shutdown()
            .await
            .map_err(|e| format!("Failed to close stdin: {}", e))?;
        Ok::<_, String>(())
    };

    // Read output from stdout
    let stdout = child.stdout.take().ok_or("Failed to get stdout")?;
    let read_task = async move {
        let reader = BufReader::new(stdout);
        let mut lines = reader.lines();
        let mut output = String::new();
        while let Ok(Some(line)) = lines.next_line().await {
            output.push_str(&line);
            output.push('\n');
        }
        Ok::<_, String>(output)
    };

    // Run with timeout
    let timeout = Duration::from_millis(plugin.timeout_ms);
    let result = tokio::time::timeout(timeout, async {
        let (write_result, read_result, wait_result) =
            tokio::join!(write_task, read_task, child.wait());

        write_result?;
        let output = read_result?;
        let status = wait_result.map_err(|e| format!("Failed to wait for process: {}", e))?;

        if !status.success() {
            return Err(format!("Plugin exited with status: {}", status));
        }

        Ok(output)
    })
    .await
    .map_err(|_| format!("Plugin timed out after {}ms", plugin.timeout_ms))??;

    // Parse output
    if result.trim().is_empty() {
        return Ok(TransformOutput::default());
    }

    serde_json::from_str(&result).map_err(|e| format!("Failed to parse plugin output: {}", e))
}

fn resolve_env_value(value: &str) -> String {
    if let Some(var) = value.strip_prefix("env:") {
        std::env::var(var).unwrap_or_default()
    } else {
        value.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_transform_input_serialization() {
        let input = TransformInput {
            transform_type: "request".to_string(),
            provider: "anthropic".to_string(),
            is_oauth: true,
            headers: HashMap::from([("Content-Type".to_string(), "application/json".to_string())]),
            body: serde_json::json!({"model": "claude-3-5-sonnet-20241022"}),
        };

        let json = serde_json::to_string(&input).unwrap();
        assert!(json.contains("\"type\":\"request\""));
        assert!(json.contains("\"provider\":\"anthropic\""));
        assert!(json.contains("\"is_oauth\":true"));
    }

    #[test]
    fn test_transform_output_deserialization() {
        let json = r#"{"headers":{"X-Custom":"value"},"body":{"modified":true}}"#;
        let output: TransformOutput = serde_json::from_str(json).unwrap();
        assert!(output.headers.is_some());
        assert!(output.body.is_some());
    }

    #[test]
    fn test_transform_output_empty() {
        let json = "{}";
        let output: TransformOutput = serde_json::from_str(json).unwrap();
        assert!(output.headers.is_none());
        assert!(output.body.is_none());
    }

    #[test]
    fn test_resolve_env_value() {
        std::env::set_var("EAVS_TEST_TRANSFORM_ENV", "test_value");
        assert_eq!(
            resolve_env_value("env:EAVS_TEST_TRANSFORM_ENV"),
            "test_value"
        );
        assert_eq!(resolve_env_value("literal_value"), "literal_value");
        std::env::remove_var("EAVS_TEST_TRANSFORM_ENV");
    }
}

//! Export configured providers and models via TypeScript adapters.
//!
//! Each harness (Pi, OpenCode, etc.) has a TypeScript adapter in
//! `adapters/<name>/adapter.ts` that transforms eavs provider data
//! into the correct format. This module discovers and invokes them.

use std::path::PathBuf;
use std::process::Command;

use anyhow::{bail, Context, Result};
use serde_json::Value;

use crate::api::ProviderDetail;

/// Find the adapters directory.
///
/// Search order:
/// 1. `$EAVS_ADAPTERS_DIR` env var
/// 2. Next to the eavs binary: `<binary_dir>/adapters/`
/// 3. `$XDG_DATA_HOME/eavs/adapters/` (or `~/.local/share/eavs/adapters/`)
/// 4. Source tree (development: `CARGO_MANIFEST_DIR/adapters/`)
pub fn adapters_dir() -> Result<PathBuf> {
    // 1. Env var override
    if let Ok(dir) = std::env::var("EAVS_ADAPTERS_DIR") {
        let p = PathBuf::from(dir);
        if p.is_dir() {
            return Ok(p);
        }
    }

    // 2. Next to the binary (release tarball layout)
    if let Ok(exe) = std::env::current_exe() {
        if let Some(parent) = exe.parent() {
            let sibling = parent.join("adapters");
            if sibling.is_dir() {
                return Ok(sibling);
            }
        }
    }

    // 3. XDG data dir
    let data_home = crate::paths::data_dir().unwrap_or_else(|_| PathBuf::from("."));
    let xdg_adapters = data_home.join("eavs/adapters");
    if xdg_adapters.is_dir() {
        return Ok(xdg_adapters);
    }

    // 4. Source tree (development)
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let dev_adapters = manifest.join("adapters");
    if dev_adapters.is_dir() {
        return Ok(dev_adapters);
    }

    bail!(
        "Could not find adapters directory.\n\
         Searched:\n\
         - $EAVS_ADAPTERS_DIR\n\
         - next to eavs binary\n\
         - {}\n\
         - {}\n\
         \n\
         Install adapters: place them next to the eavs binary or in the XDG data dir.",
        xdg_adapters.display(),
        dev_adapters.display(),
    )
}

/// List available adapter names by scanning the adapters directory.
pub fn list_adapters() -> Result<Vec<String>> {
    let dir = adapters_dir()?;
    let mut names = Vec::new();

    for entry in std::fs::read_dir(&dir).context("reading adapters directory")? {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() {
            let name = entry.file_name().to_string_lossy().to_string();
            // Skip the shared types directory
            if name == "types" || name == "node_modules" {
                continue;
            }
            // Check that adapter.ts exists
            if path.join("adapter.ts").exists() {
                names.push(name);
            }
        }
    }

    names.sort();
    Ok(names)
}

/// Build a merge request payload (updates existing config).
fn build_merge_request(
    providers: &[ProviderDetail],
    base_url: &str,
    api_key: &str,
    existing: &str,
) -> Value {
    let mut req = build_provider_data(providers);
    req["method"] = serde_json::json!("merge");
    req["base_url"] = serde_json::json!(base_url);
    req["api_key"] = serde_json::json!(api_key);
    req["existing"] = serde_json::json!(existing);
    req
}

/// Build a full export request payload.
fn build_request(providers: &[ProviderDetail], base_url: &str, api_key: &str) -> Value {
    let mut req = build_provider_data(providers);
    req["method"] = serde_json::json!("export");
    req["base_url"] = serde_json::json!(base_url);
    req["api_key"] = serde_json::json!(api_key);
    req
}

/// Build the shared provider data portion of a request.
fn build_provider_data(providers: &[ProviderDetail]) -> Value {
    let provider_values: Vec<Value> = providers
        .iter()
        .map(|p| {
            let models: Vec<Value> = p
                .models
                .iter()
                .map(|m| {
                    let mut model = serde_json::json!({
                        "id": m.id,
                        "name": if m.name.is_empty() { &m.id } else { &m.name },
                        "reasoning": m.reasoning,
                        "input": if m.input.is_empty() { vec!["text".to_string()] } else { m.input.clone() },
                        "context_window": m.context_window,
                        "max_tokens": m.max_tokens,
                        "cost": {
                            "input": m.cost.input,
                            "output": m.cost.output,
                            "cache_read": m.cost.cache_read,
                            "cache_write": m.cost.cache_write,
                        }
                    });
                    if !m.compat.is_empty() {
                        model["compat"] = serde_json::json!(m.compat);
                    }
                    model
                })
                .collect();

            let mut provider = serde_json::json!({
                "name": p.name,
                "type": p.type_,
                "pi_api": p.pi_api,
                "oauth": p.oauth,
                "has_api_key": p.has_api_key,
                "models": models,
            });
            if let Some(ref compat) = p.compat {
                provider["compat"] = compat.clone();
            }
            provider
        })
        .collect();

    serde_json::json!({
        "providers": provider_values,
    })
}

/// Run an adapter and return its JSON output.
///
/// If `existing` is provided, the adapter receives a "merge" request
/// and should update the existing config. Otherwise it does a full "export".
pub fn run_adapter(
    adapter_name: &str,
    providers: &[ProviderDetail],
    base_url: &str,
    api_key: &str,
    existing: Option<&str>,
) -> Result<Value> {
    let dir = adapters_dir()?;
    let adapter_path = dir.join(adapter_name).join("adapter.ts");

    if !adapter_path.exists() {
        let available = list_adapters().unwrap_or_default();
        bail!(
            "Adapter '{}' not found at {}\nAvailable adapters: {}",
            adapter_name,
            adapter_path.display(),
            if available.is_empty() {
                "(none)".to_string()
            } else {
                available.join(", ")
            }
        );
    }

    let request = if let Some(existing_content) = existing {
        build_merge_request(providers, base_url, api_key, existing_content)
    } else {
        build_request(providers, base_url, api_key)
    };
    let request_json = serde_json::to_string(&request).context("serializing adapter request")?;

    // Try bun first (faster startup), fall back to npx tsx
    let (cmd, args) = if which("bun") {
        (
            "bun",
            vec!["run".to_string(), adapter_path.display().to_string()],
        )
    } else if which("npx") {
        (
            "npx",
            vec!["tsx".to_string(), adapter_path.display().to_string()],
        )
    } else {
        bail!(
            "Neither 'bun' nor 'npx' found. Install bun (recommended) or Node.js to run adapters."
        )
    };

    let output = Command::new(cmd)
        .args(&args)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .and_then(|mut child| {
            use std::io::Write;
            if let Some(ref mut stdin) = child.stdin {
                stdin.write_all(request_json.as_bytes())?;
            }
            // Drop stdin to signal EOF
            child.stdin.take();
            child.wait_with_output()
        })
        .with_context(|| format!("running adapter: {} {}", cmd, args.join(" ")))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!(
            "Adapter '{}' failed (exit {}): {}",
            adapter_name,
            output.status.code().unwrap_or(-1),
            stderr.trim()
        );
    }

    let stdout = String::from_utf8(output.stdout).context("adapter output is not valid UTF-8")?;

    serde_json::from_str(&stdout)
        .with_context(|| format!("adapter '{}' returned invalid JSON", adapter_name))
}

/// Get adapter info (metadata).
pub fn adapter_info(adapter_name: &str) -> Result<Value> {
    let dir = adapters_dir()?;
    let adapter_path = dir.join(adapter_name).join("adapter.ts");

    if !adapter_path.exists() {
        bail!("Adapter '{}' not found", adapter_name);
    }

    let request = serde_json::json!({"method": "info"});
    let request_json = serde_json::to_string(&request)?;

    let (cmd, args) = if which("bun") {
        (
            "bun",
            vec!["run".to_string(), adapter_path.display().to_string()],
        )
    } else if which("npx") {
        (
            "npx",
            vec!["tsx".to_string(), adapter_path.display().to_string()],
        )
    } else {
        bail!("Neither 'bun' nor 'npx' found.")
    };

    let output = Command::new(cmd)
        .args(&args)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .and_then(|mut child| {
            use std::io::Write;
            if let Some(ref mut stdin) = child.stdin {
                stdin.write_all(request_json.as_bytes())?;
            }
            child.stdin.take();
            child.wait_with_output()
        })?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("Adapter info failed: {}", stderr.trim());
    }

    let stdout = String::from_utf8(output.stdout)?;
    serde_json::from_str(&stdout).context("parsing adapter info response")
}

fn which(name: &str) -> bool {
    Command::new("which")
        .arg(name)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

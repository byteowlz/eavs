//! Reasoning-effort normalization for self-hosted and partially compatible
//! upstreams.
//!
//! Clients (e.g. Pi) send `reasoning_effort` (Chat Completions) or
//! `reasoning.effort` (Responses) from a fixed ladder of levels, but some
//! servers accept only a subset (vLLM builds that reject `minimal`/`high`).
//! The gateway rewrites an unsupported value to the nearest supported one,
//! preferring the next higher level so quality is not silently reduced.
//!
//! Supported values come from, in precedence order:
//! 1. `reasoning_efforts` declared on the model in config (authoritative);
//! 2. values learned from an upstream 400 that names the supported set,
//!    persisted to `$XDG_STATE_HOME/eavs/reasoning-efforts.json`.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::RwLock;

use serde_json::Value;

/// Known levels from lowest to highest. `off` is an alias of `none`.
const LADDER: &[&str] = &["none", "minimal", "low", "medium", "high", "xhigh"];

fn rank(level: &str) -> Option<usize> {
    let level = level.trim().to_ascii_lowercase();
    let level = if level == "off" {
        "none"
    } else {
        level.as_str()
    };
    LADDER.iter().position(|known| *known == level)
}

/// The supported value to send instead of `requested`, or `None` when the
/// request can be forwarded unchanged (already supported, unknown level or
/// nothing usable declared).
pub fn choose(requested: &str, supported: &[String]) -> Option<String> {
    if supported.is_empty()
        || supported
            .iter()
            .any(|value| value.eq_ignore_ascii_case(requested.trim()))
    {
        return None;
    }
    let wanted = rank(requested)?;
    // Equal rank covers the off/none alias.
    let mut ranked: Vec<(usize, &String)> = supported
        .iter()
        .filter_map(|value| rank(value).map(|r| (r, value)))
        .collect();
    ranked.sort_by_key(|(r, _)| *r);
    ranked
        .iter()
        .find(|(r, _)| *r >= wanted)
        .or_else(|| ranked.last())
        .map(|(_, value)| (*value).clone())
}

/// Phrases that introduce the accepted set in upstream error messages, e.g.
/// vLLM "Supported types are xhigh (default), medium, and low." or
/// "use one of: off, low, medium, xhigh".
const LIST_MARKERS: &[&str] = &[
    "supported types are",
    "supported values are",
    "supported values:",
    "supported:",
    "must be one of",
    "expected one of",
    "use one of",
    "one of:",
];

/// Parse the accepted reasoning-effort values from an upstream error body.
/// Only messages that mention reasoning effort and name an accepted set are
/// used; anything else returns `None` and the original error passes through.
pub fn parse_supported(error_body: &str) -> Option<Vec<String>> {
    let message = serde_json::from_str::<Value>(error_body)
        .ok()
        .and_then(|json| {
            json.pointer("/error/message")
                .or_else(|| json.get("message"))
                .or_else(|| json.get("detail"))
                .and_then(Value::as_str)
                .map(str::to_owned)
        })
        .unwrap_or_else(|| error_body.to_owned());
    let lower = message.to_ascii_lowercase();
    if !(lower.contains("reasoning effort") || lower.contains("reasoning_effort")) {
        return None;
    }
    let start = LIST_MARKERS
        .iter()
        .filter_map(|marker| lower.rfind(marker).map(|at| at + marker.len()))
        .max()?;
    let mut values: Vec<String> = Vec::new();
    for word in lower[start..].split(|c: char| !c.is_ascii_alphanumeric() && c != '_') {
        if rank(word).is_some() && !values.iter().any(|v| v == word) {
            values.push(word.to_owned());
        }
    }
    (!values.is_empty()).then_some(values)
}

/// Where a request carries its reasoning effort.
fn effort_slot(body: &mut Value) -> Option<&mut Value> {
    if body.get("reasoning_effort").is_some() {
        return body.get_mut("reasoning_effort");
    }
    body.get_mut("reasoning")?.get_mut("effort")
}

/// The reasoning effort in a JSON request body, if any.
pub fn requested(body: &Value) -> Option<String> {
    body.get("reasoning_effort")
        .or_else(|| body.pointer("/reasoning/effort"))
        .and_then(Value::as_str)
        .map(str::to_owned)
}

/// Rewrite the effort in `body` for `supported`. Returns `(from, to)` when the
/// body changed.
pub fn rewrite(body: &mut Value, supported: &[String]) -> Option<(String, String)> {
    let from = requested(body)?;
    let to = choose(&from, supported)?;
    *effort_slot(body)? = Value::String(to.clone());
    Some((from, to))
}

/// Rewrite raw JSON request bytes; `None` when unchanged or not JSON.
pub fn rewrite_bytes(body: &[u8], supported: &[String]) -> Option<(Vec<u8>, String, String)> {
    let mut json: Value = serde_json::from_slice(body).ok()?;
    let (from, to) = rewrite(&mut json, supported)?;
    Some((serde_json::to_vec(&json).ok()?, from, to))
}

/// Provider -> model -> learned value.
type LearnedMap<T> = BTreeMap<String, BTreeMap<String, T>>;

/// Facts learned from upstream errors (accepted reasoning efforts, adaptive
/// thinking), keyed by provider then model. Process-wide and persisted
/// best-effort; config declarations always win.
pub struct Learned<T> {
    path: Option<PathBuf>,
    entries: RwLock<LearnedMap<T>>,
}

/// Accepted reasoning efforts per model.
pub type LearnedEfforts = Learned<Vec<String>>;
/// Models that require Anthropic adaptive thinking.
pub type LearnedAdaptive = Learned<bool>;

impl<T> Learned<T>
where
    T: Clone + PartialEq + Default + serde::Serialize + serde::de::DeserializeOwned,
{
    pub fn load(path: Option<PathBuf>) -> Self {
        let entries = path
            .as_ref()
            .and_then(|path| std::fs::read(path).ok())
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default();
        Self {
            path,
            entries: RwLock::new(entries),
        }
    }

    /// `$XDG_STATE_HOME/eavs/<file>`, or `None` in tests.
    pub fn state_file(file: &str) -> Option<PathBuf> {
        if cfg!(test) {
            return None;
        }
        Some(crate::paths::state_dir().ok()?.join("eavs").join(file))
    }

    pub fn get(&self, provider: &str, model: &str) -> Option<T> {
        self.entries.read().ok()?.get(provider)?.get(model).cloned()
    }

    pub fn learn(&self, provider: &str, model: &str, value: T) {
        let snapshot = {
            let Ok(mut entries) = self.entries.write() else {
                return;
            };
            let slot = entries
                .entry(provider.to_owned())
                .or_default()
                .entry(model.to_owned())
                .or_default();
            if *slot == value {
                return;
            }
            *slot = value;
            entries.clone()
        };
        let Some(path) = &self.path else { return };
        let result = path
            .parent()
            .map_or(Ok(()), std::fs::create_dir_all)
            .and_then(|()| {
                let tmp = path.with_extension("json.tmp");
                std::fs::write(
                    &tmp,
                    serde_json::to_vec_pretty(&snapshot).unwrap_or_default(),
                )?;
                std::fs::rename(&tmp, path)
            });
        if let Err(error) = result {
            tracing::warn!(%error, path = %path.display(), "Could not persist learned upstream behavior");
        }
    }
}

/// Whether an Anthropic error says the model requires adaptive thinking, e.g.
/// `"thinking.type.enabled" is not supported for this model. Use
/// "thinking.type.adaptive" ...`.
pub fn requires_adaptive(error_body: &str) -> bool {
    let lower = error_body.to_ascii_lowercase();
    lower.contains("thinking.type.enabled") && lower.contains("thinking.type.adaptive")
}

/// Whether an Anthropic Messages body uses budget-based (`enabled`) thinking.
pub fn uses_enabled_thinking(body: &Value) -> bool {
    body.pointer("/thinking/type").and_then(Value::as_str) == Some("enabled")
}

/// Anthropic effort for a thinking token budget (Pi's budgets: 1024 minimal,
/// ~2-4k low, ~8-10k medium, ~16k+ high).
const fn effort_for_budget(budget: Option<u64>) -> &'static str {
    match budget {
        Some(0..=4_096) => "low",
        Some(4_097..=12_288) => "medium",
        _ => "high",
    }
}

/// Convert `thinking: {type: "enabled", budget_tokens}` to adaptive thinking
/// with `output_config.effort` (kept if the client already set one).
/// Returns `(from, to)` when the body changed.
pub fn to_adaptive(body: &mut Value) -> Option<(String, String)> {
    if !uses_enabled_thinking(body) {
        return None;
    }
    let budget = body
        .pointer("/thinking/budget_tokens")
        .and_then(Value::as_u64);
    let display = body.pointer("/thinking/display").cloned();
    let object = body.as_object_mut()?;
    let mut thinking = serde_json::Map::new();
    thinking.insert("type".into(), "adaptive".into());
    if let Some(display) = display {
        thinking.insert("display".into(), display);
    }
    object.insert("thinking".into(), Value::Object(thinking));
    let config = object
        .entry("output_config")
        .or_insert_with(|| Value::Object(serde_json::Map::new()))
        .as_object_mut()?;
    let effort = config
        .entry("effort")
        .or_insert_with(|| effort_for_budget(budget).into())
        .as_str()
        .unwrap_or_default()
        .to_owned();
    Some(("enabled".into(), format!("adaptive({effort})")))
}

/// `to_adaptive` over raw JSON bytes; `None` when unchanged or not JSON.
pub fn to_adaptive_bytes(body: &[u8]) -> Option<(Vec<u8>, String, String)> {
    let mut json: Value = serde_json::from_slice(body).ok()?;
    let (from, to) = to_adaptive(&mut json)?;
    Some((serde_json::to_vec(&json).ok()?, from, to))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn list(values: &[&str]) -> Vec<String> {
        values.iter().map(|v| (*v).to_owned()).collect()
    }

    #[test]
    fn choose_prefers_next_higher_then_highest_below() {
        let vllm = list(&["low", "medium", "xhigh"]);
        assert_eq!(choose("minimal", &vllm).as_deref(), Some("low"));
        assert_eq!(choose("high", &vllm).as_deref(), Some("xhigh"));
        assert_eq!(choose("medium", &vllm), None);
        assert_eq!(choose("none", &vllm).as_deref(), Some("low"));
        let capped = list(&["low", "medium"]);
        assert_eq!(choose("xhigh", &capped).as_deref(), Some("medium"));
    }

    #[test]
    fn choose_handles_off_alias_case_and_unknowns() {
        let sushi = list(&["off", "low", "medium", "xhigh"]);
        assert_eq!(choose("none", &sushi).as_deref(), Some("off"));
        assert_eq!(choose("HIGH", &sushi).as_deref(), Some("xhigh"));
        assert_eq!(choose("Low", &sushi), None);
        assert_eq!(choose("turbo", &sushi), None, "unknown levels pass through");
        assert_eq!(choose("high", &[]), None);
    }

    #[test]
    fn parses_vllm_and_sushi_errors() {
        let vllm = json!({"error": {"code": "BadRequestError", "message": "Unexpected reasoning effort minimal. Supported types are xhigh (default), medium, and low.", "type": "invalid_request_error"}}).to_string();
        assert_eq!(
            parse_supported(&vllm),
            Some(list(&["xhigh", "medium", "low"]))
        );
        let sushi = json!({"error": {"message": "reasoning effort 'high' is not supported by beamster/Qwen3.8-Flash-Next-Sushi-4bpw; use one of: off, low, medium, xhigh", "type": "invalid_request_error"}}).to_string();
        assert_eq!(
            parse_supported(&sushi),
            Some(list(&["off", "low", "medium", "xhigh"]))
        );
    }

    #[test]
    fn ignores_unrelated_or_listless_errors() {
        let other =
            json!({"error": {"message": "max_tokens must be one of: low, high"}}).to_string();
        assert_eq!(parse_supported(&other), None, "not about reasoning effort");
        let no_list = json!({"error": {"message": "invalid reasoning effort"}}).to_string();
        assert_eq!(parse_supported(&no_list), None);
        assert_eq!(parse_supported("not json, nothing here"), None);
    }

    #[test]
    fn rewrites_chat_and_responses_shapes_only_when_needed() {
        let supported = list(&["low", "medium", "xhigh"]);
        let mut chat = json!({"model": "m", "reasoning_effort": "high"});
        assert_eq!(
            rewrite(&mut chat, &supported),
            Some(("high".into(), "xhigh".into()))
        );
        assert_eq!(chat["reasoning_effort"], "xhigh");
        let mut responses =
            json!({"model": "m", "reasoning": {"effort": "minimal", "summary": "auto"}});
        assert_eq!(
            rewrite(&mut responses, &supported),
            Some(("minimal".into(), "low".into()))
        );
        assert_eq!(responses["reasoning"]["effort"], "low");
        assert_eq!(responses["reasoning"]["summary"], "auto");
        let mut fine = json!({"reasoning_effort": "medium"});
        assert_eq!(rewrite(&mut fine, &supported), None);
        let mut absent = json!({"model": "m"});
        assert_eq!(rewrite(&mut absent, &supported), None);
        assert!(absent.get("reasoning_effort").is_none());
    }

    #[test]
    #[expect(
        clippy::panic_in_result_fn,
        reason = "assertions are intentional in tests"
    )]
    fn learned_values_persist_across_loads() -> anyhow::Result<()> {
        let dir = tempfile::tempdir()?;
        let path = dir.path().join("eavs").join("reasoning-efforts.json");
        let store = LearnedEfforts::load(Some(path.clone()));
        assert_eq!(store.get("rtx", "qwen"), None);
        store.learn("rtx", "qwen", list(&["low", "medium", "xhigh"]));
        let reloaded = LearnedEfforts::load(Some(path));
        assert_eq!(
            reloaded.get("rtx", "qwen"),
            Some(list(&["low", "medium", "xhigh"]))
        );
        assert_eq!(reloaded.get("rtx", "other"), None);
        Ok(())
    }

    #[test]
    fn detects_anthropic_adaptive_requirement() {
        let body = json!({"type": "error", "error": {"type": "invalid_request_error", "message": "\"thinking.type.enabled\" is not supported for this model. Use \"thinking.type.adaptive\" and \"output_config.effort\" to control thinking behavior."}}).to_string();
        assert!(requires_adaptive(&body));
        assert!(!requires_adaptive(
            r#"{"error":{"message":"thinking budget too large"}}"#
        ));
    }

    #[test]
    fn converts_enabled_thinking_to_adaptive_with_effort() {
        let mut body = json!({"model": "claude", "max_tokens": 32000, "thinking": {"type": "enabled", "budget_tokens": 16384, "display": "summarized"}});
        assert_eq!(
            to_adaptive(&mut body),
            Some(("enabled".into(), "adaptive(high)".into()))
        );
        assert_eq!(
            body["thinking"],
            json!({"type": "adaptive", "display": "summarized"})
        );
        assert_eq!(body["output_config"]["effort"], "high");
        let mut low = json!({"thinking": {"type": "enabled", "budget_tokens": 2048}});
        assert_eq!(
            to_adaptive(&mut low).map(|(_, to)| to).as_deref(),
            Some("adaptive(low)")
        );
        let mut kept = json!({"thinking": {"type": "enabled", "budget_tokens": 2048}, "output_config": {"effort": "max", "other": 1}});
        to_adaptive(&mut kept);
        assert_eq!(kept["output_config"], json!({"effort": "max", "other": 1}));
        let mut disabled = json!({"thinking": {"type": "disabled"}});
        assert_eq!(to_adaptive(&mut disabled), None);
        let mut adaptive = json!({"thinking": {"type": "adaptive"}});
        assert_eq!(to_adaptive(&mut adaptive), None);
    }
}

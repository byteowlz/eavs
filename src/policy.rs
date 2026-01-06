use glob_match::glob_match;
use serde::Deserialize;
use serde_json::Value;

#[derive(Debug, Clone)]
pub struct PolicyViolation {
    pub message: String,
}

#[derive(Debug, Deserialize, Clone, Default)]
#[serde(default)]
pub struct PolicyConfig {
    pub enabled: bool,
    pub rules: Vec<PolicyRule>,
}

#[derive(Debug, Deserialize, Clone)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum PolicyRule {
    /// Deny a request outright.
    Deny {
        #[serde(default)]
        provider: Option<String>,
        #[serde(default)]
        model: Option<String>,
        #[serde(default)]
        path: Option<String>,
        #[serde(default)]
        reason: Option<String>,
    },
    /// Rewrite the request model.
    RewriteModel {
        #[serde(default)]
        provider: Option<String>,
        #[serde(default)]
        model: Option<String>,
        to: String,
    },
    /// Filter tool definitions (OpenAI `tools`) by name.
    FilterTools {
        #[serde(default)]
        provider: Option<String>,
        #[serde(default)]
        model: Option<String>,
        #[serde(default)]
        allow: Option<Vec<String>>,
        #[serde(default)]
        deny: Option<Vec<String>>,
    },
}

impl PolicyConfig {
    pub fn apply(&self, provider: &str, path: &str, body: &mut Value) -> Result<(), PolicyViolation> {
        if !self.enabled {
            return Ok(());
        }

        let mut model = body
            .get("model")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string();

        for rule in &self.rules {
            match rule {
                PolicyRule::Deny {
                    provider: p,
                    model: m,
                    path: rp,
                    reason,
                } => {
                    if !matches_opt(p.as_deref(), provider) {
                        continue;
                    }
                    if !matches_opt(m.as_deref(), &model) {
                        continue;
                    }
                    if !matches_opt(rp.as_deref(), path) {
                        continue;
                    }

                    return Err(PolicyViolation {
                        message: reason
                            .clone()
                            .unwrap_or_else(|| "Request denied by policy".to_string()),
                    });
                }
                PolicyRule::RewriteModel { provider: p, model: m, to } => {
                    if !matches_opt(p.as_deref(), provider) {
                        continue;
                    }
                    if !matches_opt(m.as_deref(), &model) {
                        continue;
                    }
                    body["model"] = Value::String(to.clone());
                    model = to.clone();
                }
                PolicyRule::FilterTools {
                    provider: p,
                    model: m,
                    allow,
                    deny,
                } => {
                    if !matches_opt(p.as_deref(), provider) {
                        continue;
                    }
                    if !matches_opt(m.as_deref(), &model) {
                        continue;
                    }
                    filter_tools(body, allow.as_deref(), deny.as_deref());
                }
            }
        }

        Ok(())
    }
}

fn matches_opt(pattern: Option<&str>, value: &str) -> bool {
    match pattern {
        None => true,
        Some(p) => glob_match(p, value),
    }
}

fn filter_tools(body: &mut Value, allow: Option<&[String]>, deny: Option<&[String]>) {
    let Some(tools) = body.get_mut("tools").and_then(|v| v.as_array_mut()) else {
        return;
    };

    tools.retain(|tool| {
        let name = tool
            .get("function")
            .and_then(|f| f.get("name"))
            .and_then(|n| n.as_str())
            .unwrap_or_default();

        if let Some(allow) = allow {
            if !allow.iter().any(|p| glob_match(p, name)) {
                return false;
            }
        }

        if let Some(deny) = deny {
            if deny.iter().any(|p| glob_match(p, name)) {
                return false;
            }
        }

        true
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn policy_deny_by_model() {
        let policy = PolicyConfig {
            enabled: true,
            rules: vec![PolicyRule::Deny {
                provider: None,
                model: Some("gpt-4*".to_string()),
                path: None,
                reason: Some("nope".to_string()),
            }],
        };

        let mut body = json!({"model":"gpt-4o-mini","messages":[]});
        let err = policy.apply("default", "/v1/chat/completions", &mut body).unwrap_err();
        assert_eq!(err.message, "nope");
    }

    #[test]
    fn policy_rewrite_model() {
        let policy = PolicyConfig {
            enabled: true,
            rules: vec![PolicyRule::RewriteModel {
                provider: Some("default".to_string()),
                model: Some("gpt-4o-mini".to_string()),
                to: "gpt-4o".to_string(),
            }],
        };

        let mut body = json!({"model":"gpt-4o-mini","messages":[]});
        policy.apply("default", "/v1/chat/completions", &mut body).unwrap();
        assert_eq!(body["model"], "gpt-4o");
    }

    #[test]
    fn policy_filter_tools_allow_list() {
        let policy = PolicyConfig {
            enabled: true,
            rules: vec![PolicyRule::FilterTools {
                provider: None,
                model: None,
                allow: Some(vec!["get_*".to_string()]),
                deny: None,
            }],
        };

        let mut body = json!({
            "model":"gpt-4o-mini",
            "tools":[
                {"type":"function","function":{"name":"get_weather","parameters":{}}},
                {"type":"function","function":{"name":"delete_all","parameters":{}}}
            ]
        });

        policy.apply("default", "/v1/chat/completions", &mut body).unwrap();
        let tools = body["tools"].as_array().unwrap();
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0]["function"]["name"], "get_weather");
    }
}

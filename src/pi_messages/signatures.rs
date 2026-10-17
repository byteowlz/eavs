//! Advisory provenance for opaque provider signatures; never authorization.
use serde_json::{json, Value};

pub fn seal(signature: &Value, provider: &str, model: &str, api: &str) -> String {
    json!({"eavsSignature":1,"provider":provider,"model":model,"api":api,"value":signature})
        .to_string()
}
pub fn open(value: &str, provider: &str, model: &str, api: &str) -> Result<Value, ()> {
    let parsed: Value = serde_json::from_str(value).map_err(|_| ())?;
    if parsed["eavsSignature"] != 1
        || parsed["provider"] != provider
        || parsed["model"] != model
        || parsed["api"] != api
    {
        return Err(());
    }
    Ok(parsed["value"].clone())
}

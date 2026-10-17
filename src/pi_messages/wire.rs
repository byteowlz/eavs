use crate::keys::pricing::ModelPricing;
use serde_json::{json, Value};
use std::collections::BTreeMap;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Text,
    Thinking,
    Tool,
}
struct Block {
    kind: Kind,
    text: String,
    id: String,
    name: String,
    signature: Option<Value>,
    redacted: bool,
    ended: bool,
}
#[derive(Clone, Copy, Default)]
pub struct Counts {
    pub input: u32,
    pub output: u32,
    pub read: u32,
    pub write: u32,
}
pub struct Wire {
    pub provider: String,
    pub model: String,
    pub api: &'static str,
    blocks: Vec<Block>,
    keys: BTreeMap<String, usize>,
    pub counts: Option<Counts>,
    pub includes_cache: bool,
    pub reason: Option<&'static str>,
    pub response_id: Option<String>,
    pub terminal: bool,
    pub started: bool,
    pub oauth_tools: bool,
    pub request_id: String,
    total_bytes: usize,
}
impl Wire {
    pub fn new(provider: String, model: String, api: &'static str) -> Self {
        Self {
            provider,
            model,
            api,
            blocks: Vec::new(),
            keys: BTreeMap::new(),
            counts: None,
            includes_cache: api != "anthropic-messages",
            reason: None,
            response_id: None,
            terminal: false,
            started: false,
            oauth_tools: false,
            request_id: uuid::Uuid::new_v4().to_string(),
            total_bytes: 0,
        }
    }
    pub const fn len(&self) -> usize {
        self.blocks.len()
    }
    pub fn has_tools(&self) -> bool {
        self.blocks.iter().any(|b| b.kind == Kind::Tool)
    }
    pub fn last(&self, kind: Kind) -> Option<usize> {
        self.blocks
            .len()
            .checked_sub(1)
            .filter(|i| self.blocks[*i].kind == kind && !self.blocks[*i].ended)
    }
    pub fn empty(&self, index: usize) -> Result<bool, ()> {
        Ok(self.blocks.get(index).ok_or(())?.text.is_empty())
    }
    pub fn start(&mut self, output: &mut Vec<Value>) {
        if !self.started {
            self.started = true;
            output.push(json!({"type":"start"}));
        }
    }
    pub fn block(
        &mut self,
        key: String,
        kind: Kind,
        id: &str,
        name: &str,
        output: &mut Vec<Value>,
    ) -> Result<usize, ()> {
        self.start(output);
        if let Some(index) = self.keys.get(&key) {
            if self.blocks[*index].kind != kind {
                return Err(());
            }
            return Ok(*index);
        }
        if self.blocks.len() >= 1024 || key.len() > 1024 || id.len() > 512 || name.len() > 512 {
            return Err(());
        }
        let index = self.blocks.len();
        self.keys.insert(key, index);
        self.blocks.push(Block {
            kind,
            text: String::new(),
            id: id.to_owned(),
            name: name.to_owned(),
            signature: None,
            redacted: false,
            ended: false,
        });
        output.push(match kind {
            Kind::Text => json!({"type":"text_start","contentIndex":index}),
            Kind::Thinking => json!({"type":"thinking_start","contentIndex":index}),
            Kind::Tool => {
                json!({"type":"toolcall_start","contentIndex":index,"id":id,"toolName":name})
            }
        });
        Ok(index)
    }
    pub fn index(&self, key: &str) -> Result<usize, ()> {
        self.keys.get(key).copied().ok_or(())
    }
    pub fn delta(&mut self, index: usize, delta: &str, output: &mut Vec<Value>) -> Result<(), ()> {
        self.total_bytes += delta.len();
        if self.total_bytes > 16 * 1024 * 1024 {
            return Err(());
        }
        let block = self.blocks.get_mut(index).ok_or(())?;
        if block.ended || block.text.len() + delta.len() > 4 * 1024 * 1024 {
            return Err(());
        }
        block.text.push_str(delta);
        let kind = match block.kind {
            Kind::Text => "text_delta",
            Kind::Thinking => "thinking_delta",
            Kind::Tool => "toolcall_delta",
        };
        output.push(json!({"type":kind,"contentIndex":index,"delta":delta}));
        Ok(())
    }
    pub fn signature(&mut self, index: usize, value: Value, append: bool) -> Result<(), ()> {
        let size = value.to_string().len();
        self.total_bytes = self.total_bytes.checked_add(size).ok_or(())?;
        if size > 1024 * 1024 || self.total_bytes > 16 * 1024 * 1024 {
            return Err(());
        }
        let block = self.blocks.get_mut(index).ok_or(())?;
        if append {
            let old = block
                .signature
                .as_ref()
                .and_then(Value::as_str)
                .unwrap_or_default();
            let new = value.as_str().ok_or(())?;
            if old.len() + new.len() > 1024 * 1024 {
                return Err(());
            }
            block.signature = Some(format!("{old}{new}").into());
        } else {
            block.signature = Some(value);
        }
        Ok(())
    }
    pub fn redacted(&mut self, index: usize) -> Result<(), ()> {
        self.blocks.get_mut(index).ok_or(())?.redacted = true;
        Ok(())
    }
    pub fn end(&mut self, index: usize, output: &mut Vec<Value>) -> Result<(), ()> {
        let block = self.blocks.get_mut(index).ok_or(())?;
        if block.ended {
            return Ok(());
        }
        block.ended = true;
        let mut value = match block.kind {
            Kind::Text => json!({"type":"text_end","contentIndex":index,"content":block.text}),
            Kind::Thinking => {
                json!({"type":"thinking_end","contentIndex":index,"content":block.text,"redacted":block.redacted})
            }
            Kind::Tool => {
                let arguments: Value = serde_json::from_str(if block.text.is_empty() {
                    "{}"
                } else {
                    &block.text
                })
                .map_err(|_| ())?;
                if !arguments.is_object() {
                    return Err(());
                }
                json!({"type":"toolcall_end","contentIndex":index,"toolCall":{"type":"toolCall","id":block.id,"name":block.name,"arguments":arguments}})
            }
        };
        if let Some(signature) = &block.signature {
            let signature =
                super::signatures::seal(signature, &self.provider, &self.model, self.api);
            if block.kind == Kind::Tool {
                value["toolCall"]["thoughtSignature"] = signature.into();
            } else {
                value["contentSignature"] = signature.into();
            }
        }
        output.push(value);
        Ok(())
    }
    pub fn usage(&self, pricing: &ModelPricing) -> Result<Value, ()> {
        let counts = self.counts.ok_or(())?;
        let cached = counts.read.checked_add(counts.write).ok_or(())?;
        let input = if self.includes_cache {
            counts.input.checked_sub(cached).ok_or(())?
        } else {
            counts.input
        };
        let total = input
            .checked_add(counts.output)
            .and_then(|v| v.checked_add(cached))
            .ok_or(())?;
        let input_cost = f64::from(input) * pricing.input_cost_per_token;
        let output_cost = f64::from(counts.output) * pricing.output_cost_per_token;
        let read_cost = rate_cost(counts.read, pricing.cached_input_cost_per_token)?;
        let write_cost = rate_cost(counts.write, pricing.cache_write_cost_per_token)?;
        let total_cost = input_cost + output_cost + read_cost + write_cost;
        if ![input_cost, output_cost, read_cost, write_cost, total_cost]
            .iter()
            .all(|v| v.is_finite() && *v >= 0.0)
        {
            return Err(());
        }
        Ok(
            json!({"input":input,"output":counts.output,"cacheRead":counts.read,"cacheWrite":counts.write,"totalTokens":total,"cost":{"input":input_cost,"output":output_cost,"cacheRead":read_cost,"cacheWrite":write_cost,"total":total_cost}}),
        )
    }
    pub fn finish(&mut self, pricing: &ModelPricing, output: &mut Vec<Value>) -> Result<(), ()> {
        if !self.terminal {
            return Err(());
        }
        let reason = self.reason.ok_or(())?;
        if reason == "error" || (reason == "toolUse") != self.has_tools() {
            return Err(());
        }
        self.start(output);
        for index in 0..self.blocks.len() {
            self.end(index, output)?;
        }
        let mut event = json!({"type":"done","reason":reason,"usage":self.usage(pricing)?});
        if let Some(id) = &self.response_id {
            event["responseId"] = id.clone().into();
        }
        output.push(event);
        Ok(())
    }
    pub fn failure(&self, pricing: &ModelPricing) -> Value {
        let usage = self.usage(pricing).unwrap_or_else(|()|json!({"input":0,"output":0,"cacheRead":0,"cacheWrite":0,"totalTokens":0,"cost":{"input":0,"output":0,"cacheRead":0,"cacheWrite":0,"total":0}}));
        json!({"type":"error","reason":"error","usage":usage,"errorMessage":"Provider stream could not be represented safely; usage may have been incurred. Do not retry automatically."})
    }
}
fn rate_cost(tokens: u32, rate: Option<f64>) -> Result<f64, ()> {
    if tokens == 0 {
        return Ok(0.0);
    }
    let rate = rate.ok_or(())?;
    if !rate.is_finite() || rate < 0.0 {
        return Err(());
    }
    Ok(f64::from(tokens) * rate)
}
pub fn count(value: &Value) -> Result<u32, ()> {
    u32::try_from(value.as_u64().ok_or(())?).map_err(|_| ())
}
pub fn optional_count(value: &Value) -> Result<u32, ()> {
    if value.is_null() {
        Ok(0)
    } else {
        count(value)
    }
}
pub fn reason(value: &str) -> Result<&'static str, ()> {
    match value {
        "stop" | "end_turn" | "stop_sequence" | "STOP" => Ok("stop"),
        "length" | "max_tokens" | "MAX_TOKENS" => Ok("length"),
        "tool_calls" | "tool_use" => Ok("toolUse"),
        _ => Err(()),
    }
}

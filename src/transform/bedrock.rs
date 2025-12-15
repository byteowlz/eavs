use crate::transform::{RequestTransformer, ResponseTransformer, TransformError};
use crate::types::{ApiType, AssistantMessage, Context, StopReason, StreamEvent, StreamState, TextContent, Usage};
use serde_json::{json, Value};

/// AWS Bedrock transformer.
///
/// Notes:
/// - Bedrock model IDs are supplied via `context.model` and used in the URL path.
/// - Request bodies vary by model family; this transformer supports:
///   - Anthropic Claude models (`anthropic.*`) via the Bedrock "Anthropic Messages" schema
///   - Amazon Titan text models (`amazon.titan*`) with a best-effort text schema
#[derive(Debug, Clone, Default)]
pub struct BedrockTransformer;

impl BedrockTransformer {
    pub fn new() -> Self {
        Self
    }

    fn is_anthropic_model(model_id: &str) -> bool {
        model_id.starts_with("anthropic.")
    }

    fn is_titan_text_model(model_id: &str) -> bool {
        model_id.starts_with("amazon.titan")
    }
}

impl RequestTransformer for BedrockTransformer {
    fn transform_request(&self, context: &Context) -> Result<Value, TransformError> {
        if context.stream {
            return Err(TransformError::Unsupported(
                "Bedrock streaming is not implemented (invoke-with-response-stream)".to_string(),
            ));
        }

        if Self::is_anthropic_model(&context.model) {
            // Reuse Anthropic transformer request shape and adapt to Bedrock schema.
            let anthropic = crate::transform::AnthropicTransformer::new().with_cache(false);
            let mut req = anthropic.transform_request(context)?;

            // Bedrock uses model ID in the URL path, not in the body.
            req.as_object_mut()
                .map(|o| o.remove("model"));

            // Bedrock requires anthropic_version for Claude models.
            req["anthropic_version"] = json!("bedrock-2023-05-31");
            return Ok(req);
        }

        if Self::is_titan_text_model(&context.model) {
            let mut input = String::new();
            if let Some(system) = &context.system_prompt {
                input.push_str(system);
                input.push_str("\n\n");
            }

            for msg in &context.messages {
                match msg {
                    crate::types::Message::User(u) => {
                        for block in &u.content {
                            if let crate::types::ContentBlock::Text(t) = block {
                                input.push_str(&t.text);
                                input.push('\n');
                            }
                        }
                    }
                    crate::types::Message::Assistant(a) => {
                        for block in &a.content {
                            if let crate::types::ContentBlock::Text(t) = block {
                                input.push_str(&t.text);
                                input.push('\n');
                            }
                        }
                    }
                    _ => {}
                }
            }

            let mut cfg = json!({});
            if let Some(max_tokens) = context.max_tokens {
                cfg["maxTokenCount"] = json!(max_tokens);
            }
            if let Some(temp) = context.temperature {
                cfg["temperature"] = json!(temp);
            }
            if let Some(top_p) = context.top_p {
                cfg["topP"] = json!(top_p);
            }
            if let Some(stop) = &context.stop {
                cfg["stopSequences"] = json!(stop);
            }

            let mut req = json!({
                "inputText": input.trim_end(),
            });
            if cfg.as_object().map(|o| !o.is_empty()).unwrap_or(false) {
                req["textGenerationConfig"] = cfg;
            }
            return Ok(req);
        }

        Err(TransformError::Unsupported(format!(
            "Unsupported Bedrock model family: {}",
            context.model
        )))
    }

    fn headers(&self, _api_key: &str) -> Vec<(String, String)> {
        // SigV4 is applied at the HTTP layer.
        vec![("Content-Type".to_string(), "application/json".to_string())]
    }

    fn endpoint_path(&self, context: &Context) -> String {
        // Non-streaming invoke.
        format!("/model/{}/invoke", context.model)
    }
}

impl ResponseTransformer for BedrockTransformer {
    fn parse_stream_chunk(
        &self,
        _chunk: &str,
        _state: &mut StreamState,
    ) -> Result<Vec<StreamEvent>, TransformError> {
        Err(TransformError::Unsupported(
            "Bedrock streaming is not implemented".to_string(),
        ))
    }

    fn parse_response(&self, body: &Value) -> Result<Vec<StreamEvent>, TransformError> {
        // Try Claude (Anthropic-compatible) response first.
        if body.get("content").is_some() && body.get("usage").is_some() {
            let anthropic = crate::transform::AnthropicTransformer::new().with_cache(false);
            return anthropic.parse_response(body);
        }

        // Titan text response (best-effort).
        if let Some(text) = body
            .get("results")
            .and_then(|r| r.get(0))
            .and_then(|r| r.get("outputText"))
            .and_then(|t| t.as_str())
        {
            let mut msg = AssistantMessage {
                api: ApiType::OpenAICompletions,
                stop_reason: StopReason::EndTurn,
                ..Default::default()
            };
            msg.content.push(crate::types::ContentBlock::Text(TextContent::new(text)));
            msg.usage = Usage::default();

            return Ok(vec![
                StreamEvent::TextEnd {
                    content_index: 0,
                    content: text.to_string(),
                },
                StreamEvent::Done {
                    reason: StopReason::EndTurn,
                    message: msg,
                },
            ]);
        }

        Err(TransformError::InvalidJson(
            "Unrecognized Bedrock response format".to_string(),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::Message;

    #[test]
    fn bedrock_anthropic_request_contains_anthropic_version() {
        let ctx = Context::new("anthropic.claude-3-opus-20240229-v1:0")
            .with_messages(vec![Message::user("hi")])
            .with_max_tokens(16);
        let req = BedrockTransformer::new().transform_request(&ctx).unwrap();
        assert_eq!(req["anthropic_version"], "bedrock-2023-05-31");
        assert!(req.get("model").is_none());
    }

    #[test]
    fn bedrock_titan_request_uses_input_text() {
        let ctx = Context::new("amazon.titan-text-express-v1")
            .with_messages(vec![Message::user("hi")])
            .with_max_tokens(16);
        let req = BedrockTransformer::new().transform_request(&ctx).unwrap();
        assert!(req.get("inputText").is_some());
    }

    #[test]
    fn bedrock_titan_response_parses_text() {
        let body = json!({
            "results": [
                { "outputText": "ok" }
            ]
        });

        let events = BedrockTransformer::new().parse_response(&body).unwrap();
        assert!(events.iter().any(|e| matches!(e, StreamEvent::TextEnd { content, .. } if content == "ok")));
        assert!(events.iter().any(|e| matches!(e, StreamEvent::Done { .. })));
    }
}

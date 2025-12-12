//! Cross-provider message transformation.
//!
//! Handles transforming messages when they cross provider boundaries,
//! including thinking block conversion and orphan tool call filtering.

use crate::types::{AssistantMessage, ContentBlock, Message, TextContent, ToolCall};
use std::collections::HashSet;

/// Transform messages for cross-provider compatibility.
///
/// When messages from one provider are sent to a different provider, special handling is needed:
/// - Thinking blocks are converted to text with `<thinking>` tags
/// - Orphan tool calls (without results) are filtered out
pub fn transform_messages(
    messages: &[Message],
    target_provider: &str,
    target_api: &str,
) -> Vec<Message> {
    let transformed: Vec<Message> = messages
        .iter()
        .map(|msg| match msg {
            Message::Assistant(assistant) => {
                // Check if from different provider/API
                let needs_transform = assistant.provider != target_provider
                    || !api_matches(&assistant.api, target_api);

                if needs_transform {
                    Message::Assistant(transform_assistant_message(assistant))
                } else {
                    msg.clone()
                }
            }
            _ => msg.clone(),
        })
        .collect();

    filter_orphan_tool_calls(transformed)
}

/// Check if an ApiType matches a target string.
fn api_matches(api: &crate::types::ApiType, target: &str) -> bool {
    use crate::types::ApiType;
    let target_lower = target.to_lowercase().replace('_', "");
    match api {
        ApiType::OpenAICompletions => target_lower == "openaicompletions",
        ApiType::OpenAIResponses => target_lower == "openairesponses",
        ApiType::AnthropicMessages => target_lower == "anthropicmessages",
        ApiType::GoogleGenerativeAI => target_lower == "googlegenerativeai",
    }
}

/// Transform an assistant message for cross-provider compatibility.
fn transform_assistant_message(assistant: &AssistantMessage) -> AssistantMessage {
    let transformed_content: Vec<ContentBlock> = assistant
        .content
        .iter()
        .map(|block| match block {
            ContentBlock::Thinking(thinking) => {
                // Convert thinking to text with tags
                ContentBlock::Text(TextContent::new(format!(
                    "<thinking>\n{}\n</thinking>",
                    thinking.thinking
                )))
            }
            other => other.clone(),
        })
        .collect();

    AssistantMessage {
        content: transformed_content,
        ..assistant.clone()
    }
}

/// Filter out orphan tool calls that don't have corresponding tool results.
///
/// A tool call is considered orphan if:
/// - It's not in the last message, AND
/// - There's no tool result message following it with matching tool_call_id
pub fn filter_orphan_tool_calls(mut messages: Vec<Message>) -> Vec<Message> {
    if messages.is_empty() {
        return messages;
    }

    // Collect all tool result IDs
    let tool_result_ids: HashSet<String> = messages
        .iter()
        .filter_map(|msg| match msg {
            Message::Tool(tool) => Some(tool.tool_call_id.clone()),
            _ => None,
        })
        .collect();

    // Process all messages except the last one
    let last_idx = messages.len() - 1;
    
    for (idx, msg) in messages.iter_mut().enumerate() {
        if idx == last_idx {
            // Don't filter tool calls from the last message
            continue;
        }

        if let Message::Assistant(assistant) = msg {
            // Filter out tool calls without matching results
            let filtered_content: Vec<ContentBlock> = assistant
                .content
                .iter()
                .filter(|block| {
                    match block {
                        ContentBlock::ToolCall(tc) => {
                            // Keep if there's a matching tool result
                            tool_result_ids.contains(&tc.id)
                        }
                        _ => true,
                    }
                })
                .cloned()
                .collect();

            assistant.content = filtered_content;
        }
    }

    messages
}

/// Check if messages contain any thinking blocks.
pub fn has_thinking_blocks(messages: &[Message]) -> bool {
    messages.iter().any(|msg| {
        if let Message::Assistant(assistant) = msg {
            assistant.content.iter().any(|block| {
                matches!(block, ContentBlock::Thinking(_))
            })
        } else {
            false
        }
    })
}

/// Extract all tool calls from messages.
pub fn extract_tool_calls(messages: &[Message]) -> Vec<&ToolCall> {
    messages
        .iter()
        .filter_map(|msg| {
            if let Message::Assistant(assistant) = msg {
                Some(
                    assistant
                        .content
                        .iter()
                        .filter_map(|block| {
                            if let ContentBlock::ToolCall(tc) = block {
                                Some(tc)
                            } else {
                                None
                            }
                        })
                        .collect::<Vec<_>>(),
                )
            } else {
                None
            }
        })
        .flatten()
        .collect()
}

/// Count the number of messages by role.
pub fn count_messages_by_role(messages: &[Message]) -> (usize, usize, usize) {
    let mut user = 0;
    let mut assistant = 0;
    let mut tool = 0;

    for msg in messages {
        match msg {
            Message::User(_) => user += 1,
            Message::Assistant(_) => assistant += 1,
            Message::Tool(_) => tool += 1,
            Message::System(_) => {}
        }
    }

    (user, assistant, tool)
}

/// Convert all thinking blocks to text blocks with tags.
pub fn convert_thinking_to_text(messages: Vec<Message>) -> Vec<Message> {
    messages
        .into_iter()
        .map(|msg| match msg {
            Message::Assistant(mut assistant) => {
                assistant.content = assistant
                    .content
                    .into_iter()
                    .map(|block| match block {
                        ContentBlock::Thinking(thinking) => {
                            ContentBlock::Text(TextContent::new(format!(
                                "<thinking>\n{}\n</thinking>",
                                thinking.thinking
                            )))
                        }
                        other => other,
                    })
                    .collect();
                Message::Assistant(assistant)
            }
            other => other,
        })
        .collect()
}

/// Merge consecutive text blocks in messages.
pub fn merge_text_blocks(messages: Vec<Message>) -> Vec<Message> {
    messages
        .into_iter()
        .map(|msg| match msg {
            Message::Assistant(mut assistant) => {
                assistant.content = merge_consecutive_text(&assistant.content);
                Message::Assistant(assistant)
            }
            Message::User(mut user) => {
                user.content = merge_consecutive_text(&user.content);
                Message::User(user)
            }
            other => other,
        })
        .collect()
}

fn merge_consecutive_text(blocks: &[ContentBlock]) -> Vec<ContentBlock> {
    let mut result = Vec::new();
    let mut current_text = String::new();

    for block in blocks {
        match block {
            ContentBlock::Text(t) => {
                if !current_text.is_empty() {
                    current_text.push('\n');
                }
                current_text.push_str(&t.text);
            }
            other => {
                if !current_text.is_empty() {
                    result.push(ContentBlock::Text(TextContent::new(std::mem::take(&mut current_text))));
                }
                result.push(other.clone());
            }
        }
    }

    if !current_text.is_empty() {
        result.push(ContentBlock::Text(TextContent::new(current_text)));
    }

    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{ApiType, ThinkingContent, ToolResultMessage};

    fn make_assistant_with_thinking() -> AssistantMessage {
        AssistantMessage {
            content: vec![
                ContentBlock::Thinking(ThinkingContent::new("Let me think...")),
                ContentBlock::Text(TextContent::new("Here's my answer")),
            ],
            provider: "anthropic".to_string(),
            api: ApiType::AnthropicMessages,
            ..Default::default()
        }
    }

    fn make_assistant_with_tool_call(id: &str, name: &str) -> AssistantMessage {
        AssistantMessage {
            content: vec![
                ContentBlock::Text(TextContent::new("I'll get the weather")),
                ContentBlock::ToolCall(ToolCall::new(id, name, serde_json::json!({"city": "NYC"}))),
            ],
            ..Default::default()
        }
    }

    #[test]
    fn test_transform_thinking_blocks() {
        let messages = vec![
            Message::user("Hello"),
            Message::Assistant(make_assistant_with_thinking()),
        ];

        let transformed = transform_messages(&messages, "openai", "openai_completions");

        if let Message::Assistant(assistant) = &transformed[1] {
            // First content should now be text with tags
            if let ContentBlock::Text(t) = &assistant.content[0] {
                assert!(t.text.contains("<thinking>"));
                assert!(t.text.contains("Let me think..."));
                assert!(t.text.contains("</thinking>"));
            } else {
                panic!("Expected text block");
            }
        } else {
            panic!("Expected assistant message");
        }
    }

    #[test]
    fn test_no_transform_same_provider() {
        let assistant = AssistantMessage {
            content: vec![
                ContentBlock::Thinking(ThinkingContent::new("Thinking...")),
            ],
            provider: "anthropic".to_string(),
            api: ApiType::AnthropicMessages,
            ..Default::default()
        };

        let messages = vec![Message::Assistant(assistant)];

        // Same provider should not transform
        let transformed = transform_messages(&messages, "anthropic", "anthropic_messages");

        if let Message::Assistant(assistant) = &transformed[0] {
            // Should still be thinking block
            assert!(matches!(&assistant.content[0], ContentBlock::Thinking(_)));
        } else {
            panic!("Expected assistant message");
        }
    }

    #[test]
    fn test_filter_orphan_tool_calls() {
        let messages = vec![
            Message::user("Get weather"),
            Message::Assistant(make_assistant_with_tool_call("call_123", "get_weather")),
            // No tool result for call_123
            Message::user("Thanks"),
            Message::Assistant(make_assistant_with_tool_call("call_456", "search")),
            Message::Tool(ToolResultMessage::text("call_456", "search", "Results")),
        ];

        let filtered = filter_orphan_tool_calls(messages);

        // First assistant should have tool call filtered out
        if let Message::Assistant(assistant) = &filtered[1] {
            assert_eq!(assistant.content.len(), 1); // Only text, tool call removed
            assert!(matches!(&assistant.content[0], ContentBlock::Text(_)));
        } else {
            panic!("Expected assistant message");
        }

        // Second assistant should keep tool call (has result)
        if let Message::Assistant(assistant) = &filtered[3] {
            assert_eq!(assistant.content.len(), 2);
            assert!(matches!(&assistant.content[1], ContentBlock::ToolCall(_)));
        } else {
            panic!("Expected assistant message");
        }
    }

    #[test]
    fn test_keep_last_message_tool_calls() {
        let messages = vec![
            Message::user("Search for something"),
            Message::Assistant(make_assistant_with_tool_call("call_999", "search")),
            // This is the last message, tool call should be kept even without result
        ];

        let filtered = filter_orphan_tool_calls(messages);

        if let Message::Assistant(assistant) = &filtered[1] {
            // Tool call should still be there (last message)
            assert_eq!(assistant.content.len(), 2);
            assert!(matches!(&assistant.content[1], ContentBlock::ToolCall(_)));
        } else {
            panic!("Expected assistant message");
        }
    }

    #[test]
    fn test_has_thinking_blocks() {
        let with_thinking = vec![
            Message::user("Hello"),
            Message::Assistant(make_assistant_with_thinking()),
        ];
        assert!(has_thinking_blocks(&with_thinking));

        let without_thinking = vec![
            Message::user("Hello"),
            Message::assistant("Hi there"),
        ];
        assert!(!has_thinking_blocks(&without_thinking));
    }

    #[test]
    fn test_extract_tool_calls() {
        let messages = vec![
            Message::user("Get weather and search"),
            Message::Assistant(AssistantMessage {
                content: vec![
                    ContentBlock::ToolCall(ToolCall::new("call_1", "get_weather", serde_json::json!({}))),
                    ContentBlock::ToolCall(ToolCall::new("call_2", "search", serde_json::json!({}))),
                ],
                ..Default::default()
            }),
        ];

        let tool_calls = extract_tool_calls(&messages);
        assert_eq!(tool_calls.len(), 2);
        assert_eq!(tool_calls[0].name, "get_weather");
        assert_eq!(tool_calls[1].name, "search");
    }

    #[test]
    fn test_count_messages_by_role() {
        let messages = vec![
            Message::user("Hello"),
            Message::assistant("Hi"),
            Message::user("Get weather"),
            Message::Assistant(make_assistant_with_tool_call("call_1", "get_weather")),
            Message::Tool(ToolResultMessage::text("call_1", "get_weather", "Sunny")),
        ];

        let (user, assistant, tool) = count_messages_by_role(&messages);
        assert_eq!(user, 2);
        assert_eq!(assistant, 2);
        assert_eq!(tool, 1);
    }

    #[test]
    fn test_convert_thinking_to_text() {
        let messages = vec![
            Message::user("Think about this"),
            Message::Assistant(AssistantMessage {
                content: vec![
                    ContentBlock::Thinking(ThinkingContent::new("Thinking...")),
                    ContentBlock::Text(TextContent::new("Answer")),
                ],
                ..Default::default()
            }),
        ];

        let converted = convert_thinking_to_text(messages);

        if let Message::Assistant(assistant) = &converted[1] {
            assert_eq!(assistant.content.len(), 2);
            // Both should be text now
            assert!(matches!(&assistant.content[0], ContentBlock::Text(_)));
            assert!(matches!(&assistant.content[1], ContentBlock::Text(_)));
        } else {
            panic!("Expected assistant message");
        }
    }

    #[test]
    fn test_merge_text_blocks() {
        let messages = vec![
            Message::Assistant(AssistantMessage {
                content: vec![
                    ContentBlock::Text(TextContent::new("First")),
                    ContentBlock::Text(TextContent::new("Second")),
                    ContentBlock::ToolCall(ToolCall::new("call_1", "test", serde_json::json!({}))),
                    ContentBlock::Text(TextContent::new("Third")),
                ],
                ..Default::default()
            }),
        ];

        let merged = merge_text_blocks(messages);

        if let Message::Assistant(assistant) = &merged[0] {
            assert_eq!(assistant.content.len(), 3);
            
            // First two texts should be merged
            if let ContentBlock::Text(t) = &assistant.content[0] {
                assert!(t.text.contains("First"));
                assert!(t.text.contains("Second"));
            } else {
                panic!("Expected text block");
            }
            
            // Tool call preserved
            assert!(matches!(&assistant.content[1], ContentBlock::ToolCall(_)));
            
            // Last text preserved
            if let ContentBlock::Text(t) = &assistant.content[2] {
                assert_eq!(t.text, "Third");
            } else {
                panic!("Expected text block");
            }
        } else {
            panic!("Expected assistant message");
        }
    }

    #[test]
    fn test_empty_messages() {
        let messages: Vec<Message> = vec![];
        
        let transformed = transform_messages(&messages, "openai", "openai_completions");
        assert!(transformed.is_empty());
        
        let filtered = filter_orphan_tool_calls(messages);
        assert!(filtered.is_empty());
    }
}

use crate::tools::ownership::ChatToolCall;
use serde_json::{json, Value};

pub(crate) fn chat_tool_calls(chat: &Value) -> Vec<ChatToolCall> {
    chat.pointer("/choices/0/message/tool_calls")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(|item| {
                    let id = item.get("id").and_then(Value::as_str)?.to_owned();
                    let function = item.get("function")?;
                    let name = function.get("name").and_then(Value::as_str)?.to_owned();
                    let arguments = function
                        .get("arguments")
                        .and_then(Value::as_str)
                        .unwrap_or("{}")
                        .to_owned();
                    Some(ChatToolCall {
                        id,
                        name,
                        arguments,
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

pub(crate) fn normalize_assistant_tool_message(mut message: Value) -> Value {
    if message.get("content").is_none() || message.get("content") == Some(&Value::Null) {
        message["content"] = Value::String(String::new());
    }
    let has_tool_calls = message
        .get("tool_calls")
        .and_then(Value::as_array)
        .map(|calls| !calls.is_empty())
        .unwrap_or(false);
    if message
        .get("reasoning_content")
        .and_then(Value::as_str)
        .map(str::trim)
        .unwrap_or_default()
        .is_empty()
    {
        if has_tool_calls {
            message["reasoning_content"] = Value::String(String::new());
        } else if let Some(object) = message.as_object_mut() {
            object.remove("reasoning_content");
        }
    }
    message
}

pub(crate) fn full_assistant_tool_message_from_chat(chat: &Value) -> Result<Value, String> {
    chat.pointer("/choices/0/message")
        .cloned()
        .ok_or_else(|| "tool call response did not include an assistant message".to_owned())
        .map(normalize_assistant_tool_message)
}

pub(crate) fn assistant_message_from_chat_tool_subset(
    chat: &Value,
    tool_calls: &[ChatToolCall],
) -> Value {
    let message = chat.pointer("/choices/0/message").unwrap_or(&Value::Null);
    let content = message
        .get("content")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let reasoning_content = message
        .get("reasoning_content")
        .and_then(Value::as_str)
        .unwrap_or_default();
    chat_tool_calls_to_assistant_message(tool_calls, content, reasoning_content)
}

pub(crate) fn chat_tool_calls_to_assistant_message(
    tool_calls: &[ChatToolCall],
    content: &str,
    reasoning_content: &str,
) -> Value {
    let mut message = json!({
        "role": "assistant",
        "content": content,
        "tool_calls": tool_calls.iter().map(|call| json!({
            "id": call.id,
            "type": "function",
            "function": {
                "name": call.name,
                "arguments": call.arguments
            }
        })).collect::<Vec<_>>()
    });
    if !tool_calls.is_empty() || !reasoning_content.trim().is_empty() {
        message["reasoning_content"] = Value::String(reasoning_content.to_owned());
    }
    message
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_chat_tool_calls_from_deepseek_shape() {
        let chat = json!({
            "choices": [{
                "message": {
                    "tool_calls": [{
                        "id": "call_1",
                        "type": "function",
                        "function": {
                            "name": "list_directory",
                            "arguments": "{\"path\":\".\"}"
                        }
                    }]
                }
            }]
        });

        let calls = chat_tool_calls(&chat);

        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].id, "call_1");
        assert_eq!(calls[0].name, "list_directory");
        assert_eq!(calls[0].arguments, "{\"path\":\".\"}");
    }

    #[test]
    fn assistant_tool_message_preserves_reasoning_content_even_when_empty() {
        let calls = vec![ChatToolCall {
            id: "call_abc".to_owned(),
            name: "list_directory".to_owned(),
            arguments: r#"{"path":"."}"#.to_owned(),
        }];

        let message = chat_tool_calls_to_assistant_message(&calls, "", "");

        assert_eq!(message["role"], "assistant");
        assert_eq!(message["content"], "");
        assert_eq!(message["reasoning_content"], "");
        assert_eq!(message["tool_calls"][0]["id"], "call_abc");
    }

    #[test]
    fn subset_message_keeps_deepseek_reasoning_for_proxy_tool_replay() {
        let chat = json!({
            "choices": [{
                "message": {
                    "role": "assistant",
                    "content": "",
                    "reasoning_content": "I need a directory listing first.",
                    "tool_calls": [
                        {
                            "id": "call_code",
                            "type": "function",
                            "function": {
                                "name": "list_directory",
                                "arguments": "{\"path\":\".\"}"
                            }
                        },
                        {
                            "id": "call_external",
                            "type": "function",
                            "function": {
                                "name": "mcp__demo__add",
                                "arguments": "{\"a\":1,\"b\":2}"
                            }
                        }
                    ]
                }
            }]
        });
        let code_calls = vec![ChatToolCall {
            id: "call_code".to_owned(),
            name: "list_directory".to_owned(),
            arguments: r#"{"path":"."}"#.to_owned(),
        }];

        let message = assistant_message_from_chat_tool_subset(&chat, &code_calls);

        assert_eq!(
            message["reasoning_content"],
            "I need a directory listing first."
        );
        assert_eq!(message["tool_calls"].as_array().unwrap().len(), 1);
        assert_eq!(message["tool_calls"][0]["id"], "call_code");
    }

    #[test]
    fn final_assistant_without_tools_drops_empty_reasoning_content() {
        let message = normalize_assistant_tool_message(json!({
            "role": "assistant",
            "content": "done",
            "reasoning_content": ""
        }));

        assert!(message.get("reasoning_content").is_none());
    }
}

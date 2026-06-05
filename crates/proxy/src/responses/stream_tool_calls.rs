mod visible_bridge;

pub(crate) use visible_bridge::StreamingVisibleToolBridge;

use crate::tools::ownership::ChatToolCall;
use serde_json::Value;
use std::collections::BTreeMap;
use uuid::Uuid;

#[derive(Debug, Default)]
pub(crate) struct StreamingToolCallState {
    id: String,
    name: String,
    arguments: String,
}

pub(crate) fn collect_streaming_tool_call_deltas(
    delta: &Value,
    states: &mut BTreeMap<u64, StreamingToolCallState>,
    last_tool_index: &mut u64,
) {
    let Some(calls) = delta.get("tool_calls").and_then(Value::as_array) else {
        return;
    };
    for call in calls {
        let index = call
            .get("index")
            .and_then(Value::as_u64)
            .unwrap_or(*last_tool_index);
        *last_tool_index = index;
        let state = states.entry(index).or_default();
        if let Some(id) = call.get("id").and_then(Value::as_str) {
            state.id = id.to_owned();
        }
        if let Some(function) = call.get("function") {
            if let Some(name) = function.get("name").and_then(Value::as_str) {
                state.name.push_str(name);
            }
            if let Some(arguments) = function.get("arguments").and_then(Value::as_str) {
                state.arguments.push_str(arguments);
            }
        }
    }
}

pub(crate) fn streaming_tool_calls(
    states: BTreeMap<u64, StreamingToolCallState>,
) -> Vec<ChatToolCall> {
    states
        .into_values()
        .filter(|state| !state.name.trim().is_empty())
        .map(|state| ChatToolCall {
            id: if state.id.trim().is_empty() {
                format!("call_{}", Uuid::new_v4().simple())
            } else {
                state.id
            },
            name: state.name,
            arguments: if state.arguments.trim().is_empty() {
                "{}".to_owned()
            } else {
                state.arguments
            },
        })
        .collect()
}

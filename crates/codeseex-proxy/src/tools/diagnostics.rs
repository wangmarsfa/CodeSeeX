use crate::tools::ownership::{ChatToolCall, ToolCallPartition};
use serde_json::{json, Value};
use std::collections::BTreeMap;

#[derive(Debug, Default)]
pub(crate) struct ToolLoopDiagnostics {
    seen_signatures: BTreeMap<String, u32>,
}

impl ToolLoopDiagnostics {
    pub(crate) fn record_iteration(
        &mut self,
        iteration: u32,
        calls: &[ChatToolCall],
        partition: &ToolCallPartition,
    ) -> Value {
        let calls = calls
            .iter()
            .map(|call| {
                let signature = tool_signature(call);
                let seen = self.seen_signatures.entry(signature).or_insert(0);
                *seen += 1;
                json!({
                    "call_id": call.id,
                    "name": call.name,
                    "arguments_chars": call.arguments.chars().count(),
                    "arguments_fingerprint": stable_fingerprint(&call.arguments),
                    "repeat_count": *seen
                })
            })
            .collect::<Vec<_>>();

        json!({
            "iteration": iteration,
            "calls": calls,
            "counts": {
                "code": partition.code.len(),
                "hosted": partition.hosted.len(),
                "native": partition.native.len(),
                "external": partition.external.len(),
                "unknown": partition.unknown.len()
            }
        })
    }
}

fn tool_signature(call: &ChatToolCall) -> String {
    format!("{}\n{}", call.name, compact_preview(&call.arguments, 2_000))
}

fn stable_fingerprint(value: &str) -> String {
    let mut hash = 0xcbf29ce484222325_u64;
    for byte in value.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    format!("{hash:016x}")
}

fn compact_preview(value: &str, max_chars: usize) -> String {
    let mut output = String::new();
    let mut previous_was_space = false;
    let mut chars = 0_usize;
    for ch in value.chars() {
        let ch = if ch.is_whitespace() { ' ' } else { ch };
        if ch == ' ' {
            if previous_was_space {
                continue;
            }
            previous_was_space = true;
        } else {
            previous_was_space = false;
        }
        if chars >= max_chars {
            output.push_str("...");
            break;
        }
        output.push(ch);
        chars += 1;
    }
    output
}

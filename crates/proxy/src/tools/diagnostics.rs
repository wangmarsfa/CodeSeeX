use crate::tools::ownership::{ChatToolCall, ToolCallPartition};
use serde_json::{json, Value};
use std::collections::BTreeMap;

pub(crate) const MAX_TOOL_LOOP_ITERATIONS: u32 = 8;

#[derive(Debug, Default)]
pub(crate) struct ToolLoopDiagnostics {
    seen_signatures: BTreeMap<String, u32>,
    consecutive_failures_by_name: BTreeMap<String, u32>,
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

    pub(crate) fn repeated_call_warning(&self, call: &ChatToolCall) -> Option<String> {
        let repeat_count = self
            .seen_signatures
            .get(&tool_signature(call))
            .copied()
            .unwrap_or(0);
        if repeat_count < 3 {
            return None;
        }
        Some(format!(
            "CodeSeeX noticed this exact tool call has repeated {repeat_count} times in the same response. Reuse the existing returned evidence when possible instead of repeating the same call again."
        ))
    }

    pub(crate) fn record_tool_result_and_repeated_failure(
        &mut self,
        call: &ChatToolCall,
        result: &Value,
    ) -> Option<String> {
        let failed = result.get("ok").and_then(Value::as_bool) == Some(false)
            || result.get("error").is_some();
        if !failed {
            self.consecutive_failures_by_name
                .insert(call.name.clone(), 0);
            return None;
        }
        let failures = self
            .consecutive_failures_by_name
            .entry(call.name.clone())
            .or_insert(0);
        *failures = failures.saturating_add(1);
        let threshold = consecutive_failure_threshold(&call.name);
        if *failures < threshold {
            return None;
        }
        Some(format!(
            "Tool '{}' failed {failures} consecutive times in one response. CodeSeeX stopped the loop to avoid burning tokens on repeated failing tool retries.",
            call.name,
        ))
    }

    pub(crate) fn iteration_limit_error(&self) -> String {
        format!(
            "Tool loop exceeded {MAX_TOOL_LOOP_ITERATIONS} iterations in one response. CodeSeeX stopped the loop to avoid unbounded tool execution and upstream token usage."
        )
    }
}

fn consecutive_failure_threshold(tool_name: &str) -> u32 {
    if tool_name == "web_search" {
        2
    } else {
        3
    }
}

pub(crate) fn attach_tool_loop_warning(result: &mut Value, warning: &str) {
    if let Some(object) = result.as_object_mut() {
        object.insert(
            "_codeseex_tool_loop_warning".to_owned(),
            Value::String(warning.to_owned()),
        );
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

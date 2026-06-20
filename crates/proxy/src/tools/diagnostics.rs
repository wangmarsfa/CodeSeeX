use crate::tools::ownership::{ChatToolCall, ToolCallPartition};
use serde_json::{json, Value};
use std::collections::BTreeMap;

pub(crate) const MAX_TOOL_LOOP_ITERATIONS: u32 = 8;
pub(crate) const MAX_WEB_SEARCH_LOOP_CALLS: u32 = 3;

#[derive(Debug, Default)]
pub(crate) struct ToolLoopDiagnostics {
    seen_signatures: BTreeMap<String, u32>,
    consecutive_unsuccessful_by_name: BTreeMap<String, u32>,
}

#[derive(Debug, Clone)]
pub(crate) struct ToolLoopStop {
    pub(crate) message: String,
    pub(crate) recover_with_final_response: bool,
}

pub(crate) fn prepare_tool_loop_recovery_payload(
    payload: &mut Value,
    stop_message: &str,
) -> Result<(), &'static str> {
    if let Some(object) = payload.as_object_mut() {
        object.remove("tools");
        object.remove("tool_choice");
        object.remove("parallel_tool_calls");
    }
    let messages = payload
        .get_mut("messages")
        .and_then(Value::as_array_mut)
        .ok_or("chat payload messages were not an array during tool loop recovery")?;
    messages.push(json!({
        "role": "user",
        "content": format!(
            "CodeSeeX stopped the tool loop and removed tools for this recovery turn: {stop_message} Provide the final answer now using only the existing conversation evidence, returned tool results, and diagnostics. Do not request or encode additional tool calls in the response text. If the evidence is insufficient, say so briefly."
        )
    }));
    Ok(())
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
    ) -> Option<ToolLoopStop> {
        let health = tool_result_loop_health(call, result);
        if health == ToolResultLoopHealth::Success {
            self.consecutive_unsuccessful_by_name
                .insert(call.name.clone(), 0);
            return None;
        }
        let unsuccessful = self
            .consecutive_unsuccessful_by_name
            .entry(call.name.clone())
            .or_insert(0);
        *unsuccessful = unsuccessful.saturating_add(1);
        let threshold = consecutive_failure_threshold(&call.name);
        if *unsuccessful < threshold {
            return None;
        }
        Some(ToolLoopStop {
            message: tool_loop_stop_message(&call.name, health, *unsuccessful),
            recover_with_final_response: tool_loop_stop_is_recoverable(&call.name, health),
        })
    }

    pub(crate) fn iteration_limit_error(&self) -> String {
        format!(
            "Tool loop exceeded {MAX_TOOL_LOOP_ITERATIONS} iterations in one response. CodeSeeX stopped the loop to avoid unbounded tool execution and upstream token usage."
        )
    }

    pub(crate) fn web_search_budget_stop(&self) -> Option<ToolLoopStop> {
        let calls = self
            .seen_signatures
            .iter()
            .filter(|(signature, _)| signature.starts_with("web_search\n"))
            .map(|(_, count)| *count)
            .sum::<u32>();
        if calls < MAX_WEB_SEARCH_LOOP_CALLS {
            return None;
        }
        Some(ToolLoopStop {
            message: format!(
                "Tool 'web_search' reached its per-response budget of {MAX_WEB_SEARCH_LOOP_CALLS} calls. CodeSeeX stopped the loop to avoid burning tokens on repeated web searches."
            ),
            recover_with_final_response: true,
        })
    }
}

fn consecutive_failure_threshold(tool_name: &str) -> u32 {
    if tool_name == "web_search" {
        2
    } else {
        3
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ToolResultLoopHealth {
    Success,
    Failed,
    LowConfidenceFallback,
}

fn tool_result_loop_health(call: &ChatToolCall, result: &Value) -> ToolResultLoopHealth {
    if result.get("ok").and_then(Value::as_bool) == Some(false) || result.get("error").is_some() {
        return ToolResultLoopHealth::Failed;
    }
    if call.name == "web_search"
        && result
            .get("low_confidence_fallback")
            .and_then(Value::as_bool)
            == Some(true)
    {
        return ToolResultLoopHealth::LowConfidenceFallback;
    }
    ToolResultLoopHealth::Success
}

fn tool_loop_stop_message(
    tool_name: &str,
    health: ToolResultLoopHealth,
    unsuccessful: u32,
) -> String {
    match health {
        ToolResultLoopHealth::LowConfidenceFallback => format!(
            "Tool '{tool_name}' returned low-confidence fallback results {unsuccessful} consecutive times in one response. CodeSeeX stopped the loop to avoid burning tokens on repeated weak web search retries."
        ),
        ToolResultLoopHealth::Failed | ToolResultLoopHealth::Success => format!(
            "Tool '{tool_name}' failed {unsuccessful} consecutive times in one response. CodeSeeX stopped the loop to avoid burning tokens on repeated failing tool retries."
        ),
    }
}

fn tool_loop_stop_is_recoverable(tool_name: &str, health: ToolResultLoopHealth) -> bool {
    tool_name == "web_search"
        && matches!(
            health,
            ToolResultLoopHealth::Failed | ToolResultLoopHealth::LowConfidenceFallback
        )
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

#[cfg(test)]
mod tests {
    use super::*;

    fn call(name: &str) -> ChatToolCall {
        ChatToolCall {
            id: format!("call_{name}"),
            name: name.to_owned(),
            arguments: "{}".to_owned(),
        }
    }

    fn call_with_args(id: &str, name: &str, arguments: &str) -> ChatToolCall {
        ChatToolCall {
            id: id.to_owned(),
            name: name.to_owned(),
            arguments: arguments.to_owned(),
        }
    }

    #[test]
    fn web_search_budget_counts_distinct_searches() {
        let mut diagnostics = ToolLoopDiagnostics::default();
        let partition = ToolCallPartition {
            hosted: vec![call("web_search")],
            ..ToolCallPartition::default()
        };

        for index in 0..MAX_WEB_SEARCH_LOOP_CALLS {
            let call = call_with_args(
                &format!("call_web_{index}"),
                "web_search",
                &format!(r#"{{"query":"weather {index}"}}"#),
            );
            diagnostics.record_iteration(index + 1, &[call], &partition);
        }

        let stop = diagnostics
            .web_search_budget_stop()
            .expect("web_search budget should stop after bounded calls");
        assert!(stop.message.contains("per-response budget"));
        assert!(stop.recover_with_final_response);
    }

    #[test]
    fn non_web_calls_do_not_trigger_web_search_budget() {
        let mut diagnostics = ToolLoopDiagnostics::default();
        let partition = ToolCallPartition {
            code: vec![call("list_directory")],
            ..ToolCallPartition::default()
        };

        for index in 0..MAX_WEB_SEARCH_LOOP_CALLS {
            let call = call_with_args(
                &format!("call_ls_{index}"),
                "list_directory",
                &format!(r#"{{"path":"src/{index}"}}"#),
            );
            diagnostics.record_iteration(index + 1, &[call], &partition);
        }

        assert!(diagnostics.web_search_budget_stop().is_none());
    }

    #[test]
    fn web_search_low_confidence_fallback_counts_as_weak_loop_result() {
        let call = call("web_search");
        let mut diagnostics = ToolLoopDiagnostics::default();
        let result = json!({
            "ok": true,
            "low_confidence_fallback": true,
            "candidate_count": 3
        });

        assert!(diagnostics
            .record_tool_result_and_repeated_failure(&call, &result)
            .is_none());
        let error = diagnostics
            .record_tool_result_and_repeated_failure(&call, &result)
            .expect("second weak web_search should stop");

        assert!(error
            .message
            .contains("low-confidence fallback results 2 consecutive times"));
        assert!(error.message.contains("weak web search retries"));
        assert!(error.recover_with_final_response);
    }

    #[test]
    fn non_web_low_confidence_fallback_does_not_count_as_failure() {
        let call = call("read_file_range");
        let mut diagnostics = ToolLoopDiagnostics::default();
        let result = json!({
            "ok": true,
            "low_confidence_fallback": true
        });

        assert!(diagnostics
            .record_tool_result_and_repeated_failure(&call, &result)
            .is_none());
        assert!(diagnostics
            .record_tool_result_and_repeated_failure(&call, &result)
            .is_none());
        assert!(diagnostics
            .record_tool_result_and_repeated_failure(&call, &result)
            .is_none());
    }

    #[test]
    fn successful_web_search_resets_weak_loop_count() {
        let call = call("web_search");
        let mut diagnostics = ToolLoopDiagnostics::default();

        assert!(diagnostics
            .record_tool_result_and_repeated_failure(
                &call,
                &json!({ "ok": true, "low_confidence_fallback": true })
            )
            .is_none());
        assert!(diagnostics
            .record_tool_result_and_repeated_failure(
                &call,
                &json!({ "ok": true, "low_confidence_fallback": false })
            )
            .is_none());
        assert!(diagnostics
            .record_tool_result_and_repeated_failure(
                &call,
                &json!({ "ok": true, "low_confidence_fallback": true })
            )
            .is_none());
    }
}

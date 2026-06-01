use crate::tools::chat_protocol::{
    assistant_message_from_chat_tool_subset, chat_tool_calls,
    full_assistant_tool_message_from_chat, normalize_assistant_tool_message,
};
use crate::tools::hosted::{
    execute_code_tool, is_code_tool_executable, summarize_tool_result, tool_fact_line,
};
use crate::tools::ownership::{partition_tool_calls, proxy_executed_calls_in_order};
use codeseex_core::context::redact_inline_data_urls;
use codeseex_core::AppConfig;
use codeseex_store::Store;
use serde_json::{json, Value};

pub(crate) struct ToolLoopContext<'a> {
    pub(crate) client: &'a reqwest::Client,
    pub(crate) store: &'a Store,
    pub(crate) config: &'a AppConfig,
    pub(crate) auth: Option<&'a str>,
    pub(crate) request_id: &'a str,
    pub(crate) enabled_tools: &'a [String],
    pub(crate) community_tools: &'a crate::community_tools::CommunityToolSet,
    pub(crate) external_tool_context: &'a crate::tool_passthrough::ToolContext,
}

pub(crate) enum ToolLoopResult {
    FinalChat(Value),
    ClientToolCalls(Value),
}

pub(crate) async fn complete_chat_with_tools(
    context: ToolLoopContext<'_>,
    mut payload: Value,
    mut chat: Value,
) -> Result<ToolLoopResult, String> {
    let mut completed_tool_iterations = 0_u32;
    loop {
        let iteration = completed_tool_iterations;
        let tool_calls = chat_tool_calls(&chat);
        if tool_calls.is_empty() {
            return Ok(ToolLoopResult::FinalChat(chat));
        }
        let partition = partition_tool_calls(
            tool_calls.clone(),
            context.community_tools,
            context.external_tool_context,
        );
        if let Some(unknown) = partition.unknown.first() {
            return Err(format!(
                "tool '{}' is not available to CodeSeeX Next or Codex",
                unknown.name
            ));
        }
        let proxy_executed_calls = proxy_executed_calls_in_order(&tool_calls, &partition);
        if let Some(disabled) = proxy_executed_calls.iter().find(|call| {
            !is_code_tool_executable(&call.name, context.enabled_tools, context.community_tools)
        }) {
            return Err(format!(
                "tool '{}' is not enabled or not executable by CodeSeeX Next",
                disabled.name
            ));
        }
        let has_client_tools = partition.has_client_executed_calls();
        if has_client_tools && !partition.has_proxy_executed_calls() {
            let stored_assistant = full_assistant_tool_message_from_chat(&chat)?;
            context
                .store
                .append_request_turn_messages(context.request_id, &[stored_assistant])
                .await
                .map_err(|error| format!("failed to persist client tool turn message: {error}"))?;
            return Ok(ToolLoopResult::ClientToolCalls(chat));
        }
        if has_client_tools {
            let _ = context
                .store
                .record_event(
                    "info",
                    "mixed_tool_turn_split",
                    "Mixed CodeSeeX and native Codex tool calls were split; CodeSeeX tools will run first.",
                    Some(&json!({
                        "id": context.request_id,
                        "code_tools": partition.code.iter().map(|call| call.name.as_str()).collect::<Vec<_>>(),
                        "hosted_tools": partition.hosted.iter().map(|call| call.name.as_str()).collect::<Vec<_>>(),
                        "native_tools": partition.native.iter().map(|call| call.name.as_str()).collect::<Vec<_>>(),
                        "external_tools": partition.external.iter().map(|call| call.name.as_str()).collect::<Vec<_>>(),
                        "iteration": iteration + 1
                    })),
                )
                .await;
        }
        let messages = payload
            .get_mut("messages")
            .and_then(Value::as_array_mut)
            .ok_or_else(|| "chat payload messages were not an array".to_owned())?;
        let stored_assistant = full_assistant_tool_message_from_chat(&chat)?;
        let assistant_message = if has_client_tools {
            assistant_message_from_chat_tool_subset(&chat, &proxy_executed_calls)
        } else {
            chat.pointer("/choices/0/message")
                .cloned()
                .ok_or_else(|| "tool call response did not include an assistant message".to_owned())
                .map(normalize_assistant_tool_message)?
        };
        context
            .store
            .append_request_turn_messages(context.request_id, &[stored_assistant])
            .await
            .map_err(|error| format!("failed to persist assistant tool turn message: {error}"))?;
        messages.push(assistant_message);
        for call in proxy_executed_calls {
            let _ = context
                .store
                .record_event(
                    "info",
                    "tool_call",
                    "CodeSeeX tool requested.",
                    Some(&json!({
                        "id": context.request_id,
                        "call_id": call.id,
                        "name": call.name,
                        "iteration": iteration + 1
                    })),
                )
                .await;
            let result = execute_code_tool(context.client, context.community_tools, &call).await;
            let result_text = serde_json::to_string(&result).unwrap_or_else(|_| "{}".to_owned());
            let result_text = redact_inline_data_urls(&result_text);
            let fact = tool_fact_line(&call, &result);
            context
                .store
                .append_request_tool_fact(context.request_id, &fact)
                .await
                .map_err(|error| format!("failed to persist tool fact: {error}"))?;
            let _ = context
                .store
                .record_event(
                    "info",
                    "tool_result",
                    "CodeSeeX tool result returned.",
                    Some(&json!({
                        "id": context.request_id,
                        "call_id": call.id,
                        "name": call.name,
                        "iteration": iteration + 1,
                        "ok": result.get("ok").and_then(Value::as_bool),
                        "summary": summarize_tool_result(&result)
                    })),
                )
                .await;
            let tool_message = json!({
                "role": "tool",
                "tool_call_id": call.id,
                "content": result_text
            });
            context
                .store
                .append_request_turn_messages(
                    context.request_id,
                    std::slice::from_ref(&tool_message),
                )
                .await
                .map_err(|error| format!("failed to persist tool result turn message: {error}"))?;
            messages.push(tool_message);
        }
        if has_client_tools {
            return Ok(ToolLoopResult::ClientToolCalls(chat));
        }
        completed_tool_iterations += 1;
        let response = crate::upstream::post_chat_completions(
            context.client,
            &context.config.upstream,
            context.auth,
            payload.clone(),
        )
        .await
        .map_err(|error| error.to_string())?;
        let status = response.status();
        if !status.is_success() {
            let body = response
                .text()
                .await
                .unwrap_or_else(|error| error.to_string());
            return Err(format!(
                "upstream returned {status} after tool execution: {body}"
            ));
        }
        chat = response
            .json::<Value>()
            .await
            .map_err(|error| error.to_string())?;
    }
}

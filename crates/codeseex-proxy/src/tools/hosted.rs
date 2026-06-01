use crate::text::compact_line;
use crate::tools::ownership::ChatToolCall;
use codeseex_core::context::redact_inline_data_urls;
use serde_json::Value;

pub(crate) fn is_code_tool_executable(
    name: &str,
    enabled_tools: &[String],
    community_tools: &crate::community_tools::CommunityToolSet,
) -> bool {
    crate::tools::is_executable_tool_enabled(name, enabled_tools)
        || community_tools.is_executable_tool(name)
}

pub(crate) async fn execute_code_tool(
    client: &reqwest::Client,
    community_tools: &crate::community_tools::CommunityToolSet,
    call: &ChatToolCall,
) -> Value {
    if let Some(result) = community_tools.execute(&call.name, &call.arguments).await {
        return result;
    }
    crate::tools::execute_tool_with_client(client, &call.name, &call.arguments).await
}

pub(crate) fn tool_fact_line(call: &ChatToolCall, result: &Value) -> String {
    format!(
        "tool={} call_id={} arguments={} ok={} result={}",
        call.name,
        call.id,
        compact_line(&redact_inline_data_urls(&call.arguments), 800),
        result
            .get("ok")
            .and_then(Value::as_bool)
            .map(|value| value.to_string())
            .unwrap_or_else(|| "unknown".to_owned()),
        summarize_tool_result(result)
    )
}

pub(crate) fn summarize_tool_result(result: &Value) -> String {
    let text = serde_json::to_string(result).unwrap_or_else(|_| "{}".to_owned());
    compact_line(&redact_inline_data_urls(&text), 2400)
}

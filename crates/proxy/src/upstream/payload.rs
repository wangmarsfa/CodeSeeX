use codeseex_core::{AppConfig, ModelRouteHint, UserConfig};
use serde_json::{json, Value};

const LIGHTWEIGHT_MAX_INPUT_ITEMS: usize = 6;
const LIGHTWEIGHT_MAX_TEXT_CHARS: usize = 8_000;
const LIGHTWEIGHT_MAX_OUTPUT_TOKENS: u64 = 1_024;
const TEXT_SCAN_CHAR_LIMIT: usize = 32_000;

pub(crate) fn normalize_chat_payload(config: &AppConfig, request: &Value, payload: &mut Value) {
    if let Some(model) = payload
        .get("model")
        .and_then(Value::as_str)
        .map(str::to_owned)
    {
        payload["model"] = Value::String(resolve_upstream_model(config, request, &model));
    }
    if let Some(temperature) = config.temperature.value() {
        payload["temperature"] = json!(temperature);
    } else if let Some(temperature) = request.get("temperature").and_then(Value::as_f64) {
        payload["temperature"] = json!(temperature);
    }
    if let Some(top_p) = request.get("top_p").and_then(Value::as_f64) {
        payload["top_p"] = json!(top_p);
    }
    if let Some(max_tokens) = request
        .get("max_output_tokens")
        .or_else(|| request.get("max_completion_tokens"))
        .and_then(value_to_u64)
    {
        payload["max_tokens"] = json!(max_tokens);
    }
    if let Some(response_format) = response_format_from_request(request) {
        payload["response_format"] = response_format;
    }
    if let Some(thinking) = thinking_from_request(config, request) {
        payload["thinking"] = thinking;
    }
    if payload
        .get("stream")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        payload["stream_options"] = json!({ "include_usage": true });
    }
}

pub(crate) fn resolve_upstream_model(
    config: &AppConfig,
    request: &Value,
    requested: &str,
) -> String {
    config
        .model_override
        .upstream_slug_with_hint(requested, model_route_hint_from_request(request))
}

pub(crate) fn model_route_hint_from_request(request: &Value) -> ModelRouteHint {
    if request_looks_like_lightweight_codex_task(request) {
        ModelRouteHint::Lightweight
    } else {
        ModelRouteHint::Default
    }
}

pub(crate) fn request_is_lightweight_auxiliary(request: &Value) -> bool {
    request_looks_like_lightweight_codex_task(request)
}

pub(crate) fn request_shape_diagnostic(request: &Value) -> Value {
    let shape = RequestShape::from_request(request);
    json!({
        "model_route_hint": model_route_hint_label(model_route_hint_from_request(request)),
        "lightweight_auxiliary": request_is_lightweight_auxiliary(request),
        "has_previous_response_id": shape.has_previous_response_id,
        "has_instructions": shape.has_instructions,
        "has_context_management": shape.has_context_management,
        "input_items": shape.input_items,
        "input_kind": shape.input_kind,
        "estimated_text_chars": shape.estimated_text_chars,
        "tools_count": shape.tools_count,
        "max_output_tokens": shape.max_output_tokens,
        "reasoning_effort": shape.reasoning_effort,
        "text_format": shape.text_format,
        "store": shape.store,
        "metadata_keys": shape.metadata_keys,
        "client_metadata": shape.client_metadata,
        "prompt_cache_key": shape.prompt_cache_key,
        "has_title_task_signal": shape.has_title_task_signal,
        "has_suggestion_task_signal": shape.has_suggestion_task_signal
    })
}

fn request_looks_like_lightweight_codex_task(request: &Value) -> bool {
    let requested = request
        .get("model")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if !is_codex_native_gpt_model(requested) {
        return false;
    }

    let shape = RequestShape::from_request(request);
    if shape.has_previous_response_id || shape.has_context_management || shape.tools_count > 0 {
        return false;
    }
    if shape.input_items > LIGHTWEIGHT_MAX_INPUT_ITEMS
        || shape.estimated_text_chars > LIGHTWEIGHT_MAX_TEXT_CHARS
    {
        return false;
    }
    if shape
        .max_output_tokens
        .is_some_and(|value| value > LIGHTWEIGHT_MAX_OUTPUT_TOKENS)
    {
        return false;
    }

    if shape.has_auxiliary_task_signal() {
        return true;
    }

    is_known_codex_lightweight_model(requested) && shape.input_items <= LIGHTWEIGHT_MAX_INPUT_ITEMS
}

#[derive(Debug)]
struct RequestShape {
    has_previous_response_id: bool,
    has_instructions: bool,
    has_context_management: bool,
    input_items: usize,
    input_kind: &'static str,
    estimated_text_chars: usize,
    tools_count: usize,
    max_output_tokens: Option<u64>,
    reasoning_effort: Option<String>,
    text_format: Option<String>,
    store: Option<bool>,
    metadata_keys: Vec<String>,
    client_metadata: bool,
    prompt_cache_key: bool,
    has_title_task_signal: bool,
    has_suggestion_task_signal: bool,
}

impl RequestShape {
    fn from_request(request: &Value) -> Self {
        let input = request.get("input").unwrap_or(&Value::Null);
        let instructions = request.get("instructions").unwrap_or(&Value::Null);
        let metadata = request.get("metadata").unwrap_or(&Value::Null);
        Self {
            has_previous_response_id: request
                .get("previous_response_id")
                .and_then(Value::as_str)
                .map(|value| !value.trim().is_empty())
                .unwrap_or(false),
            has_instructions: !instructions.is_null(),
            has_context_management: request.get("context_management").is_some(),
            input_items: input_item_count(input),
            input_kind: value_kind(input),
            estimated_text_chars: estimate_text_chars(input)
                .saturating_add(estimate_text_chars(instructions)),
            tools_count: tools_count(request.get("tools")),
            max_output_tokens: request
                .get("max_output_tokens")
                .or_else(|| request.get("max_completion_tokens"))
                .and_then(value_to_u64),
            reasoning_effort: request
                .pointer("/reasoning/effort")
                .and_then(Value::as_str)
                .map(str::to_owned),
            text_format: request
                .pointer("/text/format/type")
                .and_then(Value::as_str)
                .map(str::to_owned),
            store: request.get("store").and_then(Value::as_bool),
            metadata_keys: object_keys(metadata),
            client_metadata: request.get("client_metadata").is_some(),
            prompt_cache_key: request.get("prompt_cache_key").is_some(),
            has_title_task_signal: value_has_title_task_signal(instructions)
                || value_has_title_task_signal(input)
                || value_has_title_task_signal(metadata),
            has_suggestion_task_signal: value_has_suggestion_task_signal(instructions)
                || value_has_suggestion_task_signal(input)
                || value_has_suggestion_task_signal(metadata),
        }
    }

    fn has_auxiliary_task_signal(&self) -> bool {
        self.has_title_task_signal || self.has_suggestion_task_signal
    }
}

fn model_route_hint_label(value: ModelRouteHint) -> &'static str {
    match value {
        ModelRouteHint::Default => "default",
        ModelRouteHint::Lightweight => "lightweight",
    }
}

fn is_codex_native_gpt_model(value: &str) -> bool {
    let value = value.trim().to_ascii_lowercase();
    value == "gpt-5" || value.starts_with("gpt-5.") || value.starts_with("gpt-5-")
}

fn is_known_codex_lightweight_model(value: &str) -> bool {
    matches!(
        value.trim().to_ascii_lowercase().as_str(),
        "gpt-5.4-mini" | "gpt-5.5-mini"
    )
}

fn input_item_count(value: &Value) -> usize {
    match value {
        Value::Null => 0,
        Value::Array(items) => items.len(),
        _ => 1,
    }
}

fn value_kind(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "bool",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

fn tools_count(value: Option<&Value>) -> usize {
    match value {
        Some(Value::Array(items)) => items.len(),
        Some(Value::Object(object)) if !object.is_empty() => 1,
        Some(Value::String(value)) if !value.trim().is_empty() => 1,
        _ => 0,
    }
}

fn object_keys(value: &Value) -> Vec<String> {
    value
        .as_object()
        .map(|object| object.keys().take(24).cloned().collect())
        .unwrap_or_default()
}

fn estimate_text_chars(value: &Value) -> usize {
    estimate_text_chars_inner(value, 0, &mut 0)
}

fn estimate_text_chars_inner(value: &Value, depth: usize, scanned: &mut usize) -> usize {
    if depth > 8 || *scanned >= TEXT_SCAN_CHAR_LIMIT {
        return 0;
    }
    match value {
        Value::String(text) => {
            let remaining = TEXT_SCAN_CHAR_LIMIT.saturating_sub(*scanned);
            let count = text.chars().take(remaining).count();
            *scanned = (*scanned).saturating_add(count);
            count
        }
        Value::Array(items) => items
            .iter()
            .take(128)
            .map(|item| estimate_text_chars_inner(item, depth + 1, scanned))
            .sum(),
        Value::Object(object) => object
            .values()
            .take(128)
            .map(|item| estimate_text_chars_inner(item, depth + 1, scanned))
            .sum(),
        _ => 0,
    }
}

fn value_has_title_task_signal(value: &Value) -> bool {
    value_has_title_task_signal_inner(value, 0, &mut 0)
}

fn value_has_title_task_signal_inner(value: &Value, depth: usize, scanned: &mut usize) -> bool {
    if depth > 8 || *scanned >= TEXT_SCAN_CHAR_LIMIT {
        return false;
    }
    match value {
        Value::String(text) => {
            let remaining = TEXT_SCAN_CHAR_LIMIT.saturating_sub(*scanned);
            let snippet = text.chars().take(remaining).collect::<String>();
            *scanned = (*scanned).saturating_add(snippet.chars().count());
            text_has_title_task_signal(&snippet)
        }
        Value::Array(items) => items
            .iter()
            .take(128)
            .any(|item| value_has_title_task_signal_inner(item, depth + 1, scanned)),
        Value::Object(object) => object.iter().take(128).any(|(key, item)| {
            text_has_title_task_signal(key)
                || value_has_title_task_signal_inner(item, depth + 1, scanned)
        }),
        _ => false,
    }
}

fn text_has_title_task_signal(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    let title_word = lower.contains("title")
        || lower.contains("headline")
        || lower.contains("rename")
        || text.contains("标题")
        || text.contains("话题")
        || text.contains("命名");
    if !title_word {
        return false;
    }
    lower.contains("conversation")
        || lower.contains("chat")
        || lower.contains("thread")
        || lower.contains("session")
        || lower.contains("transcript")
        || lower.contains("summarize")
        || lower.contains("short")
        || text.contains("对话")
        || text.contains("会话")
        || text.contains("聊天")
        || text.contains("线程")
}

fn value_has_suggestion_task_signal(value: &Value) -> bool {
    value_has_suggestion_task_signal_inner(value, 0, &mut 0)
}

fn value_has_suggestion_task_signal_inner(
    value: &Value,
    depth: usize,
    scanned: &mut usize,
) -> bool {
    if depth > 8 || *scanned >= TEXT_SCAN_CHAR_LIMIT {
        return false;
    }
    match value {
        Value::String(text) => {
            let remaining = TEXT_SCAN_CHAR_LIMIT.saturating_sub(*scanned);
            let snippet = text.chars().take(remaining).collect::<String>();
            *scanned = (*scanned).saturating_add(snippet.chars().count());
            text_has_suggestion_task_signal(&snippet)
        }
        Value::Array(items) => items
            .iter()
            .take(128)
            .any(|item| value_has_suggestion_task_signal_inner(item, depth + 1, scanned)),
        Value::Object(object) => object.iter().take(128).any(|(key, item)| {
            text_has_suggestion_task_signal(key)
                || value_has_suggestion_task_signal_inner(item, depth + 1, scanned)
        }),
        _ => false,
    }
}

fn text_has_suggestion_task_signal(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    let suggestion_word = lower.contains("suggest")
        || lower.contains("recommend")
        || lower.contains("suggestion")
        || lower.contains("recommendation")
        || lower.contains("starter")
        || text.contains("建议")
        || text.contains("推荐")
        || text.contains("起始");
    if !suggestion_word {
        return false;
    }
    let prompt_context = lower.contains("prompt")
        || lower.contains("starter")
        || lower.contains("conversation starter")
        || text.contains("提示")
        || text.contains("开场");
    let conversation_context = lower.contains("conversation")
        || lower.contains("chat")
        || lower.contains("thread")
        || lower.contains("topic")
        || text.contains("对话")
        || text.contains("会话")
        || text.contains("聊天")
        || text.contains("话题");
    let workspace_context = lower.contains("workspace")
        || lower.contains("project")
        || text.contains("工作区")
        || text.contains("项目");
    let new_topic_context = lower.contains("new conversation")
        || lower.contains("new chat")
        || lower.contains("new topic")
        || lower.contains("create conversation")
        || lower.contains("start conversation")
        || lower.contains("start a chat")
        || text.contains("新对话")
        || text.contains("新话题")
        || text.contains("新建")
        || text.contains("创建");

    prompt_context
        || (conversation_context && (workspace_context || new_topic_context))
        || (workspace_context && new_topic_context)
}

fn response_format_from_request(request: &Value) -> Option<Value> {
    let format_type = request
        .pointer("/text/format/type")
        .and_then(Value::as_str)
        .unwrap_or_default();
    match format_type {
        "json_object" | "json_schema" => Some(json!({ "type": "json_object" })),
        _ => None,
    }
}

fn thinking_from_request(config: &AppConfig, request: &Value) -> Option<Value> {
    let forced = UserConfig::read_from(&config.config_path())
        .ok()
        .and_then(|user_config| user_config.model.and_then(|model| model.thinking))
        .unwrap_or_else(|| "auto".to_owned())
        .trim()
        .to_ascii_lowercase();
    if forced == "enabled" || forced == "on" {
        return Some(json!({ "type": "enabled" }));
    }
    if forced == "disabled" || forced == "off" {
        return Some(json!({ "type": "disabled" }));
    }
    let effort = request
        .pointer("/reasoning/effort")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if effort == "none" {
        Some(json!({ "type": "disabled" }))
    } else if !effort.is_empty() {
        Some(json!({ "type": "enabled" }))
    } else {
        None
    }
}

fn value_to_u64(value: &Value) -> Option<u64> {
    value
        .as_u64()
        .or_else(|| value.as_i64().and_then(|value| u64::try_from(value).ok()))
        .or_else(|| value.as_f64().map(|value| value.max(0.0).floor() as u64))
}

#[cfg(test)]
mod tests {
    use super::*;
    use codeseex_core::models::{MODEL_FLASH, MODEL_PRO};

    #[test]
    fn title_shaped_codex_gpt_request_routes_to_flash() {
        let config = AppConfig::default();
        let request = json!({
            "model": "gpt-5.4",
            "input": "Generate a short conversation title for: user asked to test visual tools.",
            "max_output_tokens": 32
        });

        assert_eq!(
            resolve_upstream_model(&config, &request, "gpt-5.4"),
            MODEL_FLASH
        );
        assert_eq!(
            request_shape_diagnostic(&request)["model_route_hint"],
            "lightweight"
        );
    }

    #[test]
    fn suggestion_shaped_codex_gpt_request_routes_to_flash() {
        let config = AppConfig::default();
        let request = json!({
            "model": "gpt-5.4",
            "instructions": "Generate suggested starter prompts for a new project workspace conversation.",
            "input": "The user opened a new project workspace topic.",
            "max_output_tokens": 128
        });

        assert_eq!(
            resolve_upstream_model(&config, &request, "gpt-5.4"),
            MODEL_FLASH
        );
        assert_eq!(
            request_shape_diagnostic(&request)["lightweight_auxiliary"],
            true
        );
        assert_eq!(
            request_shape_diagnostic(&request)["has_suggestion_task_signal"],
            true
        );
    }

    #[test]
    fn ordinary_project_recommendation_does_not_route_as_auxiliary() {
        let config = AppConfig::default();
        let request = json!({
            "model": "gpt-5.4",
            "input": "Recommend improvements for this Rust project.",
            "max_output_tokens": 512
        });

        assert_eq!(
            resolve_upstream_model(&config, &request, "gpt-5.4"),
            MODEL_PRO
        );
        assert_eq!(
            request_shape_diagnostic(&request)["lightweight_auxiliary"],
            false
        );
    }

    #[test]
    fn non_title_codex_gpt_request_uses_default_pro_fallback() {
        let config = AppConfig::default();
        let request = json!({
            "model": "gpt-5.4",
            "input": "Write a small HTML file.",
            "max_output_tokens": 512
        });

        assert_eq!(
            resolve_upstream_model(&config, &request, "gpt-5.4"),
            MODEL_PRO
        );
    }

    #[test]
    fn non_title_codex_gpt_request_respects_configured_flash_override() {
        let config = AppConfig {
            model_override: codeseex_core::models::UpstreamModelOverride::Flash,
            ..AppConfig::default()
        };
        let request = json!({
            "model": "gpt-5.4",
            "input": "Write a small HTML file.",
            "max_output_tokens": 512
        });

        assert_eq!(
            resolve_upstream_model(&config, &request, "gpt-5.4"),
            MODEL_FLASH
        );
    }

    #[test]
    fn tool_requests_do_not_downshift_to_flash_even_with_title_text() {
        let config = AppConfig::default();
        let request = json!({
            "model": "gpt-5.4",
            "input": "Update the page title.",
            "max_output_tokens": 32,
            "tools": [{
                "type": "function",
                "function": { "name": "apply_patch" }
            }]
        });

        assert_eq!(
            resolve_upstream_model(&config, &request, "gpt-5.4"),
            MODEL_PRO
        );
    }

    #[test]
    fn arbitrary_mini_suffix_is_not_the_primary_classifier() {
        let config = AppConfig::default();
        let request = json!({
            "model": "gpt-5.6-mini",
            "input": "Write a small HTML file.",
            "max_output_tokens": 512
        });

        assert_eq!(
            resolve_upstream_model(&config, &request, "gpt-5.6-mini"),
            MODEL_PRO
        );
    }
}

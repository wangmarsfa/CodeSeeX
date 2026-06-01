use codeseex_core::{AppConfig, UserConfig};
use serde_json::{json, Value};

pub(crate) fn normalize_chat_payload(config: &AppConfig, request: &Value, payload: &mut Value) {
    if let Some(model) = payload
        .get("model")
        .and_then(Value::as_str)
        .map(str::to_owned)
    {
        payload["model"] = Value::String(config.model_override.upstream_slug(&model));
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

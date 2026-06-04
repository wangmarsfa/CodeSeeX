use serde_json::{json, Value};

pub(crate) fn response_usage_from_chat_usage(usage: Option<&Value>) -> Value {
    let usage = usage.unwrap_or(&Value::Null);
    let (input, cached, cache_miss, output, reasoning, total) = response_usage_components(usage);
    json!({
        "input_tokens": input,
        "cached_input_tokens": cached,
        "cache_miss_input_tokens": cache_miss,
        "input_tokens_details": { "cached_tokens": cached },
        "output_tokens": output,
        "reasoning_output_tokens": reasoning,
        "output_tokens_details": { "reasoning_tokens": reasoning },
        "total_tokens": total
    })
}

pub(crate) fn merge_response_usage(left: &Value, right: &Value) -> Value {
    let (left_input, left_cached, left_miss, left_output, left_reasoning, left_total) =
        response_usage_components(left);
    let (right_input, right_cached, right_miss, right_output, right_reasoning, right_total) =
        response_usage_components(right);
    json!({
        "input_tokens": left_input.saturating_add(right_input),
        "cached_input_tokens": left_cached.saturating_add(right_cached),
        "cache_miss_input_tokens": left_miss.saturating_add(right_miss),
        "input_tokens_details": {
            "cached_tokens": left_cached.saturating_add(right_cached)
        },
        "output_tokens": left_output.saturating_add(right_output),
        "reasoning_output_tokens": left_reasoning.saturating_add(right_reasoning),
        "output_tokens_details": {
            "reasoning_tokens": left_reasoning.saturating_add(right_reasoning)
        },
        "total_tokens": left_total.saturating_add(right_total)
    })
}

fn response_usage_components(usage: &Value) -> (u64, u64, u64, u64, u64, u64) {
    let input = usage_field(usage, &["input_tokens", "prompt_tokens"]).unwrap_or(0);
    let cached = usage_field(
        usage,
        &[
            "cached_input_tokens",
            "cache_hit_input_tokens",
            "prompt_cache_hit_tokens",
            "cache_hit_tokens",
        ],
    )
    .or_else(|| usage_pointer(usage, "/input_tokens_details/cached_tokens"))
    .or_else(|| usage_pointer(usage, "/prompt_tokens_details/cached_tokens"))
    .unwrap_or(0);
    let cache_miss = usage_field(
        usage,
        &[
            "cache_miss_input_tokens",
            "input_cache_miss_tokens",
            "prompt_cache_miss_tokens",
            "cache_miss_tokens",
        ],
    )
    .unwrap_or_else(|| input.saturating_sub(cached));
    let output = usage_field(usage, &["output_tokens", "completion_tokens"]).unwrap_or(0);
    let reasoning = usage_field(usage, &["reasoning_output_tokens"])
        .or_else(|| usage_pointer(usage, "/output_tokens_details/reasoning_tokens"))
        .or_else(|| usage_pointer(usage, "/completion_tokens_details/reasoning_tokens"))
        .unwrap_or(0);
    let total =
        usage_field(usage, &["total_tokens"]).unwrap_or_else(|| input.saturating_add(output));
    (input, cached, cache_miss, output, reasoning, total)
}

fn usage_field(usage: &Value, keys: &[&str]) -> Option<u64> {
    keys.iter()
        .filter_map(|key| usage.get(*key))
        .find_map(value_to_u64)
}

fn usage_pointer(usage: &Value, pointer: &str) -> Option<u64> {
    usage.pointer(pointer).and_then(value_to_u64)
}

fn value_to_u64(value: &Value) -> Option<u64> {
    value
        .as_u64()
        .or_else(|| value.as_i64().and_then(|number| u64::try_from(number).ok()))
        .or_else(|| {
            value
                .as_f64()
                .filter(|number| number.is_finite() && *number >= 0.0)
                .map(|number| number as u64)
        })
}

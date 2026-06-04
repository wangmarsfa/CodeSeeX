use serde_json::Value;
use std::collections::HashSet;

use super::extract::compact_whitespace;
use super::safety::normalize_candidate_url;
use super::{MAX_OPEN_TARGETS, MAX_QUERIES};

pub(super) fn mode(args: &Value) -> String {
    args.get("mode")
        .or_else(|| args.get("type"))
        .and_then(Value::as_str)
        .map(|value| value.trim().to_ascii_lowercase())
        .filter(|value| value == "search" || value == "open")
        .unwrap_or_else(|| "search".to_owned())
}

pub(super) fn queries(args: &Value) -> Vec<String> {
    let mut values = Vec::new();
    push_query_values(args.get("queries"), &mut values);
    push_string_or_array(args.get("search_query"), &mut values);
    push_string_or_array(args.get("query"), &mut values);
    push_string_or_array(args.get("q"), &mut values);
    clean_deduped_strings(values, MAX_QUERIES, 300)
}

pub(super) fn open_targets(args: &Value) -> Vec<String> {
    let mut values = Vec::new();
    push_string_or_array(args.get("open_urls"), &mut values);
    push_string_or_array(args.get("urls"), &mut values);
    push_string_or_array(args.get("url"), &mut values);
    if values.is_empty() {
        for query in queries(args) {
            if normalize_candidate_url(&query).is_some() {
                values.push(query);
            }
        }
    }
    clean_deduped_strings(values, MAX_OPEN_TARGETS, 2048)
        .into_iter()
        .filter_map(|value| normalize_candidate_url(&value))
        .collect()
}

pub(super) fn open_ids(args: &Value) -> Vec<String> {
    let mut values = Vec::new();
    push_string_or_array(args.get("open_ids"), &mut values);
    push_string_or_array(args.get("ids"), &mut values);
    push_string_or_array(args.get("id"), &mut values);
    clean_deduped_strings(values, MAX_OPEN_TARGETS, 128)
}

fn push_query_values(value: Option<&Value>, output: &mut Vec<String>) {
    match value {
        Some(Value::Array(items)) => {
            for item in items {
                if let Some(text) = item
                    .as_str()
                    .or_else(|| item.get("q").and_then(Value::as_str))
                    .or_else(|| item.get("query").and_then(Value::as_str))
                {
                    output.push(text.to_owned());
                }
            }
        }
        other => push_string_or_array(other, output),
    }
}

fn push_string_or_array(value: Option<&Value>, output: &mut Vec<String>) {
    match value {
        Some(Value::String(text)) => {
            for line in text.split(['\n', '\r']) {
                output.push(line.to_owned());
            }
        }
        Some(Value::Array(items)) => {
            for item in items {
                if let Some(text) = item.as_str() {
                    output.push(text.to_owned());
                }
            }
        }
        _ => {}
    }
}

fn clean_deduped_strings(values: Vec<String>, max_items: usize, max_chars: usize) -> Vec<String> {
    let mut seen = HashSet::new();
    let mut output = Vec::new();
    for value in values {
        let cleaned = compact_whitespace(&value)
            .chars()
            .take(max_chars)
            .collect::<String>();
        if cleaned.is_empty() {
            continue;
        }
        let key = cleaned.to_ascii_lowercase();
        if seen.insert(key) {
            output.push(cleaned);
            if output.len() >= max_items {
                break;
            }
        }
    }
    output
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn normalizes_legacy_argument_shapes() {
        let args = json!({
            "type": "search",
            "queries": ["CodeSeeX GitHub", { "q": "DeepSeek V4 pricing" }],
            "q": "ignored after max"
        });
        assert_eq!(mode(&args), "search");
        assert_eq!(
            queries(&args),
            vec![
                "CodeSeeX GitHub",
                "DeepSeek V4 pricing",
                "ignored after max"
            ]
        );

        let open = json!({
            "type": "open",
            "open_urls": ["example.com/a"],
            "open_ids": ["cand_abc"],
            "id": "cand_def"
        });
        assert_eq!(mode(&open), "open");
        assert_eq!(open_targets(&open), vec!["https://example.com/a"]);
        assert_eq!(open_ids(&open), vec!["cand_abc", "cand_def"]);
    }
}

mod candidates;
mod extract;
mod input;
mod net;
mod open;
mod safety;
mod search;

use codeseex_core::NetworkProxyMode;
use futures_util::future::join_all;
use serde_json::{json, Value};
use std::time::Duration;

const MAX_BYTES: u64 = 524_288;
const MAX_TEXT_CHARS: usize = 12_000;
const MAX_RESULTS: usize = 8;
const MAX_QUERIES: usize = 3;
const MAX_OPEN_TARGETS: usize = 6;
const WEB_SEARCH_TOTAL_TIMEOUT_SECS: u64 = 15;

pub(crate) async fn warm_search_sources(proxy_mode: NetworkProxyMode) -> Value {
    let web_client = net::web_client(proxy_mode);
    search::warm_sources(&web_client, proxy_mode).await
}

pub(crate) async fn execute(
    _client: &reqwest::Client,
    proxy_mode: NetworkProxyMode,
    arguments: &Value,
    messages: &[Value],
) -> Value {
    let action_mode = action_mode(arguments);
    match tokio::time::timeout(
        Duration::from_secs(WEB_SEARCH_TOTAL_TIMEOUT_SECS),
        execute_inner(proxy_mode, arguments, messages),
    )
    .await
    {
        Ok(result) => result,
        Err(_) => json!({
            "ok": false,
            "stage": action_mode,
            "mode": action_mode,
            "error": "web_search_timeout",
            "message": "web_search exceeded the bounded execution time. Try a narrower query, fewer URLs, or retry when network connectivity is stable.",
            "timeout_seconds": WEB_SEARCH_TOTAL_TIMEOUT_SECS
        }),
    }
}

async fn execute_inner(
    proxy_mode: NetworkProxyMode,
    arguments: &Value,
    messages: &[Value],
) -> Value {
    let mode = input::mode(arguments);
    let urls = input::open_targets(arguments);
    let open_ids = input::open_ids(arguments);
    if mode == "open" || !urls.is_empty() || !open_ids.is_empty() {
        let mut targets = urls;
        let lookup = candidates::lookup_from_messages(messages);
        let unresolved_ids = candidates::resolve_open_ids(&open_ids, &lookup, &mut targets);
        return open::many(proxy_mode, &targets, &open_ids, &unresolved_ids).await;
    }

    let queries = input::queries(arguments);
    if queries.is_empty() {
        return json!({
            "ok": false,
            "error": "missing_query",
            "message": "web_search requires query/search_query/queries for mode=search or url/urls/open_urls/open_ids for mode=open."
        });
    }

    let max_results = usize_arg(arguments, "max_results", 5, 1, MAX_RESULTS);
    let mut per_query = Vec::new();
    let mut all_results = Vec::new();
    let mut fallback_errors = Vec::new();
    let mut source_diagnostics = Vec::new();
    let mut source_order = Vec::new();
    let mut sources_deprioritized = Vec::new();
    let mut source_health = Value::Array(Vec::new());
    let mut has_low_confidence_fallback_candidates = false;
    let web_client = net::web_client(proxy_mode);
    let query_results = join_all(
        queries
            .iter()
            .take(MAX_QUERIES)
            .map(|query| search::query(&web_client, proxy_mode, query, max_results)),
    )
    .await;
    let mut sources_attempted = Vec::new();
    for result in query_results {
        if let Some(results) = result.get("results").and_then(Value::as_array) {
            all_results.extend(results.iter().cloned());
        }
        if let Some(errors) = result.get("fallback_errors").and_then(Value::as_array) {
            fallback_errors.extend(errors.iter().cloned());
        }
        if let Some(diagnostics) = result.get("source_diagnostics").and_then(Value::as_array) {
            source_diagnostics.extend(diagnostics.iter().cloned());
        }
        if let Some(sources) = result.get("sources_attempted").and_then(Value::as_array) {
            for source in sources.iter().filter_map(Value::as_str) {
                if !sources_attempted.iter().any(|value| value == source) {
                    sources_attempted.push(source.to_owned());
                }
            }
        }
        if source_order.is_empty() {
            if let Some(sources) = result.get("source_order").and_then(Value::as_array) {
                source_order.extend(sources.iter().filter_map(Value::as_str).map(str::to_owned));
            }
        }
        if let Some(sources) = result
            .get("sources_deprioritized")
            .and_then(Value::as_array)
        {
            for source in sources.iter().filter_map(Value::as_str) {
                if !sources_deprioritized.iter().any(|value| value == source) {
                    sources_deprioritized.push(source.to_owned());
                }
            }
        }
        if source_health.as_array().is_some_and(Vec::is_empty) {
            source_health = result
                .get("source_health")
                .cloned()
                .unwrap_or_else(|| Value::Array(Vec::new()));
        }
        has_low_confidence_fallback_candidates |= result
            .get("low_confidence_fallback")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        per_query.push(query_diagnostic(&result));
    }
    let (all_results, low_confidence_fallback) = finalize_search_results(
        all_results,
        max_results,
        has_low_confidence_fallback_candidates,
    );
    let quality = candidates::average_score(&all_results);
    let ok = !all_results.is_empty();

    json!({
        "ok": ok,
        "stage": "search",
        "mode": "search",
        "queries": queries,
        "source": if ok { "multi_source_html" } else { "none" },
        "sources_attempted": sources_attempted,
        "source_order": source_order,
        "sources_deprioritized": sources_deprioritized,
        "source_health": source_health,
        "quality": quality,
        "low_confidence": !ok || quality < 0.24,
        "low_confidence_fallback": low_confidence_fallback,
        "results": all_results.clone(),
        "candidates": all_results.clone(),
        "candidate_count": all_results.len(),
        "per_query": per_query,
        "fallback_errors": fallback_errors,
        "source_diagnostics": source_diagnostics,
        "next_action": "When page content is needed, call web_search again with mode=open and selected open_urls. open_ids are also accepted for candidate lookup.",
        "truncated": queries.len() > MAX_QUERIES
    })
}

fn finalize_search_results(
    raw_results: Vec<Value>,
    max_results: usize,
    has_low_confidence_fallback_candidates: bool,
) -> (Vec<Value>, bool) {
    let fallback_source = raw_results.clone();
    let mut results = candidates::dedupe_results(raw_results);
    candidates::retain_usable_results(&mut results);
    results.sort_by(|left, right| {
        let right_score = right.get("score").and_then(Value::as_f64).unwrap_or(0.0);
        let left_score = left.get("score").and_then(Value::as_f64).unwrap_or(0.0);
        right_score
            .partial_cmp(&left_score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    results.truncate(max_results);
    if !results.is_empty() || !has_low_confidence_fallback_candidates {
        return (results, false);
    }
    let fallback = search::low_confidence_fallback_results(fallback_source, max_results);
    let used_fallback = !fallback.is_empty();
    (fallback, used_fallback)
}

fn action_mode(arguments: &Value) -> &'static str {
    let mode = input::mode(arguments);
    if mode == "open"
        || !input::open_targets(arguments).is_empty()
        || !input::open_ids(arguments).is_empty()
    {
        "open"
    } else {
        "search"
    }
}

fn query_diagnostic(result: &Value) -> Value {
    let result_count = result
        .get("results")
        .and_then(Value::as_array)
        .map(Vec::len)
        .unwrap_or(0);
    let fallback_error_count = result
        .get("fallback_errors")
        .and_then(Value::as_array)
        .map(Vec::len)
        .unwrap_or(0);
    let source_diagnostic_count = result
        .get("source_diagnostics")
        .and_then(Value::as_array)
        .map(Vec::len)
        .unwrap_or(0);
    json!({
        "ok": result.get("ok").cloned().unwrap_or(Value::Bool(false)),
        "query": result.get("query").cloned().unwrap_or(Value::Null),
        "source": result.get("source").cloned().unwrap_or(Value::Null),
        "candidate_count": result_count,
        "quality": result.get("quality").cloned().unwrap_or(Value::Null),
        "low_confidence": result.get("low_confidence").cloned().unwrap_or(Value::Null),
        "fallback_error_count": fallback_error_count,
        "source_diagnostic_count": source_diagnostic_count
    })
}

fn usize_arg(args: &Value, key: &str, fallback: usize, min: usize, max: usize) -> usize {
    args.get(key)
        .and_then(Value::as_u64)
        .and_then(|value| usize::try_from(value).ok())
        .unwrap_or(fallback)
        .clamp(min, max)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn query_diagnostic_omits_duplicate_result_bodies() {
        let diagnostic = query_diagnostic(&json!({
            "ok": true,
            "query": "Rust README",
            "source": "multi_source_html",
            "quality": 0.8,
            "low_confidence": false,
            "results": [{
                "id": "cand_1",
                "title": "Rust",
                "url": "https://example.com",
                "snippet": "large snippet should not be duplicated here"
            }],
            "fallback_errors": [{ "source": "bing_html", "error": "empty_results" }],
            "source_diagnostics": [{ "source": "bing_html", "result_count": 0 }]
        }));

        assert_eq!(diagnostic["candidate_count"], 1);
        assert_eq!(diagnostic["fallback_error_count"], 1);
        assert_eq!(diagnostic["source_diagnostic_count"], 1);
        assert!(diagnostic.get("results").is_none());
        assert!(diagnostic.get("fallback_errors").is_none());
        assert!(diagnostic.get("source_diagnostics").is_none());
    }

    #[test]
    fn low_confidence_fallback_survives_outer_aggregation() {
        let (all_results, low_confidence_fallback) = finalize_search_results(
            vec![json!({
                "url": "https://weak.example.com/noise",
                "title": "Noise",
                "snippet": "",
                "query": "today weather in Zhongshan China",
                "source": "bing_html",
                "score": 0.12
            })],
            5,
            true,
        );

        assert_eq!(all_results.len(), 1);
        assert!(low_confidence_fallback);
        assert_eq!(all_results[0]["url"], "https://weak.example.com/noise");
    }

    #[test]
    fn low_confidence_fallback_flag_requires_fallback_results_to_be_used() {
        let (all_results, low_confidence_fallback) = finalize_search_results(
            vec![
                json!({
                    "url": "https://weather.example.com/zhongshan",
                    "title": "Zhongshan weather today",
                    "snippet": "Zhongshan weather details",
                    "query": "today weather in Zhongshan China",
                    "source": "brave_html",
                    "score": 0.988
                }),
                json!({
                    "url": "https://noise.example.com",
                    "title": "Noise",
                    "snippet": "",
                    "query": "today weather in Zhongshan China",
                    "source": "bing_html",
                    "score": 0.08
                }),
            ],
            5,
            true,
        );

        assert_eq!(all_results.len(), 1);
        assert_eq!(all_results[0]["source"], "brave_html");
        assert!(!low_confidence_fallback);
    }

    #[test]
    fn web_search_total_timeout_stays_bounded() {
        assert!(WEB_SEARCH_TOTAL_TIMEOUT_SECS <= 15);
    }

    #[test]
    fn action_mode_treats_urls_as_open_requests() {
        assert_eq!(
            action_mode(&json!({ "query": "https://example.com" })),
            "open"
        );
        assert_eq!(action_mode(&json!({ "query": "weather today" })), "search");
        assert_eq!(action_mode(&json!({ "open_ids": ["cand_123"] })), "open");
    }
}

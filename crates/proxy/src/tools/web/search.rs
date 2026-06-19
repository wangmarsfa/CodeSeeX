use regex::Regex;
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::sync::OnceLock;
use std::time::{Duration, Instant};
use tokio::sync::Mutex;

use codeseex_core::NetworkProxyMode;

use super::candidates::{average_score, dedupe_results, make_search_result, retain_usable_results};
use super::extract::{
    clean_visible_text, decode_basic_html_entities, strip_html_tags, truncate_chars,
};
use super::net::{fetch_text, read_limited_response_bytes, request_error_message, user_agent};

const SEARCH_SOURCE_HEALTH_TTL_SECS: u64 = 300;

static SEARCH_SOURCE_HEALTH: OnceLock<Mutex<BTreeMap<String, SearchHealthSnapshot>>> =
    OnceLock::new();

pub(super) async fn query(
    client: &reqwest::Client,
    proxy_mode: NetworkProxyMode,
    query: &str,
    max_results: usize,
) -> Value {
    let plan = search_plan(client, proxy_mode).await;
    let mut fallback_errors = Vec::new();
    let mut collected = Vec::new();
    let mut sources_attempted = Vec::new();

    let primary_sources = plan.primary_sources();
    let primary_results = run_search_sources(client, query, max_results, &primary_sources).await;
    collect_source_results(
        primary_results,
        &mut collected,
        &mut fallback_errors,
        &mut sources_attempted,
    );

    if collected.is_empty() && !plan.deprioritized_sources().is_empty() {
        let fallback_results =
            run_search_sources(client, query, max_results, &plan.deprioritized_sources()).await;
        collect_source_results(
            fallback_results,
            &mut collected,
            &mut fallback_errors,
            &mut sources_attempted,
        );
    }

    let mut results = dedupe_results(collected);
    retain_usable_results(&mut results);
    results.sort_by(|left, right| {
        let right_score = right.get("score").and_then(Value::as_f64).unwrap_or(0.0);
        let left_score = left.get("score").and_then(Value::as_f64).unwrap_or(0.0);
        right_score
            .partial_cmp(&left_score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    results.truncate(max_results);
    let quality = average_score(&results);

    json!({
        "ok": !results.is_empty(),
        "stage": "search",
        "mode": "search",
        "query": query,
        "source": if results.is_empty() { "none" } else { "multi_source_html" },
        "sources_attempted": sources_attempted,
        "source_order": plan.source_order_names(),
        "sources_deprioritized": plan.deprioritized_source_names(),
        "source_health": plan.health_diagnostic(),
        "results": results.clone(),
        "candidates": results.clone(),
        "candidate_count": results.len(),
        "quality": quality,
        "low_confidence": results.is_empty() || quality < 0.24,
        "fallback_errors": fallback_errors
    })
}

pub(super) async fn warm_sources(client: &reqwest::Client, proxy_mode: NetworkProxyMode) -> Value {
    let snapshot =
        refresh_search_source_health(client, &crate::network::proxy_cache_key(proxy_mode)).await;
    json!({
        "ok": true,
        "stage": "search_source_probe",
        "proxy_key": snapshot.cache_key,
        "source_order": ranked_sources_from_health(&snapshot.sources)
            .iter()
            .map(|source| source.name())
            .collect::<Vec<_>>(),
        "source_health": source_health_diagnostic(&snapshot.sources, snapshot.checked_at.elapsed().as_millis() as u64)
    })
}

async fn run_search_sources(
    client: &reqwest::Client,
    query: &str,
    max_results: usize,
    sources: &[SearchSource],
) -> Vec<Value> {
    futures_util::future::join_all(
        sources
            .iter()
            .copied()
            .map(|source| source.search(client, query, max_results)),
    )
    .await
}

fn collect_source_results(
    results_by_source: Vec<Value>,
    collected: &mut Vec<Value>,
    fallback_errors: &mut Vec<Value>,
    sources_attempted: &mut Vec<String>,
) {
    for result in results_by_source {
        let source = result
            .get("source")
            .and_then(Value::as_str)
            .unwrap_or("unknown");
        push_unique_source(sources_attempted, source);
        if let Some(results) = result.get("results").and_then(Value::as_array) {
            collected.extend(results.iter().cloned());
        }
        if result.get("ok").and_then(Value::as_bool) != Some(true) {
            fallback_errors.push(json!({
                "source": source,
                "error": result.get("error").and_then(Value::as_str).unwrap_or("empty_results"),
                "status": result.get("status").and_then(Value::as_u64),
                "message": result.get("message").and_then(Value::as_str).unwrap_or_default()
            }));
        }
    }
}

fn push_unique_source(output: &mut Vec<String>, source: &str) {
    if !output.iter().any(|value| value == source) {
        output.push(source.to_owned());
    }
}

async fn search_plan(client: &reqwest::Client, proxy_mode: NetworkProxyMode) -> SearchPlan {
    let snapshot = search_source_health(client, &crate::network::proxy_cache_key(proxy_mode)).await;
    let ordered_sources = ranked_sources_from_health(&snapshot.sources);
    SearchPlan {
        cache_key: snapshot.cache_key,
        checked_at_age_ms: snapshot.checked_at.elapsed().as_millis() as u64,
        ordered_sources,
        health: snapshot.sources,
    }
}

async fn search_source_health(client: &reqwest::Client, cache_key: &str) -> SearchHealthSnapshot {
    let cache = SEARCH_SOURCE_HEALTH.get_or_init(|| Mutex::new(BTreeMap::new()));
    {
        let guard = cache.lock().await;
        if let Some(snapshot) = guard.get(cache_key) {
            if snapshot.checked_at.elapsed() < Duration::from_secs(SEARCH_SOURCE_HEALTH_TTL_SECS) {
                return snapshot.clone();
            }
        }
    }

    refresh_search_source_health(client, cache_key).await
}

async fn refresh_search_source_health(
    client: &reqwest::Client,
    cache_key: &str,
) -> SearchHealthSnapshot {
    let snapshot = SearchHealthSnapshot {
        cache_key: cache_key.to_owned(),
        checked_at: Instant::now(),
        sources: futures_util::future::join_all(
            SearchSource::ALL
                .iter()
                .copied()
                .map(|source| probe_search_source(client, source)),
        )
        .await,
    };
    let cache = SEARCH_SOURCE_HEALTH.get_or_init(|| Mutex::new(BTreeMap::new()));
    let mut guard = cache.lock().await;
    guard.insert(cache_key.to_owned(), snapshot.clone());
    snapshot
}

async fn probe_search_source(client: &reqwest::Client, source: SearchSource) -> SearchSourceHealth {
    let Some(url) = source.probe_url() else {
        return SearchSourceHealth {
            source,
            reachable: false,
            latency_ms: None,
            status: None,
            error: Some("invalid_probe_url".to_owned()),
        };
    };
    let started = Instant::now();
    let response = tokio::time::timeout(
        Duration::from_secs(super::net::WEB_REQUEST_TIMEOUT_SECS),
        client
            .get(url)
            .header(reqwest::header::USER_AGENT, user_agent())
            .header(
                reqwest::header::ACCEPT,
                "text/html,application/xhtml+xml,application/json;q=0.9,*/*;q=0.8",
            )
            .send(),
    )
    .await;
    let latency_ms = Some(started.elapsed().as_millis() as u64);
    match response {
        Ok(Ok(response)) => SearchSourceHealth {
            source,
            reachable: true,
            latency_ms,
            status: Some(response.status().as_u16()),
            error: None,
        },
        Ok(Err(error)) => SearchSourceHealth {
            source,
            reachable: false,
            latency_ms,
            status: None,
            error: Some(request_error_message(&error)),
        },
        Err(_) => SearchSourceHealth {
            source,
            reachable: false,
            latency_ms,
            status: None,
            error: Some("probe_timeout".to_owned()),
        },
    }
}

fn ranked_sources_from_health(health: &[SearchSourceHealth]) -> Vec<SearchSource> {
    let mut health = health.to_vec();
    health.sort_by(|left, right| {
        right
            .reachable
            .cmp(&left.reachable)
            .then_with(|| {
                left.latency_ms
                    .unwrap_or(u64::MAX)
                    .cmp(&right.latency_ms.unwrap_or(u64::MAX))
            })
            .then_with(|| {
                left.source
                    .preferred_rank()
                    .cmp(&right.source.preferred_rank())
            })
    });
    let mut sources = health
        .into_iter()
        .map(|health| health.source)
        .collect::<Vec<_>>();
    for source in SearchSource::ALL {
        if !sources.contains(&source) {
            sources.push(source);
        }
    }
    sources
}

async fn bing_html(client: &reqwest::Client, query: &str, max_results: usize) -> Value {
    bing_html_at(
        client,
        query,
        max_results,
        "bing_html",
        "https://www.bing.com/search",
    )
    .await
}

async fn bing_html_at(
    client: &reqwest::Client,
    query: &str,
    max_results: usize,
    source: &'static str,
    endpoint: &'static str,
) -> Value {
    let locale = web_locale(query);
    let Ok(url) = reqwest::Url::parse_with_params(
        endpoint,
        &[
            ("q", query),
            ("setlang", locale.bing_setlang),
            ("mkt", locale.bing_market),
            ("cc", locale.bing_country),
            ("ensearch", locale.bing_english_search),
        ],
    ) else {
        return json!({ "ok": false, "query": query, "source": source, "error": "invalid_search_url" });
    };
    let fetched = fetch_text(client, url, "text/html,application/xhtml+xml").await;
    if fetched.get("ok").and_then(Value::as_bool) != Some(true) {
        return merge_search_error(source, query, fetched);
    }
    let html = fetched
        .get("text")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let results = parse_bing_results(query, html, max_results, source);
    json!({
        "ok": !results.is_empty(),
        "query": query,
        "source": source,
        "results": results,
        "error": if results.is_empty() { "empty_results" } else { "" }
    })
}

async fn brave_html(client: &reqwest::Client, query: &str, max_results: usize) -> Value {
    let Ok(url) = reqwest::Url::parse_with_params(
        "https://search.brave.com/search",
        &[("q", query), ("source", "web")],
    ) else {
        return json!({ "ok": false, "query": query, "source": "brave_html", "error": "invalid_search_url" });
    };
    let fetched = fetch_text(client, url, "text/html,application/xhtml+xml").await;
    if fetched.get("ok").and_then(Value::as_bool) != Some(true) {
        return merge_search_error("brave_html", query, fetched);
    }
    let html = fetched
        .get("text")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let results = parse_brave_results(query, html, max_results);
    json!({
        "ok": !results.is_empty(),
        "query": query,
        "source": "brave_html",
        "results": results,
        "error": if results.is_empty() { "empty_results" } else { "" }
    })
}

async fn duckduckgo_lite(client: &reqwest::Client, query: &str, max_results: usize) -> Value {
    let locale = web_locale(query);
    let Ok(url) = reqwest::Url::parse_with_params(
        "https://lite.duckduckgo.com/lite/",
        &[("q", query), ("kl", locale.duckduckgo_kl)],
    ) else {
        return json!({ "ok": false, "query": query, "source": "duckduckgo_lite", "error": "invalid_search_url" });
    };
    let fetched = fetch_text(client, url, "text/html,application/xhtml+xml").await;
    if fetched.get("ok").and_then(Value::as_bool) != Some(true) {
        return merge_search_error("duckduckgo_lite", query, fetched);
    }
    let html = fetched
        .get("text")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let results = parse_duckduckgo_lite_results(query, html, max_results);
    json!({
        "ok": !results.is_empty(),
        "query": query,
        "source": "duckduckgo_lite",
        "results": results,
        "error": if results.is_empty() { "empty_results" } else { "" }
    })
}

async fn duckduckgo_html(client: &reqwest::Client, query: &str, max_results: usize) -> Value {
    let locale = web_locale(query);
    let Ok(url) = reqwest::Url::parse_with_params(
        "https://html.duckduckgo.com/html/",
        &[("q", query), ("kl", locale.duckduckgo_kl)],
    ) else {
        return json!({ "ok": false, "query": query, "source": "duckduckgo_html", "error": "invalid_search_url" });
    };
    let fetched = fetch_text(client, url, "text/html,application/xhtml+xml").await;
    if fetched.get("ok").and_then(Value::as_bool) != Some(true) {
        return merge_search_error("duckduckgo_html", query, fetched);
    }
    let html = fetched
        .get("text")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let results = parse_duckduckgo_results(query, html, max_results);
    json!({
        "ok": !results.is_empty(),
        "query": query,
        "source": "duckduckgo_html",
        "results": results,
        "error": if results.is_empty() { "empty_results" } else { "" }
    })
}

async fn duckduckgo_instant_answer(
    client: &reqwest::Client,
    query: &str,
    max_results: usize,
) -> Value {
    let Ok(url) = reqwest::Url::parse_with_params(
        "https://api.duckduckgo.com/",
        &[
            ("q", query),
            ("format", "json"),
            ("no_html", "1"),
            ("skip_disambig", "1"),
        ],
    ) else {
        return json!({ "ok": false, "query": query, "source": "duckduckgo_instant_answer", "error": "invalid_search_url" });
    };
    let response = match client
        .get(url)
        .header(reqwest::header::USER_AGENT, user_agent())
        .header(reqwest::header::ACCEPT, "application/json")
        .send()
        .await
    {
        Ok(response) => response,
        Err(error) => {
            return json!({ "ok": false, "query": query, "source": "duckduckgo_instant_answer", "error": "request_failed", "message": request_error_message(&error) });
        }
    };
    let status = response.status().as_u16();
    let (bytes, byte_truncated) = read_limited_response_bytes(response).await;
    if byte_truncated {
        return json!({
            "ok": false,
            "query": query,
            "source": "duckduckgo_instant_answer",
            "status": status,
            "error": "search_response_too_large",
            "bytes": bytes.len()
        });
    }
    let payload = serde_json::from_slice::<Value>(&bytes).unwrap_or_else(|_| json!({}));
    let mut results = Vec::new();

    let abstract_text = payload
        .get("AbstractText")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|text| !text.is_empty());
    if let Some(text) = abstract_text {
        let text = clean_visible_text(text);
        results.push(json!({
            "title": clean_visible_text(payload.get("Heading").and_then(Value::as_str).unwrap_or(query)),
            "url": payload.get("AbstractURL").and_then(Value::as_str).unwrap_or(""),
            "snippet": truncate_chars(&text, 1200),
            "query": query,
            "source": "abstract"
        }));
    }
    if let Some(answer) = payload
        .get("Answer")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|text| !text.is_empty())
    {
        let answer = clean_visible_text(answer);
        results.push(json!({
            "title": "Answer",
            "url": payload.get("AnswerType").and_then(Value::as_str).unwrap_or(""),
            "snippet": truncate_chars(&answer, 1200),
            "query": query,
            "source": "answer"
        }));
    }
    collect_duckduckgo_related(
        query,
        payload.get("RelatedTopics"),
        &mut results,
        max_results,
    );
    results.truncate(max_results);

    json!({
        "ok": (200..400).contains(&status),
        "query": query,
        "status": status,
        "source": "duckduckgo_instant_answer",
        "results": results,
        "truncated": false,
        "bytes": bytes.len()
    })
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SearchSource {
    BingHtml,
    BraveHtml,
    DuckDuckGoLite,
    DuckDuckGoHtml,
    DuckDuckGoInstantAnswer,
}

impl SearchSource {
    const ALL: [SearchSource; 5] = [
        SearchSource::BingHtml,
        SearchSource::BraveHtml,
        SearchSource::DuckDuckGoLite,
        SearchSource::DuckDuckGoHtml,
        SearchSource::DuckDuckGoInstantAnswer,
    ];

    fn name(self) -> &'static str {
        match self {
            SearchSource::BingHtml => "bing_html",
            SearchSource::BraveHtml => "brave_html",
            SearchSource::DuckDuckGoLite => "duckduckgo_lite",
            SearchSource::DuckDuckGoHtml => "duckduckgo_html",
            SearchSource::DuckDuckGoInstantAnswer => "duckduckgo_instant_answer",
        }
    }

    fn preferred_rank(self) -> usize {
        match self {
            SearchSource::BingHtml => 0,
            SearchSource::BraveHtml => 1,
            SearchSource::DuckDuckGoLite => 2,
            SearchSource::DuckDuckGoHtml => 3,
            SearchSource::DuckDuckGoInstantAnswer => 4,
        }
    }

    fn probe_url(self) -> Option<reqwest::Url> {
        match self {
            SearchSource::BingHtml => reqwest::Url::parse_with_params(
                "https://www.bing.com/search",
                &[("q", "codeseex web search probe")],
            ),
            SearchSource::BraveHtml => reqwest::Url::parse_with_params(
                "https://search.brave.com/search",
                &[("q", "codeseex web search probe"), ("source", "web")],
            ),
            SearchSource::DuckDuckGoLite => reqwest::Url::parse_with_params(
                "https://lite.duckduckgo.com/lite/",
                &[("q", "codeseex web search probe")],
            ),
            SearchSource::DuckDuckGoHtml => reqwest::Url::parse_with_params(
                "https://html.duckduckgo.com/html/",
                &[("q", "codeseex web search probe")],
            ),
            SearchSource::DuckDuckGoInstantAnswer => reqwest::Url::parse_with_params(
                "https://api.duckduckgo.com/",
                &[
                    ("q", "codeseex web search probe"),
                    ("format", "json"),
                    ("no_html", "1"),
                    ("skip_disambig", "1"),
                ],
            ),
        }
        .ok()
    }

    async fn search(self, client: &reqwest::Client, query: &str, max_results: usize) -> Value {
        match self {
            SearchSource::BingHtml => bing_html(client, query, max_results).await,
            SearchSource::BraveHtml => brave_html(client, query, max_results).await,
            SearchSource::DuckDuckGoLite => duckduckgo_lite(client, query, max_results).await,
            SearchSource::DuckDuckGoHtml => duckduckgo_html(client, query, max_results).await,
            SearchSource::DuckDuckGoInstantAnswer => {
                duckduckgo_instant_answer(client, query, max_results).await
            }
        }
    }
}

#[derive(Clone, Debug)]
struct SearchSourceHealth {
    source: SearchSource,
    reachable: bool,
    latency_ms: Option<u64>,
    status: Option<u16>,
    error: Option<String>,
}

#[derive(Clone, Debug)]
struct SearchHealthSnapshot {
    cache_key: String,
    checked_at: Instant,
    sources: Vec<SearchSourceHealth>,
}

#[derive(Clone, Debug)]
struct SearchPlan {
    cache_key: String,
    checked_at_age_ms: u64,
    ordered_sources: Vec<SearchSource>,
    health: Vec<SearchSourceHealth>,
}

impl SearchPlan {
    fn primary_sources(&self) -> Vec<SearchSource> {
        let primary = self
            .ordered_sources
            .iter()
            .copied()
            .filter(|source| self.source_reachable(*source).unwrap_or(true))
            .collect::<Vec<_>>();
        if primary.is_empty() {
            self.ordered_sources.clone()
        } else {
            primary
        }
    }

    fn deprioritized_sources(&self) -> Vec<SearchSource> {
        self.ordered_sources
            .iter()
            .copied()
            .filter(|source| self.source_reachable(*source) == Some(false))
            .collect()
    }

    fn source_order_names(&self) -> Vec<&'static str> {
        self.ordered_sources
            .iter()
            .map(|source| source.name())
            .collect()
    }

    fn deprioritized_source_names(&self) -> Vec<&'static str> {
        self.deprioritized_sources()
            .iter()
            .map(|source| source.name())
            .collect()
    }

    fn health_diagnostic(&self) -> Vec<Value> {
        let mut diagnostic = source_health_diagnostic(&self.health, self.checked_at_age_ms);
        for item in &mut diagnostic {
            if let Some(object) = item.as_object_mut() {
                object.insert(
                    "proxy_key".to_owned(),
                    Value::String(self.cache_key.clone()),
                );
            }
        }
        diagnostic
    }

    fn source_reachable(&self, source: SearchSource) -> Option<bool> {
        self.health
            .iter()
            .find(|health| health.source == source)
            .map(|health| health.reachable)
    }
}

fn source_health_diagnostic(health: &[SearchSourceHealth], age_ms: u64) -> Vec<Value> {
    health
        .iter()
        .map(|health| {
            json!({
                "source": health.source.name(),
                "reachable": health.reachable,
                "latency_ms": health.latency_ms,
                "status": health.status,
                "error": health.error,
                "age_ms": age_ms
            })
        })
        .collect()
}

fn merge_search_error(source: &str, query: &str, error: Value) -> Value {
    json!({
        "ok": false,
        "query": query,
        "source": source,
        "results": [],
        "error": error.get("error").and_then(Value::as_str).unwrap_or("request_failed"),
        "status": error.get("status").and_then(Value::as_u64),
        "message": error.get("message").and_then(Value::as_str).unwrap_or_default()
    })
}

fn collect_duckduckgo_related(
    query: &str,
    value: Option<&Value>,
    output: &mut Vec<Value>,
    max_results: usize,
) {
    if output.len() >= max_results {
        return;
    }
    let Some(items) = value.and_then(Value::as_array) else {
        return;
    };
    for item in items {
        if output.len() >= max_results {
            return;
        }
        if let Some(topics) = item.get("Topics") {
            collect_duckduckgo_related(query, Some(topics), output, max_results);
            continue;
        }
        let text = item
            .get("Text")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|text| !text.is_empty());
        let Some(text) = text else {
            continue;
        };
        let text = clean_visible_text(text);
        output.push(json!({
            "title": truncate_chars(&text, 120),
            "url": item.get("FirstURL").and_then(Value::as_str).unwrap_or(""),
            "snippet": truncate_chars(&text, 1200),
            "query": query,
            "source": "related_topic"
        }));
    }
}

fn parse_bing_results(
    query: &str,
    html: &str,
    max_results: usize,
    source: &'static str,
) -> Vec<Value> {
    let Ok(block_re) = Regex::new(
        r#"(?is)<li\b[^>]*\bclass\s*=\s*(?:"[^"]*\bb_algo\b[^"]*"|'[^']*\bb_algo\b[^']*'|[^\s>]*\bb_algo\b[^\s>]*)[^>]*>(.*?)</li>"#,
    ) else {
        return Vec::new();
    };
    let Ok(link_re) =
        Regex::new(r#"(?is)<h2[^>]*>.*?<a[^>]+href="([^"]+)"[^>]*>(.*?)</a>.*?</h2>"#)
    else {
        return Vec::new();
    };
    let Ok(snippet_re) = Regex::new(
        r#"(?is)<(?:p|div)\b[^>]*\bclass\s*=\s*(?:"[^"]*\b(?:b_caption|b_snippet|b_lineclamp\d*)\b[^"]*"|'[^']*\b(?:b_caption|b_snippet|b_lineclamp\d*)\b[^']*'|[^\s>]*\b(?:b_caption|b_snippet|b_lineclamp\d*)\b[^\s>]*)[^>]*>(.*?)</(?:p|div)>"#,
    ) else {
        return Vec::new();
    };
    let mut results = Vec::new();
    for block in block_re.captures_iter(html) {
        if results.len() >= max_results {
            break;
        }
        let block = block.get(1).map(|value| value.as_str()).unwrap_or_default();
        let Some(link) = link_re.captures(block) else {
            continue;
        };
        let url = decode_basic_html_entities(link.get(1).map(|value| value.as_str()).unwrap_or(""));
        let title = strip_html_tags(link.get(2).map(|value| value.as_str()).unwrap_or(""));
        let snippet = snippet_re
            .captures(block)
            .and_then(|caps| caps.get(1).map(|value| strip_html_tags(value.as_str())))
            .unwrap_or_default();
        if let Some(item) = make_search_result(query, &title, &url, &snippet, source, results.len())
        {
            results.push(item);
        }
    }
    results
}

fn parse_duckduckgo_results(query: &str, html: &str, max_results: usize) -> Vec<Value> {
    let Ok(link_re) =
        Regex::new(r#"(?is)<a[^>]+class="[^"]*result__a[^"]*"[^>]+href="([^"]+)"[^>]*>(.*?)</a>"#)
    else {
        return Vec::new();
    };
    let Ok(snippet_re) = Regex::new(
        r#"(?is)<a[^>]+class="[^"]*result__snippet[^"]*"[^>]*>(.*?)</a>|<div[^>]+class="[^"]*result__snippet[^"]*"[^>]*>(.*?)</div>"#,
    ) else {
        return Vec::new();
    };
    let snippets = snippet_re
        .captures_iter(html)
        .map(|caps| {
            caps.get(1)
                .or_else(|| caps.get(2))
                .map(|value| strip_html_tags(value.as_str()))
                .unwrap_or_default()
        })
        .collect::<Vec<_>>();
    let mut results = Vec::new();
    for (index, link) in link_re.captures_iter(html).enumerate() {
        if results.len() >= max_results {
            break;
        }
        let raw_url = link.get(1).map(|value| value.as_str()).unwrap_or_default();
        let url = normalize_duckduckgo_result_url(raw_url);
        let title = strip_html_tags(link.get(2).map(|value| value.as_str()).unwrap_or(""));
        let snippet = snippets.get(index).cloned().unwrap_or_default();
        if let Some(item) = make_search_result(
            query,
            &title,
            &url,
            &snippet,
            "duckduckgo_html",
            results.len(),
        ) {
            results.push(item);
        }
    }
    results
}

fn parse_brave_results(query: &str, html: &str, max_results: usize) -> Vec<Value> {
    let Ok(link_re) = Regex::new(r#"(?is)<a[^>]+href="(https?://[^"]+)"[^>]*>(.*?)</a>"#) else {
        return Vec::new();
    };
    let matches = link_re.captures_iter(html).collect::<Vec<_>>();
    let mut results = Vec::new();
    for (index, link) in matches.iter().enumerate() {
        if results.len() >= max_results {
            break;
        }
        let url = decode_basic_html_entities(link.get(1).map(|value| value.as_str()).unwrap_or(""));
        if url.contains("search.brave.com") || url.contains("imgs.search.brave.com") {
            continue;
        }
        let title = strip_html_tags(link.get(2).map(|value| value.as_str()).unwrap_or(""));
        let title = title.trim();
        if title.is_empty() {
            continue;
        }
        let snippet_start = link.get(0).map(|value| value.end()).unwrap_or_default();
        let snippet_end = matches
            .get(index + 1)
            .and_then(|next| next.get(0).map(|value| value.start()))
            .unwrap_or(html.len())
            .min(snippet_start.saturating_add(1800));
        let snippet = html
            .get(snippet_start..snippet_end)
            .map(strip_html_tags)
            .unwrap_or_default();
        if let Some(item) =
            make_search_result(query, title, &url, &snippet, "brave_html", results.len())
        {
            results.push(item);
        }
    }
    results
}

fn normalize_duckduckgo_result_url(value: &str) -> String {
    let raw = decode_basic_html_entities(value);
    let raw = if raw.starts_with("//") {
        format!("https:{raw}")
    } else {
        raw
    };
    let Ok(url) = reqwest::Url::parse(&raw)
        .or_else(|_| reqwest::Url::parse(&format!("https://duckduckgo.com{raw}")))
    else {
        return raw;
    };
    url.query_pairs()
        .find(|(key, _)| key == "uddg")
        .map(|(_, value)| value.to_string())
        .unwrap_or_else(|| url.to_string())
}

fn parse_duckduckgo_lite_results(query: &str, html: &str, max_results: usize) -> Vec<Value> {
    let Ok(link_re) = Regex::new(r#"(?is)<a[^>]+href="([^"]+)"[^>]*>(.*?)</a>"#) else {
        return Vec::new();
    };
    let matches = link_re.captures_iter(html).collect::<Vec<_>>();
    let mut results = Vec::new();
    for (index, link) in matches.iter().enumerate() {
        if results.len() >= max_results {
            break;
        }
        let raw_url = link.get(1).map(|value| value.as_str()).unwrap_or_default();
        if !raw_url.contains("uddg=") {
            continue;
        }
        let url = normalize_duckduckgo_result_url(raw_url);
        let title = strip_html_tags(link.get(2).map(|value| value.as_str()).unwrap_or(""));
        let title = title.trim();
        if title.is_empty() {
            continue;
        }
        let snippet_start = link.get(0).map(|value| value.end()).unwrap_or_default();
        let snippet_end = matches
            .get(index + 1)
            .and_then(|next| next.get(0).map(|value| value.start()))
            .unwrap_or(html.len())
            .min(snippet_start.saturating_add(1600));
        let snippet = html
            .get(snippet_start..snippet_end)
            .map(strip_html_tags)
            .unwrap_or_default();
        if let Some(item) = make_search_result(
            query,
            title,
            &url,
            &snippet,
            "duckduckgo_lite",
            results.len(),
        ) {
            results.push(item);
        }
    }
    results
}

struct WebLocale {
    bing_market: &'static str,
    bing_setlang: &'static str,
    bing_country: &'static str,
    bing_english_search: &'static str,
    duckduckgo_kl: &'static str,
}

fn web_locale(query: &str) -> WebLocale {
    if query
        .chars()
        .any(|ch| ('\u{3400}'..='\u{9fff}').contains(&ch))
    {
        return WebLocale {
            bing_market: "zh-CN",
            bing_setlang: "zh-CN",
            bing_country: "CN",
            bing_english_search: "0",
            duckduckgo_kl: "cn-zh",
        };
    }
    WebLocale {
        bing_market: "en-US",
        bing_setlang: "en-US",
        bing_country: "US",
        bing_english_search: "1",
        duckduckgo_kl: "us-en",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_bing_html_results() {
        let html = r#"
            <html><body>
              <li class="b_algo">
                <h2><a href="https://example.com/result">Example &#8212; Result</a></h2>
                <div class="b_caption"><p>Useful&nbsp;snippet &amp;#187; CodeSeeX.</p></div>
              </li>
            </body></html>
        "#;
        let results = parse_bing_results("CodeSeeX", html, 5, "bing_html");

        assert_eq!(results.len(), 1);
        assert_eq!(results[0]["url"], "https://example.com/result");
        assert_eq!(results[0]["title"], "Example — Result");
        assert_eq!(results[0]["snippet"], "Useful snippet » CodeSeeX.");
        assert!(results[0]["id"]
            .as_str()
            .unwrap_or_default()
            .starts_with("cand_"));
    }

    #[test]
    fn parses_bing_html_results_with_unquoted_data_attributes() {
        let html = r#"
            <ol id="b_results">
              <li class="b_algo" data-id iid=SERP.5159>
                <h2 class=""><a target="_blank" href="https://example.com/current" h="ID=SERP,5102.2">Current Result</a></h2>
                <div class="b_caption"><p class="b_lineclamp2">Current Bing HTML snippet.</p></div>
              </li>
            </ol>
        "#;
        let results = parse_bing_results("Current Bing HTML", html, 5, "bing_html");

        assert_eq!(results.len(), 1);
        assert_eq!(results[0]["url"], "https://example.com/current");
        assert_eq!(results[0]["title"], "Current Result");
        assert_eq!(results[0]["snippet"], "Current Bing HTML snippet.");
    }

    #[test]
    fn source_ranking_moves_unreachable_sources_after_reachable_sources() {
        let ranked = ranked_sources_from_health(&[
            SearchSourceHealth {
                source: SearchSource::DuckDuckGoLite,
                reachable: false,
                latency_ms: Some(3000),
                status: None,
                error: Some("probe_timeout".to_owned()),
            },
            SearchSourceHealth {
                source: SearchSource::BingHtml,
                reachable: true,
                latency_ms: Some(580),
                status: Some(200),
                error: None,
            },
        ]);

        assert_eq!(ranked[0], SearchSource::BingHtml);
        assert!(
            ranked
                .iter()
                .position(|source| *source == SearchSource::DuckDuckGoLite)
                .unwrap()
                > ranked
                    .iter()
                    .position(|source| *source == SearchSource::BingHtml)
                    .unwrap()
        );
    }

    #[test]
    fn source_ranking_keeps_slow_reachable_sources_before_unreachable_sources() {
        let ranked = ranked_sources_from_health(&[
            SearchSourceHealth {
                source: SearchSource::DuckDuckGoHtml,
                reachable: true,
                latency_ms: Some(9000),
                status: Some(200),
                error: None,
            },
            SearchSourceHealth {
                source: SearchSource::BraveHtml,
                reachable: false,
                latency_ms: Some(3000),
                status: None,
                error: Some("probe_timeout".to_owned()),
            },
        ]);

        assert!(
            ranked
                .iter()
                .position(|source| *source == SearchSource::DuckDuckGoHtml)
                .unwrap()
                < ranked
                    .iter()
                    .position(|source| *source == SearchSource::BraveHtml)
                    .unwrap()
        );
    }

    #[test]
    fn search_source_set_stays_quality_only_without_region_specific_fallbacks() {
        let names = SearchSource::ALL
            .iter()
            .map(|source| source.name())
            .collect::<Vec<_>>();

        assert_eq!(
            names,
            vec![
                "bing_html",
                "brave_html",
                "duckduckgo_lite",
                "duckduckgo_html",
                "duckduckgo_instant_answer"
            ]
        );
        assert!(!names.iter().any(|name| name.contains("cn_bing")));
        assert!(!names.iter().any(|name| name.contains("sogou")));
        assert!(!names.iter().any(|name| name.contains("360")));
        assert!(!names.iter().any(|name| name.contains("baidu")));
    }

    #[test]
    fn parses_duckduckgo_lite_results() {
        let html = r#"
            <html><body>
              <a rel="nofollow" href="//duckduckgo.com/l/?uddg=https%3A%2F%2Fpeps.python.org%2Fpep%2D0745%2F&rut=abc">
                PEP 745 - Python 3.14 Release Schedule | peps.python.org
              </a>
              <td class="result-snippet">Python 3.14 release schedule with beta and final dates.</td>
            </body></html>
        "#;
        let results = parse_duckduckgo_lite_results("Python 3.14 release schedule", html, 5);

        assert_eq!(results.len(), 1);
        assert_eq!(results[0]["url"], "https://peps.python.org/pep-0745/");
        assert!(results[0]["title"]
            .as_str()
            .unwrap_or_default()
            .contains("PEP 745"));
        assert!(results[0]["score"].as_f64().unwrap_or_default() >= 0.24);
    }

    #[test]
    fn parses_brave_results() {
        let html = r#"
            <html><body>
              <div class="snippet" data-type="web">
                <a href="https://peps.python.org/pep-0745/" class="result-header">
                  <div class="title">PEP 745 - Python 3.14 Release Schedule | peps.python.org</div>
                </a>
                <div class="generic-snippet">
                  Python 3.14 release schedule with final release dates and bugfix cadence.
                </div>
              </div>
            </body></html>
        "#;
        let results = parse_brave_results("Python 3.14 release schedule", html, 5);

        assert_eq!(results.len(), 1);
        assert_eq!(results[0]["url"], "https://peps.python.org/pep-0745/");
        assert!(results[0]["title"]
            .as_str()
            .unwrap_or_default()
            .contains("PEP 745"));
        assert!(results[0]["score"].as_f64().unwrap_or_default() >= 0.24);
    }
}

use crate::app_state::ProxyState;
use crate::config_payload::{
    model_override_to_ui, normalize_catalog_mode, temperature_to_ui, user_config_from_payload,
};
use crate::http_utils::{config_version, is_newer_version, normalize_version_label, now_seconds};
use crate::tools::registry::{enabled_tool_ids, tool_registry, tool_settings};
use axum::extract::{Path as AxumPath, Query, State};
use axum::http::{header, StatusCode};
use axum::response::IntoResponse;
use axum::routing::{get, post};
use axum::{Json, Router};
use codeseex_core::catalog::{build_codeseex_catalog, codex_toml_snippet, write_catalog_atomic};
use codeseex_core::codex_auth::read_codex_auth_api_key;
use codeseex_core::models::available_models;
use codeseex_core::urls::balance_url;
use codeseex_core::{AppConfig, UserConfig};
use serde::Deserialize;
use serde_json::{json, Value};

#[derive(Debug, Deserialize)]
struct EventsQuery {
    limit: Option<u32>,
    before: Option<String>,
}

pub(crate) fn router() -> Router<ProxyState> {
    Router::new()
        .route("/health", get(health))
        .route("/api/status", get(api_status))
        .route("/api/config", get(api_config).post(save_config))
        .route("/api/languages", get(api_languages))
        .route("/api/tools", get(api_tools))
        .route("/tool-assets/{tool_id}/{file}", get(tool_asset))
        .route("/api/app-info", get(api_app_info))
        .route("/api/update-check", get(api_update_check))
        .route("/api/deepseek/balance", get(api_balance))
        .route("/api/events", get(api_events))
        .route("/api/start", post(noop_ok))
        .route("/api/restart", post(noop_ok))
        .route("/api/stop", post(noop_ok))
        .route("/api/window/minimize", post(noop_ok))
        .route("/api/window/maximize", post(noop_ok))
        .route("/api/window/close", post(noop_ok))
        .route("/api/window/theme", post(noop_ok))
        .route("/api/codex-adapter", get(generate_adapter))
        .route(
            "/api/codex-adapter/generate",
            post(generate_adapter).get(generate_adapter),
        )
}

pub(crate) fn ensure_catalog(config: &AppConfig) -> anyhow::Result<()> {
    let catalog = build_codeseex_catalog();
    write_catalog_atomic(&config.catalog_path(), &catalog)
}

async fn health() -> impl IntoResponse {
    Json(json!({ "ok": true, "service": "codeseex-next" }))
}

async fn api_status(State(state): State<ProxyState>) -> impl IntoResponse {
    let config = state.active_config();
    let runtime = state.store.runtime_summary(120).await.ok();
    let events = state
        .store
        .recent_events(30, None)
        .await
        .map(|(events, _)| events)
        .unwrap_or_default();
    Json(json!({
        "ok": true,
        "running": true,
        "runtime_status": "running",
        "process_mode": "inline",
        "process_label": "CodeSeeX Next proxy",
        "pid": std::process::id(),
        "config_version": config_version(&config),
        "data_dir": config.data_dir.to_string_lossy(),
        "base_url": config.proxy_base_url(),
        "catalog_path": config.catalog_path().to_string_lossy(),
        "models": available_models().into_iter().map(|m| m.slug).collect::<Vec<_>>(),
        "runtime": {
            "status": "running",
            "port": state.config.port,
            "active_requests": runtime.as_ref().map(|value| value.active_requests).unwrap_or(0),
            "request_count": runtime.as_ref().map(|value| value.request_count).unwrap_or(0),
            "failed_request_count": runtime.as_ref().map(|value| value.failed_request_count).unwrap_or(0),
            "last_request_at": runtime.as_ref().and_then(|value| value.last_request_at.clone()),
            "last_turn": runtime.as_ref().and_then(|value| value.last_turn.clone()),
            "turn_history": runtime.as_ref().map(|value| value.turn_history.clone()).unwrap_or_default(),
            "total_cached_input_tokens": runtime.as_ref().map(|value| value.total_cached_input_tokens).unwrap_or(0),
            "total_cache_miss_input_tokens": runtime.as_ref().map(|value| value.total_cache_miss_input_tokens).unwrap_or(0),
            "total_output_tokens": runtime.as_ref().map(|value| value.total_output_tokens).unwrap_or(0),
            "average_ms": runtime.as_ref().map(|value| value.average_ms).unwrap_or(0)
        },
        "events": events,
        "upstream": {
            "base_url": config.upstream.base_url,
            "official_v1_compat": config.upstream.official_v1_compat
        }
    }))
}

async fn api_config(State(state): State<ProxyState>) -> impl IntoResponse {
    let config = state.active_config();
    let user_config = UserConfig::read_from(&config.config_path()).unwrap_or_default();
    let proxy = user_config.proxy.as_ref();
    let upstream = user_config.upstream.as_ref();
    let model = user_config.model.as_ref();
    let catalog = user_config.catalog.as_ref();
    let ui = user_config.ui.as_ref();
    let billing = user_config.billing.as_ref();
    let tools = user_config.tools.as_ref();
    let upstream_base_url = upstream
        .and_then(|value| value.base_url.as_deref())
        .filter(|value| !value.trim().is_empty())
        .unwrap_or("");
    let model_override = model
        .and_then(|value| value.override_mode)
        .unwrap_or(state.config.model_override);
    let temperature = model
        .and_then(|value| value.temperature)
        .unwrap_or(state.config.temperature);

    let mut payload = json!({
        "config_version": config_version(&config),
        "PROXY_PORT": proxy.and_then(|value| value.port).unwrap_or(state.config.port).to_string(),
        "DEEPSEEK_BASE_URL": upstream_base_url,
        "DEEPSEEK_OFFICIAL_V1_COMPAT": upstream.and_then(|value| value.official_v1_compat).unwrap_or(config.upstream.official_v1_compat).to_string(),
        "UPSTREAM_MODEL_OVERRIDE": model_override_to_ui(model_override),
        "DEEPSEEK_TEMPERATURE_PRESET": temperature_to_ui(temperature),
        "DEEPSEEK_THINKING": model.and_then(|value| value.thinking.as_deref()).unwrap_or("auto"),
        "CATALOG_MODE": catalog.and_then(|value| value.mode.as_deref()).map(normalize_catalog_mode).unwrap_or("default").to_string(),
        "SHOW_THINKING": ui.and_then(|value| value.show_thinking).unwrap_or(true).to_string(),
        "AUTO_START": ui.and_then(|value| value.auto_start).unwrap_or(false).to_string(),
        "UI_THEME": ui.and_then(|value| value.theme.as_deref()).unwrap_or("system"),
        "UI_LANGUAGE": ui.and_then(|value| value.language.as_deref()).unwrap_or("system"),
        "UI_CLOSE_BEHAVIOR": ui.and_then(|value| value.close_behavior.as_deref()).unwrap_or("exit"),
        "LOG_RETENTION_DAYS": ui.and_then(|value| value.log_retention_days).unwrap_or(7).to_string(),
        "BILLING_FLASH_CACHED_INPUT_CNY": billing.and_then(|value| value.flash_cached_input_cny).unwrap_or(0.02).to_string(),
        "BILLING_FLASH_CACHE_MISS_INPUT_CNY": billing.and_then(|value| value.flash_cache_miss_input_cny).unwrap_or(1.0).to_string(),
        "BILLING_FLASH_OUTPUT_CNY": billing.and_then(|value| value.flash_output_cny).unwrap_or(2.0).to_string(),
        "BILLING_PRO_CACHED_INPUT_CNY": billing.and_then(|value| value.pro_cached_input_cny).unwrap_or(0.025).to_string(),
        "BILLING_PRO_CACHE_MISS_INPUT_CNY": billing.and_then(|value| value.pro_cache_miss_input_cny).unwrap_or(3.0).to_string(),
        "BILLING_PRO_OUTPUT_CNY": billing.and_then(|value| value.pro_output_cny).unwrap_or(6.0).to_string(),
        "ENABLED_TOOLS": tools.and_then(|value| value.enabled.clone()).map(Value::from).unwrap_or(Value::Null)
    });
    if let Some(settings) = user_config
        .tools
        .as_ref()
        .and_then(|tools| tools.settings.as_ref())
    {
        if let Some(object) = payload.as_object_mut() {
            for key in crate::community_tools::community_tool_config_keys(&config.data_dir) {
                if let Some(value) = settings.get(&key) {
                    object.insert(key, Value::String(value.clone()));
                }
            }
        }
    }
    Json(payload)
}

async fn save_config(
    State(state): State<ProxyState>,
    Json(payload): Json<Value>,
) -> impl IntoResponse {
    let config = state.active_config();
    let existing_config = UserConfig::read_from(&config.config_path()).unwrap_or_default();
    let user_config = user_config_from_payload(&payload, existing_config, &config);
    match user_config.write_atomic(&config.config_path()) {
        Ok(()) => {
            let _ = state
                .store
                .record_event(
                    "info",
                    "manager_config_saved",
                    "Configuration saved.",
                    Some(&json!({ "path": config.config_path().to_string_lossy() })),
                )
                .await;
            Json(json!({
                "ok": true,
                "saved": true,
                "config_version": now_seconds().to_string(),
                "path": config.config_path().to_string_lossy()
            }))
            .into_response()
        }
        Err(error) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "ok": false, "error": error.to_string() })),
        )
            .into_response(),
    }
}

async fn api_languages() -> impl IntoResponse {
    Json(json!({
        "ok": true,
        "default": "en_us",
        "system": "system",
        "system_locale": std::env::var("LANG").ok(),
        "languages": [
            { "id": "en_us", "name": "English", "url": "/lang/en_us.json" },
            { "id": "zh_cn", "name": "Chinese (Simplified)", "url": "/lang/zh_cn.json" },
            { "id": "zh_tw", "name": "Chinese (Traditional)", "url": "/lang/zh_tw.json" },
            { "id": "zh_hk", "name": "Chinese (Hong Kong)", "url": "/lang/zh_hk.json" },
            { "id": "ja_jp", "name": "Japanese", "url": "/lang/ja_jp.json" },
            { "id": "ko_kr", "name": "Korean", "url": "/lang/ko_kr.json" },
            { "id": "fr_fr", "name": "French", "url": "/lang/fr_fr.json" },
            { "id": "de_de", "name": "Deutsch", "url": "/lang/de_de.json" },
            { "id": "ru_ru", "name": "Russian", "url": "/lang/ru_ru.json" }
        ]
    }))
}

async fn api_tools(State(state): State<ProxyState>) -> impl IntoResponse {
    let config = state.active_config();
    let enabled_tools = enabled_tool_ids(&config);
    let settings = tool_settings(&config);
    Json(json!({
        "ok": true,
        "tools": tool_registry(&config, &enabled_tools, &settings)
    }))
}

async fn tool_asset(
    State(state): State<ProxyState>,
    AxumPath((tool_id, file)): AxumPath<(String, String)>,
) -> impl IntoResponse {
    let config = state.active_config();
    let Some(path) = crate::community_tools::tool_asset_path(&config.data_dir, &tool_id, &file)
    else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let Ok(bytes) = std::fs::read(path) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let content_type = match file.to_ascii_lowercase().as_str() {
        "icon.svg" => "image/svg+xml; charset=utf-8",
        "icon.png" => "image/png",
        _ => "application/octet-stream",
    };
    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, content_type)],
        bytes,
    )
        .into_response()
}

async fn api_app_info() -> impl IntoResponse {
    Json(json!({
        "ok": true,
        "name": "CodeSeeX Next",
        "product_name": "CodeSeeX Next",
        "version": env!("CARGO_PKG_VERSION"),
        "license": "AGPL-3.0-only",
        "description": "Local Codex and DeepSeek bridge with a lightweight Tauri manager.",
        "repository": "https://github.com/TasteSteak/CodeSeeX",
        "urls": {
            "source": "https://github.com/TasteSteak/CodeSeeX",
            "feedback": "https://github.com/TasteSteak/CodeSeeX/issues",
            "license": "https://github.com/TasteSteak/CodeSeeX/blob/main/LICENSE",
            "releases": "https://github.com/TasteSteak/CodeSeeX/releases"
        }
    }))
}

async fn api_update_check(State(state): State<ProxyState>) -> impl IntoResponse {
    let current_version = env!("CARGO_PKG_VERSION");
    let checked_at = now_seconds().to_string();
    let fallback_url = "https://github.com/TasteSteak/CodeSeeX/releases";

    let result = state
        .client
        .get("https://api.github.com/repos/TasteSteak/CodeSeeX/releases/latest")
        .header(header::USER_AGENT, "CodeSeeX-Next")
        .header(header::ACCEPT, "application/vnd.github+json")
        .send()
        .await;

    let Ok(response) = result else {
        return Json(json!({
            "ok": false,
            "has_update": false,
            "latest_version": current_version,
            "current_version": current_version,
            "url": fallback_url,
            "checked_at": checked_at,
            "error": "update_check_unreachable"
        }));
    };

    if !response.status().is_success() {
        return Json(json!({
            "ok": false,
            "has_update": false,
            "latest_version": current_version,
            "current_version": current_version,
            "url": fallback_url,
            "checked_at": checked_at,
            "error": format!("github_status_{}", response.status().as_u16())
        }));
    }

    let payload = response.json::<Value>().await.unwrap_or_else(|_| json!({}));
    let latest_version = payload
        .get("tag_name")
        .and_then(Value::as_str)
        .or_else(|| payload.get("name").and_then(Value::as_str))
        .unwrap_or(current_version);
    let url = payload
        .get("html_url")
        .and_then(Value::as_str)
        .unwrap_or(fallback_url);

    Json(json!({
        "ok": true,
        "has_update": is_newer_version(latest_version, current_version),
        "latest_version": normalize_version_label(latest_version),
        "current_version": current_version,
        "url": url,
        "checked_at": checked_at,
        "error": null
    }))
}

async fn api_balance(State(state): State<ProxyState>) -> impl IntoResponse {
    let config = state.active_config();
    let Some(api_key) = read_codex_auth_api_key() else {
        return Json(json!({
            "ok": false,
            "code": "missing_api_key",
            "message": "API key is not configured."
        }))
        .into_response();
    };
    let balance_url = match balance_url(&config.upstream.base_url) {
        Ok(value) => value,
        Err(_) => {
            return Json(json!({
                "ok": false,
                "code": "invalid_deepseek_base_url",
                "message": "Invalid DeepSeek base URL."
            }))
            .into_response();
        }
    };

    match state
        .client
        .get(balance_url)
        .bearer_auth(api_key)
        .header(header::ACCEPT, "application/json")
        .timeout(std::time::Duration::from_secs(30))
        .send()
        .await
    {
        Ok(response) => {
            let status = response.status();
            match response.bytes().await {
                Ok(bytes) if status.is_success() => {
                    let body = serde_json::from_slice::<Value>(&bytes)
                        .unwrap_or_else(|_| json!({ "raw": String::from_utf8_lossy(&bytes) }));
                    Json(json!({
                        "ok": true,
                        "is_available": body.get("is_available").and_then(Value::as_bool).unwrap_or(false),
                        "balance_infos": body.get("balance_infos")
                            .and_then(Value::as_array)
                            .map(|items| items.iter().map(normalize_balance_info).collect::<Vec<_>>())
                            .unwrap_or_default(),
                        "checked_at": now_seconds().to_string()
                    }))
                    .into_response()
                }
                Ok(bytes) => Json(json!({
                        "ok": false,
                        "code": "deepseek_balance_error",
                        "status": status.as_u16(),
                        "message": balance_error_message(&bytes)
                }))
                .into_response(),
                Err(error) => Json(json!({
                        "ok": false,
                        "code": "deepseek_balance_failed",
                        "message": error.to_string()
                }))
                .into_response(),
            }
        }
        Err(error) => {
            let code = if error.is_timeout() {
                "deepseek_balance_timeout"
            } else {
                "deepseek_balance_failed"
            };
            Json(json!({
                "ok": false,
                "code": code,
                "message": if error.is_timeout() {
                    "DeepSeek balance request timed out.".to_owned()
                } else {
                    error.to_string()
                }
            }))
            .into_response()
        }
    }
}

fn normalize_balance_info(item: &Value) -> Value {
    json!({
        "currency": item.get("currency").and_then(Value::as_str).unwrap_or("").to_owned(),
        "total_balance": balance_value_to_string(item.get("total_balance")),
        "granted_balance": balance_value_to_string(item.get("granted_balance")),
        "topped_up_balance": balance_value_to_string(item.get("topped_up_balance"))
    })
}

fn balance_value_to_string(value: Option<&Value>) -> String {
    match value {
        Some(Value::String(text)) => text.clone(),
        Some(Value::Number(number)) => number.to_string(),
        Some(Value::Bool(value)) => value.to_string(),
        _ => "0".to_owned(),
    }
}

fn balance_error_message(bytes: &[u8]) -> String {
    let body = serde_json::from_slice::<Value>(bytes).unwrap_or_else(|_| json!({}));
    body.get("error")
        .and_then(|value| value.get("message"))
        .and_then(Value::as_str)
        .or_else(|| body.get("message").and_then(Value::as_str))
        .map(str::to_owned)
        .unwrap_or_else(|| {
            let text = String::from_utf8_lossy(bytes).trim().to_owned();
            if text.is_empty() {
                "DeepSeek balance request failed.".to_owned()
            } else {
                text
            }
        })
}

async fn api_events(
    State(state): State<ProxyState>,
    Query(query): Query<EventsQuery>,
) -> impl IntoResponse {
    match state
        .store
        .recent_events(query.limit.unwrap_or(30), query.before.as_deref())
        .await
    {
        Ok((events, has_more)) => Json(json!({
            "ok": true,
            "events": events,
            "has_more": has_more
        }))
        .into_response(),
        Err(error) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "ok": false, "error": error.to_string(), "events": [] })),
        )
            .into_response(),
    }
}

async fn noop_ok(State(state): State<ProxyState>) -> impl IntoResponse {
    let _ = state
        .store
        .record_event(
            "info",
            "manager_action",
            "Manager action acknowledged in inline proxy mode.",
            None,
        )
        .await;
    Json(json!({ "ok": true, "mode": "inline" }))
}

async fn generate_adapter(State(state): State<ProxyState>) -> impl IntoResponse {
    let config = state.active_config();
    let user_config = UserConfig::read_from(&config.config_path()).unwrap_or_default();
    let catalog_mode = user_config
        .catalog
        .and_then(|value| value.mode)
        .map(|value| normalize_catalog_mode(&value).to_owned())
        .unwrap_or_else(|| "default".to_owned());
    match ensure_catalog(&config) {
        Ok(()) => Json(json!({
            "ok": true,
            "ready": true,
            "catalog_mode": catalog_mode,
            "catalog_path": config.catalog_path().to_string_lossy(),
            "models": available_models().into_iter().map(|m| m.slug).collect::<Vec<_>>(),
            "context_window": 1_000_000,
            "effective_context_window_percent": 90,
            "toml_snippet": codex_toml_snippet(&config.catalog_path(), &config.proxy_base_url())
        }))
        .into_response(),
        Err(error) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "ok": false, "error": error.to_string() })),
        )
            .into_response(),
    }
}

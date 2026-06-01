use codeseex_core::models::{TemperaturePreset, UpstreamModelOverride};
use codeseex_core::{
    AppConfig, UserBillingConfig, UserCatalogConfig, UserConfig, UserModelConfig, UserProxyConfig,
    UserToolsConfig, UserUiConfig, UserUpstreamConfig,
};
use serde_json::Value;
use std::collections::{BTreeMap, HashSet};

pub(crate) fn user_config_from_payload(
    payload: &Value,
    mut config: UserConfig,
    app_config: &AppConfig,
) -> UserConfig {
    if payload.get("PROXY_PORT").is_some() {
        let proxy = config.proxy.get_or_insert_with(UserProxyConfig::default);
        proxy.port = value_u16(payload, "PROXY_PORT");
    }

    if payload.get("DEEPSEEK_BASE_URL").is_some()
        || payload.get("DEEPSEEK_OFFICIAL_V1_COMPAT").is_some()
        || payload.get("DEEPSEEK_API_KEY").is_some()
    {
        let upstream = config
            .upstream
            .get_or_insert_with(UserUpstreamConfig::default);
        if payload.get("DEEPSEEK_BASE_URL").is_some() {
            upstream.base_url = value_string(payload, "DEEPSEEK_BASE_URL");
        }
        if payload.get("DEEPSEEK_OFFICIAL_V1_COMPAT").is_some() {
            upstream.official_v1_compat = value_bool(payload, "DEEPSEEK_OFFICIAL_V1_COMPAT");
        }
        if payload.get("DEEPSEEK_API_KEY").is_some() {
            upstream.api_key = value_string(payload, "DEEPSEEK_API_KEY");
        }
    }

    if payload.get("UPSTREAM_MODEL_OVERRIDE").is_some()
        || payload.get("DEEPSEEK_TEMPERATURE_PRESET").is_some()
        || payload.get("DEEPSEEK_THINKING").is_some()
    {
        let model = config.model.get_or_insert_with(UserModelConfig::default);
        if payload.get("UPSTREAM_MODEL_OVERRIDE").is_some() {
            model.override_mode = value_model_override(payload, "UPSTREAM_MODEL_OVERRIDE");
        }
        if payload.get("DEEPSEEK_TEMPERATURE_PRESET").is_some() {
            model.temperature = value_temperature(payload, "DEEPSEEK_TEMPERATURE_PRESET");
        }
        if payload.get("DEEPSEEK_THINKING").is_some() {
            model.thinking = value_string(payload, "DEEPSEEK_THINKING");
        }
    }

    if payload.get("CATALOG_MODE").is_some() {
        config
            .catalog
            .get_or_insert_with(UserCatalogConfig::default)
            .mode = value_string(payload, "CATALOG_MODE")
            .map(|value| normalize_catalog_mode(&value).to_owned());
    }

    if payload.get("UI_THEME").is_some()
        || payload.get("UI_LANGUAGE").is_some()
        || payload.get("SHOW_THINKING").is_some()
        || payload.get("AUTO_START").is_some()
        || payload.get("UI_CLOSE_BEHAVIOR").is_some()
        || payload.get("LOG_RETENTION_DAYS").is_some()
    {
        let ui = config.ui.get_or_insert_with(UserUiConfig::default);
        if payload.get("UI_THEME").is_some() {
            ui.theme = value_string(payload, "UI_THEME");
        }
        if payload.get("UI_LANGUAGE").is_some() {
            ui.language = value_string(payload, "UI_LANGUAGE");
        }
        if payload.get("SHOW_THINKING").is_some() {
            ui.show_thinking = value_bool(payload, "SHOW_THINKING");
        }
        if payload.get("AUTO_START").is_some() {
            ui.auto_start = value_bool(payload, "AUTO_START");
        }
        if payload.get("UI_CLOSE_BEHAVIOR").is_some() {
            ui.close_behavior = value_string(payload, "UI_CLOSE_BEHAVIOR");
        }
        if payload.get("LOG_RETENTION_DAYS").is_some() {
            ui.log_retention_days = value_u16(payload, "LOG_RETENTION_DAYS");
        }
    }

    if payload.get("BILLING_FLASH_CACHED_INPUT_CNY").is_some()
        || payload.get("BILLING_FLASH_CACHE_MISS_INPUT_CNY").is_some()
        || payload.get("BILLING_FLASH_OUTPUT_CNY").is_some()
        || payload.get("BILLING_PRO_CACHED_INPUT_CNY").is_some()
        || payload.get("BILLING_PRO_CACHE_MISS_INPUT_CNY").is_some()
        || payload.get("BILLING_PRO_OUTPUT_CNY").is_some()
    {
        let billing = config
            .billing
            .get_or_insert_with(UserBillingConfig::default);
        if payload.get("BILLING_FLASH_CACHED_INPUT_CNY").is_some() {
            billing.flash_cached_input_cny = value_f64(payload, "BILLING_FLASH_CACHED_INPUT_CNY");
        }
        if payload.get("BILLING_FLASH_CACHE_MISS_INPUT_CNY").is_some() {
            billing.flash_cache_miss_input_cny =
                value_f64(payload, "BILLING_FLASH_CACHE_MISS_INPUT_CNY");
        }
        if payload.get("BILLING_FLASH_OUTPUT_CNY").is_some() {
            billing.flash_output_cny = value_f64(payload, "BILLING_FLASH_OUTPUT_CNY");
        }
        if payload.get("BILLING_PRO_CACHED_INPUT_CNY").is_some() {
            billing.pro_cached_input_cny = value_f64(payload, "BILLING_PRO_CACHED_INPUT_CNY");
        }
        if payload.get("BILLING_PRO_CACHE_MISS_INPUT_CNY").is_some() {
            billing.pro_cache_miss_input_cny =
                value_f64(payload, "BILLING_PRO_CACHE_MISS_INPUT_CNY");
        }
        if payload.get("BILLING_PRO_OUTPUT_CNY").is_some() {
            billing.pro_output_cny = value_f64(payload, "BILLING_PRO_OUTPUT_CNY");
        }
    }

    let community_config_keys =
        crate::community_tools::community_tool_config_keys(&app_config.data_dir);
    let has_tool_settings = community_config_keys
        .iter()
        .any(|key| payload.get(key).is_some());
    if payload.get("ENABLED_TOOLS").is_some() || has_tool_settings {
        let tools = config.tools.get_or_insert_with(UserToolsConfig::default);
        if payload.get("ENABLED_TOOLS").is_some() {
            tools.enabled = value_string_list(payload, "ENABLED_TOOLS").map(configurable_tool_ids);
        }
        if has_tool_settings {
            let settings = tools.settings.get_or_insert_with(BTreeMap::new);
            for key in community_config_keys {
                let Some(value) = payload.get(&key) else {
                    continue;
                };
                if value.is_null() {
                    settings.remove(&key);
                } else if let Some(value) = crate::community_tools::value_to_setting_string(value) {
                    settings.insert(key, value);
                }
            }
        }
    }

    config
}

fn configurable_tool_ids(ids: Vec<String>) -> Vec<String> {
    let mut seen = HashSet::new();
    ids.into_iter()
        .filter(|id| {
            !matches!(
                id.as_str(),
                "apply_patch" | "web_search" | "mcp" | "mcp_server"
            )
        })
        .filter(|id| seen.insert(id.clone()))
        .collect()
}

fn value_string(payload: &Value, key: &str) -> Option<String> {
    payload
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
}

fn value_bool(payload: &Value, key: &str) -> Option<bool> {
    let value = payload.get(key)?;
    if let Some(value) = value.as_bool() {
        return Some(value);
    }
    let value = value.as_str()?.trim().to_ascii_lowercase();
    Some(matches!(
        value.as_str(),
        "1" | "true" | "yes" | "on" | "enabled"
    ))
}

fn value_string_list(payload: &Value, key: &str) -> Option<Vec<String>> {
    let value = payload.get(key)?;
    if let Some(items) = value.as_array() {
        return Some(normalize_string_list(
            items.iter().filter_map(Value::as_str),
        ));
    }
    let text = value.as_str()?.trim();
    if text.is_empty() {
        return Some(Vec::new());
    }
    if let Ok(parsed) = serde_json::from_str::<Vec<String>>(text) {
        return Some(normalize_string_list(parsed.iter().map(String::as_str)));
    }
    Some(normalize_string_list(text.split(',')))
}

fn normalize_string_list<'a>(items: impl Iterator<Item = &'a str>) -> Vec<String> {
    let mut output = Vec::new();
    for item in items {
        let normalized = item
            .trim()
            .to_ascii_lowercase()
            .chars()
            .map(|ch| {
                if ch.is_ascii_alphanumeric() || ch == '_' || ch == '-' {
                    ch
                } else {
                    '_'
                }
            })
            .collect::<String>();
        let normalized = normalized.trim_matches('_').to_owned();
        if normalized.is_empty() || output.contains(&normalized) {
            continue;
        }
        output.push(normalized);
    }
    output.sort();
    output
}

fn value_u16(payload: &Value, key: &str) -> Option<u16> {
    if let Some(value) = payload.get(key).and_then(Value::as_u64) {
        return u16::try_from(value).ok();
    }
    payload
        .get(key)
        .and_then(Value::as_str)
        .and_then(|value| value.trim().parse().ok())
}

fn value_f64(payload: &Value, key: &str) -> Option<f64> {
    if let Some(value) = payload.get(key).and_then(Value::as_f64) {
        return Some(value);
    }
    payload
        .get(key)
        .and_then(Value::as_str)
        .and_then(|value| value.trim().parse().ok())
}

fn value_model_override(payload: &Value, key: &str) -> Option<UpstreamModelOverride> {
    match value_string(payload, key)?.to_ascii_lowercase().as_str() {
        "flash" | "deepseek-v4-flash" => Some(UpstreamModelOverride::Flash),
        "pro" | "deepseek-v4-pro" => Some(UpstreamModelOverride::Pro),
        "default" => Some(UpstreamModelOverride::Default),
        _ => None,
    }
}

fn value_temperature(payload: &Value, key: &str) -> Option<TemperaturePreset> {
    match value_string(payload, key)?.to_ascii_lowercase().as_str() {
        "strict" => Some(TemperaturePreset::Strict),
        "balanced" => Some(TemperaturePreset::Balanced),
        "general" => Some(TemperaturePreset::General),
        "creative" => Some(TemperaturePreset::Creative),
        "default" => Some(TemperaturePreset::Default),
        _ => None,
    }
}

pub(crate) fn model_override_to_ui(value: UpstreamModelOverride) -> &'static str {
    match value {
        UpstreamModelOverride::Default => "default",
        UpstreamModelOverride::Flash => "deepseek-v4-flash",
        UpstreamModelOverride::Pro => "deepseek-v4-pro",
    }
}

pub(crate) fn temperature_to_ui(value: TemperaturePreset) -> &'static str {
    match value {
        TemperaturePreset::Default => "default",
        TemperaturePreset::Strict => "strict",
        TemperaturePreset::Balanced => "balanced",
        TemperaturePreset::General => "general",
        TemperaturePreset::Creative => "creative",
    }
}

pub(crate) fn normalize_catalog_mode(value: &str) -> &'static str {
    match value.trim().to_ascii_lowercase().as_str() {
        "auto" => "auto",
        "builtin" | "built-in" | "built_in" => "builtin",
        _ => "default",
    }
}

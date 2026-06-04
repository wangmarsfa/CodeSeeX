use crate::models::{available_models, ModelInfo};
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Catalog {
    pub models: Vec<CatalogModel>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CatalogModel {
    pub slug: String,
    pub display_name: String,
    pub description: String,
    pub context_window: u64,
    pub effective_context_window_percent: u8,
    pub max_output_tokens: u64,
    pub priority: u32,
    pub available_plans: Vec<String>,
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}

pub fn build_codeseex_catalog() -> Catalog {
    Catalog {
        models: available_models()
            .into_iter()
            .enumerate()
            .map(|(index, model)| catalog_model(model, index))
            .collect(),
    }
}

pub fn write_catalog_atomic(path: &Path, catalog: &Catalog) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("create catalog directory {}", parent.display()))?;
    }
    let temp = path.with_extension("json.tmp");
    let text = serde_json::to_string_pretty(catalog)? + "\n";
    fs::write(&temp, text).with_context(|| format!("write temp catalog {}", temp.display()))?;
    fs::rename(&temp, path).with_context(|| format!("replace catalog {}", path.display()))?;
    Ok(())
}

pub fn codex_toml_snippet(catalog_path: &Path, base_url: &str) -> String {
    [
        "model_provider = \"custom\"".to_owned(),
        "model = \"deepseek-v4-pro\"".to_owned(),
        "disable_response_storage = true".to_owned(),
        "model_reasoning_effort = \"xhigh\"".to_owned(),
        format!(
            "model_catalog_json = {}",
            toml_string(catalog_path.to_string_lossy().as_ref())
        ),
        "".to_owned(),
        "[model_providers.custom]".to_owned(),
        "name = \"DeepSeek\"".to_owned(),
        "wire_api = \"responses\"".to_owned(),
        "requires_openai_auth = true".to_owned(),
        format!("base_url = {}", toml_string(base_url)),
    ]
    .join("\n")
}

fn catalog_model(model: ModelInfo, index: usize) -> CatalogModel {
    let mut extra = BTreeMap::new();
    let auto_compact_token_limit =
        model.context_window * u64::from(model.effective_context_window_percent) / 100;
    let is_default = model.slug == "deepseek-v4-pro";
    extra.insert("id".to_owned(), json!(model.slug));
    extra.insert("model".to_owned(), json!(model.slug));
    extra.insert("displayName".to_owned(), json!(model.display_name));
    extra.insert("visibility".to_owned(), json!("list"));
    extra.insert("hidden".to_owned(), json!(false));
    extra.insert("isDefault".to_owned(), json!(is_default));
    extra.insert("supported_in_api".to_owned(), json!(true));
    extra.insert("shell_type".to_owned(), json!("shell_command"));
    extra.insert("base_model".to_owned(), json!("gpt-5.5"));
    extra.insert("supports_reasoning".to_owned(), json!(true));
    extra.insert("supports_streaming".to_owned(), json!(true));
    extra.insert("default_reasoning_level".to_owned(), json!("medium"));
    extra.insert(
        "supported_reasoning_levels".to_owned(),
        json!(default_reasoning_levels()),
    );
    extra.insert("defaultReasoningEffort".to_owned(), json!("medium"));
    extra.insert(
        "supportedReasoningEfforts".to_owned(),
        json!(["low", "medium", "high", "xhigh"]),
    );
    extra.insert("supports_reasoning_summaries".to_owned(), json!(true));
    extra.insert("default_reasoning_summary".to_owned(), json!("none"));
    extra.insert("support_verbosity".to_owned(), json!(true));
    extra.insert("default_verbosity".to_owned(), json!("low"));
    extra.insert("apply_patch_tool_type".to_owned(), json!("freeform"));
    extra.insert("web_search_tool_type".to_owned(), json!("text_and_image"));
    extra.insert(
        "truncation_policy".to_owned(),
        json!({ "mode": "tokens", "limit": 10000 }),
    );
    extra.insert("supports_parallel_tool_calls".to_owned(), json!(true));
    extra.insert("supports_image_detail_original".to_owned(), json!(true));
    extra.insert("max_context_window".to_owned(), json!(model.context_window));
    extra.insert(
        "auto_compact_token_limit".to_owned(),
        json!(auto_compact_token_limit),
    );
    extra.insert("experimental_supported_tools".to_owned(), json!([]));
    extra.insert("input_modalities".to_owned(), json!(["text", "image"]));
    extra.insert("supports_search_tool".to_owned(), json!(true));
    extra.insert("supportsPersonality".to_owned(), json!(true));
    extra.insert("additional_speed_tiers".to_owned(), json!(["fast"]));
    extra.insert("additionalSpeedTiers".to_owned(), json!(["fast"]));
    extra.insert("service_tiers".to_owned(), json!([]));
    extra.insert("serviceTiers".to_owned(), json!([]));
    extra.insert("available_in_plans".to_owned(), json!(default_plans()));
    extra.insert("minimal_client_version".to_owned(), json!("0.98.0"));
    extra.insert("codeseex_next".to_owned(), json!(true));

    CatalogModel {
        slug: model.slug,
        display_name: model.display_name,
        description: model.description,
        context_window: model.context_window,
        effective_context_window_percent: model.effective_context_window_percent,
        max_output_tokens: 64_000,
        priority: 10 + index as u32,
        available_plans: default_plans(),
        extra,
    }
}

fn default_reasoning_levels() -> Vec<Value> {
    vec![
        json!({ "effort": "low", "description": "Fast responses with lighter reasoning" }),
        json!({ "effort": "medium", "description": "Balances speed and reasoning depth for everyday tasks" }),
        json!({ "effort": "high", "description": "Greater reasoning depth for complex problems" }),
        json!({ "effort": "xhigh", "description": "Extra high reasoning depth for complex problems" }),
    ]
}

fn default_plans() -> Vec<String> {
    [
        "free",
        "plus",
        "pro",
        "team",
        "business",
        "enterprise",
        "edu",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect()
}

fn toml_string(value: &str) -> String {
    serde_json::to_string(value).unwrap_or_else(|_| "\"\"".to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalog_contains_both_models() {
        let catalog = build_codeseex_catalog();
        let slugs: Vec<_> = catalog
            .models
            .iter()
            .map(|model| model.slug.as_str())
            .collect();
        assert!(slugs.contains(&"deepseek-v4-flash"));
        assert!(slugs.contains(&"deepseek-v4-pro"));
    }

    #[test]
    fn catalog_preserves_codex_desktop_capability_fields() {
        let catalog = build_codeseex_catalog();
        for model in catalog.models {
            assert_eq!(
                model
                    .extra
                    .get("apply_patch_tool_type")
                    .and_then(Value::as_str),
                Some("freeform")
            );
            assert_eq!(
                model
                    .extra
                    .get("web_search_tool_type")
                    .and_then(Value::as_str),
                Some("text_and_image")
            );
            assert_eq!(
                model
                    .extra
                    .get("supports_search_tool")
                    .and_then(Value::as_bool),
                Some(true)
            );
            assert_eq!(
                model
                    .extra
                    .get("supports_parallel_tool_calls")
                    .and_then(Value::as_bool),
                Some(true)
            );
            assert_eq!(
                model
                    .extra
                    .get("auto_compact_token_limit")
                    .and_then(Value::as_u64),
                Some(900_000)
            );
        }
    }

    #[test]
    fn toml_snippet_contains_catalog_and_proxy() {
        let snippet = codex_toml_snippet(
            Path::new("C:/Users/test/.codeseex-next/model-catalog.json"),
            "http://127.0.0.1:8787/v1",
        );
        assert!(snippet.contains("model_catalog_json"));
        assert!(snippet.contains("http://127.0.0.1:8787/v1"));
    }
}

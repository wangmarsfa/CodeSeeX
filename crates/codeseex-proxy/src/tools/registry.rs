use codeseex_core::{AppConfig, UserConfig};
use serde_json::{json, Value};
use std::collections::{BTreeMap, HashSet};

pub(crate) fn tool_registry(
    config: &AppConfig,
    enabled_tools: &[String],
    settings: &BTreeMap<String, String>,
) -> Value {
    let mut tools = match json!([
        {
            "id": "apply_patch",
            "name": "Apply Patch",
            "description": "Codex-native patch editing capability. CodeSeeX tracks it as a system tool and does not expose a client-side switch.",
            "source": "builtin",
            "system": true,
            "configurable": false,
            "enabled": true,
            "iconPath": "/assets/icons/apply-patch.svg",
            "labels": [
                { "id": "system", "labelKey": "toolLabelSystem", "label": "System" },
                { "id": "built_in", "labelKey": "toolLabelBuiltIn", "label": "Built-in" }
            ]
        },
        {
            "id": "web_search",
            "name": "Web Search",
            "description": "System web search and public page opener executed by the Rust proxy. It is always available and has no client-side switch.",
            "source": "builtin",
            "system": true,
            "configurable": false,
            "enabled": true,
            "iconPath": "/assets/icons/web-search.svg",
            "labels": [
                { "id": "system", "labelKey": "toolLabelSystem", "label": "System" },
                { "id": "built_in", "labelKey": "toolLabelBuiltIn", "label": "Built-in" }
            ]
        },
        {
            "id": "mcp_server",
            "name": "MCP Server",
            "description": "Codex-native MCP discovery and invocation. Configuration remains in Codex, not in CodeSeeX.",
            "source": "builtin",
            "system": true,
            "configurable": false,
            "enabled": true,
            "iconPath": "/assets/icons/tools.svg",
            "labels": [
                { "id": "system", "labelKey": "toolLabelSystem", "label": "System" },
                { "id": "built_in", "labelKey": "toolLabelBuiltIn", "label": "Built-in" }
            ]
        },
        {
            "id": "list_directory",
            "name": "List Directory",
            "description": "Built-in read-only workspace directory listing tool executed by the Rust proxy.",
            "source": "builtin",
            "system": false,
            "configurable": true,
            "enabled": builtin_tool_enabled(enabled_tools, "list_directory"),
            "iconPath": "/assets/icons/tools.svg",
            "labels": [{ "id": "built_in", "labelKey": "toolLabelBuiltIn", "label": "Built-in" }]
        },
        {
            "id": "read_file_range",
            "name": "Read File Range",
            "description": "Built-in read-only text file range reader executed by the Rust proxy.",
            "source": "builtin",
            "system": false,
            "configurable": true,
            "enabled": builtin_tool_enabled(enabled_tools, "read_file_range"),
            "iconPath": "/assets/icons/tools.svg",
            "labels": [{ "id": "built_in", "labelKey": "toolLabelBuiltIn", "label": "Built-in" }]
        },
        {
            "id": "workspace_search",
            "name": "Workspace Search",
            "description": "Built-in read-only workspace text search executed by the Rust proxy.",
            "source": "builtin",
            "system": false,
            "configurable": true,
            "enabled": builtin_tool_enabled(enabled_tools, "workspace_search"),
            "iconPath": "/assets/icons/tools.svg",
            "labels": [{ "id": "built_in", "labelKey": "toolLabelBuiltIn", "label": "Built-in" }]
        }
    ]) {
        Value::Array(items) => items,
        _ => Vec::new(),
    };
    tools.extend(crate::community_tools::list_community_tools(
        &config.data_dir,
        enabled_tools,
        settings,
    ));
    Value::Array(tools)
}

pub(crate) fn enabled_tool_ids(config: &AppConfig) -> Vec<String> {
    UserConfig::read_from(&config.config_path())
        .ok()
        .and_then(|user_config| user_config.tools.and_then(|tools| tools.enabled))
        .unwrap_or_else(crate::tools::default_enabled_tool_ids)
}

pub(crate) fn tool_settings(config: &AppConfig) -> BTreeMap<String, String> {
    UserConfig::read_from(&config.config_path())
        .ok()
        .and_then(|user_config| user_config.tools.and_then(|tools| tools.settings))
        .unwrap_or_default()
}

pub(crate) fn dedupe_tool_definitions(tools: Vec<Value>) -> Vec<Value> {
    let mut seen = HashSet::new();
    tools
        .into_iter()
        .filter(|tool| {
            let Some(name) = tool.pointer("/function/name").and_then(Value::as_str) else {
                return true;
            };
            seen.insert(name.to_owned())
        })
        .collect()
}

pub(crate) fn normalized_tool_choice(choice: Option<&Value>, tools: &[Value]) -> Option<Value> {
    let choice = choice?;
    if let Some(value) = choice.as_str() {
        return matches!(value, "auto" | "none" | "required")
            .then(|| Value::String(value.to_owned()));
    }
    let name = choice
        .get("name")
        .or_else(|| choice.pointer("/function/name"))
        .or_else(|| choice.get("type"))
        .and_then(Value::as_str)
        .map(|value| {
            if value == "web_search_preview" {
                "web_search"
            } else {
                value
            }
        })?;
    if !tools.iter().any(|tool| {
        tool.pointer("/function/name")
            .and_then(Value::as_str)
            .map(|tool_name| tool_name == name)
            .unwrap_or(false)
    }) {
        return None;
    }
    Some(json!({ "type": "function", "function": { "name": name } }))
}

fn builtin_tool_enabled(enabled_tools: &[String], id: &str) -> bool {
    enabled_tools.iter().any(|enabled_id| enabled_id == id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tool_choice_none_is_not_rewritten_to_auto() {
        let tools = vec![json!({
            "type": "function",
            "function": { "name": "web_search" }
        })];

        assert_eq!(
            normalized_tool_choice(Some(&json!("none")), &tools),
            Some(json!("none"))
        );
    }

    #[test]
    fn tool_choice_preview_maps_to_web_search_when_available() {
        let tools = vec![json!({
            "type": "function",
            "function": { "name": "web_search" }
        })];

        assert_eq!(
            normalized_tool_choice(Some(&json!({ "type": "web_search_preview" })), &tools),
            Some(json!({ "type": "function", "function": { "name": "web_search" } }))
        );
    }
}

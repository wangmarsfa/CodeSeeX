use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};
use uuid::Uuid;

use crate::tools::ownership::ChatToolCall;

const CODEX_TOOL_SEARCH_NAMES: &[&str] = &["tool_search_tool", "tool_search"];
const MAX_MALFORMED_ARGUMENTS_CHARS: usize = 2_048;
const MAX_EXTERNAL_TOOL_DECLARATIONS: usize = 128;
const MAX_EXTERNAL_TOOL_DESCRIPTION_CHARS: usize = 2_000;
const MAX_EXTERNAL_TOOL_SCHEMA_CHARS: usize = 24_000;
const MAX_EXTERNAL_TOOLS_PAYLOAD_CHARS: usize = 160_000;
const MAX_TOOL_BUDGET_DIAGNOSTIC_NAMES: usize = 20;

#[derive(Debug, Clone, Default)]
pub struct ToolContext {
    entries: BTreeMap<String, ToolEntry>,
    pub upstream_tools: Vec<Value>,
    request_tool_items: usize,
    source_names: Vec<String>,
    discovered_tool_items: usize,
    budget: ToolBudgetStats,
}

#[derive(Debug, Clone, Default)]
struct ToolBudgetStats {
    attempted_declarations: usize,
    accepted_declarations: usize,
    budgeted_declarations: usize,
    exempt_declarations: usize,
    payload_chars: usize,
    truncated_descriptions: usize,
    dropped_by_count: usize,
    dropped_by_schema_size: usize,
    dropped_by_payload_size: usize,
    dropped_names: Vec<String>,
}

#[derive(Debug, Clone)]
struct ToolEntry {
    response_name: String,
    namespace: Option<String>,
    kind: ToolEntryKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ToolEntryKind {
    Function,
    CodexToolSearch,
}

impl ToolContext {
    pub fn from_request_tools(tools: Option<&Value>) -> Self {
        let mut context = Self::default();
        let Some(Value::Array(items)) = tools else {
            return context;
        };
        context.request_tool_items = items.len();

        for (index, tool) in items.iter().enumerate() {
            for declaration in normalize_tool_declarations(tool, index) {
                if is_conflicting_visual_tool(&declaration.response_name) {
                    continue;
                }
                context.push_external_declaration(declaration);
            }
        }

        context
    }

    pub fn promote_codex_tool_search_tools(&mut self) {
        for entry in self.entries.values_mut() {
            if CODEX_TOOL_SEARCH_NAMES.contains(&entry.response_name.as_str()) {
                entry.kind = ToolEntryKind::CodexToolSearch;
            }
        }
    }

    pub fn add_tool_search_output_tools(
        &mut self,
        input: Option<&Value>,
        valid_previous_call_ids: &BTreeSet<String>,
    ) {
        let existing_request_tools = self.response_tool_name_set();
        let Some(input) = input else {
            return;
        };
        for tool in tool_search_output_tool_declarations(input, valid_previous_call_ids) {
            for declaration in normalize_tool_declarations(&tool, self.request_tool_items) {
                if is_conflicting_visual_tool(&declaration.response_name)
                    || existing_request_tools.contains(&declaration.response_name)
                {
                    continue;
                }
                self.discovered_tool_items = self.discovered_tool_items.saturating_add(1);
                self.push_external_declaration(declaration);
            }
        }
    }

    pub fn has_external_tool(&self, name: &str) -> bool {
        self.entries.contains_key(name)
    }

    pub fn has_response_tool(&self, name: &str) -> bool {
        self.entries
            .values()
            .any(|entry| entry.response_name == name)
            || self.entries.contains_key(name)
    }

    pub fn has_any_response_tool(&self, names: &[&str]) -> bool {
        names.iter().any(|name| self.has_response_tool(name))
    }

    pub fn request_tool_items(&self) -> usize {
        self.request_tool_items
    }

    pub fn source_names(&self) -> Vec<String> {
        self.source_names.clone()
    }

    pub fn upstream_names(&self) -> Vec<String> {
        self.entries.keys().cloned().collect()
    }

    pub fn discovered_tool_items(&self) -> usize {
        self.discovered_tool_items
    }

    pub fn external_tool_budget_diagnostic(&self) -> Value {
        json!({
            "attempted_declarations": self.budget.attempted_declarations,
            "accepted_declarations": self.budget.accepted_declarations,
            "budgeted_declarations": self.budget.budgeted_declarations,
            "exempt_declarations": self.budget.exempt_declarations,
            "payload_chars": self.budget.payload_chars,
            "limits": {
                "max_declarations": MAX_EXTERNAL_TOOL_DECLARATIONS,
                "max_description_chars": MAX_EXTERNAL_TOOL_DESCRIPTION_CHARS,
                "max_schema_chars": MAX_EXTERNAL_TOOL_SCHEMA_CHARS,
                "max_payload_chars": MAX_EXTERNAL_TOOLS_PAYLOAD_CHARS
            },
            "truncated_descriptions": self.budget.truncated_descriptions,
            "dropped": {
                "count_limit": self.budget.dropped_by_count,
                "schema_size": self.budget.dropped_by_schema_size,
                "payload_size": self.budget.dropped_by_payload_size,
                "sample_names": self.budget.dropped_names
            }
        })
    }

    fn response_tool_name_set(&self) -> BTreeSet<String> {
        self.entries
            .values()
            .map(|entry| entry.response_name.clone())
            .chain(self.entries.keys().cloned())
            .collect()
    }

    pub fn ensure_codex_tool_search_bridge(&mut self) {
        if self.has_response_tool("tool_search_tool") || self.has_response_tool("tool_search") {
            return;
        }
        self.push_internal_declaration(ToolDeclaration {
            response_name: "tool_search_tool".to_owned(),
            description: "Search deferred Codex native tool metadata, such as sub-agents, computer-use, thread, automation, or plugin runtime tools, and expose matching tools for the next model turn.".to_owned(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "query": {
                        "type": "string",
                        "description": "Search query for deferred Codex tools, such as sub-agent, computer-use, thread, automation, or plugin runtime tools."
                    },
                    "limit": {
                        "type": "integer",
                        "description": "Maximum number of tool matches to return."
                    }
                },
                "required": ["query"],
                "additionalProperties": false
            }),
            namespace: None,
            kind: ToolEntryKind::CodexToolSearch,
        });
    }

    fn push_external_declaration(&mut self, declaration: ToolDeclaration) {
        self.budget.attempted_declarations = self.budget.attempted_declarations.saturating_add(1);
        if self.budget.budgeted_declarations >= MAX_EXTERNAL_TOOL_DECLARATIONS {
            self.budget.dropped_by_count = self.budget.dropped_by_count.saturating_add(1);
            self.remember_dropped_tool_name(&declaration.response_name);
            return;
        }

        let schema_chars = serialized_json_chars(&declaration.parameters);
        if schema_chars > MAX_EXTERNAL_TOOL_SCHEMA_CHARS {
            self.budget.dropped_by_schema_size =
                self.budget.dropped_by_schema_size.saturating_add(1);
            self.remember_dropped_tool_name(&declaration.response_name);
            return;
        }

        let mut declaration = declaration;
        if declaration.description.chars().count() > MAX_EXTERNAL_TOOL_DESCRIPTION_CHARS {
            declaration.description = truncate_chars(
                &declaration.description,
                MAX_EXTERNAL_TOOL_DESCRIPTION_CHARS.saturating_sub(3),
            );
            self.budget.truncated_descriptions =
                self.budget.truncated_descriptions.saturating_add(1);
        }

        let Some(tool_value) = self.tool_value_for_declaration(&declaration) else {
            return;
        };
        let tool_payload_chars = serialized_json_chars(&tool_value.value);
        if self.budget.payload_chars.saturating_add(tool_payload_chars)
            > MAX_EXTERNAL_TOOLS_PAYLOAD_CHARS
        {
            self.budget.dropped_by_payload_size =
                self.budget.dropped_by_payload_size.saturating_add(1);
            self.remember_dropped_tool_name(&declaration.response_name);
            return;
        }

        self.push_tool_value(declaration, tool_value);
        self.budget.accepted_declarations = self.budget.accepted_declarations.saturating_add(1);
        self.budget.payload_chars = self.budget.payload_chars.saturating_add(tool_payload_chars);
        self.budget.budgeted_declarations = self.budget.budgeted_declarations.saturating_add(1);
    }

    fn push_internal_declaration(&mut self, declaration: ToolDeclaration) {
        if let Some(tool_value) = self.tool_value_for_declaration(&declaration) {
            self.push_tool_value(declaration, tool_value);
        }
    }

    fn tool_value_for_declaration(
        &self,
        declaration: &ToolDeclaration,
    ) -> Option<PreparedToolValue> {
        let upstream_name = unique_tool_name(
            &declaration.response_name,
            declaration.namespace.as_deref(),
            &self.entries,
        );
        let value = json!({
            "type": "function",
            "function": {
                "name": upstream_name,
                "description": declaration.description,
                "parameters": declaration.parameters
            }
        });
        Some(PreparedToolValue {
            upstream_name,
            value,
        })
    }

    fn push_tool_value(&mut self, declaration: ToolDeclaration, tool_value: PreparedToolValue) {
        self.upstream_tools.push(tool_value.value);
        self.source_names.push(declaration.response_name.clone());
        self.entries.insert(
            tool_value.upstream_name,
            ToolEntry {
                response_name: declaration.response_name,
                namespace: declaration.namespace,
                kind: declaration.kind,
            },
        );
    }

    fn remember_dropped_tool_name(&mut self, name: &str) {
        if self.budget.dropped_names.len() < MAX_TOOL_BUDGET_DIAGNOSTIC_NAMES {
            self.budget.dropped_names.push(name.to_owned());
        }
    }

    pub fn response_item_from_chat_call(&self, call: &ChatToolCall) -> Value {
        self.response_item_from_chat_call_with_id(call, &format!("fc_{}", Uuid::new_v4().simple()))
    }

    pub fn response_item_from_chat_call_with_id(
        &self,
        call: &ChatToolCall,
        item_id: &str,
    ) -> Value {
        let entry = self.entries.get(&call.name);
        if entry
            .map(|value| value.kind == ToolEntryKind::CodexToolSearch)
            .unwrap_or(false)
        {
            let parsed_arguments = parse_tool_arguments(&call.arguments);
            let mut item = json!({
                "id": item_id,
                "type": "tool_search_call",
                "status": "completed",
                "execution": "client",
                "call_id": call.id,
                "arguments": parsed_arguments
            });
            if let Some(metadata) = malformed_tool_arguments_metadata(&call.arguments) {
                item["metadata"] = metadata;
            }
            return item;
        }

        let mut item = json!({
            "id": item_id,
            "type": "function_call",
            "status": "completed",
            "call_id": call.id,
            "name": entry.map(|value| value.response_name.as_str()).unwrap_or(call.name.as_str()),
            "arguments": call.arguments
        });
        if let Some(namespace) = entry.and_then(|value| value.namespace.as_deref()) {
            item["namespace"] = Value::String(namespace.to_owned());
        }
        if let Some(entry) = entry {
            if call.name != entry.response_name {
                item["metadata"] = json!({
                    "codeseex_upstream_name": call.name,
                    "codeseex_response_name": entry.response_name
                });
            }
        }
        item
    }

    pub fn is_codex_tool_search_tool(&self, name: &str) -> bool {
        self.entries
            .get(name)
            .map(|entry| entry.kind == ToolEntryKind::CodexToolSearch)
            .unwrap_or(false)
    }
}

#[derive(Debug)]
struct ToolDeclaration {
    response_name: String,
    description: String,
    parameters: Value,
    namespace: Option<String>,
    kind: ToolEntryKind,
}

#[derive(Debug)]
struct PreparedToolValue {
    upstream_name: String,
    value: Value,
}

fn normalize_tool_declarations(tool: &Value, index: usize) -> Vec<ToolDeclaration> {
    let namespace = namespace_from_tool(tool);
    let tool_type = tool
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_ascii_lowercase();
    if tool_type == "mcp" && tool.get("tools").and_then(Value::as_array).is_none() {
        return Vec::new();
    }
    if let Some(nested_tools) = tool.get("tools").and_then(Value::as_array) {
        return nested_tools
            .iter()
            .enumerate()
            .filter_map(|(nested_index, nested)| {
                normalize_single_tool(nested, nested_index, namespace.clone())
            })
            .collect();
    }

    normalize_single_tool(tool, index, namespace)
        .map(|value| vec![value])
        .unwrap_or_default()
}

fn normalize_single_tool(
    tool: &Value,
    index: usize,
    namespace: Option<String>,
) -> Option<ToolDeclaration> {
    let object = tool.as_object()?;
    let nested_tool = object.get("tool").filter(|value| value.is_object());
    let name = first_string(
        &[
            object.get("name"),
            object.get("function").and_then(|value| value.get("name")),
            nested_tool.and_then(|value| value.get("name")),
            object.get("server_label"),
        ],
        &format!("tool_{}", index + 1),
    );
    let description = first_string(
        &[
            object.get("description"),
            object
                .get("function")
                .and_then(|value| value.get("description")),
            nested_tool.and_then(|value| value.get("description")),
            object.get("title"),
        ],
        &name,
    );
    let parameters = object
        .get("parameters")
        .cloned()
        .or_else(|| {
            object
                .get("function")
                .and_then(|value| value.get("parameters"))
                .cloned()
        })
        .or_else(|| object.get("input_schema").cloned())
        .or_else(|| object.get("inputSchema").cloned())
        .or_else(|| {
            nested_tool
                .and_then(|value| value.get("input_schema"))
                .cloned()
        })
        .or_else(|| {
            nested_tool
                .and_then(|value| value.get("inputSchema"))
                .cloned()
        })
        .or_else(|| {
            nested_tool
                .and_then(|value| value.get("parameters"))
                .cloned()
        })
        .unwrap_or_else(|| json!({ "type": "object", "properties": {} }));

    if !is_callable_tool(tool, &parameters) {
        return None;
    }

    Some(ToolDeclaration {
        response_name: sanitize_tool_name(&name),
        description,
        parameters: normalize_schema(parameters),
        namespace,
        kind: ToolEntryKind::Function,
    })
}

fn parse_tool_arguments(arguments: &str) -> Value {
    serde_json::from_str(arguments).unwrap_or_else(|_| json!({}))
}

fn malformed_tool_arguments_metadata(arguments: &str) -> Option<Value> {
    match serde_json::from_str::<Value>(arguments) {
        Ok(_) => None,
        Err(error) => Some(json!({
            "codeseex_malformed_arguments": true,
            "codeseex_raw_arguments": truncate_chars(arguments, MAX_MALFORMED_ARGUMENTS_CHARS),
            "codeseex_argument_parse_error": error.to_string()
        })),
    }
}

fn truncate_chars(value: &str, max_chars: usize) -> String {
    if value.chars().count() <= max_chars {
        return value.to_owned();
    }
    let mut output = value.chars().take(max_chars).collect::<String>();
    output.push_str("...");
    output
}

fn serialized_json_chars(value: &Value) -> usize {
    serde_json::to_string(value)
        .map(|value| value.chars().count())
        .unwrap_or(usize::MAX)
}

fn tool_search_output_tool_declarations(
    input: &Value,
    valid_previous_call_ids: &BTreeSet<String>,
) -> Vec<Value> {
    let Some(items) = input.as_array() else {
        return Vec::new();
    };
    let mut valid_call_ids = valid_previous_call_ids.clone();
    for item in items {
        if item.get("type").and_then(Value::as_str) == Some("tool_search_call") {
            if let Some(call_id) = response_item_call_id(item) {
                valid_call_ids.insert(call_id.to_owned());
            }
        }
    }
    if valid_call_ids.is_empty() {
        return Vec::new();
    }
    items
        .iter()
        .filter(|item| item.get("type").and_then(Value::as_str) == Some("tool_search_output"))
        .filter(|item| {
            response_item_call_id(item)
                .map(|call_id| valid_call_ids.contains(call_id))
                .unwrap_or(false)
        })
        .filter_map(|item| item.get("tools").and_then(Value::as_array))
        .flat_map(|tools| tools.iter().cloned())
        .collect()
}

fn response_item_call_id(item: &Value) -> Option<&str> {
    item.get("call_id")
        .or_else(|| item.get("tool_call_id"))
        .or_else(|| item.get("id"))
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
}

fn namespace_from_tool(tool: &Value) -> Option<String> {
    for key in ["namespace", "server_namespace", "server"] {
        if let Some(value) = tool.get(key).and_then(Value::as_str) {
            let trimmed = value.trim();
            if !trimmed.is_empty() {
                return Some(trimmed.to_owned());
            }
        }
    }

    let tool_type = tool
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_ascii_lowercase();
    if tool_type == "namespace" || (tool_type == "mcp" && tool.get("tools").is_some()) {
        for key in ["name", "server_label"] {
            if let Some(value) = tool.get(key).and_then(Value::as_str) {
                let trimmed = value.trim();
                if !trimmed.is_empty() {
                    return Some(trimmed.to_owned());
                }
            }
        }
    }
    None
}

fn is_callable_tool(tool: &Value, parameters: &Value) -> bool {
    if tool.get("type").and_then(Value::as_str) == Some("function")
        || tool.get("function").is_some()
    {
        return true;
    }
    let tool_type = tool
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_ascii_lowercase();
    if tool_type == "mcp" {
        return has_callable_shape(tool, parameters);
    }
    if let Some(nested) = tool.get("tool").filter(|value| value.is_object()) {
        return has_callable_shape(nested, parameters);
    }
    tool.get("name").is_some() && has_callable_shape(tool, parameters)
}

fn has_callable_shape(tool: &Value, parameters: &Value) -> bool {
    tool.get("name").and_then(Value::as_str).is_some()
        || (tool.get("server_label").and_then(Value::as_str).is_some()
            && (tool.get("description").is_some() || parameters.is_object()))
}

fn normalize_schema(schema: Value) -> Value {
    if !schema.is_object() {
        return json!({ "type": "object", "properties": {} });
    }
    if schema.get("type").is_some() {
        return schema;
    }
    json!({
        "type": "object",
        "properties": schema.get("properties").cloned().unwrap_or_else(|| json!({})),
        "required": schema.get("required").cloned().unwrap_or_else(|| json!([])),
        "additionalProperties": schema
            .get("additionalProperties")
            .cloned()
            .unwrap_or(Value::Bool(true))
    })
}

fn first_string(candidates: &[Option<&Value>], fallback: &str) -> String {
    candidates
        .iter()
        .filter_map(|value| value.and_then(Value::as_str))
        .map(str::trim)
        .find(|value| !value.is_empty())
        .unwrap_or(fallback)
        .to_owned()
}

fn unique_tool_name(
    response_name: &str,
    namespace: Option<&str>,
    entries: &BTreeMap<String, ToolEntry>,
) -> String {
    let base = sanitize_tool_name(response_name);
    if !entries.contains_key(&base) {
        return base;
    }
    let prefix = namespace
        .map(|value| sanitize_tool_name(&format!("{value}_{base}")))
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| base.clone());
    let mut candidate = prefix.clone();
    let mut suffix = 2_u32;
    while entries.contains_key(&candidate) {
        candidate = suffixed_tool_name(&prefix, suffix);
        suffix += 1;
        if suffix > 10_000 {
            return suffixed_tool_name(&format!("tool_{}", Uuid::new_v4().simple()), 2);
        }
    }
    candidate
}

fn suffixed_tool_name(prefix: &str, suffix: u32) -> String {
    let suffix = format!("_{suffix}");
    let max_prefix_chars = 64_usize.saturating_sub(suffix.chars().count());
    let mut base = prefix.chars().take(max_prefix_chars).collect::<String>();
    if base.trim_matches('_').is_empty() {
        base = "tool".to_owned();
    }
    sanitize_tool_name(&format!("{base}{suffix}"))
}

fn sanitize_tool_name(name: &str) -> String {
    let sanitized = name
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '_' || ch == '-' {
                ch
            } else {
                '_'
            }
        })
        .collect::<String>();
    let truncated = sanitized.chars().take(64).collect::<String>();
    if truncated.trim_matches('_').is_empty() {
        "tool".to_owned()
    } else {
        truncated
    }
}

fn is_conflicting_visual_tool(name: &str) -> bool {
    matches!(
        name.trim().to_ascii_lowercase().as_str(),
        "imagegen"
            | "imagegenext"
            | "image_gen"
            | "image_generation"
            | "generate_image"
            | "image_generate"
            | "create_image"
            | "vision_analyze"
            | "vision_generate"
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn normalizes_mcp_nested_tools_with_namespace() {
        let context = ToolContext::from_request_tools(Some(&json!([
            {
                "type": "mcp",
                "name": "smoke_server",
                "tools": [
                    {
                        "name": "smoke_add",
                        "description": "Add two numbers",
                        "input_schema": {
                            "type": "object",
                            "properties": { "a": { "type": "integer" }, "b": { "type": "integer" } },
                            "required": ["a", "b"]
                        }
                    }
                ]
            }
        ])));

        assert_eq!(context.upstream_tools.len(), 1);
        assert_eq!(
            context.upstream_tools[0]
                .pointer("/function/name")
                .and_then(Value::as_str),
            Some("smoke_add")
        );
        let item = context.response_item_from_chat_call(&ChatToolCall {
            id: "call_1".to_owned(),
            name: "smoke_add".to_owned(),
            arguments: "{\"a\":1,\"b\":2}".to_owned(),
        });
        assert_eq!(
            item.get("namespace").and_then(Value::as_str),
            Some("smoke_server")
        );
        assert_eq!(item.get("name").and_then(Value::as_str), Some("smoke_add"));
    }

    #[test]
    fn mcp_server_without_nested_tools_is_not_callable_function() {
        let context = ToolContext::from_request_tools(Some(&json!([
            {
                "type": "mcp",
                "server_label": "node_repl"
            }
        ])));

        assert!(context.upstream_tools.is_empty());
        assert!(!context.has_external_tool("node_repl"));
    }

    #[test]
    fn flat_function_tool_search_passes_through() {
        let context = ToolContext::from_request_tools(Some(&json!([
            {
                "type": "function",
                "name": "tool_search_tool",
                "description": "Search deferred tool metadata",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "query": { "type": "string" },
                        "limit": { "type": "number" }
                    },
                    "required": ["query"]
                }
            }
        ])));

        assert_eq!(context.request_tool_items(), 1);
        assert_eq!(context.source_names(), vec!["tool_search_tool"]);
        assert_eq!(context.upstream_names(), vec!["tool_search_tool"]);
        assert_eq!(
            context.upstream_tools[0]
                .pointer("/function/name")
                .and_then(Value::as_str),
            Some("tool_search_tool")
        );
        assert!(context.has_external_tool("tool_search_tool"));
    }

    #[test]
    fn promoted_flat_tool_search_returns_native_search_call_item() {
        let mut context = ToolContext::from_request_tools(Some(&json!([
            {
                "type": "function",
                "name": "tool_search_tool",
                "description": "Search deferred tool metadata",
                "parameters": {
                    "type": "object",
                    "properties": { "query": { "type": "string" } },
                    "required": ["query"]
                }
            }
        ])));
        context.promote_codex_tool_search_tools();

        let item = context.response_item_from_chat_call(&ChatToolCall {
            id: "call_search".to_owned(),
            name: "tool_search_tool".to_owned(),
            arguments: r#"{"query":"spawn_agent"}"#.to_owned(),
        });

        assert_eq!(
            item.get("type").and_then(Value::as_str),
            Some("tool_search_call")
        );
        assert_eq!(
            item.get("execution").and_then(Value::as_str),
            Some("client")
        );
        assert!(item.get("metadata").is_none());
    }

    #[test]
    fn malformed_tool_search_arguments_are_preserved_in_metadata() {
        let mut context = ToolContext::from_request_tools(Some(&json!([
            {
                "type": "function",
                "name": "tool_search_tool",
                "description": "Search deferred tool metadata",
                "parameters": { "type": "object", "properties": {} }
            }
        ])));
        context.promote_codex_tool_search_tools();

        let item = context.response_item_from_chat_call(&ChatToolCall {
            id: "call_search".to_owned(),
            name: "tool_search_tool".to_owned(),
            arguments: r#"{"query":"spawn_agent""#.to_owned(),
        });

        assert_eq!(
            item.get("type").and_then(Value::as_str),
            Some("tool_search_call")
        );
        assert_eq!(item["arguments"], json!({}));
        assert_eq!(
            item.pointer("/metadata/codeseex_malformed_arguments")
                .and_then(Value::as_bool),
            Some(true)
        );
        assert_eq!(
            item.pointer("/metadata/codeseex_raw_arguments")
                .and_then(Value::as_str),
            Some(r#"{"query":"spawn_agent""#)
        );
        assert!(item
            .pointer("/metadata/codeseex_argument_parse_error")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .contains("EOF"));
    }

    #[test]
    fn promoted_tool_search_alias_returns_native_search_call_item() {
        let mut context = ToolContext::from_request_tools(Some(&json!([
            {
                "type": "function",
                "name": "tool_search",
                "description": "Search deferred tool metadata",
                "parameters": { "type": "object", "properties": {} }
            }
        ])));
        context.promote_codex_tool_search_tools();

        let item = context.response_item_from_chat_call(&ChatToolCall {
            id: "call_search".to_owned(),
            name: "tool_search".to_owned(),
            arguments: r#"{"query":"spawn_agent"}"#.to_owned(),
        });

        assert_eq!(
            item.get("type").and_then(Value::as_str),
            Some("tool_search_call")
        );
    }

    #[test]
    fn matching_tool_search_output_tools_become_callable() {
        let mut context = ToolContext::default();
        context.add_tool_search_output_tools(
            Some(&json!([
                {
                    "type": "tool_search_call",
                    "call_id": "call_search",
                    "arguments": { "query": "spawn_agent" }
                },
                {
                    "type": "tool_search_output",
                    "call_id": "call_search",
                    "tools": [
                        {
                            "type": "namespace",
                            "name": "multi_agent_v1",
                            "tools": [
                                {
                                    "name": "spawn_agent",
                                    "description": "Spawn a sub-agent",
                                    "input_schema": {
                                        "type": "object",
                                        "properties": { "message": { "type": "string" } },
                                        "required": ["message"]
                                    }
                                }
                            ]
                        }
                    ]
                }
            ])),
            &BTreeSet::new(),
        );

        assert_eq!(context.discovered_tool_items(), 1);
        assert!(context.has_external_tool("spawn_agent"));
        assert_eq!(context.upstream_names(), vec!["spawn_agent"]);
    }

    #[test]
    fn duplicate_response_names_keep_upstream_identity_metadata() {
        let context = ToolContext::from_request_tools(Some(&json!([
            {
                "type": "namespace",
                "name": "alpha",
                "tools": [{
                    "name": "run",
                    "description": "Run alpha",
                    "input_schema": { "type": "object", "properties": {} }
                }]
            },
            {
                "type": "namespace",
                "name": "beta",
                "tools": [{
                    "name": "run",
                    "description": "Run beta",
                    "input_schema": { "type": "object", "properties": {} }
                }]
            }
        ])));

        let upstream_names = context.upstream_names();
        assert!(
            upstream_names.contains(&"run".to_owned()),
            "{upstream_names:?}"
        );
        assert!(
            upstream_names.contains(&"beta_run".to_owned()),
            "{upstream_names:?}"
        );
        let item = context.response_item_from_chat_call(&ChatToolCall {
            id: "call_beta".to_owned(),
            name: "beta_run".to_owned(),
            arguments: "{}".to_owned(),
        });

        assert_eq!(item.get("name").and_then(Value::as_str), Some("run"));
        assert_eq!(item.get("namespace").and_then(Value::as_str), Some("beta"));
        assert_eq!(
            item.pointer("/metadata/codeseex_upstream_name")
                .and_then(Value::as_str),
            Some("beta_run")
        );
        assert_eq!(
            item.pointer("/metadata/codeseex_response_name")
                .and_then(Value::as_str),
            Some("run")
        );
    }

    #[test]
    fn unmatched_tool_search_output_tools_are_not_callable() {
        let mut context = ToolContext::default();
        context.add_tool_search_output_tools(
            Some(&json!([
                {
                    "type": "tool_search_output",
                    "call_id": "call_search",
                    "tools": [
                        {
                            "name": "spawn_agent",
                            "description": "Spawn a sub-agent",
                            "input_schema": { "type": "object", "properties": {} }
                        }
                    ]
                }
            ])),
            &BTreeSet::new(),
        );

        assert_eq!(context.discovered_tool_items(), 0);
        assert!(!context.has_external_tool("spawn_agent"));
    }

    #[test]
    fn explicit_request_tool_wins_over_discovered_tool() {
        let mut context = ToolContext::from_request_tools(Some(&json!([{
            "type": "function",
            "name": "spawn_agent",
            "description": "Explicit request tool",
            "parameters": { "type": "object", "properties": {} }
        }])));
        context.add_tool_search_output_tools(
            Some(&json!([
                { "type": "tool_search_call", "call_id": "call_search", "arguments": {} },
                {
                    "type": "tool_search_output",
                    "call_id": "call_search",
                    "tools": [
                        {
                            "name": "spawn_agent",
                            "description": "Discovered tool",
                            "input_schema": { "type": "object", "properties": {} }
                        }
                    ]
                }
            ])),
            &BTreeSet::new(),
        );

        assert_eq!(context.discovered_tool_items(), 0);
        assert_eq!(context.upstream_tools.len(), 1);
        assert_eq!(
            context.upstream_tools[0]
                .pointer("/function/description")
                .and_then(Value::as_str),
            Some("Explicit request tool")
        );
    }

    #[test]
    fn synthetic_tool_search_bridge_can_be_added_for_codex_native_tools() {
        let mut context = ToolContext::from_request_tools(Some(&json!([
            {
                "type": "function",
                "function": {
                    "name": "shell_command",
                    "description": "Run a shell command",
                    "parameters": { "type": "object", "properties": {} }
                }
            }
        ])));

        context.ensure_codex_tool_search_bridge();

        assert!(context.has_external_tool("shell_command"));
        assert!(context.has_external_tool("tool_search_tool"));
        assert_eq!(context.upstream_tools.len(), 2);
        assert!(context
            .upstream_names()
            .iter()
            .any(|name| name == "tool_search_tool"));
    }

    #[test]
    fn synthetic_tool_search_bridge_returns_native_search_call_item() {
        let mut context = ToolContext::from_request_tools(Some(&json!([
            {
                "type": "function",
                "function": {
                    "name": "shell_command",
                    "description": "Run a shell command",
                    "parameters": { "type": "object", "properties": {} }
                }
            }
        ])));
        context.ensure_codex_tool_search_bridge();

        let item = context.response_item_from_chat_call(&ChatToolCall {
            id: "call_search".to_owned(),
            name: "tool_search_tool".to_owned(),
            arguments: r#"{"query":"sub-agent","limit":5}"#.to_owned(),
        });

        assert_eq!(
            item.get("type").and_then(Value::as_str),
            Some("tool_search_call")
        );
        assert_eq!(
            item.get("execution").and_then(Value::as_str),
            Some("client")
        );
        assert_eq!(
            item.pointer("/arguments/query").and_then(Value::as_str),
            Some("sub-agent")
        );
        assert_eq!(
            item.pointer("/arguments/limit").and_then(Value::as_u64),
            Some(5)
        );
    }

    #[test]
    fn synthetic_tool_search_bridge_does_not_duplicate_native_declaration() {
        let mut context = ToolContext::from_request_tools(Some(&json!([
            {
                "type": "function",
                "name": "tool_search_tool",
                "description": "Search deferred tools",
                "parameters": { "type": "object", "properties": {} }
            }
        ])));

        context.ensure_codex_tool_search_bridge();

        assert_eq!(context.upstream_tools.len(), 1);
        assert_eq!(context.upstream_names(), vec!["tool_search_tool"]);
    }

    #[test]
    fn request_tool_declarations_are_count_and_payload_bounded() {
        let tools = (0..(MAX_EXTERNAL_TOOL_DECLARATIONS + 20))
            .map(|index| {
                json!({
                    "type": "function",
                    "name": format!("external_{index}"),
                    "description": "small",
                    "parameters": { "type": "object", "properties": {} }
                })
            })
            .collect::<Vec<_>>();

        let context = ToolContext::from_request_tools(Some(&Value::Array(tools)));
        let diagnostic = context.external_tool_budget_diagnostic();

        assert_eq!(context.upstream_tools.len(), MAX_EXTERNAL_TOOL_DECLARATIONS);
        assert_eq!(
            diagnostic["attempted_declarations"].as_u64(),
            Some((MAX_EXTERNAL_TOOL_DECLARATIONS + 20) as u64)
        );
        assert_eq!(
            diagnostic["accepted_declarations"].as_u64(),
            Some(MAX_EXTERNAL_TOOL_DECLARATIONS as u64)
        );
        assert_eq!(diagnostic["dropped"]["count_limit"].as_u64(), Some(20));
        assert!(
            diagnostic["payload_chars"].as_u64().unwrap()
                <= MAX_EXTERNAL_TOOLS_PAYLOAD_CHARS as u64
        );
    }

    #[test]
    fn oversized_external_schema_is_dropped() {
        let huge_description = "d".repeat(MAX_EXTERNAL_TOOL_DESCRIPTION_CHARS + 100);
        let huge_schema_text = "x".repeat(MAX_EXTERNAL_TOOL_SCHEMA_CHARS + 1);
        let context = ToolContext::from_request_tools(Some(&json!([
            {
                "type": "function",
                "name": "small_tool",
                "description": huge_description,
                "parameters": {
                    "type": "object",
                    "properties": {
                        "query": {
                            "type": "string"
                        }
                    }
                }
            },
            {
                "type": "function",
                "name": "huge_schema_tool",
                "description": "too large",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "blob": {
                            "type": "string",
                            "description": huge_schema_text
                        }
                    }
                }
            }
        ])));
        let diagnostic = context.external_tool_budget_diagnostic();

        assert_eq!(context.upstream_names(), vec!["small_tool"]);
        assert_eq!(diagnostic["truncated_descriptions"].as_u64(), Some(1));
        assert_eq!(diagnostic["dropped"]["schema_size"].as_u64(), Some(1));
        assert!(
            context.upstream_tools[0]
                .pointer("/function/description")
                .and_then(Value::as_str)
                .unwrap()
                .chars()
                .count()
                <= MAX_EXTERNAL_TOOL_DESCRIPTION_CHARS
        );
    }

    #[test]
    fn discovered_tool_search_output_declarations_are_budgeted() {
        let discovered_tools = (0..(MAX_EXTERNAL_TOOL_DECLARATIONS + 5))
            .map(|index| {
                json!({
                    "name": format!("discovered_{index}"),
                    "description": "Discovered tool",
                    "input_schema": { "type": "object", "properties": {} }
                })
            })
            .collect::<Vec<_>>();
        let mut context = ToolContext::default();
        context.add_tool_search_output_tools(
            Some(&json!([
                {
                    "type": "tool_search_call",
                    "call_id": "call_search",
                    "arguments": { "query": "many tools" }
                },
                {
                    "type": "tool_search_output",
                    "call_id": "call_search",
                    "tools": [
                        {
                            "type": "namespace",
                            "name": "many",
                            "tools": discovered_tools
                        }
                    ]
                }
            ])),
            &BTreeSet::new(),
        );
        let diagnostic = context.external_tool_budget_diagnostic();

        assert_eq!(context.upstream_tools.len(), MAX_EXTERNAL_TOOL_DECLARATIONS);
        assert_eq!(
            diagnostic["attempted_declarations"].as_u64(),
            Some((MAX_EXTERNAL_TOOL_DECLARATIONS + 5) as u64)
        );
        assert_eq!(diagnostic["dropped"]["count_limit"].as_u64(), Some(5));
        assert!(
            context.discovered_tool_items() >= MAX_EXTERNAL_TOOL_DECLARATIONS,
            "discovered count should still expose incoming discovery pressure"
        );
    }

    #[test]
    fn external_codex_tool_search_declaration_is_budgeted() {
        let mut tools = (0..MAX_EXTERNAL_TOOL_DECLARATIONS)
            .map(|index| {
                json!({
                    "type": "function",
                    "name": format!("external_{index}"),
                    "description": "small",
                    "parameters": { "type": "object", "properties": {} }
                })
            })
            .collect::<Vec<_>>();
        tools.push(json!({
            "type": "function",
            "name": "tool_search_tool",
            "description": "Search deferred tools",
            "parameters": { "type": "object", "properties": {} }
        }));

        let context = ToolContext::from_request_tools(Some(&Value::Array(tools)));
        let diagnostic = context.external_tool_budget_diagnostic();

        assert!(!context.has_external_tool("tool_search_tool"));
        assert_eq!(context.upstream_tools.len(), MAX_EXTERNAL_TOOL_DECLARATIONS);
        assert_eq!(diagnostic["exempt_declarations"].as_u64(), Some(0));
        assert_eq!(diagnostic["dropped"]["count_limit"].as_u64(), Some(1));
        assert_eq!(
            diagnostic["dropped"]["sample_names"][0].as_str(),
            Some("tool_search_tool")
        );
    }

    #[test]
    fn internal_codex_tool_search_bridge_survives_external_count_budget() {
        let tools = (0..MAX_EXTERNAL_TOOL_DECLARATIONS)
            .map(|index| {
                json!({
                    "type": "function",
                    "name": format!("external_{index}"),
                    "description": "small",
                    "parameters": { "type": "object", "properties": {} }
                })
            })
            .collect::<Vec<_>>();
        let mut context = ToolContext::from_request_tools(Some(&Value::Array(tools)));

        context.ensure_codex_tool_search_bridge();

        assert!(context.has_external_tool("tool_search_tool"));
        assert_eq!(
            context.upstream_tools.len(),
            MAX_EXTERNAL_TOOL_DECLARATIONS + 1
        );
        assert_eq!(
            context.external_tool_budget_diagnostic()["exempt_declarations"].as_u64(),
            Some(0)
        );
    }

    #[test]
    fn discovered_codex_tool_search_declaration_is_budgeted() {
        let discovered_tools = (0..MAX_EXTERNAL_TOOL_DECLARATIONS)
            .map(|index| {
                json!({
                    "name": format!("discovered_{index}"),
                    "description": "Discovered tool",
                    "input_schema": { "type": "object", "properties": {} }
                })
            })
            .chain(std::iter::once(json!({
                "name": "tool_search_tool",
                "description": "Discovered tool search should not bypass budgets",
                "input_schema": { "type": "object", "properties": {} }
            })))
            .collect::<Vec<_>>();
        let mut context = ToolContext::default();
        context.add_tool_search_output_tools(
            Some(&json!([
                {
                    "type": "tool_search_call",
                    "call_id": "call_search",
                    "arguments": { "query": "many tools" }
                },
                {
                    "type": "tool_search_output",
                    "call_id": "call_search",
                    "tools": [
                        {
                            "type": "namespace",
                            "name": "many",
                            "tools": discovered_tools
                        }
                    ]
                }
            ])),
            &BTreeSet::new(),
        );
        let diagnostic = context.external_tool_budget_diagnostic();

        assert!(!context.has_external_tool("tool_search_tool"));
        assert_eq!(context.upstream_tools.len(), MAX_EXTERNAL_TOOL_DECLARATIONS);
        assert_eq!(diagnostic["dropped"]["count_limit"].as_u64(), Some(1));
        assert_eq!(
            diagnostic["dropped"]["sample_names"][0].as_str(),
            Some("tool_search_tool")
        );
    }

    #[test]
    fn custom_tool_declaration_passes_through_as_callable_function() {
        let context = ToolContext::from_request_tools(Some(&json!([{
            "type": "custom",
            "name": "apply_patch",
            "description": "Apply a patch",
            "input_schema": {
                "type": "object",
                "properties": {
                    "patch": { "type": "string" }
                },
                "required": ["patch"],
                "additionalProperties": false
            }
        }])));

        assert_eq!(context.request_tool_items(), 1);
        assert_eq!(context.upstream_names(), vec!["apply_patch"]);
        assert_eq!(
            context.upstream_tools[0]
                .pointer("/function/name")
                .and_then(Value::as_str),
            Some("apply_patch")
        );
        assert_eq!(
            context.upstream_tools[0]
                .pointer("/function/parameters/required/0")
                .and_then(Value::as_str),
            Some("patch")
        );
    }

    #[test]
    fn nested_tools_without_namespace_still_pass_through() {
        let context = ToolContext::from_request_tools(Some(&json!([
            {
                "type": "namespace",
                "tools": [
                    {
                        "name": "spawn_agent",
                        "description": "Spawn a sub-agent",
                        "input_schema": {
                            "type": "object",
                            "properties": {
                                "message": { "type": "string" }
                            }
                        }
                    }
                ]
            }
        ])));

        assert_eq!(context.request_tool_items(), 1);
        assert_eq!(context.source_names(), vec!["spawn_agent"]);
        assert_eq!(context.upstream_names(), vec!["spawn_agent"]);
        assert!(context.has_external_tool("spawn_agent"));
    }

    #[test]
    fn filters_conflicting_native_visual_tools() {
        let context = ToolContext::from_request_tools(Some(&json!([
            {
                "type": "function",
                "name": "imagegen",
                "description": "Generate images",
                "parameters": { "type": "object", "properties": {} }
            },
            {
                "type": "function",
                "name": "image_gen",
                "description": "Native-compatible image generation",
                "parameters": { "type": "object", "properties": {} }
            },
            {
                "type": "function",
                "name": "vision_generate",
                "description": "Legacy image generation",
                "parameters": { "type": "object", "properties": {} }
            },
            {
                "type": "function",
                "name": "view_image",
                "description": "View a local image",
                "parameters": { "type": "object", "properties": {} }
            },
            {
                "type": "function",
                "name": "spawn_agent",
                "description": "Spawn a sub-agent",
                "parameters": { "type": "object", "properties": {} }
            }
        ])));

        assert_eq!(context.request_tool_items(), 5);
        assert_eq!(context.upstream_names(), vec!["spawn_agent", "view_image"]);
        assert!(!context.has_external_tool("imagegen"));
        assert!(!context.has_external_tool("image_gen"));
        assert!(!context.has_external_tool("vision_generate"));
        assert!(context.has_external_tool("view_image"));
        assert!(context.has_external_tool("spawn_agent"));
    }

    #[test]
    fn duplicate_long_tool_names_get_distinct_bounded_names() {
        let long = "a".repeat(64);
        let context = ToolContext::from_request_tools(Some(&json!([
            {
                "type": "function",
                "function": {
                    "name": long,
                    "description": "first",
                    "parameters": { "type": "object", "properties": {} }
                }
            },
            {
                "type": "function",
                "function": {
                    "name": long,
                    "description": "second",
                    "parameters": { "type": "object", "properties": {} }
                }
            }
        ])));
        let names = context
            .upstream_tools
            .iter()
            .filter_map(|tool| tool.pointer("/function/name").and_then(Value::as_str))
            .collect::<Vec<_>>();

        assert_eq!(names.len(), 2);
        assert_ne!(names[0], names[1]);
        assert!(names.iter().all(|name| name.chars().count() <= 64));
    }
}

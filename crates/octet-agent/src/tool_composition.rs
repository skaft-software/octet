//! Host-owned contracts for single-shot composition of the effective tool surface.
//!
//! The JavaScript engine belongs to an optional extension. This module owns only
//! bounded presentation metadata, request-scoped dispatch and private branch state.

use std::collections::BTreeMap;
use std::sync::Arc;

use octet_ai::ToolDef;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::tool::{CancellationToken, Tool, ToolError};

/// Maximum nested calls in one single-shot program.
pub const MAX_COMPOSITION_CALLS: usize = 256;
/// Host-owned wall ceiling; a program cannot raise it.
pub const COMPOSITION_TIMEOUT_MS: u64 = 30_000;
/// Maximum encoded bytes of one stored value.
pub const MAX_COMPOSITION_STORE_VALUE_BYTES: usize = 256 * 1024;
/// Maximum encoded bytes of the complete branch store.
pub const MAX_COMPOSITION_STORE_BYTES: usize = 1024 * 1024;

/// How an explicitly enabled composing tool presents its effective loadout.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolCompositionMode {
    /// Keep ordinary tools directly callable as well as callable from scripts.
    #[default]
    On,
    /// Advertise only composing tools; ordinary tools are nested-only.
    Only,
}

/// Presentation options declared by a negotiated composing tool, not authority.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ToolCompositionConfig {
    /// Direct-plus-script or script-only presentation.
    pub mode: ToolCompositionMode,
    /// Estimated token allowance for inline declarations (four bytes per token).
    pub inline_budget: usize,
}

/// An active model-tool request's host-issued nested dispatch capability.
///
/// Instances freeze host-policed tools and inherit the owning run's broker,
/// hooks, cancellation and workspace. No process-scoped service is sufficient.
#[async_trait::async_trait]
pub trait ToolCompositionService: Send + Sync {
    /// Returns the frozen callable catalog, branch store and host-owned limits.
    async fn context(&self) -> Result<Value, ToolError>;
    /// Runs one validated nested call, returning its programmatic projection.
    async fn call(
        &self,
        name: String,
        arguments: Value,
        cancellation: CancellationToken,
    ) -> Result<Value, ToolError>;
    /// Runs through the same broker, returning the complete canonical outcome.
    async fn call_outcome(&self, _name: String, _arguments: Value,
        _cancellation: CancellationToken) -> Result<Value, ToolError> {
        Err(ToolError::new("complete nested outcomes are unavailable"))
    }
    /// Commits successful script writes to private, branch-scoped session state.
    async fn store(&self, set: Map<String, Value>, delete: Vec<String>) -> Result<(), ToolError>;
}

/// Private durable evidence for composition. Never provider-visible context.
///
/// A started call without a finished receipt is indeterminate, not replayable.
/// Nested payloads are not copied into the model transcript or this journal.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum ToolCompositionRecord {
    /// Synced before any nested effect is admitted.
    CallStarted {
        /// Host-issued parent tool-call ID.
        parent: String,
        /// Host-issued nested tool-call ID.
        id: String,
        /// Exact frozen tool name.
        tool: String,
        /// Digest of the exact arguments, not their potentially private contents.
        arguments_hash: String,
    },
    /// Durable outcome of a nested call; it does not undo earlier effects.
    CallFinished {
        /// Host-issued parent tool-call ID.
        parent: String,
        /// Host-issued nested tool-call ID.
        id: String,
        /// Exact frozen tool name.
        tool: String,
        /// Whether execution completed without a tool error.
        ok: bool,
        /// Wall time in milliseconds.
        duration_ms: u64,
        /// Exact host-owned effect classification, if admission reached it.
        effect: Option<crate::effect::ToolEffect>,
        /// Whether the exact nested call passed effect admission.
        allowed: bool,
        /// Machine-readable policy denial without arguments or secrets.
        denial_code: Option<crate::effect::ToolPolicyDenialCode>,
        /// Exact private delivery payload for tools with provisional leases.
        /// Ordinary read/process output is deliberately not duplicated here.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        delivery_text: Option<String>,
    },
    /// Successful script writes. An older reader ignores this additive sidecar.
    Store {
        /// Composing tool namespace, derived by the host from the active call.
        tool: String,
        /// Complete values written by this script.
        set: Map<String, Value>,
        /// Keys deleted by this script.
        delete: Vec<String>,
    },
}

impl ToolCompositionRecord {
    pub(crate) fn valid(&self) -> bool {
        let short = |s: &str| !s.is_empty() && s.len() <= 512 && !s.chars().any(char::is_control);
        match self {
            Self::CallStarted {
                parent,
                id,
                tool,
                arguments_hash,
            } => {
                short(parent)
                    && short(id)
                    && short(tool)
                    && arguments_hash.len() == 64
                    && arguments_hash
                        .bytes()
                        .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
            }
            Self::CallFinished {
                parent,
                id,
                tool,
                delivery_text,
                ..
            } => {
                short(parent)
                    && short(id)
                    && short(tool)
                    && delivery_text
                        .as_ref()
                        .is_none_or(|text| text.len() <= 1024 * 1024)
            }
            Self::Store { tool, set, delete } => {
                short(tool) && validate_store_writes(set, delete).is_ok()
            }
        }
    }
}

pub(crate) fn validate_store_writes(
    set: &Map<String, Value>,
    delete: &[String],
) -> Result<(), ToolError> {
    if set.len().saturating_add(delete.len()) > 4096 {
        return Err(ToolError::new("composition store has too many keys"));
    }
    for key in set.keys().chain(delete.iter()) {
        if key.len() > 1024 {
            return Err(ToolError::new("composition store key exceeds 1024 bytes"));
        }
    }
    for value in set.values() {
        crate::tool::validate_tool_detail(
            "composition store value",
            value,
            MAX_COMPOSITION_STORE_VALUE_BYTES,
            false,
        )
        .map_err(|error| ToolError::new(error.to_string()))?;
    }
    crate::tool::validate_tool_detail(
        "composition store writes",
        &Value::Object(set.clone()),
        MAX_COMPOSITION_STORE_BYTES,
        false,
    )
    .map_err(|error| ToolError::new(error.to_string()))
}

pub(crate) fn callable_catalog(tools: &[Arc<dyn Tool>]) -> Vec<Value> {
    tools.iter().filter(|tool| tool.composition_config().is_none()).map(|tool| {
        let definition = tool.definition();
        let mut entry = serde_json::json!({
            "name":definition.name, "description":definition.description, "parameters":definition.parameters,
        });
        if let Some(schema) = tool.output_schema() {
            entry["output_schema"] = schema;
        }
        entry
    }).collect()
}

/// Creates a bounded model-visible presentation without changing execution policy.
/// The ordinary direct registry remains the exact advertised set; nested-only
/// tools live in the separately frozen composition surface.
pub(crate) fn direct_surface(tools: &[Arc<dyn Tool>]) -> Vec<Arc<dyn Tool>> {
    let only = tools.iter().any(|tool| {
        tool.composition_config()
            .is_some_and(|config| config.mode == ToolCompositionMode::Only)
    });
    tools
        .iter()
        .filter(|tool| !only || tool.composition_config().is_some())
        .cloned()
        .collect()
}

pub(crate) fn advertised_surface(tools: &[Arc<dyn Tool>]) -> Vec<ToolDef> {
    let only = tools.iter().any(|tool| {
        tool.composition_config()
            .is_some_and(|config| config.mode == ToolCompositionMode::Only)
    });
    let ordinary = tools
        .iter()
        .filter(|tool| tool.composition_config().is_none())
        .collect::<Vec<_>>();
    let bindings = tool_bindings(&ordinary);
    tools.iter().filter(|tool| !only || tool.composition_config().is_some()).map(|tool| {
        let mut definition = tool.definition();
        if let Some(config) = tool.composition_config() {
            let mut remaining = config.inline_budget.saturating_mul(4);
            let mut declarations = Vec::new();
            // Cheapest declarations first; stable name order breaks ties.
            let mut candidates = ordinary.iter().filter(|tool| !tool.deferred_composition()).filter_map(|tool| {
                let schema = tool.definition();
                let property = callable_property(&schema.name, &bindings)?;
                let section = render_property_declaration(&schema, &property, tool.output_schema().as_ref());
                Some((schema.name, section))
            }).collect::<Vec<_>>();
            candidates.sort_by(|a, b| a.1.len().cmp(&b.1.len()).then_with(|| a.0.cmp(&b.0)));
            for (_, section) in candidates {
                if section.len() <= remaining {
                    remaining -= section.len();
                    declarations.push(section);
                }
            }
            let visible = declarations.len();
            definition.description.push_str(&format!("\n\nCallable tool declarations: {visible} of {} (partial catalogs are intentional). Find unlisted tools with searchTools(), describeTool(), describeNamespace(), or ALL_TOOLS.\n{}", ordinary.len(), declarations.join("\n")));
        } else if !ordinary.is_empty() && tools.iter().any(|tool| tool.composition_config().is_some()) {
            if let Some(property) = callable_property(&definition.name, &bindings) {
                definition.description.push_str(&format!("\nAlso callable in composition scripts as await {property}(args)."));
            } else {
                definition.description.push_str("\nIts JavaScript property is shadowed by an earlier tool's Pi alias; use this direct tool instead.");
            }
        }
        definition
    }).collect()
}

pub(crate) fn identifier(name: &str) -> String {
    let mut result = String::new();
    for ch in name.chars() {
        let valid = ch.is_ascii_alphabetic()
            || ch == '_'
            || ch == '$'
            || (!result.is_empty() && ch.is_ascii_digit());
        result.push(if valid { ch } else { '_' });
    }
    if result.is_empty() {
        result.push('_');
    }
    result
}

fn tool_bindings(tools: &[&Arc<dyn Tool>]) -> BTreeMap<String, String> {
    let mut bindings = BTreeMap::new();
    for tool in tools {
        let name = tool.definition().name;
        bindings
            .entry(identifier(&name))
            .or_insert_with(|| name.clone());
        bindings.entry(name.clone()).or_insert(name);
    }
    bindings
}

fn callable_property(name: &str, bindings: &BTreeMap<String, String>) -> Option<String> {
    let alias = identifier(name);
    if bindings.get(&alias).is_some_and(|owner| owner == name) {
        Some(format!("tools.{alias}"))
    } else if bindings.get(name).is_some_and(|owner| owner == name) {
        Some(format!("tools[{}]", Value::String(name.into())))
    } else {
        None
    }
}

fn render_property_declaration(tool: &ToolDef, property: &str, output: Option<&Value>) -> String {
    let description = tool.description.replace("*/", "* /");
    format!(
        "/** {} */\n{property}(args: {}): Promise<{}>;",
        description,
        schema_type(&tool.parameters, 0),
        output.map_or_else(|| "string".to_owned(), |schema| schema_type(schema, 0))
    )
}

fn schema_type(schema: &Value, depth: usize) -> String {
    if depth > 16 {
        return "unknown".into();
    }
    if let Some(values) = schema.get("enum").and_then(Value::as_array) {
        return values
            .iter()
            .map(Value::to_string)
            .collect::<Vec<_>>()
            .join(" | ");
    }
    if let Some(value) = schema.get("const") {
        return value.to_string();
    }
    if let Some(variants) = schema
        .get("anyOf")
        .or_else(|| schema.get("oneOf"))
        .and_then(Value::as_array)
    {
        return variants
            .iter()
            .map(|schema| schema_type(schema, depth + 1))
            .collect::<Vec<_>>()
            .join(" | ");
    }
    match schema.get("type").and_then(Value::as_str) {
        Some("string") => "string".into(),
        Some("number" | "integer") => "number".into(),
        Some("boolean") => "boolean".into(),
        Some("null") => "null".into(),
        Some("array") => format!(
            "Array<{}>",
            schema
                .get("items")
                .map_or_else(|| "unknown".into(), |item| schema_type(item, depth + 1))
        ),
        Some("object") => {
            let required = schema.get("required").and_then(Value::as_array);
            let props = schema
                .get("properties")
                .and_then(Value::as_object)
                .map(|properties| {
                    properties
                        .iter()
                        .map(|(name, schema)| {
                            let optional = if required.is_some_and(|values| {
                                values.iter().any(|value| value.as_str() == Some(name))
                            }) {
                                ""
                            } else {
                                "?"
                            };
                            format!(
                                "{}{}: {}",
                                Value::String(name.clone()),
                                optional,
                                schema_type(schema, depth + 1)
                            )
                        })
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();
            if props.is_empty() {
                "Record<string, unknown>".into()
            } else {
                format!("{{ {} }}", props.join("; "))
            }
        }
        _ => "unknown".into(),
    }
}

pub(crate) fn restore_store(session: &crate::session::Session, tool: &str) -> Map<String, Value> {
    let mut chain = Vec::new();
    let mut cursor = session.head();
    while let Some(id) = cursor {
        let Some(entry) = session.entry(&id) else {
            break;
        };
        cursor = entry.parent.clone();
        chain.push(entry);
    }
    let mut store = Map::new();
    for entry in chain.into_iter().rev() {
        if let Some(ToolCompositionRecord::Store {
            tool: namespace,
            set,
            delete,
        }) = entry
            .metadata
            .as_ref()
            .and_then(|metadata| metadata.tool_composition.as_ref())
        {
            if namespace == tool {
                for key in delete {
                    store.remove(key);
                }
                store.extend(set.clone());
            }
        }
    }
    store
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn schema_declarations_and_identifiers_are_data() {
        assert_eq!(
            identifier("mcp__dev-radius__search"),
            "mcp__dev_radius__search"
        );
        assert_eq!(identifier("8-tool"), "__tool");
        assert_eq!(
            schema_type(
                &serde_json::json!({"type":"object","properties":{"path":{"type":"string"},"limit":{"type":"integer"}},"required":["path"]}),
                0
            ),
            "{ \"limit\"?: number; \"path\": string }"
        );
    }

    #[test]
    fn oversized_or_deep_store_is_refused_without_truncation() {
        let mut values = Map::new();
        values.insert(
            "large".into(),
            Value::String("x".repeat(MAX_COMPOSITION_STORE_VALUE_BYTES)),
        );
        assert!(validate_store_writes(&values, &[]).is_err());
        values.insert("large".into(), Value::String("a\nb".into()));
        assert!(validate_store_writes(&values, &[]).is_ok());
    }
}

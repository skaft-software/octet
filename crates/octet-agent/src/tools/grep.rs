//! Pi-shaped grep arguments over the bounded native search implementation.
use octet_ai::ToolDef;
use serde_json::{json, Value};
use crate::effect::ToolEffect;
use crate::tool::{Tool, ToolContext, ToolError, ToolOutput};
use super::SearchTool;

/// Built-in `grep` tool.
pub struct GrepTool;
fn normalize(mut args: Value) -> Result<Value,ToolError> {
    let object = args.as_object_mut().ok_or_else(|| ToolError::new("invalid arguments: expected object"))?;
    if object.keys().any(|key| !matches!(key.as_str(), "pattern"|"path"|"glob"|"ignoreCase"|"literal"|"context"|"limit"|"hidden")) { return Err(ToolError::new("invalid arguments: unknown grep property")); }
    let pattern = object.remove("pattern").ok_or_else(|| ToolError::new("invalid arguments: pattern is required"))?;
    let literal = match object.remove("literal") { None => false, Some(Value::Bool(value)) => value, _ => return Err(ToolError::new("invalid arguments: literal must be boolean")) };
    object.insert("query".into(),pattern);
    object.insert("mode".into(),json!(if literal {"literal"} else {"regex"}));
    object.entry("limit").or_insert(json!(100));
    Ok(args)
}
#[async_trait::async_trait]
impl Tool for GrepTool {
    fn definition(&self) -> ToolDef {
        let mut def = SearchTool.definition();
        def.name = "grep".into();
        def.description = "Search file contents using regex (or literal=true), including hidden files and respecting ignore rules. Supports ignoreCase, context lines and a per-call match limit (default 100).".into();
        let props = def.parameters["properties"].as_object_mut().expect("search schema properties");
        props.remove("query"); props.remove("mode"); props.remove("max_results");
        props.insert("pattern".into(),json!({"type":"string"}));
        props.insert("literal".into(),json!({"type":"boolean"}));
        props.insert("limit".into(),json!({"type":"integer","minimum":1,"description":"Maximum matches (default 100); context lines do not consume this limit."}));
        def.parameters["required"] = json!(["pattern"]);
        def
    }
    fn prompt_snippet(&self) -> Option<&str> { Some("Search file contents for patterns (respects .gitignore)") }
    fn effect(&self,args:&Value,ctx:&ToolContext<'_>)->Result<ToolEffect,ToolError>{ SearchTool.effect(&normalize(args.clone())?,ctx) }
    async fn execute(&self,args:Value,ctx:&ToolContext<'_>)->Result<ToolOutput,ToolError>{ SearchTool.execute(normalize(args)?,ctx).await }
}

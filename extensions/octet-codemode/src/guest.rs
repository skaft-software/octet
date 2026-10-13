//! Fixed, embedded guest program — not a general JavaScript bundler.
//! The pinned Pi prelude and Octet discovery implementation remain unchanged.
//!
//! The invariant program (modules plus the `(contextJson, sourceJson) => driver`
//! factory) is built once per process and compiled once per runner lifetime by
//! the Wasmi lane. Only the two JSON arguments change per script, so a fresh
//! isolated realm costs a call instead of a full parse.
use anyhow::Result;
use serde_json::Value;
use std::sync::OnceLock;

const MODULES: &[(&str, &str)] = &[
    (
        include_str!("../vendor/pi-codemode/dist/identifier.js"),
        "toCodemodeIdentifier",
    ),
    (
        include_str!("../vendor/pi-codemode/dist/declarations.js"),
        "renderToolSample",
    ),
    (
        include_str!("js/common.js"),
        "isObject, has, exactKeys, MAX_CALLS, MAX_HOST_FILE_BYTES, validateJson",
    ),
    (
        include_str!("js/discovery.js"),
        "discovery, validateContext",
    ),
    (
        include_str!("../vendor/pi-codemode/dist/runtime/prelude-source.js"),
        "PRELUDE_SOURCE",
    ),
];

fn module(output: &mut String, source: &str, exports: &str) {
    output.push_str("const {");
    output.push_str(exports);
    output.push_str("} = (() => {\n");
    for line in source.lines() {
        if line.starts_with("import ") || line.starts_with("export {") {
            continue;
        }
        let line = if ["export const ", "export class ", "export function "]
            .iter()
            .any(|prefix| line.starts_with(prefix))
        {
            &line[7..]
        } else {
            line
        };
        output.push_str(line);
        output.push('\n');
    }
    output.push_str("return {");
    output.push_str(exports);
    output.push_str("};\n})();\n");
}

/// The invariant guest program, built once per process.
pub fn prelude() -> &'static str {
    static PRELUDE: OnceLock<String> = OnceLock::new();
    PRELUDE.get_or_init(|| {
        let template = include_str!("guest.js");
        let mut output = String::with_capacity(
            template.len()
                + MODULES
                    .iter()
                    .map(|(source, _)| source.len())
                    .sum::<usize>(),
        );
        let mut rest = template;
        while let Some((before, after)) = rest.split_once("@@") {
            output.push_str(before);
            let (marker, after) = after.split_once("@@").expect("fixed template delimiter");
            match marker {
                "MODULES" => {
                    for (source, exports) in MODULES {
                        module(&mut output, source, exports);
                    }
                }
                _ => unreachable!("fixed template marker"),
            }
            rest = after;
        }
        output.push_str(rest);
        output
    })
}

/// Per-script guest arguments: the context as JSON text (the guest parses it)
/// and the raw source text (the guest evaluates it as an async function body,
/// exactly as Pi's codemode contract defines). Neither is spliced into the
/// invariant program, so no value can become template or module source.
pub fn script(context: &Value, code: &str) -> Result<(String, String)> {
    Ok((serde_json::to_string(context)?, code.to_owned()))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn data_cannot_become_a_template_or_module() {
        let data = "@@MODULES@@ @@CONTEXT@@ @@SOURCE@@\"\nexport const bad = 1;";
        let context = serde_json::json!({"store":{"marker":data}});
        let prelude = prelude();
        let (context_json, source) = script(&context, data).unwrap();
        assert!(!prelude.contains("@@"));
        assert_eq!(context_json, context.to_string());
        assert_eq!(source, data);
        assert!(!prelude
            .lines()
            .any(|line| line.starts_with("export ") || line.starts_with("import ")));
    }
}

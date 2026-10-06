use anyhow::{ensure, Context, Result};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::{HashSet, VecDeque};
use std::fs::{self, OpenOptions};
use std::io::{Read, Write};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use tempfile::{NamedTempFile, TempDir};

pub(super) const MIB: usize = 1024 * 1024;
pub(super) const FRAME_BYTES: usize = MIB;
pub(super) use crate::IPC_BYTES;
pub(super) const CAPTURE_BYTES: usize = 16 * MIB;
pub(super) const OUTPUT_BYTES: usize = 50 * 1024;
pub(super) const MAX_HOST_FILE_BYTES: usize = 8 * MIB;
pub(super) const MAX_CALLS: u64 = 256;
pub(super) const LOCAL_TIMEOUT_MS: u64 = 25000;
pub(super) const VM_HEAP_BYTES: usize = 256 * MIB;
pub(super) const SAFE_INTEGER: u64 = (1 << 53) - 1;
const REQUIRED: &[&str] = &[
    "request_cancellation",
    "content_parts",
    "tool_composition_v1",
];
const SUPPORTED: &[&str] = &[
    "request_cancellation",
    "content_parts",
    "tool_composition_v1",
    "request_progress",
    "artifacts",
];

// Exact upstream source.js String.raw literal, including the surrounding LF.
const GRAMMAR: &str = r#"
start: options_source | plain_source
options_source: OPTIONS_LINE NEWLINE SOURCE
plain_source: SOURCE

OPTIONS_LINE: /[ \t]*\/\/ @options:[^\r\n]*/
NEWLINE: /\r?\n/
SOURCE: /[\s\S]+/
"#;
pub(super) const DESCRIPTION: &str = r#"Run JavaScript (not TypeScript) as an async function body in Pi's QuickJS/WASM sandbox. No Node, Python, filesystem, network or process runtime is involved. Use top-level await and return; tools.<name>({arguments}) makes real host-brokered calls. Use Promise.allSettled to batch, chain calls, and filter large results before returning. No Node globals, filesystem, network, subprocesses, timers, imports, or models namespace.
Globals: tools, ALL_TOOLS, text(value), image(dataUrlOrImageBlock), exit(), console.log/info/warn/error/debug, store(key,value), load(key), searchTools(query,{limit?,namespace?}), describeTool(name), describeNamespace(name). Tool failures reject with their error text. Tools without output_schema return text, otherwise JSON. Pending/unawaited calls are cancelled, not undone. Store writes persist only after successful execution on the current session branch.
Optional first line: // @options: {"max_output_tokens":10000,"timeout_ms":25000}
Defaults: 10000 estimated output tokens; 25000 ms total local deadline (host maximum 30000 ms, lower host limits win); 256 calls, four concurrent nested calls, 256 MiB VM heap. Each script gets a fresh isolated VM in a warm runner: the Wasm module and process stay prepared between scripts, no global, heap object or pending promise survives. Output retains its head/tail within 50 KiB; full truncated UTF-8 text is saved in private scratch. Discovery uses BM25 (default 8); aliases replace non-identifier characters with _, first collision wins. image() accepts base64 PNG/JPEG/GIF/WebP and requires negotiated host artifacts. Models helpers are unavailable."#;

#[derive(Debug)]
pub(super) struct RpcError(pub i64, pub String);
impl std::fmt::Display for RpcError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.1.fmt(f)
    }
}
impl std::error::Error for RpcError {}

pub(super) fn params(condition: bool, message: &str) -> Result<()> {
    if !condition {
        return Err(RpcError(-32602, format!("Invalid params: {message}")).into());
    }
    Ok(())
}
pub(super) fn exact(value: &Value, allowed: &[&str]) -> bool {
    value
        .as_object()
        .is_some_and(|v| v.keys().all(|k| allowed.contains(&k.as_str())))
}
pub(super) fn integer(value: &Value, low: u64, high: u64) -> bool {
    value.as_u64().is_some_and(|n| (low..=high).contains(&n))
}
pub(super) fn valid_id(value: &Value) -> bool {
    integer(value, 0, SAFE_INTEGER)
        || value
            .as_str()
            .is_some_and(|s| !s.is_empty() && s.len() <= 256)
}
pub(super) fn validate_json(value: &Value, depth: usize) -> Result<()> {
    ensure!(depth <= 32, "JSON nesting exceeds 32 levels");
    match value {
        Value::Array(values) => {
            for v in values {
                validate_json(v, depth + 1)?;
            }
        }
        Value::Object(values) => {
            for v in values.values() {
                // Object keys occupy one level too, including in otherwise empty values.
                ensure!(depth < 32, "JSON nesting exceeds 32 levels");
                validate_json(v, depth + 1)?;
            }
        }
        _ => {}
    }
    Ok(())
}
pub(super) fn loads(bytes: &[u8]) -> Result<Value> {
    let value = serde_json::from_slice(bytes)?;
    validate_json(&value, 0)?;
    Ok(value)
}
pub(super) fn head(text: &str, bytes: usize) -> &str {
    let mut end = bytes.min(text.len());
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}
fn tail(text: &str, bytes: usize) -> &str {
    let mut start = text.len().saturating_sub(bytes);
    while !text.is_char_boundary(start) {
        start += 1;
    }
    &text[start..]
}

pub(super) struct Source {
    pub code: String,
    pub tokens: u64,
    pub timeout_ms: u64,
}
pub(super) fn source(arguments: &Value) -> Result<Source> {
    params(
        exact(arguments, &["code"])
            && arguments["code"]
                .as_str()
                .is_some_and(|s| s.chars().count() <= 65536),
        "codemode accepts only {code: string}, at most 65536 Unicode characters",
    )?;
    let mut code = arguments["code"].as_str().unwrap().to_owned();
    params(
        !code.trim().is_empty(),
        "Expected non-empty JavaScript source text",
    )?;
    let mut tokens = 10000;
    let mut timeout_ms = LOCAL_TIMEOUT_MS;
    let (first, rest) = code.split_once('\n').unwrap_or((&code, ""));
    if let Some(options) = first
        .trim_end_matches('\r')
        .trim_start()
        .strip_prefix("// @options:")
    {
        params(
            !rest.trim().is_empty(),
            "The @options line must be followed by JavaScript source",
        )?;
        let options = loads(options.trim().as_bytes())
            .map_err(|_| RpcError(-32602, "Invalid params: @options must be valid JSON".into()))?;
        params(
            exact(&options, &["max_output_tokens", "timeout_ms"]),
            "@options only supports max_output_tokens and timeout_ms",
        )?;
        if let Some(value) = options.get("max_output_tokens") {
            params(
                integer(value, 0, SAFE_INTEGER),
                "max_output_tokens must be a non-negative safe integer",
            )?;
            tokens = value.as_u64().unwrap();
        }
        if let Some(value) = options.get("timeout_ms") {
            params(
                integer(value, 1, 2147483647),
                "timeout_ms must be a positive integer up to 2147483647",
            )?;
            timeout_ms = value.as_u64().unwrap().min(LOCAL_TIMEOUT_MS);
        }
        code = format!("\n{rest}");
    }
    Ok(Source {
        code,
        tokens,
        timeout_ms,
    })
}

fn schema(value: &Value, depth: usize) -> Result<()> {
    if value.is_boolean() {
        return Ok(());
    }
    let fields = value
        .as_object()
        .context("Invalid composition tool JSON schema")?;
    ensure!(depth <= 32, "Invalid composition tool JSON schema");
    if let Some(types) = fields.get("type") {
        let types: Vec<&Value> = match types.as_array() {
            Some(a) => a.iter().collect(),
            None => vec![types],
        };
        let mut seen = HashSet::new();
        ensure!(
            !types.is_empty()
                && types.iter().all(|t| t.as_str().is_some_and(|t| [
                    "object", "array", "string", "number", "integer", "boolean", "null"
                ]
                .contains(&t)
                    && seen.insert(t))),
            "Invalid composition schema type"
        );
    }
    for key in [
        "properties",
        "patternProperties",
        "$defs",
        "definitions",
        "dependentSchemas",
    ] {
        if let Some(children) = fields.get(key) {
            for child in children
                .as_object()
                .with_context(|| format!("Invalid composition schema {key}"))?
                .values()
            {
                schema(child, depth + 1)?;
            }
        }
    }
    for key in [
        "additionalProperties",
        "unevaluatedProperties",
        "not",
        "if",
        "then",
        "else",
        "contains",
        "propertyNames",
    ] {
        if let Some(child) = fields.get(key) {
            schema(child, depth + 1)?;
        }
    }
    if let Some(items) = fields.get("items") {
        if let Some(children) = items.as_array() {
            for child in children {
                schema(child, depth + 1)?;
            }
        } else {
            schema(items, depth + 1)?;
        }
    }
    for key in ["allOf", "anyOf", "oneOf", "prefixItems"] {
        if let Some(children) = fields.get(key) {
            let children = children
                .as_array()
                .with_context(|| format!("Invalid composition schema {key}"))?;
            ensure!(!children.is_empty(), "Invalid composition schema {key}");
            for child in children {
                schema(child, depth + 1)?;
            }
        }
    }
    if let Some(required) = fields.get("required") {
        let mut seen = HashSet::new();
        ensure!(
            required
                .as_array()
                .is_some_and(|a| a.iter().all(|k| k.as_str().is_some_and(|k| seen.insert(k)))),
            "Invalid composition schema required"
        );
    }
    for key in [
        "minLength",
        "maxLength",
        "minItems",
        "maxItems",
        "minProperties",
        "maxProperties",
    ] {
        ensure!(
            fields.get(key).is_none_or(|v| integer(v, 0, SAFE_INTEGER)),
            "Invalid composition schema {key}"
        );
    }
    for key in [
        "minimum",
        "maximum",
        "exclusiveMinimum",
        "exclusiveMaximum",
        "multipleOf",
    ] {
        ensure!(
            fields.get(key).is_none_or(Value::is_number),
            "Invalid composition schema {key}"
        );
    }
    ensure!(
        fields
            .get("enum")
            .is_none_or(|v| v.as_array().is_some_and(|a| !a.is_empty())),
        "Invalid composition schema enum"
    );
    for key in ["description", "title", "$ref", "pattern", "format"] {
        ensure!(
            fields.get(key).is_none_or(Value::is_string),
            "Invalid composition schema {key}"
        );
    }
    Ok(())
}
pub(super) fn validate_context(context: &Value) -> Result<()> {
    ensure!(
        exact(context, &["tools", "store", "limits"])
            && context["tools"].as_array().is_some_and(|a| a.len() <= 4096)
            && context["store"].is_object()
            && exact(&context["limits"], &["timeout_ms", "max_calls"])
            && integer(&context["limits"]["timeout_ms"], 1, 30000)
            && integer(&context["limits"]["max_calls"], 1, MAX_CALLS),
        "Invalid composition/context response or limits"
    );
    validate_json(context, 0)?;
    ensure!(
        serde_json::to_vec(context)?.len() <= MAX_HOST_FILE_BYTES,
        "Composition context exceeds 8 MiB"
    );
    let mut names = HashSet::new();
    for tool in context["tools"].as_array().unwrap() {
        let name = tool["name"].as_str().unwrap_or("");
        ensure!(
            exact(
                tool,
                &["name", "description", "parameters", "output_schema"]
            ) && !name.is_empty()
                && name.len() <= 128
                && name
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"_.:$-".contains(&b))
                && name != "codemode"
                && names.insert(name)
                && tool["description"].is_string()
                && tool["parameters"].is_object(),
            "Invalid/duplicate/recursive composition tool definition"
        );
        schema(&tool["parameters"], 0)?;
        ensure!(
            tool["parameters"].get("type").is_none_or(|t| t == "object"),
            "Composition tool parameters must be an object schema"
        );
        if let Some(output) = tool.get("output_schema") {
            schema(output, 0)?;
        }
    }
    let mut total = 0;
    for value in context["store"].as_object().unwrap().values() {
        let size = serde_json::to_vec(value)?.len();
        total += size;
        ensure!(
            size <= 256 * 1024 && total <= MIB,
            "Composition store exceeds value/total bounds"
        );
    }
    Ok(())
}

pub(super) struct Negotiated {
    pub result: Value,
    pub features: HashSet<String>,
    pub mode: String,
    pub inline_budget: u64,
    pub concurrent: usize,
}
pub(super) fn negotiate(init: &Value) -> Result<Negotiated> {
    params(
        init["api_version"] == "0.4"
            && init["octet_version"] == "0.9.0"
            && init["extension"]["name"] == "octet-codemode"
            && init["extension"]["version"] == "0.9.0",
        "requires API 0.4, octet 0.9.0 and octet-codemode 0.9.0",
    )?;
    let offer = &init["protocol"];
    params(
        exact(
            offer,
            &[
                "version",
                "required_features",
                "optional_features",
                "limits",
                // Offered as initialization data, never selected: this
                // extension declares no bulk-object or resource capability.
                "bulk_objects_v1",
                "session_snapshot_transport_v1",
            ],
        ) && offer["version"] == "0.4"
            && offer["required_features"].is_array()
            && offer["optional_features"].is_array(),
        "feature-negotiated API 0.4 offer required",
    )?;
    let mut offered = Vec::new();
    let mut seen = HashSet::new();
    for f in offer["required_features"]
        .as_array()
        .unwrap()
        .iter()
        .chain(offer["optional_features"].as_array().unwrap())
    {
        params(
            f.as_str().is_some_and(|s| seen.insert(s)),
            "duplicate or invalid offered features",
        )?;
        offered.push(f.as_str().unwrap());
    }
    let required = offer["required_features"].as_array().unwrap();
    params(
        required
            .iter()
            .all(|v| SUPPORTED.contains(&v.as_str().unwrap()))
            && REQUIRED.iter().all(|f| seen.contains(f))
            && ["request_cancellation", "content_parts"]
                .iter()
                .all(|f| required.iter().any(|v| v == f)),
        "required protocol features unavailable",
    )?;
    params(
        exact(
            &offer["limits"],
            // Transport-only values; this extension selects no resource service.
            &[
                "max_concurrent_requests",
                "max_message_bytes",
                "resource_refs_v1",
            ],
        ) && integer(&offer["limits"]["max_concurrent_requests"], 4, 64),
        "host concurrency offer must be 4..64",
    )?;
    params(init["flag_values"].is_array(), "flag_values must be a list")?;
    let mut mode = json!("on");
    let mut inline_budget = json!(3000);
    let mut seen = HashSet::new();
    for flag in init["flag_values"].as_array().unwrap() {
        let name = flag["name"].as_str().unwrap_or("");
        params(
            exact(flag, &["name", "value"])
                && ["codemode-mode", "codemode-inline-budget"].contains(&name)
                && seen.insert(name)
                && flag.get("value").is_some(),
            "unknown/duplicate/malformed flag",
        )?;
        if name == "codemode-mode" {
            mode = flag["value"].clone();
        } else {
            inline_budget = flag["value"].clone();
        }
    }
    params(
        mode == "on" || mode == "only",
        "codemode-mode must be on or only",
    )?;
    params(
        integer(&inline_budget, 0, 16000),
        "codemode-inline-budget must be 0..16000",
    )?;
    let features: Vec<&str> = offered
        .into_iter()
        .filter(|f| SUPPORTED.contains(f))
        .collect();
    let concurrent = 8.min(offer["limits"]["max_concurrent_requests"].as_u64().unwrap()) as usize;
    let result = json!({"api_version":"0.4", "protocol":{"version":"0.4", "features":features, "limits":{"max_concurrent_requests":concurrent}},
        "tools":[{"name":"codemode", "description":DESCRIPTION, "parameters":{"type":"object", "properties":{"code":{"type":"string", "maxLength":65536}}, "required":["code"], "additionalProperties":false},
        "composition":{"mode":mode, "inline_budget":inline_budget}, "constrained_sampling":{"type":"grammar", "variants":{"openai_lark":GRAMMAR}}}],
        "commands":[{"name":"codemode", "description":"Show codemode status, limits and JavaScript help", "usage":"/codemode [status|help]"}]});
    Ok(Negotiated {
        result,
        features: features.into_iter().map(str::to_owned).collect(),
        mode: mode.as_str().unwrap().into(),
        inline_budget: inline_budget.as_u64().unwrap(),
        concurrent,
    })
}

pub(super) fn digest(data: &[u8]) -> String {
    format!("{:x}", Sha256::digest(data))
}

// A successfully validated flat reference is always unlinked, including on a
// size/digest/UTF-8/JSON failure. Invalid paths are never opened or removed.
pub(super) fn read_host_file(reference: &Value, scratch: &Path) -> Result<Value> {
    let name = reference["path"].as_str().unwrap_or("");
    let sha = reference["sha256"].as_str().unwrap_or("");
    ensure!(
        exact(reference, &["path", "bytes", "sha256"])
            && !name.is_empty()
            && name.len() <= 256
            && name
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"_.-".contains(&b))
            && ![".", ".."].contains(&name)
            && integer(&reference["bytes"], 1, MAX_HOST_FILE_BYTES as u64)
            && sha.len() == 64
            && sha
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
            && scratch.is_absolute(),
        "Invalid composition scratch-file reference"
    );
    let path = scratch.join(name);
    let result = (|| {
        let before = fs::symlink_metadata(&path)?;
        ensure!(
            before.is_file(),
            "Composition scratch file is not a regular non-symlink file"
        );
        let file = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(&path)?;
        let after = file.metadata()?;
        let size = reference["bytes"].as_u64().unwrap();
        ensure!(
            after.is_file()
                && after.len() == size
                && after.dev() == before.dev()
                && after.ino() == before.ino(),
            "Composition scratch-file size/type/identity mismatch"
        );
        let mut data = Vec::with_capacity(size as usize);
        (&file).take(size + 1).read_to_end(&mut data)?;
        ensure!(
            data.len() as u64 == size && file.metadata()?.len() == size,
            "Composition scratch-file byte count mismatch"
        );
        ensure!(
            digest(&data) == sha,
            "Composition scratch-file SHA256 mismatch"
        );
        loads(&data)
    })();
    let removed = fs::remove_file(path);
    if let Err(error) = removed {
        if error.kind() != std::io::ErrorKind::NotFound {
            return Err(error.into());
        }
    }
    result
}
pub(super) fn host_result(response: Value, scratch: &Path, kind: &str) -> Result<Value> {
    let file_key = format!("{kind}_file");
    if let Some(reference) = response.get(&file_key) {
        ensure!(
            response.as_object().unwrap().len() == 1,
            "Invalid composition sidecar envelope"
        );
        return read_host_file(reference, scratch);
    }
    if kind == "context" {
        return Ok(response);
    }
    ensure!(
        exact(&response, &["value"]) && response.get("value").is_some(),
        "Invalid composition/call response; expected {{value}}"
    );
    Ok(response["value"].clone())
}

pub(super) struct Scratch {
    pub root: PathBuf,
    directory: Option<TempDir>,
    files: VecDeque<(PathBuf, usize)>,
    bytes: usize,
}
impl Scratch {
    pub fn new(root: PathBuf) -> Self {
        Self {
            root,
            directory: None,
            files: VecDeque::new(),
            bytes: 0,
        }
    }
    pub fn write(&mut self, data: &[u8], suffix: &str) -> Result<NamedTempFile> {
        ensure!(
            self.root.is_absolute(),
            "OCTET_EXTENSION_SCRATCH must be an absolute host-owned directory"
        );
        if self.directory.is_none() {
            fs::create_dir_all(&self.root)?;
            let directory = tempfile::Builder::new()
                .prefix("codemode-")
                .tempdir_in(&self.root)?;
            fs::set_permissions(directory.path(), fs::Permissions::from_mode(0o700))?;
            self.directory = Some(directory);
        }
        let mut file = tempfile::Builder::new()
            .suffix(&format!(".{suffix}"))
            .tempfile_in(self.directory.as_ref().unwrap().path())?;
        file.write_all(data)?;
        Ok(file)
    }
    pub fn spill(&mut self, text: &str) -> Result<PathBuf> {
        ensure!(
            text.len() <= 64 * MIB,
            "Full output exceeds private scratch retention budget"
        );
        while self.files.len() >= 32 || self.bytes + text.len() > 64 * MIB {
            let (path, size) = self.files.pop_front().unwrap();
            match fs::remove_file(path) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(e.into()),
            }
            self.bytes -= size;
        }
        let (_, path) = self.write(text.as_bytes(), "txt")?.keep()?;
        self.files.push_back((path.clone(), text.len()));
        self.bytes += text.len();
        Ok(path)
    }
}

pub(super) struct Output {
    pub ok: bool,
    pub items: Vec<Value>,
    pub error: Option<Value>,
    pub calls: Vec<Value>,
    pub timeout_ms: u64,
    pub max_calls: u64,
    pub tokens: u64,
    pub elapsed: f64,
}
pub(super) fn format_result(output: Output, scratch: &mut Scratch) -> Value {
    let Output {
        ok,
        items,
        error,
        calls,
        timeout_ms,
        max_calls,
        tokens,
        elapsed,
    } = output;
    let body = items
        .iter()
        .filter(|i| i["type"] == "text")
        .map(|i| i["text"].as_str().unwrap())
        .collect::<Vec<_>>()
        .join("\n");
    let error_text = error
        .as_ref()
        .map(|error| {
            let fallback = error.to_string();
            let message = error["stack"]
                .as_str()
                .filter(|s| !s.is_empty())
                .or_else(|| error["message"].as_str())
                .unwrap_or(&fallback);
            format!(
                "\nScript error:\n{}\nTool calls made before failure are not undone.\n",
                head(message, 4096)
            )
        })
        .unwrap_or_default();
    let header = format!(
        "Script {}\nWall time {elapsed:.1} seconds\nOutput:\n",
        if ok { "completed" } else { "failed" }
    );
    let available = OUTPUT_BYTES.saturating_sub(header.len() + error_text.len() + 2048);
    let budget = tokens.saturating_mul(4).min(available as u64) as usize;
    let units = body.encode_utf16().count();
    let truncated = units > budget || body.len() > available;
    let mut metadata = json!({"timeout_ms":timeout_ms, "max_calls":max_calls, "output_truncated":truncated, "call_count":calls.len(), "calls":[], "calls_omitted":0});
    let mut visible = body.clone();
    if truncated {
        let utf16: Vec<u16> = body.encode_utf16().collect();
        // Discard any surrogate cut at the boundary, as the Python adapter did.
        let decode = |s: &[u16]| {
            char::decode_utf16(s.iter().copied())
                .filter_map(std::result::Result::ok)
                .collect::<String>()
        };
        let start = decode(&utf16[..(budget / 2).min(units)]);
        let end = decode(&utf16[units.saturating_sub(budget - budget / 2)..]);
        visible = format!(
            "Warning: truncated output (original estimated tokens: {})\n{}\n…output omitted…\n{}",
            units.div_ceil(4),
            head(&start, available / 2),
            tail(&end, available.div_ceil(2))
        );
        match scratch.spill(&(body + &error_text)) {
            Ok(path) => {
                metadata["full_output_path"] = json!(path);
                visible += &format!(
                    "\n[Full UTF-8 output: {} (read with offset/limit; oldest files expire)]",
                    path.display()
                );
            }
            Err(e) => {
                visible += &format!(
                    "\n[Could not save full output: {}]",
                    head(&e.to_string(), 500)
                )
            }
        }
    }
    if let Some(error) = error {
        metadata["error_kind"] = error.get("kind").cloned().unwrap_or(json!("sandbox"));
    }
    let mut used = metadata.to_string().len();
    for call in calls {
        let size = call.to_string().len() + 1;
        if used + size > 60 * 1024 {
            metadata["calls_omitted"] = json!(metadata["calls_omitted"].as_u64().unwrap() + 1);
        } else {
            metadata["calls"].as_array_mut().unwrap().push(call);
            used += size;
        }
    }
    let text = format!(
        "{header}{}{error_text}",
        head(
            &visible,
            OUTPUT_BYTES.saturating_sub(header.len() + error_text.len())
        )
    );
    let mut content = vec![json!({"type":"text", "text":text})];
    content.extend(items.into_iter().filter(|i| i["type"] == "image"));
    json!({"content":content, "is_error":!ok, "metadata":metadata})
}

#[cfg(test)]
mod tests {
    use super::*;
    fn offer() -> Value {
        json!({"api_version":"0.4","octet_version":"0.9.0","extension":{"name":"octet-codemode","version":"0.9.0"},"protocol":{"version":"0.4","required_features":["request_cancellation","content_parts"],"optional_features":["tool_composition_v1","request_progress","artifacts"],"limits":{"max_concurrent_requests":8,"resource_refs_v1":{"max_records":128,"max_registrations_per_parent":16}},"bulk_objects_v1":{"profile":"local-file.v1"}},"flag_values":[]})
    }
    fn context() -> Value {
        json!({"tools":[{"name":"first","description":"Read numbers","parameters":{"type":"object"},"output_schema":{"type":"object"}}],"store":{"previous":5},"limits":{"timeout_ms":30000,"max_calls":256}})
    }
    fn host_file(root: &Path, value: Value) -> Value {
        let data = value.to_string();
        fs::write(root.join("host.json"), &data).unwrap();
        json!({"path":"host.json","bytes":data.len(),"sha256":digest(data.as_bytes())})
    }
    #[test]
    fn negotiation_and_upstream_grammar() {
        let mut init = offer();
        init["flag_values"] = json!([{"name":"codemode-mode","value":"only"},{"name":"codemode-inline-budget","value":16000}]);
        assert_eq!(
            negotiate(&init).unwrap().result["tools"][0]["composition"],
            json!({"mode":"only","inline_budget":16000})
        );
        let upstream = include_str!("../../vendor/pi-codemode/dist/source.js");
        let literal = upstream
            .split_once("CODEMODE_SOURCE_GRAMMAR = String.raw `")
            .unwrap()
            .1
            .split_once("`;")
            .unwrap()
            .0;
        assert_eq!(GRAMMAR, literal);
        for (key, value) in [
            ("api_version", json!("0.3")),
            (
                "flag_values",
                json!([{"name":"codemode-mode","value":"off"}]),
            ),
        ] {
            let mut bad = init.clone();
            bad[key] = value;
            assert!(negotiate(&bad).is_err());
        }
        init["protocol"]["optional_features"] = json!([]);
        assert!(negotiate(&init).is_err());
        for features in [
            json!(["tool_composition_v1", "content_parts"]),
            json!(["tool_composition_v1", 1]),
        ] {
            let mut bad = offer();
            bad["protocol"]["optional_features"] = features;
            assert!(negotiate(&bad).is_err());
        }
    }
    #[test]
    fn unqualified_extension_value_services_are_not_negotiated() {
        for feature in [
            "resource_refs_v1",
            "operation_descriptors_v1",
            "bulk_objects_v1",
        ] {
            let mut init = offer();
            init["protocol"]["optional_features"]
                .as_array_mut()
                .unwrap()
                .push(json!(feature));
            let negotiated = negotiate(&init).unwrap();
            assert!(!negotiated.features.contains(feature));
            assert!(!negotiated.result["protocol"]["features"]
                .as_array()
                .unwrap()
                .contains(&json!(feature)));
            let mut init = offer();
            init["protocol"]["required_features"]
                .as_array_mut()
                .unwrap()
                .push(json!(feature));
            assert!(negotiate(&init).is_err());
        }
    }
    #[test]
    fn source_and_context_boundaries() {
        for bad in [
            json!({"code":""}),
            json!({"code":"return 1", "extra":true}),
            json!({"code":"x".repeat(65537)}),
            json!({"code":"// @options: {\"timeout_ms\":0}\nreturn 1"}),
            json!({"code":"// @options: {\"unknown\":1}\nreturn 1"}),
            json!({"code":"// @options: {\"max_output_tokens\":true}\nreturn 1"}),
        ] {
            assert!(source(&bad).is_err());
        }
        assert_eq!(
            source(&json!({"code":"// @options: {\"max_output_tokens\":0}\nreturn 1"}))
                .unwrap()
                .tokens,
            0
        );
        assert!(source(&json!({"code":"🌱".repeat(65536)})).is_ok());
        validate_context(&context()).unwrap();
        let mut c = context();
        c["resource_owner"] = json!("override");
        assert!(validate_context(&c).is_err());
        let mut c = context();
        let t = c["tools"][0].clone();
        c["tools"].as_array_mut().unwrap().push(t);
        assert!(validate_context(&c).is_err());
        let mut c = context();
        c["limits"]["timeout_ms"] = json!(30001);
        assert!(validate_context(&c).is_err());
        let mut c = context();
        c["tools"][0]["output_schema"] = json!({"type":"invalid"});
        assert!(validate_context(&c).is_err());
        for bad in [
            json!({"type":["object","object"]}),
            json!({"required":["a","a"]}),
            json!({"allOf":[]}),
            json!({"properties":[]}),
            json!({"minItems":true}),
        ] {
            assert!(schema(&bad, 0).is_err());
        }
        schema(&json!({"type":"object","properties":{"a":{"anyOf":[{"type":"array","items":{"type":"integer"}},false]}}}), 0).unwrap();
        assert!(loads(b"{\"x\":NaN}").is_err());
        assert!(loads(b"\"\\ud800\"").is_err());
        let deep = format!("{}0{}", "[".repeat(33), "]".repeat(33));
        assert!(loads(deep.as_bytes()).is_err());
    }
    #[test]
    fn secure_sidecars() {
        let root = tempfile::tempdir().unwrap();
        let value = json!({"large":"🌱".repeat(300000)});
        let reference = host_file(root.path(), value.clone());
        assert!(reference["bytes"].as_u64().unwrap() > FRAME_BYTES as u64);
        assert_eq!(read_host_file(&reference, root.path()).unwrap(), value);
        assert!(!root.path().join("host.json").exists());
        for (key, value) in [("bytes", json!(3)), ("sha256", json!("0".repeat(64)))] {
            let mut reference = host_file(root.path(), json!({}));
            reference[key] = value;
            assert!(read_host_file(&reference, root.path())
                .unwrap_err()
                .to_string()
                .contains("mismatch"));
            assert!(!root.path().join("host.json").exists());
        }
        let mut reference = host_file(root.path(), json!({"keep":true}));
        reference["path"] = json!("../host.json");
        assert!(read_host_file(&reference, root.path()).is_err());
        std::os::unix::fs::symlink(
            root.path().join("host.json"),
            root.path().join("linked.json"),
        )
        .unwrap();
        reference["path"] = json!("linked.json");
        assert!(read_host_file(&reference, root.path())
            .unwrap_err()
            .to_string()
            .contains("non-symlink"));
        assert!(root.path().join("host.json").exists());
        assert!(fs::symlink_metadata(root.path().join("linked.json")).is_err());
        let data = b"\xff";
        fs::write(root.path().join("bad.json"), data).unwrap();
        assert!(read_host_file(
            &json!({"path":"bad.json","bytes":1,"sha256":digest(data)}),
            root.path()
        )
        .is_err());
        assert!(!root.path().join("bad.json").exists());
    }
    #[test]
    fn spill_permissions_and_retention() {
        let root = tempfile::tempdir().unwrap();
        let mut scratch = Scratch::new(root.path().to_owned());
        let text = "🌱".repeat(20000);
        let result = format_result(
            Output {
                ok: true,
                items: vec![json!({"type":"text","text":text})],
                error: None,
                calls: vec![],
                timeout_ms: 25000,
                max_calls: 256,
                tokens: 0,
                elapsed: 0.0,
            },
            &mut scratch,
        );
        let path = PathBuf::from(result["metadata"]["full_output_path"].as_str().unwrap());
        assert_eq!(fs::read_to_string(&path).unwrap(), text);
        assert!(result["content"][0]["text"].as_str().unwrap().len() <= OUTPUT_BYTES);
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert_eq!(
            fs::metadata(path.parent().unwrap())
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
        for _ in 0..32 {
            scratch.spill("x").unwrap();
        }
        assert!(!path.exists());
        assert_eq!(scratch.files.len(), 32);
        // Aggregate byte retention is independent of the file count.
        let large = "x".repeat(17 * MIB);
        for _ in 0..4 {
            scratch.spill(&large).unwrap();
        }
        assert_eq!(scratch.files.len(), 3);
        assert!(scratch.bytes <= 64 * MIB);
        let directory = scratch.directory.as_ref().unwrap().path().to_owned();
        drop(scratch);
        assert!(!directory.exists());
    }
}

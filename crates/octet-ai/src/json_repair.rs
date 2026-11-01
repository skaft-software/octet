//! Conservative JSON repair for provider-generated tool arguments.
//!
//! Repairs lexical mistakes commonly produced by local/open-compatible models
//! without inventing missing values or closing truncated objects. This keeps
//! tool execution safe: a cut-off command is still rejected rather than being
//! completed speculatively.

use crate::error::DecodeError;
use crate::types::ToolCallArgumentError;
use std::collections::HashSet;

mod formats;
mod keywords;
use keywords::{validate_schema, validate_value};

const VALID_ESCAPES: &[u8] = b"\"\\/bfnrtu";

/// Parse a JSON value after applying conservative provider-output repairs.
pub(crate) fn parse_json_value(input: &str) -> Result<serde_json::Value, DecodeError> {
    let input = strip_json_fence(input.trim());
    if let Ok(value) = serde_json::from_str(input) {
        return Ok(value);
    }

    let repaired = repair_json(input);
    serde_json::from_str(&repaired).map_err(|error| DecodeError::Json(error.to_string()))
}

/// Parse and normalize a repaired JSON object without serializing it first.
pub(crate) fn normalize_json_object_value(input: &str) -> Result<serde_json::Value, DecodeError> {
    let value = parse_json_value(input)?;
    if !value.is_object() {
        return Err(DecodeError::Json(
            "Arguments must be a JSON object".to_string(),
        ));
    }
    Ok(value)
}

/// Normalize provider-generated tool arguments into a canonical JSON object.
pub(crate) fn normalize_json_object(input: &str) -> Result<String, DecodeError> {
    let value = normalize_json_object_value(input)?;
    serde_json::to_string(&value).map_err(|error| DecodeError::Json(error.to_string()))
}

/// Arguments retained for a call whose provider text could not be repaired into
/// a JSON object. Never a guess at the provider's intent: an unexecutable call
/// stays visibly empty instead of being completed speculatively.
pub(crate) const UNREPAIRABLE_TOOL_ARGUMENTS: &str = "{}";

/// Normalize completed provider tool arguments without inventing values.
///
/// The returned marker tells a consumer to pair a bounded error result with the
/// call and never execute it. Conservative repair that cannot recover a JSON
/// object yields [`UNREPAIRABLE_TOOL_ARGUMENTS`] plus
/// [`ToolCallArgumentError::Malformed`], so the call keeps its identity and the
/// model can correct itself. Pi parses such arguments leniently and lets the
/// call fail its own validation for exactly this reason: one malformed call
/// must not end the run.
pub(crate) fn normalize_completed_tool_arguments(
    raw_arguments: &str,
) -> (String, Option<ToolCallArgumentError>) {
    match normalize_json_object(raw_arguments) {
        Ok(arguments_json) => (arguments_json, None),
        Err(_) => (
            UNREPAIRABLE_TOOL_ARGUMENTS.to_owned(),
            Some(ToolCallArgumentError::Malformed),
        ),
    }
}

/// Diagnostic attached to a response that kept a call whose provider arguments
/// could not be repaired. Shared by every transport so the streamed and
/// completed shapes stay observably identical.
pub(crate) fn malformed_tool_arguments_diagnostic() -> crate::Diagnostic {
    crate::Diagnostic {
        code: "malformed_tool_arguments".to_owned(),
        message: "Tool arguments that were not valid JSON and could not be repaired were replaced with an empty object and must not be executed".to_owned(),
    }
}

const MAX_SCHEMA_DEPTH: usize = 32;
const MAX_SCHEMA_NODES: usize = 4_096;
const MAX_SCHEMA_TOOLS: usize = 1_024;
const MAX_SCHEMA_PROPERTY_NAME_BYTES: usize = 256;
const MAX_SCHEMA_ERROR_BYTES: usize = 512;
const MAX_SCHEMA_CONSTANT_BYTES: usize = 64 * 1024;
const MAX_SCHEMA_PATTERN: usize = 1_024;
const UNSUPPORTED_PATTERN: &str = "unsupported JSON Schema pattern: require bounded ^...$ ASCII literals/classes with finite repetitions (1024 bytes/maximum characters)";

struct PatternAtom {
    characters: u128,
    minimum: usize,
    maximum: usize,
}

struct SchemaPattern {
    atoms: Vec<PatternAtom>,
    maximum: usize,
}

impl SchemaPattern {
    fn parse(pattern: &str) -> Result<Self, &'static str> {
        fn class_byte(byte: u8) -> bool {
            (b' '..=b'~').contains(&byte) && !matches!(byte, b'[' | b']' | b'\\' | b'^' | b'-')
        }
        fn count(bytes: &[u8], index: &mut usize) -> Result<usize, &'static str> {
            let start = *index;
            let mut value = 0;
            while let Some(byte) = bytes.get(*index).filter(|byte| byte.is_ascii_digit()) {
                value = value * 10 + usize::from(*byte - b'0');
                if value > MAX_SCHEMA_PATTERN {
                    return Err(UNSUPPORTED_PATTERN);
                }
                *index += 1;
            }
            if *index == start || (*index > start + 1 && bytes[start] == b'0') {
                return Err(UNSUPPORTED_PATTERN);
            }
            Ok(value)
        }
        let bytes = pattern.as_bytes();
        if bytes.len() > MAX_SCHEMA_PATTERN
            || bytes.first() != Some(&b'^')
            || bytes.last() != Some(&b'$')
        {
            return Err(UNSUPPORTED_PATTERN);
        }
        let mut atoms = Vec::new();
        let mut maximum = 0;
        let mut index = 1;
        while index < bytes.len() - 1 {
            let mut characters = 0_u128;
            if bytes[index] == b'[' {
                index += 1;
                let start = index;
                loop {
                    let byte = *bytes.get(index).ok_or(UNSUPPORTED_PATTERN)?;
                    if byte == b']' {
                        break;
                    }
                    if byte == b'-' && (index == start || bytes.get(index + 1) == Some(&b']')) {
                        characters |= 1_u128 << byte;
                        index += 1;
                        continue;
                    }
                    if !class_byte(byte) {
                        return Err(UNSUPPORTED_PATTERN);
                    }
                    if bytes.get(index + 1) == Some(&b'-') && bytes.get(index + 2) != Some(&b']') {
                        let end = *bytes.get(index + 2).ok_or(UNSUPPORTED_PATTERN)?;
                        if !class_byte(end) || end < byte {
                            return Err(UNSUPPORTED_PATTERN);
                        }
                        for member in byte..=end {
                            characters |= 1_u128 << member;
                        }
                        index += 3;
                    } else {
                        characters |= 1_u128 << byte;
                        index += 1;
                    }
                }
                if index == start {
                    return Err(UNSUPPORTED_PATTERN);
                }
                index += 1;
            } else {
                let byte = bytes[index];
                if !(b' '..=b'~').contains(&byte) || b".+*?^$(){}[]\\|".contains(&byte) {
                    return Err(UNSUPPORTED_PATTERN);
                }
                characters |= 1_u128 << byte;
                index += 1;
            }
            let (mut minimum, mut upper) = (1, 1);
            if bytes.get(index) == Some(&b'{') {
                index += 1;
                minimum = count(bytes, &mut index)?;
                upper = minimum;
                if bytes.get(index) == Some(&b',') {
                    index += 1;
                    upper = count(bytes, &mut index)?;
                }
                if bytes.get(index) != Some(&b'}') || minimum > upper {
                    return Err(UNSUPPORTED_PATTERN);
                }
                index += 1;
            }
            maximum += upper;
            if maximum > MAX_SCHEMA_PATTERN {
                return Err(UNSUPPORTED_PATTERN);
            }
            atoms.push(PatternAtom {
                characters,
                minimum,
                maximum: upper,
            });
        }
        if index != bytes.len() - 1 {
            return Err(UNSUPPORTED_PATTERN);
        }
        Ok(Self { atoms, maximum })
    }

    fn matches(
        &self,
        value: &str,
        budget: &mut ValueValidationBudget,
    ) -> Result<bool, ValueValidationFailure> {
        if value.len() > self.maximum || !value.is_ascii() {
            return Ok(false);
        }
        let bytes = value.as_bytes();
        let mut reachable = vec![false; bytes.len() + 1];
        reachable[0] = true;
        for atom in &self.atoms {
            // Prefix counts answer whether any preceding match can reach this
            // position within the repetition interval. Unlike a greedy matcher,
            // this handles overlapping classes and adjacent variable repeats.
            let mut prefix = vec![0_usize; reachable.len() + 1];
            for (index, reached) in reachable.iter().enumerate() {
                prefix[index + 1] = prefix[index] + usize::from(*reached);
            }
            let mut next = vec![false; reachable.len()];
            let mut after_mismatch = 0;
            for end in 0..=bytes.len() {
                budget.consume()?;
                if end > 0 && atom.characters & (1_u128 << bytes[end - 1]) == 0 {
                    after_mismatch = end;
                }
                if let Some(last_start) = end.checked_sub(atom.minimum) {
                    let first_start = end.saturating_sub(atom.maximum).max(after_mismatch);
                    if first_start <= last_start {
                        next[end] = prefix[last_start + 1] > prefix[first_start];
                    }
                }
            }
            reachable = next;
        }
        Ok(reachable[bytes.len()])
    }
}

fn supported_json_type(name: &str) -> bool {
    matches!(
        name,
        "null" | "boolean" | "object" | "array" | "number" | "integer" | "string"
    )
}

fn truncate_utf8(value: &mut String, max_bytes: usize) {
    if value.len() > max_bytes {
        let mut end = max_bytes;
        while end > 0 && !value.is_char_boundary(end) {
            end -= 1;
        }
        value.truncate(end);
    }
}

fn bounded_text(value: &str, max_bytes: usize) -> String {
    let mut bounded = value.to_owned();
    truncate_utf8(&mut bounded, max_bytes);
    bounded
}

fn schema_path(path: &str, component: &str) -> String {
    let mut child = format!(
        "{path}.{}",
        bounded_text(component, MAX_SCHEMA_PROPERTY_NAME_BYTES)
    );
    truncate_utf8(&mut child, MAX_SCHEMA_ERROR_BYTES);
    child
}

fn index_path(path: &str, index: usize) -> String {
    let mut child = format!("{path}[{index}]");
    truncate_utf8(&mut child, MAX_SCHEMA_ERROR_BYTES);
    child
}

fn bounded_error(prefix: &str, detail: impl Into<String>) -> DecodeError {
    let mut message = format!("{prefix}: {}", detail.into());
    truncate_utf8(&mut message, MAX_SCHEMA_ERROR_BYTES);
    DecodeError::Json(message)
}

struct ValidationBudget {
    remaining: usize,
}

impl Default for ValidationBudget {
    fn default() -> Self {
        Self {
            remaining: MAX_SCHEMA_NODES,
        }
    }
}

impl ValidationBudget {
    fn consume(&mut self, path: &str) -> Result<(), String> {
        self.remaining = self.remaining.checked_sub(1).ok_or_else(|| {
            format!(
                "validation work limit exceeded (budget) at {}",
                bounded_text(path, MAX_SCHEMA_ERROR_BYTES)
            )
        })?;
        Ok(())
    }
}

/// Validate the shape of every active tool schema before sending it to a provider.
pub(crate) fn validate_tool_definitions(
    tools: &[crate::types::ToolDef],
) -> Result<(), DecodeError> {
    if tools.len() > MAX_SCHEMA_TOOLS {
        return Err(bounded_error(
            "tool argument schema validation failed",
            format!("tool count exceeds {MAX_SCHEMA_TOOLS}"),
        ));
    }
    let mut names = HashSet::new();
    let mut budget = ValidationBudget::default();
    for tool in tools {
        if tool.name.is_empty() || tool.name.len() > MAX_SCHEMA_PROPERTY_NAME_BYTES {
            return Err(bounded_error(
                "tool argument schema validation failed",
                "tool name must contain 1 to 256 bytes",
            ));
        }
        if !names.insert(tool.name.as_str()) {
            return Err(bounded_error(
                "tool argument schema validation failed",
                format!("duplicate tool name `{}`", bounded_text(&tool.name, 128)),
            ));
        }
        validate_schema(
            &tool.parameters,
            &format!("tool `{}`", bounded_text(&tool.name, 128)),
            0,
            &mut budget,
        )
        .map_err(|detail| bounded_error("tool argument schema validation failed", detail))?;
    }
    Ok(())
}

/// Validate one normalized call against the exact request tool-definition snapshot.
///
/// Schema mismatches are a completed, recoverable provider output: callers
/// retain the call and pair it with a bounded tool error. Malformed schemas and
/// validation work-limit exhaustion remain fatal decode errors.
pub(crate) fn validate_tool_arguments(
    tool_name: &str,
    arguments: &serde_json::Value,
    tools: &[crate::types::ToolDef],
) -> Result<crate::types::ToolArgumentValidation, DecodeError> {
    if tools.len() > MAX_SCHEMA_TOOLS {
        return Err(bounded_error(
            "tool argument schema validation failed",
            format!("tool count exceeds {MAX_SCHEMA_TOOLS}"),
        ));
    }
    let mut schema = None;
    for tool in tools {
        if tool.name == tool_name {
            if schema.is_some() {
                return Err(bounded_error(
                    "tool argument schema validation failed",
                    format!(
                        "duplicate schema for tool `{}`",
                        bounded_text(tool_name, 128)
                    ),
                ));
            }
            schema = Some(&tool.parameters);
        }
    }
    let Some(schema) = schema else {
        // Keep unknown calls in the canonical response so the agent dispatcher
        // can return its bounded `unknown tool` result to the model. There is
        // no schema to validate or typed tool to invoke in this case; treating
        // the provider response as a decode failure would prevent that normal
        // recovery path.
        return Ok(crate::types::ToolArgumentValidation::UnknownTool);
    };

    let mut schema_budget = ValidationBudget::default();
    validate_schema(
        schema,
        &format!("tool `{}`", bounded_text(tool_name, 128)),
        0,
        &mut schema_budget,
    )
    .map_err(|detail| bounded_error("tool argument schema validation failed", detail))?;

    let mut value_budget = ValueValidationBudget::default();
    match validate_value(schema, arguments, "$", 0, &mut value_budget) {
        Ok(()) => Ok(crate::types::ToolArgumentValidation::Valid),
        Err(ValueValidationFailure::Mismatch) => {
            Ok(crate::types::ToolArgumentValidation::SchemaMismatch)
        }
        Err(ValueValidationFailure::Unsafe(detail)) => {
            Err(bounded_error("tool argument validation failed", detail))
        }
    }
}

#[allow(missing_docs)]
pub fn validate_json_schema_definition(schema: &serde_json::Value) -> Result<(), DecodeError> {
    validate_schema(schema, "$", 0, &mut ValidationBudget::default())
        .map_err(|detail| bounded_error("JSON Schema validation failed", detail))
}

#[allow(missing_docs)]
pub fn validate_json_schema_value(
    schema: &serde_json::Value,
    value: &serde_json::Value,
) -> Result<bool, DecodeError> {
    validate_json_schema_definition(schema)?;
    match validate_value(schema, value, "$", 0, &mut ValueValidationBudget::default()) {
        Ok(()) => Ok(true),
        Err(ValueValidationFailure::Mismatch) => Ok(false),
        Err(ValueValidationFailure::Unsafe(detail)) => {
            Err(bounded_error("JSON Schema value validation failed", detail))
        }
    }
}

/// Value-validation errors deliberately distinguish a normal schema mismatch
/// from a resource-bound or invariant failure. Only the former is safe to
/// return to the model as a paired tool error.
enum ValueValidationFailure {
    Mismatch,
    Unsafe(&'static str),
}

/// Independent budget for untrusted argument values. Schema validation has its
/// own budget because a valid schema may be used for many calls in one response.
struct ValueValidationBudget {
    remaining: usize,
}

impl Default for ValueValidationBudget {
    fn default() -> Self {
        Self {
            remaining: MAX_SCHEMA_NODES,
        }
    }
}

impl ValueValidationBudget {
    fn consume(&mut self) -> Result<(), ValueValidationFailure> {
        self.remaining = self
            .remaining
            .checked_sub(1)
            .ok_or(ValueValidationFailure::Unsafe(
                "validation work limit exceeded",
            ))?;
        Ok(())
    }
}

fn value_matches_type(value: &serde_json::Value, expected: &str) -> bool {
    match expected {
        "null" => value.is_null(),
        "boolean" => value.is_boolean(),
        "object" => value.is_object(),
        "array" => value.is_array(),
        "number" => value.is_number(),
        "integer" => value.as_f64().is_some_and(|number| number.fract() == 0.0),
        "string" => value.is_string(),
        _ => false,
    }
}

fn strip_json_fence(input: &str) -> &str {
    let Some(after_open) = input.strip_prefix("```") else {
        return input;
    };
    let after_language = after_open
        .strip_prefix("json")
        .or_else(|| after_open.strip_prefix("JSON"))
        .unwrap_or(after_open);
    let after_language = after_language
        .strip_prefix("\r\n")
        .or_else(|| after_language.strip_prefix('\n'))
        .unwrap_or(after_language);
    after_language
        .strip_suffix("```")
        .map(str::trim_end)
        .unwrap_or(input)
}

fn repair_json(input: &str) -> String {
    let lexical = repair_string_literals(input);
    let keys = quote_unquoted_object_keys(&lexical);
    let python = replace_python_literals(&keys);
    remove_trailing_commas(&python)
}

/// Escape raw controls and invalid backslash escapes, and accept Python-style
/// single-quoted strings. This mirrors pi's invalid-escape repair while also
/// covering a frequent local-model dialect.
fn repair_string_literals(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut output = String::with_capacity(input.len() + 8);
    let mut index = 0usize;
    let mut quote: Option<u8> = None;

    while index < bytes.len() {
        let byte = bytes[index];
        if !byte.is_ascii() {
            let character = input[index..].chars().next().expect("valid UTF-8 boundary");
            output.push(character);
            index += character.len_utf8();
            continue;
        }
        match quote {
            None => {
                if byte == b'\'' {
                    quote = Some(b'\'');
                    output.push('"');
                } else {
                    output.push(byte as char);
                    if byte == b'"' {
                        quote = Some(b'"');
                    }
                }
                index += 1;
            }
            Some(active_quote) => {
                if byte == active_quote {
                    output.push('"');
                    quote = None;
                    index += 1;
                    continue;
                }
                if active_quote == b'\'' && byte == b'"' {
                    output.push_str("\\\"");
                    index += 1;
                    continue;
                }
                if byte == b'\\' {
                    let Some(&next) = bytes.get(index + 1) else {
                        output.push_str("\\\\");
                        index += 1;
                        continue;
                    };
                    if active_quote == b'\'' && next == b'\'' {
                        output.push('\'');
                        index += 2;
                        continue;
                    }
                    if next == b'u'
                        && bytes.get(index + 2..index + 6).is_some_and(|digits| {
                            digits.len() == 4
                                && digits.iter().all(|digit| digit.is_ascii_hexdigit())
                        })
                    {
                        output.push_str(std::str::from_utf8(&bytes[index..index + 6]).unwrap());
                        index += 6;
                        continue;
                    }
                    if VALID_ESCAPES.contains(&next) {
                        output.push('\\');
                        output.push(next as char);
                        index += 2;
                        continue;
                    }
                    // Preserve the literal backslash instead of allowing an
                    // invalid JSON escape such as a Windows `\U` path.
                    output.push_str("\\\\");
                    index += 1;
                    continue;
                }
                match byte {
                    b'\n' => output.push_str("\\n"),
                    b'\r' => output.push_str("\\r"),
                    b'\t' => output.push_str("\\t"),
                    0x08 => output.push_str("\\b"),
                    0x0c => output.push_str("\\f"),
                    0x00..=0x1f => {
                        use std::fmt::Write as _;
                        let _ = write!(output, "\\u{byte:04x}");
                    }
                    _ => output.push(byte as char),
                }
                index += 1;
            }
        }
    }

    // Do not synthesize a missing closing quote. Leaving it open makes the
    // final serde parse reject truncated arguments.
    output
}

/// Quote JavaScript/Python-style bare object keys. A key is changed only when
/// it follows an object boundary (`{` or `,`) and is immediately followed by a
/// colon after optional whitespace, so values and truncated structure are not
/// guessed or completed.
fn quote_unquoted_object_keys(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut output = String::with_capacity(input.len() + 8);
    let mut index = 0usize;
    let mut in_string = false;
    let mut escaped = false;

    while index < bytes.len() {
        let byte = bytes[index];
        if !byte.is_ascii() {
            let character = input[index..].chars().next().expect("valid UTF-8 boundary");
            output.push(character);
            index += character.len_utf8();
            continue;
        }
        output.push(byte as char);
        index += 1;
        if in_string {
            if escaped {
                escaped = false;
            } else if byte == b'\\' {
                escaped = true;
            } else if byte == b'"' {
                in_string = false;
            }
            continue;
        }
        if byte == b'"' {
            in_string = true;
            continue;
        }
        if !matches!(byte, b'{' | b',') {
            continue;
        }

        let whitespace_start = index;
        while bytes.get(index).is_some_and(u8::is_ascii_whitespace) {
            index += 1;
        }
        output.push_str(&input[whitespace_start..index]);
        let key_start = index;
        if !bytes
            .get(index)
            .is_some_and(|byte| byte.is_ascii_alphabetic() || *byte == b'_')
        {
            continue;
        }
        index += 1;
        while bytes
            .get(index)
            .is_some_and(|byte| byte.is_ascii_alphanumeric() || matches!(*byte, b'_' | b'-' | b'.'))
        {
            index += 1;
        }
        let key_end = index;
        while bytes.get(index).is_some_and(u8::is_ascii_whitespace) {
            index += 1;
        }
        if bytes.get(index) == Some(&b':') {
            output.push('"');
            output.push_str(&input[key_start..key_end]);
            output.push('"');
            output.push_str(&input[key_end..=index]);
            index += 1;
        } else {
            output.push_str(&input[key_start..index]);
        }
    }
    output
}

fn replace_python_literals(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut output = String::with_capacity(input.len());
    let mut index = 0usize;
    let mut in_string = false;
    let mut escaped = false;

    while index < bytes.len() {
        let byte = bytes[index];
        if !byte.is_ascii() {
            let character = input[index..].chars().next().expect("valid UTF-8 boundary");
            output.push(character);
            index += character.len_utf8();
            continue;
        }
        if in_string {
            output.push(byte as char);
            if escaped {
                escaped = false;
            } else if byte == b'\\' {
                escaped = true;
            } else if byte == b'"' {
                in_string = false;
            }
            index += 1;
            continue;
        }
        if byte == b'"' {
            in_string = true;
            output.push('"');
            index += 1;
            continue;
        }

        let rest = &input[index..];
        let replacement = [("None", "null"), ("True", "true"), ("False", "false")]
            .into_iter()
            .find(|(token, _)| {
                rest.starts_with(token)
                    && token_boundary(bytes.get(index.wrapping_sub(1)).copied())
                    && token_boundary(bytes.get(index + token.len()).copied())
            });
        if let Some((token, value)) = replacement {
            output.push_str(value);
            index += token.len();
        } else {
            output.push(byte as char);
            index += 1;
        }
    }
    output
}

fn token_boundary(byte: Option<u8>) -> bool {
    byte.is_none_or(|byte| !(byte.is_ascii_alphanumeric() || byte == b'_'))
}

fn remove_trailing_commas(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut output = String::with_capacity(input.len());
    let mut index = 0usize;
    let mut in_string = false;
    let mut escaped = false;

    while index < bytes.len() {
        let byte = bytes[index];
        if !byte.is_ascii() {
            let character = input[index..].chars().next().expect("valid UTF-8 boundary");
            output.push(character);
            index += character.len_utf8();
            continue;
        }
        if in_string {
            output.push(byte as char);
            if escaped {
                escaped = false;
            } else if byte == b'\\' {
                escaped = true;
            } else if byte == b'"' {
                in_string = false;
            }
            index += 1;
            continue;
        }
        if byte == b'"' {
            in_string = true;
            output.push('"');
            index += 1;
            continue;
        }
        if byte == b',' {
            let next = bytes[index + 1..]
                .iter()
                .copied()
                .find(|candidate| !candidate.is_ascii_whitespace());
            if matches!(next, Some(b'}' | b']')) {
                index += 1;
                continue;
            }
        }
        output.push(byte as char);
        index += 1;
    }
    output
}

#[cfg(test)]
mod schema_profile_tests;
#[cfg(test)]
mod tests;

#[cfg(test)]
mod keyword_tests;

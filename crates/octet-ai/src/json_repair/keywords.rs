use super::*;
use serde_json::{Map, Value};
use std::collections::BTreeSet;

const KEYWORDS: &[&str] = &[
    "$schema",
    "$id",
    "$ref",
    "$defs",
    "definitions",
    "$anchor",
    "$dynamicAnchor",
    "$dynamicRef",
    "$recursiveAnchor",
    "$recursiveRef",
    "$vocabulary",
    "$comment",
    "title",
    "description",
    "default",
    "examples",
    "type",
    "properties",
    "required",
    "additionalProperties",
    "patternProperties",
    "propertyNames",
    "dependencies",
    "dependentRequired",
    "dependentSchemas",
    "unevaluatedProperties",
    "items",
    "additionalItems",
    "prefixItems",
    "contains",
    "minContains",
    "maxContains",
    "unevaluatedItems",
    "enum",
    "const",
    "allOf",
    "anyOf",
    "oneOf",
    "not",
    "if",
    "then",
    "else",
    "minimum",
    "maximum",
    "exclusiveMinimum",
    "exclusiveMaximum",
    "multipleOf",
    "minLength",
    "maxLength",
    "minItems",
    "maxItems",
    "uniqueItems",
    "minProperties",
    "maxProperties",
    "pattern",
    "format",
    "deprecated",
    "readOnly",
    "writeOnly",
    "contentEncoding",
    "contentMediaType",
    "contentSchema",
];

fn ecma_pattern(pattern: &str) -> Result<String, String> {
    let mut output = String::with_capacity(pattern.len());
    let mut chars = pattern.chars().peekable();
    let mut class = false;
    let mut quantified = false;
    let mut lazy = false;
    while let Some(ch) = chars.next() {
        match ch {
            '\\' => {
                let escaped = chars.next().ok_or("incomplete pattern escape")?;
                if matches!(escaped, 'p' | 'P' | 'u') && chars.peek() == Some(&'{') {
                    output.push('\\');
                    output.push(escaped);
                    output.push(chars.next().unwrap());
                    for part in chars.by_ref() {
                        output.push(part);
                        if part == '}' {
                            break;
                        }
                    }
                    quantified = false;
                    continue;
                }
                let replacement = match escaped {
                    'd' => Some("[0-9]"),
                    'D' => Some("[^0-9]"),
                    'w' => Some("[A-Za-z0-9_]"),
                    'W' => Some("[^A-Za-z0-9_]"),
                    's' => Some(
                        r"[\u{0009}-\u{000D}\u{0020}\u{00A0}\u{1680}\u{2000}-\u{200A}\u{2028}\u{2029}\u{202F}\u{205F}\u{3000}\u{FEFF}]",
                    ),
                    'S' => Some(
                        r"[^\u{0009}-\u{000D}\u{0020}\u{00A0}\u{1680}\u{2000}-\u{200A}\u{2028}\u{2029}\u{202F}\u{205F}\u{3000}\u{FEFF}]",
                    ),
                    'b' if class => Some(r"\x08"),
                    'b' => Some(
                        r"(?:(?<=[A-Za-z0-9_])(?![A-Za-z0-9_])|(?<![A-Za-z0-9_])(?=[A-Za-z0-9_]))",
                    ),
                    'B' if !class => Some(
                        r"(?:(?<=[A-Za-z0-9_])(?=[A-Za-z0-9_])|(?<![A-Za-z0-9_])(?![A-Za-z0-9_]))",
                    ),
                    '/' => Some("/"),
                    _ => None,
                };
                if let Some(replacement) = replacement {
                    output.push_str(replacement);
                } else {
                    output.push('\\');
                    output.push(escaped);
                }
                quantified = false;
            }
            '(' if !class && chars.peek() == Some(&'?') => {
                let mut group = chars.clone();
                group.next();
                if !matches!(group.next(), Some(':' | '=' | '!' | '<')) {
                    return Err("invalid ECMAScript group".into());
                }
                output.push('(');
                quantified = false;
            }
            '&' | '~' if class => {
                output.push('\\');
                output.push(ch);
            }
            '-' if class && chars.peek() == Some(&'-') => {
                if output.ends_with('[') || output.ends_with("[^") {
                    output.push_str(r"\-");
                } else {
                    return Err("invalid ECMAScript class range".into());
                }
            }
            '[' if class => output.push_str(r"\["),
            '[' => {
                if chars.peek() == Some(&']') {
                    chars.next();
                    output.push_str("(?!)");
                } else if chars.clone().take(2).collect::<String>() == "^]" {
                    chars.next();
                    chars.next();
                    output.push_str(r"[\s\S]");
                } else {
                    class = true;
                    output.push('[');
                }
                quantified = false;
            }
            ']' => {
                class = false;
                output.push(']');
                quantified = false;
            }
            '.' if !class => {
                output.push_str(r"[^\n\r\u{2028}\u{2029}]");
                quantified = false;
            }
            '*' | '+' | '?' if !class => {
                let group = ch == '?' && output.ends_with('(');
                if (lazy || ch != '?') && quantified {
                    return Err("invalid repeated pattern quantifier".into());
                }
                lazy = quantified && ch == '?';
                quantified = !group;
                output.push(ch);
            }
            '{' if !class => {
                if quantified {
                    return Err("invalid repeated pattern quantifier".into());
                }
                output.push(ch);
            }
            '}' if !class => {
                output.push(ch);
                quantified = true;
                lazy = false;
            }
            _ => {
                output.push(ch);
                quantified = false;
            }
        }
    }
    Ok(output)
}

pub(super) fn ecma_regex(pattern: &str) -> Result<fancy_regex::Regex, String> {
    if pattern.len() > MAX_SCHEMA_PATTERN {
        return Err("schema pattern exceeds 1024 bytes".into());
    }
    let translated =
        ecma_pattern(pattern).map_err(|detail| format!("invalid schema pattern: {detail}"))?;
    fancy_regex::RegexBuilder::new(&translated)
        .backtrack_limit(MAX_SCHEMA_NODES)
        .delegate_size_limit(256 * 1024)
        .delegate_dfa_size_limit(256 * 1024)
        .build()
        .map_err(|_| "invalid or over-budget schema pattern".into())
}

pub(super) fn validate_schema(
    schema: &Value,
    path: &str,
    depth: usize,
    budget: &mut ValidationBudget,
) -> Result<(), String> {
    if depth > MAX_SCHEMA_DEPTH {
        return Err(format!("schema nesting exceeds {MAX_SCHEMA_DEPTH}"));
    }
    budget.consume(path)?;
    if schema.is_boolean() {
        return Ok(());
    }
    let object = schema
        .as_object()
        .ok_or("schema nodes must be objects or booleans")?;
    for (keyword, value) in object {
        budget.consume(path)?;
        if !KEYWORDS.contains(&keyword.as_str()) {
            return Err(format!(
                "unsupported JSON Schema keyword `{}` at {}",
                bounded_text(keyword, 128),
                bounded_text(path, MAX_SCHEMA_ERROR_BYTES)
            ));
        }
        match keyword.as_str() {
            "pattern" => {
                ecma_regex(value.as_str().ok_or("pattern must be a string")?)?;
            }
            "$vocabulary" => {
                if !value
                    .as_object()
                    .is_some_and(|v| v.values().all(Value::is_boolean))
                {
                    return Err("$vocabulary must map URIs to booleans".into());
                }
            }
            "deprecated" | "readOnly" | "writeOnly" | "$recursiveAnchor" | "uniqueItems" => {
                if !value.is_boolean() {
                    return Err(format!("{keyword} must be boolean"));
                }
            }
            "$schema" | "$id" | "$ref" | "$anchor" | "$dynamicAnchor" | "$dynamicRef"
            | "$recursiveRef" | "$comment" | "title" | "description" | "format"
            | "contentEncoding" | "contentMediaType" => {
                if !value.is_string() {
                    return Err(format!("{keyword} must be a string"));
                }
            }
            "type" => {
                let valid = match value {
                    Value::String(name) => supported_json_type(name),
                    Value::Array(names) => {
                        !names.is_empty()
                            && names
                                .iter()
                                .all(|v| v.as_str().is_some_and(supported_json_type))
                    }
                    _ => false,
                };
                if !valid {
                    return Err("type must name supported JSON types".into());
                }
            }
            "properties" | "patternProperties" | "$defs" | "definitions" | "dependentSchemas"
            | "dependencies" => {
                for (name, child) in value
                    .as_object()
                    .ok_or_else(|| format!("{keyword} must be an object"))?
                {
                    if name.len() > MAX_SCHEMA_PROPERTY_NAME_BYTES && keyword != "patternProperties"
                    {
                        return Err("property name exceeds 256 bytes".into());
                    }
                    if keyword == "patternProperties" {
                        ecma_regex(name)?;
                    }
                    if keyword == "dependencies" && child.is_array() {
                        string_array(child, budget, path)?;
                    } else {
                        validate_schema(child, &schema_path(path, name), depth + 1, budget)?;
                    }
                }
            }
            "dependentRequired" => {
                for child in value
                    .as_object()
                    .ok_or("dependentRequired must be an object")?
                    .values()
                {
                    string_array(child, budget, path)?;
                }
            }
            "required" => string_array(value, budget, path)?,
            "additionalProperties"
            | "additionalItems"
            | "unevaluatedProperties"
            | "unevaluatedItems"
            | "propertyNames"
            | "contains"
            | "not"
            | "if"
            | "then"
            | "else"
            | "contentSchema" => {
                validate_schema(value, &schema_path(path, keyword), depth + 1, budget)?
            }
            "items" if !value.is_array() => {
                validate_schema(value, &schema_path(path, keyword), depth + 1, budget)?
            }
            "items" | "allOf" | "anyOf" | "oneOf" | "prefixItems" => {
                let branches = value
                    .as_array()
                    .filter(|a| !a.is_empty())
                    .ok_or_else(|| format!("{keyword} must be a non-empty array"))?;
                for (index, branch) in branches.iter().enumerate() {
                    validate_schema(branch, &index_path(path, index), depth + 1, budget)?;
                }
            }
            "minimum" | "maximum" | "exclusiveMinimum" | "exclusiveMaximum" | "multipleOf" => {
                if !value.is_number()
                    || keyword == "multipleOf" && value.as_f64().is_none_or(|n| n <= 0.0)
                {
                    return Err(format!(
                        "{keyword} must be a {}number",
                        if keyword == "multipleOf" {
                            "positive "
                        } else {
                            ""
                        }
                    ));
                }
            }
            "minLength" | "maxLength" | "minItems" | "maxItems" | "minProperties"
            | "maxProperties" | "minContains" | "maxContains" => {
                if value.as_u64().is_none() {
                    return Err(format!("{keyword} must be a non-negative integer"));
                }
            }
            "enum" => {
                for child in value
                    .as_array()
                    .filter(|a| !a.is_empty())
                    .ok_or("enum must be a non-empty array")?
                {
                    budget.consume(path)?;
                    constant(child)?;
                }
            }
            "default" | "examples" | "const" => constant(value)?,
            _ => {}
        }
    }
    Ok(())
}

fn constant(value: &Value) -> Result<(), String> {
    if serde_json::to_vec(value)
        .map_err(|_| "cannot encode schema constant")?
        .len()
        > MAX_SCHEMA_CONSTANT_BYTES
    {
        return Err("schema constant exceeds 65536 bytes".into());
    }
    Ok(())
}

fn string_array(value: &Value, budget: &mut ValidationBudget, path: &str) -> Result<(), String> {
    let mut names = HashSet::new();
    for name in value
        .as_array()
        .ok_or("property dependencies must be arrays")?
    {
        budget.consume(path)?;
        let name = name.as_str().ok_or("required entries must be strings")?;
        if !names.insert(name) {
            return Err("duplicate required property".into());
        }
    }
    Ok(())
}

#[derive(Default)]
struct Evaluated {
    keys: BTreeSet<String>,
    indices: BTreeSet<usize>,
}
impl Evaluated {
    fn merge(&mut self, other: Self) {
        self.keys.extend(other.keys);
        self.indices.extend(other.indices);
    }
}

type Outcome = Result<Evaluated, ValueValidationFailure>;

fn matches_pattern(
    pattern: &str,
    value: &str,
    budget: &mut ValueValidationBudget,
) -> Result<bool, ValueValidationFailure> {
    if let Ok(portable) = SchemaPattern::parse(pattern) {
        return portable.matches(value, budget);
    }
    for _ in 0..=value.len() / 32 {
        budget.consume()?;
    }
    ecma_regex(pattern)
        .map_err(|_| ValueValidationFailure::Unsafe("invalid schema pattern"))?
        .is_match(value)
        .map_err(|_| ValueValidationFailure::Unsafe("schema pattern work limit exceeded"))
}

fn matched(result: Outcome) -> Result<Option<Evaluated>, ValueValidationFailure> {
    match result {
        Ok(evaluated) => Ok(Some(evaluated)),
        Err(ValueValidationFailure::Mismatch) => Ok(None),
        Err(error) => Err(error),
    }
}

fn find_anchor<'a>(
    schema: &'a Value,
    name: &str,
    budget: &mut ValueValidationBudget,
) -> Result<Option<&'a Value>, ValueValidationFailure> {
    budget.consume()?;
    if let Some(object) = schema.as_object() {
        if ["$anchor", "$dynamicAnchor", "$id"]
            .iter()
            .any(|key| object.get(*key).and_then(Value::as_str) == Some(name))
        {
            return Ok(Some(schema));
        }
        for (key, value) in object {
            if matches!(
                key.as_str(),
                "properties"
                    | "patternProperties"
                    | "$defs"
                    | "definitions"
                    | "dependentSchemas"
                    | "dependencies"
            ) {
                if let Some(children) = value.as_object() {
                    for child in children.values() {
                        if let Some(found) = find_anchor(child, name, budget)? {
                            return Ok(Some(found));
                        }
                    }
                }
            } else if matches!(
                key.as_str(),
                "allOf" | "anyOf" | "oneOf" | "prefixItems" | "items"
            ) && value.is_array()
            {
                for child in value.as_array().unwrap() {
                    if let Some(found) = find_anchor(child, name, budget)? {
                        return Ok(Some(found));
                    }
                }
            } else if matches!(
                key.as_str(),
                "additionalProperties"
                    | "additionalItems"
                    | "items"
                    | "contains"
                    | "propertyNames"
                    | "not"
                    | "if"
                    | "then"
                    | "else"
                    | "unevaluatedProperties"
                    | "unevaluatedItems"
            ) {
                if let Some(found) = find_anchor(value, name, budget)? {
                    return Ok(Some(found));
                }
            }
        }
    }
    Ok(None)
}

fn resolve<'a>(
    root: &'a Value,
    schema: &'a Value,
    reference: &str,
    kind: &str,
    stack: &[&'a Value],
    budget: &mut ValueValidationBudget,
) -> Result<Option<&'a Value>, ValueValidationFailure> {
    if kind == "$recursiveRef" && reference == "#" {
        if let Some(target) = stack
            .iter()
            .find(|node| node.get("$recursiveAnchor") == Some(&Value::Bool(true)))
        {
            return Ok(Some(target));
        }
    }
    if kind == "$dynamicRef" {
        if let Some(name) = reference.strip_prefix('#') {
            if let Some(target) = stack
                .iter()
                .find(|node| node.get("$dynamicAnchor").and_then(Value::as_str) == Some(name))
            {
                return Ok(Some(target));
            }
        }
    }
    let lexical = stack
        .iter()
        .rev()
        .find(|node| node.get("$id").is_some())
        .copied()
        .unwrap_or(root);
    let (resource, fragment) = reference.split_once('#').unwrap_or((reference, ""));
    let target =
        if resource.is_empty() || lexical.get("$id").and_then(Value::as_str) == Some(resource) {
            lexical
        } else if let Some(found) = find_anchor(root, resource, budget)? {
            found
        } else {
            return Ok(None);
        };
    if fragment.is_empty() {
        return Ok(Some(target));
    }
    if fragment.starts_with('/') {
        return Ok(target.pointer(fragment));
    }
    let _ = schema;
    find_anchor(target, fragment, budget)
}

pub(super) fn validate_value(
    schema: &Value,
    value: &Value,
    path: &str,
    depth: usize,
    budget: &mut ValueValidationBudget,
) -> Result<(), ValueValidationFailure> {
    evaluate(schema, schema, value, path, depth, budget, &mut Vec::new()).map(|_| ())
}

fn evaluate<'a>(
    root: &'a Value,
    schema: &'a Value,
    value: &Value,
    path: &str,
    depth: usize,
    budget: &mut ValueValidationBudget,
    stack: &mut Vec<&'a Value>,
) -> Outcome {
    if depth > MAX_SCHEMA_DEPTH {
        return Err(ValueValidationFailure::Unsafe(
            "validation nesting limit exceeded",
        ));
    }
    budget.consume()?;
    if let Some(allowed) = schema.as_bool() {
        return if allowed {
            Ok(Evaluated::default())
        } else {
            Err(ValueValidationFailure::Mismatch)
        };
    }
    let object = schema.as_object().ok_or(ValueValidationFailure::Unsafe(
        "validated schema node is malformed",
    ))?;
    stack.push(schema);
    let result = evaluate_object(root, object, value, path, depth, budget, stack);
    stack.pop();
    result
}

fn evaluate_object<'a>(
    root: &'a Value,
    object: &'a Map<String, Value>,
    value: &Value,
    path: &str,
    depth: usize,
    budget: &mut ValueValidationBudget,
    stack: &mut Vec<&'a Value>,
) -> Outcome {
    let mut evaluated = Evaluated::default();
    let mismatch = || ValueValidationFailure::Mismatch;
    if let Some(expected) = object.get("type") {
        let accepted = match expected {
            Value::String(name) => value_matches_type(value, name),
            Value::Array(names) => names
                .iter()
                .filter_map(Value::as_str)
                .any(|name| value_matches_type(value, name)),
            _ => false,
        };
        if !accepted {
            return Err(mismatch());
        }
    }
    if let Some(values) = object.get("enum").and_then(Value::as_array) {
        let mut accepted = false;
        for candidate in values {
            budget.consume()?;
            if equal(candidate, value) {
                accepted = true;
                break;
            }
        }
        if !accepted {
            return Err(mismatch());
        }
    }
    if object
        .get("const")
        .is_some_and(|constant| !equal(constant, value))
    {
        return Err(mismatch());
    }
    for kind in ["$ref", "$dynamicRef", "$recursiveRef"] {
        if let Some(reference) = object.get(kind).and_then(Value::as_str) {
            let target = resolve(root, stack.last().unwrap(), reference, kind, stack, budget)?
                .ok_or(ValueValidationFailure::Unsafe(
                    "unresolved JSON Schema reference",
                ))?;
            evaluated.merge(evaluate(
                root,
                target,
                value,
                path,
                depth + 1,
                budget,
                stack,
            )?);
        }
    }
    for keyword in ["allOf", "anyOf", "oneOf"] {
        if let Some(branches) = object.get(keyword).and_then(Value::as_array) {
            let mut count = 0;
            for branch in branches {
                match matched(evaluate(
                    root,
                    branch,
                    value,
                    path,
                    depth + 1,
                    budget,
                    stack,
                ))? {
                    Some(result) => {
                        count += 1;
                        evaluated.merge(result);
                    }
                    None if keyword == "allOf" => return Err(mismatch()),
                    None => {}
                }
            }
            if keyword == "anyOf" && count == 0 || keyword == "oneOf" && count != 1 {
                return Err(mismatch());
            }
        }
    }
    if let Some(branch) = object.get("not") {
        if matched(evaluate(
            root,
            branch,
            value,
            path,
            depth + 1,
            budget,
            stack,
        ))?
        .is_some()
        {
            return Err(mismatch());
        }
    }
    if let Some(condition) = object.get("if") {
        let outcome = matched(evaluate(
            root,
            condition,
            value,
            path,
            depth + 1,
            budget,
            stack,
        ))?;
        let keyword = if let Some(result) = outcome {
            evaluated.merge(result);
            "then"
        } else {
            "else"
        };
        if let Some(branch) = object.get(keyword) {
            evaluated.merge(evaluate(
                root,
                branch,
                value,
                path,
                depth + 1,
                budget,
                stack,
            )?);
        }
    }
    if let Some(values) = value.as_object() {
        if let Some(required) = object.get("required").and_then(Value::as_array) {
            for name in required {
                budget.consume()?;
                if !values.contains_key(name.as_str().unwrap()) {
                    return Err(mismatch());
                }
            }
        }
        for keyword in ["dependencies", "dependentRequired", "dependentSchemas"] {
            if let Some(dependencies) = object.get(keyword).and_then(Value::as_object) {
                for (name, dependency) in dependencies {
                    budget.consume()?;
                    if !values.contains_key(name) {
                        continue;
                    }
                    if let Some(names) = dependency.as_array() {
                        for name in names {
                            budget.consume()?;
                            if !values.contains_key(name.as_str().unwrap()) {
                                return Err(mismatch());
                            }
                        }
                    } else {
                        evaluated.merge(evaluate(
                            root,
                            dependency,
                            value,
                            path,
                            depth + 1,
                            budget,
                            stack,
                        )?);
                    }
                }
            }
        }
        let properties = object.get("properties").and_then(Value::as_object);
        let patterns = object.get("patternProperties").and_then(Value::as_object);
        for (name, child) in values {
            budget.consume()?;
            let child_path = schema_path(path, name);
            if let Some(names) = object.get("propertyNames") {
                evaluate(
                    root,
                    names,
                    &Value::String(name.clone()),
                    &child_path,
                    depth + 1,
                    budget,
                    stack,
                )?;
            }
            let mut known = false;
            if let Some(child_schema) = properties.and_then(|p| p.get(name)) {
                known = true;
                evaluate(
                    root,
                    child_schema,
                    child,
                    &child_path,
                    depth + 1,
                    budget,
                    stack,
                )?;
            }
            if let Some(patterns) = patterns {
                for (pattern, child_schema) in patterns {
                    if matches_pattern(pattern, name, budget)? {
                        known = true;
                        evaluate(
                            root,
                            child_schema,
                            child,
                            &child_path,
                            depth + 1,
                            budget,
                            stack,
                        )?;
                    }
                }
            }
            if !known {
                if let Some(additional) = object.get("additionalProperties") {
                    evaluate(
                        root,
                        additional,
                        child,
                        &child_path,
                        depth + 1,
                        budget,
                        stack,
                    )?;
                    known = true;
                }
            }
            if known {
                evaluated.keys.insert(name.clone());
            }
        }
        length(
            object,
            values.len() as u64,
            "minProperties",
            "maxProperties",
        )?;
        if let Some(unevaluated) = object.get("unevaluatedProperties") {
            for (name, child) in values {
                budget.consume()?;
                if !evaluated.keys.contains(name) {
                    evaluate(
                        root,
                        unevaluated,
                        child,
                        &schema_path(path, name),
                        depth + 1,
                        budget,
                        stack,
                    )?;
                    evaluated.keys.insert(name.clone());
                }
            }
        }
    }
    if let Some(values) = value.as_array() {
        let prefix = object.get("prefixItems").and_then(Value::as_array);
        let items = object.get("items");
        let tuple = items.and_then(Value::as_array);
        for (index, child) in values.iter().enumerate() {
            budget.consume()?;
            let schema = if let Some(prefix) = prefix {
                prefix.get(index).or(items)
            } else if let Some(tuple) = tuple {
                tuple.get(index).or_else(|| object.get("additionalItems"))
            } else {
                items
            };
            if let Some(schema) = schema {
                evaluate(
                    root,
                    schema,
                    child,
                    &index_path(path, index),
                    depth + 1,
                    budget,
                    stack,
                )?;
                evaluated.indices.insert(index);
            }
        }
        if let Some(contains) = object.get("contains") {
            let mut count = 0;
            for (index, child) in values.iter().enumerate() {
                if matched(evaluate(
                    root,
                    contains,
                    child,
                    &index_path(path, index),
                    depth + 1,
                    budget,
                    stack,
                ))?
                .is_some()
                {
                    count += 1;
                    evaluated.indices.insert(index);
                }
            }
            let minimum = object
                .get("minContains")
                .and_then(Value::as_u64)
                .unwrap_or(1);
            if count < minimum
                || object
                    .get("maxContains")
                    .and_then(Value::as_u64)
                    .is_some_and(|maximum| count > maximum)
            {
                return Err(mismatch());
            }
        }
        length(object, values.len() as u64, "minItems", "maxItems")?;
        if object.get("uniqueItems") == Some(&Value::Bool(true)) {
            for (index, child) in values.iter().enumerate() {
                for previous in &values[..index] {
                    budget.consume()?;
                    if equal(previous, child) {
                        return Err(mismatch());
                    }
                }
            }
        }
        if let Some(unevaluated) = object.get("unevaluatedItems") {
            for (index, child) in values.iter().enumerate() {
                budget.consume()?;
                if !evaluated.indices.contains(&index) {
                    evaluate(
                        root,
                        unevaluated,
                        child,
                        &index_path(path, index),
                        depth + 1,
                        budget,
                        stack,
                    )?;
                    evaluated.indices.insert(index);
                }
            }
        }
    }
    if let Some(string) = value.as_str() {
        length(
            object,
            string.chars().count() as u64,
            "minLength",
            "maxLength",
        )?;
        if let Some(pattern) = object.get("pattern").and_then(Value::as_str) {
            if !matches_pattern(pattern, string, budget)? {
                return Err(mismatch());
            }
        }
        if let Some(format) = object.get("format").and_then(Value::as_str) {
            if !super::formats::check(format, string, budget)? {
                return Err(mismatch());
            }
        }
    }
    if let Some(number) = value.as_f64() {
        for (keyword, invalid) in [
            ("minimum", false),
            ("maximum", false),
            ("exclusiveMinimum", true),
            ("exclusiveMaximum", true),
        ] {
            if let Some(limit) = object.get(keyword).and_then(Value::as_f64) {
                let less = keyword.ends_with("Minimum") || keyword == "minimum";
                if if less {
                    number < limit || invalid && number == limit
                } else {
                    number > limit || invalid && number == limit
                } {
                    return Err(mismatch());
                }
            }
        }
        if let Some(divisor) = object.get("multipleOf").and_then(Value::as_f64) {
            if !(number.fract() == 0.0 && (1.0 / divisor).fract() == 0.0) {
                let remainder = number % divisor;
                if remainder
                    .abs()
                    .min((remainder - divisor).abs())
                    .min((remainder + divisor).abs())
                    >= 1e-10
                {
                    return Err(mismatch());
                }
            }
        }
    }
    Ok(evaluated)
}

fn length(
    object: &Map<String, Value>,
    count: u64,
    minimum: &str,
    maximum: &str,
) -> Result<(), ValueValidationFailure> {
    if object
        .get(minimum)
        .and_then(Value::as_u64)
        .is_some_and(|n| count < n)
        || object
            .get(maximum)
            .and_then(Value::as_u64)
            .is_some_and(|n| count > n)
    {
        return Err(ValueValidationFailure::Mismatch);
    }
    Ok(())
}

fn equal(left: &Value, right: &Value) -> bool {
    match (left, right) {
        (Value::Number(a), Value::Number(b)) => {
            if let (Some(a), Some(b)) = (a.as_i64(), b.as_i64()) {
                return a == b;
            }
            if let (Some(a), Some(b)) = (a.as_u64(), b.as_u64()) {
                return a == b;
            }
            if a.is_f64() && b.is_f64() {
                return a.as_f64() == b.as_f64();
            }
            let (integer, float) = if a.is_f64() { (b, a) } else { (a, b) };
            let Some(float) = float.as_f64().filter(|_| float.is_f64()) else {
                return false;
            };
            if float.fract() != 0.0 {
                return false;
            }
            if float < 0.0 {
                float >= i64::MIN as f64 && integer.as_i64() == Some(float as i64)
            } else {
                float < u64::MAX as f64 && integer.as_u64() == Some(float as u64)
            }
        }
        (Value::Array(a), Value::Array(b)) => {
            a.len() == b.len() && a.iter().zip(b).all(|(a, b)| equal(a, b))
        }
        (Value::Object(a), Value::Object(b)) => {
            a.len() == b.len()
                && a.iter()
                    .all(|(key, a)| b.get(key).is_some_and(|b| equal(a, b)))
        }
        _ => left == right,
    }
}

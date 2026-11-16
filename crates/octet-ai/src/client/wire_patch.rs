//! Retain codec bytes for JSON nodes a payload hook did not change.
//! No cross-turn snapshots: the encoded request is the sole wire authority.

use serde_json::{value::RawValue, Value};
use std::collections::BTreeMap;

pub(super) fn encode(original: &[u8], replacement: &Value) -> Result<Vec<u8>, serde_json::Error> {
    let before: Value = serde_json::from_slice(original)?;
    let raw = std::str::from_utf8(original).expect("decoded JSON is UTF-8");
    let mut output = Vec::with_capacity(original.len());
    patch(raw, &before, replacement, &mut output)?;
    Ok(output)
}

fn patch(
    raw: &str,
    before: &Value,
    after: &Value,
    out: &mut Vec<u8>,
) -> Result<(), serde_json::Error> {
    if before == after {
        out.extend_from_slice(raw.as_bytes());
        return Ok(());
    }
    match (before, after) {
        (Value::Object(before), Value::Object(after)) => {
            let fields: BTreeMap<String, &RawValue> = serde_json::from_str(raw)?;
            let mut fields: Vec<_> = fields.into_iter().collect();
            // serde_json::Value sorts map keys. Recover the codec's wire order
            // from borrowed raw spans instead of imposing that map order.
            fields.sort_unstable_by_key(|(_, value)| value.get().as_ptr() as usize);
            let mut cursor = raw.find('{').expect("object");
            out.extend_from_slice(&raw.as_bytes()[..=cursor]);
            cursor += 1;
            let mut count = 0;
            for (key, value) in &fields {
                let start = value.get().as_ptr() as usize - raw.as_ptr() as usize;
                if let Some(next) = after.get(key) {
                    member_prefix(&raw[cursor..start], count, out);
                    patch(value.get(), &before[key], next, out)?;
                    count += 1;
                }
                cursor = start + value.get().len();
            }
            for (key, value) in after {
                if !before.contains_key(key) {
                    if count > 0 {
                        out.push(b',');
                    }
                    serde_json::to_writer(&mut *out, key)?;
                    out.push(b':');
                    serde_json::to_writer(&mut *out, value)?;
                    count += 1;
                }
            }
            out.extend_from_slice(&raw.as_bytes()[cursor..]);
        }
        (Value::Array(before), Value::Array(after)) => {
            let values: Vec<&RawValue> = serde_json::from_str(raw)?;
            let mut cursor = raw.find('[').expect("array");
            out.extend_from_slice(&raw.as_bytes()[..=cursor]);
            cursor += 1;
            for (index, value) in values.iter().enumerate() {
                let start = value.get().as_ptr() as usize - raw.as_ptr() as usize;
                if let Some(next) = after.get(index) {
                    member_prefix(&raw[cursor..start], index, out);
                    patch(value.get(), &before[index], next, out)?;
                }
                cursor = start + value.get().len();
            }
            for (index, value) in after.iter().enumerate().skip(values.len()) {
                if index > 0 {
                    out.push(b',');
                }
                serde_json::to_writer(&mut *out, value)?;
            }
            out.extend_from_slice(&raw.as_bytes()[cursor..]);
        }
        _ => serde_json::to_writer(out, after)?,
    }
    Ok(())
}

fn member_prefix(prefix: &str, count: usize, out: &mut Vec<u8>) {
    // A member's prefix contains only whitespace, its delimiter and (for an
    // object) its original encoded key/colon. Never re-encode an untouched key.
    let comma = prefix.bytes().position(|b| !b.is_ascii_whitespace());
    let has_comma = comma.is_some_and(|i| prefix.as_bytes()[i] == b',');
    if count > 0 && !has_comma {
        out.push(b',');
    }
    if count == 0 && has_comma {
        let i = comma.expect("delimiter");
        out.extend_from_slice(&prefix.as_bytes()[..i]);
        out.extend_from_slice(&prefix.as_bytes()[i + 1..]);
    } else {
        out.extend_from_slice(prefix.as_bytes());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn patches_only_changed_leaves_and_keeps_raw_order_escapes_and_numbers() {
        let raw = br#" { "z": [{ "b":1.00, "a":"\u0061" }, 2], "a": false } "#;
        let mut replacement: Value = serde_json::from_slice(raw).unwrap();
        assert_eq!(encode(raw, &replacement).unwrap(), raw);
        replacement["a"] = json!(true);
        assert_eq!(
            encode(raw, &replacement).unwrap(),
            br#" { "z": [{ "b":1.00, "a":"\u0061" }, 2], "a": true } "#
        );
        replacement["z"]
            .as_array_mut()
            .unwrap()
            .push(json!({"next":3}));
        let encoded = encode(raw, &replacement).unwrap();
        assert!(std::str::from_utf8(&encoded)
            .unwrap()
            .contains(r#"[{ "b":1.00, "a":"\u0061" }, 2,{"next":3}]"#));
        assert_eq!(
            serde_json::from_slice::<Value>(&encoded).unwrap(),
            replacement
        );
    }

    #[test]
    fn object_deletions_additions_arrays_and_type_changes_remain_exact_semantically() {
        for raw in [
            r#"{"b":1,"a":2,"c":3}"#,
            "{}",
            "[]",
            "[1,2,3]",
            r#"{"nested":{"b":1,"a":2}}"#,
        ] {
            for after in [
                json!({}),
                json!([]),
                json!({"a":2,"new":[1,2]}),
                json!([1]),
                json!([1, 2, 3, 4]),
                json!({"c":3}),
                json!({"nested":{"a":2}}),
            ] {
                let encoded = encode(raw.as_bytes(), &after).unwrap();
                assert_eq!(serde_json::from_slice::<Value>(&encoded).unwrap(), after);
            }
        }
    }
}

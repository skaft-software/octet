/// TOML config deserialization and hot-reload support.
use std::collections::HashMap;

/// Load theme values from a TOML config file.
pub fn load_toml(path: &str, values: &mut HashMap<String, String>) {
    if let Ok(contents) = std::fs::read_to_string(path) {
        // A theme file is a TOML document: since toml 0.9, `str::parse` into
        // `toml::Value` accepts a single value only, so parse a table.
        if let Ok(table) = contents.parse::<toml::Table>() {
            flatten_toml(&toml::Value::Table(table), "", values);
        }
    }
}

/// Flatten a TOML value into dot-separated keys.
fn flatten_toml(value: &toml::Value, prefix: &str, values: &mut HashMap<String, String>) {
    match value {
        toml::Value::Table(table) => {
            for (key, val) in table {
                let new_prefix = if prefix.is_empty() {
                    key.clone()
                } else {
                    format!("{}.{}", prefix, key)
                };
                flatten_toml(val, &new_prefix, values);
            }
        }
        toml::Value::String(value) => insert_value(prefix, value.clone(), values),
        toml::Value::Integer(value) => insert_value(prefix, value.to_string(), values),
        toml::Value::Float(value) => insert_value(prefix, value.to_string(), values),
        toml::Value::Boolean(value) => insert_value(prefix, value.to_string(), values),
        _ => {}
    }
}

fn insert_value(prefix: &str, value: String, values: &mut HashMap<String, String>) {
    values.insert(prefix.to_owned(), value.clone());
    // Friendly sections map onto the stable legacy/semantic token names while
    // retaining the fully-qualified value for callers that resolve it.
    if let Some((section, key)) = prefix.split_once('.') {
        let alias = match section {
            "colors" | "tokens" => Some(key.to_owned()),
            "spacing" => Some(format!("spacing_{key}")),
            "icons" => Some(format!("icon_{key}")),
            _ => None,
        };
        if let Some(alias) = alias {
            values.insert(alias, value);
        }
    }
}

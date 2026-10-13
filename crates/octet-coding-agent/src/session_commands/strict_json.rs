//! Reject duplicate object fields before serde can discard conflicting parents/IDs.
use serde::de::{MapAccess, SeqAccess, Visitor};
use serde::{Deserialize, Deserializer};
use serde_json::Value;

struct Unique(Value);
impl<'de> Deserialize<'de> for Unique {
    fn deserialize<D: Deserializer<'de>>(de: D) -> Result<Self, D::Error> {
        struct JsonVisitor;
        impl<'de> Visitor<'de> for JsonVisitor {
            type Value = Unique;
            fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str("JSON without duplicate fields")
            }
            fn visit_bool<E: serde::de::Error>(self, v: bool) -> Result<Unique, E> {
                Ok(Unique(v.into()))
            }
            fn visit_i64<E: serde::de::Error>(self, v: i64) -> Result<Unique, E> {
                Ok(Unique(v.into()))
            }
            fn visit_u64<E: serde::de::Error>(self, v: u64) -> Result<Unique, E> {
                Ok(Unique(v.into()))
            }
            fn visit_f64<E: serde::de::Error>(self, v: f64) -> Result<Unique, E> {
                serde_json::Number::from_f64(v)
                    .map(|n| Unique(Value::Number(n)))
                    .ok_or_else(|| E::custom("invalid number"))
            }
            fn visit_str<E: serde::de::Error>(self, v: &str) -> Result<Unique, E> {
                Ok(Unique(v.into()))
            }
            fn visit_string<E: serde::de::Error>(self, v: String) -> Result<Unique, E> {
                Ok(Unique(v.into()))
            }
            fn visit_unit<E: serde::de::Error>(self) -> Result<Unique, E> {
                Ok(Unique(Value::Null))
            }
            fn visit_seq<A: SeqAccess<'de>>(self, mut a: A) -> Result<Unique, A::Error> {
                let mut values = Vec::new();
                while let Some(Unique(v)) = a.next_element()? {
                    values.push(v);
                }
                Ok(Unique(Value::Array(values)))
            }
            fn visit_map<A: MapAccess<'de>>(self, mut a: A) -> Result<Unique, A::Error> {
                let mut values = serde_json::Map::new();
                while let Some((k, v)) = a.next_entry::<String, Unique>()? {
                    if values.insert(k, v.0).is_some() {
                        return Err(serde::de::Error::custom("duplicate JSON object field"));
                    }
                }
                Ok(Unique(Value::Object(values)))
            }
        }
        de.deserialize_any(JsonVisitor)
    }
}

pub(super) fn parse(bytes: &[u8]) -> anyhow::Result<Value> {
    let mut de = serde_json::Deserializer::from_slice(bytes);
    let value = Unique::deserialize(&mut de)?;
    de.end()?;
    Ok(value.0)
}

pub(super) fn jsonl(bytes: &[u8]) -> anyhow::Result<Vec<Value>> {
    let text = std::str::from_utf8(bytes)?;
    let mut records = Vec::new();
    for (line, text) in text.lines().enumerate() {
        if text.trim().is_empty() {
            anyhow::bail!("blank JSONL record at line {}", line + 1);
        }
        records.push(parse(text.as_bytes()).map_err(|_| {
            anyhow::anyhow!("invalid or truncated JSONL record at line {}", line + 1)
        })?);
        if records.len() > 1_000_000 {
            anyhow::bail!("too many session records");
        }
    }
    if records.is_empty() {
        anyhow::bail!("empty session import");
    }
    Ok(records)
}

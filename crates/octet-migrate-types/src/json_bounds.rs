//! The bounded raw-JSON preflight: everything that must be true of a document
//! *before* any schema field is decoded.
//!
//! `serde_json::from_str` cannot enforce this crate's guarantees, because a
//! `Value` has already collapsed duplicate object names and because serde will
//! happily allocate a hundred-megabyte string. So every wire entry point first
//! walks the raw token stream with a counting visitor: total bytes, nesting
//! depth, collection sizes, aggregate string bytes, and — the reason this is a
//! separate pass at all — *duplicate object names*, which no post-hoc value
//! comparison can recover.
//!
//! It is kept apart from [`crate::wire`] because it is the only place in the
//! crate that reasons about JSON as text rather than as a schema. The version
//! probe in [`crate::version`] is a second such pass, deliberately minimal: it
//! runs *before* this one so a version mismatch wins over a hostile sibling
//! field. Two passes, one file each, is clearer than one pass with two modes.

use std::collections::{btree_map::Entry, BTreeMap};
use std::fmt;
use std::marker::PhantomData;

use serde::de::{
    self, DeserializeOwned, DeserializeSeed, Deserializer, Error as _, MapAccess, SeqAccess,
    Visitor,
};
use serde::Deserialize;

use crate::error::{Result, SchemaVersionMismatch, ValidationError};
use crate::limits::{
    MAX_JSON_INPUT_BYTES, MAX_JSON_NESTING, MAX_LIST_ENTRIES, MAX_MAP_ENTRIES,
    MAX_PORTABLE_JSON_INTEGER, MAX_STRING_BYTES, MAX_TOTAL_JSON_ENTRIES,
    MAX_TOTAL_JSON_STRING_BYTES,
};

pub(super) fn preflight_json(json: &str) -> Result<()> {
    if json.len() > MAX_JSON_INPUT_BYTES {
        return Err(ValidationError::new(format!(
            "JSON input exceeds MAX_JSON_INPUT_BYTES ({MAX_JSON_INPUT_BYTES} bytes)"
        )));
    }

    let mut deserializer = serde_json::Deserializer::from_str(json);
    let mut bounds = JsonBounds::default();
    BoundedJsonSeed {
        bounds: &mut bounds,
        depth: 0,
    }
    .deserialize(&mut deserializer)
    .map_err(ValidationError::from_serde)?;
    deserializer.end().map_err(ValidationError::from_serde)
}

#[derive(Default)]
pub(super) struct JsonBounds {
    string_bytes: usize,
    entries: usize,
}

impl JsonBounds {
    pub(super) fn enter_container(&self, depth: usize) -> Result<()> {
        if depth > MAX_JSON_NESTING {
            return Err(ValidationError::new(format!(
                "JSON nesting exceeds MAX_JSON_NESTING ({MAX_JSON_NESTING})"
            )));
        }
        Ok(())
    }

    pub(super) fn take_string(&mut self, value: &str) -> Result<()> {
        if value.len() > MAX_STRING_BYTES {
            return Err(ValidationError::new(format!(
                "JSON string exceeds MAX_STRING_BYTES ({MAX_STRING_BYTES} bytes)"
            )));
        }
        self.string_bytes = self
            .string_bytes
            .checked_add(value.len())
            .ok_or_else(|| ValidationError::new("JSON string-byte counter overflow"))?;
        if self.string_bytes > MAX_TOTAL_JSON_STRING_BYTES {
            return Err(ValidationError::new(format!(
                "decoded JSON strings exceed MAX_TOTAL_JSON_STRING_BYTES ({MAX_TOTAL_JSON_STRING_BYTES} bytes)"
            )));
        }
        Ok(())
    }

    pub(super) fn take_entry(
        &mut self,
        count: &mut usize,
        maximum: usize,
        kind: &str,
    ) -> Result<()> {
        if *count >= maximum {
            return Err(ValidationError::new(format!(
                "JSON {kind} exceeds its limit of {maximum} entries"
            )));
        }
        *count += 1;
        self.entries = self
            .entries
            .checked_add(1)
            .ok_or_else(|| ValidationError::new("JSON entry counter overflow"))?;
        if self.entries > MAX_TOTAL_JSON_ENTRIES {
            return Err(ValidationError::new(format!(
                "JSON entries exceed MAX_TOTAL_JSON_ENTRIES ({MAX_TOTAL_JSON_ENTRIES})"
            )));
        }
        Ok(())
    }
}

pub(super) struct BoundedJsonSeed<'a> {
    bounds: &'a mut JsonBounds,
    depth: usize,
}

impl<'de> DeserializeSeed<'de> for BoundedJsonSeed<'_> {
    type Value = ();

    fn deserialize<D>(self, deserializer: D) -> std::result::Result<Self::Value, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_any(BoundedJsonVisitor {
            bounds: self.bounds,
            depth: self.depth,
        })
    }
}

pub(super) struct BoundedJsonVisitor<'a> {
    bounds: &'a mut JsonBounds,
    depth: usize,
}

impl<'de> Visitor<'de> for BoundedJsonVisitor<'_> {
    type Value = ();

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("bounded JSON")
    }

    fn visit_bool<E>(self, _: bool) -> std::result::Result<Self::Value, E>
    where
        E: de::Error,
    {
        Ok(())
    }

    fn visit_i64<E>(self, _: i64) -> std::result::Result<Self::Value, E>
    where
        E: de::Error,
    {
        Ok(())
    }

    fn visit_u64<E>(self, _: u64) -> std::result::Result<Self::Value, E>
    where
        E: de::Error,
    {
        Ok(())
    }

    fn visit_f64<E>(self, _: f64) -> std::result::Result<Self::Value, E>
    where
        E: de::Error,
    {
        Ok(())
    }

    fn visit_none<E>(self) -> std::result::Result<Self::Value, E>
    where
        E: de::Error,
    {
        Ok(())
    }

    fn visit_unit<E>(self) -> std::result::Result<Self::Value, E>
    where
        E: de::Error,
    {
        Ok(())
    }

    fn visit_some<D>(self, deserializer: D) -> std::result::Result<Self::Value, D::Error>
    where
        D: Deserializer<'de>,
    {
        BoundedJsonSeed {
            bounds: self.bounds,
            depth: self.depth,
        }
        .deserialize(deserializer)
    }

    fn visit_borrowed_str<E>(self, value: &'de str) -> std::result::Result<Self::Value, E>
    where
        E: de::Error,
    {
        self.bounds.take_string(value).map_err(E::custom)
    }

    fn visit_str<E>(self, value: &str) -> std::result::Result<Self::Value, E>
    where
        E: de::Error,
    {
        self.bounds.take_string(value).map_err(E::custom)
    }

    fn visit_string<E>(self, value: String) -> std::result::Result<Self::Value, E>
    where
        E: de::Error,
    {
        self.bounds.take_string(&value).map_err(E::custom)
    }

    fn visit_seq<A>(self, mut sequence: A) -> std::result::Result<Self::Value, A::Error>
    where
        A: SeqAccess<'de>,
    {
        self.bounds
            .enter_container(self.depth + 1)
            .map_err(A::Error::custom)?;
        let bounds = self.bounds;
        let mut entries = 0;
        loop {
            let next = sequence.next_element_seed(BoundedSequenceElementSeed {
                bounds: &mut *bounds,
                entries: &mut entries,
                depth: self.depth + 1,
            })?;
            if next.is_none() {
                break;
            }
        }
        Ok(())
    }

    fn visit_map<A>(self, mut map: A) -> std::result::Result<Self::Value, A::Error>
    where
        A: MapAccess<'de>,
    {
        self.bounds
            .enter_container(self.depth + 1)
            .map_err(A::Error::custom)?;
        let bounds = self.bounds;
        let mut entries = 0;
        loop {
            let key = map.next_key_seed(BoundedMapKeySeed {
                bounds: &mut *bounds,
                entries: &mut entries,
            })?;
            if key.is_none() {
                break;
            }
            map.next_value_seed(BoundedJsonSeed {
                bounds: &mut *bounds,
                depth: self.depth + 1,
            })?;
        }
        Ok(())
    }
}

pub(super) struct BoundedMapKeySeed<'a> {
    bounds: &'a mut JsonBounds,
    entries: &'a mut usize,
}

impl<'de> DeserializeSeed<'de> for BoundedMapKeySeed<'_> {
    type Value = ();

    fn deserialize<D>(self, deserializer: D) -> std::result::Result<Self::Value, D::Error>
    where
        D: Deserializer<'de>,
    {
        self.bounds
            .take_entry(self.entries, MAX_MAP_ENTRIES, "object")
            .map_err(D::Error::custom)?;
        deserializer.deserialize_str(BoundedJsonStringVisitor {
            bounds: self.bounds,
        })
    }
}

pub(super) struct BoundedSequenceElementSeed<'a> {
    bounds: &'a mut JsonBounds,
    entries: &'a mut usize,
    depth: usize,
}

impl<'de> DeserializeSeed<'de> for BoundedSequenceElementSeed<'_> {
    type Value = ();

    fn deserialize<D>(self, deserializer: D) -> std::result::Result<Self::Value, D::Error>
    where
        D: Deserializer<'de>,
    {
        self.bounds
            .take_entry(self.entries, MAX_LIST_ENTRIES, "array")
            .map_err(D::Error::custom)?;
        BoundedJsonSeed {
            bounds: self.bounds,
            depth: self.depth,
        }
        .deserialize(deserializer)
    }
}

pub(super) struct BoundedJsonStringVisitor<'a> {
    bounds: &'a mut JsonBounds,
}

impl<'de> Visitor<'de> for BoundedJsonStringVisitor<'_> {
    type Value = ();

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a bounded JSON object key")
    }

    fn visit_borrowed_str<E>(self, value: &'de str) -> std::result::Result<Self::Value, E>
    where
        E: de::Error,
    {
        self.bounds.take_string(value).map_err(E::custom)
    }

    fn visit_str<E>(self, value: &str) -> std::result::Result<Self::Value, E>
    where
        E: de::Error,
    {
        self.bounds.take_string(value).map_err(E::custom)
    }

    fn visit_string<E>(self, value: String) -> std::result::Result<Self::Value, E>
    where
        E: de::Error,
    {
        self.bounds.take_string(&value).map_err(E::custom)
    }
}

pub(super) fn ensure_supported_version(
    found: Option<u64>,
    document: &'static str,
    expected: u32,
) -> Result<()> {
    if let Some(found) = found {
        if found != u64::from(expected) {
            return Err(ValidationError::new(
                SchemaVersionMismatch {
                    document,
                    found,
                    expected,
                }
                .to_string(),
            ));
        }
    }
    Ok(())
}

pub(super) fn decode_raw<T>(json: &str) -> Result<T>
where
    T: DeserializeOwned,
{
    serde_json::from_str(json).map_err(ValidationError::from_serde)
}

pub(super) struct RawVersion(pub(super) u64);

impl<'de> Deserialize<'de> for RawVersion {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserialize_portable_integer(deserializer, "schema_version").map(Self)
    }
}

pub(super) struct PortableIntegerVisitor {
    field: &'static str,
}

impl Visitor<'_> for PortableIntegerVisitor {
    type Value = u64;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "an exact portable JSON integer for {} no greater than {}",
            self.field, MAX_PORTABLE_JSON_INTEGER
        )
    }

    fn visit_u64<E>(self, value: u64) -> std::result::Result<Self::Value, E>
    where
        E: de::Error,
    {
        if value > MAX_PORTABLE_JSON_INTEGER {
            return Err(E::custom(format!(
                "{} must be an exact portable JSON integer no greater than {}",
                self.field, MAX_PORTABLE_JSON_INTEGER
            )));
        }
        Ok(value)
    }

    fn visit_u128<E>(self, value: u128) -> std::result::Result<Self::Value, E>
    where
        E: de::Error,
    {
        if value > u128::from(MAX_PORTABLE_JSON_INTEGER) {
            return Err(E::custom(format!(
                "{} must be an exact portable JSON integer no greater than {}",
                self.field, MAX_PORTABLE_JSON_INTEGER
            )));
        }
        Ok(value as u64)
    }

    fn visit_i64<E>(self, _: i64) -> std::result::Result<Self::Value, E>
    where
        E: de::Error,
    {
        Err(E::custom(format!(
            "{} must be a non-negative exact JSON integer",
            self.field
        )))
    }

    fn visit_i128<E>(self, _: i128) -> std::result::Result<Self::Value, E>
    where
        E: de::Error,
    {
        Err(E::custom(format!(
            "{} must be a non-negative exact JSON integer",
            self.field
        )))
    }

    fn visit_f64<E>(self, _: f64) -> std::result::Result<Self::Value, E>
    where
        E: de::Error,
    {
        Err(E::custom(format!(
            "{} must be a JSON integer without a fraction or exponent",
            self.field
        )))
    }
}

pub(super) fn deserialize_portable_integer<'de, D>(
    deserializer: D,
    field: &'static str,
) -> std::result::Result<u64, D::Error>
where
    D: Deserializer<'de>,
{
    deserializer.deserialize_any(PortableIntegerVisitor { field })
}

pub(super) fn deserialize_wall_clock<'de, D>(deserializer: D) -> std::result::Result<u64, D::Error>
where
    D: Deserializer<'de>,
{
    deserialize_portable_integer(deserializer, "wall_clock")
}

pub(super) fn deserialize_peak_rss_bytes<'de, D>(
    deserializer: D,
) -> std::result::Result<u64, D::Error>
where
    D: Deserializer<'de>,
{
    deserialize_portable_integer(deserializer, "peak_rss_bytes")
}

pub(super) fn deserialize_tokens_in<'de, D>(deserializer: D) -> std::result::Result<u64, D::Error>
where
    D: Deserializer<'de>,
{
    deserialize_portable_integer(deserializer, "tokens_in")
}

pub(super) fn deserialize_tokens_out<'de, D>(deserializer: D) -> std::result::Result<u64, D::Error>
where
    D: Deserializer<'de>,
{
    deserialize_portable_integer(deserializer, "tokens_out")
}

pub(super) struct BoundedString(pub(super) String);

impl BoundedString {
    pub(super) fn from_str(value: &str) -> Result<Self> {
        if value.len() > MAX_STRING_BYTES {
            return Err(ValidationError::new(format!(
                "JSON string exceeds MAX_STRING_BYTES ({MAX_STRING_BYTES} bytes)"
            )));
        }
        Ok(Self(value.to_owned()))
    }
}

impl<'de> Deserialize<'de> for BoundedString {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_string(BoundedStringVisitor)
    }
}

pub(super) struct BoundedStringVisitor;

impl<'de> Visitor<'de> for BoundedStringVisitor {
    type Value = BoundedString;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a bounded JSON string")
    }

    fn visit_borrowed_str<E>(self, value: &'de str) -> std::result::Result<Self::Value, E>
    where
        E: de::Error,
    {
        BoundedString::from_str(value).map_err(E::custom)
    }

    fn visit_str<E>(self, value: &str) -> std::result::Result<Self::Value, E>
    where
        E: de::Error,
    {
        BoundedString::from_str(value).map_err(E::custom)
    }

    fn visit_string<E>(self, value: String) -> std::result::Result<Self::Value, E>
    where
        E: de::Error,
    {
        if value.len() > MAX_STRING_BYTES {
            return Err(E::custom(format!(
                "JSON string exceeds MAX_STRING_BYTES ({MAX_STRING_BYTES} bytes)"
            )));
        }
        Ok(BoundedString(value))
    }
}

pub(super) fn deserialize_bounded_list<'de, D, T>(
    deserializer: D,
) -> std::result::Result<Vec<T>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    struct BoundedListVisitor<T>(PhantomData<T>);

    impl<'de, T> Visitor<'de> for BoundedListVisitor<T>
    where
        T: Deserialize<'de>,
    {
        type Value = Vec<T>;

        fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
            formatter.write_str("a bounded JSON array")
        }

        fn visit_seq<A>(self, mut sequence: A) -> std::result::Result<Self::Value, A::Error>
        where
            A: SeqAccess<'de>,
        {
            let mut values = Vec::new();
            while let Some(value) = sequence.next_element()? {
                if values.len() >= MAX_LIST_ENTRIES {
                    return Err(A::Error::custom(format!(
                        "JSON array exceeds its limit of {MAX_LIST_ENTRIES} entries"
                    )));
                }
                values.push(value);
            }
            Ok(values)
        }
    }

    deserializer.deserialize_seq(BoundedListVisitor(PhantomData))
}

pub(super) struct UniqueStringMap(pub(super) BTreeMap<String, String>);

impl<'de> Deserialize<'de> for UniqueStringMap {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct UniqueStringMapVisitor;

        impl<'de> Visitor<'de> for UniqueStringMapVisitor {
            type Value = UniqueStringMap;

            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("a bounded comparison metadata map with unique decoded keys")
            }

            fn visit_map<A>(self, mut entries: A) -> std::result::Result<Self::Value, A::Error>
            where
                A: MapAccess<'de>,
            {
                let mut values = BTreeMap::new();
                while let Some(BoundedString(key)) = entries.next_key()? {
                    // Detect a literal or escape-equivalent decoded duplicate before
                    // requesting its value, so an invalid duplicate value cannot
                    // mask the duplicate-key error or force a value allocation.
                    let at_capacity = values.len() >= MAX_MAP_ENTRIES;
                    match values.entry(key) {
                        Entry::Occupied(entry) => {
                            return Err(A::Error::custom(format!(
                                "duplicate comparison metadata key {:?}",
                                entry.key()
                            )));
                        }
                        Entry::Vacant(entry) => {
                            if at_capacity {
                                return Err(A::Error::custom(format!(
                                    "JSON object exceeds its limit of {MAX_MAP_ENTRIES} entries"
                                )));
                            }
                            let BoundedString(value) = entries.next_value()?;
                            entry.insert(value);
                        }
                    }
                }
                Ok(UniqueStringMap(values))
            }
        }

        deserializer.deserialize_map(UniqueStringMapVisitor)
    }
}

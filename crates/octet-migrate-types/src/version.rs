//! Minimal raw-token schema-version probes.
//!
//! A reader must reject an unsupported schema version *before* it can be
//! influenced by the rest of the document. A full decode cannot promise that: a
//! v2 document may add a field the v1 struct would refuse, or a field of a
//! different type, and the resulting error would blame the wrong thing. So each
//! document type gets its own probe here that walks the raw token stream,
//! ignores everything except its version field, and reports the number it found
//!.
//!
//! The three probes are near-identical on purpose and are *not* collapsed into
//! one generic probe: each names a different document, a different version key
//! and a different nesting position for that key, and the whole value of the
//! probe is that those three things are written out literally at the point of
//! use rather than parameterised.

use std::fmt;

use serde::de::{Deserializer, IgnoredAny, MapAccess, Visitor};
use serde::Deserialize;

use crate::error::Result;
use crate::json_bounds::{decode_raw, BoundedString, RawVersion};

pub(super) struct SetupVersionProbe {
    version: Option<u64>,
}

impl<'de> Deserialize<'de> for SetupVersionProbe {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct SetupVersionProbeVisitor;

        impl<'de> Visitor<'de> for SetupVersionProbeVisitor {
            type Value = SetupVersionProbe;

            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("a migrated setup JSON object")
            }

            fn visit_map<A>(self, mut map: A) -> std::result::Result<Self::Value, A::Error>
            where
                A: MapAccess<'de>,
            {
                let mut version = None;
                while let Some(BoundedString(key)) = map.next_key()? {
                    if key == "schema_version" && version.is_none() {
                        version = Some(map.next_value::<RawVersion>()?.0);
                    } else {
                        map.next_value::<IgnoredAny>()?;
                    }
                }
                Ok(SetupVersionProbe { version })
            }
        }

        deserializer.deserialize_map(SetupVersionProbeVisitor)
    }
}

pub(super) fn probe_setup_version(json: &str) -> Result<Option<u64>> {
    Ok(decode_raw::<SetupVersionProbe>(json)?.version)
}

pub(super) struct HeaderVersionProbe {
    version: Option<u64>,
}

impl<'de> Deserialize<'de> for HeaderVersionProbe {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct HeaderVersionProbeVisitor;

        impl<'de> Visitor<'de> for HeaderVersionProbeVisitor {
            type Value = HeaderVersionProbe;

            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("a comparison header JSON object")
            }

            fn visit_map<A>(self, mut map: A) -> std::result::Result<Self::Value, A::Error>
            where
                A: MapAccess<'de>,
            {
                let mut version = None;
                while let Some(BoundedString(key)) = map.next_key()? {
                    if key == "schema_version" && version.is_none() {
                        version = Some(map.next_value::<RawVersion>()?.0);
                    } else {
                        map.next_value::<IgnoredAny>()?;
                    }
                }
                Ok(HeaderVersionProbe { version })
            }
        }

        deserializer.deserialize_map(HeaderVersionProbeVisitor)
    }
}

pub(super) fn probe_header_version(json: &str) -> Result<Option<u64>> {
    Ok(decode_raw::<HeaderVersionProbe>(json)?.version)
}

pub(super) struct ReportVersionProbe {
    version: Option<u64>,
}

impl<'de> Deserialize<'de> for ReportVersionProbe {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct ReportVersionProbeVisitor;

        impl<'de> Visitor<'de> for ReportVersionProbeVisitor {
            type Value = ReportVersionProbe;

            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("a comparison report JSON object")
            }

            fn visit_map<A>(self, mut map: A) -> std::result::Result<Self::Value, A::Error>
            where
                A: MapAccess<'de>,
            {
                let mut version = None;
                while let Some(BoundedString(key)) = map.next_key()? {
                    if key == "header" {
                        let header = map.next_value::<HeaderVersionProbe>()?;
                        if version.is_none() {
                            version = header.version;
                        }
                    } else {
                        map.next_value::<IgnoredAny>()?;
                    }
                }
                Ok(ReportVersionProbe { version })
            }
        }

        deserializer.deserialize_map(ReportVersionProbeVisitor)
    }
}

pub(super) fn probe_report_version(json: &str) -> Result<Option<u64>> {
    Ok(decode_raw::<ReportVersionProbe>(json)?.version)
}

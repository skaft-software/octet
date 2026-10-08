//! Typed immutable blob descriptors and bounded local-file.v1 transfers.
//! Bytes and private locators never enter tool results or JSON-RPC payloads.
use crate::{resource, CallContext, Error, JsonSchema};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::fs::{File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Component, PathBuf};
use std::sync::{Arc, OnceLock};
use std::thread::ThreadId;

pub(crate) const FEATURE: &str = "bulk_objects_v1";
const PROFILE: &str = "local-file.v1";
const CHUNK: usize = 64 * 1024;

/// SHA-256 integrity metadata; the digest is not authority to read an object.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct BlobDigest {
    #[serde(deserialize_with = "algorithm")]
    #[schemars(schema_with = "algorithm_schema")]
    pub algorithm: String,
    #[serde(deserialize_with = "digest_value")]
    #[schemars(length(min = 64, max = 64))]
    pub value: String,
}
/// Closed immutable metadata. The host must still authorize the complete reference.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct BlobRef {
    #[serde(rename = "$blob", deserialize_with = "identity")]
    #[schemars(length(min = 1, max = 128))]
    pub id: String,
    #[serde(deserialize_with = "portable_bytes")]
    pub bytes: u64,
    pub digest: BlobDigest,
    #[serde(deserialize_with = "media_type")]
    #[schemars(length(min = 1, max = 255))]
    pub media_type: String,
}
fn valid_id(s: &str) -> bool {
    !s.is_empty() && s.len() <= 128 && s.bytes().all(|b| b.is_ascii_graphic())
}
fn checked_string<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
    check: impl FnOnce(&str) -> bool,
) -> Result<String, D::Error> {
    let value = String::deserialize(deserializer)?;
    if !check(&value) {
        return Err(serde::de::Error::custom("invalid blob metadata"));
    }
    Ok(value)
}
fn identity<'de, D: serde::Deserializer<'de>>(d: D) -> Result<String, D::Error> {
    checked_string(d, valid_id)
}
fn algorithm<'de, D: serde::Deserializer<'de>>(d: D) -> Result<String, D::Error> {
    checked_string(d, |s| s == "sha256")
}
fn digest_value<'de, D: serde::Deserializer<'de>>(d: D) -> Result<String, D::Error> {
    checked_string(d, |s| {
        s.len() == 64
            && s.bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    })
}
fn media_type<'de, D: serde::Deserializer<'de>>(d: D) -> Result<String, D::Error> {
    checked_string(d, valid_media)
}
fn valid_media(value: &str) -> bool {
    let name = |s: &str| {
        !s.is_empty()
            && s.len() <= 127
            && s.as_bytes()[0].is_ascii_alphanumeric()
            && s.bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"!#$&^_.+-".contains(&b))
    };
    let (essence, parameters) = value.split_once(';').unwrap_or((value, ""));
    if value.len() > 255
        || value.ends_with(';')
        || value
            .bytes()
            .any(|b| !(b == b'\t' || (0x20..=0x7e).contains(&b)))
        || !essence
            .trim_end_matches([' ', '\t'])
            .split_once('/')
            .is_some_and(|(a, b)| name(a) && name(b))
    {
        return false;
    }
    let token_len = |s: &[u8]| {
        s.iter()
            .take_while(|b| b.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(b))
            .count()
    };
    let mut rest = parameters.as_bytes();
    while !rest.is_empty() {
        rest = rest.trim_ascii_start();
        let length = token_len(rest);
        if length == 0 || rest.get(length) != Some(&b'=') {
            return false;
        }
        rest = &rest[length + 1..];
        if rest.first() == Some(&b'"') {
            rest = &rest[1..];
            loop {
                let Some((&byte, tail)) = rest.split_first() else {
                    return false;
                };
                rest = tail;
                if byte == b'"' {
                    break;
                }
                if byte == b'\\' {
                    let Some((_, tail)) = rest.split_first() else {
                        return false;
                    };
                    rest = tail;
                }
            }
        } else {
            let length = token_len(rest);
            if length == 0 {
                return false;
            }
            rest = &rest[length..];
        }
        rest = rest.trim_ascii_start();
        if !rest.is_empty() {
            if rest[0] != b';' || rest.len() == 1 {
                return false;
            }
            rest = &rest[1..];
        }
    }
    true
}
fn portable_bytes<'de, D: serde::Deserializer<'de>>(d: D) -> Result<u64, D::Error> {
    let value = u64::deserialize(d)?;
    if value > crate::values::MAX_INTEGER as u64 {
        return Err(serde::de::Error::custom("nonportable blob length"));
    }
    Ok(value)
}
fn algorithm_schema(_: &mut schemars::gen::SchemaGenerator) -> schemars::schema::Schema {
    serde_json::from_value(json!({"type":"string","enum":["sha256"]})).unwrap()
}
fn integrity() -> Error {
    Error::invalid("blob_integrity_mismatch")
}
fn io_error(_: io::Error) -> Error {
    Error::rpc(-32000, "bulk transfer I/O failed")
}
fn computed(hash: Sha256) -> BlobDigest {
    BlobDigest {
        algorithm: "sha256".into(),
        value: format!("{:x}", hash.finalize()),
    }
}

pub(crate) fn required(schema: &Value) -> bool {
    match schema {
        Value::Object(o) => {
            o.get("properties")
                .and_then(Value::as_object)
                .is_some_and(|p| p.contains_key("$blob"))
                || o.values().any(required)
        }
        Value::Array(a) => a.iter().any(required),
        _ => false,
    }
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Limits {
    object_bytes: u64,
    owner_bytes: u64,
    write_tickets_per_generation: u64,
    read_leases_per_generation: u64,
    blobs_per_owner: u64,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Profile {
    profile: String,
    transfer_directory: PathBuf,
    limits: Limits,
}
impl Profile {
    pub fn parse(value: &Value) -> Result<Arc<Self>, Error> {
        let mut profile: Self = serde_json::from_value(value.clone())
            .map_err(|_| Error::invalid("invalid local-file.v1 offer"))?;
        let l = &profile.limits;
        if !cfg!(unix)
            || profile.profile != PROFILE
            || !profile.transfer_directory.is_absolute()
            || [
                l.object_bytes,
                l.owner_bytes,
                l.write_tickets_per_generation,
                l.read_leases_per_generation,
                l.blobs_per_owner,
            ]
            .into_iter()
            .any(|n| n == 0 || n > crate::values::MAX_INTEGER as u64)
        {
            return Err(Error::rpc(-32000, "unsupported bulk profile or limits"));
        }
        profile.transfer_directory = profile
            .transfer_directory
            .canonicalize()
            .map_err(io_error)?;
        if !profile.transfer_directory.is_dir() {
            return Err(Error::invalid("invalid transfer directory"));
        }
        Ok(Arc::new(profile))
    }
    fn open(&self, locator: &str, write: bool) -> Result<File, Error> {
        let path = std::path::Path::new(locator);
        if locator.len() > 255
            || !locator
                .bytes()
                .all(|b| b.is_ascii_graphic() && !b"\\:".contains(&b))
            || path.components().count() != 1
            || !matches!(path.components().next(), Some(Component::Normal(_)))
        {
            return Err(Error::invalid("invalid transfer locator"));
        }
        let mut options = OpenOptions::new();
        options.read(!write).write(write);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK);
        }
        let file = options
            .open(self.transfer_directory.join(path))
            .map_err(io_error)?;
        let metadata = file.metadata().map_err(io_error)?;
        if !metadata.is_file() {
            return Err(Error::invalid("transfer must be a regular file"));
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            if metadata.nlink() != 1 {
                return Err(Error::invalid("transfer must not be hardlinked"));
            }
        }
        Ok(file)
    }
}

pub(crate) struct Call {
    runtime: Arc<resource::Runtime>,
    profile: Arc<Profile>,
    parent: u64,
    lane: OnceLock<ThreadId>,
}
impl Call {
    pub fn new(
        runtime: Arc<resource::Runtime>,
        profile: Arc<Profile>,
        id: &Value,
    ) -> Result<Arc<Self>, Error> {
        Ok(Arc::new(Self {
            runtime,
            profile,
            parent: id
                .as_u64()
                .ok_or_else(|| Error::invalid("bulk services require a host request id"))?,
            lane: OnceLock::new(),
        }))
    }
    pub fn enter(&self) {
        let _ = self.lane.set(std::thread::current().id());
    }
    fn request(&self, context: &CallContext, method: &str, params: Value) -> Result<Value, Error> {
        self.runtime
            .reverse
            .request(context, self.parent, method, params)
    }
    fn release(&self, context: &CallContext, id: &str) -> Result<(), Error> {
        let result = self.request(context, "bulk/release", json!({"id":id}))?;
        if result != json!({"released":true}) {
            return Err(Error::internal());
        }
        Ok(())
    }
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Ticket {
    ticket: String,
    profile: String,
    locator: String,
    capacity: u64,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Lease {
    lease: String,
    profile: String,
    locator: String,
    bytes: u64,
}

struct Transfer<'a> {
    file: File,
    call: &'a CallContext,
    left: u64,
    bytes: u64,
    hash: Sha256,
}
impl Transfer<'_> {
    fn active(&self) -> io::Result<()> {
        if self.call.is_cancelled() {
            Err(io::Error::new(io::ErrorKind::Other, "request cancelled"))
        } else {
            Ok(())
        }
    }
}
impl Write for Transfer<'_> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.active()?;
        if bytes.len() as u64 > self.left {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "blob capacity exceeded",
            ));
        }
        let bytes = &bytes[..bytes.len().min(CHUNK)];
        let n = self.file.write(bytes)?;
        self.hash.update(&bytes[..n]);
        self.left -= n as u64;
        self.bytes += n as u64;
        Ok(n)
    }
    fn flush(&mut self) -> io::Result<()> {
        self.active()?;
        self.file.flush()
    }
}
impl Read for Transfer<'_> {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        self.active()?;
        let len = bytes
            .len()
            .min(CHUNK)
            .min(self.left.min(usize::MAX as u64) as usize);
        let n = self.file.read(&mut bytes[..len])?;
        self.hash.update(&bytes[..n]);
        self.left -= n as u64;
        self.bytes += n as u64;
        Ok(n)
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn closed_blob_schema_and_codec() {
        let schema = crate::schema::typed_generated::<BlobRef>(false).unwrap();
        assert_eq!(schema["additionalProperties"], false);
        assert_eq!(
            schema["properties"]["digest"]["properties"]["algorithm"]["enum"],
            json!(["sha256"])
        );
        assert_eq!(
            schema["properties"]["bytes"]["maximum"],
            crate::values::MAX_INTEGER
        );
        assert!(required(&schema));
        for media in [
            "application/octet-stream",
            "text/plain; charset=utf-8",
            "application/vnd.demo+bin; name=\"a b\"",
        ] {
            assert!(valid_media(media));
        }
        for media in [
            "*/data",
            "text/plain; charset",
            "text/plain; charset=",
            "text/plain; x=\"unterminated",
            "text/plain;",
            " text/plain",
        ] {
            assert!(!valid_media(media));
        }
        let value = json!({"$blob":"opaque","bytes":0,"digest":{"algorithm":"sha256","value":"e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"},"media_type":"application/octet-stream"});
        assert_eq!(
            serde_json::to_value(serde_json::from_value::<BlobRef>(value.clone()).unwrap())
                .unwrap(),
            value
        );
        for (pointer, invalid) in [
            ("/$blob", json!("")),
            ("/bytes", json!(-1)),
            ("/bytes", json!(9_007_199_254_740_992u64)),
            ("/digest/algorithm", json!("sha512")),
            ("/digest/value", json!("Z".repeat(64))),
            ("/media_type", json!("bad\n/type")),
        ] {
            let mut bad = value.clone();
            *bad.pointer_mut(pointer).unwrap() = invalid;
            assert!(serde_json::from_value::<BlobRef>(bad).is_err());
        }
        let mut bad = value;
        bad["locator"] = "private".into();
        assert!(serde_json::from_value::<BlobRef>(bad).is_err());
    }
}

impl CallContext {
    fn bulk(&self) -> Result<&Arc<Call>, Error> {
        self.check_cancelled()?;
        let call = self
            .bulk
            .as_ref()
            .ok_or_else(|| Error::rpc(-32601, "bulk_objects_v1 was not negotiated"))?;
        if call.lane.get() != Some(&std::thread::current().id())
            || self.terminal.lock().unwrap().settled
        {
            return Err(Error::invalid(
                "bulk helpers require the active handler execution lane",
            ));
        }
        Ok(call)
    }
    /// Write at most capacity bytes through a callback, then hash/commit a provisional BlobRef.
    /// Tickets and private paths stay inside the SDK. The host admits only successful outputs.
    pub fn write_blob(
        &self,
        capacity: u64,
        media_type: &str,
        writer: impl FnOnce(&mut dyn Write) -> io::Result<()>,
    ) -> Result<BlobRef, Error> {
        let call = self.bulk()?;
        if capacity > call.profile.limits.object_bytes || !valid_media(media_type) {
            return Err(Error::invalid("invalid bulk capacity or media type"));
        }
        let ticket: Ticket = serde_json::from_value(call.request(
            self,
            "bulk/write",
            json!({"profile":PROFILE,"capacity":capacity,"media_type":media_type}),
        )?)
        .map_err(|_| Error::internal())?;
        if !valid_id(&ticket.ticket) || ticket.profile != PROFILE || ticket.capacity != capacity {
            return Err(Error::internal());
        }
        let result = (|| {
            let file = call.profile.open(&ticket.locator, true)?;
            file.set_len(0).map_err(io_error)?;
            let mut transfer = Transfer {
                file,
                call: self,
                left: capacity,
                bytes: 0,
                hash: Sha256::new(),
            };
            writer(&mut transfer).map_err(io_error)?;
            transfer.flush().map_err(io_error)?;
            transfer.file.sync_all().map_err(io_error)?;
            let digest = computed(transfer.hash);
            let bytes = transfer.bytes;
            drop(transfer.file);
            let blob: BlobRef = serde_json::from_value(call.request(
                self,
                "bulk/commit",
                json!({"ticket":ticket.ticket,"bytes":bytes,"digest":digest}),
            )?)
            .map_err(|_| Error::internal())?;
            if blob.bytes != bytes || blob.digest != digest || blob.media_type != media_type {
                return Err(integrity());
            }
            Ok(blob)
        })();
        // Commit consumes its ticket. Failure/cancellation is retired by the host parent;
        // an active failed write also explicitly releases its still-owned scratch ticket.
        if result.is_err() && !self.is_cancelled() {
            let _ = call.release(self, &ticket.ticket);
        }
        self.check_cancelled()?;
        result
    }
    /// Read a host-authorized lease with bounded buffers; verify the full digest before returning.
    /// The callback must not rely on rollback if it performs effects before verification completes.
    pub fn read_blob<R>(
        &self,
        blob: &BlobRef,
        reader: impl FnOnce(&mut dyn Read) -> io::Result<R>,
    ) -> Result<R, Error> {
        let call = self.bulk()?;
        let wire = serde_json::to_value(blob).map_err(|_| Error::internal())?;
        serde_json::from_value::<BlobRef>(wire.clone()).map_err(|_| integrity())?;
        if blob.bytes > call.profile.limits.object_bytes {
            return Err(Error::invalid("blob exceeds negotiated limit"));
        }
        let lease: Lease = serde_json::from_value(call.request(
            self,
            "bulk/read",
            json!({"profile":PROFILE,"blob":wire}),
        )?)
        .map_err(|_| Error::internal())?;
        if !valid_id(&lease.lease) || lease.profile != PROFILE || lease.bytes != blob.bytes {
            return Err(integrity());
        }
        let result = (|| {
            let file = call.profile.open(&lease.locator, false)?;
            if file.metadata().map_err(io_error)?.len() != blob.bytes {
                return Err(integrity());
            }
            let mut transfer = Transfer {
                file,
                call: self,
                left: blob.bytes,
                bytes: 0,
                hash: Sha256::new(),
            };
            let result = reader(&mut transfer).map_err(io_error)?;
            let mut buffer = [0; CHUNK];
            while transfer.read(&mut buffer).map_err(io_error)? != 0 {}
            if transfer.left != 0
                || transfer.file.read(&mut buffer[..1]).map_err(io_error)? != 0
                || computed(transfer.hash) != blob.digest
            {
                return Err(integrity());
            }
            Ok(result)
        })();
        let release = if self.is_cancelled() {
            Err(Error::cancelled())
        } else {
            call.release(self, &lease.lease)
        };
        self.check_cancelled()?;
        match result {
            Ok(value) => {
                release?;
                Ok(value)
            }
            Err(error) => Err(error),
        }
    }
}

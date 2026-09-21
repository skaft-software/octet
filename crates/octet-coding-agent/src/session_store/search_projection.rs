//! Streaming search-only projection, without a JSONL line buffer or JSON DOM.
//!
//! A validating string filter sits *before* serde_json: it forwards at most 512
//! Unicode scalars per string and consumes/validates the rest without retaining
//! it. Thus even selected or escaped multi-MiB strings cannot grow serde's string
//! scratch buffer. JSON structure, numbers and nesting limits remain serde's.
//! Only the root entry ID opts out of clipping: exact legacy IDs are required
//! output, so their memory is intrinsic to the result rather than scratch for
//! discarded transcript data. No new record-size or ID limit is imposed.

use super::*;
use serde::de::DeserializeSeed;
use std::cell::Cell;
use std::io;

const INPUT_BUFFER_BYTES: usize = 64 * 1024;

#[cfg(test)]
thread_local! { static MAX_FORWARDED_STRING: Cell<usize> = const { Cell::new(0) }; }

macro_rules! discard_scalars {
    ($value:expr) => {
        fn visit_bool<E: serde::de::Error>(self, _: bool) -> Result<Self::Value, E> {
            Ok($value)
        }
        fn visit_i64<E: serde::de::Error>(self, _: i64) -> Result<Self::Value, E> {
            Ok($value)
        }
        fn visit_u64<E: serde::de::Error>(self, _: u64) -> Result<Self::Value, E> {
            Ok($value)
        }
        fn visit_f64<E: serde::de::Error>(self, _: f64) -> Result<Self::Value, E> {
            Ok($value)
        }
        fn visit_unit<E: serde::de::Error>(self) -> Result<Self::Value, E> {
            Ok($value)
        }
    };
}

// Unlike IgnoredAny, deserialize_any validates number range and enforces the
// same nesting limit as the previous Value reader, also in unselected fields.
struct Validated;
impl<'de> Deserialize<'de> for Validated {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct ValidateVisitor;
        impl<'de> Visitor<'de> for ValidateVisitor {
            type Value = Validated;
            fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str("JSON")
            }
            fn visit_str<E: serde::de::Error>(self, _: &str) -> Result<Self::Value, E> {
                Ok(Validated)
            }
            discard_scalars!(Validated);
            fn visit_seq<A: SeqAccess<'de>>(self, seq: A) -> Result<Self::Value, A::Error> {
                validate_seq(seq)?;
                Ok(Validated)
            }
            fn visit_map<A: MapAccess<'de>>(self, map: A) -> Result<Self::Value, A::Error> {
                validate_map(map)?;
                Ok(Validated)
            }
        }
        deserializer.deserialize_any(ValidateVisitor)
    }
}
fn validate_seq<'de, A: SeqAccess<'de>>(mut seq: A) -> Result<(), A::Error> {
    while seq.next_element::<Validated>()?.is_some() {}
    Ok(())
}
fn validate_map<'de, A: MapAccess<'de>>(mut map: A) -> Result<(), A::Error> {
    while map.next_key::<IgnoredAny>()?.is_some() {
        map.next_value::<Validated>()?;
    }
    Ok(())
}

#[derive(Default)]
struct Text<const N: usize> {
    value: Option<String>,
}
impl<'de, const N: usize> Deserialize<'de> for Text<N> {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct TextVisitor<const N: usize>;
        impl<'de, const N: usize> Visitor<'de> for TextVisitor<N> {
            type Value = Text<N>;
            fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str("JSON")
            }
            fn visit_str<E: serde::de::Error>(self, text: &str) -> Result<Self::Value, E> {
                Ok(Text {
                    value: Some(text.chars().take(N).collect()),
                })
            }
            discard_scalars!(Text::default());
            fn visit_seq<A: SeqAccess<'de>>(self, seq: A) -> Result<Self::Value, A::Error> {
                validate_seq(seq)?;
                Ok(Text::default())
            }
            fn visit_map<A: MapAccess<'de>>(self, map: A) -> Result<Self::Value, A::Error> {
                validate_map(map)?;
                Ok(Text::default())
            }
        }
        deserializer.deserialize_any(TextVisitor::<N>)
    }
}

#[derive(Deserialize)]
#[serde(field_identifier)]
enum Field {
    #[serde(rename = "type")]
    Kind,
    #[serde(rename = "id")]
    Id,
    #[serde(rename = "value")]
    Value,
    User,
    Assistant,
    #[serde(rename = "content")]
    Content,
    Text,
    #[serde(other)]
    Other,
}

trait FromMap: Sized {
    fn from_map<'de, A: MapAccess<'de>>(map: A) -> Result<Self, A::Error>;
}
struct Object<T> {
    present: bool,
    value: Option<T>,
}
impl<T> Default for Object<T> {
    fn default() -> Self {
        Self {
            present: false,
            value: None,
        }
    }
}
impl<T> Object<T> {
    fn non_object() -> Self {
        Self {
            present: true,
            value: None,
        }
    }
}
impl<'de, T: FromMap> Deserialize<'de> for Object<T> {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct ObjectVisitor<T>(std::marker::PhantomData<T>);
        impl<'de, T: FromMap> Visitor<'de> for ObjectVisitor<T> {
            type Value = Object<T>;
            fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str("JSON")
            }
            fn visit_map<A: MapAccess<'de>>(self, map: A) -> Result<Self::Value, A::Error> {
                Ok(Object {
                    present: true,
                    value: Some(T::from_map(map)?),
                })
            }
            fn visit_seq<A: SeqAccess<'de>>(self, seq: A) -> Result<Self::Value, A::Error> {
                validate_seq(seq)?;
                Ok(Object::non_object())
            }
            fn visit_str<E: serde::de::Error>(self, _: &str) -> Result<Self::Value, E> {
                Ok(Object::non_object())
            }
            discard_scalars!(Object::non_object());
        }
        deserializer.deserialize_any(ObjectVisitor::<T>(std::marker::PhantomData))
    }
}

#[derive(Default)]
struct Part {
    text: Text<MAX_INDEXED_ENTRY_CHARS>,
}
impl FromMap for Part {
    fn from_map<'de, A: MapAccess<'de>>(mut map: A) -> Result<Self, A::Error> {
        let mut part = Self::default();
        while let Some(field) = map.next_key::<Field>()? {
            if matches!(field, Field::Text) {
                part.text = map.next_value()?;
            } else {
                map.next_value::<Validated>()?;
            }
        }
        Ok(part)
    }
}

#[derive(Default)]
struct Content(String);
impl<'de> Deserialize<'de> for Content {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct ContentVisitor;
        impl<'de> Visitor<'de> for ContentVisitor {
            type Value = Content;
            fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str("JSON")
            }
            fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Self::Value, A::Error> {
                let mut text = String::new();
                let mut chars = 0;
                while chars < MAX_INDEXED_ENTRY_CHARS {
                    let Some(part) = seq.next_element::<Object<Part>>()? else {
                        return Ok(Content(text));
                    };
                    let Some(part_text) = part.value.and_then(|part| part.text.value) else {
                        continue;
                    };
                    if !text.is_empty() {
                        text.push('\n');
                        chars += 1;
                    }
                    for ch in part_text.chars().take(MAX_INDEXED_ENTRY_CHARS - chars) {
                        text.push(ch);
                        chars += 1;
                    }
                }
                validate_seq(seq)?;
                Ok(Content(text))
            }
            fn visit_map<A: MapAccess<'de>>(self, map: A) -> Result<Self::Value, A::Error> {
                validate_map(map)?;
                Ok(Content::default())
            }
            fn visit_str<E: serde::de::Error>(self, _: &str) -> Result<Self::Value, E> {
                Ok(Content::default())
            }
            discard_scalars!(Content::default());
        }
        deserializer.deserialize_any(ContentVisitor)
    }
}

#[derive(Default)]
struct ProjectedMessage {
    content: Content,
}
impl FromMap for ProjectedMessage {
    fn from_map<'de, A: MapAccess<'de>>(mut map: A) -> Result<Self, A::Error> {
        let mut message = Self::default();
        while let Some(field) = map.next_key::<Field>()? {
            if matches!(field, Field::Content) {
                message.content = map.next_value()?;
            } else {
                map.next_value::<Validated>()?;
            }
        }
        Ok(message)
    }
}
#[derive(Default)]
struct ProjectedValue {
    kind: Text<16>,
    user: Object<ProjectedMessage>,
    assistant: Object<ProjectedMessage>,
}
impl FromMap for ProjectedValue {
    fn from_map<'de, A: MapAccess<'de>>(mut map: A) -> Result<Self, A::Error> {
        let mut value = Self::default();
        while let Some(field) = map.next_key::<Field>()? {
            match field {
                Field::Kind => value.kind = map.next_value()?,
                Field::User => value.user = map.next_value()?,
                Field::Assistant => value.assistant = map.next_value()?,
                _ => {
                    map.next_value::<Validated>()?;
                }
            }
        }
        Ok(value)
    }
}

#[derive(Default)]
struct ProjectedRecord {
    kind: Text<16>,
    id: Option<String>,
    value: Object<ProjectedValue>,
}
struct RecordSeed<'a>(&'a Cell<bool>);
impl<'de> DeserializeSeed<'de> for RecordSeed<'_> {
    type Value = Option<ProjectedRecord>;
    fn deserialize<D: Deserializer<'de>>(self, deserializer: D) -> Result<Self::Value, D::Error> {
        struct RecordVisitor<'a>(&'a Cell<bool>);
        impl<'de> Visitor<'de> for RecordVisitor<'_> {
            type Value = Option<ProjectedRecord>;
            fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str("JSON")
            }
            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
                let mut record = ProjectedRecord::default();
                while let Some(field) = map.next_key::<Field>()? {
                    match field {
                        Field::Kind => record.kind = map.next_value()?,
                        Field::Id => {
                            // Last-key-wins: the previous exact ID is no longer
                            // needed while a duplicate ID is being decoded.
                            record.id = None;
                            // Set before next_value_seed, not after a value's
                            // opening quote may already have been peeked.
                            self.0.set(true);
                            let id = map.next_value_seed(IdSeed(self.0));
                            self.0.set(false);
                            record.id = id?;
                        }
                        Field::Value => record.value = map.next_value()?,
                        _ => {
                            map.next_value::<Validated>()?;
                        }
                    }
                }
                Ok(Some(record))
            }
            fn visit_seq<A: SeqAccess<'de>>(self, seq: A) -> Result<Self::Value, A::Error> {
                validate_seq(seq)?;
                Ok(None)
            }
            fn visit_str<E: serde::de::Error>(self, _: &str) -> Result<Self::Value, E> {
                Ok(None)
            }
            discard_scalars!(None);
        }
        deserializer.deserialize_any(RecordVisitor(self.0))
    }
}
struct IdSeed<'a>(&'a Cell<bool>);
impl<'de> DeserializeSeed<'de> for IdSeed<'_> {
    type Value = Option<String>;
    fn deserialize<D: Deserializer<'de>>(self, deserializer: D) -> Result<Self::Value, D::Error> {
        struct IdVisitor<'a>(&'a Cell<bool>);
        impl<'de> Visitor<'de> for IdVisitor<'_> {
            type Value = Option<String>;
            fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str("JSON")
            }
            fn visit_str<E: serde::de::Error>(self, text: &str) -> Result<Self::Value, E> {
                Ok(Some(text.to_owned()))
            }
            discard_scalars!(None);
            fn visit_seq<A: SeqAccess<'de>>(self, seq: A) -> Result<Self::Value, A::Error> {
                // An invalid, non-string ID is not permission to retain large
                // strings nested in that object/array.
                self.0.set(false);
                validate_seq(seq)?;
                Ok(None)
            }
            fn visit_map<A: MapAccess<'de>>(self, map: A) -> Result<Self::Value, A::Error> {
                self.0.set(false);
                validate_map(map)?;
                Ok(None)
            }
        }
        deserializer.deserialize_any(IdVisitor(self.0))
    }
}
impl ProjectedRecord {
    fn into_entry(self) -> Option<IndexedEntry> {
        let value = self.value.value?;
        if self.kind.value.as_deref() != Some("entry")
            || value.kind.value.as_deref() != Some("message")
        {
            return None;
        }
        let (kind, message) = if value.user.present {
            (IndexedEntryKind::User, value.user.value)
        } else {
            (IndexedEntryKind::Assistant, value.assistant.value)
        };
        let message = message?;
        if message.content.0.trim().is_empty() {
            return None;
        }
        Some(IndexedEntry {
            entry_id: self.id?,
            kind,
            text: message.content.0,
        })
    }
}

// Match read_until's handling of an interrupted underlying read without
// confusing a real I/O failure with a malformed JSON record.
fn inspect_buffer<R: BufRead, T>(
    reader: &mut R,
    inspect: impl FnOnce(&[u8]) -> T,
) -> io::Result<T> {
    loop {
        match reader.fill_buf() {
            Ok(bytes) => return Ok(inspect(bytes)),
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error),
        }
    }
}

/// Physical JSONL framing, independent of JSON parse success. Recovery drains
/// only this line, in fixed-size chunks, so malformed records cannot consume the
/// next record or force a large allocation. Actual I/O/total-byte failures are
/// retained separately from the filter's malformed-string errors.
struct LineSource<'a, R> {
    reader: &'a mut R,
    observed: &'a mut usize,
    ended: bool,
    newline: bool,
    source_error: Option<io::Error>,
}
impl<R: BufRead> LineSource<'_, R> {
    fn consume(&mut self, bytes: usize) -> io::Result<()> {
        self.reader.consume(bytes);
        *self.observed += bytes;
        if *self.observed > MAX_SESSION_FILE_BYTES {
            let message = "session exceeds its byte limit while being read";
            self.source_error = Some(io::Error::new(io::ErrorKind::InvalidData, message));
            return Err(io::Error::new(io::ErrorKind::InvalidData, message));
        }
        Ok(())
    }
    fn next(&mut self) -> io::Result<Option<u8>> {
        if self.ended {
            return Ok(None);
        }
        let byte = match inspect_buffer(self.reader, |bytes| bytes.first().copied()) {
            Ok(byte) => byte,
            Err(error) => {
                self.source_error = Some(io::Error::new(error.kind(), error.to_string()));
                return Err(error);
            }
        };
        let Some(byte) = byte else {
            self.ended = true;
            return Ok(None);
        };
        self.consume(1)?;
        if byte == b'\n' {
            self.ended = true;
            self.newline = true;
            Ok(None)
        } else {
            Ok(Some(byte))
        }
    }
    fn skip_ascii_string_run(&mut self) -> io::Result<usize> {
        let count = match inspect_buffer(self.reader, |bytes| {
            bytes
                .iter()
                .take_while(|&&byte| (0x20..=0x7f).contains(&byte) && byte != b'"' && byte != b'\\')
                .count()
        }) {
            Ok(count) => count,
            Err(error) => {
                self.source_error = Some(io::Error::new(error.kind(), error.to_string()));
                return Err(error);
            }
        }
        .min(MAX_SESSION_FILE_BYTES.saturating_sub(*self.observed) + 1);
        self.consume(count)?;
        Ok(count)
    }
    fn finish(&mut self) -> io::Result<bool> {
        while !self.ended {
            let (len, newline) = inspect_buffer(self.reader, |bytes| {
                (bytes.len(), bytes.iter().position(|&byte| byte == b'\n'))
            })?;
            if len == 0 {
                self.ended = true;
                break;
            }
            let count = newline
                .map_or(len, |at| at + 1)
                .min(MAX_SESSION_FILE_BYTES.saturating_sub(*self.observed) + 1);
            self.consume(count)?;
            if newline.is_some() {
                self.newline = true;
                self.ended = true;
            }
        }
        Ok(self.newline)
    }
}

/// The only JSON lexing performed here is one string scalar at a time. The raw
/// representation of a scalar occupies at most 12 bytes (a surrogate pair).
/// Every scalar is validated even after the projection is full. Raw JSON syntax
/// is otherwise forwarded unchanged for serde to validate.
struct StringProjection<'a, 'b, R> {
    source: &'a mut LineSource<'b, R>,
    exact_id: &'a Cell<bool>,
    in_string: bool,
    exact: bool,
    forwarded: usize,
    pending: [u8; 12],
    pending_len: usize,
    pending_at: usize,
}
impl<'a, 'b, R: BufRead> StringProjection<'a, 'b, R> {
    fn new(source: &'a mut LineSource<'b, R>, exact_id: &'a Cell<bool>) -> Self {
        Self {
            source,
            exact_id,
            in_string: false,
            exact: false,
            forwarded: 0,
            pending: [0; 12],
            pending_len: 0,
            pending_at: 0,
        }
    }
    fn invalid() -> io::Error {
        io::Error::new(io::ErrorKind::InvalidData, "invalid JSON string")
    }
    fn unit_byte(&mut self) -> io::Result<u8> {
        let byte = self.source.next()?.ok_or_else(Self::invalid)?;
        self.pending[self.pending_len] = byte;
        self.pending_len += 1;
        Ok(byte)
    }
    fn hex_quad(&mut self) -> io::Result<u16> {
        let mut value = 0;
        for _ in 0..4 {
            let digit = match self.unit_byte()? {
                byte @ b'0'..=b'9' => byte - b'0',
                byte @ b'a'..=b'f' => byte - b'a' + 10,
                byte @ b'A'..=b'F' => byte - b'A' + 10,
                _ => return Err(Self::invalid()),
            };
            value = value * 16 + u16::from(digit);
        }
        Ok(value)
    }
    fn string_unit(&mut self, first: u8) -> io::Result<()> {
        self.pending[0] = first;
        self.pending_len = 1;
        self.pending_at = 0;
        match first {
            b'\\' => match self.unit_byte()? {
                b'"' | b'\\' | b'/' | b'b' | b'f' | b'n' | b'r' | b't' => {}
                b'u' => match self.hex_quad()? {
                    0xdc00..=0xdfff => return Err(Self::invalid()),
                    0xd800..=0xdbff => {
                        if self.unit_byte()? != b'\\' || self.unit_byte()? != b'u' {
                            return Err(Self::invalid());
                        }
                        if !(0xdc00..=0xdfff).contains(&self.hex_quad()?) {
                            return Err(Self::invalid());
                        }
                    }
                    _ => {}
                },
                _ => return Err(Self::invalid()),
            },
            0x00..=0x1f => return Err(Self::invalid()),
            0x20..=0x7f => {}
            _ => {
                let len = match first {
                    0xc2..=0xdf => 2,
                    0xe0..=0xef => 3,
                    0xf0..=0xf4 => 4,
                    _ => return Err(Self::invalid()),
                };
                for _ in 1..len {
                    self.unit_byte()?;
                }
                std::str::from_utf8(&self.pending[..len]).map_err(|_| Self::invalid())?;
            }
        }
        Ok(())
    }
    fn next(&mut self) -> io::Result<Option<u8>> {
        if self.pending_at < self.pending_len {
            let byte = self.pending[self.pending_at];
            self.pending_at += 1;
            return Ok(Some(byte));
        }
        if !self.in_string {
            let byte = self.source.next()?;
            if byte == Some(b'"') {
                self.in_string = true;
                self.exact = self.exact_id.get();
                self.forwarded = 0;
            }
            return Ok(byte);
        }
        loop {
            if !self.exact
                && self.forwarded == MAX_INDEXED_ENTRY_CHARS
                && self.source.skip_ascii_string_run()? > 0
            {
                continue;
            }
            let byte = self.source.next()?.ok_or_else(Self::invalid)?;
            if byte == b'"' {
                self.in_string = false;
                return Ok(Some(byte));
            }
            self.string_unit(byte)?;
            if self.exact || self.forwarded < MAX_INDEXED_ENTRY_CHARS {
                self.forwarded += 1;
                #[cfg(test)]
                if !self.exact {
                    MAX_FORWARDED_STRING.with(|max| max.set(max.get().max(self.forwarded)));
                }
                self.pending_at = 1;
                return Ok(Some(self.pending[0]));
            }
            // Discard the validated scalar, not just the remainder of the file.
            self.pending_at = self.pending_len;
        }
    }
}
impl<R: BufRead> Read for StringProjection<'_, '_, R> {
    fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
        if output.is_empty() {
            return Ok(0);
        }
        // Never read ahead across a value boundary: the root ID visitor changes
        // capture mode synchronously between fields. IoRead currently requests
        // one byte; retaining this rule also makes that contract explicit.
        match self.next()? {
            Some(byte) => {
                output[0] = byte;
                Ok(1)
            }
            None => Ok(0),
        }
    }
}

pub(super) fn index(path: &Path) -> anyhow::Result<Vec<IndexedEntry>> {
    let path = absolute_read_path(path)?;
    let file = octet_agent::secure_fs::open_regular_file_for_read(&path)?;
    let file_len = file.metadata()?.len();
    if file_len > MAX_SESSION_FILE_BYTES as u64 {
        anyhow::bail!("session is {file_len} bytes (limit {MAX_SESSION_FILE_BYTES})");
    }
    index_reader(BufReader::with_capacity(INPUT_BUFFER_BYTES, file))
}
fn index_reader<R: BufRead>(mut reader: R) -> anyhow::Result<Vec<IndexedEntry>> {
    let mut observed = 0;
    let mut records = 0;
    let mut entries = Vec::new();
    while inspect_buffer(&mut reader, |bytes| !bytes.is_empty())? {
        records += 1;
        if records > MAX_SESSION_RECORDS {
            anyhow::bail!("session has more than {MAX_SESSION_RECORDS} records");
        }
        let exact_id = Cell::new(false);
        let mut source = LineSource {
            reader: &mut reader,
            observed: &mut observed,
            ended: false,
            newline: false,
            source_error: None,
        };
        let parsed = {
            let filter = StringProjection::new(&mut source, &exact_id);
            let mut deserializer = serde_json::Deserializer::from_reader(filter);
            RecordSeed(&exact_id)
                .deserialize(&mut deserializer)
                .and_then(|record| deserializer.end().map(|()| record))
        };
        if let Some(error) = source.source_error.take() {
            return Err(error.into());
        }
        let newline = source.finish()?;
        match parsed {
            Ok(Some(record)) => {
                if let Some(entry) = record.into_entry() {
                    entries.push(entry);
                    if entries.len() == MAX_INDEXED_ENTRIES_PER_SESSION {
                        break;
                    }
                }
            }
            Err(_) if !newline => break,
            _ => {}
        }
    }
    Ok(entries)
}

#[cfg(test)]
pub(super) fn from_value(value: &serde_json::Value) -> Option<IndexedEntry> {
    index_reader(serde_json::to_vec(value).unwrap().as_slice())
        .unwrap()
        .into_iter()
        .next()
}

#[cfg(test)]
mod tests {
    use super::*;

    // Original Value-based lens, kept solely as the differential oracle.
    fn old_lens(record: &serde_json::Value) -> Option<IndexedEntry> {
        if record.get("type")?.as_str()? != "entry" {
            return None;
        }
        let entry_id = record.get("id")?.as_str()?.to_owned();
        let value = record.get("value")?;
        if value.get("type")?.as_str()? != "message" {
            return None;
        }
        let (role, kind) = if value.get("User").is_some() {
            ("User", IndexedEntryKind::User)
        } else if value.get("Assistant").is_some() {
            ("Assistant", IndexedEntryKind::Assistant)
        } else {
            return None;
        };
        let parts = value.get(role)?.get("content")?.as_array()?;
        let mut text = String::new();
        for part in parts {
            let Some(part_text) = part.get("Text").and_then(|value| value.as_str()) else {
                continue;
            };
            if !text.is_empty() {
                text.push('\n');
            }
            text.extend(part_text.chars().take(MAX_INDEXED_ENTRY_CHARS));
            if text.chars().count() >= MAX_INDEXED_ENTRY_CHARS {
                break;
            }
        }
        let text: String = text.chars().take(MAX_INDEXED_ENTRY_CHARS).collect();
        if text.trim().is_empty() {
            return None;
        }
        Some(IndexedEntry {
            entry_id,
            kind,
            text,
        })
    }
    fn old_index(bytes: &[u8]) -> Vec<IndexedEntry> {
        bytes
            .split(|&byte| byte == b'\n')
            .filter_map(|line| {
                let record =
                    serde_json::from_str::<serde_json::Value>(std::str::from_utf8(line).ok()?)
                        .ok()?;
                old_lens(&record)
            })
            .take(MAX_INDEXED_ENTRIES_PER_SESSION)
            .collect()
    }
    fn assert_parity(bytes: &[u8], capacities: &[usize]) -> Vec<IndexedEntry> {
        let expected = old_index(bytes);
        for &capacity in capacities {
            let actual = index_reader(BufReader::with_capacity(capacity, bytes)).unwrap();
            assert_eq!(actual, expected, "buffer capacity {capacity}");
        }
        expected
    }
    fn visible(text: &str) -> serde_json::Value {
        serde_json::json!({"type":"entry","id":"a","value":{"type":"message","User":{"content":[{"Text":text}]}}})
    }

    #[test]
    fn multi_mebibyte_private_media_and_selected_strings_stay_bounded() {
        let mut record = visible(&"🦀e\u{301}".repeat(300_000));
        record["metadata"] = serde_json::json!({"private": "secret".repeat(400_000)});
        record["value"]["User"]["content"]
            .as_array_mut()
            .unwrap()
            .insert(
                0,
                serde_json::json!({"Media":{"data":"A".repeat(3 * 1024 * 1024)}}),
            );
        let bytes = serde_json::to_vec(&record).unwrap();
        assert!(bytes.len() > 6 * 1024 * 1024);
        MAX_FORWARDED_STRING.with(|max| max.set(0));
        let expected = assert_parity(&bytes, &[4093, INPUT_BUFFER_BYTES]);
        assert_eq!(expected.len(), 1);
        assert_eq!(expected[0].text.chars().count(), MAX_INDEXED_ENTRY_CHARS);
        assert_eq!(
            MAX_FORWARDED_STRING.with(|max| max.get()),
            MAX_INDEXED_ENTRY_CHARS
        );
    }

    #[test]
    fn huge_escaped_strings_and_exact_legacy_ids_keep_old_results() {
        let id = "legacy🦀".repeat(20_000);
        let raw = format!(
            r#"{{"type":"entry","id":{},"private":"{}","value":{{"type":"message","Assistant":{{"content":[{{"Text":"{}"}}]}}}}}}"#,
            serde_json::to_string(&id).unwrap(),
            r"\uD83E\uDD80\n".repeat(160_000),
            r#"\uD83E\uDD80\u0061\n\\\""#.repeat(100_000)
        );
        MAX_FORWARDED_STRING.with(|max| max.set(0));
        let entries = assert_parity(raw.as_bytes(), &[4093, INPUT_BUFFER_BYTES]);
        assert_eq!(entries[0].entry_id, id);
        assert_eq!(entries[0].text.chars().count(), MAX_INDEXED_ENTRY_CHARS);
        assert_eq!(
            MAX_FORWARDED_STRING.with(|max| max.get()),
            MAX_INDEXED_ENTRY_CHARS
        );
    }

    #[test]
    fn malformed_oversized_records_recover_at_the_physical_line_boundary() {
        let good = serde_json::to_vec(&visible("needle")).unwrap();
        let long = "x".repeat(2 * 1024 * 1024);
        let mut bytes = Vec::new();
        for ending in [
            b"\\q\"}".as_slice(),
            b"\\uD800\"}",
            b"\xff\"}",
            b"\"} trailing",
            b"unfinished",
        ] {
            bytes.extend_from_slice(b"{\"private\":\"");
            bytes.extend_from_slice(long.as_bytes());
            bytes.extend_from_slice(ending);
            bytes.push(b'\n');
            bytes.extend_from_slice(&good);
            bytes.push(b'\n');
        }
        bytes.extend_from_slice(b"{\"private\":\"");
        bytes.extend_from_slice(long.as_bytes()); // torn final line
        assert_eq!(assert_parity(&bytes, &[4093]).len(), 5);
    }

    #[test]
    fn split_unicode_escapes_duplicate_keys_and_lenient_shapes_match_old_lens() {
        let records = [
            r#"{"type":"entry","id":"a","value":{"type":"message","User":{"content":[null,42,{"Text":false},{"Text":"discard","Text":"visible"},{"Text":""},{"Text":"tail"}]}}}"#,
            r#"{"type":"entry","id":"discard","id":"\uD83E\uDD80","value":{"type":"message","User":{"content":[{"Text":"é🦀\uD83E\uDD80\u0000\n\"\\\/\b\f\r\t"}]}}}"#,
            r#"{"type":"entry","id":"a","value":{"type":"message","User":null,"Assistant":{"content":[{"Text":"not visible"}]}}}"#,
            r#"{"type":"entry","id":{"private":"ignored"},"value":{"type":"message","User":{"content":[{"Text":"ignored"}]}}}"#,
            r#"{"type":"entry","id":"a","value":{"type":"message","User":{"content":[{"Text":"old"}]}},"value":{"type":"message","Assistant":{"content":[{"Text":"new"}]}}}"#,
            r#"{"type":"entry","id":"a","private":1e400,"value":{"type":"message","User":{"content":[{"Text":"invalid number"}]}}}"#,
            r#"{"type":"entry","id":"a","private":01,"value":{"type":"message","User":{"content":[{"Text":"invalid number"}]}}}"#,
        ];
        for record in records {
            assert_parity(record.as_bytes(), &[1, 2, 3, 7, INPUT_BUFFER_BYTES]);
        }
        let text = "🦀".repeat(510);
        let mut record = visible(&text);
        record["value"]["User"]["content"]
            .as_array_mut()
            .unwrap()
            .push(serde_json::json!({"Text":"XYZ"}));
        let bytes = serde_json::to_vec(&record).unwrap();
        assert_eq!(assert_parity(&bytes, &[1, 7])[0].text, format!("{text}\nX"));
    }

    #[test]
    fn invalid_string_suffixes_are_checked_after_the_projection_is_full() {
        let prefix = format!(
            r#"{{"type":"entry","id":"a","value":{{"type":"message","User":{{"content":[{{"Text":"{}"#,
            "x".repeat(MAX_INDEXED_ENTRY_CHARS + 10)
        );
        for invalid in [
            b"\\uDC00".as_slice(),
            b"\\uD800\\u0041",
            b"\\u000g",
            b"\\q",
            b"\xc0\xaf",
            b"\xed\xa0\x80",
            b"\xf4\x90\x80\x80",
            b"\x00",
        ] {
            let mut record = prefix.as_bytes().to_vec();
            record.extend_from_slice(invalid);
            record.extend_from_slice(b"\"}]}}}");
            assert!(assert_parity(&record, &[1, 7]).is_empty());
        }
    }

    #[test]
    fn every_string_scalar_boundary_keeps_complete_utf8_and_escape_units() {
        for encoded in [
            r"\u0000",
            r"\u007f",
            r"\u0080",
            r"\u07ff",
            r"\u0800",
            r"\uD7FF",
            r"\uE000",
            r"\uFFFF",
            r"\uD800\uDC00",
            r"\uDBFF\uDFFF",
            r"\uD83E\uDD80",
            r#"\""#,
            r"\\",
            r"\/",
            r"\n",
        ] {
            let record = format!(
                r#"{{"value":{{"User":{{"content":[{{"Text":"{}{encoded}tail"}}]}},"type":"message"}},"type":"entry","id":"{}"}}"#,
                "x".repeat(MAX_INDEXED_ENTRY_CHARS - 1),
                "exact-id".repeat(100)
            );
            let entries = assert_parity(record.as_bytes(), &[1, 2, 3, 5, 8]);
            assert_eq!(entries.len(), 1);
            assert_eq!(entries[0].text.chars().count(), MAX_INDEXED_ENTRY_CHARS);
            assert_eq!(entries[0].entry_id, "exact-id".repeat(100));
        }
    }

    #[test]
    fn non_string_id_does_not_enable_unbounded_nested_string_capture() {
        let mut record = visible("ignored");
        record["id"] = serde_json::json!({"private": "A".repeat(2 * 1024 * 1024)});
        let bytes = serde_json::to_vec(&record).unwrap();
        MAX_FORWARDED_STRING.with(|max| max.set(0));
        assert!(assert_parity(&bytes, &[4093]).is_empty());
        assert_eq!(
            MAX_FORWARDED_STRING.with(|max| max.get()),
            MAX_INDEXED_ENTRY_CHARS
        );
    }

    #[test]
    fn deterministic_projection_differential_covers_parts_and_unknown_fields() {
        let strings = ["", "plain", " ", "é🦀", "\n\\\"", "\u{0}", "e\u{301}"];
        let mut seed = 0x51e5_u32;
        for _ in 0..128 {
            let mut parts = Vec::new();
            for _ in 0..7 {
                seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                let text = strings[seed as usize % strings.len()].repeat((seed % 700) as usize);
                parts.push(match seed % 5 {
                    0 => serde_json::json!({"Text":text}),
                    1 => serde_json::json!({"Reasoning":{"text":text},"Text":"visible"}),
                    2 => serde_json::json!({"Text":null,"Media":{"data":text}}),
                    3 => serde_json::json!([text]),
                    _ => serde_json::Value::Bool(false),
                });
            }
            let record = serde_json::json!({"type":"entry","id":"e","value":{"type":"message","Assistant":{"content":parts}},
                "private":{"id":"not an exact ID","Text":"not indexed"}});
            assert_parity(&serde_json::to_vec(&record).unwrap(), &[1, 31]);
        }
    }

    #[test]
    fn source_failures_and_total_byte_limits_are_not_malformed_record_recovery() {
        struct FailsAfterPrefix {
            prefix: &'static [u8],
            interrupted: bool,
        }
        impl Read for FailsAfterPrefix {
            fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
                if !self.interrupted {
                    self.interrupted = true;
                    return Err(io::ErrorKind::Interrupted.into());
                }
                if self.prefix.is_empty() {
                    return Err(io::ErrorKind::BrokenPipe.into());
                }
                self.prefix.read(output)
            }
        }
        let input = FailsAfterPrefix {
            prefix: b"{\"private\":\"partial",
            interrupted: false,
        };
        let error = index_reader(BufReader::with_capacity(1, input)).unwrap_err();
        assert_eq!(
            error.downcast_ref::<io::Error>().unwrap().kind(),
            io::ErrorKind::BrokenPipe
        );
        for skip in [false, true] {
            let mut reader = b"abcdef\n".as_slice();
            let mut observed = MAX_SESSION_FILE_BYTES - 1;
            let mut source = LineSource {
                reader: &mut reader,
                observed: &mut observed,
                ended: false,
                newline: false,
                source_error: None,
            };
            if skip {
                assert!(source.skip_ascii_string_run().is_err());
            } else {
                assert!(source.finish().is_err());
            }
            assert!(source.source_error.is_some());
        }
    }

    #[test]
    fn deeply_nested_private_data_and_valid_unterminated_records_match_old_reader() {
        let good = serde_json::to_vec(&visible("last complete record, no newline")).unwrap();
        for depth in [120, 128, 200] {
            let mut bytes = format!(
                "{{\"private\":{}0{}}}\n",
                "[".repeat(depth),
                "]".repeat(depth)
            )
            .into_bytes();
            bytes.extend_from_slice(&good);
            assert_eq!(assert_parity(&bytes, &[1, 4093]).len(), 1);
        }
    }
}

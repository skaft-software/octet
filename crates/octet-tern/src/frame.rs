//! APC framing for the Tern Surface Protocol.
//!
//! Every message is an APC string: `ESC _ tsp ; <verb> [; k=v]* ; <body> ESC \`.
//! A body larger than the negotiated APC limit is split into UTF-8-safe chunks
//! sharing one chunk id, all but the last carrying `m=1`. Terminal → program
//! replies and events reassemble the same way.

use std::collections::BTreeMap;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};

use serde::Serialize;

use crate::wire::{Event, Reply, Verb, TSP_DEFAULT_APC_LIMIT, TSP_PREFIX, TSP_ST};

/// Monotonic chunk id source; ids only need to be unique within a stream.
static NEXT_CHUNK_ID: AtomicU64 = AtomicU64::new(1);

/// One message's parameters (`k=v`, ASCII, never containing `;`).
pub type Params = BTreeMap<String, String>;

/// UTF-8 byte length of the code point starting at byte index `i`, and how many
/// bytes it occupies.
fn code_point_bytes(text: &str, i: usize) -> usize {
    let b = text.as_bytes()[i];
    if b < 0x80 {
        1
    } else if b >> 5 == 0b110 {
        2
    } else if b >> 4 == 0b1110 {
        3
    } else {
        4
    }
}

/// Split `body` into pieces of at most `limit` UTF-8 bytes, never inside a code
/// point.
pub fn split_utf8(body: &str, limit: usize) -> Vec<&str> {
    let max = limit.max(4);
    let mut pieces = Vec::new();
    let mut start = 0usize;
    let mut bytes = 0usize;
    let mut i = 0usize;
    while i < body.len() {
        let step = code_point_bytes(body, i);
        if bytes + step > max {
            pieces.push(&body[start..i]);
            start = i;
            bytes = 0;
        }
        bytes += step;
        i += step;
    }
    pieces.push(&body[start..]);
    pieces
}

fn encode_params(params: &Params) -> String {
    let mut out = String::new();
    for (key, value) in params {
        out.push(';');
        out.push_str(key);
        out.push('=');
        out.push_str(value);
    }
    out
}

fn assemble(verb: Verb, params: &str, body: &str) -> String {
    let mut out = String::with_capacity(
        TSP_PREFIX.len() + verb.as_str().len() + params.len() + body.len() + 2,
    );
    out.push_str(TSP_PREFIX);
    out.push_str(verb.as_str());
    out.push_str(params);
    out.push(';');
    out.push_str(body);
    out.push_str(TSP_ST);
    out
}

/// Encode one logical message, chunking when `body` exceeds `limit` UTF-8 bytes.
pub fn encode(verb: Verb, body: &str, params: &Params, limit: usize) -> String {
    let limit = if limit == 0 {
        TSP_DEFAULT_APC_LIMIT
    } else {
        limit
    };
    let base = encode_params(params);
    if body.len() <= limit {
        return assemble(verb, &base, body);
    }
    let chunk_id = NEXT_CHUNK_ID.fetch_add(1, Ordering::Relaxed).to_string();
    let pieces = split_utf8(body, limit);
    let last = pieces.len().saturating_sub(1);
    let mut out = String::new();
    for (i, piece) in pieces.iter().enumerate() {
        let more = if i < last { ";m=1" } else { "" };
        let extra = format!("{base};c={chunk_id}{more}");
        out.push_str(&assemble(verb, &extra, piece));
    }
    out
}

/// Encode a JSON message body.
pub fn encode_json<T: Serialize>(
    verb: Verb,
    value: &T,
    limit: usize,
) -> Result<String, serde_json::Error> {
    Ok(encode(
        verb,
        &serde_json::to_string(value)?,
        &Params::new(),
        limit,
    ))
}

/// One decoded message: verb, parameters and raw body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Raw {
    /// The verb (one letter for known verbs).
    pub verb: String,
    /// Parsed `k=v` parameters.
    pub params: Params,
    /// The body.
    pub body: String,
}

/// Whether `segment` is a `k=v` parameter rather than body text.
fn is_param(segment: &str) -> bool {
    let Some((key, value)) = segment.split_once('=') else {
        return false;
    };
    if key.is_empty() {
        return false;
    }
    key.bytes()
        .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
        && value
            .bytes()
            .all(|b| (0x21..=0x3a).contains(&b) || (0x3c..=0x7e).contains(&b))
}

/// Split a complete `ESC _ tsp;… ESC \` string into verb, parameters and body.
/// Returns `None` when the sequence is not a TSP message.
pub fn split(sequence: &str) -> Option<Raw> {
    let inner = sequence.strip_prefix(TSP_PREFIX)?.strip_suffix(TSP_ST)?;
    let Some(semi) = inner.find(';') else {
        return (!inner.is_empty()).then(|| Raw {
            verb: inner.to_owned(),
            params: Params::new(),
            body: String::new(),
        });
    };
    if semi == 0 {
        return None;
    }
    let verb = inner[..semi].to_owned();
    let mut params = Params::new();
    let mut pos = semi + 1;
    while let Some(next) = inner[pos..].find(';').map(|i| pos + i) {
        let segment = &inner[pos..next];
        if !is_param(segment) {
            break;
        }
        if let Some((key, value)) = segment.split_once('=') {
            params.insert(key.to_owned(), value.to_owned());
        }
        pos = next + 1;
    }
    Some(Raw {
        verb,
        params,
        body: inner[pos..].to_owned(),
    })
}

/// A decoded terminal → program message.
#[derive(Debug, Clone, PartialEq)]
pub enum Incoming {
    /// A reply to a query.
    Reply(Reply),
    /// An event.
    Event(Event),
}

/// Decode an unchunked reply/event body. `None` for malformed JSON, missing
/// fields, or program → terminal verbs.
pub fn decode_body(verb: &str, body: &str) -> Option<Incoming> {
    match verb {
        "r" => serde_json::from_str::<Reply>(body)
            .ok()
            .map(Incoming::Reply),
        "e" => serde_json::from_str::<Event>(body)
            .ok()
            .map(Incoming::Event),
        _ => None,
    }
}

/// Reassembles chunked terminal → program messages and decodes replies and
/// events.
#[derive(Debug, Default)]
pub struct Reader {
    // None is a rejected assembly: consume its remaining fragments rather than
    // interpreting a valid-looking final fragment as a separate message.
    chunks: HashMap<(String, String), Option<String>>,
}

const MAX_INCOMING_BODY: usize = 1 << 20;
const MAX_INCOMING_CHUNKS: usize = 16;
const MAX_INCOMING_RETAINED: usize = 4 << 20;

impl Reader {
    /// A fresh reader.
    pub fn new() -> Self {
        Reader::default()
    }

    /// Feed one complete APC string. Returns the decoded message once complete,
    /// otherwise `None`.
    pub fn feed(&mut self, sequence: &str) -> Option<Incoming> {
        let raw = split(sequence)?;
        if !matches!(raw.verb.as_str(), "r" | "e") {
            return None;
        }
        let mut body = raw.body;
        if let Some(chunk_id) = raw.params.get("c") {
            if chunk_id.len() > 128 {
                return None;
            }
            let key = (raw.verb.clone(), chunk_id.clone());
            let mut joined = match self.chunks.remove(&key) {
                Some(assembly) => assembly,
                None if self.chunks.len() >= MAX_INCOMING_CHUNKS => return None,
                None => Some(String::new()),
            };
            let retained: usize = self
                .chunks
                .values()
                .filter_map(Option::as_ref)
                .map(String::len)
                .sum();
            if let Some(text) = &mut joined {
                if text.len() + body.len() > MAX_INCOMING_BODY
                    || retained + text.len() + body.len() > MAX_INCOMING_RETAINED
                {
                    joined = None;
                } else {
                    text.push_str(&body);
                }
            }
            if raw.params.get("m").map(String::as_str) == Some("1") {
                self.chunks.insert(key, joined);
                return None;
            }
            body = joined?;
        }
        if body.len() > MAX_INCOMING_BODY {
            return None;
        }
        decode_body(&raw.verb, &body)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejected_chunk_assemblies_stay_rejected_through_the_final_fragment() {
        let mut reader = Reader::new();
        let params = Params::from([("c".into(), "large".into()), ("m".into(), "1".into())]);
        for _ in 0..3 {
            assert!(reader
                .feed(&encode(
                    Verb::Event,
                    &"x".repeat(MAX_INCOMING_BODY / 2),
                    &params,
                    MAX_INCOMING_BODY
                ))
                .is_none());
        }
        let ack = r#"{"ev":"ack","sf":"test","s":1}"#;
        let last = Params::from([("c".into(), "large".into())]);
        assert!(reader
            .feed(&encode(Verb::Event, ack, &last, MAX_INCOMING_BODY))
            .is_none());
        assert!(reader
            .feed(&encode(Verb::Event, ack, &Params::new(), MAX_INCOMING_BODY))
            .is_some());
        assert!(reader.chunks.is_empty());
    }

    #[test]
    fn abandoned_chunk_ids_and_retained_payload_are_bounded() {
        let mut reader = Reader::new();
        for index in 0..100 {
            let params = Params::from([("c".into(), index.to_string()), ("m".into(), "1".into())]);
            reader.feed(&encode(
                Verb::Event,
                &"x".repeat(MAX_INCOMING_BODY / 2),
                &params,
                MAX_INCOMING_BODY,
            ));
            assert!(reader.chunks.len() <= MAX_INCOMING_CHUNKS);
            assert!(
                reader
                    .chunks
                    .values()
                    .filter_map(Option::as_ref)
                    .map(String::len)
                    .sum::<usize>()
                    <= MAX_INCOMING_RETAINED
            );
        }
    }
    use crate::wire::{Event, Text, TSP_DEFAULT_APC_LIMIT};

    #[test]
    fn assembles_and_splits_a_plain_message() {
        let body = serde_json::to_string(&crate::wire::Open {
            id: "s".into(),
            mode: crate::wire::SurfaceMode::Inline,
            title: Some("octet".into()),
            role: Some("octet.session".into()),
            adopt: None,
        })
        .unwrap();
        let encoded = encode(Verb::Open, &body, &Params::new(), TSP_DEFAULT_APC_LIMIT);
        assert!(encoded.starts_with("\x1b_tsp;o;"));
        assert!(encoded.ends_with("\x1b\\"));
        let raw = split(&encoded).expect("splits");
        assert_eq!(raw.verb, "o");
        assert_eq!(raw.body, body);
    }

    #[test]
    fn chunking_is_utf8_safe_and_reassembles() {
        // Multi-byte text larger than a small limit forces several pieces.
        let text: String = "héllo wörld — 🚀 ".repeat(400);
        let limit = 64;
        let encoded = encode(Verb::Frame, &text, &Params::new(), limit);
        // Every chunk is its own APC sequence carrying the same chunk id, and
        // reassembling them reproduces the body byte for byte.
        let sequences: Vec<&str> = encoded.split(TSP_ST).filter(|s| !s.is_empty()).collect();
        assert!(sequences.len() > 1);
        let mut joined = String::new();
        let mut chunk_id: Option<String> = None;
        for seq in sequences {
            let raw = split(&format!("{seq}{TSP_ST}")).expect("raw");
            assert_eq!(raw.verb, "f");
            let id = raw.params.get("c").expect("chunk id").clone();
            assert_eq!(*chunk_id.get_or_insert_with(|| id.clone()), id);
            joined.push_str(&raw.body);
        }
        assert_eq!(joined, text);
    }

    #[test]
    fn decodes_replies_and_events() {
        let mut reader = Reader::new();
        let reply = r#"{"r":"hello","v":1,"term":"tern","kinds":["card"],"credits":2,"cols":156}"#;
        let seq = assemble(Verb::Reply, "", reply);
        let msg = reader.feed(&seq).expect("reply");
        match msg {
            Incoming::Reply(Reply::Hello(hello)) => {
                assert_eq!(hello.term, "tern");
                assert_eq!(hello.credits, Some(2));
                assert_eq!(hello.cols, Some(156));
            }
            other => panic!("unexpected {other:?}"),
        }
        let event = r#"{"ev":"ack","sf":"s","s":3}"#;
        let seq = assemble(Verb::Event, "", event);
        let msg = reader.feed(&seq).expect("event");
        assert_eq!(
            msg,
            Incoming::Event(Event::Ack {
                sf: "s".into(),
                s: 3
            })
        );
    }

    #[test]
    fn ignores_non_tsp_sequences() {
        let mut reader = Reader::new();
        assert!(reader.feed("\x1b[31mred").is_none());
        assert!(reader.feed("plain").is_none());
    }

    #[test]
    fn text_round_trips_as_string_or_spans() {
        assert_eq!(
            serde_json::to_value(Text::from("hi")).unwrap(),
            serde_json::json!("hi")
        );
        let spans = Text::Spans(vec![crate::wire::Span::styled("hi", "muted")]);
        assert_eq!(
            serde_json::to_value(spans).unwrap(),
            serde_json::json!([{ "t": "hi", "s": "muted" }])
        );
    }
}

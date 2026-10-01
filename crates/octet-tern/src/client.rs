//! Tern detection and surface lifecycle over the tty.
//!
//! A TSP program writes APC messages to its own stdout and reads replies and
//! events from its stdin. Tern advertises itself with `TERM_PROGRAM=tern`, so a
//! client can start optimistically against the full v1 vocabulary and refine
//! itself from the terminal's `hello` reply.

use std::collections::HashMap;
use std::io::{self, Write};

use crate::frame::{self, Incoming};
use crate::tty;
use crate::wire::{
    Close, Event, Frame, HelloReply, Kind, Node, Op, Open, Palette, Query, Reply, SurfaceMode,
    Verb, REGION_DOCK, REGION_LAYER, REGION_MAIN, TSP_DEFAULT_APC_LIMIT, TSP_DEFAULT_CREDITS,
    TSP_VERSION,
};

/// Whether the process is running inside a Tern pane.
///
/// Tern sets `TERM_PROGRAM=tern` (a multiplexer started from Tern can leave it
/// stale, so callers may also probe for the pane socket).
pub fn is_tern() -> bool {
    std::env::var("TERM_PROGRAM").is_ok_and(|value| value.eq_ignore_ascii_case("tern"))
}

/// Whether a Tern pane socket is present (`TERN_PANE_SOCKET`), the stronger signal.
pub fn has_pane_socket() -> bool {
    std::env::var_os("TERN_PANE_SOCKET").is_some_and(|value| !value.is_empty())
}

/// A TSP connection writing to stdout and reading events from stdin.
pub struct TernClient {
    output: Box<dyn Write + Send>,
    limit: usize,
    credits: u32,
    seq: HashMap<String, u64>,
    acked: HashMap<String, u64>,
    buffer: Vec<u8>,
    reader: frame::Reader,
    pending: Vec<Incoming>,
    columns: u16,
    dark: bool,
    reduce_motion: bool,
    kinds: Vec<String>,
    _raw: Option<tty::RawGuard>,
}

impl TernClient {
    /// Connect: put stdin in raw mode, announce `app`, and return the client.
    ///
    /// No surface is opened yet; call [`TernClient::open`] then send a frame.
    pub fn connect(app: &str, version: Option<&str>) -> io::Result<TernClient> {
        let mut client = Self::connect_shared_input(app, version)?;
        client._raw = tty::RawGuard::enable(0)?;
        Ok(client)
    }

    /// Announce without owning stdin or changing its modes. The application's
    /// single input owner must deliver replies through [`Self::observe`].
    pub fn connect_shared_input(app: &str, version: Option<&str>) -> io::Result<TernClient> {
        Self::with_writer(app, version, io::stdout())
    }

    /// Announce through a terminal-owned output sink without taking stdin.
    /// Replies must be supplied by the application's input owner via `observe`.
    pub fn with_writer(
        app: &str,
        version: Option<&str>,
        output: impl Write + Send + 'static,
    ) -> io::Result<Self> {
        let mut client = TernClient {
            output: Box::new(output),
            limit: TSP_DEFAULT_APC_LIMIT,
            credits: TSP_DEFAULT_CREDITS,
            seq: HashMap::new(),
            acked: HashMap::new(),
            buffer: Vec::new(),
            reader: frame::Reader::new(),
            pending: Vec::new(),
            columns: 80,
            dark: true,
            reduce_motion: false,
            kinds: crate::wire::TSP_KINDS
                .iter()
                .map(|k| (*k).to_owned())
                .collect(),
            _raw: None,
        };
        client.write(
            Verb::Query,
            &Query::Hello {
                v: vec![TSP_VERSION],
                app: app.to_owned(),
                ver: version.map(str::to_owned),
            },
        )?;
        Ok(client)
    }

    /// The current APC body limit in bytes.
    pub fn limit(&self) -> usize {
        self.limit
    }

    /// The current unacknowledged-frame allowance per surface.
    pub fn credits(&self) -> u32 {
        self.credits
    }

    /// The last known terminal width in columns.
    pub fn columns(&self) -> u16 {
        self.columns
    }

    /// Whether the terminal currently reports a dark appearance.
    pub fn dark(&self) -> bool {
        self.dark
    }

    /// Whether the terminal requests reduced motion.
    pub fn reduce_motion(&self) -> bool {
        self.reduce_motion
    }

    /// Whether there is credit to send without reading from the shared stdin.
    pub fn has_credit(&self, surface: &str) -> bool {
        let sent = self.seq.get(surface).copied().unwrap_or(0);
        let acked = self.acked.get(surface).copied().unwrap_or(0);
        sent.saturating_sub(acked) < u64::from(self.credits)
    }

    /// Forget credit state after reopening an evicted surface.
    pub fn reset_surface(&mut self, surface: &str) {
        self.seq.remove(surface);
        self.acked.remove(surface);
    }

    /// Upload a content-addressed blob whose body is already base64 encoded.
    pub fn blob(&mut self, id: &str, mime: &str, body: &str) -> io::Result<()> {
        if id.len() != 64
            || !id.bytes().all(|byte| byte.is_ascii_hexdigit())
            || mime.is_empty()
            || !mime.bytes().all(|byte| {
                byte.is_ascii_alphanumeric() || matches!(byte, b'/' | b'-' | b'+' | b'.')
            })
            || !body
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'+' | b'/' | b'='))
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "invalid native blob address, MIME type or base64 body",
            ));
        }
        let params = frame::Params::from([("id".into(), id.into()), ("mime".into(), mime.into())]);
        let encoded = frame::encode(Verb::Blob, body, &params, self.limit);
        self.output.write_all(encoded.as_bytes())?;
        self.output.flush()
    }

    /// Whether the terminal draws a kind (optimistic before the real `hello`).
    pub fn supports(&self, kind: Kind) -> bool {
        self.kinds.iter().any(|k| k == kind.as_str())
    }

    fn write<T: serde::Serialize>(&mut self, verb: Verb, value: &T) -> io::Result<()> {
        let encoded = frame::encode_json(verb, value, self.limit)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
        self.output.write_all(encoded.as_bytes())?;
        self.output.flush()
    }

    /// Apply the terminal's `hello` reply.
    pub fn apply_hello(&mut self, hello: &HelloReply) {
        if hello.apc.is_some_and(|limit| limit > 0) {
            self.limit = hello.apc.unwrap_or(self.limit);
        }
        if let Some(credits) = hello.credits {
            self.credits = credits.max(1);
        }
        if let Some(cols) = hello.cols {
            self.columns = cols;
        }
        if let Some(dark) = hello.dark {
            self.dark = dark;
        }
        if let Some(reduce) = hello.reduce_motion {
            self.reduce_motion = reduce;
        }
        self.kinds = hello.kinds.clone();
    }

    /// Send the pending keyboard focus to `id` (`None` clears it).
    pub fn focus(&mut self, surface: &str, id: Option<&str>) -> io::Result<u64> {
        self.frame_ops(
            surface,
            vec![Op::Focus {
                id: id.map(str::to_owned),
            }],
        )
    }

    /// Open (or adopt) a surface.
    pub fn open(
        &mut self,
        id: &str,
        mode: SurfaceMode,
        title: &str,
        role: Option<&str>,
    ) -> io::Result<()> {
        self.write(
            Verb::Open,
            &Open {
                id: id.to_owned(),
                mode,
                title: Some(title.to_owned()),
                role: role.map(str::to_owned),
                adopt: None,
            },
        )
    }

    /// Send the surface's resolved theme palette.
    pub fn palette(&mut self, palette: &Palette) -> io::Result<()> {
        self.write(Verb::Palette, palette)
    }

    /// Close a surface; `keep` leaves `main` in scrollback.
    pub fn close(&mut self, id: &str, keep: bool) -> io::Result<()> {
        self.write(
            Verb::Close,
            &Close {
                id: id.to_owned(),
                keep,
            },
        )
    }

    /// The default idempotent region nodes for a surface root.
    pub fn regions(surface: &str) -> Vec<Op> {
        [REGION_MAIN, REGION_DOCK, REGION_LAYER]
            .iter()
            .map(|id| Op::Add {
                id: (*id).to_owned(),
                parent: surface.to_owned(),
                before: None,
                node: Node::new(*id, Kind::Col, crate::wire::Props::new()),
            })
            .collect()
    }

    /// Send a frame of ops, respecting credit-based flow control.
    pub fn frame_ops(&mut self, surface: &str, ops: Vec<Op>) -> io::Result<u64> {
        self.await_credit(surface)?;
        self.frame_ops_now(surface, ops)
    }

    /// Send a frame without waiting for credit.
    ///
    /// For a shared input owner. The caller must check [`Self::has_credit`]
    /// and deliver acknowledgements through [`Self::observe`].
    pub fn frame_ops_now(&mut self, surface: &str, ops: Vec<Op>) -> io::Result<u64> {
        if !self.has_credit(surface) {
            return Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                "Tern frame credit exhausted",
            ));
        }
        let s = self.seq.get(surface).copied().unwrap_or(0) + 1;
        let frame = Frame {
            sf: surface.to_owned(),
            s,
            ops,
        };
        self.write(Verb::Frame, &frame)?;
        self.seq.insert(surface.to_owned(), s);
        Ok(s)
    }

    /// Block until the surface has room for another frame, bounded to avoid
    /// hanging when a terminal never answers.
    fn await_credit(&mut self, surface: &str) -> io::Result<()> {
        let mut waited = 0u32;
        while !self.has_credit(surface) && waited < 2000 {
            let _ = self.read_one(250);
            waited += 250;
        }
        if self.has_credit(surface) {
            Ok(())
        } else {
            Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "Tern stopped acknowledging frames",
            ))
        }
    }

    /// Read and decode one terminal message, waiting at most `timeout_ms`.
    ///
    /// Acks update internal flow-control state; every message is also queued for
    /// [`TernClient::drain_events`].
    pub fn read_one(&mut self, timeout_ms: i32) -> Option<Incoming> {
        if !tty::poll_readable(0, timeout_ms) {
            return None;
        }
        let mut buf = [0u8; 4096];
        match tty::read(0, &mut buf) {
            Ok(0) | Err(_) => return None,
            Ok(n) => self.buffer.extend_from_slice(&buf[..n]),
        }
        let sequences = tty::extract_apc(&mut self.buffer);
        let mut first = None;
        for sequence in sequences {
            if let Some(message) = self.reader.feed(&sequence) {
                self.observe(&message);
                if first.is_none() {
                    first = Some(message.clone());
                }
                self.pending.push(message);
            }
        }
        first
    }

    /// Apply a message delivered by the application's single input owner.
    pub fn observe(&mut self, message: &Incoming) {
        match message {
            Incoming::Reply(Reply::Hello(hello)) => self.apply_hello(hello),
            Incoming::Event(Event::Ack { sf, s }) => {
                let entry = self.acked.entry(sf.clone()).or_insert(0);
                let sent = self.seq.get(sf).copied().unwrap_or(0);
                *entry = (*entry).max((*s).min(sent));
            }
            Incoming::Event(Event::Resize { cols, .. }) => self.columns = *cols,
            Incoming::Event(Event::Theme { dark }) => self.dark = *dark,
            Incoming::Event(Event::Motion { reduce }) => self.reduce_motion = *reduce,
            _ => {}
        }
    }

    /// Take the messages observed since the last call.
    pub fn drain_events(&mut self) -> Vec<Incoming> {
        std::mem::take(&mut self.pending)
    }

    /// Send an `add` of a node under a parent, returning the frame sequence.
    pub fn add(&mut self, surface: &str, parent: &str, node: Node) -> io::Result<u64> {
        self.frame_ops(surface, vec![Op::add(parent, node)])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    };

    struct Output(Arc<AtomicBool>);
    impl Write for Output {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            if self.0.load(Ordering::Relaxed) {
                return Err(io::Error::other("write failed"));
            }
            Ok(bytes.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    #[test]
    fn credit_and_sequences_advance_only_after_successful_writes() {
        let fail = Arc::new(AtomicBool::new(false));
        let mut client = TernClient::with_writer("test", None, Output(fail.clone())).unwrap();
        fail.store(true, Ordering::Relaxed);
        assert!(client.frame_ops_now("test", Vec::new()).is_err());
        assert!(!client.seq.contains_key("test"));
        fail.store(false, Ordering::Relaxed);
        assert_eq!(client.frame_ops_now("test", Vec::new()).unwrap(), 1);
        assert_eq!(client.frame_ops_now("test", Vec::new()).unwrap(), 2);
        assert_eq!(
            client.frame_ops_now("test", Vec::new()).unwrap_err().kind(),
            io::ErrorKind::WouldBlock
        );
        client.observe(&Incoming::Event(Event::Ack {
            sf: "test".into(),
            s: 1,
        }));
        client.observe(&Incoming::Event(Event::Ack {
            sf: "test".into(),
            s: 0,
        }));
        assert_eq!(client.acked["test"], 1);
        assert_eq!(client.frame_ops_now("test", Vec::new()).unwrap(), 3);
        client.observe(&Incoming::Event(Event::Ack {
            sf: "test".into(),
            s: u64::MAX,
        }));
        assert_eq!(client.acked["test"], 3);
        client.reset_surface("test");
        assert_eq!(client.frame_ops_now("test", Vec::new()).unwrap(), 1);
    }
    #[test]
    fn native_blob_headers_and_bodies_cannot_inject_escape_sequences() {
        let mut client = TernClient::with_writer("test", None, io::sink()).unwrap();
        let hash = "f".repeat(64);
        assert!(client.blob(&hash, "image/png", "YWJj").is_ok());
        assert!(client.blob("wrong", "image/png", "YWJj").is_err());
        assert!(client.blob(&hash, "image/png;bad", "YWJj").is_err());
        assert!(client.blob(&hash, "image/png", "\x1b[200~").is_err());
    }
}

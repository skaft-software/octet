//! The Tern Surface Protocol v1 message schema.
//!
//! Mirrors `@oh-my-pi/pi-wire`'s `tsp.ts` shape for shape: the same verbs,
//! component kinds, tones, ops, node structure, theme palette and event set.
//! Property bags are carried as JSON objects ([`Props`]) because each kind has
//! its own typed props and the terminal ignores unknown fields.

use serde::{Deserialize, Serialize, Serializer};
use serde_json::{Map, Value};

/// Protocol version this crate speaks.
pub const TSP_VERSION: u32 = 1;

/// APC identifier: every TSP message body starts with this (`ESC _ tsp;`).
pub const TSP_PREFIX: &str = "\x1b_tsp;";

/// APC string terminator (`ESC \`).
pub const TSP_ST: &str = "\x1b\\";

/// Largest APC body a sender emits before chunking unless the hello reply says otherwise.
pub const TSP_DEFAULT_APC_LIMIT: usize = 65_536;

/// Unacknowledged frames a sender may have in flight unless the hello reply says otherwise.
pub const TSP_DEFAULT_CREDITS: u32 = 2;

/// Every component kind in the v1 vocabulary, in the protocol's order.
pub const TSP_KINDS: &[&str] = &[
    "col",
    "row",
    "card",
    "section",
    "rule",
    "spacer",
    "text",
    "md",
    "code",
    "diff",
    "ansi",
    "math",
    "image",
    "kv",
    "table",
    "tree",
    "badge",
    "kbd",
    "icon",
    "spinner",
    "shimmer",
    "elapsed",
    "progress",
    "rate",
    "list",
    "item",
    "tabs",
    "editor",
    "input",
    "status",
    "seg",
    "overlay",
    "toast",
    "rows",
    "picker",
    "prefs",
    "tool",
    "checklist",
    "agent",
    "chart",
    "meter",
    "effort",
];

/// Fixed ids of a surface's three regions.
pub const REGION_MAIN: &str = "main";
/// Dock region id (HUD pills, background jobs).
pub const REGION_DOCK: &str = "dock";
/// Layer region id (overlays and sheets).
pub const REGION_LAYER: &str = "layer";

/// One-letter message verb.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Verb {
    /// Query (program → terminal).
    #[serde(rename = "q")]
    Query,
    /// Open or adopt a surface (program → terminal).
    #[serde(rename = "o")]
    Open,
    /// A frame of ops (program → terminal).
    #[serde(rename = "f")]
    Frame,
    /// A content-addressed blob (program → terminal).
    #[serde(rename = "b")]
    Blob,
    /// The resolved theme palette for a surface (program → terminal).
    #[serde(rename = "t")]
    Palette,
    /// Close a surface (program → terminal).
    #[serde(rename = "x")]
    Close,
    /// A reply (terminal → program).
    #[serde(rename = "r")]
    Reply,
    /// An event (terminal → program).
    #[serde(rename = "e")]
    Event,
}

impl Verb {
    /// The one-letter wire form.
    pub fn as_str(self) -> &'static str {
        match self {
            Verb::Query => "q",
            Verb::Open => "o",
            Verb::Frame => "f",
            Verb::Blob => "b",
            Verb::Palette => "t",
            Verb::Close => "x",
            Verb::Reply => "r",
            Verb::Event => "e",
        }
    }
}

/// One component kind in the v1 vocabulary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    /// Flex column.
    Col,
    /// Flex row.
    Row,
    /// Bounded card with a head and a body.
    Card,
    /// Collapsible section.
    Section,
    /// Horizontal rule, optional label.
    Rule,
    /// Spacing step.
    Spacer,
    /// Styled text.
    Text,
    /// Markdown.
    Md,
    /// Code block.
    Code,
    /// Unified or split diff.
    Diff,
    /// Raw terminal output.
    Ansi,
    /// Typeset math.
    Math,
    /// Image.
    Image,
    /// Key/value list.
    Kv,
    /// Table.
    Table,
    /// Tree.
    Tree,
    /// Short badge.
    Badge,
    /// Keycap group.
    Kbd,
    /// Named icon.
    Icon,
    /// Spinner.
    Spinner,
    /// Clocked shimmer text.
    Shimmer,
    /// Clocked elapsed time.
    Elapsed,
    /// Progress bar.
    Progress,
    /// Rate readout.
    Rate,
    /// List.
    List,
    /// List item.
    Item,
    /// Tab strip.
    Tabs,
    /// Editable text.
    Editor,
    /// Single-line input.
    Input,
    /// Status bar root.
    Status,
    /// Status segment.
    Seg,
    /// Overlay wrapper.
    Overlay,
    /// Transient toast.
    Toast,
    /// Pre-rendered ANSI rows (migration fallback).
    Rows,
    /// Data-first picker sheet.
    Picker,
    /// Data-first settings page set.
    Prefs,
    /// One tool call.
    Tool,
    /// Checklist (todo tool, dock HUD, or reminder).
    Checklist,
    /// One subagent.
    Agent,
    /// Native chart (heatmap, bars, spark).
    Chart,
    /// Bar, ring or block-grid meter.
    Meter,
    /// Thinking-effort glyph.
    Effort,
}

impl Kind {
    /// The wire name.
    pub fn as_str(self) -> &'static str {
        match self {
            Kind::Col => "col",
            Kind::Row => "row",
            Kind::Card => "card",
            Kind::Section => "section",
            Kind::Rule => "rule",
            Kind::Spacer => "spacer",
            Kind::Text => "text",
            Kind::Md => "md",
            Kind::Code => "code",
            Kind::Diff => "diff",
            Kind::Ansi => "ansi",
            Kind::Math => "math",
            Kind::Image => "image",
            Kind::Kv => "kv",
            Kind::Table => "table",
            Kind::Tree => "tree",
            Kind::Badge => "badge",
            Kind::Kbd => "kbd",
            Kind::Icon => "icon",
            Kind::Spinner => "spinner",
            Kind::Shimmer => "shimmer",
            Kind::Elapsed => "elapsed",
            Kind::Progress => "progress",
            Kind::Rate => "rate",
            Kind::List => "list",
            Kind::Item => "item",
            Kind::Tabs => "tabs",
            Kind::Editor => "editor",
            Kind::Input => "input",
            Kind::Status => "status",
            Kind::Seg => "seg",
            Kind::Overlay => "overlay",
            Kind::Toast => "toast",
            Kind::Rows => "rows",
            Kind::Picker => "picker",
            Kind::Prefs => "prefs",
            Kind::Tool => "tool",
            Kind::Checklist => "checklist",
            Kind::Agent => "agent",
            Kind::Chart => "chart",
            Kind::Meter => "meter",
            Kind::Effort => "effort",
        }
    }
}

/// Semantic colour of a node's chrome.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Tone {
    /// Neutral chrome.
    Neutral,
    /// Accent chrome.
    Accent,
    /// Informational chrome.
    Info,
    /// Success chrome.
    Success,
    /// Warning chrome.
    Warning,
    /// Error chrome.
    Error,
    /// Pending chrome.
    Pending,
    /// Muted chrome.
    Muted,
    /// The user's own colour.
    User,
}

/// Spacing step for `gap`/`size`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Space {
    /// No space.
    None,
    /// Extra small.
    Xs,
    /// Small.
    Sm,
    /// Medium.
    Md,
    /// Large.
    Lg,
}

/// Per-span visual effect, clocked by the terminal.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Effect {
    /// Travelling shimmer.
    Shimmer,
    /// Steady pulse.
    Pulse,
    /// No effect.
    None,
}

/// One styled run of text.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Span {
    /// The run's text.
    pub t: String,
    /// Space-separated semantic tokens or theme token names.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub s: Option<String>,
    /// Terminal-clocked visual effect.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fx: Option<Effect>,
    /// Link target for `open`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub href: Option<String>,
}

impl Span {
    /// A plain run.
    pub fn new(t: impl Into<String>) -> Self {
        Span {
            t: t.into(),
            s: None,
            fx: None,
            href: None,
        }
    }

    /// A run with a token string.
    pub fn styled(t: impl Into<String>, s: impl Into<String>) -> Self {
        Span {
            t: t.into(),
            s: Some(s.into()),
            fx: None,
            href: None,
        }
    }

    /// Attach a terminal-clocked effect.
    pub fn effect(mut self, fx: Effect) -> Self {
        self.fx = Some(fx);
        self
    }
}

/// Text given either as one plain string or as styled spans.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Text {
    /// A plain string.
    Plain(String),
    /// Styled runs.
    Spans(Vec<Span>),
}

impl From<&str> for Text {
    fn from(value: &str) -> Self {
        Text::Plain(value.to_owned())
    }
}

impl From<String> for Text {
    fn from(value: String) -> Self {
        Text::Plain(value)
    }
}

impl From<Vec<Span>> for Text {
    fn from(value: Vec<Span>) -> Self {
        Text::Spans(value)
    }
}

/// A property bag for a node, keyed by the protocol's prop names.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Props(Map<String, Value>);

impl Props {
    /// An empty bag.
    pub fn new() -> Self {
        Props(Map::new())
    }

    /// Set a property from any serializable value.
    pub fn set(mut self, key: impl Into<String>, value: impl Serialize) -> Self {
        // Props are built from crate-controlled types; serialization cannot fail
        // for the values used here. Fall back to null rather than panicking.
        let value = serde_json::to_value(value).unwrap_or(Value::Null);
        self.0.insert(key.into(), value);
        self
    }

    /// Set a text-typed property (`string | spans`).
    pub fn text(self, key: impl Into<String>, value: impl Into<Text>) -> Self {
        self.set(key, value.into())
    }

    /// Set a tone-typed property.
    pub fn tone(self, tone: Tone) -> Self {
        self.set("tone", tone)
    }

    /// Set the common `role` property.
    pub fn role(self, role: impl Into<String>) -> Self {
        self.set("role", role.into())
    }

    /// Whether the bag is empty (and so should be omitted from the wire).
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Borrow the underlying map.
    pub fn as_map(&self) -> &Map<String, Value> {
        &self.0
    }

    /// Build from a JSON object; a non-object yields an empty bag.
    pub fn from_value(value: Value) -> Props {
        match value {
            Value::Object(map) => Props(map),
            _ => Props::new(),
        }
    }
}

impl Serialize for Props {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.0.serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for Props {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Map::<String, Value>::deserialize(deserializer).map(Props)
    }
}

/// A node on the wire.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Node {
    /// Stable node id, unique within its surface.
    pub id: String,
    /// Component kind.
    pub k: Kind,
    /// Kind-specific and common properties; omitted when empty.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub p: Option<Props>,
    /// Children, in order; omitted when empty.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub c: Option<Vec<Node>>,
}

impl Node {
    /// A leaf node with no props.
    pub fn leaf(id: impl Into<String>, k: Kind) -> Self {
        Node {
            id: id.into(),
            k,
            p: None,
            c: None,
        }
    }

    /// A node with props.
    pub fn new(id: impl Into<String>, k: Kind, p: Props) -> Self {
        Node {
            id: id.into(),
            k,
            p: (!p.is_empty()).then_some(p),
            c: None,
        }
    }

    /// A node with children.
    pub fn with_children(id: impl Into<String>, k: Kind, p: Props, c: Vec<Node>) -> Self {
        Node {
            id: id.into(),
            k,
            p: (!p.is_empty()).then_some(p),
            c: (!c.is_empty()).then_some(c),
        }
    }

    /// Push a child.
    pub fn push(&mut self, child: Node) {
        self.c.get_or_insert_with(Vec::new).push(child);
    }
}

/// One op in a frame.
///
/// Field meanings follow the op names; the wire form is a positional array.
#[derive(Debug, Clone, PartialEq)]
#[allow(missing_docs)]
pub enum Op {
    /// Add (or replace) a node under `parent`, before `before` when given.
    Add {
        id: String,
        parent: String,
        before: Option<String>,
        node: Node,
    },
    /// Merge props into an existing node.
    Set { id: String, props: Props },
    /// Replace or append a text kind's primary text.
    Text {
        id: String,
        append: bool,
        text: String,
    },
    /// Splice a text kind's primary text.
    Splice {
        id: String,
        at: usize,
        del: usize,
        text: String,
    },
    /// Move a node under a new parent.
    Move {
        id: String,
        parent: String,
        before: Option<String>,
    },
    /// Remove a node.
    Del { id: String },
    /// Mark a node settled (its motion stops).
    Settle { id: String },
    /// Move keyboard focus.
    Focus { id: Option<String> },
    /// Scroll a node into view.
    Reveal { id: String, where_: &'static str },
    /// Suspend the surface's clocked motion.
    Suspend,
    /// Resume the surface's clocked motion.
    Resume,
}

impl Op {
    /// `add` convenience.
    pub fn add(parent: impl Into<String>, node: Node) -> Self {
        Op::Add {
            id: node.id.clone(),
            parent: parent.into(),
            before: None,
            node,
        }
    }
}

impl Serialize for Op {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeSeq;
        let mut seq = serializer.serialize_seq(None)?;
        match self {
            Op::Add {
                id,
                parent,
                before,
                node,
            } => {
                seq.serialize_element("add")?;
                seq.serialize_element(id)?;
                seq.serialize_element(parent)?;
                seq.serialize_element(before)?;
                seq.serialize_element(node)?;
            }
            Op::Set { id, props } => {
                seq.serialize_element("set")?;
                seq.serialize_element(id)?;
                seq.serialize_element(props)?;
            }
            Op::Text { id, append, text } => {
                seq.serialize_element("text")?;
                seq.serialize_element(id)?;
                seq.serialize_element(if *append { "append" } else { "replace" })?;
                seq.serialize_element(text)?;
            }
            Op::Splice { id, at, del, text } => {
                seq.serialize_element("splice")?;
                seq.serialize_element(id)?;
                seq.serialize_element(at)?;
                seq.serialize_element(del)?;
                seq.serialize_element(text)?;
            }
            Op::Move { id, parent, before } => {
                seq.serialize_element("move")?;
                seq.serialize_element(id)?;
                seq.serialize_element(parent)?;
                seq.serialize_element(before)?;
            }
            Op::Del { id } => {
                seq.serialize_element("del")?;
                seq.serialize_element(id)?;
            }
            Op::Settle { id } => {
                seq.serialize_element("settle")?;
                seq.serialize_element(id)?;
            }
            Op::Focus { id } => {
                seq.serialize_element("focus")?;
                seq.serialize_element(id)?;
            }
            Op::Reveal { id, where_ } => {
                seq.serialize_element("reveal")?;
                seq.serialize_element(id)?;
                seq.serialize_element(where_)?;
            }
            Op::Suspend => {
                seq.serialize_element("suspend")?;
            }
            Op::Resume => {
                seq.serialize_element("resume")?;
            }
        }
        seq.end()
    }
}

/// Verb `q`: the `hello` query, or a blob-presence query.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "q", rename_all = "lowercase")]
pub enum Query {
    /// Announce the program and the protocol versions it speaks.
    Hello {
        /// Protocol versions the program speaks, newest first.
        v: Vec<u32>,
        /// Program name.
        app: String,
        /// Program version.
        #[serde(skip_serializing_if = "Option::is_none")]
        ver: Option<String>,
    },
    /// Ask which blob ids the terminal already has.
    Blobs {
        /// Blob content addresses to check.
        ids: Vec<String>,
    },
}

/// Verb `o`: open (or adopt) a surface.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Open {
    /// Surface id.
    pub id: String,
    /// How the terminal presents the surface.
    pub mode: SurfaceMode,
    /// Tab title hint.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// Program-defined role namespace for the surface.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    /// Adopt an existing surface with this id instead of creating one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub adopt: Option<bool>,
}

/// How the terminal presents a surface.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SurfaceMode {
    /// In the pane's scrollback flow (the session surface).
    Inline,
    /// A fullscreen page (a sheet that borrows the viewport).
    Screen,
}

/// Verb `t`: the program's resolved theme palette for a surface.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Palette {
    /// Surface id.
    pub sf: String,
    /// Dark-variant tokens: name → `#rrggbb`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dark: Option<Map<String, Value>>,
    /// Light-variant tokens: name → `#rrggbb`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub light: Option<Map<String, Value>>,
    /// Theme names behind each variant.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<VariantNames>,
}

/// The theme names behind a palette's variants.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct VariantNames {
    /// Dark theme name.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dark: Option<String>,
    /// Light theme name.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub light: Option<String>,
}

/// Verb `f`: an atomic batch of ops for one surface.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Frame {
    /// Surface id.
    pub sf: String,
    /// Monotonic per-surface sequence number.
    pub s: u64,
    /// The ops, applied atomically.
    pub ops: Vec<Op>,
}

/// Verb `x`: close a surface.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Close {
    /// Surface id.
    pub id: String,
    /// Keep `main` in scrollback (true) or remove the surface (false).
    pub keep: bool,
}

/// Verb `r`: a terminal reply.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(tag = "r", rename_all = "lowercase")]
pub enum Reply {
    /// The terminal's answer to `hello`.
    Hello(HelloReply),
    /// The blob ids the terminal already has.
    Blobs {
        /// Held blob ids.
        have: Vec<String>,
    },
}

/// The terminal's `hello` reply.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HelloReply {
    /// Protocol version.
    pub v: u32,
    /// Terminal name.
    pub term: String,
    /// Terminal version.
    #[serde(default)]
    pub ver: Option<String>,
    /// Component kinds this terminal draws.
    pub kinds: Vec<String>,
    /// Feature flags.
    #[serde(default)]
    pub features: Option<Vec<String>>,
    /// APC body limit in bytes.
    #[serde(default)]
    pub apc: Option<usize>,
    /// Unacknowledged frames allowed per surface.
    #[serde(default)]
    pub credits: Option<u32>,
    /// Columns.
    #[serde(default)]
    pub cols: Option<u16>,
    /// Cell size in pixels.
    #[serde(default)]
    pub cell: Option<Cell>,
    /// Whether the terminal is dark.
    #[serde(default)]
    pub dark: Option<bool>,
    /// Whether the user prefers reduced motion.
    #[serde(default)]
    pub reduce_motion: Option<bool>,
}

/// A terminal cell's pixel size.
#[derive(Debug, Clone, Copy, PartialEq, Deserialize)]
pub struct Cell {
    /// Width in pixels.
    pub w: u32,
    /// Height in pixels.
    pub h: u32,
}

/// Verb `e`: terminal → program events.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(tag = "ev", rename_all = "lowercase")]
pub enum Event {
    /// A frame was acknowledged.
    Ack {
        /// Surface id.
        sf: String,
        /// Sequence number acknowledged.
        s: u64,
    },
    /// The surface or pane resized.
    Resize {
        /// Surface id.
        #[serde(default)]
        sf: Option<String>,
        /// Columns.
        cols: u16,
        /// Cell size in pixels.
        #[serde(default)]
        cell: Option<Cell>,
        /// Whether the pane is visible.
        #[serde(default)]
        visible: Option<bool>,
    },
    /// The terminal switched appearance.
    Theme {
        /// Dark (true) or light (false).
        dark: bool,
    },
    /// Reduce Motion toggled.
    Motion {
        /// Whether motion is reduced.
        reduce: bool,
    },
    /// A surface became visible or hidden.
    Visible {
        /// Surface id.
        #[serde(default)]
        sf: Option<String>,
        /// Whether it is visible.
        visible: bool,
    },
    /// A collapsible node toggled.
    Toggle {
        /// Surface id.
        sf: String,
        /// Node id.
        id: String,
        /// Bound key, when any.
        #[serde(default)]
        key: Option<String>,
        /// Whether it is now collapsed.
        collapsed: bool,
    },
    /// A list selection changed.
    Select {
        /// Surface id.
        sf: String,
        /// List node id.
        id: String,
        /// Item id.
        item: String,
    },
    /// A node activated.
    Activate {
        /// Surface id.
        sf: String,
        /// Node id.
        id: String,
        /// Item id.
        item: String,
    },
    /// A node action fired.
    Action {
        /// Surface id.
        sf: String,
        /// Node id.
        id: String,
        /// Action id.
        act: String,
        /// Action value, when any.
        #[serde(default)]
        value: Option<String>,
        /// Modifier keys held.
        #[serde(default)]
        mods: Option<Vec<String>>,
    },
    /// A typed value changed.
    Change {
        /// Surface id.
        sf: String,
        /// Node id.
        id: String,
        /// Item id.
        item: String,
        /// The new value.
        #[serde(default)]
        value: Option<Value>,
    },
    /// An edit over the terminal's selection in an editor/input node.
    Edit {
        /// Surface id.
        sf: String,
        /// Node id.
        id: String,
        /// Replace `[from, to)`.
        from: usize,
        /// End of the replaced range.
        to: usize,
        /// Replacement text.
        text: String,
        /// Caret after the edit.
        cursor: usize,
        /// Text length the terminal saw.
        len: usize,
    },
    /// The terminal reported an error for a frame.
    Error {
        /// Surface id.
        #[serde(default)]
        sf: Option<String>,
        /// Sequence number.
        #[serde(default)]
        s: Option<u64>,
        /// Op index.
        #[serde(default)]
        op: Option<usize>,
        /// Message.
        msg: String,
    },
    /// Nodes are gone (evicted or forgotten).
    Gone {
        /// Surface id.
        #[serde(default)]
        sf: Option<String>,
        /// Node ids.
        ids: Vec<String>,
    },
}

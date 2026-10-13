//! A source-only, tool-authoring SDK for supervised API 0.4 executable extensions.
//! This is neither the native-host embedding protocol nor an in-process plugin ABI.
//! Rust, C and C++ use the same framing, negotiation, validation and lifecycle.
pub mod diagnostic;
pub mod ffi;
pub use diagnostic::{Diagnostic, Severity};
mod protocol;
mod bulk;
pub use bulk::{BlobDigest, BlobRef};
mod resource;
mod reverse;
pub use resource::{CleanupStatus, ReleaseStatus, Resource, ResourceType};
mod schema;
mod values;

use std::collections::BTreeMap;
use std::fmt;
use std::sync::{Arc, Mutex};
use std::time::Duration;

pub use schemars::JsonSchema;
use serde::de::DeserializeOwned;
pub use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

/// SDK frame cap, excluding LF (not a canonical API 0.3 negotiated limit).
pub const MAX_FRAME_BYTES: usize = 1024 * 1024;
/// Per-text byte cap, leaving room for JSON escaping inside the frame.
pub const MAX_TEXT_BYTES: usize = 128 * 1024;
/// Maximum statically registered tools.
pub const MAX_TOOLS: usize = 256;

/// An explicit domain failure, cancellation, authoring error or runtime error.
#[derive(Debug, Clone)]
pub struct Error {
    pub(crate) code: i32,
    pub(crate) message: String,
}
impl Error {
    /// Domain failure: delivered as one text part with `is_error = true`.
    pub fn tool(message: impl Into<String>) -> Self {
        Self {
            code: 0,
            message: message.into(),
        }
    }
    /// Cooperative cancellation (no rollback guarantee).
    pub fn cancelled() -> Self {
        Self {
            code: -32800,
            message: "Request cancelled".into(),
        }
    }
    pub(crate) fn rpc(code: i32, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
    pub(crate) fn invalid(message: impl Into<String>) -> Self {
        Self::rpc(-32602, message)
    }
    pub(crate) fn internal() -> Self {
        Self::rpc(-32603, "Internal error")
    }
}
impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}
impl std::error::Error for Error {}

/// A bounded explicit text projection with optional typed output and diagnostics.
#[derive(Debug)]
pub struct ToolResult {
    text: String,
    is_error: bool,
    structured_content: Option<Value>,
    diagnostics: Vec<Diagnostic>,
}
impl ToolResult {
    /// Successful model-visible text.
    pub fn text(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            is_error: false,
            structured_content: None,
            diagnostics: Vec::new(),
        }
    }
    /// Inspectable domain failure, distinct from malformed requests.
    pub fn error(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            is_error: true,
            structured_content: None,
            diagnostics: Vec::new(),
        }
    }
    /// Serialize typed data without coercing nonfinite floats or nonportable integers.
    /// The registered output schema and codec are checked before successful delivery.
    pub fn structured<T: Serialize>(value: T, text: impl Into<String>) -> Result<Self, Error> {
        Ok(Self { structured_content: Some(values::encode(&value)?), ..Self::text(text) })
    }
    /// Attach validated domain diagnostics; a bounded plain-text summary is added at delivery.
    pub fn with_diagnostics(mut self, diagnostics: Vec<Diagnostic>) -> Result<Self, Error> {
        diagnostic::wire(&diagnostics).map_err(|_| Error::internal())?;
        self.diagnostics = diagnostics;
        Ok(self)
    }
    pub(crate) fn wire(mut self) -> Result<Value, Error> {
        let metadata = if self.diagnostics.is_empty() {
            Value::Null
        } else {
            let (values, summary) = diagnostic::wire(&self.diagnostics).map_err(|_| Error::internal())?;
            if !self.text.is_empty() { self.text.push('\n'); }
            self.text.push_str(&summary);
            json!({diagnostic::DIAGNOSTICS_METADATA_KEY: values})
        };
        protocol::bounded_value(&metadata, 64 * 1024)?;
        if self.text.len() > MAX_TEXT_BYTES {
            return Err(Error::internal());
        }
        let mut result = json!({"content":[{"type":"text","text":self.text}],"is_error":self.is_error,"metadata":metadata});
        if let Some(value) = self.structured_content {
            protocol::bounded_value(&value, 256 * 1024)?;
            result["structured_content"] = value;
        }
        // Reserve space for the JSON-RPC envelope and its bounded request ID.
        if serde_json::to_vec(&result).map_err(|_| Error::internal())?.len() > MAX_FRAME_BYTES - 2048 {
            return Err(Error::internal());
        }
        Ok(result)
    }
}
impl From<String> for ToolResult {
    fn from(s: String) -> Self {
        Self::text(s)
    }
}
impl From<&str> for ToolResult {
    fn from(s: &str) -> Self {
        Self::text(s)
    }
}

#[derive(Default)]
pub(crate) struct Terminal {
    pub cancelled: bool,
    pub settled: bool,
    pub progress_sequence: u64,
}

/// Per-call host context and cooperative cancellation. Cloneable;
/// native resource and bulk helpers remain scoped to the active handler lane.
#[derive(Clone)]
pub struct CallContext {
    pub(crate) terminal: Arc<Mutex<Terminal>>,
    host_context: Value,
    progress: Option<(Value, protocol::Writer)>,
    resources: Option<Arc<resource::Call>>,
    bulk: Option<Arc<bulk::Call>>,
}
impl CallContext {
    /// True once cancellation wins, including shutdown/transport loss.
    pub fn is_cancelled(&self) -> bool {
        self.terminal.lock().unwrap().cancelled
    }
    /// Check before/between effects. Cancellation does not undo previous effects.
    pub fn check_cancelled(&self) -> Result<(), Error> {
        if self.is_cancelled() {
            Err(Error::cancelled())
        } else {
            Ok(())
        }
    }
    /// An interruptible wait, polling at most every 10 ms.
    pub fn wait(&self, duration: Duration) -> Result<(), Error> {
        let deadline = std::time::Instant::now() + duration;
        loop {
            self.check_cancelled()?;
            let left = deadline.saturating_duration_since(std::time::Instant::now());
            if left.is_zero() {
                return Ok(());
            }
            std::thread::sleep(left.min(Duration::from_millis(10)));
        }
    }
    /// Opaque host-issued context, never inferred from model arguments.
    /// No reverse host-service authority is granted by this value.
    pub fn host_context(&self) -> &Value {
        &self.host_context
    }
    /// Whether this invocation negotiated progress. This does not imply that a
    /// cancelled or settled invocation may still emit it.
    pub fn supports_progress(&self) -> bool {
        self.progress.is_some()
    }
    /// Emit a bounded ephemeral status, only while negotiated and active.
    /// Sequences begin at one. Progress never extends a deadline or becomes output.
    pub fn progress(&self, message: impl Into<String>) -> Result<u64, Error> {
        self.progress_status(message, None, None, None)
    }
    /// Status with optional determinate portable counters and a unit label.
    pub fn progress_status(&self, message: impl Into<String>, current: Option<u64>, total: Option<u64>, unit: Option<&str>) -> Result<u64, Error> {
        let (id, writer) = self.progress.as_ref().ok_or_else(|| Error::rpc(-32601, "request_progress was not negotiated"))?;
        let message = message.into();
        if message.len() > 8192 || unit.is_some_and(|u| u.len() > 256)
            || current.into_iter().chain(total).any(|n| n > values::MAX_INTEGER as u64) {
            return Err(Error::invalid("progress status exceeds bounds"));
        }
        let mut terminal = self.terminal.lock().unwrap();
        if terminal.cancelled { return Err(Error::cancelled()); }
        if terminal.settled { return Err(Error::invalid("progress requires an active request")); }
        let sequence = terminal.progress_sequence.checked_add(1).ok_or_else(Error::internal)?;
        let mut event = json!({"type":"status","message":message});
        if let Some(value) = current { event["current"] = value.into(); }
        if let Some(value) = total { event["total"] = value.into(); }
        if let Some(value) = unit { event["unit"] = value.into(); }
        protocol::send(writer, json!({"jsonrpc":"2.0","method":"$/progress","params":{"request_id":id,"sequence":sequence,"event":event}}))?;
        terminal.progress_sequence = sequence;
        Ok(sequence)
    }
    pub(crate) fn cancel(&self) {
        let mut terminal = self.terminal.lock().unwrap();
        if !terminal.settled {
            terminal.cancelled = true;
        }
    }
}

type Handler = dyn Fn(Value, CallContext) -> Result<ToolResult, Error> + Send + Sync;
pub(crate) struct Tool {
    pub definition: Value,
    pub handler: Arc<Handler>,
}

/// Static tools and an API 0.4 stdio runtime. One admitted domain call at a time.
#[derive(Default)]
pub struct Extension {
    pub(crate) tools: BTreeMap<String, Arc<Tool>>,
    progress_disabled: bool,
}
impl Extension {
    /// Construct an exact API 0.4 tool-only process. No fallback to earlier wires.
    pub fn new() -> Self {
        Self::default()
    }
    /// Enable or decline optional request progress during initialization.
    ///
    /// Enabled by default. Set to `false` before `run` for a deliberately quiet
    /// extension. Progress helpers then refuse without emitting notifications.
    /// A host requiring progress (rather than offering it optionally) is rejected;
    /// cancellation, typed results, resources and blobs are unaffected.
    pub fn request_progress(&mut self, enabled: bool) -> &mut Self {
        self.progress_disabled = !enabled;
        self
    }
    /// Generate a legitimate input schema from a typed Serde/Schemars struct.
    /// Unsupported schema vocabulary/recursive types fail at registration.
    pub fn tool<I, F>(
        &mut self,
        name: &str,
        description: &str,
        handler: F,
    ) -> Result<&mut Self, Error>
    where
        I: DeserializeOwned + JsonSchema + 'static,
        F: Fn(I, CallContext) -> Result<ToolResult, Error> + Send + Sync + 'static,
    {
        let schema = schema::generated::<I>()?;
        self.add_tool(name, description, schema, move |value, context| {
            let input = serde_json::from_value::<I>(value)
                .map_err(|_| Error::invalid("arguments do not match the Rust input type"))?;
            handler(input, context)
        })?;
        Ok(self)
    }
    /// Register strict typed input and output contracts, keeping an explicit text projection.
    /// Serde defaults and Option fields control missing/default/null behavior.
    /// Unsupported schema constructs fail before this tool is registered.
    pub fn typed_tool<I, O, F>(&mut self, name: &str, description: &str, handler: F) -> Result<&mut Self, Error>
    where
        I: DeserializeOwned + Serialize + JsonSchema + 'static,
        O: DeserializeOwned + JsonSchema + 'static,
        F: Fn(I, CallContext) -> Result<ToolResult, Error> + Send + Sync + 'static,
    {
        self.typed_registration::<I, O, F>(name, description, None, false, handler)
    }
    /// Opt into operation discovery; the optional receiver is presentation-only.
    /// The tool name is the operation id; all resource slots come from I and O.
    pub fn operation<I, O, F>(&mut self, name: &str, description: &str, receiver: Option<&str>, handler: F) -> Result<&mut Self, Error>
    where
        I: DeserializeOwned + Serialize + JsonSchema + 'static,
        O: DeserializeOwned + JsonSchema + 'static,
        F: Fn(I, CallContext) -> Result<ToolResult, Error> + Send + Sync + 'static,
    {
        self.typed_registration::<I, O, F>(name, description, receiver, true, handler)
    }
    fn typed_registration<I, O, F>(&mut self, name: &str, description: &str, receiver: Option<&str>, explicit: bool, handler: F) -> Result<&mut Self, Error>
    where
        I: DeserializeOwned + Serialize + JsonSchema + 'static,
        O: DeserializeOwned + JsonSchema + 'static,
        F: Fn(I, CallContext) -> Result<ToolResult, Error> + Send + Sync + 'static,
    {
        let input = schema::typed_generated::<I>(true)?;
        let output = schema::typed_generated::<O>(false)?;
        let operation = resource::operation(name, &input, &output, receiver, explicit)?;
        let check = output.clone();
        self.add_tool_with_output(name, description, input, Some(output), operation, move |value, context| {
            values::encode(&value).map_err(|_| Error::invalid("arguments contain a nonportable scalar"))?;
            let input = serde_json::from_value::<I>(value).map_err(|_| Error::invalid("arguments do not match the Rust input type"))?;
            // Defaults and concrete float narrowing must also remain finite/portable.
            values::encode(&input).map_err(|_| Error::invalid("decoded input contains a nonportable scalar"))?;
            let result = handler(input, context)?;
            if result.text.trim().is_empty() || result.text.len() > 4096 || result.text.chars().any(|c| c.is_control() && c != '\n' && c != '\t') {
                return Err(Error::internal());
            }
            if let Some(value) = &result.structured_content {
                schema::arguments(&check, value).map_err(|_| Error::internal())?;
                serde_json::from_value::<O>(value.clone()).map_err(|_| Error::internal())?;
            } else if !result.is_error {
                return Err(Error::internal());
            }
            Ok(result)
        })?;
        Ok(self)
    }
    pub(crate) fn add_tool<F>(
        &mut self,
        name: &str,
        description: &str,
        parameters: Value,
        handler: F,
    ) -> Result<(), Error>
    where
        F: Fn(Value, CallContext) -> Result<ToolResult, Error> + Send + Sync + 'static,
    {
        self.add_tool_with_output(name, description, parameters, None, None, handler)
    }
    fn add_tool_with_output<F>(&mut self, name: &str, description: &str, parameters: Value, output: Option<Value>, operation: Option<Value>, handler: F) -> Result<(), Error>
    where F: Fn(Value, CallContext) -> Result<ToolResult, Error> + Send + Sync + 'static,
    {
        if self.tools.len() >= MAX_TOOLS || self.tools.contains_key(name) {
            return Err(Error::invalid("duplicate tool or tool catalog full"));
        }
        if name.is_empty()
            || name.len() > 64
            || !name.bytes().enumerate().all(|(i, b)| {
                b.is_ascii_alphabetic()
                    || b == b'_'
                    || (i > 0 && (b.is_ascii_digit() || b == b'-' || b == b'.'))
            })
        {
            return Err(Error::invalid("invalid tool identifier"));
        }
        if description.trim().is_empty() || description.len() > 4096 {
            return Err(Error::invalid("description must be 1..4096 bytes"));
        }
        schema::definition(&parameters)?;
        let mut definition = json!({"name":name,"description":description,"parameters":parameters});
        if let Some(output) = output { definition["output_schema"] = output; }
        if let Some(operation) = operation { definition["operation"] = operation; }
        self.tools.insert(
            name.into(),
            Arc::new(Tool {
                definition,
                handler: Arc::new(handler),
            }),
        );
        // Initialization must fit one bounded frame; registration remains atomic.
        if serde_json::to_vec(&self.catalog())
            .map_err(|_| Error::internal())?
            .len()
            > MAX_FRAME_BYTES - 4096
        {
            self.tools.remove(name);
            return Err(Error::invalid(
                "complete catalog exceeds initialization frame bound",
            ));
        }
        Ok(())
    }
    pub(crate) fn catalog(&self) -> Vec<Value> {
        self.tools.values().map(|t| t.definition.clone()).collect()
    }
    /// Own process stdin/stdout until shutdown/EOF. May be called only once per
    /// process. An uncooperative handler at drain expiry terminates this executable
    /// (exit 70), rather than returning while foreign callbacks still borrow data.
    pub fn run(self) -> Result<(), Error> {
        protocol::run(self)
    }
}

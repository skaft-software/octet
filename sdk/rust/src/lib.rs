//! A source-only, tool-authoring SDK for supervised API 0.4 executable extensions.
//! This is neither the native-host embedding protocol nor an in-process plugin ABI.
//! Rust, C and C++ use the same framing, negotiation, validation and lifecycle.
pub mod ffi;
mod protocol;
mod schema;

use std::collections::BTreeMap;
use std::fmt;
use std::sync::{Arc, Mutex};
use std::time::Duration;

pub use schemars::JsonSchema;
use serde::de::DeserializeOwned;
pub use serde::Deserialize;
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

/// A text-only result. Rich/structured results are deliberately not exposed yet.
#[derive(Debug)]
pub struct ToolResult {
    text: String,
    is_error: bool,
}
impl ToolResult {
    /// Successful model-visible text.
    pub fn text(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            is_error: false,
        }
    }
    /// Inspectable domain failure, distinct from malformed requests.
    pub fn error(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            is_error: true,
        }
    }
    pub(crate) fn wire(self) -> Result<Value, Error> {
        if self.text.len() > MAX_TEXT_BYTES {
            return Err(Error::internal());
        }
        Ok(
            json!({"content":[{"type":"text","text":self.text}],"is_error":self.is_error,"metadata":null}),
        )
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
}

/// Per-call host context and cooperative cancellation. Cloneable, read-only.
#[derive(Clone)]
pub struct CallContext {
    pub(crate) terminal: Arc<Mutex<Terminal>>,
    host_context: Value,
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
}
impl Extension {
    /// Construct an exact API 0.4 tool-only process. No fallback to earlier wires.
    pub fn new() -> Self {
        Self::default()
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
        let definition = json!({"name":name,"description":description,"parameters":parameters});
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

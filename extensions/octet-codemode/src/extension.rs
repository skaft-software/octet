//! Optional API 0.4 composition adapter. The host remains the only tool/effect
//! authority; each script runs in a disposable copy of this executable.
mod boundaries;
mod image;
mod transport;

use anyhow::{anyhow, bail, ensure, Context, Result};
use base64::{engine::general_purpose::STANDARD, Engine};
use boundaries::*;
use serde_json::{json, Value};
use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};
use tokio::sync::{oneshot, Mutex, Notify, Semaphore};
use tokio::task::{JoinHandle, JoinSet, LocalSet};
use tokio::time::Instant;
use transport::{Delivery, Writer};

#[derive(Debug)]
struct Cancelled;
impl std::fmt::Display for Cancelled {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Request cancelled")
    }
}
impl std::error::Error for Cancelled {}
#[derive(Debug)]
struct TimedOut;
impl std::fmt::Display for TimedOut {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Script timed out")
    }
}
impl std::error::Error for TimedOut {}

#[derive(Default)]
struct Cancellation {
    cancelled: Cell<bool>,
    notify: Notify,
}
impl Cancellation {
    fn cancel(&self) {
        self.cancelled.set(true);
        self.notify.notify_waiters();
    }
    async fn cancelled(&self) {
        // All token observers run on this LocalSet, so no cancellation can land
        // between this check and registering the notification future.
        if !self.cancelled.get() {
            self.notify.notified().await;
        }
    }
    fn check(&self) -> Result<()> {
        if self.cancelled.get() {
            return Err(Cancelled.into());
        }
        Ok(())
    }
}
async fn bounded<T>(
    cancel: &Cancellation,
    deadline: Instant,
    work: impl Future<Output = Result<T>>,
) -> Result<T> {
    tokio::select! {
        biased;
        _ = cancel.cancelled() => Err(Cancelled.into()),
        _ = tokio::time::sleep_until(deadline) => Err(TimedOut.into()),
        value = work => value,
    }
}
async fn checkpoint(cancel: &Cancellation, deadline: Instant) -> Result<()> {
    tokio::task::yield_now().await;
    cancel.check()?;
    if Instant::now() >= deadline {
        return Err(TimedOut.into());
    }
    Ok(())
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Stage {
    Starting,
    Ready,
    Draining,
    Stopped,
}
struct Active {
    cancel: Rc<Cancellation>,
    task: JoinHandle<()>,
    sequence: u64,
}
struct Pending {
    parent: u64,
    method: &'static str,
    delivery: Arc<Delivery>,
    response: oneshot::Sender<Result<Value>>,
}
struct Runtime {
    engine: String,
    image: image::LaunchImage,
    stage: Cell<Stage>,
    lost: Cell<bool>,
    active: RefCell<HashMap<u64, Active>>,
    pending: RefCell<HashMap<String, Pending>>,
    seen: RefCell<HashSet<u64>>,
    counter: Cell<u64>,
    config: RefCell<Option<Negotiated>>,
    writer: Writer,
    serial: Mutex<()>,
    running: Cell<bool>,
    scratch: RefCell<Scratch>,
    /// Warm disposable runner: spawned on first use, reused while scripts
    /// complete cleanly, and killed (never reused) after any abnormal end.
    runner: RefCell<Option<RunnerHandle>>,
}

/// One warm runner process. The runner compiles its Wasm module once and
/// instantiates a fresh isolated store for each script; the adapter only keeps
/// this handle while the last script ended with a clean completion event.
struct RunnerHandle {
    child: Child,
    input: Rc<Mutex<ChildStdin>>,
    output: BufReader<ChildStdout>,
    script: u64,
}

impl RunnerHandle {
    /// Spawn the runner and wait for its one-time module preparation.
    async fn spawn(image: &Path, engine: &str) -> Result<Self> {
        let mut child = Command::new(image)
            .arg(engine)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()?;
        let input = Rc::new(Mutex::new(child.stdin.take().unwrap()));
        let mut output = BufReader::new(child.stdout.take().unwrap());
        let warm = read_runner(&mut output)
            .await
            .context("Runner exited before preparing its engine")?;
        ensure!(
            warm == br#"{"type":"warm"}"#,
            "Runner did not prepare an engine"
        );
        Ok(Self {
            child,
            input,
            output,
            script: 0,
        })
    }
    /// Start one script. The runner instantiates a fresh isolated store before
    /// acknowledging, so the previous script's VM is gone by this point.
    async fn start(&mut self, context: &Value, code: &str, timeout_ms: u64) -> Result<()> {
        self.script += 1;
        send_runner_frame(
            &self.input,
            &json!({"type":"script","script":self.script,"context":context,"code":code,
                "timeout_ms":timeout_ms,"heap_bytes":VM_HEAP_BYTES}),
        )
        .await?;
        let ready = read_runner(&mut self.output)
            .await
            .context("Runner exited before acknowledging setup")?;
        ensure!(
            ready == br#"{"type":"ready"}"#,
            "Runner did not acknowledge setup"
        );
        // The VM exists before the script body starts: a stale epoch from the
        // previous script is dropped by the runner, never executed here.
        send_runner_frame(
            &self.input,
            &json!({"type":"message","script":self.script,
                "payload":json!({"type":"start"}).to_string()}),
        )
        .await?;
        Ok(())
    }
    async fn kill(mut self) -> Result<std::process::ExitStatus> {
        let _ = self.child.start_kill();
        Ok(self.child.wait().await?)
    }
}
// Dropping any cancelled/expired reverse waiter synchronously revokes its
// queued writer entry; sent requests get exactly one cancel after that frame.
struct ReverseWait {
    runtime: Rc<Runtime>,
    id: String,
}
impl Drop for ReverseWait {
    fn drop(&mut self) {
        self.runtime.abandon(&self.id);
    }
}
impl Runtime {
    fn feature(&self, feature: &str) -> bool {
        self.config
            .borrow()
            .as_ref()
            .is_some_and(|c| c.features.contains(feature))
    }
    fn send(&self, value: Value) -> Result<()> {
        if self.lost.get() {
            return Ok(());
        }
        self.writer.send(&value, None)
    }
    /// Kill any warm runner. Called once on every terminal extension path; an
    /// in-flight script's runner is already killed by its dropped handle.
    async fn close_runner(&self) {
        let runner = self.runner.borrow_mut().take();
        if let Some(runner) = runner {
            let _ = runner.kill().await;
        }
    }
    fn error(&self, id: Value, code: i64, message: &str) {
        let _ = self.send(
            json!({"jsonrpc":"2.0","id":id,"error":{"code":code,"message":head(message,4096)}}),
        );
    }
    fn progress(&self, parent: u64, message: &str) -> Result<()> {
        if !self.feature("request_progress") {
            return Ok(());
        }
        let mut active = self.active.borrow_mut();
        if let Some(parent_state) = active.get_mut(&parent) {
            if !parent_state.cancel.cancelled.get() {
                parent_state.sequence += 1;
                self.send(json!({"jsonrpc":"2.0","method":"$/progress","params":{"request_id":parent,"sequence":parent_state.sequence,"event":{"type":"status","message":message}}}))?;
            }
        }
        Ok(())
    }
    async fn request(
        self: &Rc<Self>,
        parent: u64,
        method: &'static str,
        mut fields: Value,
    ) -> Result<Value> {
        ensure!(
            self.stage.get() == Stage::Ready && !self.lost.get(),
            "Composition transport unavailable"
        );
        self.active
            .borrow()
            .get(&parent)
            .context("Composition parent is not active")?
            .cancel
            .check()?;
        if self.counter.get() >= 65536 || self.pending.borrow().len() >= 128 {
            return Err(RpcError(
                -32002,
                "Reverse request ID/pending limit exhausted; reload the extension".into(),
            )
            .into());
        }
        self.counter.set(self.counter.get() + 1);
        let id = format!("codemode:{}", self.counter.get());
        let (response, receiver) = oneshot::channel();
        let delivery = Delivery::new();
        self.pending.borrow_mut().insert(
            id.clone(),
            Pending {
                parent,
                method,
                delivery: delivery.clone(),
                response,
            },
        );
        let _wait = ReverseWait {
            runtime: self.clone(),
            id: id.clone(),
        };
        fields["parent_request_id"] = json!(parent);
        self.writer.send(
            &json!({"jsonrpc":"2.0","id":id,"method":method,"params":fields}),
            Some(delivery),
        )?;
        receiver.await.unwrap_or_else(|_| Err(Cancelled.into()))
    }
    fn abandon(&self, id: &str) {
        let slot = self.pending.borrow_mut().remove(id);
        if let Some(slot) = slot {
            if slot.delivery.abandon() {
                let _ = self.send(json!({"jsonrpc":"2.0","method":"$/cancelRequest","params":{"id":id,"reason":"cancelled"}}));
            }
        }
    }
    fn cancel_parent(&self, parent: u64) {
        if let Some(active) = self.active.borrow().get(&parent) {
            active.cancel.cancel();
        }
        self.abandon_parent_requests(parent);
    }
    fn abandon_parent_requests(&self, parent: u64) {
        let ids: Vec<String> = self
            .pending
            .borrow()
            .iter()
            .filter(|(_, p)| p.parent == parent)
            .map(|(id, _)| id.clone())
            .collect();
        for id in ids {
            self.abandon(&id);
        }
    }
    fn cancel(&self, fields: &Value) {
        if !exact(fields, &["id", "reason"])
            || !valid_id(&fields["id"])
            || fields
                .get("reason")
                .is_some_and(|r| r.as_str().is_none_or(|r| r.len() > 4096))
        {
            return;
        }
        let parent = match fields["id"].as_str() {
            Some(id) => self.pending.borrow().get(id).map(|p| p.parent),
            None => fields["id"].as_u64(),
        };
        if let Some(parent) = parent {
            self.cancel_parent(parent);
        }
    }
    fn response(&self, message: &Value) -> Result<()> {
        let id = message["id"]
            .as_str()
            .context("Unknown reverse response ID")?;
        let slot = self.pending.borrow_mut().remove(id);
        let Some(slot) = slot else {
            let sequence = id
                .strip_prefix("codemode:")
                .filter(|s| !s.starts_with('0') && s.bytes().all(|b| b.is_ascii_digit()))
                .and_then(|s| s.parse::<u64>().ok());
            ensure!(
                sequence.is_some_and(|n| n > 0 && n <= self.counter.get()),
                "Unknown reverse response ID"
            );
            // Late sidecars still belong to this process's private scratch. A
            // late value is never delivered to a new/settled parent.
            if let Some(result) = message.get("result") {
                for key in ["context_file", "value_file"] {
                    if let Some(reference) = result.get(key) {
                        let _ = read_host_file(reference, &self.scratch.borrow().root);
                    }
                }
            }
            return Ok(());
        };
        slot.delivery.settle();
        let value = if let Some(error) = message.get("error") {
            let code = error["code"].as_i64().unwrap();
            if code == -32800 {
                // Revocation cannot become a catchable guest rejection followed
                // by a success-store commit.
                self.cancel_parent(slot.parent);
                Err(Cancelled.into())
            } else {
                Err(RpcError(code, error["message"].as_str().unwrap().into()).into())
            }
        } else {
            let result = message["result"].clone();
            // Decode/unlink before handing off to a possibly cancelled waiter.
            // This also closes the response-delivery/cancellation sidecar race.
            match slot.method {
                "composition/context" => {
                    host_result(result, &self.scratch.borrow().root, "context")
                }
                "composition/call" => host_result(result, &self.scratch.borrow().root, "value"),
                _ => Ok(result),
            }
        };
        let _ = slot.response.send(value);
        Ok(())
    }
    fn status(&self) -> String {
        let config = self.config.borrow();
        let config = config.as_ref().unwrap();
        format!("QuickJS {} engine · mode {} · inline budget {}\nOne active script, one warm runner, a fresh isolated VM per script; four concurrent nested calls, 256 calls, 256 MiB VM heap, <=25 s local / 30 s host deadline.\nOutput <=50 KiB; successful store writes are branch-scoped. Models helpers are unavailable. No Node or Python runtime is required.", self.engine, config.mode, config.inline_budget)
    }
    async fn dispatch(
        self: Rc<Self>,
        id: u64,
        method: String,
        fields: Value,
        cancel: Rc<Cancellation>,
    ) {
        let result: Result<Value> = async {
            cancel.check()?;
            if method == "initialize" {
                params(self.stage.get() == Stage::Starting, "initialize may run only once")?;
                let negotiated = negotiate(&fields)?;
                let result = negotiated.result.clone();
                *self.config.borrow_mut() = Some(negotiated);
                self.stage.set(Stage::Ready);
                return Ok(result);
            }
            if self.stage.get() != Stage::Ready { return Err(RpcError(-32002,"initialize is required before requests".into()).into()); }
            match method.as_str() {
                "tool/call" => {
                    params(exact(&fields, &["name","arguments","context"]) && fields["name"] == "codemode" && fields["context"].is_object(), "expected codemode tool/call with arguments and context")?;
                    let source = source(&fields["arguments"])?;
                    let _serial = tokio::select! { biased; _ = cancel.cancelled() => return Err(Cancelled.into()), lock = self.serial.lock() => lock };
                    self.running.set(true);
                    let result = self.execute(id, source, cancel.clone()).await;
                    self.running.set(false);
                    result
                },
                "command/execute" => {
                    params(exact(&fields, &["name","arguments","context"]) && fields["name"] == "codemode" && fields["context"].is_object()
                        && fields["arguments"].as_array().is_some_and(|a| a.len() <= 1 && a.iter().all(|a| a == "status" || a == "help")), "codemode command accepts status or help")?;
                    let mut text = self.status();
                    if fields["arguments"] == json!(["help"]) { text += "\n\n"; text += DESCRIPTION; }
                    Ok(json!({"text":text,"notifications":[],"context":[]}))
                },
                "menu/collect" => {
                    params(exact(&fields, &["context"]) && fields["context"].is_object(), "menu context required")?;
                    Ok(json!({"title":"Codemode","status":{"state":if self.running.get() {"running"} else {"active"},"label":if self.running.get() {"Script running"} else {"Ready"}},"detail":self.status(),
                        "items":[{"id":"status","label":"Status and limits","command":"codemode","arguments":["status"],"recommended":true},{"id":"help","label":"JavaScript help","command":"codemode","arguments":["help"]}]}))
                },
                _ => Err(RpcError(-32601,"Method not found".into()).into()),
            }
        }.await;
        if cancel.cancelled.get() || result.as_ref().is_err_and(|e| e.is::<Cancelled>()) {
            self.error(json!(id), -32800, "Request cancelled");
        } else {
            match result
                .and_then(|result| self.send(json!({"jsonrpc":"2.0","id":id,"result":result})))
            {
                Ok(()) => {}
                Err(error) => self.error(
                    json!(id),
                    error.downcast_ref::<RpcError>().map_or(-32603, |e| e.0),
                    &error.to_string(),
                ),
            }
        }
        self.active.borrow_mut().remove(&id);
    }
    async fn frame(self: &Rc<Self>, data: &[u8]) -> Result<Option<u64>> {
        let message = match loads(data) {
            Ok(m) => m,
            Err(_) => {
                self.error(Value::Null, -32700, "Parse error");
                return Ok(None);
            }
        };
        let id = if valid_id(&message["id"]) {
            message["id"].clone()
        } else {
            Value::Null
        };
        if !message.is_object() || message["jsonrpc"] != "2.0" {
            self.error(id, -32600, "Invalid Request");
            return Ok(None);
        }
        if message.get("method").is_none() {
            ensure!(
                exact(&message, &["jsonrpc", "id", "result", "error"])
                    && !id.is_null()
                    && message.get("result").is_some() != message.get("error").is_some(),
                "Invalid reverse response envelope"
            );
            if let Some(error) = message.get("error") {
                ensure!(
                    exact(error, &["code", "message", "data"])
                        && error["code"].as_i64().is_some()
                        && error["message"].is_string(),
                    "Invalid reverse error"
                );
            }
            self.response(&message)?;
            return Ok(None);
        }
        if !exact(&message, &["jsonrpc", "id", "method", "params"])
            || !message["method"].is_string()
            || !message["params"].is_object()
        {
            self.error(id, -32600, "Invalid Request");
            return Ok(None);
        }
        let method = message["method"].as_str().unwrap();
        let fields = &message["params"];
        if message.get("id").is_none() {
            if method == "$/cancelRequest" && self.feature("request_cancellation") {
                self.cancel(fields);
            }
            return Ok(None);
        }
        if !integer(&id, 0, SAFE_INTEGER) {
            self.error(
                id,
                -32600,
                "Host request ID must be an unsigned portable integer",
            );
            return Ok(None);
        }
        let id = id.as_u64().unwrap();
        if self.seen.borrow().contains(&id) || self.seen.borrow().len() >= 65536 {
            self.error(json!(id), -32600, "Duplicate/exhausted host request ID");
            return Ok(None);
        }
        self.seen.borrow_mut().insert(id);
        if method == "shutdown" {
            if fields.as_object().unwrap().is_empty() {
                return Ok(Some(id));
            }
            self.error(
                json!(id),
                -32602,
                "Invalid params: shutdown takes no fields",
            );
            return Ok(None);
        }
        let concurrent = self.config.borrow().as_ref().map_or(8, |c| c.concurrent);
        if matches!(self.stage.get(), Stage::Draining | Stage::Stopped)
            || self.active.borrow().len() >= concurrent
        {
            self.error(
                json!(id),
                -32002,
                "Extension draining or negotiated concurrency exhausted",
            );
            return Ok(None);
        }
        let cancel = Rc::new(Cancellation::default());
        let task = tokio::task::spawn_local(self.clone().dispatch(
            id,
            method.into(),
            fields.clone(),
            cancel.clone(),
        ));
        self.active.borrow_mut().insert(
            id,
            Active {
                cancel,
                task,
                sequence: 0,
            },
        );
        // Initialization is visible before the next already-buffered host frame.
        tokio::task::yield_now().await;
        Ok(None)
    }

    async fn execute(
        self: &Rc<Self>,
        parent: u64,
        source: Source,
        cancel: Rc<Cancellation>,
    ) -> Result<Value> {
        let started = Instant::now();
        let mut timeout_ms = source.timeout_ms;
        let mut max_calls = MAX_CALLS;
        let mut deadline = started + Duration::from_millis(timeout_ms);
        let mut captured = Vec::new();
        let calls = Rc::new(RefCell::new(Vec::new()));
        let mut items = Vec::new();
        let mut ok = false;
        let mut error = None;
        let result: Result<()> = async {
            self.progress(parent, "Preparing frozen tool snapshot")?;
            let context = bounded(&cancel, deadline, self.request(parent, "composition/context", json!({}))).await?;
            validate_context(&context)?;
            timeout_ms = timeout_ms.min(context["limits"]["timeout_ms"].as_u64().unwrap());
            max_calls = max_calls.min(context["limits"]["max_calls"].as_u64().unwrap());
            deadline = started + Duration::from_millis(timeout_ms);
            checkpoint(&cancel, deadline).await?;
            self.progress(parent, &format!("Running JavaScript ({}; deadline {timeout_ms} ms, up to {max_calls} calls)", self.engine))?;
            let done = self
                .guest(parent, &source.code, context, cancel.clone(), deadline, max_calls, &mut captured, calls.clone())
                .await?;
            ok = done["ok"].as_bool().unwrap();
            if ok {
                if let Some(value) = done.get("value") {
                    let value = loads(value.as_str().unwrap().as_bytes())?;
                    captured.push(json!({"type":"text", "text":value.as_str().map(str::to_owned).unwrap_or_else(|| value.to_string())}));
                }
            } else {
                let mut failure = loads(done["error"].as_str().unwrap_or("{\"message\":\"Guest execution failed\"}").as_bytes())?;
                ensure!(failure.is_object(), "Invalid guest error");
                if failure.get("kind").is_none() { failure["kind"] = json!("script"); }
                error = Some(failure);
            }
            items = captured.iter().filter(|i| i["type"] == "text").cloned().collect();
            for image in captured.iter().filter(|i| i["type"] == "image") {
                checkpoint(&cancel, deadline).await?;
                match bounded(&cancel, deadline, self.publish(parent, image, &cancel, deadline)).await {
                    Ok(image) => items.push(image),
                    Err(failure) if failure.is::<Cancelled>() || failure.is::<TimedOut>() => return Err(failure),
                    Err(failure) => { ok = false; error = Some(json!({"kind":"artifact","message":format!("Image publication failed: {failure}")})); break; }
                }
            }
            if ok {
                let writes = loads(done["writes"].as_str().unwrap_or("[]").as_bytes())?;
                ensure!(writes.as_array().is_some_and(|a| a.len() <= 4096), "Invalid guest store writes");
                let mut sets = serde_json::Map::new();
                let mut deletes = Vec::new();
                for row in writes.as_array().unwrap() {
                    ensure!(row.as_array().is_some_and(|a| (1..=2).contains(&a.len()) && a[0].as_str().is_some_and(|s| s.len() <= 1024)), "Invalid guest store write");
                    let row = row.as_array().unwrap();
                    let key = row[0].as_str().unwrap();
                    if row.len() == 1 { deletes.push(key); }
                    else {
                        let value = row[1].as_str().context("Invalid guest store value")?;
                        ensure!(value.len() <= 256 * 1024, "Store value exceeds 256 KiB");
                        sets.insert(key.into(), loads(value.as_bytes())?);
                    }
                }
                if !sets.is_empty() || !deletes.is_empty() {
                    checkpoint(&cancel, deadline).await?;
                    self.progress(parent, "Persisting successful branch-scoped store writes")?;
                    let ack = bounded(&cancel, deadline, self.request(parent,"composition/store",json!({"set":sets,"delete":deletes}))).await?;
                    ensure!(ack == json!({}), "Invalid composition/store acknowledgement");
                }
            }
            Ok(())
        }.await;
        if let Err(failure) = result {
            if failure.is::<Cancelled>() {
                return Err(failure);
            }
            ok = false;
            error = Some(if failure.is::<TimedOut>() {
                json!({"kind":"timeout","message":format!("Script timed out after {timeout_ms} ms")})
            } else {
                json!({"kind":"sandbox","message":failure.to_string()})
            });
        }
        cancel.check()?;
        if items.is_empty() {
            items = captured
                .into_iter()
                .filter(|i| i["type"] == "text")
                .collect();
        }
        let calls = std::mem::take(&mut *calls.borrow_mut());
        Ok(format_result(
            Output {
                ok,
                items,
                error,
                calls,
                timeout_ms,
                max_calls,
                tokens: source.tokens,
                elapsed: started.elapsed().as_secs_f64(),
            },
            &mut self.scratch.borrow_mut(),
        ))
    }
    async fn publish(
        self: &Rc<Self>,
        parent: u64,
        item: &Value,
        cancel: &Cancellation,
        deadline: Instant,
    ) -> Result<Value> {
        ensure!(
            self.feature("artifacts"),
            "image() requires the host's optional artifacts feature"
        );
        let mime = item["mimeType"]
            .as_str()
            .context("Unsupported image MIME type")?;
        ensure!(
            ["image/png", "image/jpeg", "image/gif", "image/webp"].contains(&mime),
            "Unsupported image MIME type"
        );
        let encoded = item["data"].as_str().context("Invalid image data")?;
        let data = STANDARD.decode(encoded)?;
        ensure!(
            !data.is_empty() && data.len() <= 20 * MIB && STANDARD.encode(&data) == encoded,
            "Invalid or oversized image data"
        );
        let mut fields = json!({"mime_type":mime,"size":data.len(),"sha256":digest(&data)});
        let _file = if data.len() <= 256 * 1024 {
            fields["data"] = json!({"encoding":"base64","data":encoded});
            None
        } else {
            let file = self.scratch.borrow_mut().write(&data, "image")?;
            fields["path"] = json!(file
                .path()
                .strip_prefix(&self.scratch.borrow().root)?
                .to_str()
                .context("Non-UTF-8 scratch path")?);
            Some(file) // Unlinked on success, rejection, cancellation or deadline.
        };
        // Decoding, hashing and scratch I/O are synchronous. Recheck after
        // preparation so they cannot carry publication past revocation/deadline.
        checkpoint(cancel, deadline).await?;
        let result = self.request(parent, "artifact/publish", fields).await?;
        ensure!(
            exact(&result, &["artifact_id"])
                && result["artifact_id"]
                    .as_str()
                    .is_some_and(|s| !s.is_empty() && s.len() <= 256),
            "Invalid artifact/publish response"
        );
        Ok(json!({"type":"image","artifact_id":result["artifact_id"],"mime_type":mime}))
    }

    #[allow(clippy::too_many_arguments)]
    async fn guest(
        self: &Rc<Self>,
        parent: u64,
        code: &str,
        context: Value,
        cancel: Rc<Cancellation>,
        deadline: Instant,
        max_calls: u64,
        captured: &mut Vec<Value>,
        calls: Rc<RefCell<Vec<Value>>>,
    ) -> Result<Value> {
        cancel.check()?;
        if Instant::now() >= deadline {
            return Err(TimedOut.into());
        }
        let tools = Rc::new(
            context["tools"]
                .as_array()
                .unwrap()
                .iter()
                .map(|t| {
                    (
                        t["name"].as_str().unwrap().to_owned(),
                        t.get("output_schema").is_some(),
                    )
                })
                .collect::<HashMap<_, _>>(),
        );
        let timeout_ms = deadline
            .saturating_duration_since(Instant::now())
            .as_millis()
            .max(1) as u64;
        let mut script = ScriptRun {
            runtime: self,
            parent,
            outer: Rc::clone(&cancel),
            deadline,
            timeout_ms,
            max_calls,
            tools,
            cancel: Rc::new(Cancellation::default()),
            limiter: Rc::new(Semaphore::new(4)),
            tasks: JoinSet::new(),
            calls,
            captured,
        };
        // A warm runner is spawned on first use and otherwise reused. A script
        // that ends without a clean `done` kills and reaps its runner here, so
        // no VM, pipe or process can outlive it.
        let warm = self.runner.borrow_mut().take();
        let mut runner = match warm {
            Some(runner) => runner,
            None => {
                let spawned = tokio::select! {
                    biased;
                    _ = cancel.cancelled() => Err(Cancelled.into()),
                    _ = tokio::time::sleep_until(deadline) => Err(TimedOut.into()),
                    spawned = RunnerHandle::spawn(&self.image.executable, &self.engine) => spawned,
                };
                match spawned {
                    Ok(runner) => runner,
                    Err(error) => {
                        cancel.check()?;
                        return Err(error);
                    }
                }
            }
        };
        let result = match script.drive(&mut runner, &context, code, timeout_ms).await {
            Ok(done) => {
                self.runner.borrow_mut().replace(runner);
                Ok(done)
            }
            Err(error) => {
                let _ = runner.kill().await;
                Err(error)
            }
        };
        // Revoke queued writer entries synchronously, before cleanup yields or
        // nested waiters are dropped. A successful outer parent stays live for
        // artifact publication/store commit, but its unfinished calls do not.
        self.abandon_parent_requests(parent);
        script.cancel.cancel();
        // Never select cancellation around cleanup or abort this task: join
        // calls, close pipes and wait even on repeated cancellation.
        let mut task_error = None;
        while let Some(joined) = script.tasks.join_next().await {
            if let Err(error) = joined {
                task_error = Some(anyhow!(error).context("Nested call task panicked"));
            }
        }
        cancel.check()?;
        if let Some(error) = task_error {
            return Err(error);
        }
        result
    }
}

/// One script's adapter-side state: its own reciprocal cancellation, nested-call
/// limiter and call ledger. Dropping it revokes nothing that already started.
struct ScriptRun<'a> {
    runtime: &'a Rc<Runtime>,
    parent: u64,
    outer: Rc<Cancellation>,
    deadline: Instant,
    timeout_ms: u64,
    max_calls: u64,
    tools: Rc<HashMap<String, bool>>,
    cancel: Rc<Cancellation>,
    limiter: Rc<Semaphore>,
    tasks: JoinSet<()>,
    calls: Rc<RefCell<Vec<Value>>>,
    captured: &'a mut Vec<Value>,
}

impl ScriptRun<'_> {
    /// Start one script and drive the warm runner until it reports a terminal
    /// event. Cancellation and the deadline are observed between frames; the
    /// script's VM is never left running by a dropped future.
    async fn drive(
        &mut self,
        runner: &mut RunnerHandle,
        context: &Value,
        code: &str,
        timeout_ms: u64,
    ) -> Result<Value> {
        runner.start(context, code, timeout_ms).await?;
        let script = runner.script;
        let input = runner.input.clone();
        let mut seen = HashSet::new();
        let mut count = 0;
        let mut output_bytes = 0;
        let mut images = 0;
        // The runner's interrupt stops JavaScript at the script deadline and
        // reports a timeout itself; only a wedged VM that never reaches an
        // interrupt check is killed at the grace boundary (the documented
        // process-kill fallback).
        let hard_deadline = self.deadline + Duration::from_millis(750);
        loop {
            let event = loads(&tokio::select! {
                biased;
                _ = self.outer.cancelled() => return Err(Cancelled.into()),
                _ = tokio::time::sleep_until(hard_deadline) => return Err(TimedOut.into()),
                event = read_runner(&mut runner.output) => event?,
            })?;
            ensure!(event.is_object(), "Invalid runner event");
            match event["type"]
                .as_str()
                .context("Invalid runner event type")?
            {
                "crash" => bail!(
                    "{}",
                    head(event["message"].as_str().unwrap_or("Runner crashed"), 4096)
                ),
                "call" => {
                    count += 1;
                    ensure!(
                        integer(&event["id"], 0, SAFE_INTEGER)
                            && seen.insert(event["id"].as_u64().unwrap()),
                        "Invalid/duplicate guest call ID"
                    );
                    ensure!(
                        count <= MAX_CALLS
                            && event
                                .get("args")
                                .is_none_or(|a| a.as_str().is_some_and(|s| s.len() <= FRAME_BYTES)),
                        "Script bridge limit exceeded (256 tool calls, 1 MiB call arguments)"
                    );
                    let runtime = Rc::clone(self.runtime);
                    let input = input.clone();
                    let cancel = self.cancel.clone();
                    let limiter = self.limiter.clone();
                    let tools = self.tools.clone();
                    let calls = self.calls.clone();
                    let (parent, deadline, max_calls) =
                        (self.parent, self.deadline, self.max_calls);
                    self.tasks.spawn_local(async move {
                        runtime
                            .settle(
                                parent, event, count, max_calls, tools, input, script, cancel,
                                limiter, deadline, calls,
                            )
                            .await;
                    });
                }
                "output" => {
                    let item = &event["item"];
                    ensure!(
                        item.is_object() && (item["type"] == "text" || item["type"] == "image"),
                        "Invalid guest output"
                    );
                    let content = if item["type"] == "text" {
                        &item["text"]
                    } else {
                        images += 1;
                        &item["data"]
                    };
                    output_bytes += content
                        .as_str()
                        .context("Invalid guest output content")?
                        .len();
                    ensure!(self.captured.len() < 4096 && images <= 64 && output_bytes <= CAPTURE_BYTES, "Script output capture limit exceeded (16 MiB, 4096 parts, 64 images); no store writes were committed");
                    self.captured.push(item.clone());
                }
                "done" => {
                    let mut event = event;
                    ensure!(
                        event["ok"].is_boolean()
                            && ["value", "writes", "error"]
                                .iter()
                                .all(|k| event.get(k).is_none_or(Value::is_string)),
                        "Invalid guest completion"
                    );
                    ensure!(
                        ["value", "writes", "error"]
                            .iter()
                            .map(|k| event[k].as_str().map_or(0, str::len))
                            .sum::<usize>()
                            <= CAPTURE_BYTES,
                        "Script return/store serialization exceeded the 16 MiB capture limit"
                    );
                    if !event["ok"].as_bool().unwrap() {
                        // A script that fails after its deadline ran out of
                        // time. The engine interrupt is uncatchable and is
                        // already reported by the runner with an explicit
                        // timeout kind; a nested call released at the deadline
                        // rejects with the raw engine error, so classify it
                        // here as the same user-visible timeout. Neither can
                        // commit a store write.
                        let mut failure = loads(
                            event["error"]
                                .as_str()
                                .unwrap_or("{\"message\":\"Guest execution failed\"}")
                                .as_bytes(),
                        )?;
                        ensure!(failure.is_object(), "Invalid guest error");
                        if failure.get("kind").is_none() && Instant::now() >= self.deadline {
                            failure["kind"] = json!("timeout");
                            failure["message"] =
                                json!(format!("Script timed out after {} ms", self.timeout_ms));
                            failure.as_object_mut().unwrap().remove("stack");
                            event["error"] = json!(failure.to_string());
                        }
                    }
                    return Ok(event);
                }
                _ => bail!("Unknown runner event"),
            }
        }
    }
}

impl Runtime {
    #[allow(clippy::too_many_arguments)]
    async fn settle(
        self: Rc<Self>,
        parent: u64,
        event: Value,
        ordinal: u64,
        max_calls: u64,
        tools: Rc<HashMap<String, bool>>,
        input: Rc<Mutex<ChildStdin>>,
        script: u64,
        cancel: Rc<Cancellation>,
        limiter: Rc<Semaphore>,
        deadline: Instant,
        calls: Rc<RefCell<Vec<Value>>>,
    ) {
        let started = Instant::now();
        let mut record = None;
        let result: Result<Value> = bounded(&cancel,deadline,async {
            ensure!(ordinal <= max_calls, "Script exceeded {max_calls} nested tool calls");
            let name = event["name"].as_str().context("Unknown or unavailable guest tool")?;
            ensure!(event["target"] == "tool" && tools.contains_key(name), "Unknown or unavailable guest tool");
            let args = event["args"].as_str().unwrap_or("null");
            record = Some(calls.borrow().len());
            calls.borrow_mut().push(json!({"id":format!("{parent}/{ordinal}"),"name":name,"args":head(args,200),"status":"running"}));
            let args = loads(args.as_bytes())?;
            ensure!(args.is_object(), "Tool {name} arguments must be an object");
            let _permit = limiter.acquire().await?;
            cancel.check()?;
            if Instant::now() >= deadline { return Err(TimedOut.into()); }
            let value = self.request(parent,"composition/call",json!({"name":name,"arguments":args})).await?;
            ensure!(tools[name] || value.is_string(), "Host returned a non-text value for schema-less tool {name}");
            Ok(value)
        }).await;
        let cancelled = result.as_ref().is_err_and(|e| e.is::<Cancelled>());
        if let Some(index) = record {
            let mut calls = calls.borrow_mut();
            let row = &mut calls[index];
            row["duration_ms"] = json!(started.elapsed().as_millis() as u64);
            row["status"] = json!(if cancelled {
                "cancelled"
            } else if result.is_ok() {
                "ok"
            } else {
                "error"
            });
            if let Err(error) = &result {
                if !cancelled {
                    row["error"] = json!(head(&error.to_string(), 500));
                }
            }
        }
        if cancelled {
            return;
        }
        let payload = match result {
            Ok(value) => {
                json!({"type":"settle","id":event["id"],"ok":true,"payload":value.to_string()})
            }
            Err(error) => {
                json!({"type":"settle","id":event["id"],"ok":false,"payload":head(&error.to_string(),4096)})
            }
        };
        let sent = send_runner_bounded(&input, script, &payload, &cancel).await;
        if sent
            .as_ref()
            .is_err_and(|e| e.downcast_ref::<RunnerFrameTooLarge>().is_some())
        {
            let _ = send_runner_bounded(
                &input,
                script,
                &json!({"type":"settle","id":event["id"],"ok":false,"payload":"Runner IPC exceeds 20 MiB"}),
                &cancel,
            )
            .await;
        }
        // A closed child pipe is observed by the sole reader. Cancellation and
        // disposal need no guest acknowledgement and must not revive its work.
    }
}

#[derive(Debug)]
struct RunnerFrameTooLarge;
impl std::fmt::Display for RunnerFrameTooLarge {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Runner IPC exceeds 20 MiB")
    }
}
impl std::error::Error for RunnerFrameTooLarge {}
/// Enqueue one bounded frame for the warm runner. The write is never selected
/// against cancellation once it starts: a partial frame would corrupt the pipe.
async fn send_runner_frame(input: &Mutex<ChildStdin>, value: &Value) -> Result<()> {
    let mut data = serde_json::to_vec(value)?;
    if data.len() > IPC_BYTES {
        return Err(RunnerFrameTooLarge.into());
    }
    data.push(b'\n');
    let mut input = input.lock().await;
    input.write_all(&data).await?;
    input.flush().await?;
    Ok(())
}

/// Script-scoped frame send. Cancellation may skip a frame that has not started
/// writing; a stale epoch is dropped by the runner rather than reaching a new VM.
async fn send_runner_bounded(
    input: &Mutex<ChildStdin>,
    script: u64,
    payload: &Value,
    cancel: &Cancellation,
) -> Result<()> {
    let mut input = tokio::select! {
        biased;
        _ = cancel.cancelled() => return Err(Cancelled.into()),
        guard = input.lock() => guard,
    };
    let mut data = serde_json::to_vec(
        &json!({"type":"message","script":script,"payload":payload.to_string()}),
    )?;
    if data.len() > IPC_BYTES {
        return Err(RunnerFrameTooLarge.into());
    }
    data.push(b'\n');
    input.write_all(&data).await?;
    input.flush().await?;
    Ok(())
}
async fn read_runner(reader: &mut (impl AsyncBufRead + Unpin)) -> Result<Vec<u8>> {
    let mut data = Vec::new();
    loop {
        let available = reader.fill_buf().await?;
        ensure!(
            !available.is_empty(),
            "Runner exited or exceeded bounded IPC frame"
        );
        let count = available
            .iter()
            .position(|b| *b == b'\n')
            .map_or(available.len(), |n| n + 1);
        ensure!(
            data.len() + count <= IPC_BYTES + 1,
            "Runner exited or exceeded bounded IPC frame"
        );
        data.extend_from_slice(&available[..count]);
        reader.consume(count);
        if data.last() == Some(&b'\n') {
            data.pop();
            return Ok(data);
        }
        ensure!(
            data.len() <= IPC_BYTES,
            "Runner exited or exceeded bounded IPC frame"
        );
    }
}

/// Serve the feature-negotiated API 0.4 wire. No Python, Node, runtime JS files,
/// SDK extensions or local tools are loaded by this adapter.
pub async fn run(engine: String) -> Result<()> {
    ensure!(
        matches!(engine.as_str(), "native" | "wasi"),
        "expected native or wasi"
    );
    LocalSet::new().run_until(async move {
        let mut terminate = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
        let mut interrupt = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt())?;
        let scratch_root = std::env::var_os("OCTET_EXTENSION_SCRATCH").map(PathBuf::from);
        let image = image::LaunchImage::pin_current(scratch_root.as_deref())?;
        let mut input = transport::reader()?;
        let (writer, mut writer_failure) = Writer::new()?;
        let runtime = Rc::new(Runtime { engine, image, stage:Cell::new(Stage::Starting), lost:Cell::new(false), active:RefCell::new(HashMap::new()),
            pending:RefCell::new(HashMap::new()), seen:RefCell::new(HashSet::new()), counter:Cell::new(0), config:RefCell::new(None), writer,
            serial:Mutex::new(()), running:Cell::new(false), scratch:RefCell::new(Scratch::new(scratch_root.unwrap_or_default())),
            runner:RefCell::new(None) });
        let mut shutdown = None;
        let result = loop {
            tokio::select! {
                biased;
                _ = terminate.recv() => { runtime.lost.set(true); break Ok(()); },
                _ = interrupt.recv() => { runtime.lost.set(true); break Ok(()); },
                _ = writer_failure.changed() => {
                    runtime.lost.set(true);
                    break Err(anyhow!(writer_failure.borrow().clone().unwrap_or_else(|| "Protocol writer stopped".into())));
                },
                frame = input.recv() => match frame {
                    None => { runtime.lost.set(true); break Ok(()); },
                    Some(Err(error)) => { runtime.lost.set(true); break Err(error); },
                    Some(Ok(data)) => match runtime.frame(&data).await {
                        Ok(Some(id)) => { shutdown = Some(id); break Ok(()); },
                        Ok(None) => {},
                        Err(error) => { runtime.lost.set(true); break Err(error); },
                    }
                }
            }
        };
        runtime.stage.set(Stage::Draining);
        input.close();
        let parents: Vec<u64> = runtime.active.borrow().keys().copied().collect();
        for parent in parents { runtime.cancel_parent(parent); }
        let tasks: Vec<_> = runtime.active.borrow_mut().drain().map(|(_, a)| a.task).collect();
        // Keep draining independent of signals/repeated cancels until all runner
        // children have been killed and waited. An ack must not precede this.
        let mut task_error = None;
        for task in tasks { if let Err(error) = task.await { task_error = Some(anyhow!(error)); } }
        // The warm runner is killed and waited on every terminal path, including
        // a lost transport: stdin EOF alone must not leave it running.
        runtime.close_runner().await;
        if let Some(id) = shutdown { runtime.send(json!({"jsonrpc":"2.0","id":id,"result":{}}))?; }
        let writer_result = runtime.writer.close().await;
        runtime.stage.set(Stage::Stopped);
        result?;
        writer_result?;
        if let Some(error) = task_error { return Err(error); }
        Ok(())
    }).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;
    use std::pin::Pin;
    use std::task::{Context as TaskContext, Waker};

    fn test_runtime(root: &Path) -> (Rc<Runtime>, transport::TestWriter) {
        let (writer, gate) = Writer::gated_for_test();
        let config = negotiate(&json!({
            "api_version":"0.4", "octet_version":"0.9.0",
            "extension":{"name":"octet-codemode", "version":"0.9.0"},
            "protocol":{"version":"0.4", "required_features":["request_cancellation","content_parts"],
                "optional_features":["tool_composition_v1","artifacts"], "limits":{"max_concurrent_requests":8}},
            "flag_values":[]
        })).unwrap();
        let runtime = Rc::new(Runtime {
            engine: "native".into(),
            image: image::LaunchImage::pin_current(Some(root)).unwrap(),
            stage: Cell::new(Stage::Ready),
            lost: Cell::new(false),
            active: RefCell::new(HashMap::new()),
            pending: RefCell::new(HashMap::new()),
            seen: RefCell::new(HashSet::new()),
            counter: Cell::new(0),
            config: RefCell::new(Some(config)),
            writer,
            serial: Mutex::new(()),
            running: Cell::new(false),
            scratch: RefCell::new(Scratch::new(root.to_owned())),
            runner: RefCell::new(None),
        });
        for parent in [2, 3] {
            runtime.active.borrow_mut().insert(
                parent,
                Active {
                    cancel: Rc::new(Cancellation::default()),
                    task: tokio::spawn(async {}),
                    sequence: 0,
                },
            );
        }
        (runtime, gate)
    }
    fn poll_pending(future: Pin<&mut impl Future>) {
        let mut context = TaskContext::from_waker(Waker::noop());
        assert!(future.poll(&mut context).is_pending());
    }
    fn prepared_images(root: &Path) -> usize {
        std::fs::read_dir(root)
            .unwrap()
            .flat_map(|entry| std::fs::read_dir(entry.unwrap().path()).unwrap())
            .filter(|entry| {
                entry
                    .as_ref()
                    .unwrap()
                    .path()
                    .extension()
                    .is_some_and(|ext| ext == "image")
            })
            .count()
    }

    #[tokio::test(flavor = "current_thread")]
    async fn guest_cleanup_revokes_queued_calls_without_cancelling_successful_parent() {
        let root = tempfile::tempdir().unwrap();
        let (runtime, gate) = test_runtime(root.path());
        let mut nested = Box::pin(runtime.request(
            2,
            "composition/call",
            json!({"name":"first","arguments":{}}),
        ));
        let mut other = Box::pin(runtime.request(
            3,
            "composition/call",
            json!({"name":"first","arguments":{}}),
        ));
        poll_pending(nested.as_mut());
        poll_pending(other.as_mut());
        assert_eq!(runtime.pending.borrow().len(), 2);

        // Reproduce terminal guest cleanup while the waiter remains alive and
        // unpolled. Releasing the writer before dropping it must skip its call.
        runtime.abandon_parent_requests(2);
        assert!(!runtime.active.borrow()[&2].cancel.cancelled.get());
        assert_eq!(runtime.pending.borrow().len(), 1);
        let delivered = gate.drain();
        assert_eq!(delivered.len(), 1);
        assert_eq!(delivered[0]["params"]["parent_request_id"], 3);
        assert!(nested.await.unwrap_err().is::<Cancelled>());
        runtime
            .response(&json!({"id":delivered[0]["id"],"result":{"value":"other"}}))
            .unwrap();
        assert_eq!(other.await.unwrap(), "other");

        // Cleanup revoked nested authority, not the successful outer script's
        // right to make its one later store commit.
        let mut store = Box::pin(runtime.request(
            2,
            "composition/store",
            json!({"set":{"answer":42},"delete":[]}),
        ));
        poll_pending(store.as_mut());
        let delivered = gate.drain();
        assert_eq!(delivered.len(), 1);
        assert_eq!(delivered[0]["method"], "composition/store");
        runtime
            .response(&json!({"id":delivered[0]["id"],"result":{}}))
            .unwrap();
        assert_eq!(store.await.unwrap(), json!({}));
        assert!(gate.drain().is_empty());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn guest_cleanup_cancels_a_started_reverse_request_exactly_once() {
        let root = tempfile::tempdir().unwrap();
        let (runtime, gate) = test_runtime(root.path());
        let mut nested = Box::pin(runtime.request(
            2,
            "composition/call",
            json!({"name":"first","arguments":{}}),
        ));
        poll_pending(nested.as_mut());
        let delivered = gate.drain();
        assert_eq!(delivered.len(), 1);
        runtime.abandon_parent_requests(2);
        runtime.abandon_parent_requests(2);
        assert!(!runtime.active.borrow()[&2].cancel.cancelled.get());
        let cancellation = gate.drain();
        assert_eq!(cancellation.len(), 1);
        assert_eq!(cancellation[0]["method"], "$/cancelRequest");
        assert_eq!(cancellation[0]["params"]["id"], delivered[0]["id"]);
        assert!(nested.await.unwrap_err().is::<Cancelled>());
        assert!(gate.drain().is_empty());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn artifact_expired_after_preparation_never_queues_publication() {
        let root = tempfile::tempdir().unwrap();
        let (runtime, gate) = test_runtime(root.path());
        let cancel = runtime.active.borrow()[&2].cancel.clone();
        let item = json!({"type":"image","mimeType":"image/png","data":STANDARD.encode(vec![0;256 * 1024 + 1])});
        let deadline = Instant::now();
        let mut publication = Box::pin(runtime.publish(2, &item, &cancel, deadline));
        poll_pending(publication.as_mut());
        // Preparation has created the large-image sidecar, but the checkpoint
        // must not yet allocate a reverse ID or enqueue any frame.
        assert_eq!(prepared_images(root.path()), 1);
        assert_eq!(runtime.counter.get(), 0);
        assert!(publication.await.unwrap_err().is::<TimedOut>());
        assert_eq!(prepared_images(root.path()), 0);
        assert!(runtime.pending.borrow().is_empty());
        assert!(gate.drain().is_empty());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn artifact_preparation_yields_to_queued_cancellation_before_publication() {
        LocalSet::new().run_until(async {
            let root = tempfile::tempdir().unwrap();
            let (runtime, gate) = test_runtime(root.path());
            let cancel = runtime.active.borrow()[&2].cancel.clone();
            let cancelling_runtime = runtime.clone();
            let scratch = root.path().to_owned();
            let cancellation = tokio::task::spawn_local(async move {
                // This runnable task cannot execute until publish yields. The
                // image proves that yield is after synchronous preparation.
                assert_eq!(prepared_images(&scratch), 1);
                assert_eq!(cancelling_runtime.counter.get(), 0);
                cancelling_runtime.cancel_parent(2);
            });
            let item = json!({"type":"image","mimeType":"image/png","data":STANDARD.encode(vec![0;256 * 1024 + 1])});
            let result = tokio::time::timeout(Duration::from_secs(1), runtime.publish(2, &item, &cancel, Instant::now() + Duration::from_secs(60)))
                .await.expect("publication must observe the queued cancellation");
            cancellation.await.unwrap();
            assert!(result.unwrap_err().is::<Cancelled>());
            assert_eq!(prepared_images(root.path()), 0);
            assert_eq!(runtime.counter.get(), 0);
            assert!(runtime.pending.borrow().is_empty());
            assert!(gate.drain().is_empty());
        }).await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn runner_frames_are_bounded() {
        let mut reader = BufReader::new(&b"{\"type\":\"ready\"}\n"[..]);
        assert_eq!(
            read_runner(&mut reader).await.unwrap(),
            b"{\"type\":\"ready\"}"
        );
        let mut reader = BufReader::new(&b"truncated"[..]);
        assert!(read_runner(&mut reader).await.is_err());
        let oversized = vec![b'x'; IPC_BYTES + 1];
        let mut reader = BufReader::new(oversized.as_slice());
        assert!(read_runner(&mut reader).await.is_err());
    }
    #[tokio::test(flavor = "current_thread")]
    async fn repeated_cancellation_after_synchronous_spawn_still_kills_and_waits() {
        let cancel = Rc::new(Cancellation::default());
        // A real child blocks on stdin, reproducing the spawn/cancel ownership
        // boundary without depending on the test executable's runner dispatch.
        let mut child = Command::new("/bin/cat")
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        let pid = child.id().unwrap();
        cancel.cancel();
        cancel.cancel();
        let result = bounded(
            &cancel,
            Instant::now() + Duration::from_secs(5),
            std::future::pending::<Result<()>>(),
        )
        .await;
        assert!(result.unwrap_err().is::<Cancelled>());
        child.start_kill().unwrap();
        cancel.cancel();
        let status = child.wait().await.unwrap();
        assert!(!status.success());
        assert_eq!(unsafe { libc::kill(pid as i32, 0) }, -1);
        assert_eq!(
            std::io::Error::last_os_error().raw_os_error(),
            Some(libc::ESRCH)
        );
    }
}

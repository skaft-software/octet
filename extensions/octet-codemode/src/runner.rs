//! Warm disposable runner: one process per extension generation, one fresh VM
//! per script. The compiled module (Wasmi) or realm setup (native QuickJS) is
//! prepared once; every script gets an isolated session and can never observe a
//! previous script's globals, heap, store or pending promises.
use anyhow::{bail, Result};
use serde::Deserialize;
use serde_json::{json, Value};
use std::io::{self, BufRead, Write};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::{guest, native, wasi, IPC_BYTES};

/// Process kill is only the fallback for a wedged interrupt: Wasmi's interrupt
/// handler stops JavaScript at the script deadline, and this grace exists for a
/// host import or engine bug that never returns to an interrupt check.
pub(super) const HARD_STOP_GRACE_MS: u64 = 5_000;

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
enum Command {
    /// Start a new script. `script` is a monotonically increasing epoch; frames
    /// from an earlier epoch are dropped instead of reaching the current VM.
    Script {
        script: u64,
        context: Value,
        code: String,
        timeout_ms: u64,
        heap_bytes: usize,
    },
    Message {
        script: u64,
        payload: String,
    },
}

pub(super) struct Start {
    /// Per-script JSON arguments for the invariant guest factory.
    pub context: String,
    pub code: String,
    pub heap_bytes: usize,
}

/// One disposable VM, owning a fresh store/realm for exactly one script.
pub(super) enum Session {
    // The Wasmi store dominates the enum; keep the variant boxed so a session
    // never moves hundreds of bytes through the runner's stack.
    Wasi(Box<wasi::Session>),
    Native(native::Session),
}

impl Session {
    fn start(engine: &Engine, start: &Start, deadline: Instant) -> Result<Self> {
        Ok(match engine {
            Engine::Wasi(engine) => Session::Wasi(Box::new(engine.session(start, deadline)?)),
            Engine::Native(engine) => Session::Native(engine.session(start, deadline)),
        })
    }
    /// Feed one guest command, then report a promise that can never settle:
    /// with no timers or I/O in the VM, nothing could ever resume it.
    fn message(&mut self, payload: &str) -> Result<()> {
        self.drive(payload)?;
        self.drive(r#"{"type":"poll"}"#)
    }
    fn drive(&mut self, payload: &str) -> Result<()> {
        match self {
            Session::Wasi(session) => session.message(payload),
            Session::Native(session) => session.message(payload),
        }
    }
}

/// Engine state compiled/prepared once for the whole runner lifetime.
enum Engine {
    Wasi(wasi::Engine),
    Native(native::Engine),
}

impl Engine {
    fn new(engine: &str) -> Result<Self> {
        Ok(match engine {
            "wasi" => Engine::Wasi(wasi::Engine::new()?),
            "native" => Engine::Native(native::Engine::new()),
            _ => bail!("expected native or wasi"),
        })
    }
}

fn read_frame(input: &mut impl BufRead) -> Result<Option<String>> {
    let mut bytes = Vec::new();
    let mut bounded = io::Read::take(input, (IPC_BYTES + 2) as u64);
    let count = bounded.read_until(b'\n', &mut bytes)?;
    if count == 0 {
        return Ok(None);
    }
    if bytes.last() != Some(&b'\n') || count > IPC_BYTES + 1 {
        bail!("oversized or truncated runner IPC");
    }
    bytes.pop();
    Ok(Some(String::from_utf8(bytes)?))
}

pub(super) fn emit(value: &str) -> Result<()> {
    if value.len() > IPC_BYTES {
        bail!("runner output exceeds 20 MiB IPC bound");
    }
    let mut out = io::stdout().lock();
    out.write_all(value.as_bytes())?;
    out.write_all(b"\n")?;
    out.flush()?;
    Ok(())
}

pub(crate) fn emit_json(value: &Value) -> Result<()> {
    emit(&value.to_string())
}

/// Hard stop shared with the watchdog thread: the elapsed-milliseconds mark
/// after which a still-running script is killed. Zero means idle.
fn watch(hard_stop_ms: Arc<AtomicU64>, started: Instant) {
    #[cfg(unix)]
    let parent = unsafe { libc::getppid() };
    loop {
        #[cfg(unix)]
        if unsafe { libc::getppid() } != parent {
            // The extension (or host) is gone; a warm runner must never outlive it.
            std::process::exit(125);
        }
        let armed = hard_stop_ms.load(Ordering::Relaxed);
        if armed != 0 && started.elapsed().as_millis() as u64 > armed {
            std::process::exit(124);
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn crash(_hard_stop_ms: &AtomicU64, error: anyhow::Error) -> ! {
    let message: String = format!("{error:#}").chars().take(2048).collect();
    let _ = emit_json(&json!({"type": "crash", "message": message}));
    std::process::exit(1);
}

/// Serve the warm runner protocol until stdin closes or the parent is lost.
pub(super) fn serve(engine: &str) -> Result<()> {
    let started = Instant::now();
    let hard_stop_ms = Arc::new(AtomicU64::new(0));
    {
        let hard_stop_ms = hard_stop_ms.clone();
        std::thread::Builder::new()
            .name("codemode-watchdog".into())
            .spawn(move || watch(hard_stop_ms, started))?;
    }
    // Compile/validate the module once for the whole runner lifetime; every
    // later script reuses it with a fresh isolated store.
    let engine = Engine::new(engine)?;
    emit_json(&json!({"type": "warm"}))?;

    let mut input = io::stdin().lock();
    let mut session: Option<Session> = None;
    let mut current: Option<u64> = None;
    let mut deadline = started;
    let mut timeout_ms = 0u64;
    while let Some(line) = read_frame(&mut input)? {
        let command: Command = serde_json::from_str(&line)?;
        match command {
            Command::Script {
                script,
                context,
                code,
                timeout_ms: requested,
                heap_bytes,
            } => {
                if !(1..=25_000).contains(&requested)
                    || !(1024 * 1024..=256 * 1024 * 1024).contains(&heap_bytes)
                    || code.chars().count() > 65_536
                {
                    bail!("invalid runner limits");
                }
                timeout_ms = requested;
                deadline = Instant::now() + Duration::from_millis(timeout_ms);
                hard_stop_ms.store(
                    deadline.saturating_duration_since(started).as_millis() as u64
                        + HARD_STOP_GRACE_MS,
                    Ordering::Relaxed,
                );
                let (context, code) = guest::script(&context, &code)?;
                let start = Start {
                    context,
                    code,
                    heap_bytes,
                };
                // A fresh isolated store is ready before the adapter is told to
                // start the script; the previous VM is dropped here.
                session = match Session::start(&engine, &start, deadline) {
                    Ok(session) => Some(session),
                    Err(error) => crash(&hard_stop_ms, error),
                };
                emit_json(&json!({"type": "ready"}))?;
                current = Some(script);
            }
            Command::Message { script, payload } => {
                if current != Some(script) {
                    // A cancelled script's queued settle must never reach a new VM.
                    continue;
                }
                let Some(active) = session.as_mut() else {
                    continue;
                };
                if let Err(error) = active.message(&payload) {
                    if Instant::now() >= deadline {
                        // The interrupt stopped JavaScript at the deadline. Drop
                        // the wedged VM but keep the warm process for the next
                        // script; partial output already reached the adapter.
                        session = None;
                        emit_json(&json!({"type": "done", "ok": false, "error": json!({
                            "kind": "timeout", "name": "InternalError",
                            "message": format!("Script timed out after {timeout_ms} ms")}).to_string()}))?;
                    } else {
                        crash(&hard_stop_ms, error);
                    }
                }
            }
        }
    }
    Ok(())
}

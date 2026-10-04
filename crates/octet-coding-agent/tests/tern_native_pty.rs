//! Real compiled octet + controlling PTY + synthetic TSP terminal.
//!
//! These are runtime/input/protocol checks, NOT actual Tern pixels, IME,
//! accessibility, credential saves, provider authentication or positive grants.
//! Loopback SSE, when used, is explicitly synthetic; all HOME/workspace/session
//! paths are disposable and no caller environment or credentials are inherited.
#![cfg(unix)]

use octet_tern::{
    frame,
    wire::{Verb, TSP_KINDS, TSP_PREFIX, TSP_ST},
};
use serde_json::{json, Value};
use std::{
    collections::HashMap,
    fs::{self, File},
    io::{self, Read, Write},
    net::TcpListener,
    os::{
        fd::{AsRawFd, FromRawFd},
        unix::{fs::PermissionsExt, process::CommandExt},
    },
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::{mpsc, Arc, Mutex},
    thread,
    time::{Duration, Instant},
};
use tempfile::TempDir;

struct Options {
    model: bool,
    args: Vec<String>,
    kinds: Vec<String>,
    features: Vec<String>,
    credits: u32,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            model: true,
            args: Vec::new(),
            kinds: TSP_KINDS.iter().map(|kind| (*kind).to_owned()).collect(),
            features: Vec::new(),
            credits: 2,
        }
    }
}

struct NativePty {
    child: Child,
    master: File,
    _slave: File,
    _root: TempDir,
    pending: Vec<u8>,
    output: Vec<u8>,
    nodes: HashMap<String, Value>,
    parents: HashMap<String, String>,
    frames: usize,
    messages: Vec<(String, Value)>,
    hello: bool,
    options: Options,
    acknowledge: bool,
    withheld: Vec<Value>,
}

impl NativePty {
    fn spawn() -> Self {
        Self::configured(Options::default(), |_, _, _| {})
    }

    fn configured(options: Options, prepare: impl FnOnce(&Path, &Path, &Path)) -> Self {
        let root = tempfile::tempdir().unwrap();
        let canonical = root.path().canonicalize().unwrap();
        let home = canonical.join("home");
        let workspace = canonical.join("workspace");
        let sessions = canonical.join("sessions");
        fs::create_dir_all(home.join(".octet/credentials")).unwrap();
        fs::create_dir_all(&workspace).unwrap();
        let credentials = home.join(".octet/credentials/custom.json");
        fs::create_dir_all(&sessions).unwrap();
        if options.model {
            fs::write(&credentials, json!({"base_url":"http://127.0.0.1:9/v1/", "api_key":"", "api_name":"native-test", "headers":[], "models":[
                {"api_name":"probe", "reasoning":true, "reasoning_values":["off","low","high"], "reasoning_default":"off"},
                {"api_name":"alpha-model"}
            ], "auto_discover":false}).to_string()).unwrap();
            fs::set_permissions(&credentials, fs::Permissions::from_mode(0o600)).unwrap();
        }
        prepare(&home, &workspace, &sessions);
        let (mut master_fd, mut slave_fd) = (-1, -1);
        let mut size = libc::winsize {
            ws_row: 40,
            ws_col: 100,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };
        // SAFETY: valid output pointers; successful descriptors immediately get owners.
        assert_eq!(
            unsafe {
                libc::openpty(
                    &mut master_fd,
                    &mut slave_fd,
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    // glibc takes `*const winsize`, macOS `*mut`; a raw
                    // pointer satisfies both without a needless `&mut`.
                    std::ptr::addr_of_mut!(size),
                )
            },
            0
        );
        let master = unsafe { File::from_raw_fd(master_fd) };
        let slave = unsafe { File::from_raw_fd(slave_fd) };
        for fd in [master_fd, slave_fd] {
            assert_ne!(
                unsafe { libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC) },
                -1
            );
        }
        let flags = unsafe { libc::fcntl(master_fd, libc::F_GETFL) };
        assert_ne!(
            unsafe { libc::fcntl(master_fd, libc::F_SETFL, flags | libc::O_NONBLOCK) },
            -1
        );
        let mut command = Command::new(env!("CARGO_BIN_EXE_octet"));
        command.args([
            "--offline",
            "--no-context-files",
            "--no-tools",
            "--theme",
            "dark",
        ]);
        if options.model {
            command.args(["--model", "custom/probe"]);
        }
        command
            .args(&options.args)
            .arg("--workspace")
            .arg(&workspace)
            .arg("--session-dir")
            .arg(&sessions)
            .current_dir(&workspace)
            .env_clear()
            .env("HOME", &home)
            .env("PWD", &workspace)
            .env("PATH", "/usr/bin:/bin")
            .env("TERM", "xterm-256color")
            .env("TERM_PROGRAM", "tern")
            .env("COLORTERM", "truecolor")
            .env("LANG", "C.UTF-8")
            .stdin(Stdio::from(slave.try_clone().unwrap()))
            .stdout(Stdio::from(slave.try_clone().unwrap()))
            .stderr(Stdio::from(slave.try_clone().unwrap()));
        // SAFETY: only async-signal-safe syscalls between fork and exec.
        unsafe {
            command.pre_exec(move || {
                if libc::setsid() == -1
                    || libc::ioctl(slave_fd, libc::TIOCSCTTY as libc::c_ulong, 0) == -1
                {
                    return Err(io::Error::last_os_error());
                }
                Ok(())
            });
        }
        Self {
            child: command.spawn().unwrap(),
            master,
            _slave: slave,
            _root: root,
            pending: Vec::new(),
            output: Vec::new(),
            nodes: HashMap::new(),
            parents: HashMap::new(),
            frames: 0,
            messages: Vec::new(),
            hello: false,
            options,
            acknowledge: true,
            withheld: Vec::new(),
        }
    }

    fn send(&mut self, input: &[u8]) {
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut remaining = input;
        while !remaining.is_empty() {
            match self.master.write(remaining) {
                Ok(0) => panic!("PTY input closed"),
                Ok(written) => remaining = &remaining[written..],
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                    assert!(Instant::now() < deadline, "PTY input stayed blocked");
                    thread::sleep(Duration::from_millis(2));
                }
                Err(error) => panic!("PTY input: {error}"),
            }
        }
        self.master.flush().unwrap();
    }
    fn event(&mut self, event: Value) {
        self.send(
            frame::encode_json(Verb::Event, &event, 65536)
                .unwrap()
                .as_bytes(),
        );
    }
    fn add(&mut self, node: &Value, parent: &str) {
        let id = node["id"].as_str().unwrap();
        self.nodes.insert(id.into(), node.clone());
        self.parents.insert(id.into(), parent.into());
        if let Some(children) = node["c"].as_array() {
            for child in children {
                self.add(child, id);
            }
        }
    }
    fn delete(&mut self, id: &str) {
        // Retained children can be added/moved after their parent's initial Add.
        // A snapshot's original `c` array alone is not the current tree.
        let children: Vec<_> = self
            .parents
            .iter()
            .filter(|(_, parent)| parent.as_str() == id)
            .map(|(child, _)| child.clone())
            .collect();
        for child in children {
            self.delete(&child);
        }
        self.nodes.remove(id);
        self.parents.remove(id);
    }
    fn read(&mut self) {
        let mut buffer = [0; 8192];
        loop {
            match self.master.read(&mut buffer) {
                Ok(0) => break,
                Ok(read) => {
                    self.pending.extend_from_slice(&buffer[..read]);
                    self.output.extend_from_slice(&buffer[..read]);
                }
                Err(error)
                    if error.kind() == io::ErrorKind::WouldBlock
                        || error.raw_os_error() == Some(libc::EIO) =>
                {
                    break;
                }
                Err(error) => panic!("PTY read: {error}"),
            }
        }
        assert!(
            self.output.len() < 4 * 1024 * 1024,
            "unbounded native output"
        );
        while let Some(start) = self
            .pending
            .windows(TSP_PREFIX.len())
            .position(|bytes| bytes == TSP_PREFIX.as_bytes())
        {
            let Some(end) = self.pending[start..]
                .windows(TSP_ST.len())
                .position(|bytes| bytes == TSP_ST.as_bytes())
                .map(|index| start + index + TSP_ST.len())
            else {
                break;
            };
            let sequence = String::from_utf8(self.pending[start..end].to_vec()).unwrap();
            self.pending.drain(..end);
            let raw = frame::split(&sequence).unwrap();
            // Blob bodies are base64 media, not JSON. The native welcome now
            // uploads octet's immutable byte mark before its first frame.
            if raw.verb == "b" {
                continue;
            }
            let body: Value = serde_json::from_str(&raw.body).unwrap();
            match raw.verb.as_str() {
                "q" if body["q"] == "hello" => {
                    self.hello = true;
                    let hello = json!({"r":"hello","v":1,"term":"headless-test","kinds":self.options.kinds,"features":self.options.features,"apc":65536,"credits":self.options.credits,"cols":100,"dark":true,"reduceMotion":true});
                    self.send(
                        frame::encode_json(Verb::Reply, &hello, 65536)
                            .unwrap()
                            .as_bytes(),
                    );
                }
                "f" => {
                    self.frames += 1;
                    for op in body["ops"].as_array().unwrap() {
                        match op[0].as_str().unwrap() {
                            "add" => self.add(&op[4], op[2].as_str().unwrap()),
                            "set" => {
                                let node = self.nodes.get_mut(op[1].as_str().unwrap()).unwrap();
                                if !node["p"].is_object() {
                                    node["p"] = json!({});
                                }
                                for (key, value) in op[2].as_object().unwrap() {
                                    node["p"][key] = value.clone();
                                }
                            }
                            "del" => self.delete(op[1].as_str().unwrap()),
                            "move" => {
                                self.parents.insert(
                                    op[1].as_str().unwrap().into(),
                                    op[2].as_str().unwrap().into(),
                                );
                            }
                            "focus" | "reveal" | "scroll" | "suspend" | "resume" | "settle" => {}
                            other => panic!("unexpected native op {other}"),
                        }
                    }
                    let ack = json!({"ev":"ack","sf":body["sf"],"s":body["s"]});
                    if self.acknowledge {
                        self.event(ack);
                    } else {
                        self.withheld.push(ack);
                    }
                }
                "o" => {
                    self.nodes.clear();
                    self.parents.clear();
                }
                _ => {}
            }
            self.messages.push((raw.verb, body));
        }
    }
    fn wait(&mut self, predicate: impl Fn(&Self) -> bool) {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            self.read();
            if predicate(self) {
                return;
            }
            assert!(
                self.child.try_wait().unwrap().is_none() && Instant::now() < deadline,
                "native wait failed: {:?}",
                String::from_utf8_lossy(&self.output)
            );
            thread::sleep(Duration::from_millis(2));
        }
    }
    fn draft(&self) -> &str {
        self.nodes
            .get("composer.editor")
            .and_then(|node| node["p"]["text"].as_str())
            .unwrap_or_default()
    }
    fn ready(&mut self) {
        self.wait(|pty| {
            pty.nodes
                .get("composer.editor")
                .is_some_and(|node| node["p"]["readonly"] == false)
        });
    }
    fn command(&mut self, command: &str) {
        assert!(self.draft().is_empty());
        self.send(format!("\x1b[200~{command}\x1b[201~").as_bytes());
        self.wait(|pty| pty.draft() == command);
        self.send(b"\r");
    }
    fn escape(&mut self) {
        self.send(b"\x1b[27u");
    }
    fn cursor(&self) -> usize {
        self.nodes["composer.editor"]["p"]["cursor"]
            .as_u64()
            .unwrap() as usize
    }
    fn node_kind(&self, kind: &str) -> Option<&Value> {
        self.nodes.values().find(|node| node["k"] == kind)
    }
    fn panel(&self) -> Option<&Value> {
        self.nodes.values().find(|node| {
            node["id"]
                .as_str()
                .is_some_and(|id| id.starts_with("panel."))
                && matches!(node["k"].as_str(), Some("picker" | "list"))
        })
    }
    fn report_id(&self) -> String {
        self.nodes
            .values()
            .find(|node| node["p"]["role"] == "octet.report")
            .unwrap()["id"]
            .as_str()
            .unwrap()
            .into()
    }
    fn activate(&mut self, id: &str, item: &str) {
        self.event(json!({"ev":"activate","sf":"octet.session","id":id,"item":item}));
    }
    fn action(&mut self, id: &str, act: &str) {
        self.event(json!({"ev":"action","sf":"octet.session","id":id,"act":act}));
    }
    fn edit(&mut self, id: &str, range: (usize, usize), text: &str, cursor: usize, len: usize) {
        self.event(json!({"ev":"edit","sf":"octet.session","id":id,"from":range.0,"to":range.1,"text":text,"cursor":cursor,"len":len}));
    }
    fn sheet_choice(&mut self, label: &str) {
        let item = self
            .nodes
            .values()
            .find(|node| node["k"] == "item" && node["p"]["label"] == label)
            .unwrap()["id"]
            .as_str()
            .unwrap()
            .to_owned();
        let panel = self.parents[&item].clone();
        self.activate(&panel, &item);
    }
    fn ops(&self) -> impl Iterator<Item = &Value> {
        self.messages
            .iter()
            .filter(|(verb, _)| verb == "f")
            .flat_map(|(_, body)| body["ops"].as_array().unwrap())
    }
    fn resize(&mut self, cols: u16, rows: u16) {
        let size = libc::winsize {
            ws_row: rows,
            ws_col: cols,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };
        assert_eq!(
            unsafe {
                libc::ioctl(
                    self.master.as_raw_fd(),
                    libc::TIOCSWINSZ as libc::c_ulong,
                    &size,
                )
            },
            0
        );
        self.event(json!({"ev":"resize","sf":"octet.session","cols":cols,"visible":true}));
    }
    fn settle(&mut self) {
        let until = Instant::now() + Duration::from_millis(400);
        while Instant::now() < until {
            self.read();
            thread::sleep(Duration::from_millis(2));
        }
    }
    fn close(&mut self) {
        self.send(&[4]);
        let until = Instant::now() + Duration::from_secs(5);
        loop {
            self.read();
            if let Some(status) = self.child.try_wait().unwrap() {
                assert!(status.success());
                return;
            }
            assert!(Instant::now() < until);
            thread::sleep(Duration::from_millis(2));
        }
    }
}

impl Drop for NativePty {
    fn drop(&mut self) {
        if self.child.try_wait().ok().flatten().is_none() {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}

// Only user-authored saved history is seeded: no fabricated assistant, tool,
// usage, worker, or authority result. Runtime provider coverage uses actual HTTP.
fn seed_session(workspace: &Path, sessions: &Path, id: &str, prompts: &[&str]) -> PathBuf {
    let mut key = 0xcbf2_9ce4_8422_2325u64;
    for byte in workspace.to_string_lossy().as_bytes() {
        key = (key ^ u64::from(*byte)).wrapping_mul(0x0000_0100_0000_01b3);
    }
    let directory = sessions.join(format!("{key:012x}"));
    fs::create_dir_all(&directory).unwrap();
    let path = directory.join(format!("{id}.jsonl"));
    let mut session = octet_agent::Session::create(path.clone()).unwrap();
    for prompt in prompts {
        session
            .append(octet_agent::EntryValue::Message(octet_ai::Message::User(
                octet_ai::UserMessage {
                    content: vec![octet_ai::UserPart::Text((*prompt).into())],
                },
            )))
            .unwrap();
    }
    path
}

/// One bounded, loopback-only OpenAI-compatible request. The server is a
/// synthetic protocol peer, not provider measurement or live-provider proof.
struct SyntheticStream {
    url: String,
    release: mpsc::Sender<bool>,
    worker: Option<thread::JoinHandle<()>>,
    requests: Arc<Mutex<Vec<Value>>>,
}

impl SyntheticStream {
    fn new() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/v1/", listener.local_addr().unwrap());
        listener.set_nonblocking(true).unwrap();
        let (release, gate) = mpsc::channel();
        let requests = Arc::new(Mutex::new(Vec::new()));
        let recorded = requests.clone();
        let worker = thread::spawn(move || {
            let until = Instant::now() + Duration::from_secs(12);
            // Prewarming may establish an idle TCP connection before the turn.
            // Poll all accepted peers under one deadline; only a complete POST
            // chooses the response owner. Do not mistake an idle peer for a
            // failed provider request or discard a partial real request.
            let mut peers: Vec<(std::net::TcpStream, Vec<u8>)> = Vec::new();
            let (mut socket, request) = 'request: loop {
                if Instant::now() >= until || gate.try_recv().is_ok() {
                    return;
                }
                loop {
                    match listener.accept() {
                        Ok((socket, address)) => {
                            assert!(address.ip().is_loopback());
                            socket.set_nonblocking(true).unwrap();
                            assert!(peers.len() < 16);
                            peers.push((socket, Vec::new()));
                        }
                        Err(error) if error.kind() == io::ErrorKind::WouldBlock => break,
                        Err(error) => panic!("loopback accept: {error}"),
                    }
                }
                let mut index = 0;
                while index < peers.len() {
                    let (socket, bytes) = &mut peers[index];
                    let mut buffer = [0; 4096];
                    let closed = loop {
                        match socket.read(&mut buffer) {
                            Ok(0) => break true,
                            Ok(read) => {
                                bytes.extend_from_slice(&buffer[..read]);
                                assert!(bytes.len() < 256 * 1024);
                            }
                            Err(error) if error.kind() == io::ErrorKind::WouldBlock => break false,
                            Err(error) => panic!("loopback request read: {error}"),
                        }
                    };
                    if let Some(end) = bytes.windows(4).position(|bytes| bytes == b"\r\n\r\n") {
                        let head = String::from_utf8_lossy(&bytes[..end]);
                        assert!(head.starts_with("POST /v1/chat/completions "));
                        let length: usize = head
                            .lines()
                            .find_map(|line| {
                                let (name, value) = line.split_once(':')?;
                                name.eq_ignore_ascii_case("content-length")
                                    .then(|| value.trim().parse().unwrap())
                            })
                            .unwrap();
                        if bytes.len() >= end + 4 + length {
                            let request =
                                serde_json::from_slice(&bytes[end + 4..end + 4 + length]).unwrap();
                            let (socket, _) = peers.swap_remove(index);
                            break 'request (socket, request);
                        }
                    }
                    if closed {
                        assert!(bytes.is_empty(), "real request closed before completion");
                        peers.swap_remove(index);
                    } else {
                        index += 1;
                    }
                }
                thread::sleep(Duration::from_millis(2));
            };
            socket.set_nonblocking(false).unwrap();
            socket
                .set_write_timeout(Some(Duration::from_secs(3)))
                .unwrap();
            recorded.lock().unwrap().push(request);
            let head = concat!(
                "data: {\"id\":\"synthetic-native\",\"model\":\"probe\",\"choices\":[{\"index\":0,\"delta\":{\"role\":\"assistant\",\"reasoning_content\":\"# Synthetic reasoning\\nEvidence from loopback only.\"},\"finish_reason\":null}]}\n\n",
                "data: {\"id\":\"synthetic-native\",\"model\":\"probe\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"native streaming head\"},\"finish_reason\":null}]}\n\n"
            );
            socket.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\n").unwrap();
            socket.write_all(head.as_bytes()).unwrap();
            socket.flush().unwrap();
            if gate.recv_timeout(Duration::from_secs(12)).unwrap_or(false) {
                // No made-up price/rate/usage: this response deliberately omits
                // usage. Actual host uncertainty must remain uncertainty.
                let tail = concat!(
                    "data: {\"id\":\"synthetic-native\",\"model\":\"probe\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"\\n\\n```text\\nkept\\n\\n\\nblank lines\\n```\\n\\nnative final tail\"},\"finish_reason\":null}]}\n\n",
                    "data: {\"id\":\"synthetic-native\",\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n",
                    "data: [DONE]\n\n"
                );
                let _ = socket.write_all(tail.as_bytes());
                let _ = socket.flush();
            }
        });
        Self {
            url,
            release,
            worker: Some(worker),
            requests,
        }
    }
    fn pty(&self) -> NativePty {
        NativePty::configured(Options::default(), |home, _, _| {
            let path = home.join(".octet/credentials/custom.json");
            let mut record: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
            record["base_url"] = json!(self.url);
            fs::write(path, record.to_string()).unwrap();
        })
    }
}

impl Drop for SyntheticStream {
    fn drop(&mut self) {
        let _ = self.release.send(false);
        if let Some(worker) = self.worker.take() {
            let result = worker.join();
            if !thread::panicking() {
                result.unwrap();
            }
        }
    }
}

fn setup_endpoint(pty: &mut NativePty) -> String {
    pty.command("/setup");
    pty.wait(|pty| pty.panel().is_some());
    pty.sheet_choice("Local/self-hosted models");
    pty.wait(|pty| {
        pty.nodes
            .values()
            .any(|node| node["p"]["label"] == "OpenAI-compatible endpoint")
    });
    pty.sheet_choice("OpenAI-compatible endpoint");
    pty.wait(|pty| {
        pty.nodes.values().any(|node| {
            node["id"]
                .as_str()
                .is_some_and(|id| id.starts_with("prompt.") && id.ends_with(".editor"))
        })
    });
    pty.nodes
        .values()
        .find(|node| {
            node["id"]
                .as_str()
                .is_some_and(|id| id.starts_with("prompt.") && id.ends_with(".editor"))
        })
        .unwrap()["id"]
        .as_str()
        .unwrap()
        .into()
}

fn cancel_setup(pty: &mut NativePty) {
    // Each Escape acknowledges one existing owner; no save/review is reached.
    for _ in 0..4 {
        pty.escape();
        pty.settle();
        if pty
            .nodes
            .get("composer.editor")
            .is_some_and(|node| node["p"]["readonly"] == false)
        {
            return;
        }
    }
    panic!("setup did not return its input owner");
}

#[test]
fn native_keys_paste_escape_and_protocol_fragments_never_become_draft_escape_text() {
    let mut pty = NativePty::spawn();
    pty.wait(|pty| pty.hello && pty.nodes.contains_key("composer.editor"));
    pty.send("draft 🦀".as_bytes());
    pty.wait(|pty| pty.draft() == "draft 🦀");
    pty.send("\x1b[200~ pasted 雪\r\nsecond line\x1b[201~".as_bytes());
    pty.wait(|pty| pty.draft() == "draft 🦀 pasted 雪\nsecond line");
    let ack = frame::encode_json(
        Verb::Event,
        &json!({"ev":"ack","sf":"octet.session","s":0}),
        65536,
    )
    .unwrap();
    // Deliberately split ESC _, body and ESC \ across real input reads.
    for chunk in ack.as_bytes().chunks(3) {
        pty.send(chunk);
        thread::sleep(Duration::from_millis(10));
    }
    pty.send(b"\x1b[27u");
    pty.settle();
    assert!(!pty.draft().contains("tsp;") && !pty.draft().contains('\x1b'));
    assert!(!pty.nodes.values().any(|node| node["k"] == "rows"));
    pty.close();
}

#[test]
fn native_shell_keeps_full_command_and_never_sends_output_before_ctrl_o() {
    let mut pty = NativePty::spawn();
    pty.wait(|pty| pty.nodes.contains_key("composer.editor"));
    let marker = "output-requires-ctrl-o".repeat(40);
    let command = format!("printf '%s\\n' '{marker}'\nprintf 'second line\\n'");
    pty.send(format!("\x1b[200~!{command}\x1b[201~").as_bytes());
    pty.wait(|pty| pty.draft() == format!("!{command}"));
    pty.send(b"\r");
    pty.wait(|pty| {
        pty.nodes.values().any(|node| {
            node["id"]
                .as_str()
                .is_some_and(|id| id.ends_with(".command.label"))
                && node["p"]["spans"][1]["t"] == " · done"
        })
    });
    let command_leaf = pty.nodes.values().find(|node| node["k"] == "code").unwrap();
    assert_eq!(command_leaf["p"]["text"], command);
    assert_eq!(command_leaf["p"]["wrap"], true);
    assert_eq!(command_leaf["p"]["numbers"], false);
    let prefix = command_leaf["id"]
        .as_str()
        .unwrap()
        .strip_suffix(".command")
        .unwrap();
    let rail = &pty.nodes[&format!("{prefix}.shell")];
    assert_eq!(rail["k"], "row");
    assert_eq!(rail["p"]["role"], "octet.command");
    assert!(rail["p"].get("collapsed").is_none());
    assert!(rail["p"].get("collapsible").is_none());
    let output_id = format!("{prefix}.out");
    let command_id = command_leaf["id"].as_str().unwrap().to_owned();
    // Inspect all emitted frames, not only the final tree: no transient mount.
    assert!(!String::from_utf8_lossy(&pty.output).contains(&format!("\"{output_id}\"")));
    pty.send(&[15]);
    pty.wait(|pty| {
        pty.nodes.get(&output_id).is_some_and(|node| {
            node["p"]["text"]
                .as_str()
                .is_some_and(|text| text.contains(&marker))
        })
    });
    pty.send(&[15]);
    pty.wait(|pty| !pty.nodes.contains_key(&output_id));
    assert_eq!(pty.nodes[&command_id]["p"]["text"], command);
    pty.close();
}

#[test]
fn slash_completion_settings_and_theme_picker_keep_native_ownership() {
    let mut pty = NativePty::spawn();
    pty.wait(|pty| pty.nodes.contains_key("composer.editor"));
    pty.send(b"/");
    pty.wait(|pty| pty.nodes.values().any(|node| node["k"] == "list"));
    assert!(!pty.nodes.values().any(|node| node["k"] == "rows"));
    pty.send(b"\x1b[27u");
    pty.wait(|pty| !pty.nodes.values().any(|node| node["k"] == "list"));
    pty.send(b"\x7f/settings\r");
    pty.wait(|pty| {
        pty.nodes.get("report.body").is_some_and(|node| {
            node["p"]["text"]
                .as_str()
                .is_some_and(|text| text.contains("octet settings"))
        })
    });
    pty.send(b"\x1b[27u");
    pty.wait(|pty| !pty.nodes.contains_key("report.body"));
    pty.send(b"/theme\r");
    pty.wait(|pty| {
        pty.nodes
            .values()
            .any(|node| node["k"] == "overlay" && node["p"]["role"] == "octet.picker")
    });
    assert!(pty.nodes.values().any(|node| node["k"] == "list"));
    pty.send(b"\x1b[27u");
    pty.wait(|pty| {
        !pty.nodes
            .values()
            .any(|node| node["p"]["role"] == "octet.picker")
    });
    assert!(!String::from_utf8_lossy(&pty.output).contains("Native Tern rendering unavailable"));
    assert!(!pty.nodes.values().any(|node| node["k"] == "rows"));
    pty.close();
}

#[test]
fn native_fresh_startup_is_silent_and_keeps_one_welcome_and_input_owner() {
    let mut pty = NativePty::spawn();
    pty.ready();
    assert!(pty.hello);
    for region in ["main", "dock", "layer"] {
        assert!(pty.nodes.contains_key(region));
    }
    assert_eq!(
        pty.messages.iter().filter(|(verb, _)| verb == "o").count(),
        1
    );
    assert!(pty.nodes.values().any(|node| node["k"] == "image"));
    assert!(!pty.nodes.values().any(|node| node["k"] == "rows"));
    let bytes = String::from_utf8_lossy(&pty.output);
    for phase in [
        "discovering sessions",
        "replaying session",
        "forking session",
        "starting extensions",
    ] {
        assert!(!bytes.contains(phase));
    }
    pty.close();
    assert!(pty
        .messages
        .iter()
        .any(|(verb, body)| verb == "x" && body["keep"] == true));
}

#[test]
fn native_model_less_startup_can_cancel_setup_and_open_local_reports() {
    let mut pty = NativePty::configured(
        Options {
            model: false,
            ..Options::default()
        },
        |_, _, _| {},
    );
    pty.wait(|pty| pty.panel().is_some());
    pty.sheet_choice("Continue without a provider");
    pty.ready();
    for (command, title) in [
        ("/changelog", "Changelog"),
        ("/hotkeys", "Hotkeys"),
        ("/help", "Help"),
        ("/session", "Session"),
    ] {
        pty.command(command);
        pty.wait(|pty| {
            pty.nodes.values().any(|node| {
                node["p"]["role"] == "octet.report"
                    && node["p"]["head"]
                        .as_str()
                        .is_some_and(|head| head.starts_with(title))
            })
        });
        if command == "/changelog" || command == "/hotkeys" {
            assert_eq!(pty.nodes["report.body"]["k"], "md");
        }
        pty.escape();
        pty.ready();
    }
    assert!(!pty
        ._root
        .path()
        .join("home/.octet/credentials/custom.json")
        .exists());
    pty.close();
}

#[test]
fn native_reports_hotkeys_settings_and_accounting_stay_local_and_ephemeral() {
    let mut pty = NativePty::spawn();
    pty.ready();
    for (command, title) in [
        ("/help", "Help"),
        ("/status", "Status"),
        ("/cost", "Cost"),
        ("/cache", "Cache"),
        ("/session", "Session"),
        ("/settings", "Settings"),
        ("/scoped-models", "Scoped models"),
        ("/goal status", "Goal status"),
        ("/hotkeys", "Hotkeys"),
        ("/changelog", "Changelog"),
    ] {
        pty.command(command);
        pty.wait(|pty| {
            pty.nodes.values().any(|node| {
                node["p"]["role"] == "octet.report"
                    && node["p"]["head"]
                        .as_str()
                        .is_some_and(|head| head.starts_with(title))
            })
        });
        assert_eq!(pty.nodes["composer.editor"]["p"]["readonly"], true);
        if command == "/status" {
            let text = pty.nodes["report.body"]["p"]["text"].as_str().unwrap();
            assert!(text.contains("unavailable (no completed model turn)"));
            assert!(text.contains("Throughput") && text.contains("unavailable"));
        }
        if command == "/hotkeys" {
            assert_eq!(pty.nodes["report.body"]["k"], "md");
            let source = pty.nodes["report.body"]["p"]["text"].as_str().unwrap();
            assert!(source.contains("### Editing") && source.contains("| Keys | Action |"));
        }
        pty.action(&pty.report_id(), "close");
        pty.ready();
        assert!(!pty.nodes.contains_key("report.body"));
    }
    assert!(!pty.nodes.values().any(|node| node["k"] == "rows"));
    assert!(pty
        .nodes
        .values()
        .all(|node| node["p"]["role"] != "omp.assistant"));
    pty.close();
}

#[test]
fn native_context_uses_captured_meter_and_negotiated_table_or_kv_fallback() {
    for tables in [true, false] {
        let mut options = Options::default();
        if !tables {
            options.kinds.retain(|kind| kind != "table");
        }
        let mut pty = NativePty::configured(options, |_, _, _| {});
        pty.ready();
        pty.command("/context");
        pty.wait(|pty| pty.nodes.contains_key("report.context.categories"));
        assert_eq!(
            pty.nodes["report.context.categories"]["k"],
            if tables { "table" } else { "kv" }
        );
        let meter = &pty.nodes["report.context.meter"]["p"];
        assert!(meter["total"].as_str().unwrap().ends_with("tokens"));
        let parts = meter["parts"].as_array().unwrap();
        assert!(!parts.is_empty());
        let sum: f64 = parts
            .iter()
            .map(|part| part["value"].as_f64().unwrap())
            .sum();
        assert!((sum - 1.0).abs() < 0.000001);
        pty.escape();
        pty.ready();
        pty.close();
    }
}

#[test]
fn native_utf16_edits_check_lengths_boundaries_and_preserve_caret_across_picker() {
    let mut pty = NativePty::spawn();
    pty.ready();
    pty.edit("composer.editor", (0, 0), "A🦀雪\r\nZ", 4, 0);
    pty.wait(|pty| pty.draft() == "A🦀雪\nZ" && pty.cursor() == 4);
    for (range, cursor, len) in [((1, 2), 1, 6), ((0, 0), 0, 5), ((4, 2), 4, 6)] {
        pty.edit("composer.editor", range, "rejected", cursor, len);
        pty.settle();
        assert_eq!(pty.draft(), "A🦀雪\nZ");
        assert_eq!(pty.cursor(), 4);
    }
    pty.action("composer.model", "model");
    pty.wait(|pty| pty.node_kind("picker").is_some());
    assert_eq!(pty.nodes["composer.editor"]["p"]["readonly"], true);
    pty.edit(
        "composer.editor",
        (0, 6),
        "forbidden while picker owns focus",
        0,
        6,
    );
    pty.action("composer.send", "send");
    pty.settle();
    assert_eq!(pty.draft(), "A🦀雪\nZ");
    pty.escape();
    pty.ready();
    assert_eq!(pty.cursor(), 4);
    pty.send(b"!");
    pty.wait(|pty| pty.draft() == "A🦀雪!\nZ");
    pty.close();
}

#[test]
fn native_completion_pointer_activation_is_draft_fenced_and_keeps_all_candidates() {
    let mut pty = NativePty::spawn();
    pty.ready();
    pty.send(b"/");
    pty.wait(|pty| pty.node_kind("list").is_some());
    let list = pty.node_kind("list").unwrap().clone();
    assert_eq!(list["p"]["max"]["lines"], 8);
    assert!(list["c"].as_array().unwrap().len() > 8);
    let old_id = list["id"].as_str().unwrap().to_owned();
    let old_item = list["c"][0]["id"].as_str().unwrap().to_owned();
    pty.send(b"hot");
    pty.wait(|pty| pty.draft() == "/hot");
    pty.activate(&old_id, &old_item);
    pty.settle();
    assert_eq!(pty.draft(), "/hot");
    assert!(!pty.nodes.contains_key("report.body"));
    let list = pty.node_kind("list").unwrap().clone();
    pty.activate(
        list["id"].as_str().unwrap(),
        list["c"][0]["id"].as_str().unwrap(),
    );
    pty.wait(|pty| {
        pty.nodes
            .get("report.body")
            .is_some_and(|node| node["k"] == "md")
    });
    pty.escape();
    pty.ready();
    pty.close();
}

#[test]
fn native_path_completion_inserts_real_workspace_path_without_media_admission() {
    let mut pty = NativePty::configured(Options::default(), |_, workspace, _| {
        fs::write(workspace.join("qualification file.txt"), "plain text").unwrap();
    });
    pty.ready();
    pty.send(b"@qual");
    pty.wait(|pty| {
        pty.nodes.values().any(|node| {
            node["id"]
                .as_str()
                .is_some_and(|id| id.starts_with("completion.") && id.ends_with(".path"))
        })
    });
    let list = pty.node_kind("list").unwrap().clone();
    pty.activate(
        list["id"].as_str().unwrap(),
        list["c"][0]["id"].as_str().unwrap(),
    );
    pty.wait(|pty| pty.draft().contains("qualification"));
    assert!(pty.draft().contains("file.txt"));
    assert!(!pty
        .nodes
        .values()
        .any(|node| node["p"]["role"] == "omp.user"));
    pty.close();
}

#[test]
fn native_model_scope_filter_pointer_switch_and_reopened_request_reject_stale_actions() {
    let mut pty = NativePty::spawn();
    pty.ready();
    pty.command("/model");
    pty.wait(|pty| pty.node_kind("picker").is_some());
    let picker = pty.node_kind("picker").unwrap().clone();
    assert_eq!(picker["p"]["total"], 2);
    assert_eq!(picker["p"]["current"].as_array().unwrap().len(), 1);
    assert!(picker["p"]["scopes"].as_array().unwrap().len() >= 2);
    assert!(pty.nodes.contains_key("panel.preview.facts"));
    let old = picker["id"].as_str().unwrap().to_owned();
    pty.event(
        json!({"ev":"action","sf":"octet.session","id":old,"act":"scope","value":"provider.0"}),
    );
    pty.wait(|pty| pty.node_kind("picker").unwrap()["p"]["scope"] == "provider.0");
    let scoped = pty.node_kind("picker").unwrap()["id"]
        .as_str()
        .unwrap()
        .to_owned();
    assert_ne!(old, scoped);
    pty.activate(&old, "0");
    pty.settle();
    assert_eq!(pty.node_kind("picker").unwrap()["id"], scoped);
    pty.edit(&scoped, (0, 0), "no-such-model", 13, 0);
    pty.wait(|pty| {
        pty.node_kind("picker").unwrap()["p"]["order"]
            .as_array()
            .unwrap()
            .is_empty()
    });
    pty.edit(&scoped, (0, 13), "alpha", 5, 13);
    pty.wait(|pty| pty.node_kind("picker").unwrap()["p"]["query"] == "alpha");
    let selected = pty.node_kind("picker").unwrap()["p"]["selected"]
        .as_str()
        .unwrap()
        .to_owned();
    pty.activate(&scoped, &selected);
    pty.wait(|pty| {
        serde_json::to_string(&pty.nodes["composer.model.name"])
            .unwrap()
            .contains("alpha-model")
    });
    pty.ready();
    pty.command("/model");
    pty.wait(|pty| pty.node_kind("picker").is_some());
    let current = pty.node_kind("picker").unwrap().clone();
    assert_ne!(current["id"], old);
    pty.activate(&old, &selected);
    pty.action(&old, "cancel");
    pty.settle();
    assert_eq!(pty.node_kind("picker").unwrap()["id"], current["id"]);
    pty.escape();
    pty.ready();
    pty.close();
}

#[test]
fn native_thinking_sheet_uses_exact_supported_choices_and_pointer_binding() {
    let mut pty = NativePty::spawn();
    pty.ready();
    pty.command("/thinking");
    pty.wait(|pty| pty.panel().is_some());
    let list = pty.panel().unwrap().clone();
    assert_eq!(list["c"].as_array().unwrap().len(), 3);
    assert!(pty
        .nodes
        .values()
        .any(|node| node["p"]["role"] == "omp.overlay.thinking" && node["p"]["size"] == "sm"));
    pty.activate(
        list["id"].as_str().unwrap(),
        list["c"][2]["id"].as_str().unwrap(),
    );
    pty.wait(|pty| pty.nodes["composer.effort.label"]["p"]["text"] == "high");
    pty.ready();
    pty.close();
}

#[test]
fn native_disabled_composer_bindings_have_no_pointer_authority_and_remapped_picker_confirms() {
    let mut pty = NativePty::configured(Options::default(), |home, _, _| {
        fs::write(
            home.join(".octet/keybindings.json"),
            json!({"tui.input.submit":[], "app.model.select":[], "tui.select.confirm":["ctrl+y"]})
                .to_string(),
        )
        .unwrap();
    });
    pty.ready();
    assert!(pty.nodes["composer.send"]["p"].get("actions").is_none());
    assert!(pty.nodes["composer.model"]["p"].get("actions").is_none());
    pty.send(b"preserve");
    pty.wait(|pty| pty.draft() == "preserve");
    pty.action("composer.send", "send");
    pty.action("composer.model", "model");
    pty.settle();
    assert_eq!(pty.draft(), "preserve");
    assert!(pty.panel().is_none());
    pty.close();
    // A remapped confirmation alone still admits the existing host command.
    let mut pty = NativePty::configured(Options::default(), |home, _, _| {
        fs::write(
            home.join(".octet/keybindings.json"),
            json!({"tui.select.confirm":["ctrl+y"]}).to_string(),
        )
        .unwrap();
    });
    pty.ready();
    pty.command("/thinking");
    pty.wait(|pty| pty.panel().is_some());
    let list = pty.panel().unwrap().clone();
    pty.activate(
        list["id"].as_str().unwrap(),
        list["c"][1]["id"].as_str().unwrap(),
    );
    pty.wait(|pty| pty.nodes["composer.effort.label"]["p"]["text"] == "low");
    pty.ready();
    pty.close();
}

#[test]
fn native_report_close_is_request_fenced_and_stale_composer_edits_are_suppressed() {
    let mut pty = NativePty::spawn();
    pty.ready();
    pty.command("/help");
    pty.wait(|pty| pty.nodes.contains_key("report.body"));
    let old = pty.report_id();
    pty.edit("composer.editor", (0, 0), "must-not-leak", 13, 0);
    pty.settle();
    assert!(pty.draft().is_empty());
    pty.action(&old, "close");
    pty.ready();
    pty.command("/hotkeys");
    pty.wait(|pty| pty.nodes.contains_key("report.body"));
    let current = pty.report_id();
    assert_ne!(current, old);
    pty.action(&old, "close");
    pty.settle();
    assert_eq!(pty.report_id(), current);
    pty.action(&current, "close");
    pty.ready();
    pty.close();
}

#[test]
fn native_shell_failure_disclosure_and_excluded_history_are_real_local_execution() {
    let mut pty = NativePty::spawn();
    pty.ready();
    pty.command("!printf 'failure-output'; exit 7");
    pty.wait(|pty| {
        pty.nodes.values().any(|node| {
            node["id"]
                .as_str()
                .is_some_and(|id| id.ends_with(".command.label"))
                && serde_json::to_string(node).unwrap().contains("exit 7")
        })
    });
    let command = pty.node_kind("code").unwrap().clone();
    let prefix = command["id"]
        .as_str()
        .unwrap()
        .strip_suffix(".command")
        .unwrap();
    let output = format!("{prefix}.out");
    assert!(!String::from_utf8_lossy(&pty.output).contains(&format!("\"{output}\"")));
    // A terminal-owned local toggle cannot bypass global disclosure.
    pty.event(json!({"ev":"toggle","sf":"octet.session","id":format!("{prefix}.shell"),"collapsed":false}));
    pty.settle();
    assert!(!pty.nodes.contains_key(&output));
    pty.command("/verbose on");
    pty.wait(|pty| pty.nodes.contains_key(&output));
    assert!(pty.nodes[&output]["p"]["text"]
        .as_str()
        .unwrap()
        .contains("failure-output"));
    pty.command("/verbose off");
    pty.wait(|pty| !pty.nodes.contains_key(&output));
    pty.command("!!printf 'excluded-output'");
    pty.wait(|pty| {
        pty.nodes
            .values()
            .filter(|node| node["k"] == "code")
            .count()
            == 2
    });
    pty.wait(|pty| {
        pty.nodes
            .values()
            .filter(|node| {
                node["id"]
                    .as_str()
                    .is_some_and(|id| id.ends_with(".command.label"))
                    && serde_json::to_string(node).unwrap().contains("done")
            })
            .count()
            == 1
    });
    pty.close();
    fn transcripts(path: &Path, text: &mut String) {
        for entry in fs::read_dir(path).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                transcripts(&path, text);
            } else if path
                .extension()
                .is_some_and(|extension| extension == "jsonl")
            {
                text.push_str(&fs::read_to_string(path).unwrap());
            }
        }
    }
    let mut saved = String::new();
    transcripts(&pty._root.path().join("sessions"), &mut saved);
    assert!(saved.contains("failure-output"));
    let records = saved
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).unwrap())
        .collect::<Vec<_>>();
    assert!(records
        .iter()
        .any(|record| record["value"]["type"] == "config"
            && serde_json::to_string(&record["metadata"])
                .unwrap()
                .contains("excluded-output")));
    assert!(!records
        .iter()
        .any(|record| record["value"]["type"] == "message"
            && serde_json::to_string(&record["value"])
                .unwrap()
                .contains("excluded-output")));
}

#[test]
fn native_resume_filter_sort_named_paths_search_and_catalogue_fencing_use_saved_source() {
    let mut pty = NativePty::configured(Options::default(), |_, workspace, sessions| {
        seed_session(
            workspace,
            sessions,
            "alpha-history",
            &["Alpha history", "needle alpha"],
        );
        seed_session(workspace, sessions, "beta-history", &["Beta history"]);
    });
    pty.ready();
    pty.command("/resume");
    pty.wait(|pty| pty.node_kind("picker").is_some());
    let picker = pty.node_kind("picker").unwrap().clone();
    let id = picker["id"].as_str().unwrap().to_owned();
    assert!(picker["p"]["items"].as_array().unwrap().len() >= 2);
    assert!(pty.nodes.contains_key("panel.session.facts"));
    let subtitle = picker["p"]["subtitle"].clone();
    pty.action(&id, "sort");
    pty.wait(|pty| pty.node_kind("picker").unwrap()["p"]["subtitle"] != subtitle);
    for label in ["Messages", "Relevance", "Threaded", "Recent"] {
        pty.action(&id, "sort");
        pty.wait(|pty| {
            pty.node_kind("picker").unwrap()["p"]["subtitle"]
                .as_str()
                .unwrap()
                .contains(label)
        });
    }
    pty.action(&id, "paths");
    pty.settle();
    assert!(pty.node_kind("picker").unwrap()["p"]["items"]
        .as_array()
        .unwrap()
        .iter()
        .all(|item| item["detail"].as_str().unwrap().contains(".jsonl")));
    pty.action(&id, "named");
    pty.wait(|pty| {
        pty.node_kind("picker").unwrap()["p"]["order"]
            .as_array()
            .unwrap()
            .is_empty()
    });
    pty.action(&id, "named");
    pty.settle();
    for query in [
        "Beta",
        "\"Beta history\"",
        "re:Beta.*",
        "no-matching-history",
    ] {
        let len = pty.node_kind("picker").unwrap()["p"]["query"]
            .as_str()
            .unwrap()
            .encode_utf16()
            .count();
        pty.edit(&id, (0, len), query, query.encode_utf16().count(), len);
        pty.wait(|pty| pty.node_kind("picker").unwrap()["p"]["query"] == query);
        let count = pty.node_kind("picker").unwrap()["p"]["order"]
            .as_array()
            .unwrap()
            .len();
        if query == "Beta" {
            // Fuzzy matching includes paths; a random temporary-directory name
            // can legitimately match too. The named source must remain present.
            let picker = pty.node_kind("picker").unwrap();
            let beta = picker["p"]["items"]
                .as_array()
                .unwrap()
                .iter()
                .find(|item| item["label"] == "Beta history")
                .unwrap();
            assert!(picker["p"]["order"]
                .as_array()
                .unwrap()
                .contains(&beta["id"]));
        } else {
            assert_eq!(count, usize::from(query != "no-matching-history"));
        }
    }
    pty.edit(&id, (0, 19), "needle alpha", 12, 19);
    pty.wait(|pty| pty.node_kind("picker").unwrap()["p"]["query"] == "needle alpha");
    pty.action(&id, "search");
    pty.wait(|pty| {
        pty.node_kind("picker").unwrap()["p"]["order"]
            .as_array()
            .unwrap()
            .len()
            == 1
    });
    pty.escape();
    pty.ready();
    pty.command("/resume");
    pty.wait(|pty| pty.node_kind("picker").is_some());
    let current = pty.node_kind("picker").unwrap()["id"]
        .as_str()
        .unwrap()
        .to_owned();
    assert_ne!(current, id);
    pty.activate(&id, "0");
    pty.action(&id, "cancel");
    pty.settle();
    assert_eq!(pty.node_kind("picker").unwrap()["id"], current);
    let canonical = pty._root.path().canonicalize().unwrap();
    seed_session(
        &canonical.join("workspace"),
        &canonical.join("sessions"),
        "new-catalogue-entry",
        &["new catalogue source"],
    );
    pty.action(&current, "workspace");
    pty.wait(|pty| {
        let picker = pty.node_kind("picker").unwrap();
        picker["id"] != current
            && picker["p"]["items"]
                .as_array()
                .unwrap()
                .iter()
                .any(|item| item["label"] == "new catalogue source")
    });
    let refreshed = pty.node_kind("picker").unwrap().clone();
    pty.activate(&current, "0");
    pty.action(&current, "cancel");
    pty.settle();
    assert_eq!(pty.node_kind("picker").unwrap()["id"], refreshed["id"]);
    pty.escape();
    pty.ready();
    pty.close();
}

#[test]
fn native_session_rename_cancel_edit_stale_gestures_and_persistence_use_host_driver() {
    let mut metadata_path = PathBuf::new();
    let mut pty = NativePty::configured(Options::default(), |_, workspace, sessions| {
        metadata_path = seed_session(
            workspace,
            sessions,
            "rename-source",
            &["rename source history"],
        )
        .parent()
        .unwrap()
        .join(".metadata/rename-source.json");
    });
    let editor_id = |pty: &NativePty| {
        pty.nodes
            .values()
            .find(|node| {
                node["k"] == "editor"
                    && node["id"]
                        .as_str()
                        .is_some_and(|id| id.starts_with("session-edit."))
            })
            .unwrap()["id"]
            .as_str()
            .unwrap()
            .to_owned()
    };
    pty.ready();
    pty.command("/resume");
    pty.wait(|pty| pty.node_kind("picker").is_some());
    let browse = pty.node_kind("picker").unwrap()["id"]
        .as_str()
        .unwrap()
        .to_owned();
    pty.action(&browse, "rename");
    pty.wait(|pty| {
        pty.nodes
            .values()
            .any(|node| node["p"]["role"] == "octet.session.rename")
    });
    let old = editor_id(&pty);
    let old_len = pty.nodes[&old]["p"]["text"]
        .as_str()
        .unwrap()
        .encode_utf16()
        .count();
    pty.edit(&old, (0, old_len), "R🦀雪", 4, old_len);
    pty.wait(|pty| pty.nodes[&editor_id(pty)]["p"]["text"] == "R🦀雪");
    let current = editor_id(&pty);
    assert_ne!(current, old);
    pty.edit(&old, (0, old_len), "stale", 5, old_len);
    pty.action(
        &format!("{}.confirm", old.strip_suffix(".editor").unwrap()),
        "confirm",
    );
    pty.edit(&current, (1, 2), "invalid", 1, 4);
    pty.settle();
    assert_eq!(pty.nodes[&editor_id(&pty)]["p"]["text"], "R🦀雪");
    pty.send(b"\x7f");
    pty.wait(|pty| pty.nodes[&editor_id(pty)]["p"]["text"] == "R🦀");
    let cancel = format!(
        "{}.cancel",
        editor_id(&pty).strip_suffix(".editor").unwrap()
    );
    pty.action(&cancel, "cancel");
    pty.wait(|pty| pty.node_kind("picker").is_some());
    assert!(pty.node_kind("picker").unwrap()["p"]["items"]
        .as_array()
        .unwrap()
        .iter()
        .any(|item| item["label"] == "rename source history"));
    let browse = pty.node_kind("picker").unwrap()["id"]
        .as_str()
        .unwrap()
        .to_owned();
    pty.action(&browse, "rename");
    pty.wait(|pty| {
        pty.nodes
            .values()
            .any(|node| node["p"]["role"] == "octet.session.rename")
    });
    let current = editor_id(&pty);
    let len = pty.nodes[&current]["p"]["text"]
        .as_str()
        .unwrap()
        .encode_utf16()
        .count();
    let saved = "Saved 🦀 雪";
    pty.edit(&current, (0, len), saved, saved.encode_utf16().count(), len);
    pty.wait(|pty| pty.nodes[&editor_id(pty)]["p"]["text"] == saved);
    let confirm = format!(
        "{}.confirm",
        editor_id(&pty).strip_suffix(".editor").unwrap()
    );
    pty.action(&confirm, "confirm");
    pty.wait(|pty| {
        pty.node_kind("picker").is_some_and(|picker| {
            picker["p"]["items"]
                .as_array()
                .unwrap()
                .iter()
                .any(|item| item["label"] == saved)
        })
    });
    let metadata = metadata_path;
    assert_eq!(
        serde_json::from_str::<Value>(&fs::read_to_string(metadata).unwrap()).unwrap()["name"],
        saved
    );
    assert_eq!(pty.draft(), "");
    pty.escape();
    pty.ready();
    pty.close();
}

#[test]
fn native_active_session_export_writes_saved_history() {
    for mode in ["--resume", "--fork"] {
        let mut pty = NativePty::configured(
            Options {
                args: vec![mode.into(), "export-source".into()],
                ..Options::default()
            },
            |_, workspace, sessions| {
                seed_session(
                    workspace,
                    sessions,
                    "export-source",
                    &["saved export history 🦀"],
                );
            },
        );
        pty.ready();
        pty.command("/export exported.md");
        pty.wait(|pty| pty._root.path().join("workspace/exported.md").is_file());
        let exported = fs::read_to_string(pty._root.path().join("workspace/exported.md")).unwrap();
        assert!(exported.contains("saved export history 🦀"));
        pty.close();
    }
}

#[test]
fn native_resumed_forked_startup_and_fork_picker_preserve_user_history() {
    for mode in ["--resume", "--fork"] {
        let mut pty = NativePty::configured(
            Options {
                args: vec![mode.into(), "qualification-history".into()],
                ..Options::default()
            },
            |_, workspace, sessions| {
                seed_session(
                    workspace,
                    sessions,
                    "qualification-history",
                    &["saved first 🦀", "saved second 雪"],
                );
            },
        );
        pty.ready();
        assert_eq!(
            pty.nodes
                .values()
                .filter(|node| node["p"]["role"] == "omp.user")
                .count(),
            2
        );
        let source = serde_json::to_string(&pty.nodes).unwrap();
        assert!(source.contains("saved first 🦀") && source.contains("saved second 雪"));
        // Active-session export has its own resumed/forked journey above.
        pty.command("/fork");
        pty.wait(|pty| pty.node_kind("picker").is_some());
        assert!(
            pty.node_kind("picker").unwrap()["p"]["items"]
                .as_array()
                .unwrap()
                .len()
                >= 3
        );
        let picker = pty.node_kind("picker").unwrap().clone();
        let item = picker["p"]["items"]
            .as_array()
            .unwrap()
            .iter()
            .find(|item| item["label"].as_str().unwrap().contains("saved second 雪"))
            .unwrap()["id"]
            .as_str()
            .unwrap();
        pty.activate(picker["id"].as_str().unwrap(), item);
        pty.wait(|pty| pty.draft() == "saved second 雪");
        pty.ready();
        pty.close();
    }
}

#[test]
fn native_ordinary_setup_input_raw_and_pointer_edits_overflow_and_sequential_request_fences() {
    let mut pty = NativePty::spawn();
    pty.ready();
    let first = setup_endpoint(&mut pty);
    assert_eq!(pty.nodes["composer.editor"]["p"]["readonly"], true);
    pty.edit(&first, (0, 0), "A🦀雪", 4, 0);
    pty.wait(|pty| pty.nodes[&first]["p"]["text"] == "A🦀雪");
    pty.edit(&first, (1, 2), "bad", 1, 4);
    pty.settle();
    assert_eq!(pty.nodes[&first]["p"]["text"], "A🦀雪");
    pty.send(b"\x7f");
    pty.wait(|pty| pty.nodes[&first]["p"]["text"] == "A🦀");
    pty.edit(&first, (0, 3), &"x".repeat(4097), 4097, 3);
    let prefix = first.strip_suffix(".editor").unwrap();
    pty.wait(|pty| pty.nodes.contains_key(&format!("{prefix}.overflow")));
    // This labeled control clears rejected input; it must not submit it.
    assert_eq!(
        pty.nodes[&format!("{prefix}.confirm")]["p"]["title"],
        "Clear rejected input"
    );
    pty.action(&format!("{prefix}.confirm"), "confirm");
    pty.wait(|pty| !pty.nodes.contains_key(&format!("{prefix}.overflow")));
    assert_eq!(pty.nodes[&first]["p"]["text"], "");
    pty.edit(&first, (0, 0), &"x".repeat(4097), 4097, 0);
    pty.wait(|pty| pty.nodes.contains_key(&format!("{prefix}.overflow")));
    pty.send(b"\r");
    pty.wait(|pty| !pty.nodes.contains_key(&format!("{prefix}.overflow")));
    assert_eq!(pty.nodes[&first]["p"]["text"], "");
    let url = "http://127.0.0.1:9/v1/";
    let len = pty.nodes[&first]["p"]["text"]
        .as_str()
        .unwrap()
        .encode_utf16()
        .count();
    pty.edit(&first, (0, len), url, url.len(), len);
    pty.wait(|pty| pty.nodes[&first]["p"]["text"] == url);
    pty.action(&format!("{prefix}.confirm"), "confirm");
    pty.wait(|pty| {
        pty.nodes
            .values()
            .any(|node| node["p"]["label"] == "Read an API key from an environment variable")
    });
    pty.sheet_choice("Read an API key from an environment variable");
    pty.wait(|pty| {
        pty.nodes.values().any(|node| {
            node["k"] == "editor"
                && node["id"]
                    .as_str()
                    .is_some_and(|id| id.starts_with("prompt.") && id != first)
        })
    });
    let second = pty
        .nodes
        .values()
        .find(|node| {
            node["k"] == "editor"
                && node["id"]
                    .as_str()
                    .is_some_and(|id| id.starts_with("prompt."))
        })
        .unwrap()["id"]
        .as_str()
        .unwrap()
        .to_owned();
    pty.edit(&first, (0, 0), "OLD_REQUEST", 11, 0);
    pty.action(&format!("{prefix}.confirm"), "confirm");
    pty.action(&format!("{prefix}.cancel"), "cancel");
    pty.settle();
    assert_eq!(pty.nodes[&second]["p"]["text"], "");
    assert!(pty.draft().is_empty());
    cancel_setup(&mut pty);
    assert_eq!(pty.cursor(), 0);
    let credentials =
        fs::read_to_string(pty._root.path().join("home/.octet/credentials/custom.json")).unwrap();
    assert!(!credentials.contains("OLD_REQUEST"));
    pty.close();
}

#[test]
fn native_secret_setup_input_never_transports_value_or_accepts_native_confirmation() {
    let mut pty = NativePty::spawn();
    pty.ready();
    let endpoint = setup_endpoint(&mut pty);
    let prefix = endpoint.strip_suffix(".editor").unwrap();
    let url = "http://127.0.0.1:9/v1/";
    pty.edit(&endpoint, (0, 0), url, url.len(), 0);
    pty.wait(|pty| pty.nodes[&endpoint]["p"]["text"] == url);
    pty.action(&format!("{prefix}.confirm"), "confirm");
    pty.wait(|pty| {
        pty.nodes
            .values()
            .any(|node| node["p"]["label"] == "Enter an API key")
    });
    pty.sheet_choice("Enter an API key");
    pty.wait(|pty| {
        pty.nodes.values().any(|node| {
            node["id"]
                .as_str()
                .is_some_and(|id| id.starts_with("prompt.") && id.ends_with(".prompt"))
                && node["p"]["text"] == "API key:"
        })
    });
    let prompt = pty
        .nodes
        .values()
        .find(|node| node["p"]["text"] == "API key:")
        .unwrap()["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let prefix = prompt.strip_suffix(".prompt").unwrap();
    assert!(!pty.nodes.contains_key(&format!("{prefix}.editor")));
    assert!(!pty.nodes.contains_key(&format!("{prefix}.confirm")));
    let secret = "HOST_PRIVATE_TEST_NOT_A_CREDENTIAL";
    pty.send(secret.as_bytes());
    pty.action(&format!("{prefix}.confirm"), "confirm");
    pty.settle();
    assert!(pty.nodes.contains_key(&prompt));
    assert!(!String::from_utf8_lossy(&pty.output).contains(secret));
    pty.action(&format!("{prefix}.cancel"), "cancel");
    pty.wait(|pty| !pty.nodes.contains_key(&prompt));
    cancel_setup(&mut pty);
    pty.close();
    assert!(
        !fs::read_to_string(pty._root.path().join("home/.octet/credentials/custom.json"))
            .unwrap()
            .contains(secret)
    );
}

#[test]
fn native_resize_burst_keeps_protocol_and_fresh_input_live() {
    let mut pty = NativePty::spawn();
    pty.ready();
    for cycle in 0..3 {
        for cols in [118, 102, 90, 81, 74, 70, 66, 64, 62, 61, 60, 59, 58, 57] {
            pty.resize(cols, 38);
        }
        let marker = format!(" RETURN{cycle}");
        pty.send(marker.as_bytes());
        let expected = (0..=cycle)
            .map(|n| format!(" RETURN{n}"))
            .collect::<String>();
        pty.wait(|pty| pty.draft() == expected);
    }
    pty.send(&[21]);
    pty.wait(|pty| pty.draft().is_empty());
    pty.close();
}

#[test]
fn native_pointer_focus_reasserts_host_owner_and_restores_notifications() {
    let mut pty = NativePty::spawn();
    pty.ready();
    assert!(pty.output.windows(8).any(|bytes| bytes == b"\x1b[?1004h"));
    pty.settle();
    let frames = pty.frames;
    pty.event(json!({"ev":"focus","sf":"foreign.surface","id":"composer.editor"}));
    pty.event(json!({"ev":"focus","sf":"octet.session","id":"unknown.editor"}));
    pty.settle();
    assert_eq!(pty.frames, frames);
    let before = pty.messages.len();
    pty.event(json!({"ev":"focus","sf":"octet.session","id":"composer.editor"}));
    pty.wait(|pty| {
        pty.messages[before..].iter().any(|(verb, body)| {
            verb == "f"
                && body["ops"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|op| op == &json!(["focus", "composer.editor"]))
        })
    });
    pty.send(b"fresh");
    pty.wait(|pty| pty.draft() == "fresh");
    pty.send(&[21]);
    pty.wait(|pty| pty.draft().is_empty());
    pty.close();
    assert!(pty.output.windows(8).any(|bytes| bytes == b"\x1b[?1004l"));
}

#[test]
fn native_resize_focus_visibility_and_eviction_reassert_focus_without_losing_draft() {
    let mut pty = NativePty::spawn();
    pty.ready();
    pty.send("\x1b[200~retained 🦀\n雪\x1b[201~".as_bytes());
    pty.wait(|pty| pty.draft() == "retained 🦀\n雪");
    let cursor = pty.cursor();
    for (cols, rows) in [(37, 8), (140, 48), (70, 12)] {
        pty.resize(cols, rows);
        pty.settle();
        // Native geometry may reflow without changed props or a new frame.
        assert_eq!(pty.draft(), "retained 🦀\n雪");
        assert_eq!(pty.cursor(), cursor);
        pty.send(b"x");
        pty.wait(|pty| pty.draft() == "retained 🦀\n雪x");
        pty.send(b"\x7f");
        pty.wait(|pty| pty.draft() == "retained 🦀\n雪");
        assert_eq!(pty.cursor(), cursor);
    }
    pty.event(json!({"ev":"theme","dark":false}));
    pty.event(json!({"ev":"motion","reduce":false}));
    pty.settle();
    let before = pty.messages.len();
    pty.send(b"\x1b[O\x1b[I");
    pty.wait(|pty| {
        pty.messages[before..].iter().any(|(verb, body)| {
            verb == "f"
                && body["ops"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|op| op == &json!(["focus", "composer.editor"]))
        })
    });
    pty.event(json!({"ev":"visible","sf":"octet.session","visible":false}));
    pty.settle();
    let frames = pty.frames;
    pty.send(b"!");
    pty.settle();
    assert_eq!(pty.frames, frames);
    pty.event(json!({"ev":"error","sf":"octet.session","msg":"synthetic hidden-pane rejection"}));
    pty.settle();
    pty.event(json!({"ev":"visible","sf":"octet.session","visible":true}));
    pty.wait(|pty| pty.draft() == "retained 🦀\n雪!");
    let opens = pty.messages.iter().filter(|(verb, _)| verb == "o").count();
    pty.nodes.clear();
    pty.parents.clear();
    pty.event(json!({"ev":"gone","sf":"octet.session","ids":["octet.session"]}));
    pty.wait(|pty| {
        pty.messages.iter().filter(|(verb, _)| verb == "o").count() > opens
            && pty.draft() == "retained 🦀\n雪!"
    });
    for region in ["main", "dock", "layer"] {
        assert!(pty.nodes.contains_key(region));
    }
    assert!(!String::from_utf8_lossy(&pty.output).contains("Native Tern rendering unavailable"));
    pty.close();
}

#[test]
fn native_credit_starvation_coalesces_edits_until_actual_frame_acknowledgement() {
    let mut pty = NativePty::configured(
        Options {
            credits: 1,
            ..Options::default()
        },
        |_, _, _| {},
    );
    pty.ready();
    pty.settle();
    pty.acknowledge = false;
    pty.send(b"a");
    pty.wait(|pty| pty.draft() == "a");
    let frames = pty.frames;
    pty.send(b"bcdef");
    pty.settle();
    assert_eq!(pty.frames, frames);
    assert_eq!(pty.draft(), "a");
    assert_eq!(pty.withheld.len(), 1);
    pty.acknowledge = true;
    for ack in std::mem::take(&mut pty.withheld) {
        pty.event(ack);
    }
    pty.wait(|pty| pty.draft() == "abcdef");
    assert!(!String::from_utf8_lossy(&pty.output).contains("Native Tern rendering unavailable"));
    pty.close();
}

#[test]
fn native_scroll_forwarding_requires_advertisement_and_does_not_scroll_behind_report() {
    for supported in [false, true] {
        let mut pty = NativePty::configured(
            Options {
                features: if supported {
                    vec!["scroll".into()]
                } else {
                    Vec::new()
                },
                ..Options::default()
            },
            |_, _, _| {},
        );
        pty.ready();
        pty.send(b"\x1b[5~");
        pty.settle();
        assert_eq!(
            pty.ops()
                .any(|op| op[0] == "scroll" && op[1] == "main" && op[2] == "page-up"),
            supported
        );
        pty.command("/help");
        pty.wait(|pty| pty.nodes.contains_key("report.body"));
        let before = pty.messages.len();
        pty.send(b"\x1b[6~");
        pty.settle();
        // Report keys retain their host owner. This does NOT claim a native
        // report scroll target exists; only transcript forwarding is implemented.
        assert!(!pty.messages[before..]
            .iter()
            .filter(|(verb, _)| verb == "f")
            .flat_map(|(_, body)| body["ops"].as_array().unwrap())
            .any(|op| op[0] == "scroll"));
        assert!(pty.nodes.contains_key("report.body"));
        pty.escape();
        pty.ready();
        pty.close();
    }
}

#[test]
fn native_optional_image_kind_falls_back_but_missing_required_editor_explicitly_yields_to_ansi() {
    let mut pty = NativePty::configured(
        Options {
            kinds: TSP_KINDS
                .iter()
                .filter(|kind| **kind != "image")
                .map(|kind| (*kind).to_owned())
                .collect(),
            ..Options::default()
        },
        |_, _, _| {},
    );
    pty.ready();
    assert!(pty.node_kind("image").is_none());
    assert!(!String::from_utf8_lossy(&pty.output).contains("Native Tern rendering unavailable"));
    pty.close();
    let mut pty = NativePty::configured(
        Options {
            kinds: TSP_KINDS
                .iter()
                .filter(|kind| **kind != "editor")
                .map(|kind| (*kind).to_owned())
                .collect(),
            ..Options::default()
        },
        |_, _, _| {},
    );
    pty.wait(|pty| {
        String::from_utf8_lossy(&pty.output).contains("Native Tern rendering unavailable")
    });
    assert!(String::from_utf8_lossy(&pty.output).contains("native surface vocabulary"));
    let frames = pty.frames;
    pty.send(b"safe fallback draft");
    pty.settle();
    assert_eq!(pty.frames, frames);
    pty.close();
}

#[test]
fn native_extensions_empty_and_untrusted_installed_menus_cannot_grant_by_pointer() {
    let mut pty = NativePty::spawn();
    pty.ready();
    pty.command("/extensions");
    pty.wait(|pty| pty.nodes.contains_key("panel.document"));
    assert!(pty.nodes["panel.document"]["p"]["text"]
        .as_str()
        .unwrap()
        .contains("No"));
    pty.escape();
    pty.ready();
    pty.close();
    let mut pty = NativePty::configured(
        Options {
            args: vec!["--safe".into()],
            ..Options::default()
        },
        |home, workspace, _| {
            let extension = home.join(".octet/extensions/qualification-untrusted");
            fs::create_dir_all(&extension).unwrap();
            fs::write(extension.join("extension.toml"), "name = \"qualification-untrusted\"\nversion = \"0.1.0\"\napi_version = \"0.1\"\n[entrypoint]\ncommand = \"probe.sh\"\n").unwrap();
            let script = extension.join("probe.sh");
            fs::write(
                &script,
                format!(
                    "#!/bin/sh\nprintf executed > '{}'\n",
                    workspace.join("must-not-execute").display()
                ),
            )
            .unwrap();
            fs::set_permissions(&script, fs::Permissions::from_mode(0o700)).unwrap();
            fs::write(
                home.join(".octet/config.toml"),
                "enabled_extensions = [\"qualification-untrusted\"]\n",
            )
            .unwrap();
        },
    );
    pty.ready();
    pty.command("/extensions");
    pty.wait(|pty| pty.panel().is_some());
    let root = pty.panel().unwrap().clone();
    pty.activate(
        root["id"].as_str().unwrap(),
        root["c"][0]["id"].as_str().unwrap(),
    );
    pty.wait(|pty| {
        pty.nodes
            .values()
            .any(|node| node["p"]["label"] == "Grant host authority")
    });
    let grant = pty
        .nodes
        .values()
        .find(|node| node["p"]["label"] == "Grant host authority")
        .unwrap()["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let menu = pty.parents[&grant].clone();
    pty.activate(&menu, &grant);
    pty.wait(|pty| {
        pty.nodes
            .values()
            .any(|node| node["p"]["role"] == "octet.modal")
    });
    // Consent is deliberately not an interactive native picker. Forged positive
    // pointer events must not grant even after the synthetic frame ack.
    let modal = pty
        .nodes
        .values()
        .find(|node| node["p"]["role"] == "octet.modal")
        .unwrap()["id"]
        .as_str()
        .unwrap()
        .to_owned();
    pty.activate(&modal, "0");
    pty.action(&modal, "confirm");
    pty.settle();
    assert!(!pty._root.path().join("workspace/must-not-execute").exists());
    assert!(
        !fs::read_to_string(pty._root.path().join("home/.octet/config.toml"))
            .unwrap()
            .contains("trusted_extensions")
    );
    pty.escape();
    pty.settle();
    pty.escape();
    pty.settle();
    pty.escape();
    pty.ready();
    pty.close();
}

#[test]
fn native_loopback_stream_reasoning_markdown_working_finalization_and_draft_are_real_runtime() {
    let server = SyntheticStream::new();
    let mut pty = server.pty();
    pty.ready();
    pty.command("qualified loopback user prompt");
    pty.wait(|pty| {
        pty.nodes
            .values()
            .any(|node| node["k"] == "md" && node["p"]["text"] == "native streaming head")
    });
    let leaf = pty
        .nodes
        .values()
        .find(|node| node["k"] == "md" && node["p"]["text"] == "native streaming head")
        .unwrap()["id"]
        .as_str()
        .unwrap()
        .to_owned();
    assert_eq!(pty.nodes[&leaf]["p"]["stream"], true);
    let parent = &pty.nodes[&pty.parents[&leaf]];
    assert_eq!(parent["k"], "col");
    assert_eq!(parent["p"]["role"], "omp.assistant");
    assert!(pty
        .nodes
        .values()
        .any(|node| node["p"]["role"] == "omp.working"));
    assert!(pty
        .nodes
        .values()
        .any(|node| node["p"]["role"] == "omp.thinking"));
    assert!(pty
        .nodes
        .values()
        .any(|node| node["p"]["role"] == "omp.user"));
    pty.send("unsent draft 🦀".as_bytes());
    pty.wait(|pty| pty.draft() == "unsent draft 🦀");
    let caret = pty.cursor();
    pty.send(&[15]);
    pty.settle();
    assert!(pty
        .nodes
        .values()
        .any(|node| node["p"]["role"] == "omp.thinking" && node["p"]["collapsed"] == false));
    server.release.send(true).unwrap();
    pty.wait(|pty| {
        pty.nodes.get(&leaf).is_some_and(|node| {
            node["p"]["stream"] == false
                && node["p"]["text"]
                    .as_str()
                    .unwrap()
                    .contains("native final tail")
        }) && !pty
            .nodes
            .values()
            .any(|node| node["p"]["role"] == "omp.working")
    });
    assert!(pty.nodes[&leaf]["p"]["text"]
        .as_str()
        .unwrap()
        .contains("kept\n\n\nblank lines"));
    assert_eq!(pty.draft(), "unsent draft 🦀");
    assert_eq!(pty.cursor(), caret);
    let requests = server.requests.lock().unwrap();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0]["model"], "probe");
    assert!(serde_json::to_string(&requests[0])
        .unwrap()
        .contains("qualified loopback user prompt"));
    assert!(!serde_json::to_string(&requests[0])
        .unwrap()
        .contains("unsent draft"));
    drop(requests);
    pty.close();
}

#[test]
fn native_loopback_cancel_settles_without_dispatching_followup_and_can_recall_it() {
    let server = SyntheticStream::new();
    let mut pty = server.pty();
    pty.ready();
    pty.command("loopback interrupt prompt");
    pty.wait(|pty| {
        pty.nodes
            .values()
            .any(|node| node["k"] == "md" && node["p"]["text"] == "native streaming head")
    });
    pty.command("queued followup not authorized for automatic retry");
    pty.wait(|pty| pty.draft().is_empty() && pty.nodes.contains_key("pending"));
    // Ctrl+C is cancellation without dispatch. A retained answer is not proof
    // of settlement: require the actual run owner to remove Working/Stop.
    pty.send(&[3]);
    pty.wait(|pty| {
        !pty.nodes.contains_key("composer.stop")
            && !pty
                .nodes
                .values()
                .any(|node| node["p"]["role"] == "omp.working")
    });
    assert_eq!(server.requests.lock().unwrap().len(), 1);
    pty.send(b"\x1b[1;3A");
    pty.wait(|pty| pty.draft() == "queued followup not authorized for automatic retry");
    assert_eq!(server.requests.lock().unwrap().len(), 1);
    assert!(!pty.nodes.contains_key("pending"));
    pty.close();
}

#[test]
fn native_close_from_ordinary_input_owner_exits_without_provider_save() {
    let mut pty = NativePty::spawn();
    pty.ready();
    let input = setup_endpoint(&mut pty);
    pty.edit(&input, (0, 0), "unfinished", 10, 0);
    pty.wait(|pty| pty.nodes[&input]["p"]["text"] == "unfinished");
    pty.close();
    assert!(
        !fs::read_to_string(pty._root.path().join("home/.octet/credentials/custom.json"))
            .unwrap()
            .contains("unfinished")
    );
}

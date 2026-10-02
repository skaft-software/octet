//! Native TSP against the real input parser, without Tern or a live provider.
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
    os::{
        fd::FromRawFd,
        unix::{fs::PermissionsExt, process::CommandExt},
    },
    process::{Child, Command, Stdio},
    thread,
    time::{Duration, Instant},
};
use tempfile::TempDir;

struct NativePty {
    child: Child,
    master: File,
    _slave: File,
    _root: TempDir,
    pending: Vec<u8>,
    output: Vec<u8>,
    nodes: HashMap<String, Value>,
    frames: usize,
    hello: bool,
}

impl NativePty {
    fn spawn() -> Self {
        let root = tempfile::tempdir().unwrap();
        let home = root.path().join("home");
        let workspace = root.path().join("workspace");
        fs::create_dir_all(home.join(".octet/credentials")).unwrap();
        fs::create_dir_all(&workspace).unwrap();
        let credentials = home.join(".octet/credentials/custom.json");
        fs::write(&credentials, r#"{"base_url":"http://127.0.0.1:9/v1/","api_key":"","api_name":"native-test","headers":[],"models":[{"api_name":"probe"}],"auto_discover":false}"#).unwrap();
        fs::set_permissions(&credentials, fs::Permissions::from_mode(0o600)).unwrap();
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
        command
            .args([
                "--offline",
                "--no-context-files",
                "--no-tools",
                "--theme",
                "dark",
                "--model",
                "custom/probe",
            ])
            .arg("--workspace")
            .arg(&workspace)
            .arg("--session-dir")
            .arg(root.path().join("sessions"))
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
            frames: 0,
            hello: false,
        }
    }

    fn send(&mut self, input: &[u8]) {
        self.master.write_all(input).unwrap();
        self.master.flush().unwrap();
    }
    fn event(&mut self, event: Value) {
        self.send(
            frame::encode_json(Verb::Event, &event, 65536)
                .unwrap()
                .as_bytes(),
        );
    }
    fn add(&mut self, node: &Value) {
        self.nodes
            .insert(node["id"].as_str().unwrap().into(), node.clone());
        if let Some(children) = node["c"].as_array() {
            for child in children {
                self.add(child);
            }
        }
    }
    fn delete(&mut self, id: &str) {
        if let Some(node) = self.nodes.remove(id) {
            if let Some(children) = node["c"].as_array() {
                for child in children {
                    self.delete(child["id"].as_str().unwrap());
                }
            }
        }
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
            let body: Value = serde_json::from_str(&raw.body).unwrap();
            match raw.verb.as_str() {
                "q" if body["q"] == "hello" => {
                    self.hello = true;
                    let hello = json!({"r":"hello","v":1,"term":"headless-test","kinds":TSP_KINDS,"apc":65536,"credits":2,"cols":100,"dark":true,"reduceMotion":true});
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
                            "add" => self.add(&op[4]),
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
                            "move" | "focus" | "reveal" => {}
                            other => panic!("unexpected native op {other}"),
                        }
                    }
                    self.event(json!({"ev":"ack","sf":body["sf"],"s":body["s"]}));
                }
                _ => {}
            }
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
    pty.wait(|pty| pty.nodes.values().any(|node| node["k"] == "picker"));
    pty.send(b"\x1b[27u");
    pty.wait(|pty| !pty.nodes.values().any(|node| node["k"] == "picker"));
    assert!(!String::from_utf8_lossy(&pty.output).contains("Native Tern rendering unavailable"));
    assert!(!pty.nodes.values().any(|node| node["k"] == "rows"));
    pty.close();
}

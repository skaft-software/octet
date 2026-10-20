//! Dropped attachments reach the provider request, not just their chips.
//!
//! Drives the real binary under a PTY the way a terminal file drop does (one
//! bracketed paste of the path), submits, and inspects the Chat Completions
//! request a loopback provider receives. Isolated HOME/workspace, --offline,
//! no tools/context files and no live credentials.

#![cfg(unix)]

use std::fs::{self, File};
use std::io::{self, BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::os::fd::FromRawFd;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use tempfile::TempDir;

const TIMEOUT: Duration = Duration::from_secs(20);
const PNG_1X1: &[u8] = &[
    0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x48, 0x44, 0x52,
    0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00, 0x00, 0x1f, 0x15, 0xc4,
    0x89, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x44, 0x41, 0x54, 0x78, 0x9c, 0x63, 0xf8, 0xcf, 0xc0, 0xf0,
    0x1f, 0x00, 0x05, 0x00, 0x01, 0xff, 0x89, 0x99, 0x3d, 0x1d, 0x00, 0x00, 0x00, 0x00, 0x49, 0x45,
    0x4e, 0x44, 0xae, 0x42, 0x60, 0x82,
];

/// A one-shot Chat Completions endpoint that records each request body.
fn loopback_provider() -> (u16, mpsc::Receiver<serde_json::Value>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let (sender, receiver) = mpsc::channel();
    thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { return };
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut length = 0usize;
            let mut request_line = String::new();
            if reader.read_line(&mut request_line).is_err() {
                continue;
            }
            loop {
                let mut line = String::new();
                if reader.read_line(&mut line).unwrap_or(0) == 0 || line == "\r\n" {
                    break;
                }
                if let Some(value) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                    length = value.trim().parse().unwrap_or(0);
                }
            }
            let mut body = vec![0; length];
            if reader.read_exact(&mut body).is_err() {
                continue;
            }
            if !request_line.contains("/chat/completions") {
                let _ = stream.write_all(
                    b"HTTP/1.1 404 Not Found\r\ncontent-length: 0\r\nconnection: close\r\n\r\n",
                );
                continue;
            }
            if let Ok(json) = serde_json::from_slice(&body) {
                let _ = sender.send(json);
            }
            let events = concat!(
                "data: {\"id\":\"c\",\"object\":\"chat.completion.chunk\",\"choices\":[{\"index\":0,\"delta\":{\"role\":\"assistant\",\"content\":\"received\"},\"finish_reason\":null}]}\n\n",
                "data: {\"id\":\"c\",\"object\":\"chat.completion.chunk\",\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}],\"usage\":{\"prompt_tokens\":1,\"completion_tokens\":1,\"total_tokens\":2}}\n\n",
                "data: [DONE]\n\n",
            );
            let _ = write!(
                stream,
                "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{events}",
                events.len()
            );
        }
    });
    (port, receiver)
}

struct PtyOctet {
    child: Child,
    master: File,
    _slave: File,
    root: TempDir,
    parser: vt100::Parser,
}

impl PtyOctet {
    fn spawn(port: u16) -> Self {
        let root = tempfile::tempdir().unwrap();
        let home = root.path().join("home");
        let workspace = root.path().join("workspace");
        let sessions = root.path().join("sessions");
        fs::create_dir_all(home.join(".octet/credentials")).unwrap();
        fs::create_dir_all(&workspace).unwrap();
        fs::create_dir_all(&sessions).unwrap();
        let credentials = home.join(".octet/credentials/custom.json");
        fs::write(
            &credentials,
            format!(
                r#"{{"base_url":"http://127.0.0.1:{port}/v1/","api_key":"","api_name":"probe","headers":[],"models":[{{"api_name":"probe","context_window":32768,"vision":true,"tools":false}}],"auto_discover":false}}"#
            ),
        )
        .unwrap();
        fs::set_permissions(&credentials, fs::Permissions::from_mode(0o600)).unwrap();

        let (mut master_fd, mut slave_fd) = (-1, -1);
        let mut size = libc::winsize {
            ws_row: 30,
            ws_col: 120,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };
        // SAFETY: all output pointers refer to live locals; the two successful
        // descriptors are immediately owned by Files below.
        let result = unsafe {
            #[allow(clippy::unnecessary_mut_passed)]
            libc::openpty(
                &mut master_fd,
                &mut slave_fd,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                &mut size,
            )
        };
        assert_eq!(result, 0, "openpty: {}", io::Error::last_os_error());
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
                "--model",
                "custom/probe",
                "--theme",
                "dark",
            ])
            .arg("--workspace")
            .arg(&workspace)
            .arg("--session-dir")
            .arg(&sessions)
            .current_dir(&workspace)
            .env_clear()
            .env("HOME", &home)
            .env("PATH", "/usr/bin:/bin")
            .env("PWD", &workspace)
            .env("TERM", "xterm-256color")
            .env("LANG", "C.UTF-8")
            .stdin(Stdio::from(slave.try_clone().unwrap()))
            .stdout(Stdio::from(slave.try_clone().unwrap()))
            .stderr(Stdio::from(slave.try_clone().unwrap()));
        // SAFETY: the pre-exec callback uses only async-signal-safe Unix calls;
        // the parent retains the slave until after its child exits.
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
        let mut pty = Self {
            child: command.spawn().expect("spawn offline octet under PTY"),
            master,
            _slave: slave,
            root,
            parser: vt100::Parser::new(30, 120, 0),
        };
        pty.wait_for_screen("custom/probe");
        pty
    }

    fn file(&self, name: &str, bytes: &[u8]) -> PathBuf {
        let path = self.root.path().join("drops").join(name);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, bytes).unwrap();
        path
    }

    fn pump(&mut self) {
        let mut buffer = [0; 8192];
        loop {
            match self.master.read(&mut buffer) {
                Ok(0) => return,
                Ok(read) => {
                    let chunk = &buffer[..read];
                    // Answer the background-colour probe so startup never waits.
                    if chunk.windows(6).any(|window| window == b"\x1b]11;?") {
                        let _ = self.master.write_all(b"\x1b]11;rgb:0000/0000/0000\x1b\\");
                    }
                    self.parser.process(chunk);
                }
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => return,
                Err(error) if error.raw_os_error() == Some(libc::EIO) => return,
                Err(error) => panic!("read PTY: {error}"),
            }
        }
    }

    fn screen(&self) -> String {
        self.parser.screen().contents()
    }

    fn wait_for_screen(&mut self, text: &str) {
        let deadline = Instant::now() + TIMEOUT;
        while Instant::now() < deadline {
            self.pump();
            if self.screen().contains(text) {
                return;
            }
            thread::sleep(Duration::from_millis(20));
        }
        panic!("screen never showed {text:?}:\n{}", self.screen());
    }

    fn send(&mut self, input: &[u8]) {
        self.master.write_all(input).unwrap();
        self.master.flush().unwrap();
    }

    /// One terminal drop: the escaped path inside a single bracketed paste.
    fn drop_path(&mut self, path: &Path) {
        let escaped = path.display().to_string().replace(' ', "\\ ");
        self.send(format!("\x1b[200~{escaped}\x1b[201~").as_bytes());
    }

    fn submit(&mut self, provider: &mpsc::Receiver<serde_json::Value>) -> serde_json::Value {
        self.send(b"\r");
        let deadline = Instant::now() + TIMEOUT;
        loop {
            self.pump();
            if let Ok(body) = provider.try_recv() {
                return body;
            }
            assert!(
                Instant::now() < deadline,
                "the provider never received the submitted prompt:\n{}",
                self.screen()
            );
            thread::sleep(Duration::from_millis(20));
        }
    }
}

impl Drop for PtyOctet {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// The last user message, flattened to its text and its image data URIs.
fn last_user_message(body: &serde_json::Value) -> (String, Vec<String>) {
    let message = body["messages"]
        .as_array()
        .unwrap()
        .iter()
        .rev()
        .find(|message| message["role"] == "user")
        .expect("a user message");
    match &message["content"] {
        serde_json::Value::String(text) => (text.clone(), Vec::new()),
        serde_json::Value::Array(parts) => {
            let mut text = String::new();
            let mut images = Vec::new();
            for part in parts {
                match part["type"].as_str() {
                    Some("text") => text.push_str(part["text"].as_str().unwrap()),
                    Some("image_url") => {
                        images.push(part["image_url"]["url"].as_str().unwrap().to_owned())
                    }
                    other => panic!("unexpected user content part {other:?}"),
                }
            }
            (text, images)
        }
        other => panic!("unexpected user content {other}"),
    }
}

#[test]
fn dropped_pdf_reaches_the_model_as_its_path_not_its_chip() {
    let (port, provider) = loopback_provider();
    let mut pty = PtyOctet::spawn(port);
    let pdf = pty.file("Quarterly Report.pdf", b"%PDF-1.4\n%%EOF\n");
    pty.send(b"summarize ");
    pty.drop_path(&pdf);
    pty.wait_for_screen("[PDF #1]");
    let (text, images) = last_user_message(&pty.submit(&provider));
    assert!(
        text.contains(&pdf.display().to_string()),
        "the model must be told which file was attached: {text:?}"
    );
    assert!(
        !text.contains("[PDF #1]"),
        "a bare chip names no file: {text:?}"
    );
    assert!(images.is_empty());
}

#[test]
fn dropped_image_and_large_paste_reach_the_model_with_their_content() {
    let (port, provider) = loopback_provider();
    let mut pty = PtyOctet::spawn(port);
    let image = pty.file("screen shot.png", PNG_1X1);
    pty.send(b"compare ");
    pty.drop_path(&image);
    pty.wait_for_screen("[Image #1]");
    let pasted = (1..=40)
        .map(|line| format!("pasted line {line}"))
        .collect::<Vec<_>>()
        .join("\n");
    pty.send(format!(" with \x1b[200~{pasted}\x1b[201~").as_bytes());
    pty.wait_for_screen("[Pasted text #2");
    let (text, images) = last_user_message(&pty.submit(&provider));
    assert_eq!(images.len(), 1, "the image bytes travel with the prompt");
    assert!(images[0].starts_with("data:image/png;base64,"));
    assert!(
        text.contains(&format!("[attached image: {}]", image.display())),
        "{text:?}"
    );
    assert!(text.contains("pasted line 1\n") && text.contains("pasted line 40"));
    assert!(!text.contains("[Pasted text #2"), "{text:?}");
}

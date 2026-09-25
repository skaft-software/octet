//! Protocol event emission and host-process transport cleanup.
//!
//! The emitter owns sequencing, terminal-event rules, bounded serialization,
//! and stdout flushing. Process-group cleanup is kept here as transport-level
//! lifecycle work rather than mixed into request policy.

use serde::Serialize;
use tokio::io::AsyncWriteExt;

use super::framing::serialize_bounded;
use super::protocol::{MAX_FRAME_BYTES, PROTOCOL_VERSION};

#[derive(Serialize)]
struct HostEvent<'a> {
    protocol_version: u16,
    request_id: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    run_id: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    session_id: Option<&'a str>,
    seq: u64,
    #[serde(rename = "type")]
    event_type: &'a str,
    data: serde_json::Value,
}

pub(crate) struct Emitter<'a> {
    output: &'a mut tokio::io::Stdout,
    request_id: String,
    run_id: Option<String>,
    session_id: Option<String>,
    seq: u64,
    terminal: bool,
}

impl<'a> Emitter<'a> {
    pub(crate) fn new(output: &'a mut tokio::io::Stdout, request_id: String) -> Self {
        Self {
            output,
            request_id,
            run_id: None,
            session_id: None,
            seq: 0,
            terminal: false,
        }
    }

    pub(crate) fn scoped(mut self, run_id: String, session_id: Option<String>) -> Self {
        self.run_id = Some(run_id);
        self.session_id = session_id;
        self
    }

    pub(crate) fn is_terminal(&self) -> bool {
        self.terminal
    }

    pub(crate) async fn emit(
        &mut self,
        event_type: &str,
        data: impl Serialize,
    ) -> anyhow::Result<()> {
        if self.terminal {
            anyhow::bail!("request already emitted a terminal protocol event");
        }
        self.seq = self.seq.saturating_add(1);
        let data = serde_json::to_value(data)?;
        let event = HostEvent {
            protocol_version: PROTOCOL_VERSION,
            request_id: &self.request_id,
            run_id: self.run_id.as_deref(),
            session_id: self.session_id.as_deref(),
            seq: self.seq,
            event_type,
            data,
        };
        let terminal = matches!(
            event_type,
            "final_result" | "protocol_error" | "hello" | "models" | "shutdown"
        );
        let serialized = serialize_bounded(&event, MAX_FRAME_BYTES.saturating_sub(1))?;
        let oversized = serialized.is_none();
        let mut line = match serialized {
            Some(line) => line,
            None => {
                self.terminal = true;
                eprintln!("octet-host: dropping oversized outbound {event_type} event");
                serde_json::to_vec(&HostEvent {
                    protocol_version: PROTOCOL_VERSION,
                    request_id: &self.request_id,
                    run_id: self.run_id.as_deref(),
                    session_id: self.session_id.as_deref(),
                    seq: self.seq,
                    event_type: "protocol_error",
                    data: serde_json::json!({
                        "error": "outbound event exceeded the protocol frame limit",
                        "discarded_type": event_type,
                    }),
                })?
            }
        };
        if line.len().saturating_add(1) > MAX_FRAME_BYTES {
            anyhow::bail!("protocol error event exceeded the outbound frame limit");
        }
        line.push(b'\n');
        self.output.write_all(&line).await?;
        self.output.flush().await?;
        if terminal {
            self.terminal = true;
        }
        if oversized {
            anyhow::bail!("outbound event exceeded the protocol frame limit");
        }
        Ok(())
    }
}

pub(crate) async fn cleanup_host_processes() {
    octet_agent::extension_process::begin_host_shutdown();
    octet_agent::extension_process::terminate_bash_process_groups(
        std::time::Duration::from_millis(400),
    )
    .await;
    octet_agent::extension_process::force_kill_registered_process_groups();
}

#![allow(missing_docs)]

//! Versioned, bounded NDJSON host for non-Rust consumers.
//!
//! Standard output is protocol-only. Provider, discovery, and diagnostic logs
//! remain on standard error so one malformed dependency cannot corrupt IPC.
//!
//! The host facade owns only process lifecycle and decoded-command dispatch;
//! protocol DTOs/framing, transport, routing policy, orchestration, media, and
//! session authority live in private child modules.

mod events;
mod framing;
mod media;
mod policy;
mod protocol;
mod routing;
mod run;
mod sessions;
mod transport;

pub use protocol::{MAX_FRAME_BYTES, PROTOCOL_VERSION};

use tokio::io::BufReader;

use protocol::{parse_request, valid_protocol_id, HostCommand};
use routing::{emit_hello, emit_models};
use run::RunRequestOutcome;
use sessions::valid_session_id;
use transport::Emitter;

async fn cleanup_host_processes() {
    transport::cleanup_host_processes().await;
}

/// Serve bounded NDJSON requests until `shutdown`, EOF, or a termination signal.
pub async fn run_stdio() -> anyhow::Result<()> {
    crate::tui::terminal::install_signal_restore()?;
    let result = run_stdio_loop().await;
    cleanup_host_processes().await;
    crate::tui::terminal::exit_if_signaled();
    result
}

async fn run_stdio_loop() -> anyhow::Result<()> {
    let stdin = tokio::io::stdin();
    let mut input = BufReader::new(stdin);
    let mut output = tokio::io::stdout();

    loop {
        let frame = tokio::select! {
            biased;
            _ = crate::tui::terminal::wait_for_shutdown_signal() => break,
            frame = framing::read_frame(&mut input) => frame?,
        };
        let Some(frame) = frame else {
            break;
        };
        let bytes = match frame {
            framing::Frame::Data(bytes) => bytes,
            framing::Frame::Incomplete => {
                let mut emitter = Emitter::new(&mut output, "invalid".to_owned());
                emitter
                    .emit(
                        "protocol_error",
                        serde_json::json!({"error": "incomplete protocol frame at EOF"}),
                    )
                    .await?;
                break;
            }
            framing::Frame::Oversized => {
                let mut emitter = Emitter::new(&mut output, "invalid".to_owned());
                emitter
                    .emit(
                        "protocol_error",
                        serde_json::json!({
                            "error": "request exceeded the protocol frame limit",
                            "max_bytes": MAX_FRAME_BYTES,
                        }),
                    )
                    .await?;
                continue;
            }
        };
        if bytes.is_empty() {
            continue;
        }
        let request = match parse_request(&bytes) {
            Ok(request) => request,
            Err(error) => {
                let mut emitter = Emitter::new(&mut output, "invalid".to_owned());
                emitter
                    .emit(
                        "protocol_error",
                        serde_json::json!({"error": format!("invalid request: {error}")}),
                    )
                    .await?;
                continue;
            }
        };
        if !valid_protocol_id(&request.request_id) {
            let mut emitter = Emitter::new(&mut output, "invalid".to_owned());
            emitter
                .emit(
                    "protocol_error",
                    serde_json::json!({"error": "request_id is invalid"}),
                )
                .await?;
            continue;
        }
        let mut emitter = Emitter::new(&mut output, request.request_id);
        if request.protocol_version != PROTOCOL_VERSION {
            emitter
                .emit(
                    "protocol_error",
                    serde_json::json!({
                        "error": "unsupported protocol version",
                        "received": request.protocol_version,
                        "supported": [PROTOCOL_VERSION],
                    }),
                )
                .await?;
            continue;
        }
        match request.command {
            HostCommand::Hello => emit_hello(&mut emitter).await?,
            HostCommand::Models { offline } => emit_models(&mut emitter, offline).await?,
            HostCommand::Run(run_request) => {
                if !valid_protocol_id(&run_request.run_id)
                    || run_request
                        .session_id
                        .as_deref()
                        .is_some_and(|id| !valid_session_id(id))
                {
                    emitter
                        .emit(
                            "protocol_error",
                            serde_json::json!({"error": "run_id or session_id is invalid"}),
                        )
                        .await?;
                    continue;
                }
                let mut emitter =
                    emitter.scoped(run_request.run_id.clone(), run_request.session_id.clone());
                match run::run_request(&mut emitter, *run_request).await {
                    Ok(RunRequestOutcome::Completed) => {}
                    Ok(RunRequestOutcome::Signaled) => break,
                    Err(_) if crate::tui::terminal::received_shutdown_signal().is_some() => break,
                    Err(error) => {
                        if emitter.is_terminal() {
                            continue;
                        }
                        emitter
                            .emit(
                                "final_result",
                                serde_json::json!({
                                    "status": "error",
                                    "output": "",
                                    "error": events::clip_text(&error.to_string(), protocol::MAX_EVENT_TEXT_BYTES),
                                    "filesChanged": [],
                                    "toolCalls": 0,
                                    "steps": 0,
                                    "sessionFile": "",
                                }),
                            )
                            .await?;
                    }
                }
            }
            HostCommand::Shutdown => {
                emitter
                    .emit("shutdown", serde_json::json!({"accepted": true}))
                    .await?;
                break;
            }
        }
    }
    Ok(())
}

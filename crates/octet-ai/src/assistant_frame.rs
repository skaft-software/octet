//! Compact, replayable assistant-message progress frames.
//!
//! A generation stream ([`crate::StreamEvent`]) is rich and terminal-oriented:
//! each delta is only valid in the context of the events around it, and the
//! assembled [`Response`] is emitted once at the end. That makes the stream a
//! poor durability unit — a crash between two deltas loses the whole turn.
//!
//! This module mirrors Pi's `utils/assistant-message-frame.ts`. An
//! [`AssistantMessageFrameEncoder`] turns a live event stream into a sequence of
//! small frames; [`reduce_assistant_message_frames`] rebuilds the in-progress
//! [`AssistantMessage`] from any prefix of those frames, without mutating them.
//! Frames are plain `serde` values, so a harness can persist and republish a
//! partial assistant turn and resume it after a restart.
//!
//! Terminal settlement is intentionally excluded: a `Finished`/`Usage` event
//! contributes no frame. The durable frame sequence therefore represents
//! partial progress only and must never be mistaken for a completed turn.

use serde::{Deserialize, Serialize};

use crate::error::{AiError, StreamProtocolError};
use crate::stream::StreamEvent;
use crate::types::{
    AssistantMessage, AssistantPart, Media, ModelId, Protocol, ReasoningPart, ToolCall, ToolCallId,
};

/// One compact, replayable step in an assistant message's progress.
///
/// Indices are the canonical part indices reported by [`StreamEvent`]. Block
/// lifecycle frames bracket content frames so a reducer can enforce that a
/// delta never follows its block's end.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AssistantMessageFrame {
    /// Stream opened. The skeleton carries the model/protocol but no content.
    Start {
        /// Model producing the message.
        model: ModelId,
        /// Wire protocol producing the message.
        protocol: Protocol,
    },
    /// Text block started.
    TextStart {
        /// Canonical part index.
        index: usize,
    },
    /// Text delta appended.
    TextDelta {
        /// Canonical part index.
        index: usize,
        /// Newly generated text.
        delta: String,
    },
    /// Text block finished.
    TextEnd {
        /// Canonical part index.
        index: usize,
    },
    /// Reasoning block started.
    ReasoningStart {
        /// Canonical part index.
        index: usize,
    },
    /// Reasoning delta appended.
    ReasoningDelta {
        /// Canonical part index.
        index: usize,
        /// Newly generated reasoning text.
        delta: String,
    },
    /// Reasoning block finished.
    ReasoningEnd {
        /// Canonical part index.
        index: usize,
    },
    /// Tool call started.
    ToolCallStart {
        /// Canonical part index.
        index: usize,
        /// Tool call identifier.
        id: ToolCallId,
        /// Tool name.
        name: String,
    },
    /// Authoritative raw JSON arguments replacement for a tool call.
    ToolCallCheckpoint {
        /// Canonical part index.
        index: usize,
        /// Complete raw JSON arguments string so far.
        json: String,
    },
    /// Tool-call arguments delta appended.
    ToolCallDelta {
        /// Canonical part index.
        index: usize,
        /// Newly generated JSON argument bytes.
        delta: String,
    },
    /// Tool call finished.
    ToolCallEnd {
        /// Canonical part index.
        index: usize,
    },
    /// Self-contained media emitted for a block.
    MediaCompleted {
        /// Canonical part index.
        index: usize,
        /// Assembled media object.
        media: Media,
    },
}

impl AssistantMessageFrame {
    /// Stable lowercase name used in diagnostics.
    pub fn kind_name(&self) -> &'static str {
        match self {
            Self::Start { .. } => "start",
            Self::TextStart { .. } => "text_start",
            Self::TextDelta { .. } => "text_delta",
            Self::TextEnd { .. } => "text_end",
            Self::ReasoningStart { .. } => "reasoning_start",
            Self::ReasoningDelta { .. } => "reasoning_delta",
            Self::ReasoningEnd { .. } => "reasoning_end",
            Self::ToolCallStart { .. } => "toolcall_start",
            Self::ToolCallCheckpoint { .. } => "toolcall_checkpoint",
            Self::ToolCallDelta { .. } => "toolcall_delta",
            Self::ToolCallEnd { .. } => "toolcall_end",
            Self::MediaCompleted { .. } => "media_completed",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum BlockKind {
    Text,
    Reasoning,
    ToolCall,
    Media,
}

impl BlockKind {
    fn names(self) -> &'static str {
        match self {
            Self::Text => "text",
            Self::Reasoning => "reasoning",
            Self::ToolCall => "tool call",
            Self::Media => "media",
        }
    }
}

fn frame_error(message: impl Into<String>) -> AiError {
    AiError::StreamProtocol(StreamProtocolError::UnexpectedEvent(message.into()))
}

/// Stateful converter from [`StreamEvent`]s to [`AssistantMessageFrame`]s.
///
/// The encoder owns only block bookkeeping; it never mutates the events it is
/// given. A frame is emitted only when it carries new information, so an empty
/// delta yields `None`.
#[derive(Debug)]
pub struct AssistantMessageFrameEncoder {
    model: ModelId,
    protocol: Protocol,
    started: bool,
    terminal: bool,
    blocks: std::collections::HashMap<usize, BlockKind>,
}

impl AssistantMessageFrameEncoder {
    /// Create an encoder for a stream of `model`/`protocol` events.
    pub fn new(model: ModelId, protocol: Protocol) -> Self {
        Self {
            model,
            protocol,
            started: false,
            terminal: false,
            blocks: std::collections::HashMap::new(),
        }
    }

    /// Encode one event into zero or one frame.
    ///
    /// Returns an error for structurally impossible streams (a delta before its
    /// block starts, a block starting twice, or any blocking event after a
    /// terminal event), matching the stream state machine's fail-closed rule.
    pub fn encode(
        &mut self,
        event: &StreamEvent,
    ) -> Result<Option<AssistantMessageFrame>, AiError> {
        if self.terminal {
            return Err(frame_error(format!(
                "assistant frame event follows a terminal event: {event:?}"
            )));
        }
        match event {
            StreamEvent::Started { .. } => {
                if self.started {
                    return Err(frame_error("assistant stream contains more than one start"));
                }
                self.started = true;
                Ok(Some(AssistantMessageFrame::Start {
                    model: self.model.clone(),
                    protocol: self.protocol,
                }))
            }
            StreamEvent::Finished(_) => {
                self.terminal = true;
                if !self.started {
                    return Err(frame_error("assistant stream finished before it started"));
                }
                Ok(None)
            }
            // Transport telemetry and billing counters are not assistant content.
            StreamEvent::ProviderLifecycle(_) | StreamEvent::Usage(_) => Ok(None),

            StreamEvent::TextStart { index } => {
                self.require_started("text_start")?;
                self.start_block(*index, BlockKind::Text)?;
                Ok(Some(AssistantMessageFrame::TextStart { index: *index }))
            }
            StreamEvent::TextDelta { index, delta } => {
                self.require_started("text_delta")?;
                self.require_block(*index, BlockKind::Text)?;
                if delta.is_empty() {
                    return Ok(None);
                }
                Ok(Some(AssistantMessageFrame::TextDelta {
                    index: *index,
                    delta: delta.clone(),
                }))
            }
            StreamEvent::TextEnd { index } => {
                self.require_started("text_end")?;
                self.end_block(*index, BlockKind::Text)?;
                Ok(Some(AssistantMessageFrame::TextEnd { index: *index }))
            }

            StreamEvent::ReasoningStart { index } => {
                self.require_started("reasoning_start")?;
                self.start_block(*index, BlockKind::Reasoning)?;
                Ok(Some(AssistantMessageFrame::ReasoningStart {
                    index: *index,
                }))
            }
            StreamEvent::ReasoningDelta { index, delta } => {
                self.require_started("reasoning_delta")?;
                self.require_block(*index, BlockKind::Reasoning)?;
                if delta.is_empty() {
                    return Ok(None);
                }
                Ok(Some(AssistantMessageFrame::ReasoningDelta {
                    index: *index,
                    delta: delta.clone(),
                }))
            }
            StreamEvent::ReasoningEnd { index } => {
                self.require_started("reasoning_end")?;
                self.end_block(*index, BlockKind::Reasoning)?;
                Ok(Some(AssistantMessageFrame::ReasoningEnd { index: *index }))
            }

            StreamEvent::ToolCallStart { index, id, name } => {
                self.require_started("toolcall_start")?;
                self.start_block(*index, BlockKind::ToolCall)?;
                Ok(Some(AssistantMessageFrame::ToolCallStart {
                    index: *index,
                    id: id.clone(),
                    name: name.clone(),
                }))
            }
            StreamEvent::ToolCallArgsDelta { index, delta } => {
                self.require_started("toolcall_delta")?;
                self.require_block(*index, BlockKind::ToolCall)?;
                if delta.is_empty() {
                    return Ok(None);
                }
                Ok(Some(AssistantMessageFrame::ToolCallDelta {
                    index: *index,
                    delta: delta.clone(),
                }))
            }
            StreamEvent::ToolCallEnd { index, .. } => {
                self.require_started("toolcall_end")?;
                self.end_block(*index, BlockKind::ToolCall)?;
                Ok(Some(AssistantMessageFrame::ToolCallEnd { index: *index }))
            }

            StreamEvent::MediaCompleted { index, media } => {
                self.require_started("media_completed")?;
                // Media is self-contained and not bracketed by deltas.
                Ok(Some(AssistantMessageFrame::MediaCompleted {
                    index: *index,
                    media: media.clone(),
                }))
            }
        }
    }

    /// Encode a whole stream, skipping frames that carry no new information.
    pub fn encode_all(
        &mut self,
        events: &[StreamEvent],
    ) -> Result<Vec<AssistantMessageFrame>, AiError> {
        let mut frames = Vec::new();
        for event in events {
            if let Some(frame) = self.encode(event)? {
                frames.push(frame);
            }
        }
        Ok(frames)
    }

    fn require_started(&self, event: &str) -> Result<(), AiError> {
        if self.started {
            Ok(())
        } else {
            Err(frame_error(format!(
                "assistant {event} event appears before start"
            )))
        }
    }

    fn start_block(&mut self, index: usize, kind: BlockKind) -> Result<(), AiError> {
        if self.blocks.contains_key(&index) {
            return Err(frame_error(format!(
                "assistant {} block {index} starts more than once",
                kind.names()
            )));
        }
        self.blocks.insert(index, kind);
        Ok(())
    }

    fn require_block(&self, index: usize, kind: BlockKind) -> Result<(), AiError> {
        match self.blocks.get(&index) {
            Some(active) if *active == kind => Ok(()),
            Some(active) => Err(frame_error(format!(
                "assistant block {index} is a {} block, not a {} block",
                active.names(),
                kind.names()
            ))),
            None => Err(frame_error(format!(
                "assistant {} block {index} has not started",
                kind.names()
            ))),
        }
    }

    fn end_block(&mut self, index: usize, kind: BlockKind) -> Result<(), AiError> {
        self.require_block(index, kind)?;
        self.blocks.remove(&index);
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ReducerState {
    Open,
    Ended,
}

/// Rebuild a partial [`AssistantMessage`] from a frame prefix without mutating
/// the frames.
///
/// Returns `Ok(None)` when the sequence contains no `Start` frame (for example a
/// truncated journal written before the stream opened). A structurally invalid
/// sequence is an error, never a silently corrupted message.
pub fn reduce_assistant_message_frames(
    frames: &[AssistantMessageFrame],
) -> Result<Option<AssistantMessage>, AiError> {
    let mut message: Option<AssistantMessage> = None;
    let mut states: std::collections::HashMap<usize, (BlockKind, ReducerState)> =
        std::collections::HashMap::new();

    for frame in frames {
        if let AssistantMessageFrame::Start { model, protocol } = frame {
            if message.is_some() {
                return Err(frame_error(
                    "assistant frame sequence contains more than one start frame",
                ));
            }
            message = Some(AssistantMessage {
                content: Vec::new(),
                model: model.clone(),
                protocol: *protocol,
            });
            continue;
        }
        let Some(message) = message.as_mut() else {
            // A frame before the start frame is retained as an explicit
            // truncation boundary rather than dropped silently.
            return Err(frame_error(format!(
                "assistant {} frame appears before the start frame",
                frame.kind_name()
            )));
        };

        match frame {
            AssistantMessageFrame::Start { .. } => unreachable!("handled above"),
            AssistantMessageFrame::TextStart { index } => {
                append_block(
                    message,
                    &mut states,
                    *index,
                    AssistantPart::Text(String::new()),
                    BlockKind::Text,
                )?;
            }
            AssistantMessageFrame::TextDelta { index, delta } => {
                let text = active_text(message, &states, *index, "text_delta")?;
                text.push_str(delta);
            }
            AssistantMessageFrame::TextEnd { index } => {
                end_block(&mut states, *index, BlockKind::Text, "text_end")?;
            }
            AssistantMessageFrame::ReasoningStart { index } => {
                append_block(
                    message,
                    &mut states,
                    *index,
                    AssistantPart::Reasoning(ReasoningPart {
                        text: Some(String::new()),
                        state: None,
                    }),
                    BlockKind::Reasoning,
                )?;
            }
            AssistantMessageFrame::ReasoningDelta { index, delta } => {
                let text = active_reasoning(message, &states, *index, "reasoning_delta")?;
                match text.text.as_mut() {
                    Some(text) => text.push_str(delta),
                    None => text.text = Some(delta.clone()),
                }
            }
            AssistantMessageFrame::ReasoningEnd { index } => {
                end_block(&mut states, *index, BlockKind::Reasoning, "reasoning_end")?;
            }
            AssistantMessageFrame::ToolCallStart { index, id, name } => {
                append_block(
                    message,
                    &mut states,
                    *index,
                    AssistantPart::ToolCall(ToolCall {
                        id: id.clone(),
                        name: name.clone(),
                        arguments_json: String::new(),
                        argument_error: None,
                    }),
                    BlockKind::ToolCall,
                )?;
            }
            AssistantMessageFrame::ToolCallCheckpoint { index, json } => {
                let call = active_tool_call(message, &states, *index, "toolcall_checkpoint")?;
                call.arguments_json = json.clone();
            }
            AssistantMessageFrame::ToolCallDelta { index, delta } => {
                let call = active_tool_call(message, &states, *index, "toolcall_delta")?;
                call.arguments_json.push_str(delta);
            }
            AssistantMessageFrame::ToolCallEnd { index } => {
                end_block(&mut states, *index, BlockKind::ToolCall, "toolcall_end")?;
            }
            AssistantMessageFrame::MediaCompleted { index, media } => {
                append_block(
                    message,
                    &mut states,
                    *index,
                    AssistantPart::Media(media.clone()),
                    BlockKind::Media,
                )?;
            }
        }
    }

    Ok(message)
}

fn append_block(
    message: &mut AssistantMessage,
    states: &mut std::collections::HashMap<usize, (BlockKind, ReducerState)>,
    index: usize,
    block: AssistantPart,
    kind: BlockKind,
) -> Result<(), AiError> {
    if index != message.content.len() {
        let reason = if index < message.content.len() {
            "already exists"
        } else {
            "would leave a gap"
        };
        return Err(frame_error(format!(
            "cannot start assistant block at index {index}: {reason}"
        )));
    }
    message.content.push(block);
    states.insert(index, (kind, ReducerState::Open));
    Ok(())
}

fn active(
    states: &std::collections::HashMap<usize, (BlockKind, ReducerState)>,
    index: usize,
    expected: BlockKind,
    frame: &str,
) -> Result<(), AiError> {
    match states.get(&index) {
        Some((kind, ReducerState::Open)) if *kind == expected => Ok(()),
        Some((kind, ReducerState::Ended)) if *kind == expected => Err(frame_error(format!(
            "assistant {frame} frame follows the end of block {index}"
        ))),
        Some((kind, _)) => Err(frame_error(format!(
            "assistant {frame} frame expected a {} block at index {index}, found {}",
            expected.names(),
            kind.names()
        ))),
        None => Err(frame_error(format!(
            "assistant {frame} frame has no started block at index {index}"
        ))),
    }
}

fn active_text<'a>(
    message: &'a mut AssistantMessage,
    states: &std::collections::HashMap<usize, (BlockKind, ReducerState)>,
    index: usize,
    frame: &str,
) -> Result<&'a mut String, AiError> {
    active(states, index, BlockKind::Text, frame)?;
    match message.content.get_mut(index) {
        Some(AssistantPart::Text(text)) => Ok(text),
        _ => Err(frame_error(format!(
            "assistant {frame} frame found no text block at index {index}"
        ))),
    }
}

fn active_reasoning<'a>(
    message: &'a mut AssistantMessage,
    states: &std::collections::HashMap<usize, (BlockKind, ReducerState)>,
    index: usize,
    frame: &str,
) -> Result<&'a mut ReasoningPart, AiError> {
    active(states, index, BlockKind::Reasoning, frame)?;
    match message.content.get_mut(index) {
        Some(AssistantPart::Reasoning(reasoning)) => Ok(reasoning),
        _ => Err(frame_error(format!(
            "assistant {frame} frame found no reasoning block at index {index}"
        ))),
    }
}

fn active_tool_call<'a>(
    message: &'a mut AssistantMessage,
    states: &std::collections::HashMap<usize, (BlockKind, ReducerState)>,
    index: usize,
    frame: &str,
) -> Result<&'a mut ToolCall, AiError> {
    active(states, index, BlockKind::ToolCall, frame)?;
    match message.content.get_mut(index) {
        Some(AssistantPart::ToolCall(call)) => Ok(call),
        _ => Err(frame_error(format!(
            "assistant {frame} frame found no tool-call block at index {index}"
        ))),
    }
}

fn end_block(
    states: &mut std::collections::HashMap<usize, (BlockKind, ReducerState)>,
    index: usize,
    expected: BlockKind,
    frame: &str,
) -> Result<(), AiError> {
    active(states, index, expected, frame)?;
    if let Some(state) = states.get_mut(&index) {
        state.1 = ReducerState::Ended;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::Usage;

    fn model() -> ModelId {
        ModelId("frame-model".to_string())
    }

    fn stream() -> Vec<StreamEvent> {
        vec![
            StreamEvent::Started {
                response_id: Some("resp-1".to_string()),
            },
            StreamEvent::ReasoningStart { index: 0 },
            StreamEvent::ReasoningDelta {
                index: 0,
                delta: "think".to_string(),
            },
            StreamEvent::ReasoningEnd { index: 0 },
            StreamEvent::TextStart { index: 1 },
            StreamEvent::TextDelta {
                index: 1,
                delta: "Hel".to_string(),
            },
            StreamEvent::TextDelta {
                index: 1,
                delta: "lo".to_string(),
            },
            StreamEvent::TextEnd { index: 1 },
            StreamEvent::ToolCallStart {
                index: 2,
                id: ToolCallId("call-1".to_string()),
                name: "lookup".to_string(),
            },
            StreamEvent::ToolCallArgsDelta {
                index: 2,
                delta: "{\"city\":".to_string(),
            },
            StreamEvent::ToolCallArgsDelta {
                index: 2,
                delta: "\"Paris\"}".to_string(),
            },
            StreamEvent::ToolCallEnd {
                index: 2,
                argument_error: None,
            },
            StreamEvent::Usage(Usage::default()),
        ]
    }

    #[test]
    fn frames_round_trip_through_serde_and_reduce_to_the_partial_message() {
        let mut encoder = AssistantMessageFrameEncoder::new(model(), Protocol::OpenAiChat);
        let frames = encoder.encode_all(&stream()).unwrap();

        // Durability: the frames survive a JSON round trip.
        let json = serde_json::to_string(&frames).unwrap();
        let restored: Vec<AssistantMessageFrame> = serde_json::from_str(&json).unwrap();
        assert_eq!(
            serde_json::to_value(&restored).unwrap(),
            serde_json::to_value(&frames).unwrap()
        );

        let message = reduce_assistant_message_frames(&restored).unwrap().unwrap();
        assert_eq!(message.model, model());
        assert_eq!(message.protocol, Protocol::OpenAiChat);
        assert_eq!(message.content.len(), 3);
        match &message.content[0] {
            AssistantPart::Reasoning(reasoning) => {
                assert_eq!(reasoning.text.as_deref(), Some("think"));
            }
            other => panic!("expected reasoning, got {other:?}"),
        }
        match &message.content[1] {
            AssistantPart::Text(text) => assert_eq!(text, "Hello"),
            other => panic!("expected text, got {other:?}"),
        }
        match &message.content[2] {
            AssistantPart::ToolCall(call) => {
                assert_eq!(call.id, ToolCallId("call-1".to_string()));
                assert_eq!(call.name, "lookup");
                assert_eq!(call.arguments_json, "{\"city\":\"Paris\"}");
            }
            other => panic!("expected tool call, got {other:?}"),
        }
    }

    #[test]
    fn truncated_prefix_reduces_to_partial_progress() {
        let mut encoder = AssistantMessageFrameEncoder::new(model(), Protocol::OpenAiChat);
        let frames = encoder.encode_all(&stream()).unwrap();
        // A crash after the first text delta: only the prefix is durable.
        let prefix = &frames[..6];
        let message = reduce_assistant_message_frames(prefix).unwrap().unwrap();
        assert_eq!(message.content.len(), 2);
        match &message.content[1] {
            AssistantPart::Text(text) => assert_eq!(text, "Hel"),
            other => panic!("expected text, got {other:?}"),
        }
    }

    #[test]
    fn empty_before_start_is_not_a_message() {
        assert!(reduce_assistant_message_frames(&[]).unwrap().is_none());
    }

    #[test]
    fn delta_before_block_start_is_rejected() {
        let mut encoder = AssistantMessageFrameEncoder::new(model(), Protocol::OpenAiChat);
        assert!(encoder
            .encode(&StreamEvent::Started { response_id: None })
            .unwrap()
            .is_some());
        let error = encoder
            .encode(&StreamEvent::TextDelta {
                index: 0,
                delta: "x".to_string(),
            })
            .unwrap_err();
        assert!(matches!(
            error,
            AiError::StreamProtocol(StreamProtocolError::UnexpectedEvent(_))
        ));
    }

    #[test]
    fn delta_after_block_end_is_rejected_by_the_reducer() {
        let frames = vec![
            AssistantMessageFrame::Start {
                model: model(),
                protocol: Protocol::OpenAiChat,
            },
            AssistantMessageFrame::TextStart { index: 0 },
            AssistantMessageFrame::TextEnd { index: 0 },
            AssistantMessageFrame::TextDelta {
                index: 0,
                delta: "late".to_string(),
            },
        ];
        assert!(reduce_assistant_message_frames(&frames).is_err());
    }
}

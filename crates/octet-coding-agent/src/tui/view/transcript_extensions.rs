//! Semantic source retention and immutable presentation snapshots. This module
//! never performs RPC; the product extension driver owns all asynchronous work.
use std::collections::HashMap;
use std::sync::Arc;

use octet_agent::extension_process::{
    ExtensionResourceOwner, TranscriptRenderContent, TranscriptRenderResponse,
};
use serde_json::{json, Value};

use super::transcript_cache::{RenderedTranscriptBlock, SurfaceGeometry};
use super::{AssistantBlock, InteractiveShell, ShellState, TranscriptBlock};

pub(crate) const MAX_TRANSCRIPT_FRAMES: usize = 128;
const MAX_FRAME_BYTES: usize = 4 * 1024 * 1024;

#[derive(Clone, Debug)]
pub(super) enum Source {
    Message {
        id: String,
        message: Value,
    },
    Entry {
        namespace: String,
        id: String,
        entry: Value,
    },
    ToolResult(Option<Value>),
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) struct Key {
    pub epoch: u64,
    pub id: u64,
    pub revision: u64,
    pub width: u16,
    pub expanded: bool,
    pub theme: u64,
}

#[derive(Clone, Debug)]
pub(crate) struct Candidate {
    pub key: Key,
    pub source_id: String,
    pub namespace: Option<String>,
    pub render: TranscriptRenderContent,
}

#[derive(Clone, Debug)]
pub(super) struct Frame {
    pub key: Key,
    pub owner: ExtensionResourceOwner,
    pub source_id: String,
    pub response: TranscriptRenderResponse,
}

#[derive(Clone, Debug, Default)]
pub(super) struct State {
    pub ever_active: bool,
    pub sources: Arc<HashMap<u64, Arc<Source>>>,
    pub expanded: HashMap<u64, bool>,
    pub source_revisions: Arc<HashMap<u64, u64>>,
    pub frames: HashMap<u64, Arc<Frame>>,
    pub owners: Vec<ExtensionResourceOwner>,
}

impl State {
    pub fn touch_source(&mut self, id: u64) {
        let revision = Arc::make_mut(&mut self.source_revisions)
            .entry(id)
            .or_default();
        *revision = revision.wrapping_add(1);
    }
    pub fn insert_source(&mut self, id: u64, source: Source) {
        Arc::make_mut(&mut self.sources).insert(id, Arc::new(source));
    }
    pub fn remove_source(&mut self, id: &u64) {
        Arc::make_mut(&mut self.sources).remove(id);
    }
}

impl ShellState {
    fn transcript_key(&self, index: usize) -> Key {
        let block = &self.transcript[index];
        let outer = self.transcript_content_width(self.size.0);
        let plan = super::surface_layout::compile_surface_plan(
            index.checked_sub(1).and_then(|i| self.transcript.get(i)),
            block,
            &self.theme,
            outer,
        );
        Key { epoch: self.transcript_epoch, id: self.transcript_commit_ids[index],
            revision: self.extension_transcript.source_revisions.get(&self.transcript_commit_ids[index]).copied().unwrap_or(0), width: plan.geometry.content_width.max(1),
            expanded: self.extension_transcript.expanded.get(&self.transcript_commit_ids[index]).copied().unwrap_or(self.verbose_tools || matches!(block, TranscriptBlock::Reasoning(block) if block.reasoning_expanded)),
            theme: self.theme_epoch }
    }

    fn matching_frame(&self, index: usize) -> Option<&Frame> {
        let frame = self
            .extension_transcript
            .frames
            .get(&self.transcript_commit_ids[index])?;
        (frame.key == self.transcript_key(index)
            && self.extension_transcript.owners.contains(&frame.owner))
        .then_some(frame)
    }

    pub(super) fn retain_custom_message(
        &mut self,
        id: &octet_agent::EntryId,
        message: &octet_agent::session::CustomMessage,
        timestamp: Option<u64>,
    ) {
        if !message.display {
            return;
        }
        if self.extension_transcript.sources.values().any(
            |source| matches!(source.as_ref(), Source::Message { id: seen, .. } if seen == &id.0),
        ) {
            return;
        }
        let mut value = json!({"role":"custom", "customType":message.custom_type, "content":message.content, "display":true});
        if let Some(details) = &message.details {
            value["details"] = details.clone();
        }
        if let Some(timestamp) = timestamp {
            value["timestamp"] = timestamp.into();
        }
        self.seal_activity_group();
        let index = self.push_block(TranscriptBlock::Notice(format!(
            "[{}]\n{}",
            message.custom_type,
            message.text()
        )));
        self.extension_transcript.insert_source(
            self.transcript_commit_ids[index],
            Source::Message {
                id: id.0.clone(),
                message: value,
            },
        );
    }

    pub(super) fn retain_private_entry(&mut self, namespace: String, entry: Value) {
        let Some(id) = entry.get("id").and_then(Value::as_str).map(str::to_owned) else {
            return;
        };
        if self.extension_transcript.sources.values().any(|source| matches!(source.as_ref(), Source::Entry { id: seen, namespace: owner, .. } if seen == &id && owner == &namespace)) { return; }
        // No private payload is implicitly exposed by native fallback or copy.
        let index = self.push_block(TranscriptBlock::Notice(String::new()));
        self.extension_transcript.insert_source(
            self.transcript_commit_ids[index],
            Source::Entry {
                namespace,
                id,
                entry,
            },
        );
    }

    pub(super) fn retain_tool_presentation(
        &mut self,
        id: &octet_ai::ToolCallId,
        result: Option<Value>,
    ) {
        if let Some(index) = self.tool_panels.get(id).copied() {
            self.extension_transcript.insert_source(
                self.transcript_commit_ids[index],
                Source::ToolResult(result),
            );
            self.touch_block(index);
        }
    }

    /// Only cached approved rows are consumed by either renderer. Geometry is
    /// rechecked at paint as well as publication, including a resize before drain.
    pub(super) fn extension_rendered(
        &self,
        index: usize,
        outer_width: u16,
    ) -> Option<RenderedTranscriptBlock> {
        let source = self
            .extension_transcript
            .sources
            .get(&self.transcript_commit_ids[index]);
        let frame = self.matching_frame(index);
        let block = &self.transcript[index];
        let previous = index.checked_sub(1).and_then(|i| self.transcript.get(i));
        let plan =
            super::surface_layout::compile_surface_plan(previous, block, &self.theme, outer_width);
        let frame = frame.filter(|frame| frame.key.width == plan.geometry.content_width.max(1));
        let mut lines = if let Some(frame) = frame.filter(|frame| frame.response.registered) {
            if let Some(lines) = &frame.response.lines {
                lines.clone()
            } else if let Some(markdown) = &frame.response.markdown {
                // Preserve native collapsed thinking rather than exposing a trace
                // because an extension transformed its source.
                if matches!(block, TranscriptBlock::Reasoning(_)) && !frame.key.expanded {
                    return None;
                }
                AssistantBlock::finalized(markdown.clone()).render_on_surface(
                    &self.theme.rich_renderer(),
                    &self.theme,
                    frame.key.width,
                    None,
                )
            } else if matches!(source.map(Arc::as_ref), Some(Source::Entry { .. })) {
                Vec::new()
            } else {
                return None;
            }
        } else if matches!(source.map(Arc::as_ref), Some(Source::Entry { .. })) {
            Vec::new()
        } else {
            return None;
        };
        if lines.is_empty() {
            return Some(RenderedTranscriptBlock {
                lines,
                geometry: SurfaceGeometry::default(),
            });
        }
        for line in &mut lines {
            *line = format!(
                "{}\x1b[0m",
                sexy_tui_rs::truncate_to_width(
                    line,
                    usize::from(plan.geometry.content_width),
                    Some("")
                )
            );
        }
        let self_shell =
            frame.is_some_and(|frame| frame.response.render_shell.as_deref() == Some("self"));
        if self_shell {
            return Some(RenderedTranscriptBlock {
                lines,
                geometry: SurfaceGeometry {
                    content_width: plan.geometry.content_width,
                    ..Default::default()
                },
            });
        }
        let marker = super::surface_frame::event_margin_marker_with_frame(
            block,
            &self.theme,
            self.event_spinner_frame,
            Some(self.status_shimmer_frame),
            0,
            false,
        );
        let lines = super::surface_frame::decorate_surface_with_frame(
            lines,
            &plan,
            &self.theme,
            outer_width,
            None,
            false,
            marker,
        );
        Some(RenderedTranscriptBlock {
            lines,
            geometry: plan.geometry,
        })
    }

    pub(super) fn extension_tool_disclosure(&self, index: usize) -> Option<bool> {
        let frame = self.matching_frame(index)?;
        (frame.response.registered && frame.response.render_shell.as_deref() != Some("self"))
            .then_some(frame.key.expanded)
    }

    pub(super) fn extension_markdown(&self, index: usize) -> Option<&str> {
        let frame = self.matching_frame(index)?;
        frame
            .response
            .registered
            .then_some(frame.response.markdown.as_deref())
            .flatten()
    }
}

impl InteractiveShell {
    pub(crate) fn append_custom_transcript_message(
        &mut self,
        id: &octet_agent::EntryId,
        message: &octet_agent::session::CustomMessage,
        timestamp: u64,
    ) {
        self.state
            .borrow_mut()
            .retain_custom_message(id, message, Some(timestamp));
        self.render();
    }

    pub(crate) fn append_private_transcript_entry(&mut self, namespace: String, entry: Value) {
        self.state
            .borrow_mut()
            .retain_private_entry(namespace, entry);
        self.render();
    }

    pub(crate) fn set_transcript_render_owners(&mut self, owners: Vec<ExtensionResourceOwner>) {
        let mut state = self.state.borrow_mut();
        if state.extension_transcript.owners == owners {
            return;
        }
        state.extension_transcript.ever_active |= !owners.is_empty();
        state.extension_transcript.owners = owners;
        let removed = state
            .extension_transcript
            .frames
            .keys()
            .copied()
            .collect::<Vec<_>>();
        for id in removed {
            state.extension_transcript.frames.remove(&id);
            if let Some(index) = state
                .transcript_commit_ids
                .iter()
                .position(|key| *key == id)
            {
                state.touch_block(index);
            }
        }
        drop(state);
        self.render();
    }

    pub(crate) fn invalidate_transcript_renderer(
        &mut self,
        owner: &ExtensionResourceOwner,
        source_id: Option<&str>,
    ) {
        let mut state = self.state.borrow_mut();
        if !state.extension_transcript.owners.contains(owner) {
            return;
        }
        let removed = state
            .extension_transcript
            .frames
            .iter()
            .filter_map(|(id, frame)| {
                source_id
                    .is_none_or(|source| source == frame.source_id)
                    .then_some(*id)
            })
            .collect::<Vec<_>>();
        for id in removed {
            state.extension_transcript.frames.remove(&id);
            if let Some(index) = state
                .transcript_commit_ids
                .iter()
                .position(|key| *key == id)
            {
                state.touch_block(index);
            }
        }
        drop(state);
        self.render();
    }

    pub(crate) fn transcript_key_current(&self, key: &Key) -> bool {
        let state = self.state.borrow();
        state
            .transcript_commit_ids
            .iter()
            .position(|id| *id == key.id)
            .is_some_and(|index| state.transcript_key(index) == *key)
    }

    #[cfg(test)]
    pub(crate) fn transcript_render_candidates(&self) -> Vec<Candidate> {
        self.transcript_render_candidates_bounded(MAX_TRANSCRIPT_FRAMES, None)
    }

    pub(crate) fn transcript_render_candidates_bounded(
        &self,
        limit: usize,
        namespaces: Option<&std::collections::HashSet<String>>,
    ) -> Vec<Candidate> {
        let state = self.state.borrow();
        let mut candidates = Vec::new();
        // A bounded presentation window, newest first. Canonical older history
        // stays available with its native rendering when this cache evicts it.
        for index in (0..state.transcript.len())
            .rev()
            .take(MAX_TRANSCRIPT_FRAMES)
        {
            if state.matching_frame(index).is_some() {
                continue;
            }
            let key = state.transcript_key(index);
            let mut source_id = format!("block:{}:{}", key.epoch, key.id);
            let mut namespace = None;
            let source = state
                .extension_transcript
                .sources
                .get(&key.id)
                .map(Arc::as_ref);
            if matches!(source, Some(Source::Entry { namespace, .. }) if namespaces.is_some_and(|owners| !owners.contains(namespace)))
            {
                continue;
            }
            let render = match source {
                Some(Source::ToolResult(None)) => continue,
                Some(Source::Message { id, message }) => {
                    source_id = id.clone();
                    TranscriptRenderContent::Message {
                        message: message.clone(),
                        expanded: key.expanded,
                        output_pad: 0,
                    }
                }
                Some(Source::Entry {
                    namespace: owner,
                    id,
                    entry,
                }) => {
                    namespace = Some(owner.clone());
                    source_id = id.clone();
                    TranscriptRenderContent::Entry {
                        entry: entry.clone(),
                        expanded: key.expanded,
                    }
                }
                _ => match &state.transcript[index] {
                    TranscriptBlock::User { text, .. } => TranscriptRenderContent::Markdown {
                        text: text.clone(),
                        message_type: "user".into(),
                        is_streaming: false,
                    },
                    TranscriptBlock::Assistant(block) | TranscriptBlock::Reasoning(block)
                        if !block.text.is_empty() =>
                    {
                        TranscriptRenderContent::Markdown {
                            text: block.text.clone(),
                            message_type: if matches!(
                                &state.transcript[index],
                                TranscriptBlock::Reasoning(_)
                            ) {
                                "assistant-thinking"
                            } else {
                                "assistant"
                            }
                            .into(),
                            is_streaming: !block.finished,
                        }
                    }
                    TranscriptBlock::Tool(panel) => {
                        source_id = format!("tool:{}:{}", key.epoch, key.id);
                        let result = match source {
                            Some(Source::ToolResult(result)) => result.clone(),
                            _ if panel.finished || !panel.output.is_empty() => Some(
                                json!({"content":[{"type":"text","text":panel.output}], "isError":panel.is_error}),
                            ),
                            _ => None,
                        };
                        TranscriptRenderContent::Tool {
                            name: panel.name.clone(),
                            tool_call_id: panel.id.0.clone(),
                            arguments: serde_json::from_str(&panel.args).unwrap_or(Value::Null),
                            result,
                            expanded: key.expanded,
                            is_partial: !panel.finished,
                            is_error: panel.is_error,
                            execution_started: true,
                            args_complete: true,
                            show_images: panel.image_rendering.enabled,
                        }
                    }
                    _ => continue,
                },
            };
            candidates.push(Candidate {
                key,
                source_id,
                namespace,
                render,
            });
            if candidates.len() == limit {
                break;
            }
        }
        candidates
    }

    pub(crate) fn accept_transcript_render(
        &mut self,
        candidate: &Candidate,
        owner: ExtensionResourceOwner,
        response: TranscriptRenderResponse,
    ) -> bool {
        if response.validate(&candidate.render).is_err()
            || !self.transcript_key_current(&candidate.key)
        {
            return false;
        }
        let mut state = self.state.borrow_mut();
        if !state.extension_transcript.owners.contains(&owner) {
            return false;
        }
        let index = state
            .transcript_commit_ids
            .iter()
            .position(|id| *id == candidate.key.id)
            .expect("current source");
        state.touch_presentation_block(index);
        let key = state.transcript_key(index);
        state.extension_transcript.frames.insert(
            key.id,
            Arc::new(Frame {
                key,
                owner,
                source_id: candidate.source_id.clone(),
                response,
            }),
        );
        while state.extension_transcript.frames.len() > MAX_TRANSCRIPT_FRAMES {
            let id = *state
                .extension_transcript
                .frames
                .keys()
                .min()
                .expect("over budget frame");
            state.extension_transcript.frames.remove(&id);
            if let Some(index) = state
                .transcript_commit_ids
                .iter()
                .position(|key| *key == id)
            {
                state.touch_block(index);
            }
        }
        while state
            .extension_transcript
            .frames
            .values()
            .map(|frame| frame.response.bytes())
            .sum::<usize>()
            > MAX_FRAME_BYTES
        {
            let id = *state
                .extension_transcript
                .frames
                .iter()
                .filter(|(_, frame)| frame.response.bytes() > 0)
                .map(|(id, _)| id)
                .min()
                .expect("nonempty over-budget frame");
            let mut frame = state.extension_transcript.frames[&id].as_ref().clone();
            frame.response = TranscriptRenderResponse {
                registered: false,
                lines: None,
                markdown: None,
                render_shell: None,
            };
            if let Some(index) = state
                .transcript_commit_ids
                .iter()
                .position(|key| *key == id)
            {
                state.touch_presentation_block(index);
            }
            // A zero-byte fallback receipt avoids continuously re-requesting
            // an immutable source that cannot fit the bounded display cache.
            state
                .extension_transcript
                .frames
                .insert(id, Arc::new(frame));
        }
        drop(state);
        self.render();
        true
    }
}

/// Lossless bounded Pi result projection for text/inline images. Unsupported
/// media refuses the custom renderer rather than inventing payloads.
pub(crate) fn tool_result(output: &octet_agent::ToolOutput) -> Option<Value> {
    use base64::Engine;
    let mut content = Vec::new();
    for part in output.content_parts() {
        content.push(match part {
            octet_agent::ToolOutputContentPart::Text(text) => json!({"type":"text","text":text}),
            octet_agent::ToolOutputContentPart::Media(octet_ai::Media::Image(image)) => {
                let octet_ai::ImageSource::Inline(data) = &image.source else { return None; };
                if data.len() > 262_144 { return None; }
                json!({"type":"image","mimeType":image.media_type.as_ref()?.to_string(),"data":base64::engine::general_purpose::STANDARD.encode(data)})
            }
            _ => return None,
        });
    }
    let mut result = json!({"content":content,"isError":output.is_error()});
    if let Some(details) = output
        .metadata()
        .and_then(|metadata| metadata.get("pi_details"))
    {
        result["details"] = details.clone();
    }
    if let Some(structured) = output.structured_content() {
        result["structuredContent"] = structured.clone();
    }
    (serde_json::to_vec(&result).ok()?.len() <= 524_288).then_some(result)
}

#[cfg(test)]
mod tests;

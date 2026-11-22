//! Cached component composition at the ordinary shell's two presentation seams.
//! Only the existing sexy-tui renderer paints these rows.

use std::time::Instant;

use octet_agent::extension_remote_ui::ExtensionRemoteUiPlacement as Placement;
use sexy_tui_rs::truncate_to_width;

use super::{builtin_shell_chrome, builtin_welcome_card, ShellOverlay, ShellState};
use crate::extensions::remote_ui::{MountView, Projection};

fn rows(
    projection: &Projection,
    mount: &MountView,
    width: u16,
    height: u16,
    limit: usize,
) -> Vec<String> {
    projection
        .lines(mount, width, height)
        .iter()
        .take(limit)
        // SGR is validated on ingress. Preserve it rather than applying the
        // semantic/plain-text sanitizer, and reset each physical row's style.
        .map(|line| {
            format!(
                "{}\x1b[0m",
                truncate_to_width(line, usize::from(width), Some(""))
            )
        })
        .collect()
}

pub(super) fn fullscreen_overlay(
    projection: &Projection,
    width: u16,
    height: u16,
) -> Option<String> {
    let mount = projection.mount(Placement::Fullscreen)?;
    let height = usize::from(height.max(1));
    let mut lines = rows(
        projection,
        mount,
        width,
        height as u16,
        height.saturating_sub(1),
    );
    lines.resize(height.saturating_sub(1), String::new());
    let hint = format!("Ctrl+G return to octet · Ctrl+D exit · {}", mount.title);
    lines.push(truncate_to_width(&hint, usize::from(width), Some("")));
    Some(lines.join("\n"))
}

pub(super) fn shell_chrome(
    state: &ShellState,
    width: u16,
    now: Instant,
) -> builtin_shell_chrome::ShellChrome {
    let mut chrome = composed_chrome(state, width, now);
    if state
        .working_status
        .as_ref()
        .is_some_and(|status| status.is_working_activity() && status.retry_activity.is_none())
    {
        chrome.space_working_slot();
    }
    chrome
}

fn composed_chrome(
    state: &ShellState,
    width: u16,
    now: Instant,
) -> builtin_shell_chrome::ShellChrome {
    let remote = &state.extension_ui.remote;
    if state.extension_ui.remote_fullscreen_overlay
        && remote.mount(Placement::Fullscreen).is_some()
        && state.panel.is_none()
        && state.tool_input_prompt.is_none()
    {
        return builtin_shell_chrome::ShellChrome {
            header: Vec::new(),
            working: Vec::new(),
            working_bottom_padding: false,
            extension_above: Vec::new(),
            composer: Vec::new(),
            footer_spans: Vec::new(),
            extension_below: Vec::new(),
            panel: Vec::new(),
            pending: Vec::new(),
            subagents: Vec::new(),
            suggestions: Vec::new(),
            suggestions_above: false,
            error: Vec::new(),
            transcript_rows: usize::from(state.size.1.max(1)),
        };
    }
    let mut chrome = builtin_shell_chrome::shell_chrome(state, width, now);
    if super::pi_chrome::apply(state, width, &mut chrome) {
        return chrome;
    }
    if state.startup_pending || state.panel.is_some() || state.tool_input_prompt.is_some() {
        return chrome;
    }
    let height = state.size.1.max(1);
    let budget = usize::from(height);
    if remote.mount(Placement::Header).is_some() {
        // The component header replaces the welcome prefix, not an extra row
        // between that prefix and the ordinary composer.
        chrome.header.clear();
    }
    let footer = remote.mount(Placement::Footer);
    let editor = remote.mount(Placement::Editor);
    let (builtin_footer, mut footer_rows, editor_limit) =
        composer_allocation(state, remote, width, &mut chrome);
    if let Some(editor) = editor {
        // Only paint a frame rendered for a fitting rectangle. In particular,
        // never top-slice an old terminal-height frame while resize is pending.
        let frame = remote.lines(editor, width, editor_limit.max(1) as u16);
        chrome.composer = if frame.len() <= editor_limit {
            rows(
                remote,
                editor,
                width,
                editor_limit.max(1) as u16,
                usize::MAX,
            )
        } else {
            Vec::new()
        };
    }
    if footer.is_none() {
        chrome.composer.extend(builtin_footer);
    } else {
        chrome.footer_spans.clear();
    }
    // Keep installed widgets adjacent to their corresponding editor region.
    for mount in &remote.mounts {
        match mount.placement {
            Placement::AboveEditor => chrome
                .extension_above
                .extend(rows(remote, mount, width, height, budget)),
            Placement::BelowEditor => chrome
                .extension_below
                .extend(rows(remote, mount, width, height, budget)),
            _ => {}
        }
    }
    let fixed = chrome.header.len()
        + chrome.working.len()
        + chrome.composer.len()
        + chrome.error.len()
        + chrome.panel.len()
        + chrome.pending.len()
        + chrome.subagents.len()
        + chrome.suggestions.len();
    let available = budget.saturating_sub(fixed.saturating_add(1));
    // The footer stays last and has priority when widgets exceed the viewport.
    footer_rows.truncate(available);
    let transcript_rows = if state.transcript.is_empty()
        || (chrome.extension_above.is_empty() && chrome.extension_below.is_empty())
    {
        0
    } else {
        1 + state
            .rendered_transcript(width)
            .iter()
            .rev()
            .take(budget)
            .take_while(|line| {
                sexy_tui_rs::strip_terminal_sequences(line)
                    .trim()
                    .is_empty()
            })
            .count()
    };
    let widget_rows = available
        .saturating_sub(footer_rows.len())
        .saturating_sub(transcript_rows);
    chrome.extension_below.truncate(widget_rows);
    chrome
        .extension_above
        .truncate(widget_rows.saturating_sub(chrome.extension_below.len()));
    chrome.extension_below.extend(footer_rows);
    chrome.transcript_rows =
        budget.saturating_sub(fixed + chrome.extension_above.len() + chrome.extension_below.len());
    chrome
}

// Rust owns this rectangle across the process boundary. Match Pi
// packages/tui/src/layout.ts:170-176 by measuring the mounted components, not
// the hidden recovery composer. Keep a cursor row and borders when possible;
// an oversized footer may use the rest before optional widgets.
fn composer_allocation(
    state: &ShellState,
    remote: &Projection,
    width: u16,
    chrome: &mut builtin_shell_chrome::ShellChrome,
) -> (Vec<String>, Vec<String>, usize) {
    let height = state.size.1.max(1);
    let footer = remote.mount(Placement::Footer);
    let builtin_footer = if chrome.suggestions.is_empty() {
        let count = crate::tui::composer_surface::status_footer_rows(state, width);
        chrome
            .composer
            .split_off(chrome.composer.len().saturating_sub(count))
    } else {
        Vec::new()
    };
    let mut footer_rows = footer.map_or_else(Vec::new, |mount| {
        rows(remote, mount, width, height, usize::from(height))
    });
    let reserved = chrome.header.len()
        + chrome.error.len()
        + chrome.pending.len()
        + chrome.subagents.len()
        + chrome.suggestions.len()
        + 1; // Retain a transcript row.
    let available = usize::from(height).saturating_sub(reserved);
    if let Some(editor) = remote.mount(Placement::Editor) {
        let editor_min = remote
            .components
            .slot_lines(Some(&editor.id))
            .map_or(3, |lines| lines.len().clamp(1, 3));
        if chrome.working.len() < 2 && state.working_status.is_some() {
            // The native fallback draft can be much taller than the mounted
            // editor window. Reclaim any live transcript-tail rows that fit.
            let status_limit = available
                .saturating_sub(editor_min + builtin_footer.len())
                .min(2);
            if status_limit > chrome.working.len() {
                chrome.working = builtin_shell_chrome::render_working_status(state, width);
                chrome.working.truncate(status_limit);
            }
        }
        footer_rows.truncate(available.saturating_sub(chrome.working.len() + editor_min));
    }
    let available = available.saturating_sub(chrome.working.len());
    let footer_height = if footer.is_some() {
        footer_rows.len()
    } else {
        builtin_footer.len()
    };
    (
        builtin_footer,
        footer_rows,
        available.saturating_sub(footer_height),
    )
}

pub(super) fn render_welcome_card(
    state: &ShellState,
    width: u16,
    max_rows: usize,
    now: Instant,
) -> Vec<String> {
    if let Some(header) = state.extension_ui.remote.mount(Placement::Header) {
        if state.overlay.is_some() || state.startup_pending {
            return Vec::new();
        }
        return rows(
            &state.extension_ui.remote,
            header,
            state.size.0,
            state.size.1,
            max_rows.min(usize::from(state.size.1)),
        )
        .into_iter()
        .map(|line| super::fit_line(&line, width))
        .collect();
    }
    builtin_welcome_card::render_welcome_card(state, width, max_rows, now)
}

pub(super) fn refresh_fullscreen_overlay(state: &mut ShellState) {
    if !state.extension_ui.remote_fullscreen_overlay {
        return;
    }
    if let Some(text) = fullscreen_overlay(&state.extension_ui.remote, state.size.0, state.size.1) {
        state.overlay = Some(ShellOverlay::Text(text.into()));
    }
}

pub(super) fn window_title(state: &ShellState) -> String {
    state
        .extension_ui
        .remote
        .chrome
        .as_ref()
        .and_then(|chrome| chrome.title.clone())
        .unwrap_or_else(|| {
            state
                .session_name
                .as_deref()
                .map_or_else(|| "octet".into(), |name| format!("octet · {name}"))
        })
}

impl super::InteractiveShell {
    pub(crate) fn extension_editor_row_budget(&self, remote: &Projection) -> u16 {
        let state = self.state.borrow();
        if state.startup_pending || state.panel.is_some() || state.tool_input_prompt.is_some() {
            return remote
                .mount(Placement::Editor)
                .map_or(state.size.1.max(1), |mount| mount.rows);
        }
        let mut chrome = builtin_shell_chrome::shell_chrome(&state, state.size.0, Instant::now());
        if remote.mount(Placement::Header).is_some() {
            chrome.header.clear();
        }
        composer_allocation(&state, remote, state.size.0, &mut chrome)
            .2
            .max(1) as u16
    }

    /// Host-owned wake binding for this live native UI consumer, not a claim
    /// based on the extension's metadata or the test runner's stdout.
    pub(crate) fn extension_remote_ui_binding(
        &self,
    ) -> Option<std::sync::Arc<tokio::sync::Notify>> {
        if self
            .terminal_ceded
            .load(std::sync::atomic::Ordering::Acquire)
            || (self.tui.is_none() && self.render_thread.is_none())
        {
            return None;
        }
        Some(std::sync::Arc::new(tokio::sync::Notify::new()))
    }

    #[cfg_attr(not(test), allow(dead_code))] // used by tests only
    pub(crate) fn extension_window_title(&self) -> String {
        window_title(&self.state.borrow())
    }
}

impl ShellState {
    pub(super) fn sync_extension_reasoning(&mut self, labels_changed: bool) {
        if let Some(status) = &mut self.working_status {
            status.extension_working = self.extension_ui.working.clone();
        }
        // Loader frames only replace local chrome. Only a hidden-label change
        // may restyle actual transcript reasoning, never an animation tick.
        if !labels_changed {
            return;
        }
        let mut changed = Vec::new();
        for (index, block) in self.transcript.iter_mut().enumerate() {
            if let super::TranscriptBlock::Reasoning(reasoning) = block {
                reasoning.hidden_thinking_label = self.extension_ui.hidden_thinking_label.clone();
                changed.push(index);
            }
        }
        for index in changed {
            self.touch_block(index);
        }
    }
}

pub(super) fn working_frame(reasoning: &super::assistant_block::AssistantBlock) -> Option<String> {
    let working = reasoning.extension_working.as_ref()?;
    let frames = working.frames.as_ref()?;
    if frames.is_empty() {
        return None;
    }
    let age = reasoning
        .activity_started_at
        .map_or(0, |start| start.elapsed().as_millis());
    let interval = working.interval_ms.filter(|value| *value > 0).unwrap_or(80);
    Some(frames[((age / u128::from(interval)) % frames.len() as u128) as usize].clone())
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod overflow_tests;

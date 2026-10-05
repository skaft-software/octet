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
    let remote = &state.extension_ui.remote;
    if state.extension_ui.remote_fullscreen_overlay
        && remote.mount(Placement::Fullscreen).is_some()
        && state.panel.is_none()
        && state.tool_input_prompt.is_none()
    {
        return builtin_shell_chrome::ShellChrome {
            header: Vec::new(),
            extension_above: Vec::new(),
            composer: Vec::new(),
            extension_below: Vec::new(),
            panel: Vec::new(),
            pending: Vec::new(),
            subagents: Vec::new(),
            suggestions: Vec::new(),
            error: Vec::new(),
            transcript_rows: usize::from(state.size.1.max(1)),
        };
    }
    let mut chrome = builtin_shell_chrome::shell_chrome(state, width, now);
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
    let footer_visible = crate::tui::composer_surface::status_footer_visible(state, width)
        && chrome.suggestions.is_empty();
    let builtin_footer = if footer_visible {
        chrome.composer.pop()
    } else {
        None
    };
    if let Some(editor) = editor {
        chrome.composer = rows(remote, editor, width, height, budget.saturating_sub(1));
        chrome.suggestions.clear();
        // Rescue remains visible even if the component paints no rows.
        chrome.composer.push(super::fit_line(
            "Ctrl+G restore editor · Ctrl+D exit",
            width,
        ));
    }
    let mut footer_rows = if let Some(footer) = footer {
        rows(remote, footer, width, height, budget)
    } else {
        if let Some(footer) = builtin_footer {
            chrome.composer.push(footer);
        }
        Vec::new()
    };
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
        + chrome.composer.len()
        + chrome.error.len()
        + chrome.panel.len()
        + chrome.pending.len()
        + chrome.subagents.len()
        + chrome.suggestions.len();
    let available = budget.saturating_sub(fixed.saturating_add(1));
    // The footer stays last and has priority when widgets exceed the viewport.
    footer_rows.truncate(available);
    let widget_rows = available.saturating_sub(footer_rows.len());
    chrome.extension_below.truncate(widget_rows);
    chrome
        .extension_above
        .truncate(widget_rows.saturating_sub(chrome.extension_below.len()));
    chrome.extension_below.extend(footer_rows);
    chrome.transcript_rows =
        budget.saturating_sub(fixed + chrome.extension_above.len() + chrome.extension_below.len());
    chrome
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

    pub(crate) fn extension_window_title(&self) -> String {
        window_title(&self.state.borrow())
    }
}

impl ShellState {
    pub(super) fn sync_extension_reasoning(&mut self) {
        let mut changed = Vec::new();
        for (index, block) in self.transcript.iter_mut().enumerate() {
            if let super::TranscriptBlock::Reasoning(reasoning) = block {
                if reasoning.extension_working != self.extension_ui.working
                    || reasoning.hidden_thinking_label != self.extension_ui.hidden_thinking_label
                {
                    reasoning.extension_working = self.extension_ui.working.clone();
                    reasoning.hidden_thinking_label =
                        self.extension_ui.hidden_thinking_label.clone();
                    changed.push(index);
                }
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

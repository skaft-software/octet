use std::time::Instant;

use sexy_tui_rs::{CommitCursor, FrameUpdate};

use super::renderer_runtime::ShellFrameState;
use super::shell_chrome::{
    append_chrome, append_viewport_chrome, shell_chrome, shell_chrome_rows, ShellChrome,
};
use super::transcript_commit::transcript_pinned_frame;
use super::viewport::{overlay_lines, transcript_lines};
use super::{ShellState, TranscriptBlock};

/// Live tool output is replaceable telemetry, not a final result. Keep a
/// trailing pending call wholly addressable until its authoritative result
/// arrives. In particular, a five-line preview in an eight-row terminal must
/// not push the mutable tool heading into saved lines. Final results are never
/// clipped by this policy; they flow into native history in their entirety
/// (subject only to the existing explicit disclosure policy).
///
/// This intentionally handles only an ordinary trailing call. Historical live
/// calls/rosters and Markdown finalization still need a separate emitted-
/// presentation policy; silently freezing those rows would lose real updates.
fn pending_tool_tail(
    state: &ShellState,
    chrome: &ShellChrome,
    width: u16,
) -> Option<(usize, Vec<String>)> {
    let index = state.transcript.len().checked_sub(1)?;
    let TranscriptBlock::Tool(panel) = &state.transcript[index] else {
        return None;
    };
    if panel.finished || panel.subagent_activity.is_some() {
        return None;
    }
    let cache = state.transcript_cache.borrow();
    let start = cache.block_starts[index];
    let rows = &cache.lines[start..];
    let budget =
        usize::from(state.size.1).saturating_sub(shell_chrome_rows(chrome).saturating_add(1));
    if rows.len() <= budget {
        return Some((start, rows.to_vec()));
    }
    // Keep the intent heading and the newest output, never the oldest preview
    // rows. The source/copy cache remains complete and independent of this
    // ephemeral terminal projection.
    let mut preview = rows
        .iter()
        .filter(|row| !sexy_tui_rs::strip_terminal_sequences(row).trim().is_empty())
        .take(budget.min(1))
        .cloned()
        .collect::<Vec<_>>();
    if budget > 1 {
        preview.push(super::fit_line(
            &state.theme.fg(
                "muted",
                &format!(
                    "  {} live preview (result pending)",
                    state.theme.glyph("ellipsis")
                ),
            ),
            width,
        ));
        let tail = budget.saturating_sub(2);
        preview.extend_from_slice(&rows[rows.len().saturating_sub(tail)..]);
    }
    Some((start, preview))
}

fn record_native_animation_viewport(state: &ShellState, rows: usize) {
    let top = rows.saturating_sub(usize::from(state.size.1));
    state.native_animation_viewport_top.set(Some(
        state
            .native_animation_viewport_top
            .get()
            .unwrap_or(0)
            .max(top),
    ));
}

fn native_overlay_prefix_len(transcript_len: usize, chrome: &ShellChrome) -> usize {
    let chrome_rows = shell_chrome_rows(chrome);
    let normal_rows = transcript_len
        .saturating_add(usize::from(transcript_len > 0))
        .saturating_add(chrome_rows);
    let overlay_rows = chrome.transcript_rows.saturating_add(chrome_rows);
    normal_rows.saturating_sub(overlay_rows)
}

/// Chrome that can disappear independently of transcript chronology is a
/// physical viewport surface. Letting its temporary height advance native
/// history makes the later contraction retreat behind an immutable row seam.
fn native_viewport_surface(state: &ShellState, chrome: &ShellChrome) -> bool {
    state.overlay.is_some()
        || state.panel.is_some()
        || !state.editor.is_empty()
        || state.tool_input_prompt.is_some()
        || !chrome.pending.is_empty()
        || !chrome.suggestions.is_empty()
        || !chrome.error.is_empty()
}

/// Build the native overlay as a screen-sized surface over the visible tail of
/// the normal transcript frame. Rows above that surface remain part of the
/// logical frame, so a destructive resize can replay terminal-owned history
/// without copying the overlay itself into scrollback.
fn render_native_overlay_suffix(
    state: &ShellState,
    width: u16,
    chrome: ShellChrome,
    transcript: &[String],
    requested_stable_prefix: usize,
) -> (usize, Vec<String>, usize, usize) {
    let overlay_prefix_len = native_overlay_prefix_len(transcript.len(), &chrome);
    let mut overlay = overlay_lines(state, width, chrome.transcript_rows);
    append_viewport_chrome(&mut overlay, chrome);

    let transcript_prefix_len = overlay_prefix_len.min(transcript.len());
    let stable_prefix = requested_stable_prefix.min(transcript_prefix_len);

    let mut replacement = transcript[stable_prefix..transcript_prefix_len].to_vec();
    replacement.resize(
        replacement
            .len()
            .saturating_add(overlay_prefix_len.saturating_sub(transcript_prefix_len)),
        String::new(),
    );
    replacement.extend(overlay);
    let total_rows = stable_prefix.saturating_add(replacement.len());
    (stable_prefix, replacement, total_rows, overlay_prefix_len)
}

/// Full logical frame for the default terminal-owned renderer. The backend
/// paints only its visible tail; committed rows naturally move into native
/// scrollback and are never sliced into an application-owned viewport.
pub(super) fn render_shell_at(state: &ShellState, width: u16, now: Instant) -> Vec<String> {
    // A complete Pi paint (including resize/session replacement) establishes a
    // new physical seam, unlike the monotonic differential append path.
    state.native_animation_viewport_top.set(None);
    let chrome = shell_chrome(state, width, now);
    let transcript = transcript_lines(state, width);
    if state.overlay.is_some() {
        let (_, lines, _, _) = render_native_overlay_suffix(state, width, chrome, &transcript, 0);
        lines
    } else {
        let mut lines = transcript.clone();
        drop(transcript);
        if let Some((start, preview)) = pending_tool_tail(state, &chrome, width) {
            lines.truncate(start);
            lines.extend(preview);
        }
        append_chrome(&mut lines, chrome, 0);
        record_native_animation_viewport(state, lines.len());
        lines
    }
}

pub(super) fn synchronize_shell_frame(state: &ShellState, width: u16, frame: &mut ShellFrameState) {
    let _ = transcript_lines(state, width);
    let cache = state.transcript_cache.borrow();
    let overlay_prefix_len = state.overlay.as_ref().map_or(0, |_| {
        native_overlay_prefix_len(
            cache.lines.len(),
            &shell_chrome(state, width, Instant::now()),
        )
    });
    frame.initialized = true;
    frame.width = width;
    frame.height = state.size.1;
    frame.theme_epoch = state.theme_epoch;
    frame.transcript_epoch = state.transcript_epoch;
    frame.transcript_generation = cache.generation;
    frame.transcript_len = cache.lines.len();
    frame.verbose_tools = state.verbose_tools;
    frame.overlay_active = state.overlay.is_some();
    frame.application_viewport = false;
    frame.overlay_prefix_len = overlay_prefix_len;
    let chrome = shell_chrome(state, width, Instant::now());
    let pending = state
        .overlay
        .is_none()
        .then(|| pending_tool_tail(state, &chrome, width))
        .flatten();
    frame.pending_tool_start = pending.as_ref().map(|(start, _)| *start);
    if let Some((start, preview)) = pending {
        frame.transcript_len = start + preview.len();
    }
}

/// Build only the mutable suffix of the native-scrollback frame. Historic
/// transcript strings are neither cloned nor compared on streaming/status
/// ticks; sexy-tui reuses the committed prefix already retained in its frame.
pub(super) fn render_shell_update_without_cursor(
    state: &ShellState,
    width: u16,
    now: Instant,
    frame: &mut ShellFrameState,
) -> FrameUpdate {
    render_shell_update_inner(state, width, now, frame, None, false)
}

/// The extended/pinned renderer needs a commit handshake even before it has
/// acknowledged its first boundary. A missing cursor is not an opt-out.
pub(super) fn render_shell_update_with_cursor(
    state: &ShellState,
    width: u16,
    now: Instant,
    frame: &mut ShellFrameState,
    acknowledged: Option<CommitCursor>,
) -> FrameUpdate {
    render_shell_update_inner(state, width, now, frame, acknowledged, true)
}

fn render_shell_update_inner(
    state: &ShellState,
    width: u16,
    now: Instant,
    frame: &mut ShellFrameState,
    acknowledged: Option<CommitCursor>,
    include_commit_metadata: bool,
) -> FrameUpdate {
    let repaint_theme = frame.initialized && frame.theme_epoch != state.theme_epoch;
    let resized = frame.initialized && (frame.width != width || frame.height != state.size.1);
    let presentation_changed = frame.initialized && frame.verbose_tools != state.verbose_tools;
    let entering_overlay = frame.initialized && !frame.overlay_active && state.overlay.is_some();
    let leaving_overlay = frame.initialized && frame.overlay_active && state.overlay.is_none();
    let chrome = shell_chrome(state, width, now);
    let viewport_surface = native_viewport_surface(state, &chrome);

    let transcript_len = {
        let transcript = transcript_lines(state, width);
        transcript.len()
    };
    // Hydrating `/new` (or another session) replaces the logical transcript.
    // Visual row counts alone cannot identify that transition: a streaming
    // Markdown table routinely shrinks while incomplete syntax reparses.
    let pending = (!include_commit_metadata && state.overlay.is_none())
        .then(|| pending_tool_tail(state, &chrome, width))
        .flatten();
    let cache = state.transcript_cache.borrow();
    let generation = cache.generation;
    let transcript_replaced = frame.initialized
        && frame.width == width
        && !frame.overlay_active
        && frame.transcript_epoch != state.transcript_epoch;
    let mut requested_stable_prefix = if !repaint_theme
        && !presentation_changed
        && frame.initialized
        && frame.width == width
        && !leaving_overlay
    {
        if frame.transcript_generation == cache.generation {
            frame.transcript_len.min(transcript_len)
        } else {
            cache
                .last_update_start
                .min(frame.transcript_len)
                .min(transcript_len)
        }
    } else {
        0
    };
    if frame.overlay_active {
        requested_stable_prefix = requested_stable_prefix.min(frame.overlay_prefix_len);
    }

    if let Some(start) = frame.pending_tool_start {
        requested_stable_prefix = requested_stable_prefix.min(start);
    }
    if let Some((start, _)) = pending.as_ref() {
        requested_stable_prefix = requested_stable_prefix.min(*start);
    }

    if state.overlay.is_some() {
        let resize_replay = resized.then(|| {
            let mut replay = cache.lines.clone();
            append_chrome(&mut replay, chrome.clone(), 0);
            replay
        });
        let (stable_prefix, replacement, total_rows, overlay_prefix_len) =
            render_native_overlay_suffix(
                state,
                width,
                chrome,
                &cache.lines,
                requested_stable_prefix,
            );
        let pinned = include_commit_metadata
            .then(|| transcript_pinned_frame(state, total_rows, acknowledged, viewport_surface));
        drop(cache);

        frame.initialized = true;
        frame.width = width;
        frame.height = state.size.1;
        frame.theme_epoch = state.theme_epoch;
        frame.transcript_epoch = state.transcript_epoch;
        frame.transcript_generation = generation;
        frame.transcript_len = transcript_len;
        frame.verbose_tools = state.verbose_tools;
        frame.overlay_active = true;
        frame.application_viewport = false;
        frame.overlay_prefix_len = overlay_prefix_len;
        frame.pending_tool_start = None;
        return FrameUpdate {
            stable_prefix,
            replacement,
            pinned,
            resize_replay,
            reanchor_viewport: repaint_theme || resized || entering_overlay,
            // Overlay rows are a temporary screen surface. Presentation changes
            // repaint that surface without restyling terminal-owned history.
            rebuild_scrollback: false,
        };
    }

    let stable_prefix = requested_stable_prefix;
    let mut replacement = if let Some((start, preview)) = pending.as_ref() {
        let mut rows = cache.lines[stable_prefix..*start].to_vec();
        rows.extend_from_slice(preview);
        rows
    } else {
        cache.lines[stable_prefix..].to_vec()
    };
    let projected_transcript_len = stable_prefix + replacement.len();
    drop(cache);
    append_chrome(&mut replacement, chrome, stable_prefix);
    if !include_commit_metadata {
        record_native_animation_viewport(state, stable_prefix + replacement.len());
    }
    let pinned = include_commit_metadata.then(|| {
        transcript_pinned_frame(
            state,
            stable_prefix.saturating_add(replacement.len()),
            acknowledged,
            viewport_surface,
        )
    });

    frame.initialized = true;
    frame.width = width;
    frame.height = state.size.1;
    frame.theme_epoch = state.theme_epoch;
    frame.transcript_epoch = state.transcript_epoch;
    frame.transcript_generation = generation;
    frame.transcript_len = transcript_len;
    frame.verbose_tools = state.verbose_tools;
    frame.overlay_active = false;
    frame.application_viewport = false;
    frame.overlay_prefix_len = 0;
    frame.pending_tool_start = pending.as_ref().map(|(start, _)| *start);
    frame.transcript_len = projected_transcript_len;
    FrameUpdate {
        stable_prefix,
        replacement,
        // In pinned mode, a theme swap keeps the semantic commit handshake
        // and repaints only the visible tail. Pi instead owns its replay policy
        // and consumes no semantic commit metadata.
        pinned,
        resize_replay: None,
        reanchor_viewport: repaint_theme || resized || leaving_overlay || transcript_replaced,
        rebuild_scrollback: presentation_changed,
    }
}

#[cfg(test)]
pub(super) fn render_shell_update(
    state: &ShellState,
    width: u16,
    now: Instant,
    frame: &mut ShellFrameState,
) -> FrameUpdate {
    render_shell_update_with_cursor(state, width, now, frame, None)
}

pub(super) fn render_shell(state: &ShellState, width: u16) -> Vec<String> {
    render_shell_at(state, width, Instant::now())
}

use std::time::Instant;

use sexy_tui_rs::visible_width;

use super::input_overlays::{render_input_suggestions, render_pending_steering};
use super::panel_render::render_panel_with_limit;
use super::terminal_text::sanitize_for_terminal;
use super::{fit_line, semantic_separator, wrap_hanging, ShellState};

#[derive(Clone)]
pub(super) struct ShellChrome {
    pub(super) header: Vec<String>,
    pub(super) extension_above: Vec<String>,
    pub(super) composer: Vec<String>,
    pub(super) extension_below: Vec<String>,
    pub(super) panel: Vec<String>,
    pub(super) pending: Vec<String>,
    pub(super) subagents: Vec<String>,
    pub(super) suggestions: Vec<String>,
    pub(super) error: Vec<String>,
    pub(super) transcript_rows: usize,
}

impl ShellChrome {
    /// Transcript spacing and chrome breathing room share one physical seam.
    /// Inspect the canonical tail, including when a lazy update has no rows.
    pub(super) fn separator_rows(transcript_tail: Option<&String>) -> usize {
        usize::from(transcript_tail.is_some_and(|line| {
            !sexy_tui_rs::strip_terminal_sequences(line)
                .trim()
                .is_empty()
        }))
    }
}

pub(super) fn responsive_identity(state: &ShellState, width: u16) -> String {
    let wordmark = state.theme.bold(
        &state
            .theme
            .fg("model_accent", state.theme.glyph("wordmark")),
    );
    if state.model.is_empty() {
        return fit_line(&wordmark, width);
    }
    let provider = sanitize_for_terminal(&state.provider);
    let model_name = if state.model_display.is_empty() {
        &state.model
    } else {
        &state.model_display
    };
    let model = state
        .theme
        .fg("model_accent", &sanitize_for_terminal(model_name));
    let separator = semantic_separator(&state.theme);
    let reasoning = (!state.reasoning.is_empty() && state.reasoning != "off")
        .then(|| format!("{separator}{}", sanitize_for_terminal(&state.reasoning)));
    let provider_model = format!("{provider} / {model}");
    let right = format!("{provider_model}{}", reasoning.clone().unwrap_or_default());
    let wide_width = visible_width(&wordmark) + visible_width(&right) + 4;
    if usize::from(width) >= 72 && wide_width <= usize::from(width) {
        let gap =
            usize::from(width).saturating_sub(visible_width(&wordmark) + visible_width(&right));
        return format!("{wordmark}{}{right}", " ".repeat(gap));
    }

    let compact = format!(
        "{wordmark}{separator}{}/{}{}",
        provider,
        model,
        reasoning.unwrap_or_default()
    );
    if visible_width(&compact) <= usize::from(width) {
        return compact;
    }
    let model_only = format!("{wordmark}{separator}{model}");
    if visible_width(&model_only) <= usize::from(width) {
        return model_only;
    }
    fit_line(&wordmark, width)
}

fn render_shell_header(state: &ShellState, width: u16) -> Vec<String> {
    let layout = state.theme.layout_for_width(width);
    if layout.show_header {
        vec![responsive_identity(state, width)]
    } else {
        Vec::new()
    }
}

fn render_extension_line(state: &ShellState, text: &str, role: Option<&str>, width: u16) -> String {
    let text = sanitize_for_terminal(text);
    let styled = match role {
        Some("extension.pi.muted") => state.theme.fg("muted", &text),
        Some("extension.pi.accent") => state.theme.fg("model_accent", &text),
        Some("extension.pi.warning") => state.theme.fg("warning", &text),
        Some("extension.pi.error") => state.theme.fg("error", &text),
        Some("extension.pi.status") | None => state.theme.fg("foreground", &text),
        // Transport validation admits only the roles above. Retain this
        // neutral fallback at the rendering boundary as defense in depth.
        Some(_) => state.theme.fg("foreground", &text),
    };
    fit_line(&styled, width)
}

fn render_extension_ui(state: &ShellState, width: u16) -> (Vec<String>, Vec<String>) {
    let mut above = state
        .extension_ui
        .header
        .iter()
        .map(|line| render_extension_line(state, &line.text, line.style_role.as_deref(), width))
        .collect::<Vec<_>>();
    above.extend(
        state.extension_ui.above_editor.iter().map(|line| {
            render_extension_line(state, &line.text, line.style_role.as_deref(), width)
        }),
    );
    above.extend(
        state.extension_ui.statuses.iter().map(|line| {
            render_extension_line(state, &line.text, line.style_role.as_deref(), width)
        }),
    );
    let mut below = state
        .extension_ui
        .below_editor
        .iter()
        .map(|line| render_extension_line(state, &line.text, line.style_role.as_deref(), width))
        .collect::<Vec<_>>();
    below.extend(
        state.extension_ui.footer.iter().map(|line| {
            render_extension_line(state, &line.text, line.style_role.as_deref(), width)
        }),
    );
    (above, below)
}

pub(super) fn shell_chrome(state: &ShellState, width: u16, now: Instant) -> ShellChrome {
    shell_chrome_with_composer_rows(state, width, now).0
}

// Keep the original composer height before footer removal or viewport clipping.
// Native startup uses it to reserve exactly the ready welcome's geometry.
fn shell_chrome_with_composer_rows(
    state: &ShellState,
    width: u16,
    now: Instant,
) -> (ShellChrome, usize) {
    let rows = usize::from(state.size.1.max(5));
    let header = render_shell_header(state, width);
    let mut error = state
        .error
        .as_ref()
        .map(|error| {
            let marker = state.theme.fg("error", state.theme.glyph("error"));
            let first_prefix = format!("  {marker} ");
            let continuation = " ".repeat(visible_width(&first_prefix));
            let mut rendered = Vec::new();
            for (index, source) in sanitize_for_terminal(error).split('\n').enumerate() {
                if source.is_empty() {
                    rendered.push(String::new());
                    continue;
                }
                let prefix = if index == 0 {
                    first_prefix.as_str()
                } else {
                    continuation.as_str()
                };
                rendered.extend(wrap_hanging(
                    &state.theme.fg("foreground", source),
                    prefix,
                    &continuation,
                    width,
                ));
            }
            rendered
        })
        .unwrap_or_default();

    if state.startup_pending {
        // Pickers own their own cursor. Lifecycle waits own the ordinary draft,
        // and credential/endpoint prompts own the temporary composer. Neither
        // should expose a provisional model footer before launch readiness.
        let mut composer_rows = 0;
        let mut composer = if state.tool_input_prompt.is_some()
            || (state.panel.is_none() && state.overlay.is_none())
        {
            let mut lines = render_chrome_composer(state, width, now);
            composer_rows = lines.len();
            if crate::tui::composer_surface::status_footer_visible(state, width) {
                lines.pop();
                if state.tool_input_prompt.is_none()
                    && state.panel.is_none()
                    && state.overlay.is_none()
                {
                    // Metadata resolves into this row without moving the draft.
                    lines.push(String::new());
                }
            }
            lines
        } else {
            Vec::new()
        };
        let rows = usize::from(state.size.1.max(1));
        let header = if !state.run_label.is_empty() && state.panel.is_none() {
            vec![fit_line(
                &state.theme.dim(&sanitize_for_terminal(&state.run_label)),
                width,
            )]
        } else {
            Vec::new()
        };
        composer.truncate(rows.saturating_sub(header.len()));
        error.truncate(
            rows.saturating_sub(header.len() + composer.len() + usize::from(state.panel.is_some())),
        );
        let remaining = rows.saturating_sub(header.len() + error.len() + composer.len());
        let panel = render_panel_with_limit(state, width, remaining);
        return (
            ShellChrome {
                header,
                error,
                composer,
                transcript_rows: remaining.saturating_sub(panel.len()),
                panel,
                extension_above: Vec::new(),
                extension_below: Vec::new(),
                pending: Vec::new(),
                subagents: Vec::new(),
                suggestions: Vec::new(),
            },
            composer_rows,
        );
    }

    // Render the integrated composer surface with its ordinary status row.
    // Autocomplete can claim that row below once we know it has real matches.
    let footer_visible = crate::tui::composer_surface::status_footer_visible(state, width);
    let mut composer = render_chrome_composer(state, width, now);
    let composer_rows = composer.len();
    if state.transcript_search_active() {
        // The query, not a tall hidden draft/error, owns the only cursor.
        composer.truncate(rows.saturating_sub(header.len() + 2));
        error.truncate(rows.saturating_sub(header.len() + composer.len() + 2));
    }
    let (mut extension_above, mut extension_below) = if state.panel.is_none() {
        render_extension_ui(state, width)
    } else {
        (Vec::new(), Vec::new())
    };
    let extension_limit = rows.saturating_sub(
        header
            .len()
            .saturating_add(error.len())
            .saturating_add(composer.len())
            .saturating_add(1),
    );
    extension_above.truncate(extension_limit);
    extension_below.truncate(extension_limit.saturating_sub(extension_above.len()));
    if state.panel.is_some() {
        // The focused picker must retain at least its filter row and cursor,
        // even when a tiny terminal also has a wrapped error message.
        let error_limit = rows.saturating_sub(
            composer
                .len()
                .saturating_add(header.len())
                .saturating_add(1),
        );
        error.truncate(error_limit);
    }
    let mut remaining = rows.saturating_sub(
        header
            .len()
            .saturating_add(error.len())
            .saturating_add(extension_above.len())
            .saturating_add(composer.len())
            .saturating_add(extension_below.len()),
    );

    // A short native transcript stays above the picker rather than becoming
    // viewport padding. Budget those actual rows before growing the panel;
    // long history still leaves the picker its ordinary viewport-sized window.
    let panel_limit = if state.panel.is_some() && !state.transcript_search_active() {
        let transcript = state.rendered_transcript(width);
        let retained_rows = transcript.len() + ShellChrome::separator_rows(transcript.last());
        if retained_rows < remaining {
            remaining - retained_rows
        } else {
            remaining
        }
    } else {
        remaining
    };
    let panel = if state.transcript_search_active() {
        state.transcript_search_panel(width, panel_limit)
    } else {
        render_panel_with_limit(state, width, panel_limit)
    };
    remaining = remaining.saturating_sub(panel.len());

    // Let autocomplete reuse the status row, including in a short terminal
    // where that reclaimed row is what makes a choice plus its hint fit.
    let suggestion_limit = remaining
        .saturating_add(if footer_visible { 1 } else { 0 })
        .min(10);
    let suggestions = render_input_suggestions(state, width, suggestion_limit);
    if footer_visible && !suggestions.is_empty() {
        composer.pop();
        remaining = remaining.saturating_add(1);
    }
    remaining = remaining.saturating_sub(suggestions.len());

    // Wrap queued messages in the available chrome, retaining transcript space
    // and the composer instead of letting a long queue push either off-screen.
    let pending_limit = remaining.saturating_sub(3);
    let pending = render_pending_steering(state, width, pending_limit);
    remaining = remaining.saturating_sub(pending.len());

    // The one orchestration lifecycle is part of the semantic transcript,
    // never duplicated in pinned composer-adjacent chrome.
    let subagents = Vec::new();

    (
        ShellChrome {
            header,
            extension_above,
            composer,
            extension_below,
            panel,
            pending,
            subagents,
            suggestions,
            error,
            transcript_rows: remaining,
        },
        composer_rows,
    )
}

#[cfg(test)]
thread_local! {
    static CHROME_COMPOSER_RENDERS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

fn render_chrome_composer(state: &ShellState, width: u16, now: Instant) -> Vec<String> {
    #[cfg(test)]
    CHROME_COMPOSER_RENDERS.set(CHROME_COMPOSER_RENDERS.get() + 1);
    let mut lines = crate::tui::composer_surface::render_composer_surface(state, width, now);
    if !state.startup_pending {
        return lines;
    }

    // Keep the real editor projection and ready geometry, but do not paint a
    // half-loaded composer. Rules, fills and embedded metadata resolve in the
    // same frame as the welcome and footer; draft input never waits for them.
    let footer_rows = usize::from(crate::tui::composer_surface::status_footer_visible(
        state, width,
    ));
    let content_end = lines.len().saturating_sub(footer_rows);
    let chrome = state
        .theme
        .resolve::<String>("composer")
        .unwrap_or_default();
    let chrome = chrome.trim();
    if width >= 12 || (width >= 3 && chrome == "topline") {
        lines[0].clear();
        if width >= 12 && chrome != "topline" {
            lines[content_end - 1].clear();
        }
    }
    let border = state.theme.role_rgb("composer_border").unwrap_or_else(|| {
        state
            .theme
            .model_rgb(state.model_lab)
            .unwrap_or((128, 128, 128))
    });
    let vertical = state.theme.rgb_fg(border, state.theme.glyph("vertical"));
    let inset = crate::tui::layout::PresentationLayout::new(&state.theme, width).inset;
    let left_border = format!("{}{vertical} ", " ".repeat(usize::from(inset)));
    let right_border = format!(" {vertical}");
    for line in &mut lines[..content_end] {
        if width >= 12 && chrome == "framed" {
            if let Some(content) = line.strip_prefix(&left_border) {
                *line = format!("{}{content}", " ".repeat(visible_width(&left_border)));
            }
            if let Some(content) = line.strip_suffix(&right_border) {
                *line = format!("{content}{}", " ".repeat(visible_width(&right_border)));
            }
        }
        // Strip presentation styles without stripping the trusted hardware
        // cursor token. Credential/setup text remains in its existing slot.
        *line = line
            .split(sexy_tui_rs::CURSOR_MARKER)
            .map(sexy_tui_rs::strip_terminal_sequences)
            .collect::<Vec<_>>()
            .join(sexy_tui_rs::CURSOR_MARKER);
    }
    lines
}

/// Before launch readiness, setup is a transient surface in either mouse mode.
/// Ordinary native drafts reserve ready geometry; pickers keep a full viewport.
/// Never materialize the provisional transcript as native history.
pub(super) fn render_startup_surface(
    state: &ShellState,
    width: u16,
    application_viewport: bool,
) -> Vec<String> {
    let (mut chrome, composer_rows) = shell_chrome_with_composer_rows(state, width, Instant::now());
    if !application_viewport
        && state.panel.is_none()
        && state.overlay.is_none()
        && state.tool_input_prompt.is_none()
    {
        // Reserve only the ready welcome's local rows, not a screen-height
        // draft. A lifecycle label can use that neutral reservation in place.
        let reserved = super::welcome_card::welcome_placeholder_rows(state, width, composer_rows);
        let mut lines = vec![String::new(); reserved];
        if state.theme.layout_for_width(width).show_header {
            // A themed identity header resolves into its own neutral row.
            chrome.header.resize(1, String::new());
        } else if reserved > 0 && !chrome.header.is_empty() {
            lines[reserved - 1] = chrome.header.remove(0);
        }
        let separator_rows =
            super::welcome_card::welcome_placeholder_separator_rows(state, width, composer_rows);
        append_chrome(&mut lines, chrome, separator_rows);
        lines.truncate(usize::from(state.size.1));
        return lines;
    }
    let mut lines = super::viewport::overlay_lines(state, width, chrome.transcript_rows);
    append_viewport_chrome(&mut lines, chrome);
    lines
}

pub(super) fn append_viewport_chrome(lines: &mut Vec<String>, chrome: ShellChrome) {
    // Application-owned mode renders exactly one terminal viewport. The
    // terminal-owned mode uses `append_chrome` below so committed transcript
    // rows can enter native scrollback instead of being sliced away here.
    lines.truncate(chrome.transcript_rows);
    lines.resize(chrome.transcript_rows, String::new());
    lines.extend(chrome.header);
    lines.extend(chrome.error);
    lines.extend(chrome.pending);
    lines.extend(chrome.panel);
    lines.extend(chrome.extension_above);
    lines.extend(chrome.subagents);
    lines.extend(chrome.composer);
    lines.extend(chrome.suggestions);
    lines.extend(chrome.extension_below);
}

pub(super) fn append_chrome(lines: &mut Vec<String>, chrome: ShellChrome, separator_rows: usize) {
    // The default terminal-owned mode follows logical content height. Padding
    // a short frame to terminal height would pin the composer to the bottom and
    // create a large dead zone below the transcript. Once the frame naturally
    // grows past the viewport, sexy-tui moves committed rows into native
    // scrollback.
    // The caller measures the canonical/projected transcript tail, not just
    // this lazy suffix. A reserved Working blank already supplies this seam.
    lines.extend(std::iter::repeat_n(String::new(), separator_rows));
    lines.extend(chrome.header);
    lines.extend(chrome.error);
    lines.extend(chrome.pending);
    lines.extend(chrome.panel);
    lines.extend(chrome.extension_above);
    lines.extend(chrome.subagents);
    lines.extend(chrome.composer);
    // Keep autocomplete adjacent to the composer in terminal-owned mode as
    // well as in the application-owned viewport above.
    lines.extend(chrome.suggestions);
    lines.extend(chrome.extension_below);
}

pub(super) fn shell_chrome_rows(chrome: &ShellChrome) -> usize {
    chrome
        .header
        .len()
        .saturating_add(chrome.extension_above.len())
        .saturating_add(chrome.error.len())
        .saturating_add(chrome.pending.len())
        .saturating_add(chrome.subagents.len())
        .saturating_add(chrome.suggestions.len())
        .saturating_add(chrome.panel.len())
        .saturating_add(chrome.composer.len())
        .saturating_add(chrome.extension_below.len())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::view::{
        builtin_welcome_card as welcome, InteractiveShell, Panel, ShellOverlay,
    };

    #[test]
    fn native_chrome_combines_the_canonical_blank_with_its_separator() {
        let shell = InteractiveShell::test_shell();
        let state = shell.state.borrow();
        let chrome = shell_chrome(&state, 80, Instant::now());
        for (transcript, expected_separator) in [
            (Vec::new(), 0),
            (vec![String::from("Working")], 1),
            (vec![String::from("Working"), String::new()], 0),
            (
                vec![String::from("Working"), String::from("\x1b[2m  \x1b[0m")],
                0,
            ),
        ] {
            let separator = ShellChrome::separator_rows(transcript.last());
            assert_eq!(separator, expected_separator);
            let mut full = transcript.clone();
            append_chrome(&mut full, chrome.clone(), separator);
            assert_eq!(
                full.len(),
                transcript.len() + separator + shell_chrome_rows(&chrome)
            );
            let mut lazy = Vec::new();
            append_chrome(&mut lazy, chrome.clone(), separator);
            let mut reconstructed = transcript;
            reconstructed.extend(lazy);
            assert_eq!(reconstructed, full);
        }
    }

    // Reference the pre-fast-path geometry through the actual welcome painter,
    // not through the new row-count calculation.
    fn materialized_startup_reference(state: &ShellState, width: u16) -> Vec<String> {
        let (mut chrome, composer_rows) =
            shell_chrome_with_composer_rows(state, width, Instant::now());
        let reserved = welcome::render_welcome_card(
            state,
            width,
            welcome::welcome_row_budget(state, width),
            Instant::now(),
        )
        .len();
        let mut lines = vec![String::new(); reserved];
        if state.theme.layout_for_width(width).show_header {
            chrome.header.resize(1, String::new());
        } else if reserved > 0 && !chrome.header.is_empty() {
            lines[reserved - 1] = chrome.header.remove(0);
        }
        let separator_rows =
            welcome::welcome_placeholder_separator_rows(state, width, composer_rows);
        append_chrome(&mut lines, chrome, separator_rows);
        lines.truncate(usize::from(state.size.1));
        lines
    }

    #[test]
    fn native_startup_placeholder_preserves_frames_and_renders_composer_once() {
        let mut themes = vec![crate::tui::theme::test_theme()];
        for source in [
            include_str!("../../../../../examples/themes/Cards.toml"),
            include_str!("../../../../../examples/themes/Still.toml"),
            "startup = 'pi'\n",
            "[colors]\nsplash = '#d97757'\nsplash_box = '#d97757'\n[layout]\nshow_header = true\nnarrow_show_header = true\n",
            "[colors]\nsplash_compact = true\ncontent_max_width = 32\n",
        ] {
            themes.push(crate::tui::theme::test_theme_from_source(source));
        }
        for theme in themes {
            let shell = InteractiveShell::test_shell_with_theme(theme);
            for draft in ["", "one\ntwo\nthree\nfour\nfive\nsix\nseven"] {
                for label in ["", "Loading session"] {
                    for width in [1, 2, 7, 8, 23, 31, 32, 46, 80, 160] {
                        for height in [0, 1, 3, 5, 7, 8, 10, 11, 12, 16, 24, 40] {
                            let mut state = shell.state.borrow_mut();
                            state.startup_pending = true;
                            state.startup_card_started_at = Some(Instant::now());
                            state.size = (width, height);
                            state.run_label = label.into();
                            state.editor.set_text(draft);
                            let expected = materialized_startup_reference(&state, width);
                            welcome::reset_placeholder_work_counts();
                            CHROME_COMPOSER_RENDERS.set(0);
                            let actual = render_startup_surface(&state, width, false);
                            assert_eq!(
                                actual, expected,
                                "{width}x{height} label={label} draft={draft}"
                            );
                            assert_eq!(CHROME_COMPOSER_RENDERS.get(), 1);
                            assert_eq!(welcome::placeholder_work_counts(), (0, 0));
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn startup_composer_hides_themed_chrome_without_rewriting_draft_or_cursor_geometry() {
        use crate::tui::terminal::{ColorDepth, TerminalCapabilities};
        use crate::tui::theme::{test_theme_source_with, TerminalBackground};
        use sexy_tui_rs::{strip_terminal_sequences, CURSOR_MARKER};

        for chrome in ["boxed", "framed", "shaded", "topline"] {
            for depth in [ColorDepth::TrueColor, ColorDepth::None] {
                let theme = test_theme_source_with(
                    &format!("[colors]\ncomposer = '{chrome}'\ncomposer_bg = '#202020'\n[layout]\ntranscript_inset = 2\n"),
                    TerminalCapabilities::test(true, true, depth),
                    TerminalBackground::Dark,
                );
                let shell = InteractiveShell::test_shell_with_theme(theme);
                for width in [11, 46, 96] {
                    let mut state = shell.state.borrow_mut();
                    state.size = (width, 18);
                    let draft = if width < 12 {
                        "│─"
                    } else {
                        "typed │ ─ draft"
                    };
                    state.editor.set_text(draft);
                    state.startup_pending = true;
                    let pending = shell_chrome(&state, width, Instant::now()).composer;
                    let text = strip_terminal_sequences(&pending.join("\n"));
                    assert!(text.contains(draft), "{chrome} {width}: {text}");
                    assert_eq!(text.matches('│').count(), 1, "extra side rails: {text}");
                    assert_eq!(text.matches('─').count(), 1, "extra rules: {text}");
                    assert!(pending
                        .iter()
                        .all(|line| !line.replace(CURSOR_MARKER, "").contains('\x1b')));
                    let cursor = |lines: &[String]| {
                        lines
                            .iter()
                            .enumerate()
                            .find_map(|(row, line)| {
                                line.split_once(CURSOR_MARKER)
                                    .map(|(before, _)| (row, visible_width(before)))
                            })
                            .unwrap()
                    };
                    state.startup_pending = false;
                    let ready = shell_chrome(&state, width, Instant::now()).composer;
                    assert_eq!(pending.len(), ready.len());
                    assert_eq!(cursor(&pending), cursor(&ready), "{chrome} {width}");
                }
            }
        }
    }

    #[test]
    fn startup_overlay_and_application_viewports_skip_welcome_measurement() {
        let shell = InteractiveShell::test_shell();
        for application_viewport in [false, true] {
            for surface in ["draft", "overlay", "tool input", "panel"] {
                if !application_viewport && surface == "draft" {
                    continue;
                }
                let mut state = shell.state.borrow_mut();
                state.startup_pending = true;
                state.size = (46, 8);
                state.overlay =
                    (surface == "overlay").then(|| ShellOverlay::Text("setup overlay".into()));
                state.tool_input_prompt = (surface == "tool input").then(|| "API key".into());
                state.panel = (surface == "panel").then(|| Panel::ReadOnlyDocument {
                    title: "Setup".into(),
                    text: "setup instructions".into(),
                    styled: false,
                    scroll_from_bottom: 0,
                });
                let chrome = shell_chrome(&state, 46, Instant::now());
                let mut expected =
                    super::super::viewport::overlay_lines(&state, 46, chrome.transcript_rows);
                append_viewport_chrome(&mut expected, chrome);
                welcome::reset_placeholder_work_counts();
                assert_eq!(
                    render_startup_surface(&state, 46, application_viewport),
                    expected
                );
                assert_eq!(welcome::placeholder_work_counts(), (0, 0));
            }
        }
    }
}

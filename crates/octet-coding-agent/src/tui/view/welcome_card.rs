//! Startup welcome-card lifecycle and presentation.

use std::time::Instant;

use super::{fit_line, ShellState, TranscriptBlock};
use crate::tui::layout::PresentationLayout;

// Fixed semantic text columns, not overall card heights. The materializer's
// arrays use these sizes so geometry cannot drift from their row cardinality.
const DEFAULT_TEXT_ROWS: usize = 4;
const FILE_TEXT_ROWS: usize = 6;
const PI_STARTUP_ROWS: usize = 6;
// Keep permissions and the exit shortcut intact beside the mark.
const MIN_SPLASH_TEXT_WIDTH: u16 = 24;

fn changelog_hint(state: &ShellState, width: u16) -> String {
    let tip = if state.theme.unicode() {
        "/changelog · what's new"
    } else {
        "/changelog - what's new"
    };
    let tip = if sexy_tui_rs::visible_width(tip) <= usize::from(width) {
        tip
    } else {
        "/changelog"
    };
    fit_line(&state.theme.dim(tip), width)
}

fn welcome_is_mutable(state: &ShellState) -> bool {
    state.startup_card_started_at.is_some()
        && !state.transcript.iter().any(|block| {
            matches!(
                block,
                TranscriptBlock::User { .. }
                    | TranscriptBlock::Assistant(_)
                    | TranscriptBlock::Reasoning(_)
                    | TranscriptBlock::Tool(_)
                    | TranscriptBlock::Shell(_)
                    | TranscriptBlock::Outcome(_)
                    | TranscriptBlock::Compaction(_)
            )
        })
}

pub(super) fn restart_welcome_animation(state: &mut ShellState) {
    if welcome_is_mutable(state) {
        state.startup_card_started_at = Some(Instant::now());
        state.invalidate_transcript_layout();
    }
}

fn render_pi_startup(state: &ShellState, width: u16) -> Vec<String> {
    let version = format!("v{}", env!("CARGO_PKG_VERSION"));
    let accent = |text: &str| state.theme.fg("accent", text);
    let muted = |text: &str| state.theme.dim(text);
    let mut lines: [String; PI_STARTUP_ROWS] = [
        format!("{} {}", accent("octet"), muted(&version)),
        changelog_hint(state, width),
        muted("escape interrupt · ctrl+c/ctrl+d clear/exit · / commands · ! bash · ctrl+o more"),
        muted("Press ctrl+o to show full startup help and loaded resources."),
        String::new(),
        muted(
            "octet can explain its own features and look up its docs. Ask it how to use or extend octet.",
        ),
    ];
    if let Some(hint) = super::startup_update::update_hint(state, width) {
        lines[1] = hint;
    }
    lines
        .into_iter()
        .map(|line| fit_line(&line, width))
        .collect()
}

pub(super) fn render_welcome_card(
    state: &ShellState,
    width: u16,
    max_rows: usize,
    _now: Instant,
) -> Vec<String> {
    if state.startup_card_started_at.is_none() {
        return Vec::new();
    }
    let layout = PresentationLayout::new(&state.theme, width);
    let inset = " ".repeat(usize::from(layout.inset));
    render_welcome_content(state, layout.content_width, max_rows)
        .into_iter()
        .map(|line| {
            if line.is_empty() {
                line
            } else {
                format!("{inset}{line}")
            }
        })
        .collect()
}

/// Reserve the same local geometry before model/session admission, without
/// publishing provisional branding or touching the transcript cache.
/// The startup chrome already rendered this composer. Its untruncated row count
/// includes the neutral footer row that will acquire identity on admission.
pub(super) fn welcome_placeholder_rows(
    state: &ShellState,
    width: u16,
    composer_rows: usize,
) -> usize {
    welcome_content_rows(
        state,
        PresentationLayout::new(&state.theme, width).content_width,
        welcome_row_budget_for_composer(state, width, composer_rows),
    )
}

/// Mirror whether the admitted welcome's last row needs a chrome seam. The
/// neutral placeholder's own empty rows are geometry, not semantic whitespace.
pub(super) fn welcome_placeholder_separator_rows(
    state: &ShellState,
    width: u16,
    composer_rows: usize,
) -> usize {
    let max_rows = welcome_row_budget_for_composer(state, width, composer_rows);
    if state.overlay.is_some() || max_rows == 0 {
        return 0;
    }
    let width = PresentationLayout::new(&state.theme, width).content_width;
    let (_, _, text_width) = splash_geometry(state, width);
    usize::from(
        max_rows < 7
            || pi_startup(state)
            || text_width < MIN_SPLASH_TEXT_WIDTH
            || max_rows < splash_card_rows(state)
            || state.theme.role_rgb("splash_box").is_some(),
    )
}

fn welcome_content_rows(state: &ShellState, width: u16, max_rows: usize) -> usize {
    if state.overlay.is_some() || max_rows == 0 {
        return 0;
    }
    if max_rows < 7 {
        // Identity/permissions, setup or release hint, exit help.
        return max_rows.min(3);
    }
    if pi_startup(state) {
        return PI_STARTUP_ROWS.min(max_rows);
    }
    let (_, _, text_width) = splash_geometry(state, width);
    if text_width < MIN_SPLASH_TEXT_WIDTH || max_rows < splash_card_rows(state) {
        // One wordmark or two logo rows, identity, hint, permissions and exit.
        // File themes additionally retain the model/reasoning identity row.
        return (1 + usize::from(width >= 8) + 4 + usize::from(!state.theme.is_compiled_default()))
            .min(max_rows);
    }
    splash_card_rows(state)
}

fn splash_card_rows(state: &ShellState) -> usize {
    // Two-column cards have breathing/frame rows around their text column.
    // The footer owns model identity for the default; file themes keep that
    // row and its following spacer, even with a compact logo.
    let text_rows = if state.theme.is_compiled_default() {
        DEFAULT_TEXT_ROWS
    } else {
        FILE_TEXT_ROWS
    };
    text_rows + 2
}

fn pi_startup(state: &ShellState) -> bool {
    state
        .theme
        .resolve::<String>("startup")
        .is_some_and(|startup| startup.trim().eq_ignore_ascii_case("pi"))
}

/// Layout decisions shared by measurement and materialization. Colours and
/// strings are intentionally absent: reserving invisible rows must not paint.
fn splash_geometry(state: &ShellState, width: u16) -> (usize, usize, u16) {
    let compact = state.theme.is_compiled_default()
        || state
            .theme
            .resolve::<bool>("splash_compact")
            .unwrap_or(false);
    let logo_rows = if compact { 2 } else { 6 };
    let logo_width = if compact {
        8
    } else {
        (usize::from(width) / 3).clamp(14, 24)
    };
    let text_width = width.saturating_sub(
        logo_width as u16
            + 5
            + if state.theme.role_rgb("splash_box").is_some() {
                2
            } else {
                0
            },
    );
    (logo_rows, logo_width, text_width)
}

#[cfg(test)]
thread_local! {
    static WELCOME_MATERIALIZATIONS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    static WELCOME_COMPOSER_MEASUREMENTS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

#[cfg(test)]
pub(super) fn reset_placeholder_work_counts() {
    WELCOME_MATERIALIZATIONS.set(0);
    WELCOME_COMPOSER_MEASUREMENTS.set(0);
}

#[cfg(test)]
pub(super) fn placeholder_work_counts() -> (usize, usize) {
    (
        WELCOME_MATERIALIZATIONS.get(),
        WELCOME_COMPOSER_MEASUREMENTS.get(),
    )
}

pub(super) fn welcome_row_budget(state: &ShellState, width: u16) -> usize {
    #[cfg(test)]
    WELCOME_COMPOSER_MEASUREMENTS.set(WELCOME_COMPOSER_MEASUREMENTS.get() + 1);
    let composer_rows =
        crate::tui::composer_surface::render_composer_surface(state, width, Instant::now()).len();
    welcome_row_budget_for_composer(state, width, composer_rows)
}

fn welcome_row_budget_for_composer(state: &ShellState, width: u16, composer_rows: usize) -> usize {
    let header_rows = usize::from(state.theme.layout_for_width(width).show_header);
    usize::from(state.size.1)
        .saturating_sub(composer_rows + header_rows + 1)
        .min(10)
}

fn render_welcome_content(state: &ShellState, width: u16, max_rows: usize) -> Vec<String> {
    #[cfg(test)]
    WELCOME_MATERIALIZATIONS.set(WELCOME_MATERIALIZATIONS.get() + 1);
    if state.overlay.is_some() || max_rows == 0 {
        return Vec::new();
    }
    if max_rows < 7 {
        let access = if state.safe_mode {
            "safe mode"
        } else {
            "full access"
        };
        let role = if state.safe_mode { "accent" } else { "warning" };
        let access = state.theme.fg(role, access);
        let wordmark = state.theme.bold("octet");
        let version = state.theme.dim(&format!("v{}", env!("CARGO_PKG_VERSION")));
        let mut identity = format!("{wordmark} {version} {access}");
        // Keep the permission label intact before optional identity details,
        // without adding rows or moving the pending/ready composer geometry.
        if sexy_tui_rs::visible_width(&identity) > usize::from(width) {
            identity = format!("{wordmark} {access}");
        }
        if sexy_tui_rs::visible_width(&identity) > usize::from(width) {
            identity = access;
        }
        let detail = if state.model.trim().is_empty() {
            state
                .theme
                .fg("warning", "No models configured. Use /login.")
        } else {
            super::startup_update::update_hint(state, width)
                .unwrap_or_else(|| changelog_hint(state, width))
        };
        return [
            identity,
            detail,
            if state.theme.is_compiled_default() {
                state.theme.dim("Ctrl+C / Ctrl+D to exit")
            } else {
                format!(
                    "{} {}",
                    state.theme.bold("Ctrl+C / Ctrl+D"),
                    state.theme.dim("to exit")
                )
            },
        ]
        .into_iter()
        .take(max_rows)
        .map(|line| fit_line(&line, width))
        .collect();
    }
    if pi_startup(state) {
        return render_pi_startup(state, width)
            .into_iter()
            .take(max_rows)
            .collect();
    }

    // Custom themes can opt into the compiled default's smaller splash
    // geometry with `splash_compact = true`. Everything else keeps the
    // larger file-theme presentation.
    let (rows, logo_width, text_width) = splash_geometry(state, width);
    // Startup identity is static for every theme. Working/Thinking retain their
    // own liveness clocks; optional welcome colours never mutate native history.
    let elapsed = crate::tui::splash::DURATION;
    // A theme can also keep the default's model-adaptive splash with
    // `splash_model_adaptive = true`. It owns the whole splash: the byte-mark
    // follows the active model family and the text falls back to the same
    // model-accent/foreground treatment a non-`splash` file theme uses, so
    // the theme's own `splash` colour is not used at all.
    let compiled_default = state.theme.is_compiled_default();
    let model_adaptive_splash = !compiled_default
        && state
            .theme
            .resolve::<bool>("splash_model_adaptive")
            .unwrap_or(false);
    let adaptive_accent = (compiled_default || model_adaptive_splash)
        .then(|| state.theme.model_rgb(state.model_lab))
        .flatten();
    let splash_color = (!model_adaptive_splash)
        .then(|| state.theme.role_rgb("splash"))
        .flatten();
    let logo = crate::tui::splash::render_logo(
        &state.theme,
        logo_width,
        rows,
        elapsed,
        adaptive_accent,
        splash_color,
    );
    let splash_text = |text: &str| {
        splash_color
            .map(|_| state.theme.fg("splash", text))
            .unwrap_or_else(|| text.to_owned())
    };
    let splash_bold = |text: &str| {
        splash_color
            .map(|_| state.theme.bold(&state.theme.fg("splash", text)))
            .unwrap_or_else(|| state.theme.bold(text))
    };

    let model = if state.model_display.trim().is_empty() {
        state.model.as_str()
    } else {
        state.model_display.as_str()
    };
    let identity = if model.trim().is_empty() {
        "no configured model · setup needed".to_owned()
    } else if state.reasoning.is_empty() {
        model.to_owned()
    } else {
        format!("{model} / {}", state.reasoning)
    };
    let box_color = state.theme.role_rgb("splash_box");
    if text_width < MIN_SPLASH_TEXT_WIDTH || max_rows < splash_card_rows(state) {
        // A narrow or short terminal cannot fit the two-column card. Keep a compact,
        // bounded identity instead of silently dropping the startup surface.
        let mut compact = Vec::with_capacity(7);
        if width >= 8 {
            let logo = crate::tui::splash::render_logo(
                &state.theme,
                8,
                2,
                elapsed,
                adaptive_accent,
                splash_color,
            );
            compact.extend(logo.into_iter().map(|line| fit_line(&line, width)));
        } else {
            compact.push(fit_line(&splash_bold("octet"), width));
        }
        compact.push(fit_line(
            &format!(
                "{} {}",
                splash_bold("octet"),
                splash_text(&format!("v{}", env!("CARGO_PKG_VERSION"))),
            ),
            width,
        ));
        compact.push(
            if state.theme.is_compiled_default() && model.trim().is_empty() {
                fit_line(&state.theme.fg("warning", &identity), width)
            } else {
                super::startup_update::update_hint(state, width)
                    .unwrap_or_else(|| changelog_hint(state, width))
            },
        );
        if !state.theme.is_compiled_default() {
            compact.push(fit_line(&splash_text(&identity), width));
        }
        let access = if state.safe_mode {
            "safe mode"
        } else {
            "full access"
        };
        let prefix = ["permissions: ", "mode: ", ""]
            .into_iter()
            .find(|prefix| prefix.len() + access.len() <= usize::from(width))
            .unwrap_or("");
        compact.push(fit_line(
            &format!(
                "{}{}",
                splash_text(prefix),
                state.theme.bold(
                    &state
                        .theme
                        .fg(if state.safe_mode { "accent" } else { "warning" }, access,)
                ),
            ),
            width,
        ));
        let exit = if compiled_default {
            state.theme.dim("Ctrl+C / Ctrl+D to exit")
        } else {
            format!(
                "{} {}",
                splash_bold("Ctrl+C / Ctrl+D"),
                splash_text("to exit")
            )
        };
        compact.push(fit_line(&exit, width));
        // The update action and setup/permission state outrank the changelog
        // hint when a short viewport cannot fit the entire compact card.
        if compact.len() > max_rows {
            compact.remove(if width >= 8 { 3 } else { 2 });
        }
        if compact.len() > max_rows && width >= 8 {
            compact.remove(0);
        }
        compact.truncate(max_rows);
        return compact;
    }
    let mut text = if state.theme.is_compiled_default() {
        let mut text: [String; DEFAULT_TEXT_ROWS] = [
            format!(
                "{} {}",
                state.theme.bold("octet"),
                state.theme.dim(&format!("v{}", env!("CARGO_PKG_VERSION"))),
            ),
            changelog_hint(state, text_width),
            format!(
                "{} {}",
                state.theme.dim("permissions:"),
                state.theme.bold(&state.theme.fg(
                    if state.safe_mode { "accent" } else { "warning" },
                    if state.safe_mode {
                        "safe mode"
                    } else {
                        "full access"
                    },
                )),
            ),
            state.theme.dim("Ctrl+C / Ctrl+D to exit"),
        ];
        if model.trim().is_empty() {
            text[1] = state.theme.fg("warning", &identity);
        }
        Vec::from(text)
    } else if splash_color.is_some() {
        let text: [String; FILE_TEXT_ROWS] = [
            format!(
                "{} {}",
                splash_bold("octet"),
                splash_text(&format!("v{}", env!("CARGO_PKG_VERSION"))),
            ),
            changelog_hint(state, text_width),
            splash_text(&identity),
            // The footer owns the path; keep the byte-aligned breathing row.
            String::new(),
            if state.safe_mode {
                format!(
                    "{} {}",
                    splash_text("permissions:"),
                    splash_bold("safe mode")
                )
            } else {
                format!(
                    "{} {}",
                    splash_text("permissions:"),
                    splash_bold("full access")
                )
            },
            format!(
                "{} {}",
                splash_bold("Ctrl+C / Ctrl+D"),
                splash_text("to exit")
            ),
        ];
        Vec::from(text)
    } else {
        let text: [String; FILE_TEXT_ROWS] = [
            format!(
                "{} {}",
                state.theme.bold(&state.theme.fg("model_accent", "octet")),
                state.theme.dim(&format!("v{}", env!("CARGO_PKG_VERSION"))),
            ),
            changelog_hint(state, text_width),
            state.theme.fg("foreground", &identity),
            String::new(),
            if state.safe_mode {
                format!(
                    "{} {}",
                    state.theme.dim("permissions:"),
                    state.theme.bold(&state.theme.fg("accent", "safe mode"))
                )
            } else {
                format!(
                    "{} {}",
                    state.theme.dim("permissions:"),
                    state.theme.bold(&state.theme.fg("error", "full access"))
                )
            },
            format!(
                "{} {}",
                state.theme.bold("Ctrl+C / Ctrl+D"),
                state.theme.dim("to exit")
            ),
        ];
        Vec::from(text)
    };

    if !state.theme.is_compiled_default() || !model.trim().is_empty() {
        if let Some(hint) = super::startup_update::update_hint(state, text_width) {
            text[1] = hint;
        }
    }
    // A release hint replaces the changelog row instead of moving the composer.
    let mut logo = logo;
    logo.resize(text.len(), " ".repeat(logo_width));
    if let Some(color) = box_color {
        let inner_width = usize::from(width).saturating_sub(2);
        let paint = |text: &str| state.theme.rgb_fg(color, text);
        let pad = |text: String| {
            let text_width = sexy_tui_rs::visible_width(&text);
            format!(
                "{text}{}",
                " ".repeat(inner_width.saturating_sub(text_width))
            )
        };
        let edge = |left: &str, right: &str| {
            paint(&format!(
                "{left}{}{right}",
                state.theme.glyph("horizontal").repeat(inner_width)
            ))
        };
        let body = |content: String| {
            let content = pad(fit_line(&content, inner_width as u16));
            paint(&format!(
                "{}{}{}",
                state.theme.glyph("vertical"),
                content,
                state.theme.glyph("vertical")
            ))
        };
        let mut lines = Vec::with_capacity(text.len() + 2);
        lines.push(edge(
            state.theme.glyph("top_left"),
            state.theme.glyph("top_right"),
        ));
        for (logo, text) in logo.iter().zip(&text) {
            lines.push(body(format!("  {logo}   {text}")));
        }
        lines.push(edge(
            state.theme.glyph("bottom_left"),
            state.theme.glyph("bottom_right"),
        ));
        lines
    } else {
        let mut lines = Vec::with_capacity(text.len() + 2);
        lines.push(String::new());
        for (logo, text) in logo.iter().zip(&text) {
            lines.push(fit_line(&format!("  {logo}   {text}"), width));
        }
        lines.push(String::new());
        lines
    }
}

#[cfg(test)]
#[path = "welcome_card_tests.rs"]
mod tests;

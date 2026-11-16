//! Startup welcome-card lifecycle and presentation.

use std::time::Instant;

use super::{fit_line, ShellState, TranscriptBlock};
use crate::tui::layout::PresentationLayout;

// Fixed semantic text columns, not overall card heights. The materializer's
// arrays use these sizes so geometry cannot drift from their row cardinality.
const DEFAULT_TEXT_ROWS: usize = 2;
const FILE_TEXT_ROWS: usize = 6;
// Keep the complete access label and changelog command beside the mark.
const MIN_SPLASH_TEXT_WIDTH: u16 = 24;

fn changelog_hint(state: &ShellState, width: u16) -> String {
    let tip = if compact_welcome(state) {
        "/changelog"
    } else if state.theme.unicode() {
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

#[derive(Clone, Copy)]
pub(super) struct MarkReaction {
    started: Instant,
    blink_started: Instant,
    strength: f32,
    phase: f32,
}

/// Only a visible native compact mark owns this interaction. Remote headers
/// replace it completely; immutable conversation history never animates.
fn interactive_mark(state: &ShellState) -> bool {
    use octet_agent::extension_remote_ui::ExtensionRemoteUiPlacement as Placement;
    let caps = state.theme.capabilities();
    welcome_is_mutable(state)
        && !state.startup_pending
        && state.panel.is_none()
        && state.overlay.is_none()
        && state.tool_input_prompt.is_none()
        && !state.transcript_search_active()
        && state.application_viewport_requested
        && caps.interactive
        && caps.animation
        && caps.color == crate::tui::terminal::ColorDepth::TrueColor
        && !pi_startup(state)
        && state.extension_ui.remote.mount(Placement::Header).is_none()
        && splash_geometry(state, state.size.0).0 == 2
}

/// Coordinates belong to the rendered transcript, before viewport translation.
pub(super) fn mark_rect(state: &ShellState) -> Option<(u16, usize)> {
    if !interactive_mark(state) {
        return None;
    }
    let width = state.transcript_content_width(state.size.0);
    let layout = PresentationLayout::new(&state.theme, width);
    let budget = welcome_row_budget(state, state.size.0);
    if budget < 7 || layout.content_width < 8 {
        return None;
    }
    let (_, _, text_width) = splash_geometry(state, layout.content_width);
    let stacked = text_width < MIN_SPLASH_TEXT_WIDTH || budget < splash_card_rows(state);
    let framed = state.theme.role_rgb("splash_box").is_some();
    Some((
        layout.inset + if stacked { 0 } else { 2 + u16::from(framed) },
        usize::from(!stacked),
    ))
}

pub(super) fn click_mark(state: &mut ShellState, column: u16, row: u16, now: Instant) -> bool {
    if !interactive_mark(state) {
        return false;
    }
    let hit = state.pointer_render_geometry().is_some_and(|geometry| {
        geometry.welcome_mark.is_some_and(|(x, y)| {
            column >= x && column < x + 8 && usize::from(row) >= y && usize::from(row) < y + 2
        })
    });
    if !hit {
        return false;
    }
    let strength = state.welcome_reaction.map_or(0.0, |previous| {
        let gap = now.duration_since(previous.started).as_secs_f32();
        (previous.strength * (1.0 - gap / 0.8).max(0.0) + (1.0 - gap / 0.6).max(0.0) * 0.45)
            .min(1.0)
    });
    let phase = state.welcome_reaction.map_or(0.0, |previous| {
        (previous.phase
            + crate::tui::splash::rainbow_travel(
                now.duration_since(previous.started).as_secs_f32(),
                previous.strength,
            ))
        .rem_euclid(6.0)
    });
    // Let an in-flight expression finish even while clicks rev the rainbow.
    let blink_started = state
        .welcome_reaction
        .filter(|previous| {
            now.duration_since(previous.blink_started).as_secs_f32()
                < crate::tui::splash::BLINK_DURATION
        })
        .map_or(now, |previous| previous.blink_started);
    state.welcome_reaction = Some(MarkReaction {
        started: now,
        blink_started,
        strength,
        phase,
    });
    state.invalidate_transcript();
    true
}

pub(super) fn reaction_active(state: &ShellState) -> bool {
    state.welcome_reaction.is_some()
        && interactive_mark(state)
        && state
            .pointer_render_geometry()
            .is_some_and(|g| g.welcome_mark.is_some())
}

pub(super) fn advance_reaction(state: &mut ShellState, now: Instant) {
    if let Some(reaction) = state.welcome_reaction {
        if !reaction_active(state)
            || now.duration_since(reaction.started).as_secs_f32() >= crate::tui::splash::DURATION
        {
            state.welcome_reaction = None;
        }
        state.invalidate_transcript();
    }
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
        // Identity/permissions and setup or release hint; custom themes keep help.
        return max_rows.min(if compact_welcome(state) { 2 } else { 3 });
    }
    let (_, _, text_width) = splash_geometry(state, width);
    if text_width < MIN_SPLASH_TEXT_WIDTH || max_rows < splash_card_rows(state) {
        // One wordmark or two logo rows, identity, hint and permissions.
        // File themes additionally retain the model/reasoning identity row.
        return (1 + usize::from(width >= 8) + 3 + 2 * usize::from(!compact_welcome(state)))
            .min(max_rows);
    }
    splash_card_rows(state)
}

fn splash_card_rows(state: &ShellState) -> usize {
    // Two-column cards have breathing/frame rows around their text column.
    // The footer owns model identity for the default; file themes keep that
    // row and its following spacer, even with a compact logo.
    let text_rows = if compact_welcome(state) {
        DEFAULT_TEXT_ROWS
    } else {
        FILE_TEXT_ROWS
    };
    text_rows + 2
}

// Native startup geometry belongs to the host, including Pi palette projections.
// Keep palette/model policy independent and other file-theme layouts intact.
fn compact_welcome(state: &ShellState) -> bool {
    state.theme.is_compiled_default()
        || pi_startup(state)
        || (state.theme.metadata().profile == "pi"
            && state
                .theme
                .resolve::<bool>("splash_compact")
                .unwrap_or(true))
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
    let compact = compact_welcome(state)
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
            if compact_welcome(state) {
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
        .take(max_rows.min(if compact_welcome(state) { 2 } else { 3 }))
        .map(|line| fit_line(&line, width))
        .collect();
    }

    // Custom themes can opt into the compiled default's smaller splash
    // geometry with `splash_compact = true`. Everything else keeps the
    // larger file-theme presentation.
    let (rows, logo_width, text_width) = splash_geometry(state, width);
    // Startup identity is static unless its visible mark was clicked.
    // Historical prefixes never acquire a decorative clock.
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
    let mut logo = crate::tui::splash::render_logo(
        &state.theme,
        logo_width,
        rows,
        elapsed,
        adaptive_accent,
        splash_color,
    );
    if interactive_mark(state) {
        if let Some(reaction) = state.welcome_reaction {
            logo = crate::tui::splash::render_reaction(
                &state.theme,
                reaction.started.elapsed().as_secs_f32(),
                reaction.blink_started.elapsed().as_secs_f32(),
                reaction.strength,
                reaction.phase,
                adaptive_accent,
                splash_color,
            );
        }
    }
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
            let logo = if rows == 2 {
                logo
            } else {
                crate::tui::splash::render_logo(
                    &state.theme,
                    8,
                    2,
                    elapsed,
                    adaptive_accent,
                    splash_color,
                )
            };
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
        compact.push(if compact_welcome(state) && model.trim().is_empty() {
            fit_line(&state.theme.fg("warning", &identity), width)
        } else {
            super::startup_update::update_hint(state, width)
                .unwrap_or_else(|| changelog_hint(state, width))
        });
        if !compact_welcome(state) {
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
        if !compact_welcome(state) {
            compact.push(fit_line(&exit, width));
        }
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
    let mut text = if compact_welcome(state) {
        let access = state.theme.bold(&state.theme.fg(
            if state.safe_mode { "accent" } else { "warning" },
            if state.safe_mode {
                "safe mode"
            } else {
                "full access"
            },
        ));
        let separator = state
            .theme
            .dim(if state.theme.unicode() { " · " } else { " - " });
        let hint_width =
            text_width.saturating_sub((sexy_tui_rs::visible_width(&access) + 3) as u16);
        let hint = super::startup_update::update_hint(state, hint_width)
            .unwrap_or_else(|| state.theme.dim("/changelog"));
        let text: [String; DEFAULT_TEXT_ROWS] = [
            format!(
                "{}  {}",
                state.theme.bold("octet"),
                state.theme.dim(&format!("v{}", env!("CARGO_PKG_VERSION"))),
            ),
            format!("{access}{separator}{hint}"),
        ];
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

    if !compact_welcome(state) {
        if let Some(hint) = super::startup_update::update_hint(state, text_width) {
            text[1] = hint;
        }
    }
    // A release hint replaces the changelog row instead of moving the composer.
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

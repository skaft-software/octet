//! Conservative terminal capability detection and renderer-profile mapping.
//!
//! This module decides whether interactive control is safe, selects color
//! depth from the terminal's negotiated environment, and translates that
//! profile for the render-only backend. SSH transport is deliberately not a
//! color-depth signal.
#![allow(missing_docs)]

use std::io::IsTerminal;

use sexy_tui_rs::{
    ColorDepth as SexyColorDepth, SupportLevel as SexySupportLevel,
    TerminalCapabilities as SexyTerminalCapabilities, TerminalSize as SexyTerminalSize,
};

/// ANSI colour policy. Structural glyph and cursor capabilities are detected
/// separately, so forcing colour never forces an alternate-screen TUI.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ColorMode {
    #[default]
    Auto,
    Always,
    Never,
}

impl ColorMode {
    pub fn parse(value: &str) -> anyhow::Result<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "auto" => Ok(Self::Auto),
            "always" | "on" => Ok(Self::Always),
            "never" | "off" => Ok(Self::Never),
            _ => anyhow::bail!("invalid colour mode {value:?}; use auto, always, or never"),
        }
    }
}

/// Colour precision that can be emitted without changing the information
/// architecture. `None` also suppresses non-colour SGR attributes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ColorDepth {
    None,
    Ansi16,
    Ansi256,
    TrueColor,
}

/// Capabilities selected before any terminal control sequence is written.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TerminalCapabilities {
    /// Safe to enter raw mode and use differential cursor rendering.
    pub interactive: bool,
    /// Safe Unicode is expected to render as single-cell glyphs.
    pub unicode: bool,
    /// Selected foreground-colour precision.
    pub color: ColorDepth,
    /// Optional italic SGR is likely to work.
    pub italics: bool,
    /// OSC 8 links are supported by the detected terminal family. Markdown
    /// destinations remain visible text when this is false.
    pub hyperlinks: bool,
    /// Cursor-rewritten animation is allowed. octet's elapsed clock does not
    /// depend on this flag for comprehension.
    pub animation: bool,
}

#[derive(Clone, Debug)]
struct CapabilityProbe {
    stdin_tty: bool,
    stdout_tty: bool,
    term: Option<String>,
    term_program: Option<String>,
    colorterm: Option<String>,
    locale: Option<String>,
    no_color: bool,
    explicit_plain: bool,
}

fn known_terminal(term: &str) -> bool {
    [
        "xterm",
        "screen",
        "tmux",
        "rxvt",
        "vt",
        "ansi",
        "linux",
        "cygwin",
        "cons",
        "eterm",
        "konsole",
        "gnome",
        "putty",
        "st",
        "alacritty",
        "kitty",
        "foot",
        "wezterm",
        "ghostty",
        "contour",
        "rio",
        "mlterm",
        "terminator",
        "fbterm",
        "iterm",
    ]
    .iter()
    .any(|family| {
        term == *family
            || term.strip_prefix(family).is_some_and(|suffix| {
                suffix.chars().next().is_some_and(|character| {
                    matches!(character, '-' | '_') || character.is_ascii_digit()
                })
            })
    })
}

impl TerminalCapabilities {
    /// Detect the frontend tier. Unknown, dumb, redirected, and explicitly
    /// plain environments never enter alternate-screen mode.
    pub fn detect(color_mode: ColorMode, explicit_plain: bool) -> Self {
        let locale = std::env::var("LC_ALL")
            .ok()
            .filter(|value| !value.is_empty())
            .or_else(|| {
                std::env::var("LC_CTYPE")
                    .ok()
                    .filter(|value| !value.is_empty())
            })
            .or_else(|| std::env::var("LANG").ok().filter(|value| !value.is_empty()));
        Self::from_probe(
            color_mode,
            CapabilityProbe {
                stdin_tty: std::io::stdin().is_terminal(),
                stdout_tty: std::io::stdout().is_terminal(),
                term: std::env::var("TERM").ok().filter(|value| !value.is_empty()),
                term_program: std::env::var("TERM_PROGRAM")
                    .ok()
                    .filter(|value| !value.is_empty()),
                colorterm: std::env::var("COLORTERM")
                    .ok()
                    .filter(|value| !value.is_empty()),
                locale,
                no_color: std::env::var_os("NO_COLOR").is_some(),
                explicit_plain,
            },
        )
    }

    fn from_probe(color_mode: ColorMode, probe: CapabilityProbe) -> Self {
        let term = probe
            .term
            .as_deref()
            .unwrap_or_default()
            .to_ascii_lowercase();
        let known = known_terminal(&term);
        let interactive = probe.stdin_tty && probe.stdout_tty && known && !probe.explicit_plain;
        let unicode = !probe.explicit_plain
            && known
            && probe.locale.as_deref().is_some_and(|locale| {
                let locale = locale.to_ascii_lowercase();
                locale.contains("utf-8") || locale.contains("utf8")
            });

        // SSH transports the remote process's terminal environment. It is not
        // itself a colour capability boundary: TERM/COLORTERM remain the
        // authoritative negotiated values, just as they are locally.
        let ansi_allowed = !probe.no_color
            && term != "dumb"
            && !probe.explicit_plain
            && match color_mode {
                ColorMode::Auto => probe.stdout_tty && known,
                ColorMode::Always => true,
                ColorMode::Never => false,
            };
        let color = if !ansi_allowed {
            ColorDepth::None
        } else if probe.colorterm.as_deref().is_some_and(|value| {
            value.eq_ignore_ascii_case("truecolor") || value.eq_ignore_ascii_case("24bit")
        }) || term.contains("ghostty")
            || term.contains("kitty")
            || term.contains("wezterm")
        {
            ColorDepth::TrueColor
        } else if term.contains("256color") {
            ColorDepth::Ansi256
        } else {
            ColorDepth::Ansi16
        };

        let rich_terminal = term.contains("ghostty")
            || term.contains("kitty")
            || term.contains("wezterm")
            || probe.term_program.as_deref().is_some_and(|program| {
                program == "iTerm.app" || program == "WezTerm" || program == "ghostty"
            });
        // Apple Terminal supports SGR italics even though it is not an OSC 8
        // hyperlink target. Keep the capabilities separate: otherwise the
        // rich renderer's italic fallback becomes underline, making every
        // thinking row look like a link.
        let apple_terminal = probe.term_program.as_deref().is_some_and(|program| {
            matches!(
                program.to_ascii_lowercase().as_str(),
                "apple_terminal" | "apple terminal"
            )
        });
        Self {
            interactive,
            unicode,
            color,
            italics: interactive && (rich_terminal || apple_terminal) && color != ColorDepth::None,
            hyperlinks: interactive && rich_terminal,
            // A colourless terminal still gets the complete static splash;
            // cursor animation would only redraw identical bytes at 60 fps.
            animation: interactive && !probe.explicit_plain && color != ColorDepth::None,
        }
    }

    #[cfg(test)]
    pub fn test(interactive: bool, unicode: bool, color: ColorDepth) -> Self {
        Self {
            interactive,
            unicode,
            color,
            italics: interactive && color == ColorDepth::TrueColor,
            hyperlinks: interactive && color == ColorDepth::TrueColor,
            animation: interactive && color != ColorDepth::None,
        }
    }
}

/// Convert octet's negotiated terminal profile into the profile consumed by
/// `sexy-tui-rs`. The synchronized-output flags are also the backend's frame
/// boundary: terminals without CSI 2026 support safely ignore the private mode.
pub(super) fn sexy_terminal_capabilities(
    capabilities: TerminalCapabilities,
    dimensions: (u16, u16),
) -> SexyTerminalCapabilities {
    if !capabilities.interactive {
        return SexyTerminalCapabilities::plain();
    }

    let color_depth = match capabilities.color {
        ColorDepth::None => SexyColorDepth::None,
        ColorDepth::Ansi16 => SexyColorDepth::Ansi16,
        ColorDepth::Ansi256 => SexyColorDepth::Ansi256,
        ColorDepth::TrueColor => SexyColorDepth::TrueColor,
    };
    let mut rendered = SexyTerminalCapabilities::interactive(color_depth, capabilities.unicode);
    rendered.italics = if capabilities.italics {
        SexySupportLevel::Supported
    } else {
        SexySupportLevel::Unsupported
    };
    rendered.hyperlinks = capabilities.hyperlinks;
    rendered.animation = capabilities.animation;
    rendered.dimensions = Some(SexyTerminalSize {
        columns: dimensions.0,
        rows: dimensions.1,
    });
    // Reuse the foundation's conservative environment hints only for image
    // protocols. octet's own interactive/plain decision above remains the
    // authority for every other frontend capability.
    let image_hints = SexyTerminalCapabilities::detect();
    rendered.kitty_graphics = image_hints.kitty_graphics;
    rendered.iterm2_images = image_hints.iterm2_images;
    rendered.cell_pixel_size = image_hints.cell_pixel_size;

    // sexy-tui has no end-of-frame callback other than these flags. The
    // backend uses them to batch per-line writes into one flush.
    rendered.synchronized_output = true;
    rendered.sync_output = true;
    rendered
}

#[cfg(test)]
#[path = "capabilities_tests.rs"]
mod tests;

//! One-shot update presentation; networking and cancellation belong to the driver.

use sexy_tui_rs::{parse_markdown, visible_width, Block, Document, Inline, RichRenderer, TextRole};

use super::renderer_runtime::RenderCommand;
use super::{fit_line, InteractiveShell, ShellState, TranscriptBlock};
use crate::tui::terminal::ColorDepth;
use crate::tui::theme::OctetTheme;

const UPDATE_COMMAND: &str = "octet update";

pub(super) fn update_message(version: &semver::Version) -> String {
    format!("Update available: octet {version} - run {UPDATE_COMMAND}")
}

fn update_document(prefix: &str) -> Document {
    // Retain semantic inline code, not literal backticks or a hand-painted
    // imitation. The ordinary rich renderer owns code styles and fallbacks.
    let mut document = parse_markdown(&format!("{prefix}`{UPDATE_COMMAND}`"));
    if let Block::Paragraph(content) = &mut document.blocks[0] {
        *content = vec![Inline::Role {
            role: TextRole::Accent,
            content: std::mem::take(content),
        }];
    }
    document
}

fn render_update(
    prefix: &str,
    renderer: &RichRenderer,
    theme: &OctetTheme,
    width: u16,
) -> Vec<String> {
    renderer
        .render(&update_document(prefix), width.max(1))
        .lines
        .into_iter()
        .map(|line| {
            if theme.capabilities().color == ColorDepth::None {
                line.plain
            } else {
                line.styled
            }
        })
        .collect()
}

pub(super) fn render_update_notice(
    version: &semver::Version,
    renderer: &RichRenderer,
    theme: &OctetTheme,
    width: u16,
) -> Vec<String> {
    render_update(
        &format!("Update available: octet {version} - run "),
        renderer,
        theme,
        width,
    )
}

pub(super) fn update_hint(state: &ShellState, width: u16) -> Option<String> {
    let version = state.available_update.as_ref()?;
    let (arrow, separator) = if state.theme.unicode() {
        ("↑", "·")
    } else {
        ("Update", "-")
    };
    let full = format!("{arrow} v{version} available {separator} run ");
    let prefix = [full.as_str(), "Update: ", ""]
        .into_iter()
        .find(|prefix| visible_width(prefix) + UPDATE_COMMAND.len() <= usize::from(width))
        .unwrap_or("");
    // Select the whole semantic hint before rendering. Tiny panes clip the
    // interpreted command, never Markdown delimiters or a wrapped footer.
    let lines = render_update(
        prefix,
        &state.theme.rich_renderer(),
        &state.theme,
        width.max(UPDATE_COMMAND.len() as u16),
    );
    Some(fit_line(&lines[0], width))
}

impl InteractiveShell {
    /// Return a presentation-only callback for the driver's single bounded
    /// release check. It owns neither an Agent nor network configuration.
    pub(crate) fn startup_update_notifier(&self) -> impl FnOnce(semver::Version) + Send + 'static {
        let state = self.state.clone();
        let render_tx = self.render_tx.clone();
        move |version| {
            let mut state = state.borrow_mut();
            if state.close_requested {
                return;
            }
            if state.startup_pending || state.transcript.is_empty() {
                state.available_update = Some(version);
                // Replace the bounded welcome prefix, not historical Markdown.
                state.invalidate_transcript();
            } else {
                // A slow check must not rewrite a splash already in native
                // history (especially on resume). Retain one ordinary UI-only
                // notice at the live tail instead; never persist model context.
                state.late_update_notice = Some(version.clone());
                state.push_block(TranscriptBlock::UpdateAvailable(version));
            }
            drop(state);
            if let Some(render_tx) = render_tx
                .lock()
                .expect("renderer sender mutex poisoned")
                .as_ref()
            {
                let _ = render_tx.try_send(RenderCommand::Render);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use sexy_tui_rs::{strip_terminal_sequences, visible_width};

    use super::super::welcome_card;
    use super::*;
    use crate::tui::terminal::{ColorDepth, TerminalCapabilities};

    fn release() -> semver::Version {
        semver::Version::new(9, 8, 7)
    }

    fn ready_shell() -> InteractiveShell {
        let shell = InteractiveShell::test_shell();
        shell.state.borrow_mut().startup_card_started_at =
            Some(Instant::now() - Duration::from_secs(10));
        shell
    }

    #[test]
    fn update_before_readiness_is_retained_for_the_first_splash() {
        let mut shell = InteractiveShell::test_shell();
        shell.state.borrow_mut().startup_pending = true;
        shell.startup_update_notifier()(release());
        assert!(shell.state.borrow().transcript.is_empty());
        shell.finish_startup();
        let state = shell.state.borrow();
        let rows = welcome_card::render_welcome_card(&state, 100, 10, Instant::now());
        let text = strip_terminal_sequences(&rows.join("\n"));
        assert!(text.contains("↑ v9.8.7 available · run octet update"));
        assert!(text.contains("/changelog · what's new"));
    }

    #[test]
    fn update_replaces_a_warm_welcome_prefix_and_wakes_the_renderer() {
        let shell = ready_shell();
        let before = shell.state.borrow().rendered_transcript(100).to_vec();
        let (tx, rx) = std::sync::mpsc::sync_channel(1);
        *shell.render_tx.lock().unwrap() = Some(tx);
        shell.startup_update_notifier()(release());
        assert!(matches!(rx.try_recv(), Ok(RenderCommand::Render)));
        *shell.render_tx.lock().unwrap() = None;
        let state = shell.state.borrow();
        let after = state.rendered_transcript(100).to_vec();
        assert!(!strip_terminal_sequences(&before.join("\n")).contains("octet update"));
        assert!(strip_terminal_sequences(&after.join("\n")).contains("octet update"));
        assert!(state.transcript.is_empty());
    }

    #[test]
    fn late_update_appends_without_repainting_historical_splash_or_changing_draft() {
        let mut shell = ready_shell();
        shell.on_prompt_submitted("retained user prompt");
        let before = shell.state.borrow().rendered_transcript(100).to_vec();
        let editor = shell.state.borrow().editor.text().to_owned();
        shell.startup_update_notifier()(release());
        let state = shell.state.borrow();
        assert_eq!(state.editor.text(), editor);
        assert!(state.available_update.is_none());
        let after = state.rendered_transcript(100).to_vec();
        assert_eq!(&after[..before.len()], before.as_slice());
        assert!(
            matches!(state.transcript.last(), Some(TranscriptBlock::UpdateAvailable(version))
            if version == &release())
        );
    }

    #[test]
    fn notice_only_history_is_not_rewritten_by_a_late_update() {
        let mut shell = ready_shell();
        shell.notice("retained startup diagnostic");
        let before = shell.state.borrow().rendered_transcript(100).to_vec();
        shell.startup_update_notifier()(release());
        let state = shell.state.borrow();
        assert!(state.available_update.is_none());
        let after = state.rendered_transcript(100).to_vec();
        assert_eq!(&after[..before.len()], before.as_slice());
        assert!(
            matches!(state.transcript.last(), Some(TranscriptBlock::UpdateAvailable(version))
            if version == &release())
        );
    }

    #[test]
    fn update_wakes_the_current_renderer_after_sender_replacement() {
        let shell = ready_shell();
        let (old_tx, old_rx) = std::sync::mpsc::sync_channel(1);
        *shell.render_tx.lock().unwrap() = Some(old_tx);
        let notify = shell.startup_update_notifier();
        let (new_tx, new_rx) = std::sync::mpsc::sync_channel(1);
        *shell.render_tx.lock().unwrap() = Some(new_tx);
        notify(release());
        assert!(old_rx.try_recv().is_err());
        assert!(matches!(new_rx.try_recv(), Ok(RenderCommand::Render)));
        *shell.render_tx.lock().unwrap() = None;
    }

    #[test]
    fn late_update_survives_rehydration_once_without_persisting_session_input() {
        let directory = tempfile::tempdir().unwrap();
        let session = octet_agent::Session::create(directory.path().join("session.jsonl")).unwrap();
        let mut shell = ready_shell();
        shell.notice("startup diagnostic");
        shell.startup_update_notifier()(release());
        for _ in 0..3 {
            shell.hydrate(&session).unwrap();
            assert_eq!(shell.debug_snapshot().matches("octet update").count(), 1);
            assert!(shell.state.borrow().available_update.is_none());
        }
        assert!(session.entries().is_empty());
    }

    #[test]
    fn update_after_close_is_ignored() {
        let shell = ready_shell();
        let notify = shell.startup_update_notifier();
        shell.state.borrow_mut().close_requested = true;
        notify(release());
        let state = shell.state.borrow();
        assert!(state.available_update.is_none());
        assert!(state.transcript.is_empty());
    }

    #[test]
    fn update_reserves_space_without_cutting_custom_frame_borders() {
        let shell =
            InteractiveShell::test_shell_with_theme(crate::tui::theme::test_theme_from_source(
                r##"[metadata]
name = "Update fixture"
adaptive = false
[colors]
splash = "#d97757"
splash_box = "#d97757"
"##,
            ));
        {
            let mut state = shell.state.borrow_mut();
            state.startup_card_started_at = Some(Instant::now());
            state.available_update = Some(release());
        }
        for (width, height) in [1, 8, 12, 24, 40, 46, 80]
            .into_iter()
            .flat_map(|width| [7, 8, 9, 10].into_iter().map(move |height| (width, height)))
        {
            let rows = welcome_card::render_welcome_card(
                &shell.state.borrow(),
                width,
                height,
                Instant::now(),
            );
            assert!(rows.len() <= height);
            let plain = rows
                .iter()
                .map(|row| strip_terminal_sequences(row))
                .collect::<Vec<_>>();
            assert!(rows
                .iter()
                .all(|line| visible_width(line) <= usize::from(width)));
            if width >= 12 {
                assert!(plain.iter().any(|line| line.contains("octet update")));
            }
            if plain[0].starts_with('╭') {
                assert!(plain[plain.len() - 1].starts_with('╰'));
                assert!(plain[plain.len() - 1].ends_with('╯'));
            }
        }
    }

    #[test]
    fn update_hint_preserves_action_and_is_bounded_at_every_colour_depth() {
        for (color, unicode) in [
            ColorDepth::TrueColor,
            ColorDepth::Ansi256,
            ColorDepth::Ansi16,
            ColorDepth::None,
        ]
        .into_iter()
        .flat_map(|color| {
            [false, true]
                .into_iter()
                .map(move |unicode| (color, unicode))
        }) {
            let shell =
                InteractiveShell::test_shell_with_theme(crate::tui::theme::test_theme_with(
                    TerminalCapabilities::test(true, unicode, color),
                ));
            {
                let mut state = shell.state.borrow_mut();
                state.startup_card_started_at = Some(Instant::now());
                state.available_update = Some(release());
            }
            for width in [1, 8, 12, 16, 24, 40, 46, 80, 120] {
                for height in [7, 8, 9, 10] {
                    let rows = welcome_card::render_welcome_card(
                        &shell.state.borrow(),
                        width,
                        height,
                        Instant::now(),
                    );
                    assert!(rows.len() <= height);
                    assert!(rows
                        .iter()
                        .all(|row| visible_width(row) <= usize::from(width)));
                    let text = strip_terminal_sequences(&rows.join("\n"));
                    if width >= 12 {
                        assert!(text.contains("octet update"), "{width}x{height}: {text}");
                    }
                    if color == ColorDepth::None {
                        assert!(!rows.join("\n").contains('\x1b'));
                    }
                }
            }
        }
    }

    #[test]
    fn release_hints_share_the_version_column_without_moving_the_logo() {
        for framed in [false, true] {
            let theme = if framed {
                crate::tui::theme::test_theme_from_source(
                    "[metadata]\nname = 'Frame'\n[colors]\nsplash_box = '#d97757'\n",
                )
            } else {
                crate::tui::theme::test_theme()
            };
            let shell = InteractiveShell::test_shell_with_theme(theme);
            shell.state.borrow_mut().startup_card_started_at = Some(Instant::now());
            for width in [46, 80, 120] {
                let mut occupied_before = None;
                for update in [false, true] {
                    shell.state.borrow_mut().available_update = update.then(release);
                    let rows = welcome_card::render_welcome_card(
                        &shell.state.borrow(),
                        width,
                        10,
                        Instant::now(),
                    );
                    let plain = rows
                        .iter()
                        .map(|row| strip_terminal_sequences(row))
                        .collect::<Vec<_>>();
                    let locate = |needle: &str| {
                        plain
                            .iter()
                            .enumerate()
                            .find_map(|(row, text)| {
                                text.find(needle)
                                    .map(|column| (row, visible_width(&text[..column])))
                            })
                            .unwrap()
                    };
                    let title = locate(&format!("octet v{}", env!("CARGO_PKG_VERSION")));
                    assert_eq!(locate("/changelog"), (title.0 + 1, title.1));
                    assert_eq!(
                        plain
                            .iter()
                            .filter(|line| line.contains("/changelog"))
                            .count(),
                        1
                    );
                    if update {
                        let (row, _) = locate(UPDATE_COMMAND);
                        let prefix = if plain[row].contains('↑') {
                            "↑"
                        } else {
                            "Update:"
                        };
                        assert_eq!(locate(prefix), (title.0 + 2, title.1));
                    } else {
                        assert!(!plain.join("\n").contains(UPDATE_COMMAND));
                    }
                    if framed {
                        assert!(plain.last().unwrap().starts_with('╰'));
                    } else {
                        assert!(plain.last().unwrap().is_empty());
                    }
                    let occupied = plain
                        .iter()
                        .enumerate()
                        .flat_map(|(row, line)| {
                            line.chars().enumerate().filter_map(move |(column, ch)| {
                                (ch == '█').then_some((row, column))
                            })
                        })
                        .collect::<Vec<_>>();
                    assert!(!occupied.is_empty());
                    if let Some(before) = &occupied_before {
                        assert_eq!(&occupied, before, "an update moved the byte");
                    } else {
                        occupied_before = Some(occupied);
                    }
                }
            }
        }
    }

    #[test]
    fn update_action_is_semantic_inline_code_with_canonical_rich_styles() {
        use crate::tui::theme::TerminalBackground;
        let document = update_document("Update: ");
        let Block::Paragraph(content) = &document.blocks[0] else {
            panic!("expected paragraph")
        };
        let Inline::Role { content, .. } = &content[0] else {
            panic!("expected accent role")
        };
        assert!(content
            .iter()
            .any(|span| matches!(span, Inline::Code(code) if code == UPDATE_COMMAND)));
        assert_eq!(document.plain_text().trim(), "Update: octet update");

        for background in [TerminalBackground::Light, TerminalBackground::Dark] {
            for color in [
                ColorDepth::TrueColor,
                ColorDepth::Ansi256,
                ColorDepth::Ansi16,
                ColorDepth::None,
            ] {
                for unicode in [false, true] {
                    let theme = crate::tui::theme::test_theme_for(
                        background,
                        TerminalCapabilities::test(true, unicode, color),
                    );
                    let shell = InteractiveShell::test_shell_with_theme(theme.clone());
                    shell.state.borrow_mut().available_update = Some(release());
                    let hint = update_hint(&shell.state.borrow(), 80).unwrap();
                    let plain = strip_terminal_sequences(&hint);
                    assert!(!plain.contains('`'));
                    if !unicode {
                        assert!(plain.is_ascii());
                    }
                    let offset = visible_width(&plain[..plain.find(UPDATE_COMMAND).unwrap()]);
                    let expected = theme
                        .rich_renderer()
                        .render(&parse_markdown("`octet update`"), 80);
                    let mut actual_screen = vt100::Parser::new(2, 80, 0);
                    let mut expected_screen = vt100::Parser::new(2, 80, 0);
                    actual_screen.process(hint.as_bytes());
                    expected_screen.process(expected.lines[0].styled.as_bytes());
                    for column in 0..UPDATE_COMMAND.len() {
                        let actual = actual_screen
                            .screen()
                            .cell(0, (offset + column) as u16)
                            .unwrap();
                        let expected = expected_screen.screen().cell(0, column as u16).unwrap();
                        assert_eq!(actual.contents(), expected.contents());
                        assert_eq!(actual.fgcolor(), expected.fgcolor());
                        assert_eq!(actual.bgcolor(), expected.bgcolor());
                        assert_eq!(actual.bold(), expected.bold());
                        assert_eq!(actual.italic(), expected.italic());
                    }
                    if color == ColorDepth::None {
                        assert!(!hint.contains('\x1b'));
                    }
                }
            }
        }
    }

    #[test]
    fn late_update_renders_rich_but_copies_cleanly_and_keeps_plain_notices_literal() {
        use super::super::{
            transcript_render::render_block, transcript_selection::block_copy_text,
        };
        let block = TranscriptBlock::UpdateAvailable(release());
        assert_eq!(block_copy_text(&block), update_message(&release()));
        let plain_notice = TranscriptBlock::Notice("literal `octet update`".into());
        assert_eq!(block_copy_text(&plain_notice), "literal `octet update`");
        for color in [
            ColorDepth::TrueColor,
            ColorDepth::Ansi256,
            ColorDepth::Ansi16,
            ColorDepth::None,
        ] {
            let theme =
                crate::tui::theme::test_theme_with(TerminalCapabilities::test(true, true, color));
            let renderer = theme.rich_renderer();
            for width in [1, 12, 24, 46, 80, 120] {
                let rows = render_block(None, &block, &theme, &renderer, &renderer, width, false);
                assert!(rows
                    .iter()
                    .all(|row| visible_width(row) <= usize::from(width)));
                assert!(!strip_terminal_sequences(&rows.join("\n")).contains('`'));
                if width >= 80 {
                    let code = renderer.render(&parse_markdown("`octet update`"), 80);
                    assert!(rows.iter().any(|row| row.contains(&code.lines[0].styled)));
                }
                if color == ColorDepth::None {
                    assert!(!rows.join("\n").contains('\x1b'));
                }
            }
            let literal =
                render_block(None, &plain_notice, &theme, &renderer, &renderer, 80, false)
                    .join("\n");
            assert!(strip_terminal_sequences(&literal).contains("literal `octet update`"));
        }
    }

    #[test]
    fn pi_startup_groups_both_release_hints_under_its_version() {
        let mut theme = crate::tui::theme::test_theme();
        theme.override_token("startup", "pi");
        let shell = InteractiveShell::test_shell_with_theme(theme);
        shell.state.borrow_mut().startup_card_started_at = Some(Instant::now());
        shell.state.borrow_mut().available_update = Some(release());
        for height in [7, 8, 9, 10] {
            let rows = welcome_card::render_welcome_card(
                &shell.state.borrow(),
                80,
                height,
                Instant::now(),
            );
            let plain = rows
                .iter()
                .map(|row| strip_terminal_sequences(row))
                .collect::<Vec<_>>();
            assert!(rows.len() <= height);
            assert!(plain[0].starts_with("octet v"));
            assert!(plain[1].starts_with("/changelog"));
            assert!(plain[2].contains(UPDATE_COMMAND));
            assert!(!plain.join("\n").contains('`'));
        }
    }
}

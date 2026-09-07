//! First branded frame is a launch-readiness boundary, not a terminal-entry side effect.

use super::*;
use crate::tui::terminal::{ColorDepth, TerminalCapabilities};
use crate::tui::theme::{test_theme_for, TerminalBackground};
use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
use sexy_tui_rs::{Component, CURSOR_MARKER};

fn pending_shell() -> InteractiveShell {
    let mut shell = InteractiveShell::test_shell();
    shell.set_size(96, 18);
    shell.state.borrow_mut().startup_pending = true;
    shell
}

fn plain(lines: &[String]) -> String {
    strip_terminal_sequences(&lines.join("\n"))
}

fn assert_unbranded(lines: &[String]) {
    let text = plain(lines);
    assert!(!text.contains("octet"), "{text}");
    assert!(!text.contains('█'), "{text}");
    assert!(!text.contains("selecting model"), "{text}");
    assert!(!text.contains("workspace unavailable"), "{text}");
}

#[test]
fn first_branded_frame_waits_for_identity_workspace_and_appearance() {
    for application_viewport in [false, true] {
        let mut shell = pending_shell();
        let component = ShellComponent::new(shell.state.clone(), application_viewport);
        assert_unbranded(&component.render(96));
        shell.set_identity("cerebras", "cerebras/gemma-4-31b", "off");
        assert_unbranded(&component.render_update(96).unwrap().replacement);
        shell.set_workspace(PathBuf::from("/startup-fixture/workspace"));
        shell.set_theme(test_theme_for(
            TerminalBackground::Dark,
            TerminalCapabilities::test(true, true, ColorDepth::TrueColor),
        ));
        assert_unbranded(&component.render(96));
        {
            let state = shell.state.borrow();
            assert!(state.startup_card_started_at.is_none());
            assert!(!welcome_card::welcome_animating(&state, Instant::now()));
            assert!(state.transcript_cache.borrow().width.is_none());
        }

        let ready = Instant::now();
        shell.finish_startup();
        let update = component.render_update(96).unwrap();
        assert_eq!(
            update.stable_prefix, 0,
            "setup rows are not a retained prefix"
        );
        let text = plain(&update.replacement);
        assert!(text.contains("cerebras/gemma-4-31b"), "{text}");
        assert!(text.contains("/startup-fixture/workspace"), "{text}");
        assert!(!text.contains("selecting model"), "{text}");
        assert!(!text.contains("workspace unavailable"), "{text}");
        let state = shell.state.borrow();
        let started = state.startup_card_started_at.unwrap();
        assert!(started >= ready);
        assert!(welcome_card::welcome_animating(&state, started));
        assert!(!welcome_card::welcome_animating(
            &state,
            started + Duration::from_secs(3)
        ));
        assert_eq!(state.model_lab, Some(ModelLab::Google));
        let accent = state.theme.model_rgb(state.model_lab).unwrap();
        assert!(accent.1 > accent.0 && accent.1 > accent.2, "Gemma is green");
        let wordmark = state.theme.fg("model_accent", "octet");
        assert!(update
            .replacement
            .iter()
            .any(|line| line.contains(&wordmark)));
        let rule = state.theme.rgb_fg(accent, &"─".repeat(96));
        assert_eq!(
            update
                .replacement
                .iter()
                .filter(|line| **line == rule)
                .count(),
            2
        );
    }
}

#[test]
fn startup_input_owners_render_and_resize_without_releasing_branding() {
    for (width, height) in [(46, 8), (96, 18), (120, 40)] {
        let mut shell = pending_shell();
        shell.set_size(width, height);
        let component = ShellComponent::new(shell.state.clone(), false);
        let items = vec![
            "Auto".into(),
            "Light terminal".into(),
            "Dark terminal".into(),
        ];
        shell.open_panel(Panel::SelectList {
            surface: OrdinarySurfaceMetadata::new("Choose terminal appearance"),
            items: items.clone(),
            descriptions: vec![None; items.len()],
            selected: 0,
            filter: String::new(),
            action: PanelAction::ProviderSetup(items),
        });
        let frame = component.render(width);
        assert_unbranded(&frame);
        assert!(plain(&frame).contains("Choose terminal appearance"));
        assert!(frame.iter().any(|line| line.contains(CURSOR_MARKER)));
        assert_eq!(frame.len(), usize::from(height));
        shell.panel_input(&Event::Key(KeyEvent::new(
            KeyCode::Down,
            KeyModifiers::NONE,
        )));
        shell.set_theme(test_theme_for(
            TerminalBackground::Light,
            TerminalCapabilities::test(true, true, ColorDepth::TrueColor),
        ));
        assert_unbranded(&component.render_update(width).unwrap().replacement);
        let selected = shell.panel_input(&Event::Key(KeyEvent::new(
            KeyCode::Enter,
            KeyModifiers::NONE,
        )));
        assert!(matches!(selected, Some((PanelResult::Confirm(1), _))));
        shell.close_panel();
        shell.set_tool_input_prompt(Some("Endpoint URL: http://127.0.0.1:9/v1/".into()));
        let input = component.render(width);
        assert_unbranded(&input);
        assert!(plain(&input).contains("Endpoint URL:"));
        assert!(input.iter().any(|line| line.contains(CURSOR_MARKER)));
        assert!(input
            .iter()
            .all(|line| visible_width(line) <= usize::from(width)));
        shell.set_tool_input_prompt(None);
        shell.set_run_label("signing in…");
        assert!(plain(&component.render(width)).contains("signing in"));
        shell.show_overlay_text("Verification instructions".into());
        assert!(plain(&component.render(width)).contains("Verification instructions"));
        assert!(shell.state.borrow().startup_card_started_at.is_none());
    }
}

#[test]
fn readiness_inserts_one_welcome_prefix_into_a_warm_cache() {
    let mut shell = pending_shell();
    shell.set_workspace(PathBuf::from("/startup-fixture/workspace"));
    shell.notice("retained setup notice");
    let before = shell.state.borrow().rendered_transcript(96).clone();
    assert!(!plain(&before).contains("octet"));
    shell.finish_startup();
    let started = shell.state.borrow().startup_card_started_at;
    shell.finish_startup();
    assert_eq!(shell.state.borrow().startup_card_started_at, started);
    let state = shell.state.borrow();
    let rendered = state.rendered_transcript(96);
    let text = plain(&rendered);
    assert_eq!(text.matches("retained setup notice").count(), 1, "{text}");
    assert_eq!(text.matches("octet v").count(), 1, "{text}");
    assert!(
        text.contains("no configured model · setup needed"),
        "{text}"
    );
    assert!(!text.contains("selecting model"), "{text}");
    assert_eq!(state.transcript_cache.borrow().last_update_start, 0);
    assert_eq!(state.transcript_cache.borrow().block_starts, [8]);
}

#[test]
fn renderer_reconstruction_preserves_pending_and_ready_startup_state() {
    let mut shell = pending_shell();
    shell.set_run_label("signing in…");
    for application_viewport in [false, true] {
        let component = ShellComponent::new(shell.state.clone(), application_viewport);
        assert_unbranded(&component.render(96));
    }
    shell.set_run_label("idle");
    shell.set_workspace(PathBuf::from("/startup-fixture/workspace"));
    shell.finish_startup();
    let started = shell.state.borrow().startup_card_started_at;
    for application_viewport in [false, true] {
        let component = ShellComponent::new(shell.state.clone(), application_viewport);
        let text = plain(&component.render(96));
        assert_eq!(text.matches("octet v").count(), 1, "{text}");
        assert!(text.contains("setup needed"), "{text}");
        assert_eq!(shell.state.borrow().startup_card_started_at, started);
    }
}

//! Pi presentation across built-in, direct JSON and adapter-normalized snapshots.
use super::*;
use crate::extensions::resource_paths::pi_theme;
use crate::tui::terminal::{ColorDepth, TerminalCapabilities};
use crate::tui::theme::{compiled_theme_for_selector, test_theme_from_source};
use sexy_tui_rs::Color;

fn themes() -> Vec<OctetTheme> {
    let mut document = pi_theme::fixture("Pi presentation QA", "#00aaff");
    for (token, color) in [
        ("text", "#ddeeff"),
        ("userMessageText", "#ddeeff"),
        ("userMessageBg", "#223344"),
        ("toolPendingBg", "#182838"),
        ("toolSuccessBg", "#183828"),
        ("toolErrorBg", "#381828"),
    ] {
        document["colors"][token] = serde_json::json!(color);
    }
    let source = pi_theme::native_source(std::path::Path::new("qa.json"), &document.to_string())
        .unwrap()
        .into_owned();
    vec![
        compiled_theme_for_selector(
            "pi",
            TerminalCapabilities::test(true, true, ColorDepth::TrueColor),
            TerminalBackground::Dark,
        )
        .unwrap()
        .unwrap(),
        test_theme_from_source(&source),
        test_theme_from_source(include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../extensions/octet-pi-compat/test/fixtures/pi-native-layout.toml"
        ))),
    ]
}

#[test]
fn pi_prompt_is_a_three_row_full_width_band_without_a_chevron_or_model_wash() {
    for theme in themes() {
        assert!(theme.is_pi_theme());
        let renderer = theme.rich_renderer();
        let block = TranscriptBlock::User {
            text: "Hello **Pi**".into(),
            model_lab: Some(ModelLab::Anthropic),
            prompt_color: Some("#ff0000".into()),
            persisted: false,
        };
        for width in [20, 80, 160] {
            let rows = render_block(None, &block, &theme, &renderer, &renderer, width, false);
            assert_eq!(rows.len(), 3, "{rows:?}");
            let content = strip_terminal_sequences(&rows[1]);
            assert!(content.starts_with(" Hello Pi"), "{content:?}");
            assert!(!content.contains(theme.glyph("prompt")));
            let expected = theme
                .semantic_style("extension.pi.userMessageBg")
                .background;
            for row in &rows {
                let parser = surface_frame::emulate_row_for_test(row, width);
                for column in 0..width {
                    let color = parser.screen().cell(0, column).unwrap().bgcolor();
                    if let Color::Rgb(r, g, b) = expected {
                        assert_eq!(color, vt100::Color::Rgb(r, g, b));
                    }
                }
            }
            assert!(
                rows[1].contains("\x1b[1m"),
                "Markdown emphasis survives band painting"
            );
        }
    }
}

#[test]
fn pi_tool_lifecycle_selects_pending_success_and_error_fills_before_framing() {
    for theme in themes() {
        let renderer = theme.rich_renderer();
        for (finished, is_error, token) in [
            (false, false, "toolPendingBg"),
            (true, false, "toolSuccessBg"),
            (true, true, "toolErrorBg"),
        ] {
            let args = serde_json::json!({"command":"printf result"});
            let block = TranscriptBlock::Tool(Box::new(ToolPanel::new(
                ToolCallId("pi-bash".into()),
                "bash".into(),
                args.to_string(),
                summarize_tool("bash", &args),
                "result".into(),
                finished,
                is_error,
                None,
                None,
            )));
            let role = format!("extension.pi.{token}");
            let expected = theme.semantic_style(&role).background;
            let plan = compile_surface_plan(None, &block, &theme, 80);
            assert_eq!(plan.content_role, role);
            let rows = render_block(None, &block, &theme, &renderer, &renderer, 80, false);
            for row in rows.iter().filter(|row| !row.is_empty()) {
                let parser = surface_frame::emulate_row_for_test(row, 80);
                for column in [0, 1, 79] {
                    if let Color::Rgb(r, g, b) = expected {
                        assert_eq!(
                            parser.screen().cell(0, column).unwrap().bgcolor(),
                            vt100::Color::Rgb(r, g, b)
                        );
                    }
                }
            }
        }
    }
}

#[test]
fn pi_working_spinner_advances_without_label_shimmer_and_stops_on_settlement() {
    for theme in themes() {
        let mut shell = InteractiveShell::test_shell_with_theme(theme);
        shell.set_identity("test", "test-model", "max");
        let run_id = shell.begin_run("test");
        let mut state = shell.state.borrow_mut();
        assert!(state.has_active_thinking_spinner());
        assert!(!state.has_active_status_shimmer());
        let raw = state.rendered_transcript(80).join("\n");
        assert!(
            strip_terminal_sequences(&raw).contains("⠋ Working"),
            "{raw:?}"
        );
        let label = raw.split_once("Working").unwrap().1.to_owned();
        state.advance_thinking_spinner(1);
        let next = state.rendered_transcript(80).join("\n");
        assert!(
            strip_terminal_sequences(&next).contains("⠙ Working"),
            "{next:?}"
        );
        assert_eq!(next.split_once("Working").unwrap().1, label);
        assert_eq!(state.status_shimmer_frame, 0);
        drop(state);
        shell.fail_run(run_id, "fixture ended");
        assert!(!shell.state.borrow().has_active_thinking_spinner());
    }
}

#[test]
fn pi_palette_keeps_exact_accent_and_explicit_default_backgrounds_after_model_changes() {
    for mut theme in themes() {
        let accent = theme.role_rgb("model_accent");
        for lab in [ModelLab::Anthropic, ModelLab::OpenAi, ModelLab::Unknown] {
            crate::tui::theme::apply_model_lab(&mut theme, lab);
            assert_eq!(theme.model_rgb(Some(lab)), accent);
        }
        for role in ["surface.assistant", "surface.reasoning"] {
            assert_eq!(theme.semantic_style(role).background, Color::Default);
        }
    }
}

#[test]
fn pi_terminal_default_backgrounds_are_not_replaced_by_native_fallback_colors() {
    let mut document = pi_theme::fixture("Pi default backgrounds", "#00aaff");
    let tokens = [
        "selectedBg",
        "searchMatchBg",
        "userMessageBg",
        "customMessageBg",
        "toolPendingBg",
        "toolSuccessBg",
        "toolErrorBg",
    ];
    for token in tokens {
        document["colors"][token] = serde_json::json!("");
    }
    let source = pi_theme::native_source(std::path::Path::new("qa.json"), &document.to_string())
        .unwrap()
        .into_owned();
    let mut adapter = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../extensions/octet-pi-compat/test/fixtures/pi-native-layout.toml"
    ))
    .to_owned();
    for color in ["#00aaff", "#223344", "#182838", "#183828", "#381828"] {
        adapter = adapter.replace(
            &format!("background = \"{color}\""),
            "background = \"default\"",
        );
    }
    for source in [source, adapter] {
        let shell = InteractiveShell::test_shell_with_theme(test_theme_from_source(&source));
        let palette = shell.extension_theme_get(None).unwrap();
        for token in tokens {
            assert_eq!(palette["backgrounds"][token], "", "{token}");
        }
    }
}

#[test]
fn pi_simple_extension_footer_replaces_native_metadata_and_withdrawal_restores_it() {
    for theme in themes() {
        let mut shell = InteractiveShell::test_shell_with_theme(theme);
        shell.set_identity("test", "Stock model", "off");
        shell.set_extension_ui(ShellExtensionUi {
            footer: vec![ShellExtensionUiLine {
                text: "Reviewed Pi footer".into(),
                style_role: None,
                priority: 0,
            }],
            ..Default::default()
        });
        let state = shell.state.borrow();
        let chrome = shell_chrome(&state, 80, Instant::now());
        assert!(!chrome.composer.join("\n").contains("Stock model"));
        assert!(chrome
            .extension_below
            .join("\n")
            .contains("Reviewed Pi footer"));
        drop(state);
        shell.set_extension_ui(Default::default());
        let state = shell.state.borrow();
        assert!(shell_chrome(&state, 80, Instant::now())
            .composer
            .join("\n")
            .contains("Stock model"));
    }
}
